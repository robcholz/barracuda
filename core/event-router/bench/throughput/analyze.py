#!/usr/bin/env python3
"""Aggregate Event Router throughput CSV files into stable benchmark reports."""

from __future__ import annotations

import argparse
import csv
import json
import math
import statistics
from collections import defaultdict
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Iterable, Sequence


@dataclass(frozen=True, order=True)
class Configuration:
    scenario: str
    n: int
    m: int
    q: int
    payload: int
    fanout: int
    sink_yields: int
    events: int

    def partition(self) -> tuple[str, int, int, int]:
        return (self.scenario, self.payload, self.fanout, self.sink_yields)


@dataclass(frozen=True)
class Sample:
    configuration: Configuration
    sample: int
    elapsed_ns: int
    ingress_bytes_per_second: float
    routed_bytes_per_second: float
    events_per_second: float
    checksum: int


@dataclass
class Summary:
    configuration: Configuration
    sample_count: int
    median_ingress_bytes_per_second: float
    q1_ingress_bytes_per_second: float
    q3_ingress_bytes_per_second: float
    coefficient_of_variation: float
    median_routed_bytes_per_second: float
    median_events_per_second: float
    relative_to_partition_best: float = 0.0

    def configuration_dict(self) -> dict[str, object]:
        return asdict(self.configuration)

    def to_dict(self) -> dict[str, object]:
        return {
            **self.configuration_dict(),
            'sample_count': self.sample_count,
            'median_ingress_bytes_per_second': self.median_ingress_bytes_per_second,
            'q1_ingress_bytes_per_second': self.q1_ingress_bytes_per_second,
            'q3_ingress_bytes_per_second': self.q3_ingress_bytes_per_second,
            'coefficient_of_variation': self.coefficient_of_variation,
            'median_routed_bytes_per_second': self.median_routed_bytes_per_second,
            'median_events_per_second': self.median_events_per_second,
            'relative_to_partition_best': self.relative_to_partition_best,
        }


@dataclass(frozen=True)
class Comparison:
    configuration: Configuration
    baseline_bytes_per_second: float
    current_bytes_per_second: float
    ratio: float
    regression: bool

    def to_dict(self) -> dict[str, object]:
        return {
            **asdict(self.configuration),
            'baseline_bytes_per_second': self.baseline_bytes_per_second,
            'current_bytes_per_second': self.current_bytes_per_second,
            'ratio': self.ratio,
            'regression': self.regression,
        }


REQUIRED_COLUMNS = {
    'scenario',
    'n',
    'm',
    'q',
    'payload',
    'fanout',
    'sink_yields',
    'events',
    'sample',
    'elapsed_ns',
    'ingress_bytes_per_second',
    'routed_bytes_per_second',
    'events_per_second',
    'checksum',
}


def load_samples(paths: Iterable[Path]) -> list[Sample]:
    samples: list[Sample] = []
    for path in paths:
        with path.open(newline='', encoding='utf-8') as handle:
            reader = csv.DictReader(handle)
            columns = set(reader.fieldnames or ())
            missing = REQUIRED_COLUMNS - columns
            if missing:
                raise ValueError(f'{path} is missing CSV columns: {sorted(missing)}')
            for line, row in enumerate(reader, start=2):
                try:
                    configuration = Configuration(
                        scenario=row['scenario'],
                        n=int(row['n']),
                        m=int(row['m']),
                        q=int(row['q']),
                        payload=int(row['payload']),
                        fanout=int(row['fanout']),
                        sink_yields=int(row['sink_yields']),
                        events=int(row['events']),
                    )
                    samples.append(
                        Sample(
                            configuration=configuration,
                            sample=int(row['sample']),
                            elapsed_ns=int(row['elapsed_ns']),
                            ingress_bytes_per_second=float(
                                row['ingress_bytes_per_second']
                            ),
                            routed_bytes_per_second=float(
                                row['routed_bytes_per_second']
                            ),
                            events_per_second=float(row['events_per_second']),
                            checksum=int(row['checksum']),
                        )
                    )
                except (KeyError, TypeError, ValueError) as error:
                    raise ValueError(
                        f'invalid sample at {path}:{line}: {error}'
                    ) from error
    if not samples:
        raise ValueError('no benchmark samples found')
    return samples


