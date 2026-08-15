#!/usr/bin/env python3
"""Find a trusted main-branch Cargo cache seed for the musl binary build."""

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


SEED_NAME = "temps-musl-fast-seed-v1"


def parse_timestamp(value: str) -> datetime:
    return datetime.fromisoformat(value.replace("Z", "+00:00"))


def eligible_artifacts(
    artifacts: list[dict[str, Any]], current_name: str
) -> list[dict[str, Any]]:
    """Return newest-first cache seeds produced by trusted main runs."""
    candidates = [
        artifact
        for artifact in artifacts
        if artifact.get("name") == current_name
        and not artifact.get("expired", False)
        and artifact.get("workflow_run", {}).get("head_branch") == "main"
        and isinstance(artifact.get("workflow_run", {}).get("id"), int)
    ]
    return sorted(
        candidates,
        key=lambda artifact: artifact.get("created_at", ""),
        reverse=True,
    )


def should_publish(
    artifacts: list[dict[str, Any]], current_name: str, now: datetime
) -> bool:
    exact = [
        artifact
        for artifact in eligible_artifacts(artifacts, current_name)
        if artifact["name"] == current_name
    ]
    if not exact:
        return True
    expires_at = exact[0].get("expires_at")
    if not isinstance(expires_at, str):
        return True
    return parse_timestamp(expires_at) <= now + timedelta(days=2)


def github_artifacts(
    repository: str, token: str, artifact_name: str
) -> list[dict[str, Any]]:
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise ValueError("GITHUB_REPOSITORY must be an owner/repository pair")

    base_url = f"https://api.github.com/repos/{repository}/actions/artifacts"
    query = urllib.parse.urlencode({"per_page": 100, "name": artifact_name})
    request = urllib.request.Request(
        f"{base_url}?{query}",
        headers={
            "Accept": "application/vnd.github+json",
            "Authorization": f"Bearer {token}",
            "X-GitHub-Api-Version": "2022-11-28",
            "User-Agent": "temps-musl-cache-seed",
        },
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        payload = json.load(response)
    artifacts = payload.get("artifacts")
    if not isinstance(artifacts, list):
        raise ValueError("GitHub artifacts response did not contain an artifacts list")
    return artifacts


def append_outputs(output_path: Path, values: dict[str, str]) -> None:
    with output_path.open("a", encoding="utf-8") as output:
        for key, value in values.items():
            if "\n" in value or "\r" in value:
                raise ValueError(f"invalid newline in GitHub Actions output {key}")
            output.write(f"{key}={value}\n")


def find_seed(current_name: str) -> int:
    if current_name != SEED_NAME:
        print("invalid musl cache seed artifact name", file=sys.stderr)
        return 2

    repository = os.environ.get("GITHUB_REPOSITORY", "")
    token = os.environ.get("GH_TOKEN", "")
    output_path = os.environ.get("GITHUB_OUTPUT", "")
    if not repository or not token or not output_path:
        print("GITHUB_REPOSITORY, GH_TOKEN, and GITHUB_OUTPUT are required", file=sys.stderr)
        return 2

    try:
        artifacts = github_artifacts(repository, token, current_name)
        candidates = eligible_artifacts(artifacts, current_name)
        selected = candidates[0] if candidates else None
        values = {
            "artifact-name": selected["name"] if selected else "",
            "run-id": str(selected["workflow_run"]["id"]) if selected else "",
            "publish-current": str(
                should_publish(artifacts, current_name, datetime.now(timezone.utc))
            ).lower(),
        }
        append_outputs(Path(output_path), values)
    except (OSError, ValueError, KeyError, urllib.error.URLError) as error:
        # Losing the secondary seed must not prevent a correct cold build.
        print(f"warning: could not resolve musl cache seed: {error}", file=sys.stderr)
        append_outputs(
            Path(output_path),
            # Avoid uploading another multi-GB artifact from every main run
            # while the list API is unavailable and freshness is unknown.
            {"artifact-name": "", "run-id": "", "publish-current": "false"},
        )
        return 0

    if selected:
        print(
            f"Using {selected['name']} from trusted main run "
            f"{selected['workflow_run']['id']}"
        )
    else:
        print("No trusted main-branch musl cache seed found; building cold")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("current_name")
    args = parser.parse_args()
    return find_seed(args.current_name)


if __name__ == "__main__":
    sys.exit(main())
