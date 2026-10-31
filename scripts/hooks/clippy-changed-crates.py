#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Run clippy only on the crates that own the staged Rust files.

Pre-commit passes the staged `.rs` paths; each is mapped to the nearest
directory with a Cargo.toml, and clippy runs once over those packages.
The full-workspace `--all-features` clippy runs at pre-push and in CI.
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent


def package_for(path: Path) -> str | None:
    directory = (ROOT / path).resolve().parent
    while directory != ROOT and ROOT in directory.parents:
        manifest = directory / "Cargo.toml"
        if manifest.exists():
            in_package = False
            for line in manifest.read_text(encoding="utf-8").splitlines():
                stripped = line.strip()
                if stripped.startswith("["):
                    in_package = stripped == "[package]"
                elif in_package:
                    match = re.match(r'name\s*=\s*"([^"]+)"', stripped)
                    if match:
                        return match.group(1)
            return None
        directory = directory.parent
    return None


def main(paths: list[str]) -> int:
    packages = sorted({p for p in (package_for(Path(path)) for path in paths) if p})
    if not packages:
        return 0
    command = ["cargo", "clippy", "--all-targets"]
    for package in packages:
        command += ["-p", package]
    command += ["--", "-D", "warnings"]
    print("+", " ".join(command), flush=True)
    return subprocess.call(command, cwd=ROOT)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
