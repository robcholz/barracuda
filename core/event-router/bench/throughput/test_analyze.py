import csv
import json
import tempfile
import unittest
from pathlib import Path

import analyze


HEADER = [
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
]


def write_samples(path: Path, throughputs, *, checksum='99', sample_offset=0) -> None:
    with path.open('w', newline='', encoding='utf-8') as handle:
        writer = csv.writer(handle)
        writer.writerow(HEADER)
        for sample, throughput in enumerate(throughputs, start=sample_offset):
            writer.writerow(
                [
                    'matched',
                    2,
                    288,
                    0,
                    256,
                    1,
                    0,
                    1000,
                    sample,
                    10_000,
                    throughput,
                    throughput,
                    throughput / 256,
                    checksum,
                ]
            )


class AnalyzeTests(unittest.TestCase):
    def test_single_sample_uses_the_measurement_for_both_quartiles(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'samples.csv'
            write_samples(path, [42])

            summaries = analyze.aggregate(
                analyze.load_samples([path]), expected_samples=1
            )

        self.assertEqual(summaries[0].q1_ingress_bytes_per_second, 42)
        self.assertEqual(summaries[0].q3_ingress_bytes_per_second, 42)

    def test_aggregates_median_inclusive_iqr_and_relative_score(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'samples.csv'
            write_samples(path, [10, 20, 30, 40, 50, 60, 70])

            summaries = analyze.aggregate(
                analyze.load_samples([path]), expected_samples=7
            )

        self.assertEqual(len(summaries), 1)
        summary = summaries[0]
        self.assertEqual(summary.median_ingress_bytes_per_second, 40)
        self.assertEqual(summary.q1_ingress_bytes_per_second, 25)
        self.assertEqual(summary.q3_ingress_bytes_per_second, 55)
        self.assertEqual(summary.relative_to_partition_best, 1.0)

    def test_rejects_missing_or_duplicate_sample_indices(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'samples.csv'
            write_samples(path, [10, 20, 30])
            samples = analyze.load_samples([path])
            with self.assertRaisesRegex(ValueError, 'expected 7 samples'):
                analyze.aggregate(samples, expected_samples=7)

            samples.append(samples[-1])
            with self.assertRaisesRegex(ValueError, 'duplicate sample index'):
                analyze.aggregate(samples, expected_samples=4)

    def test_rejects_checksum_drift_within_one_configuration(self):
        with tempfile.TemporaryDirectory() as directory:
            first = Path(directory) / 'first.csv'
            second = Path(directory) / 'second.csv'
            write_samples(first, [10], checksum='1')
            write_samples(second, [20], checksum='2', sample_offset=1)
            samples = analyze.load_samples([first, second])

            with self.assertRaisesRegex(ValueError, 'checksum drift'):
                analyze.aggregate(samples, expected_samples=2)

    def test_writes_machine_readable_and_markdown_summaries(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'samples.csv'
            write_samples(source, [10, 20, 30, 40, 50, 60, 70])
            summaries = analyze.aggregate(
                analyze.load_samples([source]), expected_samples=7
            )
            metadata = {'head': 'abc123', 'diff_sha256': 'def456'}

            analyze.write_outputs(root, summaries, metadata)

            payload = json.loads((root / 'summary.json').read_text(encoding='utf-8'))
            report = (root / 'report.md').read_text(encoding='utf-8')

        self.assertEqual(payload['metadata'], metadata)
        self.assertEqual(
            payload['configurations'][0]['median_ingress_bytes_per_second'], 40
        )
        self.assertIn('40 B/s', report)
        self.assertIn('abc123', report)

    def test_baseline_comparison_flags_only_matching_regressions(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'samples.csv'
            write_samples(path, [80] * 7)
            summaries = analyze.aggregate(
                analyze.load_samples([path]), expected_samples=7
            )
            baseline = {
                'configurations': [
                    {
                        **summaries[0].configuration_dict(),
                        'median_ingress_bytes_per_second': 100,
                    },
                    {
                        **summaries[0].configuration_dict(),
                        'm': 999,
                        'median_ingress_bytes_per_second': 1,
                    },
                ]
            }

            comparisons = analyze.compare_to_baseline(
                summaries, baseline, minimum_ratio=0.9
            )

        self.assertEqual(len(comparisons), 1)
        self.assertAlmostEqual(comparisons[0].ratio, 0.8)
        self.assertTrue(comparisons[0].regression)


if __name__ == '__main__':
    unittest.main()
