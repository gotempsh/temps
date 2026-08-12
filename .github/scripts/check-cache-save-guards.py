#!/usr/bin/env python3
"""Fail if a pull_request-triggered workflow caches with Swatinem/rust-cache
without a `save-if` guard.

Why this exists: an unguarded Swatinem/rust-cache step in a workflow that
runs on pull_request writes one copy of the cache per open PR ref. The
repo's GitHub Actions cache pool is a single 10GB LRU space shared across
ALL refs/branches/PRs, so a handful of duplicated ~300MB+ caches is enough
to evict the much larger shared `temps-musl-fast-*` build cache, turning
every musl binary build into a ~80min cold compile instead of an
incremental one. This has happened twice (dependency-scan.yml's
cargo-audit job, network-kernel-tests.yml's unit job) -- this check exists
so a third occurrence fails CI instead of silently degrading every
pipeline for days before someone notices.

The fix is always the same: add `save-if: ${{ github.ref ==
'refs/heads/main' }}` (or `save-if: false` for jobs that intentionally
only ever read a cache another job writes, e.g. starters.yml's matrix
jobs) to the step's `with:` block.
"""

import re
import sys
from pathlib import Path

WORKFLOWS_DIR = Path(__file__).resolve().parent.parent / "workflows"


def workflow_triggers_on_pull_request(text: str) -> bool:
    on_block_match = re.search(r"^on:\s*$", text, re.MULTILINE)
    if not on_block_match:
        # `on: push` one-liner etc. -- not our pattern, but check anyway.
        return "pull_request" in text.split("\njobs:")[0]
    on_block_start = on_block_match.end()
    jobs_match = re.search(r"^jobs:", text[on_block_start:], re.MULTILINE)
    on_block_end = on_block_start + (jobs_match.start() if jobs_match else len(text))
    return "pull_request" in text[on_block_start:on_block_end]


def find_unguarded_cache_steps(text: str) -> list[int]:
    lines = text.splitlines()
    violations = []
    for i, line in enumerate(lines):
        # Anchor on an actual `uses:` step, not just the substring, so a
        # comment mentioning "Swatinem/rust-cache" (like this script's own
        # `run:` step in rust-tests.yml) can't self-trigger a false positive.
        if not re.search(r"^\s*(?:-\s+)?uses:\s*Swatinem/rust-cache", line):
            continue
        # Look at the step's `with:` block: from this line until the next
        # sibling step (a `- ` line at or above the *step's* indentation,
        # not this line's own indentation -- `uses:` and its nested `with:`
        # keys share deeper indentation than the `- name:`/`- uses:` line
        # that starts the step).
        step_indent = None
        for k in range(i, -1, -1):
            m = re.match(r"^(\s*)-\s", lines[k])
            if m:
                step_indent = len(m.group(1))
                break
        if step_indent is None:
            step_indent = len(line) - len(line.lstrip(" "))
        block_end = len(lines)
        for j in range(i + 1, len(lines)):
            stripped = lines[j].strip()
            if not stripped:
                continue
            indent = len(lines[j]) - len(lines[j].lstrip(" "))
            if indent <= step_indent:
                block_end = j
                break
        block = "\n".join(lines[i:block_end])
        if "save-if:" not in block:
            violations.append(i + 1)
    return violations


def main() -> int:
    failures = []
    for wf in sorted(WORKFLOWS_DIR.glob("*.yml")):
        text = wf.read_text()
        if not workflow_triggers_on_pull_request(text):
            continue
        for line_no in find_unguarded_cache_steps(text):
            failures.append(f"{wf.relative_to(WORKFLOWS_DIR.parent.parent)}:{line_no}")

    if failures:
        print(
            "Unguarded Swatinem/rust-cache step(s) in pull_request-triggered "
            "workflow(s) -- add `save-if: ${{ github.ref == 'refs/heads/main' }}` "
            "(or `save-if: false` if this job intentionally only reads):",
            file=sys.stderr,
        )
        for f in failures:
            print(f"  {f}", file=sys.stderr)
        return 1

    print("OK: all pull_request-triggered Swatinem/rust-cache steps are save-if guarded.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
