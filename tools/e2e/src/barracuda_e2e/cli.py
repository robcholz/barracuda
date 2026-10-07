"""Command-line entry for the end-to-end harness."""

from __future__ import annotations

import argparse
import contextlib
import json
import os
import shutil
import sys
from collections.abc import Iterator, Sequence
from dataclasses import dataclass
from pathlib import Path
from urllib.parse import urlsplit

from .assertions import (
    check_heap,
    check_logs,
    check_replay,
    check_requests,
    check_step_requests,
    check_transcript,
    heap_high_water,
)
from . import coverage
from .ntp import LocalNtp
from .scenario import Scenario, ScenarioError, discover, load_scenario
from .system import (
    WORKSPACE,
    Binaries,
    HarnessError,
    SystemProcess,
    TapeServer,
    build,
    chat,
    configure_model,
    http_request,
    require_network,
)
from .tapes import CHAT_PATH, SCRIPTED_API_PATH, write_scripted_tape
from . import virtual_io

SCENARIOS = Path(__file__).resolve().parents[2] / 'scenarios'
ARTIFACTS = WORKSPACE / 'target' / 'e2e'
TAPE_PORT = 18_787
TURN_TIMEOUT_SECONDS = 600


@dataclass(frozen=True)
class LiveModel:
    """Real provider used while recording."""

    base_url: str
    api_key: str
    model: str


def build_parser() -> argparse.ArgumentParser:
    """Build the public CLI parser."""

    parser = argparse.ArgumentParser(
        prog='barracuda-e2e',
        description='Run host Barracuda scenarios against recorded or scripted LLM tapes.',
    )
    commands = parser.add_subparsers(dest='command', required=True)
    commands.add_parser('list', help='list scenarios')
    run = commands.add_parser('run', help='run scenarios')
    run.add_argument('names', nargs='*', help='scenario file stems (default: all)')
    run.add_argument(
        '--record',
        action='store_true',
        help='record "recorded" scenarios against the live provider',
    )
    run.add_argument(
        '--direct',
        action='store_true',
        help='run "recorded" scenarios with the System calling the live provider '
        'itself, through its own TLS, without a tape',
    )
    run.add_argument(
        '--test-roots',
        type=Path,
        metavar='PEM',
        help='build the host System to also trust these roots (for a network '
        'that intercepts TLS); never use for firmware',
    )
    run.add_argument(
        '--env-file',
        type=Path,
        help='KEY=VALUE file providing BARRACUDA_LLM_BASE_URL/API_KEY/MODEL',
    )
    run.add_argument('--skip-build', action='store_true', help='reuse existing builds')
    run.add_argument(
        '--coverage',
        action='store_true',
        help='run an instrumented System and report source coverage per component '
        'under target/e2e/coverage',
    )
    run.add_argument(
        '--shard',
        type=_shard_spec,
        metavar='K/N',
        help='run only every N-th selected scenario, starting with the K-th',
    )
    run.add_argument(
        '--heap-limit',
        type=int,
        metavar='BYTES',
        help='cap the System ordinary heap, aborting on overflow like a device OOM',
    )
    run.add_argument(
        '--no-local-ntp',
        action='store_true',
        help='do not answer the System NTP requests from the host clock',
    )
    return parser


