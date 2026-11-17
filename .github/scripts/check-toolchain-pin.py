#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Fail when a workflow installs a Rust compiler other than the pinned one.

rust-toolchain.toml pins the compiler so `clippy -D warnings` cannot turn red
because a new stable release added a lint. That only holds if CI installs the
same version: `dtolnay/rust-toolchain@stable`, or a `toolchain:` input that
drifted from the file, would bring the floating compiler back.

Rules:
- every dtolnay/rust-toolchain step passes `toolchain:` equal to the channel
  in rust-toolchain.toml (nightly is allowed for fuzzing, which needs it);
- the Dockerfile's `rust:` base image either matches the channel or sets
  `RUSTUP_TOOLCHAIN` to its own version, so the committed toolchain file
  cannot silently swap the compiler that produced committed artifacts.
"""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
ALLOWED_EXTRA = {"nightly"}


def pinned_channel(root: Path) -> str:
    data = tomllib.loads((root / "rust-toolchain.toml").read_text(encoding="utf-8"))
    return data["toolchain"]["channel"]


def workflow_problems(path: Path, channel: str) -> list[str]:
    problems = []
    lines = path.read_text(encoding="utf-8").split("\n")
    for index, line in enumerate(lines):
        match = re.search(r"uses:\s*dtolnay/rust-toolchain@(\S+)", line)
        if not match:
            continue
        ref = match.group(1)
        where = f"{path.name}:{index + 1}"
        if ref in ALLOWED_EXTRA:
            continue
        # The step's keys sit deeper than the `uses:` key itself.
        step_indent = len(line) - len(line.lstrip(" -"))
        toolchain = None
        for follow in lines[index + 1 :]:
            if follow.strip() and len(follow) - len(follow.lstrip(" ")) < step_indent:
                break
            if follow.lstrip().startswith("- "):
                break
            found = re.match(r"^\s*toolchain:\s*['\"]?([^'\"\s#]+)", follow)
            if found:
                toolchain = found.group(1)
                break
        if toolchain is None:
            problems.append(f"{where}: dtolnay/rust-toolchain@{ref} without `toolchain: {channel}`")
        elif toolchain != channel and toolchain not in ALLOWED_EXTRA:
            problems.append(f"{where}: toolchain {toolchain} does not match rust-toolchain.toml ({channel})")
    return problems


def dockerfile_problems(path: Path, channel: str) -> list[str]:
    if not path.exists():
        return []
    text = path.read_text(encoding="utf-8")
    problems = []
    for match in re.finditer(r"^FROM\s+rust:(\d+\.\d+(?:\.\d+)?)", text, re.M):
        version = match.group(1)
        if version == channel:
            continue
        if not re.search(rf"^ENV\s+RUSTUP_TOOLCHAIN={re.escape(version)}\s*$", text, re.M):
            problems.append(
                f"{path.name}: base image rust:{version} differs from rust-toolchain.toml ({channel}) "
                f"and does not set ENV RUSTUP_TOOLCHAIN={version}"
            )
    return problems


def main(root: Path = ROOT) -> int:
    channel = pinned_channel(root)
    problems: list[str] = []
    for workflow in sorted((root / ".github" / "workflows").glob("*.yml")):
        problems.extend(workflow_problems(workflow, channel))
    problems.extend(dockerfile_problems(root / "Dockerfile", channel))
    if problems:
        print("Rust toolchain pins disagree with rust-toolchain.toml:", file=sys.stderr)
        for problem in problems:
            print(f"  {problem}", file=sys.stderr)
        return 1
    print(f"All Rust toolchain installs use {channel}.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
