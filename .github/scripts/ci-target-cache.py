#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Make a cached cargo target dir reusable for the workspace's own crates.

Why this exists: Build Test Binaries restored every third-party crate from
cache and still spent 21 minutes compiling, because all 116 workspace crates
and their 234 test binaries were rebuilt on every run. Caching `target/` is
not enough by itself. Cargo decides a path crate is stale when one of its
source files is newer than its last build, and a fresh checkout stamps every
file with the checkout time, which is always newer. sccache does not help
much either: it never caches test binaries or other bin targets, which are
most of this build.

So the cache records which commit it was built from (`record`). Before the
next build (`prepare`), every tracked file is stamped with a fixed old time,
except files that differ from that commit, which get the current time. Cargo
then rebuilds exactly the crates whose sources changed, plus their
dependents, which it tracks itself.

`verify` checks the dangerous direction afterwards: a crate whose `.rs`
sources changed, but which cargo reported as fresh, means stale code would be
tested. It fails the job instead.

Fail closed. If the recorded commit is missing, malformed or cannot be
fetched, nothing is restamped. Every file keeps its checkout time and every
workspace crate rebuilds, which is what happened before this cache existed.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import re
import subprocess
import sys
import time
from collections.abc import Iterable
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent

# Where the cache records the commit it was built from.
STAMP = ".ci-source-commit"
# 2000-01-01T00:00:00Z: older than anything a build could have written.
OLD = 946684800
SHA = re.compile(r"^[0-9a-f]{40}$")
# Where cargo's targets live inside a package.
COMPILED_DIRS = frozenset({"src", "tests", "benches", "examples"})


def _load(name: str, filename: str):
    spec = importlib.util.spec_from_file_location(name, HERE / filename)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {filename}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def git(*args: str, root: Path = REPO_ROOT, check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(["git", *args], cwd=root, capture_output=True, check=check)


def recorded_commit(target: Path) -> str | None:
    try:
        sha = (target / STAMP).read_text(encoding="utf-8").strip()
    except OSError:
        return None
    return sha if SHA.fullmatch(sha) else None


def have_commit(sha: str, root: Path) -> bool:
    if git("cat-file", "-e", f"{sha}^{{commit}}", root=root, check=False).returncode == 0:
        return True
    git("fetch", "--no-tags", "--depth=1", "origin", sha, root=root, check=False)
    return git("cat-file", "-e", f"{sha}^{{commit}}", root=root, check=False).returncode == 0


def changed_since(sha: str, root: Path) -> list[str]:
    out = git("diff", "--name-only", "--no-renames", "-z", sha, "HEAD", root=root).stdout
    return [path for path in out.decode("utf-8").split("\0") if path]


def tracked_files(root: Path) -> list[str]:
    out = git("ls-files", "-z", root=root).stdout
    return [path for path in out.decode("utf-8").split("\0") if path]


def prepare(target: Path, root: Path = REPO_ROOT) -> str | None:
    """Restamp sources against the cached build; return its commit, or None."""
    sha = recorded_commit(target)
    if sha is None:
        print("No cached build to reuse; every workspace crate builds from scratch.")
        return None
    if not have_commit(sha, root):
        print(f"::warning title=Rebuilding every workspace crate::The cached build's commit {sha} is not fetchable.")
        return None
    try:
        changed = set(changed_since(sha, root))
    except subprocess.CalledProcessError as error:
        print(f"::warning title=Rebuilding every workspace crate::git diff {sha}..HEAD failed: {error.stderr!r}")
        return None
    now = time.time()
    for path in tracked_files(root):
        stamp = now if path in changed else OLD
        try:
            os.utime(root / path, (stamp, stamp), follow_symlinks=False)
        except (FileNotFoundError, NotImplementedError):
            continue
    print(f"Reusing the build of {sha}: {len(changed)} file(s) changed since, stamped as new.")
    return sha


def record(target: Path, root: Path = REPO_ROOT) -> str:
    sha = git("rev-parse", "HEAD", root=root).stdout.decode().strip()
    target.mkdir(parents=True, exist_ok=True)
    (target / STAMP).write_text(sha + "\n", encoding="utf-8")
    return sha


def rebuilt_packages(messages: Iterable[Path]) -> tuple[set[str], set[str]]:
    """Return (packages with a rebuilt unit, packages that were all fresh)."""
    rebuilt: set[str] = set()
    seen: set[str] = set()
    for path in messages:
        for line in path.read_text(encoding="utf-8").splitlines():
            if not line.startswith("{"):
                continue
            message = json.loads(line)
            if message.get("reason") != "compiler-artifact":
                continue
            package_id = message.get("package_id", "")
            if not package_id.startswith("path+"):
                continue
            # path+file:///.../crates/temps-core#temps-core@0.1.0, or
            # path+file:///.../crates/temps-core#0.1.0 when the name matches
            # the directory.
            location, _, fragment = package_id.partition("#")
            name = fragment.split("@")[0] if "@" in fragment else location.rstrip("/").rsplit("/", 1)[-1]
            seen.add(name)
            if message.get("fresh") is False:
                rebuilt.add(name)
    return rebuilt, seen - rebuilt


def verify(base: str, messages: list[Path], root: Path = REPO_ROOT) -> int:
    """Fail when a crate with changed Rust sources was not rebuilt."""
    affected = _load("ci_affected_crates", "ci-affected-crates.py")
    workspace = affected.load_workspace(root)
    expected = set()
    for path in changed_since(base, root):
        crate = workspace.crate_of(path)
        if crate is None or not path.endswith(".rs") or not (root / path).exists():
            continue
        # Only files cargo compiles; a changed fixture that happens to end in
        # `.rs` rebuilds nothing, legitimately.
        inside = Path(path).relative_to(workspace.dirs[crate]).parts
        if inside[0] in COMPILED_DIRS or inside == ("build.rs",):
            expected.add(crate)
    rebuilt, fresh = rebuilt_packages(messages)
    stale = sorted(expected & fresh)
    if stale:
        print(
            "::error title=Stale build from cache::"
            f"{', '.join(stale)} changed since {base} but cargo reused the cached build. "
            "Delete the test-target cache and re-run."
        )
        return 1
    print(f"{len(expected)} changed crate(s) rebuilt as expected; {len(rebuilt)} package(s) compiled in total.")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("prepare", "record"):
        command = commands.add_parser(name)
        command.add_argument("--target", type=Path, default=REPO_ROOT / "target")
    verify_cmd = commands.add_parser("verify")
    verify_cmd.add_argument("--base", required=True)
    verify_cmd.add_argument("messages", nargs="+", type=Path)
    args = parser.parse_args(argv)

    if args.command == "prepare":
        sha = prepare(args.target)
        output = os.environ.get("GITHUB_OUTPUT")
        if output:
            with open(output, "a", encoding="utf-8") as handle:
                handle.write(f"base={sha or ''}\n")
        return 0
    if args.command == "record":
        print(f"Recorded {record(args.target)} as this build's source commit.")
        return 0
    if not SHA.fullmatch(args.base):
        print("No cached build was reused; nothing to verify.")
        return 0
    return verify(args.base, args.messages)


if __name__ == "__main__":
    sys.exit(main())
