#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Work out which workspace crates' tests a pull request can affect.

Why this exists: every pull request ran every test in the workspace, so a
one-line change to the proxy waited on the PostgreSQL upgrade orchestrator,
the backup suites and 115 other crates whose code it cannot reach. A crate's
tests can only change outcome when the crate itself changes, when one of its
(transitive) workspace dependencies changes, or when a file it reads at
compile or test time changes. This script computes that set, so a pull
request runs only those tests. The merge queue and every push to `main` still
run all of them (rust-tests.yml), which is what catches what this misses.

Outputs, as GitHub Actions outputs:

  tests         a nextest filterset of the affected crates (`all()` when the
                change can reach anything, `none()` when it reaches no crate)
  docker_tests  "false" when neither crate built with the `docker-tests`
                feature is affected, so that archive and the jobs running it
                can be skipped

How a changed file maps to crates:

  - inside a workspace member's directory: that crate;
  - read by a crate through a relative path literal (`include_str!`,
    `include_bytes!`, or a `CARGO_MANIFEST_DIR`-relative path that starts
    with `../`): the crate that reads it, wherever the file lives;
  - documentation and the frontend areas classify-ci-changes.py recognises:
    nothing beyond the readers above;
  - anything else -- the root manifest and lockfile, `.cargo/`, `.config/`,
    `.github/`, `scripts/`, a patched dependency, a directory that is not a
    member: every crate.

Then every crate that depends on an affected crate, through normal, dev or
build dependencies, transitively, is affected too.

