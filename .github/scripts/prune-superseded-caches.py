#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Delete GitHub Actions cache entries that a newer entry has replaced.

Why this exists: the Rust caches are keyed on a hash of Cargo.lock (and the
toolchain), so every dependency bump on `main` saves a new entry and leaves
the previous one behind. Restores look for the current key first and then the
newest entry sharing its prefix, so only the newest entry in each family is
ever read again. The superseded ones sat in the pool until LRU eviction:
five 2GB `nextest-archive` copies and four 1GB `test-build` copies at once,
next to a musl build cache big enough that the pool's next write evicts
whatever is oldest.

Also deleted: entries saved from tag refs. Release and nightly runs are
tag-scoped, nothing else can read their caches, and the workflows are meant
not to write any (see check-cache-save-guards.py); anything that slipped
through is dead weight.
"""

import argparse
import json
import os
import re
import subprocess
import sys
from collections import defaultdict

# Only families whose restore path is "exact key, else newest with this
# prefix". Content-addressed entries (buildkit blobs, the toolchain index)
# are managed by BuildKit and must not be touched.
FAMILIES = [
    # Swatinem/rust-cache: v0-rust-<shared-key>-<os>-<arch>-<env hash>-<lock hash>
    re.compile(r"^(v0-rust-.+)-[0-9a-f]{8}-[0-9a-f]{8}$"),
    # rust-tests.yml build-binary: hashFiles('Cargo.lock', 'Dockerfile')
    re.compile(r"^(temps-musl-fast-)[0-9a-f]{64}$"),
    # web/node_modules: hashFiles('web/bun.lock')
    re.compile(r"^(Linux-bun-)[0-9a-f]{64}$"),
]
TAG_REF = re.compile(r"^refs/(?:heads/refs/)?tags/")


def gh(*args: str) -> str:
    return subprocess.run(
        ["gh", *args], check=True, capture_output=True, text=True
    ).stdout


def list_caches(repo: str) -> list[dict]:
    out = gh(
        "api", "--paginate",
        f"repos/{repo}/actions/caches?per_page=100",
        "--jq", ".actions_caches[]",
    )
    return [json.loads(line) for line in out.splitlines() if line.strip()]


def family(key: str) -> str | None:
    for pattern in FAMILIES:
        match = pattern.match(key)
        if match:
            return match.group(1)
    return None


def gib(n: int) -> str:
    return f"{n / (1 << 30):.2f} GiB"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY"))
    parser.add_argument("--dry-run", action="store_true",
                        help="list what would be deleted without deleting it")
    args = parser.parse_args()
    if not args.repo:
        parser.error("--repo is required outside GitHub Actions")

    caches = list_caches(args.repo)
    doomed: list[tuple[dict, str]] = []
    groups: dict[tuple[str, str], list[dict]] = defaultdict(list)
    for cache in caches:
        if TAG_REF.match(cache["ref"]):
            doomed.append((cache, "saved from a tag ref"))
            continue
        prefix = family(cache["key"])
        if prefix:
            groups[(cache["ref"], prefix)].append(cache)

    for (_ref, _prefix), entries in groups.items():
        entries.sort(key=lambda c: c["created_at"], reverse=True)
        newest = entries[0]
        for cache in entries[1:]:
            doomed.append((cache, f"superseded by {newest['key']}"))

    total = sum(c["size_in_bytes"] for c in caches)
    freed = sum(c["size_in_bytes"] for c, _ in doomed)
    verb = "Would delete" if args.dry_run else "Deleting"
    for cache, reason in sorted(doomed, key=lambda d: -d[0]["size_in_bytes"]):
        print(
            f"{verb} {gib(cache['size_in_bytes']):>10}  {cache['ref']}  "
            f"{cache['key']}  ({reason})"
        )
        if not args.dry_run:
            try:
                gh("api", "--method", "DELETE",
                   f"repos/{args.repo}/actions/caches/{cache['id']}")
            except subprocess.CalledProcessError as error:
                # Another run may have deleted or evicted it first.
                print(f"::warning::Could not delete cache {cache['id']} "
                      f"({cache['key']}): {error.stderr.strip()}")

    print(
        f"{len(caches)} entries, {gib(total)} in use; "
        f"{'would free' if args.dry_run else 'freed'} {gib(freed)} "
        f"across {len(doomed)} entries."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
