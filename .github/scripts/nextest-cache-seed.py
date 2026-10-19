#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Find a trusted main-branch Cargo target seed for nextest archive builds."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any


# Bump the suffix to retire every previously published seed: they are matched by
# exact name, so a new name makes the next main run build cold and republish.
# v2: a v1 seed (main run 35353352026) made the docker-tests archive step fail
# with `E0463: can't find crate for mongodb` once the 2026-09-21 dependency
# bumps landed, while cold builds of the same tree passed.
#
# Each published seed is additionally suffixed with a fingerprint of Cargo.lock
# and the rustc version (see seed_artifact_name). A seed built against another
# lockfile or compiler shares almost no artifacts with the current tree: cargo
# then rebuilds the whole dependency graph next to ~19GB of unpacked stale
# output, and the runner runs out of disk (PR #1574 died in the linker with
# SIGBUS). Keying on the fingerprint makes such a build go cold instead, and
# makes main publish a matching seed on its first run after the change.
SEED_NAME = "temps-nextest-target-seed-v2"
FINGERPRINT_LENGTH = 16


def build_fingerprint(lockfile: bytes, rustc_version: str) -> str:
    """Identify the dependency graph and compiler a target directory was built with."""
    digest = hashlib.sha256()
    digest.update(lockfile)
    digest.update(b"\0")
    digest.update(rustc_version.encode("utf-8"))
    return digest.hexdigest()[:FINGERPRINT_LENGTH]


def seed_artifact_name(prefix: str, fingerprint: str) -> str:
    return f"{prefix}-{fingerprint}"


def current_fingerprint(lockfile_path: Path) -> str:
    rustc_version = subprocess.run(
        ["rustc", "-vV"], check=True, capture_output=True, text=True
    ).stdout
    return build_fingerprint(lockfile_path.read_bytes(), rustc_version)


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


def find_seed(seed_prefix: str, lockfile_path: Path = Path("Cargo.lock")) -> int:
    if seed_prefix != SEED_NAME:
        print("invalid nextest cache seed artifact name", file=sys.stderr)
        return 2

    repository = os.environ.get("GITHUB_REPOSITORY", "")
    token = os.environ.get("GH_TOKEN", "")
    output_path = os.environ.get("GITHUB_OUTPUT", "")
    if not repository or not token or not output_path:
        print("GITHUB_REPOSITORY, GH_TOKEN, and GITHUB_OUTPUT are required", file=sys.stderr)
        return 2

    try:
        current_name = seed_artifact_name(seed_prefix, current_fingerprint(lockfile_path))
        artifacts, trusted_run_ids = github_seed_data(repository, token, current_name)
        candidates = eligible_artifacts(artifacts, current_name, trusted_run_ids)
        selected = candidates[0] if candidates else None
        append_outputs(
            Path(output_path),
            {
                "seed-name": current_name,
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
    except (
        OSError,
        ValueError,
        KeyError,
        subprocess.CalledProcessError,
        urllib.error.URLError,
    ) as error:
        print(f"warning: could not resolve nextest cache seed: {error}", file=sys.stderr)
        append_outputs(
            Path(output_path),
            {"seed-name": "", "artifact-name": "", "run-id": "", "publish-current": "false"},
        )
        return 0

    if selected:
        print(
            f"Using {selected['name']} from trusted main run "
            f"{selected['workflow_run']['id']}"
        )
    else:
        print(f"No trusted main-branch {current_name} found; building cold")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("current_name")
    args = parser.parse_args()
    return find_seed(args.current_name)


if __name__ == "__main__":
    sys.exit(main())
