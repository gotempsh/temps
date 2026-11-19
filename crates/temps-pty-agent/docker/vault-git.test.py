# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Exercise the shipped Git wrapper without requiring a sandbox or credentials."""
import json
import pathlib
import subprocess
import socket
import tempfile
import unittest


class VaultGitTest(unittest.TestCase):
    def test_routes_new_sessions_and_preserves_arguments_and_failures(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            ca = root / 'ca.pem'
            git = root / 'git'
            git.write_text('#!/usr/bin/env python3\nimport json,sys\nprint(json.dumps(sys.argv[1:]))\nsys.exit(17)\n')
            git.chmod(0o755)
            wrapper = root / 'vault-git'
            source = pathlib.Path(__file__).with_name('vault-git.sh').read_text()
            wrapper.write_text(source.replace('/run/temps-service-access/github-git', str(root / 'github-git')).replace('/usr/bin/git', str(git)).replace('/run/temps-service-access/ca.pem', str(ca)).replace('/run/temps-service-access/proxy.sock', str(root / 'proxy.sock')))
            command = ['sh', str(wrapper), 'clone', 'https://github.com/example/repository', 'directory with spaces']
            direct = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(json.loads(direct.stdout), command[2:])
            ca.touch()
            api_only = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(json.loads(api_only.stdout), command[2:])
            self.assertEqual(api_only.stderr, '')
            (root / 'github-git').touch()
            routed = subprocess.run(command, capture_output=True, text=True)
            args = json.loads(routed.stdout)
            self.assertEqual(args[-3:], command[2:])
            self.assertIn('http.https://github.com/.proxy=http://127.0.0.1:3128', args)
            self.assertIn('credential.https://github.com.helper=', args)
            self.assertIn('http.https://github.com/.sslVerify=true', args)
            # No socket is present: do not silently fall back to direct access.
            self.assertEqual(routed.returncode, 17)
            self.assertEqual(direct.returncode, 17)
            self.assertEqual(direct.stderr, '')
            self.assertIn('Vault access is unavailable', routed.stderr)
            self.assertIn('If the session expired or was revoked', routed.stderr)
            self.assertIn('approve a new session', routed.stderr)
            # An attached proxy may reject credentials, scope or upstream traffic;
            # none of those errors should be mislabeled as missing access.
            with socket.socket(socket.AF_UNIX) as attachment:
                attachment.bind(str(root / 'proxy.sock'))
                attached = subprocess.run(command, capture_output=True, text=True)
                self.assertEqual(attached.returncode, 17)
                self.assertEqual(attached.stderr, '')
            (root / 'proxy.sock').unlink()
            # Successful local Git commands work even after access expires.
            git.write_text('#!/bin/sh\nexit 0\n')
            local = subprocess.run(['sh', str(wrapper), 'status'], capture_output=True, text=True)
            self.assertEqual(local.returncode, 0)
            self.assertEqual(local.stderr, '')


if __name__ == '__main__':
    unittest.main()
