#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Validate the Temps skill structure and local Markdown references."""

from __future__ import annotations

import json
import re
from pathlib import Path


LINK = re.compile(r"\[[^]]*\]\(([^)]+)\)")
PINNED_CLI = "@temps-sdk/cli@0.1.36"
# sha512 of the published tarball for PINNED_CLI, as reported by
# `npm view @temps-sdk/cli@<version> dist.integrity`. Refresh it in the same
# commit that bumps PINNED_CLI, once the release is actually on npm — the value
# has no version substring, so a find-and-replace bump silently leaves it stale
# and every preflight guard below fails closed against the new release.
PINNED_CLI_INTEGRITY = "sha512-Md9Hs2IQug6YIL8mzeq7GyGsI+bN2lttm1gsri27+nfH7S9Knez8ZLbQXbLoM+er6G3sYyYbhjWEEJn6jq8A3A=="
INTEGRITY_PIN = re.compile(r"expected_temps_cli_integrity='([^']*)'")
# Skills whose preflight guard installs the pinned CLI. They embed the same
# hash independently, so they are validated together.
GUARDED_SKILLS = ("temps", "temps-cli")


def check_integrity_pins(
    skills_root: Path, expected: str = PINNED_CLI_INTEGRITY
) -> list[str]:
    """Every CLI preflight guard must pin the same, current package integrity."""
    errors: list[str] = []
    pins = 0
    for skill in GUARDED_SKILLS:
        skill_root = skills_root / skill
        if not skill_root.is_dir():
            errors.append(f"guarded skill is missing: skills/{skill}")
            continue
        for markdown in sorted(skill_root.rglob("*.md")):
            text = markdown.read_text(encoding="utf-8")
            for pin in INTEGRITY_PIN.findall(text):
                pins += 1
                if pin != expected:
                    errors.append(
                        f"{markdown.relative_to(skills_root)}: integrity pin {pin} "
                        f"does not match the published {PINNED_CLI} artifact"
                    )
    if not pins:
        errors.append(
            "no expected_temps_cli_integrity guard found in "
            f"{', '.join(f'skills/{skill}' for skill in GUARDED_SKILLS)}"
        )
    return errors


def main() -> int:
    root = Path(__file__).resolve().parents[1]
    errors: list[str] = check_integrity_pins(root.parent)
    skill = root / "SKILL.md"
    if len(skill.read_text(encoding="utf-8").splitlines()) > 500:
        errors.append("SKILL.md exceeds 500 lines")

    for markdown in root.rglob("*.md"):
        text = markdown.read_text(encoding="utf-8")
        for target in LINK.findall(text):
            if target.startswith(("http://", "https://", "mailto:", "#")):
                continue
            path_text = target.split("#", 1)[0]
            if path_text and not (markdown.parent / path_text).resolve().exists():
                errors.append(f"{markdown.relative_to(root)}: missing link target {target}")
        for match in re.finditer(r"@temps-sdk/cli(?:@([0-9]+\.[0-9]+\.[0-9]+))?", text):
            if match.group(0) != PINNED_CLI:
                errors.append(
                    f"{markdown.relative_to(root)}: unpinned or unexpected CLI reference {match.group(0)}"
                )

    index = root / "references" / "commands" / "INDEX.md"
    command_files = set((root / "references" / "commands").glob("*.md")) - {index}
    indexed_files = {
        (index.parent / target).resolve()
        for target in LINK.findall(index.read_text(encoding="utf-8"))
    }
    missing_from_index = sorted(path.name for path in command_files if path.resolve() not in indexed_files)
    if missing_from_index:
        errors.append(f"command references absent from index: {', '.join(missing_from_index)}")

    evals = json.loads((root / "evals" / "evals.json").read_text(encoding="utf-8"))
    if evals.get("skill_name") != root.name:
        errors.append("eval suite skill_name does not match the skill directory")
    seen_ids: set[int] = set()
    for case in evals.get("evals", []):
        case_id = case.get("id")
        if not isinstance(case_id, int) or case_id in seen_ids:
            errors.append(f"eval case has invalid or duplicate id: {case_id!r}")
        seen_ids.add(case_id)
        if not case.get("prompt") or not case.get("expected_output"):
            errors.append(f"eval case {case_id!r} is missing prompt or expected_output")
        if not case.get("expectations"):
            errors.append(f"eval case {case_id!r} has no expectations")

    if errors:
        print("Temps skill validation failed:")
        for error in errors:
            print(f"- {error}")
        return 1
    print(f"Temps skill validation passed ({len(command_files)} command groups).")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
