#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Validate the Temps skill structure and local Markdown references."""

from __future__ import annotations

import json
import re
from pathlib import Path


LINK = re.compile(r"\[[^]]*\]\(([^)]+)\)")
CLI_SKILLS = ("temps", "temps-cli", "temps-platform-setup")


# Dist-tags the skills may run: `latest` while the documented CLI is a release,
# `beta` while it is a prerelease. SDK release preparation moves them together.
CLI_CHANNEL = re.compile(r"@temps-sdk/cli@(latest|beta)(?![\w.-])")
FIXED_PIN = re.compile(r"(?:bunx|npx) @temps-sdk/cli@(?!(?:latest|beta)(?![\w.-]))[^\s`]+")


def check_cli_references(skills_root: Path) -> list[str]:
    """Reject fixed version pins and mixed channels while allowing source attribution."""
    errors = []
    channels: dict[str, str] = {}
    for skill in CLI_SKILLS:
        root = skills_root / skill
        if not root.is_dir():
            errors.append(f"missing skill: {skill}")
            continue
        for markdown in root.rglob("*.md"):
            text = markdown.read_text()
            if FIXED_PIN.search(text):
                errors.append(f"{markdown.relative_to(skills_root)}: stale CLI invocation pin")
            for channel in CLI_CHANNEL.findall(text):
                channels.setdefault(channel, str(markdown.relative_to(skills_root)))
            if "expected_temps_cli_integrity=" in text:
                errors.append(f"{markdown.relative_to(skills_root)}: stale static integrity guard")
    if len(channels) > 1:
        found = ", ".join(f"@{channel} in {path}" for channel, path in sorted(channels.items()))
        errors.append(f"CLI invocations mix channels ({found}); use one channel everywhere")
    return errors


def main() -> int:
    root = Path(__file__).resolve().parents[1]
    errors: list[str] = check_cli_references(root.parent)
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