Fail closed. A manifest that cannot be parsed, or an unreadable diff, makes
every crate affected; a crash leaves the outputs unset, which rust-tests.yml
reads as "run everything". A wrong subset merges an untested change into the
merge queue, which then fails; a wrong "everything" only costs runner time.
"""

from __future__ import annotations

import argparse
import importlib.util
import os
import re
import subprocess
import sys
import tomllib
from collections.abc import Iterable
from pathlib import Path, PurePosixPath

HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent

# The crates rust-tests.yml builds a second archive for with their
# `docker-tests` feature enabled.
DOCKER_TESTS_CRATES = frozenset({"temps-providers", "temps-backup"})

DEPENDENCY_TABLES = ("dependencies", "dev-dependencies", "build-dependencies")

# A string literal holding a relative path that leaves the directory it is
# resolved against. Optional leading `/` covers
# `concat!(env!("CARGO_MANIFEST_DIR"), "/../web/...")`.
RELATIVE_PATH_LITERAL = re.compile(r'"/?((?:\.\./)+[A-Za-z0-9_./@+-]*)"')

# Package names are what the filterset is built from; keep them to the
# characters Cargo itself allows, so nothing else can reach the expression.
PACKAGE_NAME = re.compile(r"^[A-Za-z0-9_-]+$")

OUTPUTS = ("tests", "docker_tests")
ALL = "all()"
NONE = "none()"


def _load_classifier():
    spec = importlib.util.spec_from_file_location(
        "classify_ci_changes", HERE / "classify-ci-changes.py"
    )
    if spec is None or spec.loader is None:
        raise RuntimeError("cannot load classify-ci-changes.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


classifier = _load_classifier()


class WorkspaceError(Exception):
    """The workspace could not be read; callers treat it as "everything"."""


class Workspace:
    """The workspace members, their dependencies and what else they read."""

    def __init__(
        self,
        dirs: dict[str, str],
        dependencies: dict[str, set[str]],
        readers: dict[str, set[str]] | None = None,
    ) -> None:
        # crate name -> repository-relative directory, POSIX, no trailing `/`
        self.dirs = dirs
        # crate name -> names of the workspace crates it depends on
        self.dependencies = dependencies
        # repository-relative path -> crates that read it via a relative path
        self.readers = readers or {}

    def crate_of(self, path: str) -> str | None:
        """Return the member whose directory holds `path` (the deepest one)."""
        best: str | None = None
        best_len = -1
        for name, directory in self.dirs.items():
            if (path == directory or path.startswith(directory + "/")) and len(directory) > best_len:
                best, best_len = name, len(directory)
        return best

    def readers_of(self, path: str) -> set[str]:
        """Return the crates that read `path`, or a directory containing it."""
        found: set[str] = set()
        for read_path, crates in self.readers.items():
            if path == read_path or path.startswith(read_path + "/"):
                found |= crates
        return found

    def dependents(self, crates: Iterable[str]) -> set[str]:
        """Return `crates` plus every member that depends on one, transitively."""
        reverse: dict[str, set[str]] = {name: set() for name in self.dirs}
        for name, deps in self.dependencies.items():
            for dep in deps:
                reverse.setdefault(dep, set()).add(name)
        affected = set(crates)
        pending = list(affected)
        while pending:
            for dependent in reverse.get(pending.pop(), ()):
                if dependent not in affected:
                    affected.add(dependent)
                    pending.append(dependent)
        return affected


def _normalise(path: PurePosixPath) -> str | None:
    """Collapse `..` segments; None when the path leaves the repository."""
    parts: list[str] = []
    for part in path.parts:
        if part in ("", "."):
            continue
        if part == "..":
            if not parts:
                return None
            parts.pop()
        else:
            parts.append(part)
    return "/".join(parts) or None


def _read_toml(path: Path) -> dict:
    try:
        with path.open("rb") as handle:
            return tomllib.load(handle)
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise WorkspaceError(f"cannot read {path}: {error}") from error


def _dependency_tables(manifest: dict) -> Iterable[dict]:
    for table in DEPENDENCY_TABLES:
        yield manifest.get(table, {})
    for target in manifest.get("target", {}).values():
        for table in DEPENDENCY_TABLES:
            yield target.get(table, {})


def load_workspace(root: Path = REPO_ROOT) -> Workspace:
    """Read every member's manifest and the relative paths its sources read."""
    workspace_manifest = _read_toml(root / "Cargo.toml")
    workspace = workspace_manifest.get("workspace", {})
    members: list[str] = []
    for pattern in workspace.get("members", []):
        matches = sorted(p for p in root.glob(pattern) if (p / "Cargo.toml").is_file())
        if not matches:
            raise WorkspaceError(f"workspace member {pattern!r} matches no crate")
        members.extend(str(p.relative_to(root).as_posix()) for p in matches)
    if not members:
        raise WorkspaceError("the workspace lists no members")

    # `foo = { workspace = true }` resolves through [workspace.dependencies].
    workspace_deps = workspace.get("dependencies", {})

    dirs: dict[str, str] = {}
    manifests: dict[str, dict] = {}
    for member in members:
        manifest = _read_toml(root / member / "Cargo.toml")
        name = manifest.get("package", {}).get("name")
        if not isinstance(name, str) or not PACKAGE_NAME.fullmatch(name):
            raise WorkspaceError(f"{member}/Cargo.toml has no usable package name: {name!r}")
        dirs[name] = member
        manifests[name] = manifest
    by_dir = {directory: name for name, directory in dirs.items()}

    dependencies: dict[str, set[str]] = {}
    for name, manifest in manifests.items():
        deps: set[str] = set()
        for table in _dependency_tables(manifest):
            for key, spec in table.items():
                if not isinstance(spec, dict):
                    continue
                if spec.get("workspace") is True:
                    inherited = workspace_deps.get(key)
                    if not isinstance(inherited, dict) or "path" not in inherited:
                        continue
                    target = _normalise(PurePosixPath(inherited["path"]))
                elif "path" in spec:
                    target = _normalise(PurePosixPath(dirs[name]) / spec["path"])
                else:
                    continue
                if target is None or target not in by_dir:
                    # A path dependency outside the workspace (a patch, a
                    # vendored crate) is still something this crate builds
                    # from. Record the directory as read, so a change there
                    # reaches it.
                    if target is not None:
                        deps.add(f"path:{target}")
                    continue
                deps.add(by_dir[target])
        dependencies[name] = deps

    readers: dict[str, set[str]] = {}
    for name, deps in dependencies.items():
        for dep in [d for d in deps if d.startswith("path:")]:
            deps.discard(dep)
            readers.setdefault(dep.removeprefix("path:"), set()).add(name)
    for name, directory in dirs.items():
        crate_dir = root / directory
        for source in crate_dir.rglob("*.rs"):
            relative = source.relative_to(root)
            if "target" in relative.parts:
                continue
            try:
                text = source.read_text(encoding="utf-8", errors="replace")
            except OSError as error:
                raise WorkspaceError(f"cannot read {relative}: {error}") from error
            for literal in RELATIVE_PATH_LITERAL.findall(text):
                # include_str! resolves against the file that contains it,
                # CARGO_MANIFEST_DIR paths against the crate. Record both:
                # an extra reader costs a few tests, a missing one coverage.
                #
                # Only paths that exist count. Most `../` literals in this
                # workspace are path-traversal test inputs ("../outside",
                # "../.."), and treating one as a read of the whole parent
                # directory would make every change reach that crate.
                if not literal.strip("./"):
                    continue
                for base in (relative.parent.as_posix(), directory):
                    resolved = _normalise(PurePosixPath(base) / literal)
                    if resolved is None or not (root / resolved).exists():
                        continue
                    if resolved == directory or resolved.startswith(directory + "/"):
                        continue  # inside the crate already
                    readers.setdefault(resolved.rstrip("/"), set()).add(name)

    return Workspace(dirs=dirs, dependencies=dependencies, readers=readers)


