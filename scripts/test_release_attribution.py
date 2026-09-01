#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Contract tests ensuring distributed Temps artifacts retain attribution."""

from __future__ import annotations

import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
REQUIRED_FILES = {"LICENSE", "LICENSE-MIT", "NOTICE"}


class ReleaseAttributionTests(unittest.TestCase):
    def test_every_binary_tarball_contains_license_and_notice_files(self) -> None:
        workflow = (ROOT / ".github/workflows/release.yml").read_text(encoding="utf-8")
        commands = re.findall(r"tar -czf temps-[^\s]+\.tar\.gz ([^\n]+)", workflow)

        self.assertEqual(len(commands), 4, "expected one tar command for each release target")
        for members in commands:
            included = set(members.split())
            self.assertIn("temps", included)
            self.assertTrue(REQUIRED_FILES.issubset(included), members)

    def test_runtime_image_contains_license_and_notice_files(self) -> None:
        dockerfile = (ROOT / "Dockerfile").read_text(encoding="utf-8")

        self.assertIn(
            "COPY LICENSE LICENSE-MIT NOTICE /usr/share/licenses/temps/",
            dockerfile,
        )

    def test_installer_extracts_only_the_binary_into_path(self) -> None:
        installer = (ROOT / "scripts/install.sh").read_text(encoding="utf-8")

        self.assertIn('tar -xzf "$tarball" -C "$bin_dir" temps', installer)


if __name__ == "__main__":
    unittest.main()
