# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

import unittest

from advisory_ignores import problems


class AdvisoryIgnoresTest(unittest.TestCase):
    report = {
        "vulnerabilities": {"list": [{"advisory": {"id": "vulnerability"}}]},
        "warnings": {
            "unmaintained": [{"advisory": {"id": "warning"}}],
            "yanked": [{"advisory": None}],
        },
    }

    def test_matches_vulnerabilities_and_warnings(self):
        ids = {"vulnerability", "warning"}
        self.assertEqual(list(problems(ids, ids, self.report)), [])

    def test_reports_stale_and_asymmetric_entries(self):
        errors = list(problems({"stale", "warning"}, {"stale", "vulnerability"}, self.report))
        self.assertTrue(any("stale matches no locked package" in error for error in errors))
        self.assertTrue(any("warning appears only in AUDIT_IGNORE" in error for error in errors))
        self.assertTrue(any("vulnerability appears only in deny.toml" in error for error in errors))
