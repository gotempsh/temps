#!/usr/bin/env python3
"""Fail when PR or release workflows can write unusable cache entries.

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

The fix for PR workflows is to save only from main. Publishing release runs
are tag-scoped, so the release workflow stays entirely read-only; manual dry
runs follow the same policy to keep its behavior predictable.
"""

import re
import sys
from pathlib import Path

WORKFLOWS_DIR = Path(__file__).resolve().parent.parent / "workflows"
RELEASE_WORKFLOW = WORKFLOWS_DIR / "release.yml"


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


def step_block(lines: list[str], uses_line: int) -> str:
    """Return one workflow step, starting at a zero-based `uses:` line."""
    step_indent = None
    for k in range(uses_line, -1, -1):
        match = re.match(r"^(\s*)-\s", lines[k])
        if match:
            step_indent = len(match.group(1))
            break
    if step_indent is None:
        step_indent = len(lines[uses_line]) - len(lines[uses_line].lstrip(" "))

    block_end = len(lines)
    for j in range(uses_line + 1, len(lines)):
        if not lines[j].strip():
            continue
        indent = len(lines[j]) - len(lines[j].lstrip(" "))
        if indent <= step_indent:
            block_end = j
            break
    return "\n".join(lines[uses_line:block_end])


def find_release_cache_writes(text: str) -> list[int]:
    """Return lines that can write Actions caches from the release workflow."""
    lines = text.splitlines()
    violations = []

    for i, line in enumerate(lines):
        # The combined action restores during the step and saves at job exit.
        # Tag-only workflows must use actions/cache/restore instead.
        if re.search(r"^\s*(?:-\s+)?uses:\s*actions/cache(?:/save)?@", line):
            violations.append(i + 1)

        # A BuildKit GHA export is also a tag-scoped Actions cache write.
        cache_to = re.match(r"^(\s*)cache-to:\s*(.*)$", line)
        if cache_to:
            value = cache_to.group(2)
            block_indent = len(cache_to.group(1))
            for following in lines[i + 1 :]:
                if not following.strip():
                    continue
                indent = len(following) - len(following.lstrip(" "))
                if indent <= block_indent:
                    break
                value += "\n" + following
            if "type=gha" in value:
                violations.append(i + 1)

        if re.search(r"^\s*(?:-\s+)?uses:\s*Swatinem/rust-cache", line):
            block = step_block(lines, i)
            if not re.search(r"^\s*save-if:\s*(?:false|\$\{\{\s*false\s*\}\})\s*$", block, re.MULTILINE):
                violations.append(i + 1)

        # These Docker setup actions write small Actions cache entries unless
        # their implicit caches are explicitly disabled.
        implicit_cache_actions = {
            "docker/setup-buildx-action": "cache-binary",
            "docker/setup-qemu-action": "cache-image",
        }
        for action, opt_out in implicit_cache_actions.items():
            if not re.search(rf"^\s*(?:-\s+)?uses:\s*{re.escape(action)}@", line):
                continue
            block = step_block(lines, i)
            if not re.search(rf"^\s*{re.escape(opt_out)}:\s*false\s*$", block, re.MULTILINE):
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

    release_text = RELEASE_WORKFLOW.read_text()
    release_failures = find_release_cache_writes(release_text)
    if release_failures:
        print(
            "Cache write(s) in release workflow -- use "
            "`actions/cache/restore`, set `Swatinem/rust-cache` to "
            "`save-if: false`, disable Docker setup caches, and remove "
            "BuildKit `type=gha` exports:",
            file=sys.stderr,
        )
        for line_no in release_failures:
            print(f"  .github/workflows/release.yml:{line_no}", file=sys.stderr)
        return 1

    print("OK: PR caches are guarded and the release workflow cache is read-only.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
