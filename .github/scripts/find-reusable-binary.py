#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Find a `temps` binary built by `main` that a pull request can run instead of compiling.

Why this exists: `build-binary` in rust-tests.yml compiles the musl `temps`
binary, and on a pull request that takes 45-65 minutes, longer than every job
that consumes it. A pull request that changes only the console, the TypeScript
CLI or the skills (classify-ci-changes.py reports `rust=false`) cannot change
that binary's Rust code, so a binary `main` already built from the same Rust
source is exactly as good as a fresh one -- except for the console bundle it
embeds, which the E2E job then serves from this checkout instead.

The search walks the first-parent history of the pull request's base, newest
first, and stops at the first commit that has a usable binary. That commit is
accepted only if every file that differs between it and the pull request's
merge commit is outside the Rust inputs, so a binary from a few commits back
is used only when those commits did not touch Rust either.

Trust. A binary is only taken from a `push` run of rust-tests.yml on `main` of
this repository -- the same provenance rule nextest-cache-seed.py applies to
its target seed. A fork, a pull request run, a dispatch from another branch or
another workflow can never be selected, because the artifact a pull request
downloads here ships in the image it tests.

Fail closed. Any error, or no candidate, writes empty outputs and exits 0, and
`build-binary` compiles as before. A wrong match would test the wrong binary; a
missed one only costs the build this exists to skip.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Any

SCRIPTS = Path(__file__).resolve().parent
WORKFLOW_PATH = ".github/workflows/rust-tests.yml"
WORKFLOW_FILE = "rust-tests.yml"

_spec = importlib.util.spec_from_file_location(
    "classify_ci_changes", SCRIPTS / "classify-ci-changes.py"
)
assert _spec is not None and _spec.loader is not None
classifier = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(classifier)

FetchJson = Callable[[str], dict[str, Any]]
ChangedPaths = Callable[[str, str], list[str]]


@dataclass(frozen=True)
class Selection:
    sha: str
    run_id: int
    artifact_name: str


def git_lines(*args: str) -> list[str]:
    result = subprocess.run(["git", *args], check=False, capture_output=True, text=True)
    if result.returncode != 0:
        raise RuntimeError(
            f"git {' '.join(args)} failed with exit code {result.returncode}: "
            f"{result.stderr.strip()}"
        )
    return [line for line in result.stdout.splitlines() if line]


def candidate_commits(base: str, limit: int) -> list[str]:
    """Return `base` and its first-parent ancestors, newest first."""
    return git_lines("rev-list", "--first-parent", f"--max-count={limit}", base)


def trusted_runs(runs: list[dict[str, Any]], repository: str, sha: str) -> list[dict[str, Any]]:
    """Return the runs that may supply a binary for `sha`, newest first."""
    trusted = [
        run
        for run in runs
        if isinstance(run.get("id"), int)
        and run.get("event") == "push"
        and run.get("head_branch") == "main"
        and run.get("head_sha") == sha
        and run.get("path") == WORKFLOW_PATH
        and run.get("repository", {}).get("full_name") == repository
        and run.get("head_repository", {}).get("full_name") == repository
    ]
    return sorted(trusted, key=lambda run: run.get("created_at", ""), reverse=True)


def usable_artifact(
    artifacts: list[dict[str, Any]], name: str, run_id: int, sha: str
) -> dict[str, Any] | None:
    for artifact in artifacts:
        run = artifact.get("workflow_run", {})
        if (
            artifact.get("name") == name
            and not artifact.get("expired", True)
            and run.get("id") == run_id
            and run.get("head_sha") == sha
            and run.get("head_branch") == "main"
            and run.get("repository_id") is not None
            and run.get("head_repository_id") == run.get("repository_id")
        ):
            return artifact
    return None


