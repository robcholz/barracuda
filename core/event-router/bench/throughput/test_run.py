import tempfile
import unittest
from pathlib import Path

import run


class RunTests(unittest.TestCase):
    def test_profile_support_crate_is_shared_not_agent_owned(self):
        repository_root = Path(__file__).resolve().parents[4]
        event_router_manifest = (
            repository_root / 'core' / 'event-router' / 'Cargo.toml'
        ).read_text(encoding='utf-8')
        agent_profile_manifest = (
            repository_root
            / 'components'
            / 'agent'
            / 'bench'
            / 'crates'
            / 'agent-profile'
            / 'Cargo.toml'
        ).read_text(encoding='utf-8')

        self.assertTrue(
            (repository_root / 'shared' / 'profile' / 'Cargo.toml').is_file()
        )
        self.assertFalse(
            (
                repository_root
                / 'components'
                / 'agent'
                / 'bench'
                / 'crates'
                / 'profile'
                / 'Cargo.toml'
            ).exists()
        )
        self.assertIn('path = "../../shared/profile"', event_router_manifest)
        self.assertIn('path = "../../../../../shared/profile"', agent_profile_manifest)

    def test_measurement_workloads_are_internal_bench_targets(self):
        package_root = Path(__file__).resolve().parents[2]
        workspace_root = package_root.parents[1]
        package_manifest = (package_root / 'Cargo.toml').read_text(encoding='utf-8')
        workspace_manifest = (workspace_root / 'Cargo.toml').read_text(encoding='utf-8')

        self.assertEqual(package_manifest.count('[[bench]]'), 2)
        self.assertIn('name = "profile"', package_manifest)
        self.assertIn('path = "bench/profile/main.rs"', package_manifest)
        self.assertIn('name = "throughput"', package_manifest)
        self.assertIn('path = "bench/throughput/main.rs"', package_manifest)
        self.assertFalse((package_root / 'bench' / 'profile' / 'Cargo.toml').exists())
        self.assertFalse(
            (package_root / 'bench' / 'throughput' / 'Cargo.toml').exists()
        )
        self.assertNotIn('"core/event-router/bench/profile"', workspace_manifest)
        self.assertNotIn('"core/event-router/bench/throughput"', workspace_manifest)

    def test_throughput_and_memory_profiles_have_separate_directories(self):
        benchmark_root = Path(__file__).resolve().parents[1]

        self.assertTrue((benchmark_root / 'profile' / 'README.md').is_file())
        self.assertTrue((benchmark_root / 'throughput' / 'README.md').is_file())
        self.assertEqual(
            {path.name for path in benchmark_root.iterdir() if path.is_file()},
            {'README.md'},
        )

    def test_runner_invokes_the_internal_throughput_bench(self):
        self.assertEqual(
            run.cargo_build_command(),
            [
                'cargo',
                'bench',
                '--no-run',
                '-p',
                'barracuda-event-router',
                '--bench',
                'throughput',
            ],
        )

    def test_default_output_directory_is_named_for_throughput(self):
        self.assertTrue(
            run.default_output_dir().name.startswith('event-router-throughput-')
        )
        self.assertEqual(
            run.cargo_run_command(('--suite', 'quick', '--samples', '1')),
            [
                'cargo',
                'bench',
                '--quiet',
                '-p',
                'barracuda-event-router',
                '--bench',
                'throughput',
                '--',
                '--suite',
                'quick',
                '--samples',
                '1',
            ],
        )

    def test_documented_commands_use_the_uv_workspace(self):
        readme = (Path(__file__).parent / 'README.md').read_text(encoding='utf-8')

        self.assertIn('uv run python core/event-router/bench/throughput/run.py', readme)
        self.assertIn(
            'uv run python -m unittest discover -s core/event-router/bench/throughput',
            readme,
        )
        self.assertNotIn('python3 core/event-router/bench/throughput', readme)

    def test_source_fingerprint_ignores_generated_python_cache(self):
        self.assertFalse(
            run.is_source_path(
                Path('core/event-router/bench/throughput/__pycache__/run.pyc')
            )
        )
        self.assertFalse(
            run.is_source_path(Path('core/event-router/bench/throughput/result.tmp'))
        )
        self.assertTrue(
            run.is_source_path(Path('core/event-router/bench/throughput/run.py'))
        )
        self.assertTrue(
            run.is_source_path(Path('core/event-router/bench/throughput/main.rs'))
        )

    def test_quick_suite_runs_one_representative_matrix(self):
        commands = run.suite_commands('quick', samples=3)

        self.assertEqual(len(commands), 1)
        self.assertEqual(commands[0].suite, 'quick')
        self.assertIn('--samples', commands[0].arguments)
        self.assertIn('3', commands[0].arguments)

    def test_full_suite_preserves_broad_fine_and_joint_outputs(self):
        commands = run.suite_commands('full', samples=7)

        self.assertEqual(
            [command.suite for command in commands], ['matrix', 'fine-m', 'joint']
        )
        self.assertEqual(
            [command.output_name for command in commands],
            ['raw-matrix.csv', 'raw-fine-m.csv', 'raw-joint.csv'],
        )

    def test_rejects_source_change_between_build_and_measurement(self):
        before = run.SourceFingerprint(head='a', diff_sha256='b')
        after = run.SourceFingerprint(head='a', diff_sha256='c')

        with self.assertRaisesRegex(RuntimeError, 'source changed'):
            run.require_same_source(before, after)

    def test_output_directory_must_not_already_contain_samples(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            (output / 'raw-matrix.csv').write_text('existing', encoding='utf-8')

            with self.assertRaisesRegex(FileExistsError, 'already contains'):
                run.require_clean_output(output)


if __name__ == '__main__':
    unittest.main()
