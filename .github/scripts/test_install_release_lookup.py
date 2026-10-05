# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Exercise installer lookup errors without network access or installation."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


INSTALLER = Path(__file__).resolve().parents[2] / "scripts" / "install.sh"


class ReleaseLookupTests(unittest.TestCase):
    def run_lookup(self, status, *arguments):
        with tempfile.TemporaryDirectory(prefix="temps-install-lookup-") as temporary:
            fake_curl = Path(temporary) / "curl"
            fake_curl.write_text(
                '#!/bin/bash\n'
                'case "$*" in\n'
                '  *api.github.com*) printf "{}\\n%s" "$LOOKUP_STATUS" ;;\n'
                '  *) echo "unexpected download" >&2; exit 99 ;;\n'
                'esac\n'
            )
            fake_curl.chmod(0o755)
            environment = os.environ.copy()
            environment["PATH"] = temporary + os.pathsep + environment["PATH"]
            environment["LOOKUP_STATUS"] = status
            result = subprocess.run(
                ["bash", str(INSTALLER), *arguments],
                env=environment,
                capture_output=True,
                text=True,
                timeout=10,
            )
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertNotIn("unexpected download", result.stderr)
        return result.stdout + result.stderr

    def test_nightly_only_optional_warning_does_not_abort_stable_install(self):
        source = INSTALLER.read_text()
        begin = source.index("warn_if_stable_predates_beta()")
        end = source.index("\n# Explain", begin)
        script = "set -euo pipefail\n" + source[begin:end] + "\n" + r'''
github_api_get() { api_status=200; api_body='[{"tag_name":"v0.1.0-nightly.20261005"}]'; }
warning() { echo warning; }
warn_if_stable_predates_beta v1.0.0
echo stable-install-continues
'''
        result = subprocess.run(["bash", "-c", script], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("stable-install-continues", result.stdout)

    def test_missing_stable_explains_beta_opt_in(self):
        output = self.run_lookup("404")
        self.assertIn("has not published a stable release yet", output)
        self.assertIn("--channel beta", output)

    def test_rate_limit_is_not_reported_as_no_stable_release(self):
        for status in ["403", "429"]:
            with self.subTest(status=status):
                output = self.run_lookup(status)
                self.assertIn("GitHub API rate limit", output)
                self.assertNotIn("has not published a stable release", output)

    def test_network_error_is_explained(self):
        output = self.run_lookup("000")
        self.assertIn("network or DNS failure", output)

    def test_beta_lookup_reports_api_errors(self):
        output = self.run_lookup("403", "--channel", "beta")
        self.assertIn("Could not query GitHub for beta releases", output)


if __name__ == "__main__":
    unittest.main()
