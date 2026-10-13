# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Unit tests for release_latest.py (which stable release becomes Latest)."""

import io
from pathlib import Path
import subprocess
import sys
import unittest

from release_latest import decide, main, stable_version

SCRIPT = Path(__file__).with_name("release_latest.py")


class StableVersionTests(unittest.TestCase):
    def test_only_plain_stable_tags_parse(self):
        self.assertEqual(stable_version("v1.2.3"), (1, 2, 3))
        self.assertEqual(stable_version("v0.10.0"), (0, 10, 0))
        for tag in ["1.2.3", "v1.2", "v1.2.3-rc.1", "v0.1.0-nightly.20261005.abc12345",
                    "test-v1.2.3", "v01.2.3", "latest", ""]:
            with self.subTest(tag=tag):
                self.assertIsNone(stable_version(tag))


class DecideTests(unittest.TestCase):
    def test_first_stable_release_is_latest(self):
        self.assertEqual(decide("v0.1.0", ["v0.1.0-rc.3", "v0.1.0-nightly.20261005.abc12345"]),
                         (True, None))

    def test_newer_stable_release_is_latest(self):
        self.assertEqual(decide("v0.2.0", ["v0.1.0", "v0.1.1"]), (True, "v0.1.1"))

    def test_backport_does_not_become_latest(self):
        self.assertEqual(decide("v0.0.9", ["v0.1.0", "v0.0.8"]), (False, "v0.1.0"))

    def test_patch_on_older_minor_line_does_not_become_latest(self):
        self.assertEqual(decide("v0.9.5", ["v0.10.0", "v0.9.4"]), (False, "v0.10.0"))

    def test_numeric_not_lexical_comparison(self):
        self.assertEqual(decide("v0.10.0", ["v0.9.0", "v0.2.0"]), (True, "v0.9.0"))

    def test_prereleases_and_test_tags_never_compete(self):
        existing = ["v9.0.0-rc.1", "v9.0.0-nightly.20261005.abc12345", "test-v9.9.9", "junk", ""]
        self.assertEqual(decide("v0.1.0", existing), (True, None))

    def test_rerun_of_published_tag_stays_latest(self):
        self.assertEqual(decide("v0.2.0", ["v0.2.0", "v0.1.0"]), (True, "v0.1.0"))

    def test_rerun_of_published_backport_stays_not_latest(self):
        self.assertEqual(decide("v0.0.9", ["v0.1.0", "v0.0.9"]), (False, "v0.1.0"))

    def test_non_stable_tag_is_rejected(self):
        for tag in ["v0.1.0-rc.1", "v0.1.0-nightly.20261005.abc12345", "main", "test-v0.1.0"]:
            with self.subTest(tag=tag):
                with self.assertRaises(ValueError):
                    decide(tag, ["v0.0.1"])


class CommandLineTests(unittest.TestCase):
    def run_main(self, argv, stdin_text):
        stdout, stderr = io.StringIO(), io.StringIO()
        code = main(argv, io.StringIO(stdin_text), stdout, stderr)
        return code, stdout.getvalue(), stderr.getvalue()

    def test_prints_false_for_backport_and_explains(self):
        code, out, err = self.run_main(["release_latest.py", "v0.0.9"], "v0.1.0\nv0.0.8\n")
        self.assertEqual((code, out), (0, "false\n"))
        self.assertIn("WITHOUT moving Latest", err)
        self.assertIn("v0.1.0", err)

    def test_prints_true_for_highest(self):
        code, out, _ = self.run_main(["release_latest.py", "v0.2.0"], "v0.1.0\n")
        self.assertEqual((code, out), (0, "true\n"))

    def test_non_stable_tag_fails(self):
        code, out, err = self.run_main(["release_latest.py", "v0.2.0-rc.1"], "")
        self.assertEqual((code, out), (2, ""))
        self.assertIn("::error::", err)

    def test_missing_argument_fails(self):
        code, _, err = self.run_main(["release_latest.py"], "")
        self.assertEqual(code, 2)
        self.assertIn("usage", err)

    def test_script_entry_point(self):
        result = subprocess.run([sys.executable, str(SCRIPT), "v0.0.9"], input="v0.1.0\n",
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "false\n")


if __name__ == "__main__":
    unittest.main()
