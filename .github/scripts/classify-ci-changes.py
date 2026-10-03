#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Decide whether a pull request needs the build and test jobs of rust-tests.yml.

Why this exists: a pull request that only edits documentation still paid for
the musl build, the test-binary build and the whole E2E and integration matrix,
roughly an hour of runner time that cannot catch anything in a page of prose.
This script lists the files a pull request changes and reports `code=false`
when every one of them is documentation, so those jobs can be skipped.

Fail closed. Anything this script does not positively recognise as
documentation counts as code, an empty or unreadable diff counts as code, and a
crash leaves the `code` output unset, which rust-tests.yml treats as code too.
A wrong `false` merges untested code; a wrong `true` only costs runner time.

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
from collections.abc import Iterable
from pathlib import Path

# Pages under docs/ that CI executes or asserts on. Edits to them run full CI.
TEST_INPUT_DOCS = frozenset(
    {
        "docs/installation/page.mdx",
        "docs/upgrade/page.mdx",
    }
)


def is_documentation(path: str) -> bool:
    """Return True when changing `path` cannot affect any rust-tests.yml job."""
    if path in TEST_INPUT_DOCS:
        return False
    if path.startswith("docs/"):
        return True
    return "/" not in path and path.endswith(".md")


def first_code_path(paths: Iterable[str]) -> str | None:
    """Return the first changed path that needs full CI, or None if all are docs.

    An empty change set returns a sentinel rather than None: a diff that lists
    nothing is far more likely to be a broken diff than a no-op pull request.
    """
    seen = False
    for path in paths:
        if not path:
            continue
        seen = True
        if not is_documentation(path):
            return path
    return None if seen else "<empty diff>"


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


def write_output(code: bool) -> None:
    value = "true" if code else "false"
    output_path = os.environ.get("GITHUB_OUTPUT")
    if output_path:
        with Path(output_path).open("a", encoding="utf-8") as output:
            output.write(f"code={value}\n")
    print(f"code={value}")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--base", required=True, help="revision the change is compared against")
    parser.add_argument("--head", required=True, help="revision holding the change")
    args = parser.parse_args(argv)

    try:
        paths = changed_paths(args.base, args.head)
    except RuntimeError as error:
        print(f"classify-ci-changes: {error}; leaving `code` unset", file=sys.stderr)
        return 1

    reason = first_code_path(paths)
    if reason is None:
        print(f"All {len(paths)} changed file(s) are documentation; skipping build and test jobs.")
    elif reason in TEST_INPUT_DOCS:
        print(f"Full CI required: {reason} is read by CI ({len(paths)} file(s) changed).")
    else:
        print(f"Full CI required: {reason} is not documentation ({len(paths)} file(s) changed).")
    write_output(reason is not None)
    return 0


if __name__ == "__main__":
    sys.exit(main())