def main(argv: Sequence[str] | None = None) -> None:
    """Run the harness and exit non-zero when any scenario fails."""

    arguments = build_parser().parse_args(argv)
    try:
        scenarios = [load_scenario(path) for path in discover(SCENARIOS)]
    except ScenarioError as exc:
        sys.exit(f'error: {exc}')
    if arguments.command == 'list':
        for scenario in scenarios:
            print(f'{scenario.slug:<28} {scenario.mode:<9} {scenario.name}')
        return

    selected = [
        s for s in scenarios if not arguments.names or s.slug in arguments.names
    ]
    missing = set(arguments.names) - {s.slug for s in selected}
    if missing:
        sys.exit(f'error: unknown scenario(s): {", ".join(sorted(missing))}')
    if arguments.shard is not None:
        selected = shard(selected, *arguments.shard)
    if arguments.record and arguments.direct:
        sys.exit('error: --record and --direct are exclusive')
    live = (
        _live_model(arguments.env_file)
        if arguments.record or arguments.direct
        else None
    )
    if arguments.test_roots is not None:
        os.environ['BARRACUDA_TLS_TEST_ROOTS'] = str(arguments.test_roots.resolve())
    try:
        require_network()
        binaries = build(
            skip=arguments.skip_build,
            coverage=coverage.TARGET_DIR if arguments.coverage else None,
        )
    except HarnessError as exc:
        sys.exit(f'error: {exc}')
    coverage_dir = ARTIFACTS / 'coverage'
    if arguments.coverage:
        shutil.rmtree(coverage_dir, ignore_errors=True)
        (coverage_dir / 'profraw').mkdir(parents=True)
        os.environ['LLVM_PROFILE_FILE'] = str(
            coverage_dir / 'profraw' / coverage.PROFILE_PATTERN
        )

    failed = 0
    summary: dict[str, dict[str, object]] = {}
    with _local_ntp(enabled=not arguments.no_local_ntp):
        for scenario in selected:
            failures = run_scenario(
                scenario, binaries, live, arguments.heap_limit, arguments.direct
            )
            peak = _heap_peak(scenario)
            status = 'PASS' if not failures else 'FAIL'
            heap = f' [heap {peak / 1024:.0f} KiB]' if peak is not None else ''
            print(f'{status} {scenario.slug}: {scenario.name}{heap}', flush=True)
            for failure in failures:
                print(f'    - {failure}')
            failed += bool(failures)
            summary[scenario.slug] = {
                'passed': not failures,
                'failures': failures,
                'heap_high_water_bytes': peak,
            }
    ARTIFACTS.mkdir(parents=True, exist_ok=True)
    (ARTIFACTS / 'summary.json').write_text(
        json.dumps(summary, indent=2) + '\n', encoding='utf-8'
    )
    if arguments.coverage:
        try:
            components = coverage.report(
                binaries.system, coverage_dir / 'profraw', coverage_dir
            )
        except HarnessError as exc:
            sys.exit(f'error: {exc}')
        print(f'\n{coverage.format_table(components)}')
        print(f'coverage report: {coverage_dir / "html" / "index.html"}')
    print(f'\n{len(selected) - failed} passed, {failed} failed')
    print(f'artifacts: {ARTIFACTS}')
    sys.exit(1 if failed else 0)


def shard(scenarios: list[Scenario], index: int, count: int) -> list[Scenario]:
    """Every `count`-th scenario from the 1-based `index`; shards partition the list."""

    return scenarios[index - 1 :: count]


def _shard_spec(value: str) -> tuple[int, int]:
    index, sep, count = value.partition('/')
    try:
        parsed = int(index), int(count)
    except ValueError:
        parsed = (0, 0)
    if not sep or not 1 <= parsed[0] <= parsed[1]:
        raise argparse.ArgumentTypeError(
            f'expected K/N with 1 <= K <= N, got {value!r}'
        )
    return parsed


