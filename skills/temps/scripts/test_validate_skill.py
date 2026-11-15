#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

import tempfile
import unittest
from pathlib import Path
from validate_skill import CLI_SKILLS, check_cli_references


class CliReferenceTests(unittest.TestCase):
    def test_latest_and_source_attribution_are_allowed_but_fixed_invocations_fail(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for skill in CLI_SKILLS:
                (root / skill).mkdir()
                (root / skill / 'SKILL.md').write_text(
                    'Generated from: @temps-sdk/cli@1.2.3\nbunx @temps-sdk/cli@latest --help')
            self.assertEqual(check_cli_references(root), [])
            (root / 'temps/SKILL.md').write_text('npx @temps-sdk/cli@1.2.3 --help')
            self.assertEqual(len(check_cli_references(root)), 1)

    def test_checked_in_skills(self):
        self.assertEqual(check_cli_references(Path(__file__).resolve().parents[2]), [])


if __name__ == '__main__':
    unittest.main()
