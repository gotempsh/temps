#!/usr/bin/env python3
"""Regression tests for the Temps skill validator."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from validate_skill import GUARDED_SKILLS, check_integrity_pins


GUARD = """```bash
expected_temps_cli_integrity='{pin}'
actual_temps_cli_integrity="$(npm view @temps-sdk/cli@9.9.9 dist.integrity)"
```
"""


def write_skills(root: Path, pins: dict[str, str]) -> None:
    for skill, pin in pins.items():
        skill_root = root / skill
        skill_root.mkdir(parents=True)
        (skill_root / "SKILL.md").write_text(GUARD.format(pin=pin), encoding="utf-8")


class IntegrityPinTests(unittest.TestCase):
    def test_accepts_skills_pinning_the_published_artifact(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_skills(root, {skill: "sha512-current" for skill in GUARDED_SKILLS})

            self.assertEqual(check_integrity_pins(root, "sha512-current"), [])

    def test_rejects_a_pin_left_behind_by_a_version_bump(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_skills(
                root,
                {
                    "temps": "sha512-previous",
                    "temps-cli": "sha512-current",
                },
            )

            errors = check_integrity_pins(root, "sha512-current")

            self.assertEqual(len(errors), 1)
            self.assertIn("temps/SKILL.md", errors[0])
            self.assertIn("sha512-previous", errors[0])

    def test_rejects_a_skill_with_no_guard_at_all(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for skill in GUARDED_SKILLS:
                (root / skill).mkdir(parents=True)
                (root / skill / "SKILL.md").write_text("no guard", encoding="utf-8")

            errors = check_integrity_pins(root, "sha512-current")

            self.assertEqual(len(errors), 1)
            self.assertIn("no expected_temps_cli_integrity guard", errors[0])

    def test_reports_a_missing_guarded_skill(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_skills(root, {GUARDED_SKILLS[0]: "sha512-current"})

            errors = check_integrity_pins(root, "sha512-current")

            self.assertEqual(len(errors), 1)
            self.assertIn(f"skills/{GUARDED_SKILLS[1]}", errors[0])

    def test_the_checked_in_skills_pin_the_published_artifact(self) -> None:
        skills_root = Path(__file__).resolve().parents[2]

        self.assertEqual(check_integrity_pins(skills_root), [])


if __name__ == "__main__":
    unittest.main()
