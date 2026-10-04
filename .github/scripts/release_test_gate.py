# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Refuse to publish a release whose commit has not passed the test workflow.

`release.yml` builds and publishes whatever commit a `v*` tag points at. It
runs no tests of its own; re-running the full suite there would double the
release wall-clock and write caches nobody can read (see the note at the top
of release.yml). Instead the release asks GitHub whether `rust-tests.yml`
already succeeded for the exact commit being released.

Only `push` and `workflow_dispatch` runs count. A `pull_request` run reports
the PR head SHA but tests the merge commit, and a docs-only PR skips every
job while still concluding `success`, so it proves nothing about this commit.
Pushes to `main` and manual dispatches always run every job.

States (see `classify`):
  success  at least one qualifying run completed successfully
  pending  no success yet, but a qualifying run is queued or in progress
  failed   every qualifying run completed without success
  missing  no qualifying run exists for this commit
"""

import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

QUALIFYING_EVENTS = frozenset({"push", "workflow_dispatch"})

# A run that has not reached `completed` may still succeed.
UNFINISHED_STATUSES = frozenset(
    {"queued", "in_progress", "waiting", "requested", "pending"}
)


def classify(runs):
    """Reduce the workflow runs for one commit to a single gate state."""
    qualifying = [run for run in runs if run.get("event") in QUALIFYING_EVENTS]
    if any(
        run.get("status") == "completed" and run.get("conclusion") == "success"
        for run in qualifying
    ):
        return "success"
    if any(run.get("status") in UNFINISHED_STATUSES for run in qualifying):
        return "pending"
    if qualifying:
        return "failed"
    return "missing"


def describe(runs):
    """One line per qualifying run, for the failure message and job summary."""
    lines = []
    for run in runs:
        if run.get("event") not in QUALIFYING_EVENTS:
            continue
        lines.append(
            "- {event} on {branch}: {status}/{conclusion} {url}".format(
                event=run.get("event"),
                branch=run.get("head_branch"),
                status=run.get("status"),
                conclusion=run.get("conclusion") or "-",
                url=run.get("html_url", ""),
            )
        )
    return "\n".join(lines) or "- (no push or workflow_dispatch runs for this commit)"


def fetch_runs(repository, workflow, sha, token, api_url="https://api.github.com"):
    query = urllib.parse.urlencode({"head_sha": sha, "per_page": 100})
    url = f"{api_url}/repos/{repository}/actions/workflows/{workflow}/runs?{query}"
    request = urllib.request.Request(
        url,
        headers={
            "Accept": "application/vnd.github+json",
            "Authorization": f"Bearer {token}",
            "X-GitHub-Api-Version": "2022-11-28",
            "User-Agent": "temps-release-test-gate",
        },
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.load(response).get("workflow_runs", [])


def guidance(state, workflow, sha, ref_name):
    rerun = (
        f"gh workflow run {workflow} --ref {ref_name}"
        if ref_name
        else f"gh workflow run {workflow} --ref <tag>"
    )
    reasons = {
        "failed": f"{workflow} did not succeed for {sha}.",
        "missing": f"{workflow} has no push or workflow_dispatch run for {sha}.",
        "pending": f"{workflow} for {sha} did not finish before the gate timed out.",
    }
    return (
        f"{reasons[state]} The release will not be published.\n"
        f"To test this exact commit, run `{rerun}` and re-run this release once it is green.\n"
        "Emergency only: a maintainer can dispatch release.yml at the tag with "
        "`-f dry_run=false -f skip_test_gate=true`; the bypass is recorded in the run summary."
    )


def write_summary(text):
    path = os.environ.get("GITHUB_STEP_SUMMARY")
    if path:
        with open(path, "a", encoding="utf-8") as summary:
            summary.write(text + "\n")


def wait(args, fetch=fetch_runs, sleep=time.sleep, clock=time.monotonic):
    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    if not token:
        print("::error::GH_TOKEN is required to query workflow runs", file=sys.stderr)
        return 2

    started = clock()
    deadline = started + args.timeout_minutes * 60
    missing_deadline = started + args.missing_grace_minutes * 60
    runs = []
    state = "missing"
    while True:
        try:
            runs = fetch(args.repository, args.workflow, args.sha, token)
            state = classify(runs)
        except (urllib.error.URLError, TimeoutError, ValueError) as error:
            # Transient API trouble must not publish, but should not fail the
            # gate on the first hiccup either: keep polling until the deadline.
            print(f"::warning::Could not query {args.workflow} runs: {error}")
            state = "pending"

        print(f"{args.workflow} state for {args.sha}: {state}")
        if state == "success":
            write_summary(
                f"Release test gate: `{args.workflow}` passed for `{args.sha}`.\n\n{describe(runs)}"
            )
            return 0
        if state == "failed":
            break
        now = clock()
        # Right after `git push origin main vX.Y.Z` the push run may not be
        # registered yet, so a missing run is retried for a short grace period.
        if state == "missing" and now >= missing_deadline:
            break
        if now >= deadline:
            break
        sleep(args.poll_seconds)

    message = guidance(state, args.workflow, args.sha, args.ref_name)
    print(f"::error::{message.splitlines()[0]}")
    print(message)
    print(describe(runs))
    write_summary(f"Release test gate FAILED ({state}).\n\n{message}\n\n{describe(runs)}")
    return 1


def print_state(args, fetch=fetch_runs):
    """Single non-blocking query. API trouble reports `unknown`, never `failed`,
    so a GitHub hiccup cannot suppress a nightly that would have passed."""
    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    if not token:
        print("::error::GH_TOKEN is required to query workflow runs", file=sys.stderr)
        return 2
    try:
        state = classify(fetch(args.repository, args.workflow, args.sha, token))
    except (urllib.error.URLError, TimeoutError, ValueError) as error:
        print(f"::warning::Could not query {args.workflow} runs: {error}", file=sys.stderr)
        state = "unknown"
    print(f"tests_state={state}")
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repository", required=True)
    parser.add_argument("--sha", required=True)
    parser.add_argument("--workflow", default="rust-tests.yml")
    parser.add_argument("--ref-name", default="")
    parser.add_argument("--timeout-minutes", type=float, default=180)
    parser.add_argument("--missing-grace-minutes", type=float, default=10)
    parser.add_argument("--poll-seconds", type=float, default=60)
    parser.add_argument(
        "--print-state",
        action="store_true",
        help="query once, print `tests_state=<state>` and exit 0 (used by nightly-release.yml)",
    )
    args = parser.parse_args(argv)
    if args.print_state:
        return print_state(args)
    return wait(args)


if __name__ == "__main__":
    sys.exit(main())