def find_binary(
    *,
    repository: str,
    head: str,
    candidates: list[str],
    artifact_name: str,
    fetch_json: FetchJson,
    changed_paths: ChangedPaths,
    log: Callable[[str], None] = print,
) -> Selection | None:
    api = f"https://api.github.com/repos/{repository}/actions"
    for sha in candidates:
        query = urllib.parse.urlencode(
            {"event": "push", "branch": "main", "head_sha": sha, "per_page": 20}
        )
        payload = fetch_json(f"{api}/workflows/{WORKFLOW_FILE}/runs?{query}")
        runs = payload.get("workflow_runs")
        if not isinstance(runs, list):
            raise ValueError(f"workflow-runs response for {sha} did not contain a list")

        for run in trusted_runs(runs, repository, sha):
            query = urllib.parse.urlencode({"name": artifact_name, "per_page": 10})
            listing = fetch_json(f"{api}/runs/{run['id']}/artifacts?{query}")
            artifacts = listing.get("artifacts")
            if not isinstance(artifacts, list):
                raise ValueError(f"artifacts response for run {run['id']} did not contain a list")
            if usable_artifact(artifacts, artifact_name, run["id"], sha) is None:
                continue

            # The newest commit with a binary decides: an older one differs
            # from the merge commit by at least as much, revert aside.
            reason = classifier.first_rust_path(changed_paths(sha, head))
            if reason is not None:
                log(
                    f"main binary for {sha[:12]} (run {run['id']}) is not reusable: "
                    f"{reason} differs from this pull request's merge commit"
                )
                return None
            return Selection(sha=sha, run_id=run["id"], artifact_name=artifact_name)

        log(f"no unexpired {artifact_name} for main commit {sha[:12]}")
    return None


def github_json(url: str, token: str) -> dict[str, Any]:
    request = urllib.request.Request(
        url,
        headers={
            "Accept": "application/vnd.github+json",
            "Authorization": f"Bearer {token}",
            "X-GitHub-Api-Version": "2022-11-28",
            "User-Agent": "temps-find-reusable-binary",
        },
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        payload = json.load(response)
    if not isinstance(payload, dict):
        raise ValueError(f"GitHub API response for {url} was not an object")
    return payload


def write_outputs(output_path: str, selection: Selection | None) -> None:
    values = {
        "sha": selection.sha if selection else "",
        "run-id": str(selection.run_id) if selection else "",
        "artifact-name": selection.artifact_name if selection else "",
    }
    with Path(output_path).open("a", encoding="utf-8") as output:
        for key, value in values.items():
            output.write(f"{key}={value}\n")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--base", required=True, help="the pull request's base commit")
    parser.add_argument("--head", required=True, help="the pull request's merge commit")
    parser.add_argument("--artifact", required=True, help="binary artifact name")
    parser.add_argument(
        "--max-commits",
        type=int,
        default=10,
        help="how many main commits, starting at --base, to look for a binary on",
    )
    args = parser.parse_args(argv)

    repository = os.environ.get("GITHUB_REPOSITORY", "")
    token = os.environ.get("GH_TOKEN", "")
    output_path = os.environ.get("GITHUB_OUTPUT", "")
    if not repository or not token or not output_path:
        print("GITHUB_REPOSITORY, GH_TOKEN and GITHUB_OUTPUT are required", file=sys.stderr)
        return 2
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        print(f"GITHUB_REPOSITORY {repository!r} is not an owner/repository pair", file=sys.stderr)
        return 2

    try:
        head = git_lines("rev-parse", "--verify", f"{args.head}^{{commit}}")[0]
        selection = find_binary(
            repository=repository,
            head=head,
            candidates=candidate_commits(args.base, args.max_commits),
            artifact_name=args.artifact,
            fetch_json=lambda url: github_json(url, token),
            changed_paths=classifier.changed_paths,
        )
    except (OSError, ValueError, KeyError, RuntimeError, urllib.error.URLError) as error:
        print(
            f"warning: could not look up a reusable {args.artifact}: {error}; building it instead",
            file=sys.stderr,
        )
        selection = None

    write_outputs(output_path, selection)
    if selection:
        print(
            f"Reusing {selection.artifact_name} from main commit {selection.sha} "
            f"(run {selection.run_id})"
        )
    else:
        print(f"No reusable {args.artifact} found; building it from this checkout")
    return 0


if __name__ == "__main__":
    sys.exit(main())
