# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Bind all runtime dependencies to one release's verified multiarch digests."""

import argparse
import json
from pathlib import Path
import re
import subprocess

REPOSITORIES = {
    **{f"daemon_{flavor}": f"ghcr.io/gotempsh/temps-sandbox-{flavor}"
       for flavor in ("nodejs", "python", "all")},
    **{f"sandbox_{runtime}": f"ghcr.io/gotempsh/temps-sandbox-{runtime}"
       for runtime in ("node", "bun", "python", "rust", "go", "full")},
    "preview_gateway": "ghcr.io/gotempsh/temps-preview-gateway",
}


def record(kind, digest, revision):
    if kind not in REPOSITORIES or not re.fullmatch(r"sha256:[0-9a-f]{64}", digest):
        raise ValueError("Unknown runtime image or invalid build digest")
    if not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise ValueError("Release revision must be a full commit SHA")
    return {"revision": revision, "images": {kind: f"{REPOSITORIES[kind]}@{digest}"}}


def assemble(records, revision):
    images = {}
    for item in records:
        if item["revision"] != revision:
            raise ValueError("Runtime image belongs to a different release revision")
        for kind, reference in item["images"].items():
            if kind in images:
                raise ValueError(f"Duplicate runtime image: {kind}")
            repository, digest = reference.split("@", 1)
            validated = record(kind, digest, revision)
            if validated["images"][kind] != reference:
                raise ValueError(f"Unexpected repository: {repository}")
            images[kind] = reference
    if images.keys() != REPOSITORIES.keys():
        raise ValueError("Release must include all ten runtime images")
    return {"revision": revision, "images": dict(sorted(images.items()))}


def verify_platforms(index):
    platforms = {(entry.get("platform", {}).get("os"),
                  entry.get("platform", {}).get("architecture"))
                 for entry in index.get("manifests", [])}
    if not {("linux", "amd64"), ("linux", "arm64")} <= platforms:
        raise ValueError("Runtime digest must include linux/amd64 and linux/arm64")


def verify_registry(manifest):
    for kind, reference in manifest["images"].items():
        result = subprocess.run(
            ["docker", "buildx", "imagetools", "inspect", "--raw", reference],
            check=True, capture_output=True, text=True, timeout=120)
        verify_platforms(json.loads(result.stdout))
        print(f"Verified {kind}: {reference}")


def promotion_tags(kind, channel, daemon_version, sandbox_version, gateway_version):
    if channel not in ("stable", "beta"):
        raise ValueError("Unknown release channel")
    for version in (daemon_version, sandbox_version, gateway_version):
        if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
            raise ValueError("Invalid image version")
    if kind.startswith("daemon_"):
        suffixes = [daemon_version if channel == "stable" else f"{daemon_version}-beta"]
    else:
        version = gateway_version if kind == "preview_gateway" else sandbox_version
        suffixes = ([version, f"{version}-stable", "latest", "stable"]
                    if channel == "stable" else [f"{version}-beta", "beta"])
    return [f"{REPOSITORIES[kind]}:{suffix}" for suffix in suffixes]


def promote_images(manifest, channel, daemon_version, sandbox_version, gateway_version):
    # Validate the complete plan before the first registry write. Python's
    # legacy and daemon images share a repository but must never share an alias.
    plan = []
    owners = {}
    for kind, reference in manifest["images"].items():
        tags = promotion_tags(kind, channel, daemon_version, sandbox_version, gateway_version)
        for tag in tags:
            if tag in owners:
                raise ValueError(f"Image alias collision: {tag} belongs to both {owners[tag]} and {kind}; use distinct daemon and sandbox versions")
            owners[tag] = kind
        plan.append((tags, reference))
    verify_registry(manifest)
    for tags, reference in plan:
        command = ["docker", "buildx", "imagetools", "create"]
        for tag in tags:
            command.extend(["--tag", tag])
        subprocess.run([*command, reference], check=True, timeout=120)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    write = sub.add_parser("record")
    write.add_argument("--kind", required=True)
    write.add_argument("--digest", required=True)
    write.add_argument("--revision", required=True)
    write.add_argument("--output", type=Path, required=True)
    combine = sub.add_parser("assemble")
    combine.add_argument("--directory", type=Path, required=True)
    combine.add_argument("--revision", required=True)
    combine.add_argument("--output", type=Path, required=True)
    combine.add_argument("--dry-run", action="store_true")
    promote = sub.add_parser("promote")
    promote.add_argument("--manifest", type=Path, required=True)
    promote.add_argument("--revision", required=True)
    promote.add_argument("--channel", choices=("stable", "beta"), required=True)
    for name in ("daemon", "sandbox", "gateway"):
        promote.add_argument(f"--{name}-version", required=True)
    args = parser.parse_args()
    if args.command == "promote":
        manifest = assemble([json.loads(args.manifest.read_text())], args.revision)
        promote_images(manifest, args.channel, args.daemon_version,
                       args.sandbox_version, args.gateway_version)
        return
    if args.command == "record":
        manifest = record(args.kind, args.digest, args.revision)
    else:
        manifest = assemble(
            [json.loads(path.read_text()) for path in sorted(args.directory.glob("*.json"))],
            args.revision)
        if not args.dry_run:
            verify_registry(manifest)
    args.output.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