def aggregate(samples: Sequence[Sample], *, expected_samples: int) -> list[Summary]:
    if expected_samples <= 0:
        raise ValueError('expected_samples must be positive')
    grouped: dict[Configuration, list[Sample]] = defaultdict(list)
    for sample in samples:
        grouped[sample.configuration].append(sample)

    summaries: list[Summary] = []
    for configuration, group in sorted(grouped.items()):
        indices = [sample.sample for sample in group]
        if len(indices) != len(set(indices)):
            raise ValueError(f'duplicate sample index for {configuration}')
        if len(group) != expected_samples:
            raise ValueError(
                f'expected {expected_samples} samples for {configuration}, got {len(group)}'
            )
        if set(indices) != set(range(expected_samples)):
            raise ValueError(
                f'sample indices for {configuration} must be 0..{expected_samples - 1}'
            )
        checksums = {sample.checksum for sample in group}
        if len(checksums) != 1:
            raise ValueError(f'checksum drift for {configuration}: {sorted(checksums)}')

        ingress = [sample.ingress_bytes_per_second for sample in group]
        routed = [sample.routed_bytes_per_second for sample in group]
        event_rates = [sample.events_per_second for sample in group]
        if len(ingress) == 1:
            q1 = q3 = ingress[0]
        else:
            q1, _, q3 = statistics.quantiles(ingress, n=4, method='inclusive')
        mean = statistics.fmean(ingress)
        cv = statistics.pstdev(ingress) / mean if mean else math.inf
        summaries.append(
            Summary(
                configuration=configuration,
                sample_count=len(group),
                median_ingress_bytes_per_second=statistics.median(ingress),
                q1_ingress_bytes_per_second=q1,
                q3_ingress_bytes_per_second=q3,
                coefficient_of_variation=cv,
                median_routed_bytes_per_second=statistics.median(routed),
                median_events_per_second=statistics.median(event_rates),
            )
        )

    partition_best: dict[tuple[str, int, int, int], float] = defaultdict(float)
    for summary in summaries:
        partition = summary.configuration.partition()
        partition_best[partition] = max(
            partition_best[partition], summary.median_ingress_bytes_per_second
        )
    for summary in summaries:
        best = partition_best[summary.configuration.partition()]
        summary.relative_to_partition_best = (
            summary.median_ingress_bytes_per_second / best if best else 0.0
        )
    return summaries


def compare_to_baseline(
    summaries: Sequence[Summary],
    baseline: dict[str, object],
    *,
    minimum_ratio: float,
) -> list[Comparison]:
    if not 0 < minimum_ratio <= 1:
        raise ValueError('minimum_ratio must be in (0, 1]')
    baseline_configs = baseline.get('configurations')
    if not isinstance(baseline_configs, list):
        raise ValueError('baseline has no configurations list')

    by_configuration: dict[Configuration, float] = {}
    for item in baseline_configs:
        if not isinstance(item, dict):
            continue
        try:
            configuration = Configuration(
                scenario=str(item['scenario']),
                n=int(item['n']),
                m=int(item['m']),
                q=int(item['q']),
                payload=int(item['payload']),
                fanout=int(item['fanout']),
                sink_yields=int(item['sink_yields']),
                events=int(item['events']),
            )
            by_configuration[configuration] = float(
                item['median_ingress_bytes_per_second']
            )
        except (KeyError, TypeError, ValueError):
            continue

    comparisons: list[Comparison] = []
    for summary in summaries:
        baseline_value = by_configuration.get(summary.configuration)
        if baseline_value is None or baseline_value <= 0:
            continue
        ratio = summary.median_ingress_bytes_per_second / baseline_value
        comparisons.append(
            Comparison(
                configuration=summary.configuration,
                baseline_bytes_per_second=baseline_value,
                current_bytes_per_second=summary.median_ingress_bytes_per_second,
                ratio=ratio,
                regression=ratio < minimum_ratio,
            )
        )
    return comparisons


