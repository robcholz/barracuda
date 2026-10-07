"""Source coverage of the host System across an E2E run.

The System is built with LLVM source-based coverage into its own target
directory. Its profile runtime uses continuous mode (`%c`), so counters live in
a memory-mapped file and survive the SIGINT that ends every scenario. After
the run the profiles are merged and summarized per component.
"""

from __future__ import annotations

import json
import subprocess
from collections.abc import Iterable
from dataclasses import dataclass
from pathlib import Path

from .system import WORKSPACE, HarnessError, host_triple

TARGET_DIR = WORKSPACE / 'target' / 'coverage'
RUSTFLAGS = '-C instrument-coverage -C llvm-args=-runtime-counter-relocation'
# One file per System process; `%c` turns on continuous mode.
PROFILE_PATTERN = '%p-%c.profraw'
BUSINESS_ROOTS = ('plugins', 'shared', 'core')


@dataclass(frozen=True)
class Component:
    """Line, function, and region coverage of one crate or plugin."""

    name: str
    business: bool
    lines: tuple[int, int]
    functions: tuple[int, int]
    regions: tuple[int, int]

    @property
    def line_percent(self) -> float:
        covered, count = self.lines
        return 100.0 * covered / count if count else 100.0


def component_of(path: str) -> str | None:
    """Group a source file by root directory, plugin, and crate.

    `plugins/agent/crates/session/src/x.rs` becomes `plugins/agent/session`;
    files outside the workspace (dependencies, std) have no component.
    """

    try:
        parts = Path(path).resolve().relative_to(WORKSPACE).parts
    except ValueError:
        return None
    if len(parts) < 2 or parts[0] in ('target', 'tools'):
        return None
    if len(parts) > 3 and parts[2] == 'crates':
        return '/'.join((parts[0], parts[1], parts[3]))
    return '/'.join(parts[:2])


def summarize(files: Iterable[dict[str, object]]) -> list[Component]:
    """Aggregate `llvm-cov export` file summaries into components."""

    totals: dict[str, list[int]] = {}
    for entry in files:
        name = component_of(str(entry['filename']))
        if name is None:
            continue
        summary = entry['summary']
        assert isinstance(summary, dict)
        counts = totals.setdefault(name, [0] * 6)
        for index, key in enumerate(('lines', 'functions', 'regions')):
            counts[2 * index] += summary[key]['covered']
            counts[2 * index + 1] += summary[key]['count']
    return sorted(
        (
            Component(
                name,
                name.split('/', 1)[0] in BUSINESS_ROOTS,
                (c[0], c[1]),
                (c[2], c[3]),
                (c[4], c[5]),
            )
            for name, c in totals.items()
        ),
        key=lambda component: (not component.business, component.name),
    )


def report(binary: Path, profiles: Path, out: Path) -> list[Component]:
    """Merge the run's profiles and write summary.json and an HTML report."""

    raw = sorted(profiles.glob('*.profraw'))
    if not raw:
        raise HarnessError(f'no coverage profiles in {profiles}')
    merged = out / 'e2e.profdata'
    _run(
        [_tool('llvm-profdata'), 'merge', '-sparse', *map(str, raw), '-o', str(merged)]
    )
    cov = [str(binary), f'-instr-profile={merged}']
    exported = _run([_tool('llvm-cov'), 'export', '-summary-only', *cov])
    components = summarize(json.loads(exported)['data'][0]['files'])
    _run(
        [
            _tool('llvm-cov'),
            'show',
            '-format=html',
            f'-output-dir={out / "html"}',
            *cov,
            *(str(WORKSPACE / root) for root in BUSINESS_ROOTS),
        ]
    )
    (out / 'summary.json').write_text(
        json.dumps(
            {
                c.name: {
                    'business': c.business,
                    'lines': c.lines,
                    'functions': c.functions,
                    'regions': c.regions,
                }
                for c in components
            },
            indent=2,
        )
        + '\n',
        encoding='utf-8',
    )
    return components


def format_table(components: list[Component]) -> str:
    """Per-component line coverage, business code first, with totals."""

    lines = [f'{"component":<44} {"lines":>15} {"%":>6}']
    for business in (True, False):
        group = [c for c in components if c.business == business]
        if not group:
            continue
        lines.append('-- business code --' if business else '-- infrastructure --')
        for c in group:
            lines.append(
                f'{c.name:<44} {c.lines[0]:>7}/{c.lines[1]:<7} {c.line_percent:>5.1f}'
            )
        covered = sum(c.lines[0] for c in group)
        count = sum(c.lines[1] for c in group)
        percent = 100.0 * covered / count if count else 100.0
        lines.append(f'{"total":<44} {covered:>7}/{count:<7} {percent:>5.1f}')
    return '\n'.join(lines)


def _tool(name: str) -> str:
    sysroot = subprocess.run(
        ['rustc', '--print', 'sysroot'], capture_output=True, text=True, check=True
    ).stdout.strip()
    path = Path(sysroot) / 'lib' / 'rustlib' / host_triple() / 'bin' / name
    if not path.exists():
        raise HarnessError(f'{name} not found; run: rustup component add llvm-tools')
    return str(path)


def _run(command: list[str]) -> str:
    result = subprocess.run(command, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        raise HarnessError(f'{command[0]} failed:\n{result.stderr[-4000:]}')
    return result.stdout
