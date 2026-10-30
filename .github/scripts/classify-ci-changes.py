#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Decide which build and test jobs of rust-tests.yml a pull request needs.

Why this exists: a pull request that only edits documentation still paid for
the musl build, the test-binary build and the whole E2E and integration matrix,
roughly an hour of runner time that cannot catch anything in a page of prose.
A pull request that only edits the console or the TypeScript CLI paid the same:
Cargo Check, Clippy, the test binaries and every Rust test group, none of which
read a line of TypeScript.

This script sorts each changed file into an area and reports, as GitHub
Actions outputs, which areas the pull request touches:

  code  anything other than documentation
  rust  anything other than documentation, `web/`, `apps/temps-cli/` and
        `skills/` -- that is, anything that can change the compiled `temps`
        binary, other than the console bundle it embeds, or that a Rust test
        reads
  web   the console under `web/`
  cli   the TypeScript CLI under `apps/temps-cli/`

`rust=false` is what lets rust-tests.yml skip the Rust jobs and reuse the
`temps` binary built for `main` (see find-reusable-binary.py) instead of
compiling one.

Fail closed. Anything this script does not positively recognise as one of
those areas counts as Rust, an empty or unreadable diff counts as Rust, and a
crash leaves every output unset, which rust-tests.yml treats as "run it". A
wrong `false` merges untested code; a wrong `true` only costs runner time.

The non-Rust areas are deliberately narrow. `web/` reaches the binary only
through `crates/temps-cli/build.rs`, which bundles it into the embedded
console; the E2E job tests that bundle separately when the binary is reused.
`apps/temps-cli/` and `skills/` are not read by any crate. A file elsewhere
that only looks like frontend code (`packages/`, `sdks/`, `apps/temps-e2e/`)
stays Rust because the scenario E2E suite builds it.

Documentation means a file under `docs/` or a Markdown file at the repository
root. Markdown elsewhere is not exempt: some of it is build or test input
(`crates/temps-core/templates/README.md` is embedded into the binary, and test
fixtures carry their own READMEs).

Some pages under `docs/` are test input as well. `scripts/test-compose-security.sh`
runs the upgrade guide's shell blocks and checks both installation pages for
the private `.env` instruction, so an edit to those pages has to run the
harness. They are listed in TEST_INPUT_DOCS, and
`test-classify-ci-changes.py` fails when a script under `scripts/` or
`.github/` starts reading a `docs/` file that is missing from that list.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from collections.abc import Callable, Iterable
from pathlib import Path

# Pages under docs/ that CI executes or asserts on. Edits to them run full CI.
TEST_INPUT_DOCS = frozenset(
    {
        "docs/installation/page.mdx",
        "docs/upgrade/page.mdx",
    }
)


# Areas whose files cannot change the `temps` binary's Rust code. Prefix match
# on the full directory, so `web-foo/` or `apps/temps-cli-ee/` stay Rust.
FRONTEND_AREAS = {
    "web/": "web",
    "apps/temps-cli/": "cli",
    "skills/": "skills",
}

EMPTY_DIFF = "<empty diff>"


def is_documentation(path: str) -> bool:
    """Return True when changing `path` cannot affect any rust-tests.yml job."""
    if path in TEST_INPUT_DOCS:
        return False
    if path.startswith("docs/"):
        return True
    return "/" not in path and path.endswith(".md")


def area(path: str) -> str:
    """Return the area of `path`: docs, web, cli, skills or rust."""
    if is_documentation(path):
        return "docs"
    for prefix, name in FRONTEND_AREAS.items():
        if path.startswith(prefix):
            return name
    return "rust"


def _first_path(paths: Iterable[str], matches: Callable[[str], bool]) -> str | None:
    seen = False
    for path in paths:
        if not path:
            continue
        seen = True
        if matches(path):
            return path
    # An empty change set returns a sentinel rather than None: a diff that
    # lists nothing is far more likely to be broken than a no-op pull request.
    return None if seen else EMPTY_DIFF


def first_code_path(paths: Iterable[str]) -> str | None:
    """Return the first changed path that is not documentation, or None."""
    return _first_path(paths, lambda path: area(path) != "docs")


def first_rust_path(paths: Iterable[str]) -> str | None:
    """Return the first changed path that needs the Rust jobs, or None."""
    return _first_path(paths, lambda path: area(path) == "rust")


def classify(paths: Iterable[str]) -> dict[str, str]:
    """Return the rust-tests.yml outputs for a change set, plus the reasons."""
    paths = [path for path in paths if path]
    areas = {area(path) for path in paths}
    code_reason = first_code_path(paths)
    rust_reason = first_rust_path(paths)
    rust = rust_reason is not None
    return {
        "code": "true" if code_reason is not None else "false",
        "rust": "true" if rust else "false",
        # Every area is assumed touched when the Rust jobs run, so a consumer
        # gating on `web` or `cli` alone can never skip work a full run does.
        "web": "true" if rust or "web" in areas else "false",
        "cli": "true" if rust or "cli" in areas else "false",
        "code_reason": code_reason or "",
        "rust_reason": rust_reason or "",
    }


def changed_paths(base: str, head: str) -> list[str]:
    # --no-renames reports a rename as a deletion plus an addition. Without it,
    # moving crates/foo.rs to docs/foo.md lists only the docs/ side and the
    # deleted source file would never be seen.
    result = subprocess.run(
        ["git", "diff", "--name-only", "--no-renames", "-z", base, head],
        check=False,
        capture_output=True,
    )
    if result.returncode != 0:
        stderr = result.stderr.decode("utf-8", "replace").strip()
        raise RuntimeError(
            f"git diff {base}..{head} failed with exit code {result.returncode}: {stderr}"
        )
    return [path for path in result.stdout.decode("utf-8").split("\0") if path]


OUTPUTS = ("code", "rust", "web", "cli")


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
        paths = changed_paths(args.base, args.head)
    except RuntimeError as error:
        print(f"classify-ci-changes: {error}; leaving every output unset", file=sys.stderr)
        return 1

    result = classify(paths)
    count = f"{len(paths)} file(s) changed"
    rust_reason = result["rust_reason"]
    if result["code"] == "false":
        print(f"All {count} are documentation; skipping build and test jobs.")
    elif result["rust"] == "false":
        touched = sorted({area(path) for path in paths} - {"docs"})
        print(
            f"No Rust input changed ({count}, areas: {', '.join(touched)}); "
            "skipping the Rust jobs and reusing main's temps binary where one exists."
        )
    elif rust_reason in TEST_INPUT_DOCS:
        print(f"Full CI required: {rust_reason} is read by CI ({count}).")
    else:
        print(f"Full CI required: {rust_reason} is a Rust input ({count}).")
    write_outputs(result)
    return 0


if __name__ == "__main__":
    sys.exit(main())
