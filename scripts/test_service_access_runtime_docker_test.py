# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

import subprocess
import unittest
from pathlib import Path
from unittest.mock import patch

import test_service_access_runtime_docker as runner


class DockerContracts(unittest.TestCase):
    def exercise(self, fail=False):
        calls = []

        def command(args, **kwargs):
            calls.append(args)
            if args[:2] == ['docker', 'cp']:
                source = Path(args[2])
                self.assertTrue((source / 'rust-toolchain.toml').exists())
                self.assertIn('temps-service-access-runtime', (source / 'Cargo.toml').read_text())
            if fail and args[:3] == ['docker', 'start', '--attach']:
                raise subprocess.CalledProcessError(1, args)
            return subprocess.CompletedProcess(args, 0)

        with patch('sys.argv', ['runner']), patch.object(runner.shutil, 'which', return_value='docker'), \
                patch.object(runner.subprocess, 'run', side_effect=command), \
                patch.object(runner.subprocess, 'check_output', return_value='0'):
            if fail:
                with self.assertRaises(subprocess.CalledProcessError):
                    runner.main()
            else:
                runner.main()
        created = next(c for c in calls if c[:2] == ['docker', 'create'])
        name = created[created.index('--name') + 1]
        self.assertRegex(name, r'^temps-access-contract-[0-9a-f]{32}$')
        self.assertEqual(calls[-1], ['docker', 'rm', '-f', name])
        self.assertFalse(any('prune' in c for c in calls))

    def test_success_cleans_own_container_and_copies_pinned_toolchain(self):
        self.exercise()

    def test_failure_cleans_own_container(self):
        self.exercise(fail=True)

    def test_missing_docker_skips(self):
        with patch('sys.argv', ['runner']), patch.object(runner.shutil, 'which', return_value=None), \
                patch.object(runner.subprocess, 'run') as command:
            runner.main()
            command.assert_not_called()

    def test_unavailable_docker_skips_without_mutation(self):
        with patch('sys.argv', ['runner']), patch.object(runner.shutil, 'which', return_value='docker'), \
                patch.object(runner.subprocess, 'run', side_effect=subprocess.TimeoutExpired('docker', 15)) as command:
            runner.main()
            self.assertEqual(command.call_count, 1)
            self.assertEqual(command.call_args.args[0], ['docker', 'info'])


if __name__ == '__main__':
    unittest.main()
