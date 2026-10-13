# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Pick the release an upgrade test starts from (or upgrades to).

`upgrade-tests.yml` seeds an installation with a published release and
upgrades it to the candidate. Which release it starts from decides which
upgrade path is exercised, so it is chosen by version, not publication date:
a backport published after a newer line must not be mistaken for "the
release operators are on".

Channels (tags are read from stdin, one per line; callers pass only
published, non-draft releases that ship the platform tarball):

  stable   highest `vMAJOR.MINOR.PATCH`
  beta     highest `-beta.N` / `-rc.N` prerelease (the tags that move the
           `:beta` image channel, see release-channel.sh)
  nightly  highest `-nightly.` build

Anything that is not a version tag -- the `test-v*` releases cut by the
release-workflow smoke test, for instance -- is ignored. `--fallback beta`
lets the stable default still find something to test before the first
stable release exists.

`--min-core vX.Y.Z` ignores every release whose MAJOR.MINOR.PATCH core is
below it, prereleases of that core included. A release older than the
tooling the caller drives is not a usable starting point, and without the
floor a leftover stable tag from an earlier line would hide the fallback.

Prints the chosen tag on stdout and the reasoning on stderr.
"""

import argparse
import re
import sys

VERSION_TAG = re.compile(
    r"^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
    r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$"
)
BETA_PRERELEASE = re.compile(r"^(beta|rc)\.[0-9]+$")
CHANNELS = ("stable", "beta", "nightly")


def parse(tag):
    """`(core, prerelease_or_None)` for a version tag, else None."""
    match = VERSION_TAG.match(tag.strip())
    if not match:
        return None
    core = tuple(int(part) for part in match.groups()[:3])
    return core, match.group(4)


def sort_key(parsed):
    """Semver precedence: a release outranks its prereleases; numeric
    identifiers compare numerically and rank below alphanumeric ones."""
    core, prerelease = parsed
    if prerelease is None:
        return core, True, []
    identifiers = [
        (0, int(part), "") if part.isdigit() else (1, 0, part)
        for part in prerelease.split(".")
    ]
    return core, False, identifiers


def in_channel(parsed, channel):
    prerelease = parsed[1]
    if channel == "stable":
        return prerelease is None
    if channel == "beta":
        return prerelease is not None and bool(BETA_PRERELEASE.match(prerelease))
    if channel == "nightly":
        return prerelease is not None and prerelease.startswith("nightly.")
    raise ValueError(f"unknown channel '{channel}'")


def highest(tags, channel, min_core=None):
    candidates = []
    for tag in tags:
        parsed = parse(tag)
        if parsed is None or not in_channel(parsed, channel):
            continue
        if min_core is not None and parsed[0] < min_core:
            continue
        candidates.append((sort_key(parsed), tag.strip()))
    return max(candidates)[1] if candidates else None


def parse_min_core(value):
    """`(major, minor, patch)` from `vX.Y.Z`; argparse type for `--min-core`."""
    parsed = parse(value)
    if parsed is None or parsed[1] is not None:
        raise argparse.ArgumentTypeError(
            f"--min-core must be a release version like v0.1.0, got '{value}'")
    return parsed[0]


def select(tags, channel, fallback=None, min_core=None):
    """Return `(tag_or_None, channel_used)`."""
    tags = [tag for tag in tags if tag.strip()]
    chosen = highest(tags, channel, min_core)
    if chosen is None and fallback and fallback != channel:
        return highest(tags, fallback, min_core), fallback
    return chosen, channel


def main(argv, stdin, stdout, stderr):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--channel", required=True, choices=CHANNELS)
    parser.add_argument("--fallback", choices=CHANNELS)
    parser.add_argument("--min-core", type=parse_min_core, metavar="vX.Y.Z")
    args = parser.parse_args(argv[1:])
    tag, used = select(stdin.read().splitlines(), args.channel, args.fallback, args.min_core)
    floor = ("" if args.min_core is None
             else " at or above v" + ".".join(str(part) for part in args.min_core))
    if tag is None:
        wanted = args.channel + (f" or {args.fallback}" if args.fallback else "")
        print(f"::error::No published {wanted} release{floor} with the required asset was found.",
              file=stderr)
        return 1
    if used != args.channel:
        print(f"::notice::No {args.channel} release{floor} yet; using the highest {used} "
              f"release {tag}.", file=stderr)
    else:
        print(f"Highest {used} release: {tag}", file=stderr)
    print(tag, file=stdout)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv, sys.stdin, sys.stdout, sys.stderr))
