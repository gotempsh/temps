# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Unit tests for release_select.py (upgrade-test release selection)."""

import io
from pathlib import Path
import subprocess
import sys
import unittest

from release_select import highest, main, select

SCRIPT = Path(__file__).with_name("release_select.py")

# Newest-published first, deliberately out of version order.
PUBLISHED = [
    "v0.0.9",                              # backport published after v0.1.0
    "v0.1.0-nightly.20261006.abc12345",
    "test-v9.9.9",                         # release-workflow smoke test
    "v0.1.0",
    "v0.1.1-rc.2",
    "v0.1.1-rc.10",
    "v0.1.1-beta.3",
    "v0.1.0-nightly.20261005.0abc1234",
    "v9.0.0-test",                         # ad-hoc prerelease, no channel
    "not-a-version",
    "",
]


class HighestTests(unittest.TestCase):
    def test_stable_is_highest_version_not_newest_published(self):
        self.assertEqual(highest(PUBLISHED, "stable"), "v0.1.0")

    def test_stable_compares_numerically(self):
        self.assertEqual(highest(["v0.9.0", "v0.10.0", "v0.2.0"], "stable"), "v0.10.0")

    def test_beta_orders_rc_numerically_and_above_beta(self):
        self.assertEqual(highest(PUBLISHED, "beta"), "v0.1.1-rc.10")

    def test_beta_ignores_nightly_test_and_adhoc_prereleases(self):
        tags = ["v0.1.0-nightly.20261006.abc12345", "test-v9.9.9-rc.1", "v9.0.0-test", "v0.0.1-rc.1"]
        self.assertEqual(highest(tags, "beta"), "v0.0.1-rc.1")

    def test_nightly_is_highest_nightly(self):
        self.assertEqual(highest(PUBLISHED, "nightly"), "v0.1.0-nightly.20261006.abc12345")

    def test_nothing_matches(self):
        self.assertIsNone(highest(["test-v1.0.0", "v1.0.0-rc.1"], "stable"))


class SelectTests(unittest.TestCase):
    def test_stable_default_without_fallback_needed(self):
        self.assertEqual(select(PUBLISHED, "stable", "beta"), ("v0.1.0", "stable"))

    def test_stable_falls_back_to_beta_before_first_stable(self):
        tags = ["v0.1.0-rc.2", "v0.1.0-rc.10", "v0.1.0-nightly.20261006.abc12345", "test-v1.0.0"]
        self.assertEqual(select(tags, "stable", "beta"), ("v0.1.0-rc.10", "beta"))

    def test_no_fallback_means_none(self):
        self.assertEqual(select(["v0.1.0-rc.1"], "stable"), (None, "stable"))

    def test_stable_below_floor_falls_back_to_beta(self):
        # A stable tag left over from an earlier line must not hide the
        # fallback: it predates the tooling the upgrade test drives.
        tags = ["v0.0.8", "v0.1.0-beta.56", "v0.1.0-beta.9", "v0.1.0-nightly.20261007.abc12345"]
        self.assertEqual(select(tags, "stable", "beta", (0, 1, 0)), ("v0.1.0-beta.56", "beta"))

    def test_floor_admits_the_first_stable_release_of_its_core(self):
        tags = ["v0.0.8", "v0.1.0", "v0.1.1-rc.2"]
        self.assertEqual(select(tags, "stable", "beta", (0, 1, 0)), ("v0.1.0", "stable"))

    def test_floor_applies_to_the_fallback_channel(self):
        self.assertEqual(select(["v0.0.8", "v0.0.9-rc.1"], "stable", "beta", (0, 1, 0)),
                         (None, "beta"))


class CommandLineTests(unittest.TestCase):
    def run_main(self, arguments, tags):
        stdout, stderr = io.StringIO(), io.StringIO()
        code = main(["release_select.py", *arguments], io.StringIO("\n".join(tags) + "\n"),
                    stdout, stderr)
        return code, stdout.getvalue(), stderr.getvalue()

    def test_prints_tag(self):
        self.assertEqual(self.run_main(["--channel", "stable"], PUBLISHED)[:2], (0, "v0.1.0\n"))

    def test_fallback_emits_notice(self):
        code, out, err = self.run_main(["--channel", "stable", "--fallback", "beta"], ["v0.1.0-rc.1"])
        self.assertEqual((code, out), (0, "v0.1.0-rc.1\n"))
        self.assertIn("::notice::No stable release yet", err)

    def test_min_core_notice_names_the_floor(self):
        code, out, err = self.run_main(
            ["--channel", "stable", "--fallback", "beta", "--min-core", "v0.1.0"],
            ["v0.0.8", "v0.1.0-beta.56"])
        self.assertEqual((code, out), (0, "v0.1.0-beta.56\n"))
        self.assertIn("::notice::No stable release at or above v0.1.0 yet", err)

    def test_min_core_rejects_a_prerelease_floor(self):
        with self.assertRaises(SystemExit):
            self.run_main(["--channel", "stable", "--min-core", "v0.1.0-beta.1"], ["v0.1.0"])

    def test_empty_selection_fails_with_error(self):
        code, out, err = self.run_main(["--channel", "beta"], ["v0.1.0", "test-v0.2.0-rc.1"])
        self.assertEqual((code, out), (1, ""))
        self.assertIn("::error::", err)

    def test_script_entry_point(self):
        result = subprocess.run([sys.executable, str(SCRIPT), "--channel", "nightly"],
                                input="\n".join(PUBLISHED), capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "v0.1.0-nightly.20261006.abc12345\n")


if __name__ == "__main__":
    unittest.main()
