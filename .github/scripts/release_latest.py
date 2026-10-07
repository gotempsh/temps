# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Decide whether a stable release may be marked as the repository's Latest.

GitHub's "Latest" release is what `GET /releases/latest` returns, and that
endpoint is how the installer's default stable channel picks a version. If
every stable tag were published with `--latest`, a backport cut after a newer
line shipped (say `v0.0.9` after `v0.1.0`) would become Latest and silently
downgrade everyone who runs the installer.

So a stable tag is Latest only when no other published stable release has a
higher version. Prereleases, nightlies, `test-v*` tags and anything else that
is not a plain `vMAJOR.MINOR.PATCH` tag are ignored: they never compete for
Latest. Re-running the release for a tag that is already published compares
the tag against itself, which keeps it Latest.

Usage:
    gh release list --exclude-drafts --exclude-pre-releases --limit 1000 \
        --json tagName --jq '.[].tagName' | python3 release_latest.py v1.2.3

Prints `true` or `false` on stdout and the reasoning on stderr.
"""

import re
import sys

STABLE_TAG = re.compile(r"^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")


def stable_version(tag):
    """`(major, minor, patch)` for a stable release tag, else None."""
    match = STABLE_TAG.match(tag.strip())
    if not match:
        return None
    return tuple(int(part) for part in match.groups())


def decide(tag, existing_tags):
    """Return `(is_latest, highest_other_stable_tag_or_None)`.

    Raises ValueError when `tag` itself is not a stable release tag: only the
    stable channel has a Latest, and guessing for anything else would be the
    exact bug this guards against.
    """
    version = stable_version(tag)
    if version is None:
        raise ValueError(f"'{tag}' is not a stable release tag (expected vMAJOR.MINOR.PATCH)")
    others = [
        (stable_version(other), other.strip())
        for other in existing_tags
        if other.strip() and other.strip() != tag and stable_version(other) is not None
    ]
    if not others:
        return True, None
    highest_version, highest_tag = max(others)
    return version >= highest_version, highest_tag


def main(argv, stdin, stdout, stderr):
    if len(argv) != 2:
        print("usage: release_latest.py <release-tag>  (existing release tags on stdin)", file=stderr)
        return 2
    tag = argv[1]
    try:
        is_latest, highest = decide(tag, stdin.read().splitlines())
    except ValueError as error:
        print(f"::error::{error}", file=stderr)
        return 2
    if highest is None:
        print(f"{tag} is the first stable release; marking it Latest.", file=stderr)
    elif is_latest:
        print(f"{tag} is the highest stable release (previous highest: {highest}); marking it Latest.", file=stderr)
    else:
        print(f"{tag} is older than the published stable release {highest}; "
              "publishing it WITHOUT moving Latest, so installs keep resolving to the newer line.",
              file=stderr)
    print("true" if is_latest else "false", file=stdout)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv, sys.stdin, sys.stdout, sys.stderr))