def affected_crates(workspace: Workspace, paths: Iterable[str]) -> set[str] | None:
    """Return the crates whose tests `paths` can affect; None means all of them."""
    paths = [path for path in paths if path]
    if not paths:
        return None  # an empty diff is more likely broken than a no-op
    seeds: set[str] = set()
    for path in paths:
        seeds |= workspace.readers_of(path)
        crate = workspace.crate_of(path)
        if crate is not None:
            seeds.add(crate)
        elif classifier.area(path) == "rust":
            return None
    return workspace.dependents(seeds)


def filterset(crates: set[str] | None) -> str:
    """Return the nextest filterset selecting the tests of `crates`."""
    if crates is None:
        return ALL
    if not crates:
        return NONE
    return " | ".join(f"package(={name})" for name in sorted(crates))


def outputs(workspace: Workspace | None, paths: Iterable[str]) -> dict[str, str]:
    crates = None if workspace is None else affected_crates(workspace, paths)
    docker = crates is None or bool(crates & DOCKER_TESTS_CRATES)
    return {"tests": filterset(crates), "docker_tests": "true" if docker else "false"}


def write_outputs(result: dict[str, str]) -> None:
    lines = [f"{key}={result[key]}" for key in OUTPUTS]
    output_path = os.environ.get("GITHUB_OUTPUT")
    if output_path:
        with Path(output_path).open("a", encoding="utf-8") as output:
            output.writelines(f"{line}\n" for line in lines)
    for line in lines:
        print(line)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--base", required=True, help="revision the change is compared against")
    parser.add_argument("--head", required=True, help="revision holding the change")
    args = parser.parse_args(argv)

    try:
        paths = classifier.changed_paths(args.base, args.head)
    except RuntimeError as error:
        print(f"ci-affected-crates: {error}; leaving every output unset", file=sys.stderr)
        return 1

    try:
        workspace: Workspace | None = load_workspace()
    except WorkspaceError as error:
        print(f"::warning title=Testing every crate::{error}")
        workspace = None

    crates = None if workspace is None else affected_crates(workspace, paths)
    if crates is None:
        print(f"Every crate's tests run ({len(paths)} file(s) changed).")
    else:
        total = len(workspace.dirs) if workspace else 0
        print(f"{len(crates)} of {total} crates are reachable from this change:")
        for name in sorted(crates):
            print(f"  {name}")
    write_outputs(outputs(workspace, paths))
    return 0


if __name__ == "__main__":
    sys.exit(main())
