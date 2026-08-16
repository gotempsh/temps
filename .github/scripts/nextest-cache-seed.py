#!/usr/bin/env python3
"""Find a trusted main-branch Cargo target seed for nextest archive builds."""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any


SEED_NAME = "temps-nextest-target-seed-v1"


def parse_timestamp(value: str) -> datetime:
    return datetime.fromisoformat(value.replace("Z", "+00:00"))


def eligible_artifacts(
    artifacts: list[dict[str, Any]], current_name: str, trusted_run_ids: set[int]
) -> list[dict[str, Any]]:
    """Return newest-first seeds produced by successful main pushes."""
    candidates = [
        artifact
        for artifact in artifacts
        if artifact.get("name") == current_name
        and not artifact.get("expired", False)
        and artifact.get("workflow_run", {}).get("head_branch") == "main"
        and isinstance(artifact.get("workflow_run", {}).get("id"), int)
        and artifact.get("workflow_run", {}).get("head_repository_id")
        == artifact.get("workflow_run", {}).get("repository_id")
        and artifact.get("workflow_run", {}).get("id") in trusted_run_ids
    ]
    return sorted(candidates, key=lambda item: item.get("created_at", ""), reverse=True)


def should_publish(
    artifacts: list[dict[str, Any]],
    current_name: str,
    trusted_run_ids: set[int],
    now: datetime,
) -> bool:
    candidates = eligible_artifacts(artifacts, current_name, trusted_run_ids)
    if not candidates:
        return True
    expires_at = candidates[0].get("expires_at")
    if not isinstance(expires_at, str):
        return True
    return parse_timestamp(expires_at) <= now + timedelta(days=2)


def github_json(url: str, token: str) -> dict[str, Any]:
    request = urllib.request.Request(
        url,
        headers={
            "Accept": "application/vnd.github+json",
            "Authorization": f"Bearer {token}",
            "X-GitHub-Api-Version": "2022-11-28",
            "User-Agent": "temps-nextest-cache-seed",
        },
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        payload = json.load(response)
    if not isinstance(payload, dict):
        raise ValueError("GitHub API response was not an object")
    return payload


def github_seed_data(
    repository: str, token: str, artifact_name: str
) -> tuple[list[dict[str, Any]], set[int]]:
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise ValueError("GITHUB_REPOSITORY must be an owner/repository pair")

    query = urllib.parse.urlencode({"per_page": 100, "name": artifact_name})
    artifacts_payload = github_json(
        f"https://api.github.com/repos/{repository}/actions/artifacts?{query}", token
    )
    artifacts = artifacts_payload.get("artifacts")
    if not isinstance(artifacts, list):
        raise ValueError("GitHub artifacts response did not contain an artifacts list")

    runs_query = urllib.parse.urlencode(
        {"branch": "main", "event": "push", "status": "success", "per_page": 100}
    )
    runs_payload = github_json(
        f"https://api.github.com/repos/{repository}/actions/workflows/"
        f"rust-tests.yml/runs?{runs_query}",
        token,
    )
    workflow_runs = runs_payload.get("workflow_runs")
    if not isinstance(workflow_runs, list):
        raise ValueError("GitHub workflow-runs response did not contain a list")
    trusted_run_ids = {
        run["id"]
        for run in workflow_runs
        if isinstance(run.get("id"), int)
        and run.get("event") == "push"
        and run.get("head_branch") == "main"
        and run.get("conclusion") == "success"
        and run.get("path") == ".github/workflows/rust-tests.yml"
        and run.get("repository", {}).get("full_name") == repository
        and run.get("head_repository", {}).get("full_name") == repository
    }
    return artifacts, trusted_run_ids


def append_outputs(output_path: Path, values: dict[str, str]) -> None:
    with output_path.open("a", encoding="utf-8") as output:
        for key, value in values.items():
            if "\n" in value or "\r" in value:
                raise ValueError(f"invalid newline in GitHub Actions output {key}")
            output.write(f"{key}={value}\n")


def find_seed(current_name: str) -> int:
    if current_name != SEED_NAME:
        print("invalid nextest cache seed artifact name", file=sys.stderr)
        return 2

    repository = os.environ.get("GITHUB_REPOSITORY", "")
    token = os.environ.get("GH_TOKEN", "")
    output_path = os.environ.get("GITHUB_OUTPUT", "")
    if not repository or not token or not output_path:
        print("GITHUB_REPOSITORY, GH_TOKEN, and GITHUB_OUTPUT are required", file=sys.stderr)
        return 2

    try:
        artifacts, trusted_run_ids = github_seed_data(repository, token, current_name)
        candidates = eligible_artifacts(artifacts, current_name, trusted_run_ids)
        selected = candidates[0] if candidates else None
        append_outputs(
            Path(output_path),
            {
                "artifact-name": selected["name"] if selected else "",
                "run-id": str(selected["workflow_run"]["id"]) if selected else "",
                "publish-current": str(
                    should_publish(
                        artifacts,
                        current_name,
                        trusted_run_ids,
                        datetime.now(timezone.utc),
                    )
                ).lower(),
            },
        )
    except (OSError, ValueError, KeyError, urllib.error.URLError) as error:
        print(f"warning: could not resolve nextest cache seed: {error}", file=sys.stderr)
        append_outputs(
            Path(output_path),
            {"artifact-name": "", "run-id": "", "publish-current": "false"},
        )
        return 0

    if selected:
        print(
            f"Using {selected['name']} from trusted main run "
            f"{selected['workflow_run']['id']}"
        )
    else:
        print("No trusted main-branch nextest cache seed found; building cold")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("current_name")
    args = parser.parse_args()
    return find_seed(args.current_name)


if __name__ == "__main__":
    sys.exit(main())
