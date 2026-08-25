#!/usr/bin/env python3
"""Build and run the reusable Event Router throughput benchmark pipeline."""

from __future__ import annotations

import argparse
import hashlib
import json
import platform
import subprocess
import sys
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
from pathlib import Path

import analyze


REPO_ROOT = Path(__file__).resolve().parents[4]


@dataclass(frozen=True)
class SourceFingerprint:
    head: str
    diff_sha256: str


@dataclass(frozen=True)
class SuiteCommand:
    suite: str
    output_name: str
    arguments: tuple[str, ...]


def is_source_path(path: Path) -> bool:
    return '__pycache__' not in path.parts and path.suffix in {
        '.lock',
        '.py',
        '.rs',
        '.toml',
    }


def suite_commands(suite: str, *, samples: int) -> list[SuiteCommand]:
    if samples <= 0:
        raise ValueError('samples must be positive')
    suites = {
        'quick': [('quick', 'raw-quick.csv')],
        'full': [
            ('matrix', 'raw-matrix.csv'),
            ('fine-m', 'raw-fine-m.csv'),
            ('joint', 'raw-joint.csv'),
        ],
    }
    try:
        selected = suites[suite]
    except KeyError as error:
        raise ValueError(f'unknown suite: {suite}') from error
    return [
        SuiteCommand(
            suite=name,
            output_name=output,
            arguments=('--suite', name, '--samples', str(samples)),
        )
        for name, output in selected
    ]


def cargo_build_command() -> list[str]:
    return [
        'cargo',
        'bench',
        '--no-run',
        '-p',
        'barracuda-event-router',
        '--bench',
        'throughput',
    ]


def cargo_run_command(arguments: tuple[str, ...]) -> list[str]:
    return [
        'cargo',
        'bench',
        '--quiet',
        '-p',
        'barracuda-event-router',
        '--bench',
        'throughput',
        '--',
        *arguments,
    ]


def _git(*arguments: str, binary: bool = False) -> str | bytes:
    result = subprocess.run(
        ['git', '-C', str(REPO_ROOT), *arguments],
        check=True,
        capture_output=True,
        text=not binary,
    )
    return result.stdout


def source_fingerprint() -> SourceFingerprint:
    head = str(_git('rev-parse', 'HEAD')).strip()
    diff = bytes(
        _git(
            'diff',
            '--binary',
            '--',
            'Cargo.toml',
            'Cargo.lock',
            'core/event-router',
            binary=True,
        )
    )
    untracked = str(
        _git(
            'ls-files',
            '--others',
            '--exclude-standard',
            '--',
            'Cargo.toml',
            'Cargo.lock',
            'core/event-router',
        )
    ).splitlines()
    digest = hashlib.sha256(diff)
    for relative in sorted(untracked):
        relative_path = Path(relative)
        path = REPO_ROOT / relative_path
        if path.is_file() and is_source_path(relative_path):
            digest.update(relative.encode('utf-8'))
            digest.update(b'\0')
            digest.update(path.read_bytes())
    return SourceFingerprint(head=head, diff_sha256=digest.hexdigest())


def require_same_source(before: SourceFingerprint, after: SourceFingerprint) -> None:
    if before != after:
        raise RuntimeError(
            'Event Router source changed during benchmark: '
            f'before={before}, after={after}'
        )


def require_clean_output(output: Path) -> None:
    artifacts = [
        *output.glob('raw-*.csv'),
        *(output / name for name in ('metadata.json', 'summary.json', 'report.md')),
    ]
    existing = [path for path in artifacts if path.exists()]
    if existing:
        raise FileExistsError(
            f'output directory already contains benchmark artifacts: {existing}'
        )


def _capture(command: list[str]) -> str:
    result = subprocess.run(
        command,
        cwd=REPO_ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout.strip()


def default_output_dir() -> Path:
    timestamp = datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')
    return (
        REPO_ROOT
        / '.gstack'
        / 'benchmark-reports'
        / f'event-router-throughput-{timestamp}'
    )


def run_pipeline(args: argparse.Namespace) -> int:
    output = args.output.resolve() if args.output else default_output_dir()
    output.mkdir(parents=True, exist_ok=True)
    require_clean_output(output)
    commands = suite_commands(args.suite, samples=args.samples)
    before = source_fingerprint()

    subprocess.run(cargo_build_command(), cwd=REPO_ROOT, check=True)
    require_same_source(before, source_fingerprint())

    raw_paths: list[Path] = []
    for command in commands:
        path = output / command.output_name
        with path.open('w', encoding='utf-8') as handle:
            subprocess.run(
                cargo_run_command(command.arguments),
                cwd=REPO_ROOT,
                check=True,
                stdout=handle,
            )
        raw_paths.append(path)
        require_same_source(before, source_fingerprint())

    metadata = {
        **asdict(before),
        'timestamp_utc': datetime.now(timezone.utc).isoformat(),
        'suite': args.suite,
        'samples_per_configuration': args.samples,
        'host': platform.platform(),
        'machine': platform.machine(),
        'python': platform.python_version(),
        'uv': _capture(['uv', '--version']),
        'rustc': _capture(['rustc', '-Vv']),
        'cargo': _capture(['cargo', '-V']),
        'commands': [
            {
                'suite': command.suite,
                'output': command.output_name,
                'arguments': list(command.arguments),
            }
            for command in commands
        ],
    }
    (output / 'metadata.json').write_text(
        json.dumps(metadata, indent=2, sort_keys=True) + '\n', encoding='utf-8'
    )
    summaries = analyze.aggregate(
        analyze.load_samples(raw_paths), expected_samples=args.samples
    )
    comparisons: list[analyze.Comparison] = []
    if args.baseline:
        baseline = json.loads(args.baseline.read_text(encoding='utf-8'))
        comparisons = analyze.compare_to_baseline(
            summaries, baseline, minimum_ratio=args.minimum_ratio
        )
    analyze.write_outputs(output, summaries, metadata, comparisons)
    print(output)
    return 2 if any(comparison.regression for comparison in comparisons) else 0


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--suite', choices=('quick', 'full'), default='quick')
    parser.add_argument('--samples', type=int, default=7)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--baseline', type=Path)
    parser.add_argument('--minimum-ratio', type=float, default=0.9)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    try:
        return run_pipeline(parse_args(argv))
    except (
        FileExistsError,
        RuntimeError,
        subprocess.CalledProcessError,
        ValueError,
    ) as error:
        print(f'event-router benchmark failed: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
