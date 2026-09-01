#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Add and verify Temps SPDX attribution headers on first-party source files."""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path


COPYRIGHT_TEXT = "SPDX-FileCopyrightText: 2024-2026 Temps Contributors"
LICENSE_TEXT = "SPDX-License-Identifier: MIT OR Apache-2.0"

SLASH_SUFFIXES = {
    ".c",
    ".cc",
    ".cpp",
    ".cjs",
    ".go",
    ".h",
    ".hpp",
    ".js",
    ".jsx",
    ".mjs",
    ".proto",
    ".rs",
    ".ts",
    ".tsx",
}
HASH_SUFFIXES = {
    ".bash",
    ".nft",
    ".py",
    ".rb",
    ".sh",
    ".toml",
    ".yaml",
    ".yml",
    ".zsh",
}
SQL_SUFFIXES = {".sql"}
BLOCK_SUFFIXES = {".css", ".sass", ".scss"}
HTML_SUFFIXES = {".html", ".htm", ".svelte", ".vue"}
HASH_FILENAMES = {"Containerfile", "Justfile", "Makefile"}


class AttributionError(Exception):
    """Raised when a source file cannot be annotated safely."""


def repository_root() -> Path:
    return Path(__file__).resolve().parents[1]


def comment_prefix(path: Path) -> str | None:
    suffix = path.suffix.lower()
    if suffix in SLASH_SUFFIXES:
        return "//"
    if suffix in HASH_SUFFIXES:
        return "#"
    if suffix in SQL_SUFFIXES:
        return "--"
    if suffix in BLOCK_SUFFIXES:
        return "/*"
    if suffix in HTML_SUFFIXES:
        return "<!--"
    if path.name in HASH_FILENAMES or path.name.startswith(("Dockerfile", "Containerfile")):
        return "#"
    return None


def is_source_file(path: Path) -> bool:
    return comment_prefix(path) is not None


def tracked_source_files(root: Path) -> list[Path]:
    result = subprocess.run(
        ["git", "ls-files", "-z"],
        cwd=root,
        check=True,
        capture_output=True,
    )
    return sorted(
        (root / name for name in result.stdout.decode().split("\0") if name),
        key=lambda path: path.as_posix(),
    )


def source_files_for_paths(root: Path, values: list[str]) -> list[Path]:
    if not values:
        return [path for path in tracked_source_files(root) if is_source_file(path)]

    files: set[Path] = set()
    for value in values:
        candidate = (root / value).resolve()
        try:
            candidate.relative_to(root)
        except ValueError as error:
            raise AttributionError(f"path is outside the repository: {value}") from error

        if candidate.is_dir():
            files.update(path for path in candidate.rglob("*") if path.is_file() and is_source_file(path))
        elif candidate.is_file() and is_source_file(candidate):
            files.add(candidate)
        elif not candidate.exists():
            raise AttributionError(f"path does not exist: {value}")

    return sorted(files, key=lambda path: path.as_posix())


def has_attribution(contents: str) -> bool:
    first_lines = "\n".join(contents.splitlines()[:12])
    return COPYRIGHT_TEXT in first_lines and LICENSE_TEXT in first_lines


def render_header(path: Path, newline: str) -> str:
    prefix = comment_prefix(path)
    if prefix == "/*":
        return f"/* {COPYRIGHT_TEXT} */{newline}/* {LICENSE_TEXT} */{newline}{newline}"
    if prefix == "<!--":
        return f"<!-- {COPYRIGHT_TEXT} -->{newline}<!-- {LICENSE_TEXT} -->{newline}{newline}"
    if prefix is None:
        raise AttributionError(f"unsupported source format: {path}")
    return f"{prefix} {COPYRIGHT_TEXT}{newline}{prefix} {LICENSE_TEXT}{newline}{newline}"


def insertion_offset(path: Path, contents: str) -> int:
    lines = contents.splitlines(keepends=True)
    if not lines:
        return 0

    offset = 0
    line_index = 0

    if lines[0].startswith("#!") and not lines[0].startswith("#!["):
        offset += len(lines[0])
        line_index = 1

    if path.suffix.lower() == ".py" and line_index < len(lines):
        if re.match(r"^[ \t]*#.*coding[:=][ \t]*[-\w.]+", lines[line_index]):
            offset += len(lines[line_index])

    if path.suffix.lower() in BLOCK_SUFFIXES and lines[0].lstrip().lower().startswith("@charset"):
        offset = len(lines[0])

    if path.suffix.lower() in HTML_SUFFIXES:
        first_line = lines[0].lstrip().lower()
        if first_line.startswith("<!doctype") or first_line.startswith("<?xml"):
            offset = len(lines[0])

    return offset


def annotate_file(path: Path) -> bool:
    raw = path.read_bytes()
    try:
        contents = raw.decode("utf-8")
    except UnicodeDecodeError as error:
        raise AttributionError(f"source file is not UTF-8: {path}") from error

    if has_attribution(contents):
        return False

    first_lines = "\n".join(contents.splitlines()[:12])
    if "SPDX-FileCopyrightText:" in first_lines or "SPDX-License-Identifier:" in first_lines:
        raise AttributionError(f"existing SPDX header requires manual review: {path}")

    newline = "\r\n" if b"\r\n" in raw[:4096] else "\n"
    if contents.strip():
        offset = insertion_offset(path, contents)
        updated = contents[:offset] + render_header(path, newline) + contents[offset:]
    else:
        updated = render_header(path, newline).rstrip() + newline
    path.write_bytes(updated.encode("utf-8"))
    return True


def check_files(root: Path, files: list[Path]) -> int:
    missing: list[str] = []
    unreadable: list[str] = []
    for path in files:
        try:
            contents = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as error:
            unreadable.append(f"{path.relative_to(root)}: {error}")
            continue
        if not has_attribution(contents):
            missing.append(path.relative_to(root).as_posix())

    if not missing and not unreadable:
        print(f"Attribution check passed for {len(files)} source files.")
        return 0

    if missing:
        print("Source files missing the Temps SPDX attribution header:", file=sys.stderr)
        for path in missing:
            print(f"  {path}", file=sys.stderr)
    if unreadable:
        print("Source files that could not be checked:", file=sys.stderr)
        for error in unreadable:
            print(f"  {error}", file=sys.stderr)
    print("Run: python3 scripts/source_attribution.py annotate", file=sys.stderr)
    return 1


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("annotate", "check"))
    parser.add_argument("paths", nargs="*", help="repository-relative files or directories")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    root = repository_root()
    try:
        files = source_files_for_paths(root, args.paths)
        if args.command == "check":
            return check_files(root, files)

        changed = sum(1 for path in files if annotate_file(path))
        print(f"Added attribution headers to {changed} of {len(files)} source files.")
        return 0
    except (AttributionError, OSError, subprocess.CalledProcessError) as error:
        print(f"Attribution error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
