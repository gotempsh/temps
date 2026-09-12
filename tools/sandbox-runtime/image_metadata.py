# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Validate daemon image publication and emit GitHub Actions build metadata."""

import os
from pathlib import Path
import re


ROOT = Path(__file__).resolve().parents[2]


def image_version(source):
    # Read the actual images selected by Temps, not an independently versioned tag.
    function = source.split("pub fn managed_application_workspace_image", 1)
    if len(function) != 2:
        raise ValueError("Cannot find managed daemon image selection")
    images = re.findall(r'ghcr\.io/gotempsh/temps-sandbox-(nodejs|python|all):(\d+\.\d+\.\d+)', function[1].split("\n}", 1)[0])
    if {flavor for flavor, _ in images} != {"nodejs", "python", "all"} or len({version for _, version in images}) != 1:
        raise ValueError("Managed daemon image flavors must have one shared pinned version")
    return images[0][1]


def metadata(environment, version):
    flavor = environment["FLAVOR"]
    channel = environment["CHANNEL"]
    if flavor not in ("nodejs", "python", "all") or channel not in ("stable", "beta"):
        raise ValueError("Invalid daemon image flavor or publication channel")
    dry_run = environment["DRY_RUN"]
    if dry_run not in ("true", "false"):
        raise ValueError("DRY_RUN must be explicitly true or false")
    event = environment["GITHUB_EVENT_NAME"]
    ref = environment["GITHUB_REF"]
    publish = dry_run == "false" and event != "pull_request"
    if publish:
        if environment["GITHUB_REPOSITORY"] != "gotempsh/temps":
            raise ValueError("Only gotempsh/temps can publish managed runtime images")
        if channel == "stable" and not re.fullmatch(r"refs/tags/v\d+\.\d+\.\d+", ref):
            raise ValueError("Stable daemon images require a stable release tag")
        if channel == "beta" and ref != "refs/heads/main" and not re.fullmatch(r"refs/tags/v\d+\.\d+\.\d+-[A-Za-z0-9.-]+", ref):
            raise ValueError("Beta daemon images require main or a prerelease tag")
    sha = environment["GITHUB_SHA"]
    if not re.fullmatch(r"[0-9a-f]{40}", sha):
        raise ValueError("Image revision must be a full commit SHA")
    repository = f"ghcr.io/gotempsh/temps-sandbox-{flavor}"
    # Never overwrite legacy python:latest, python:beta, or python:<sha>.
    tags = [f"{repository}:daemon-{sha}"]
    tags.append(f"{repository}:{version}" if channel == "stable" else f"{repository}:{version}-beta")
    return {"publish": str(publish).lower(), "version": version, "tags": ",".join(tags)}


if __name__ == "__main__":
    source = (ROOT / "crates/temps-sandbox/src/services/sandbox_service.rs").read_text()
    for key, value in metadata(os.environ, image_version(source)).items():
        print(f"{key}={value}")
