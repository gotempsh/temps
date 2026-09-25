#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Drop build output the current build no longer uses from a cached cargo dir.

Why this exists: the musl build cache (`temps-musl-fast-*` in rust-tests.yml)
restores from the newest older entry whenever Cargo.lock or the Dockerfile
changes, builds on top of it, and saves the result under the new key. Nothing
ever removes the previous generation. A dependency bump changes the cargo
metadata hash of every crate that depends on it, so each bump leaves a full
second copy of the workspace's rlibs, fingerprints and incremental sessions
behind, plus the old crate sources in the registry. The entry grew to 21GB
this way and was downloaded in full by every build.

Cargo reports every unit of the build with `--message-format=json`,
including units that were already fresh, so the live set is known exactly:

- `deps/`, `build/`, `.fingerprint/`: entries are named `<stem>-<hash>`.
  Anything whose 16-hex hash was not reported by this build is stale.
- `incremental/`: directories are named `<crate>-<rustc id>`, which cargo
  does not report. Keep the most recently written directory per crate name
  and drop the rest. Getting this wrong only costs incremental reuse for that
  crate on the next compile; it cannot produce a wrong binary.
- cargo registry: keep only crate archives and sources that Cargo.lock still
  references.

Pruning a live entry by mistake is never a correctness problem (cargo rebuilds
what is missing), and the workflow re-runs cargo after pruning, before it
saves, so a mistake is repaired before it reaches the cache and shows up as a
warning. Without `--apply` the script only reports what it would remove.
"""

import argparse
import filecmp
import json
import os
import re
import shutil
import sys
import tomllib
from pathlib import Path

HASHED_NAME = re.compile(r"^(?P<stem>.+?)-(?P<hash>[0-9a-f]{16})(?:\..*)?$")
# Build script crates all share this crate name, so "newest per crate name"
# would keep one of them and delete the rest. They are tiny; keep them all.
SHARED_INCREMENTAL_NAMES = {"build_script_build"}


def tree_size(path: Path) -> int:
    if path.is_symlink() or path.is_file():
        return path.lstat().st_size
    total = 0
    for root, _dirs, files in os.walk(path):
        for name in files:
            try:
                total += os.lstat(os.path.join(root, name)).st_size
            except FileNotFoundError:
                pass
    return total


def newest_mtime(path: Path) -> float:
    newest = path.lstat().st_mtime
    for root, dirs, files in os.walk(path):
        for name in dirs + files:
            try:
                newest = max(newest, os.lstat(os.path.join(root, name)).st_mtime)
            except FileNotFoundError:
                pass
    return newest


def gib(n: int) -> str:
    return f"{n / (1 << 30):.2f} GiB"


class Pruner:
    def __init__(self, apply: bool):
        self.apply = apply
        self.kept: dict[str, int] = {}
        self.removed: dict[str, int] = {}

    def keep(self, category: str, path: Path) -> None:
        self.kept[category] = self.kept.get(category, 0) + tree_size(path)

    def remove(self, category: str, path: Path) -> None:
        self.removed[category] = self.removed.get(category, 0) + tree_size(path)
        if not self.apply:
            return
        if path.is_dir() and not path.is_symlink():
            shutil.rmtree(path)
        else:
            path.unlink()

    def report(self) -> None:
        verb = "Removed" if self.apply else "Would remove"
        categories = sorted(set(self.kept) | set(self.removed))
        width = max(len(c) for c in categories) if categories else 10
        print(f"{'category':<{width}}  {'kept':>11}  {verb.lower():>12}")
        for category in categories:
            print(
                f"{category:<{width}}  {gib(self.kept.get(category, 0)):>11}  "
                f"{gib(self.removed.get(category, 0)):>12}"
            )
        total_kept = sum(self.kept.values())
        total_removed = sum(self.removed.values())
        print(f"{'total':<{width}}  {gib(total_kept):>11}  {gib(total_removed):>12}")
        print(f"{verb} {gib(total_removed)} of {gib(total_kept + total_removed)}.")


def resolve_uplifted(path: Path, deps_dir: Path) -> str | None:
    """Map an uplifted artifact (target/<profile>/temps) to its hashed copy.

    Cargo hard-links the hashed file in deps/ to the unhashed name on Linux
    and copies it on some platforms, so match by inode first, then contents.
    """
    if not path.exists():
        return None
    name = path.name
    base, dot, ext = name.partition(".")
    pattern = f"{base}-*{dot}{ext}" if dot else f"{base}-*"
    candidates = []
    for candidate in deps_dir.glob(pattern):
        match = HASHED_NAME.match(candidate.name)
        if match and match.group("stem") == base:
            candidates.append((candidate, match.group("hash")))
    stat = path.stat()
    for candidate, digest in candidates:
        if candidate.stat().st_ino == stat.st_ino:
            return digest
    for candidate, digest in candidates:
        if candidate.stat().st_size == stat.st_size and filecmp.cmp(
            candidate, path, shallow=False
        ):
            return digest
    return None


def live_units(messages: Path, container_target: str, profile_dir: Path):
    """Return (hashes, crate names) for every unit cargo reported."""
    hashes: set[str] = set()
    crate_names: set[str] = set()
    unresolved: list[str] = []
    finished = False
    prefix = container_target.rstrip("/") + "/"

    def local(path: str) -> Path | None:
        if not path.startswith(prefix):
            return None
        return profile_dir.parent / path[len(prefix):]

    for line in messages.read_text().splitlines():
        if not line.startswith("{"):
            continue
        message = json.loads(line)
        reason = message.get("reason")
        if reason == "build-finished":
            finished = message.get("success") is True
        elif reason == "compiler-artifact":
            crate_names.add(message["target"]["name"].replace("-", "_"))
            for filename in message.get("filenames", []):
                path = local(filename)
                if path is None:
                    continue
                parts = path.relative_to(profile_dir).parts
                if len(parts) >= 2 and parts[0] in ("deps", "build"):
                    match = HASHED_NAME.match(parts[1])
                    if match:
                        hashes.add(match.group("hash"))
                        continue
                digest = resolve_uplifted(path, profile_dir / "deps")
                if digest:
                    hashes.add(digest)
                else:
                    unresolved.append(filename)
        elif reason == "build-script-executed":
            path = local(message.get("out_dir", ""))
            if path is not None:
                match = HASHED_NAME.match(path.parent.name)
                if match:
                    hashes.add(match.group("hash"))

    return finished, hashes, crate_names, unresolved


def prune_target(pruner: Pruner, profile_dir: Path, hashes: set[str], crate_names: set[str]):
    for sub in ("deps", "build", ".fingerprint"):
        directory = profile_dir / sub
        if not directory.is_dir():
            continue
        for entry in directory.iterdir():
            match = HASHED_NAME.match(entry.name)
            if match and match.group("hash") not in hashes:
                pruner.remove(f"target/{sub}", entry)
            else:
                pruner.keep(f"target/{sub}", entry)

    incremental = profile_dir / "incremental"
    if not incremental.is_dir():
        return
    by_crate: dict[str, list[Path]] = {}
    for entry in incremental.iterdir():
        crate, sep, _ = entry.name.rpartition("-")
        if not sep:
            pruner.keep("target/incremental", entry)
            continue
        by_crate.setdefault(crate, []).append(entry)
    for crate, entries in by_crate.items():
        if crate in SHARED_INCREMENTAL_NAMES:
            for entry in entries:
                pruner.keep("target/incremental", entry)
            continue
        if crate not in crate_names:
            for entry in entries:
                pruner.remove("target/incremental", entry)
            continue
        entries.sort(key=newest_mtime, reverse=True)
        pruner.keep("target/incremental", entries[0])
        for entry in entries[1:]:
            pruner.remove("target/incremental", entry)


def prune_registry(pruner: Pruner, registry: Path, lockfile: Path):
    lock = tomllib.loads(lockfile.read_text())
    crates = {
        f"{package['name']}-{package['version']}"
        for package in lock.get("package", [])
        if package.get("source", "").startswith(("registry+", "sparse+"))
    }

    for index in sorted((registry / "cache").glob("*")):
        for archive in index.iterdir():
            if archive.name.removesuffix(".crate") in crates:
                pruner.keep("registry/cache", archive)
            else:
                pruner.remove("registry/cache", archive)
    for index in sorted((registry / "src").glob("*")):
        for source_dir in index.iterdir():
            if source_dir.name in crates:
                pruner.keep("registry/src", source_dir)
            else:
                pruner.remove("registry/src", source_dir)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--messages", type=Path, required=True,
                        help="stdout of `cargo build --message-format=json-render-diagnostics`")
    parser.add_argument("--target-dir", type=Path, required=True,
                        help="host path of the cargo target dir")
    parser.add_argument("--container-target", default="/build/target",
                        help="target dir path as cargo saw it (inside the build container)")
    parser.add_argument("--profile", default="fast")
    parser.add_argument("--registry", type=Path, help="host path of CARGO_HOME/registry")
    parser.add_argument("--lockfile", type=Path, default=Path("Cargo.lock"))
    parser.add_argument("--apply", action="store_true",
                        help="delete stale entries (default: report only)")
    args = parser.parse_args()

    profile_dir = args.target_dir / args.profile
    finished, hashes, crate_names, unresolved = live_units(
        args.messages, args.container_target, profile_dir
    )
    # An empty or truncated message log would make every entry look stale.
    if not finished:
        print(
            f"::warning::{args.messages} has no successful build-finished "
            "message; not pruning the cargo cache."
        )
        return 0
    if not hashes:
        print(
            f"::warning::No artifact in {args.messages} is under "
            f"{args.container_target}; not pruning the cargo cache."
        )
        return 0
    for filename in unresolved:
        print(f"::notice::No hashed copy found for {filename}; its unit may be rebuilt.")

    pruner = Pruner(apply=args.apply)
    print(f"{len(hashes)} live cargo units in {profile_dir}")
    prune_target(pruner, profile_dir, hashes, crate_names)
    if args.registry:
        prune_registry(pruner, args.registry, args.lockfile)
    pruner.report()
    return 0


if __name__ == "__main__":
    sys.exit(main())