def run_scenario(
    scenario: Scenario,
    binaries: Binaries,
    live: LiveModel | None,
    heap_limit: int | None = None,
    direct: bool = False,
) -> list[str]:
    """Run one scenario in an isolated state directory and return failures."""

    artifacts = ARTIFACTS / scenario.slug
    shutil.rmtree(artifacts, ignore_errors=True)
    artifacts.mkdir(parents=True)
    tape_server = TapeServer(TAPE_PORT, artifacts / 'llm-tape.log')
    system = SystemProcess(
        binaries, artifacts / 'state', artifacts / 'system.log', heap_limit
    )
    recording = scenario.mode == 'recorded' and live is not None and not direct
    direct = direct and scenario.mode == 'recorded' and live is not None
    model_url = None
    try:
        if scenario.mode == 'scripted':
            tape = artifacts / 'scripted.jsonl'
            interactions = write_scripted_tape(scenario.responses, tape)
            api_path, model, api_key = SCRIPTED_API_PATH, 'scripted', 'scripted'
            tape_server.replay(tape, artifacts / 'requests')
        elif direct:
            assert live is not None
            interactions, api_path = 0, ''
            model, api_key = live.model, live.api_key
            model_url = live.base_url.rstrip('/')
        elif recording:
            assert live is not None
            parts = urlsplit(live.base_url)
            api_path, model, api_key = parts.path.rstrip('/'), live.model, live.api_key
            interactions = 0
            tape_server.record(f'{parts.scheme}://{parts.netloc}', scenario.tape)
        elif scenario.mode == 'none':
            interactions, api_path, model, api_key = 0, '', '', ''
        else:
            if scenario.tape is None or not scenario.tape.exists():
                return [f'no recorded tape at {scenario.tape}; run with --record']
            interactions, api_path = _recorded_shape(scenario.tape)
            model, api_key = 'recorded', 'recorded'
            tape_server.replay(scenario.tape, artifacts / 'requests')

        system.start()
        for pattern in scenario.ready_logs:
            system.wait_for_log(pattern, scenario.await_seconds)
        hardware = virtual_io.VirtualIoClient()
        if scenario.virtual_io is not None:
            virtual_io.apply_setup(hardware, scenario.virtual_io)
        http_failures = _run_http(scenario, 'before')
        records: list[dict[str, object]] = []
        if scenario.steps:
            configure_model(model_url or tape_server.base_url(api_path), model, api_key)
            records = chat(
                binaries.cli,
                [step.send for step in scenario.steps],
                TURN_TIMEOUT_SECONDS,
            )
        for pattern in scenario.await_logs:
            system.wait_for_log(pattern, scenario.await_seconds)
        http_failures += _run_http(scenario, 'after')
        http_failures += virtual_io.check(hardware, scenario.virtual_io)
        (artifacts / 'virtual-io.json').write_text(
            json.dumps(virtual_io.snapshot(hardware), indent=2) + '\n', encoding='utf-8'
        )
        hardware.close()
        (artifacts / 'transcript.jsonl').write_text(
            ''.join(json.dumps(r, ensure_ascii=False) + '\n' for r in records),
            encoding='utf-8',
        )
    except (HarnessError, virtual_io.VirtualIoError) as exc:
        return [str(exc)]
    finally:
        system.stop()
        tape_server.stop()

    failures = http_failures + check_transcript(scenario, records)
    if system.early_exit is not None:
        failures.append(f'System exited with {system.early_exit} during the scenario')
    failures += check_logs(scenario, system.log())
    failures += check_heap(scenario, system.log())
    if not recording and not direct and scenario.mode != 'none':
        failures += check_replay(tape_server.counts(), interactions)
        failures += check_requests(scenario, artifacts / 'requests')
        failures += check_step_requests(scenario, artifacts / 'requests')
    return failures


def _heap_peak(scenario: Scenario) -> int | None:
    log = ARTIFACTS / scenario.slug / 'system.log'
    if not log.exists():
        return None
    return heap_high_water(log.read_text(encoding='utf-8', errors='replace'))


def _run_http(scenario: Scenario, when: str) -> list[str]:
    failures: list[str] = []
    for check in scenario.http:
        if check.when != when:
            continue
        status, body = http_request(check.method, check.path, check.body)
        if status != check.status:
            failures.append(
                f'{check.method} {check.path}: status {status} != {check.status}: '
                f'{body[:200]}'
            )
        failures += [
            f'{check.method} {check.path}: body lacks {needle!r}: {body[:300]}'
            for needle in check.body_contains
            if needle not in body
        ]
    return failures


@contextlib.contextmanager
def _local_ntp(enabled: bool) -> Iterator[None]:
    if not enabled:
        yield
        return
    try:
        ntp = LocalNtp().__enter__()
    except HarnessError as exc:
        print(f'warning: local NTP unavailable ({exc}); time scenarios may fail')
        yield
        return
    try:
        yield
    finally:
        ntp.__exit__(None, None, None)


def _recorded_shape(tape: Path) -> tuple[int, str]:
    paths = [
        event['path']
        for event in map(json.loads, tape.read_text(encoding='utf-8').splitlines())
        if event.get('kind') == 'request'
    ]
    if not paths or not paths[0].endswith(CHAT_PATH):
        raise HarnessError(f'{tape} contains no chat-completions requests')
    return len(paths), paths[0][: -len(CHAT_PATH)]


def _live_model(env_file: Path | None) -> LiveModel:
    values = dict(os.environ)
    if env_file is not None:
        for line in env_file.read_text(encoding='utf-8').splitlines():
            key, separator, value = line.strip().partition('=')
            if separator and not key.startswith('#'):
                values[key.strip()] = value.strip()
    try:
        return LiveModel(
            base_url=values['BARRACUDA_LLM_BASE_URL'],
            api_key=values['BARRACUDA_LLM_API_KEY'],
            model=values['BARRACUDA_LLM_MODEL'],
        )
    except KeyError as exc:
        sys.exit(f'error: recording needs {exc.args[0]}')


if __name__ == '__main__':
    main()