def write_outputs(
    output_dir: Path,
    summaries: Sequence[Summary],
    metadata: dict[str, object],
    comparisons: Sequence[Comparison] = (),
) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    payload = {
        'metadata': metadata,
        'configurations': [summary.to_dict() for summary in summaries],
        'comparisons': [comparison.to_dict() for comparison in comparisons],
    }
    (output_dir / 'summary.json').write_text(
        json.dumps(payload, indent=2, sort_keys=True) + '\n', encoding='utf-8'
    )
    (output_dir / 'report.md').write_text(
        render_markdown(summaries, metadata, comparisons), encoding='utf-8'
    )


def render_markdown(
    summaries: Sequence[Summary],
    metadata: dict[str, object],
    comparisons: Sequence[Comparison],
) -> str:
    lines = [
        '# Event Router throughput report',
        '',
        f'- Commit: `{metadata.get("head", "unknown")}`',
        f'- Source diff SHA-256: `{metadata.get("diff_sha256", "unknown")}`',
        f'- Generated: `{metadata.get("timestamp_utc", "unknown")}`',
        '- Throughput counts original event payload bytes per wall-clock second.',
        '',
        '| Scenario | N | M | Q | Payload | Fan-out | Yields | Median | IQR | Relative | CV |',
        '|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|',
    ]
    for summary in summaries:
        configuration = summary.configuration
        lines.append(
            '| {scenario} | {n} | {m} | {q} | {payload} | {fanout} | '
            '{yields} | {median} B/s | {q1}–{q3} | {relative:.1%} | {cv:.1%} |'.format(
                scenario=configuration.scenario,
                n=configuration.n,
                m=configuration.m,
                q=configuration.q,
                payload=configuration.payload,
                fanout=configuration.fanout,
                yields=configuration.sink_yields,
                median=round(summary.median_ingress_bytes_per_second),
                q1=round(summary.q1_ingress_bytes_per_second),
                q3=round(summary.q3_ingress_bytes_per_second),
                relative=summary.relative_to_partition_best,
                cv=summary.coefficient_of_variation,
            )
        )
    if comparisons:
        regressions = sum(comparison.regression for comparison in comparisons)
        lines.extend(
            [
                '',
                '## Baseline comparison',
                '',
                f'Matched configurations: {len(comparisons)}; regressions: {regressions}.',
                '',
            ]
        )
    return '\n'.join(lines) + '\n'


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('inputs', nargs='+', type=Path)
    parser.add_argument('--output-dir', required=True, type=Path)
    parser.add_argument('--samples', required=True, type=int)
    parser.add_argument('--metadata', type=Path)
    parser.add_argument('--baseline', type=Path)
    parser.add_argument('--minimum-ratio', type=float, default=0.9)
    args = parser.parse_args()

    metadata = {}
    if args.metadata:
        metadata = json.loads(args.metadata.read_text(encoding='utf-8'))
    summaries = aggregate(load_samples(args.inputs), expected_samples=args.samples)
    comparisons: list[Comparison] = []
    if args.baseline:
        baseline = json.loads(args.baseline.read_text(encoding='utf-8'))
        comparisons = compare_to_baseline(
            summaries, baseline, minimum_ratio=args.minimum_ratio
        )
    write_outputs(args.output_dir, summaries, metadata, comparisons)
    return 2 if any(comparison.regression for comparison in comparisons) else 0


if __name__ == '__main__':
    raise SystemExit(main())
