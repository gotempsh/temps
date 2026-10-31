#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Generate and check docs/architecture/crates.md, the map of the Rust workspace.

The map lists every workspace member with its Cargo `description`, its layer,
the workspace crates that depend on it directly ("consumers") and the
binaries it builds. Everything except the layer comes from
`cargo metadata --format-version 1 --no-deps`; the layer comes from LAYERS
below, which is the one hand-maintained part.

Usage:
    python3 scripts/crate_map.py            # regenerate docs/architecture/crates.md
    python3 scripts/crate_map.py --check    # CI: fail if the file is stale, a member
                                            # has no description, or a crate has no layer

Adding a crate: give its Cargo.toml a one-line `description`, add it to LAYERS,
run this script and commit the regenerated map.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

OUTPUT = Path("docs/architecture/crates.md")

# Layers and their tier, lowest tier first. A crate may depend on crates in a
# lower tier and on peers in its own tier; the map lists every edge that points
# to a higher tier, so the dependency-graph work can see what is left to cut.
LAYER_TIERS: dict[str, int] = {
    "library": 0,
    "foundation": 1,
    "toolkit": 2,
    "runtime": 2,
    "traffic": 2,
    "data-browser": 2,
    "domain": 3,
    "observability": 3,
    "ai": 3,
    "import": 3,
    "application": 4,
    "example": 4,
    "test-support": 4,
}

LAYER_DESCRIPTIONS: dict[str, str] = {
    "library": "Self-contained libraries, leaf types and wire protocols with few or no internal dependencies.",
    "foundation": "Shared types, persistence, settings and auth that nearly every crate builds on.",
    "toolkit": "Stateless helper libraries built on the foundation: project detection, screenshots, the plugin SDK.",
    "runtime": "Containers, networking, DNS and the processes that run on worker nodes and inside sandboxes.",
    "traffic": "The request path: reverse proxy, edge cache, route table and bot protection.",
    "data-browser": "The data browser: one trait and a driver per external datastore.",
    "domain": "Control-plane features: a service layer plus HTTP handlers for one product area.",
    "observability": "Analytics, error tracking, OpenTelemetry, metrics and logs: ingest, storage and queries.",
    "ai": "AI foundation, agents, chat, gateway and the MCP surfaces.",
    "import": "Importing workloads from other platforms: the orchestrator and one adapter per platform.",
    "application": "Composition roots that wire the crates into the shipped binaries.",
    "example": "Example external plugins built on the plugin SDK.",
    "test-support": "Crates used only by tests.",
}

LAYERS: dict[str, str] = {
    # foundation
    "temps-core": "foundation",
    "temps-database": "foundation",
    "temps-entities": "foundation",
    "temps-migrations": "foundation",
    "temps-queue": "foundation",
    "temps-config": "foundation",
    "temps-auth": "foundation",
    "temps-clickhouse": "foundation",
    # library
    "temps-captcha-wasm": "library",
    "temps-cloud-protocol": "library",
    "temps-credential-checks": "library",
    "temps-embeddings": "library",
    "temps-file-store": "library",
    "temps-memory": "library",
    "temps-netpol-ebpf-common": "library",
    "temps-signatures": "library",
    "temps-workload": "library",
    # toolkit
    "temps-plugin-sdk": "toolkit",
    "temps-presets": "toolkit",
    "temps-screenshots": "toolkit",
    # runtime
    "temps-deployer": "runtime",
    "temps-git-credential": "runtime",
    "temps-netpol-probe": "runtime",
    "temps-network": "runtime",
    "temps-network-policy": "runtime",
    "temps-node-agent": "runtime",
    "temps-node-dns": "runtime",
    "temps-preview-gateway": "runtime",
    "temps-pty-agent": "runtime",
    "temps-runtime-health": "runtime",
    "temps-vm-agent": "runtime",
    "temps-wireguard": "runtime",
    "temps-workload-activator": "runtime",
    "temps-workload-docker": "runtime",
    "temps-workload-env": "runtime",
    "temps-workload-ingress": "runtime",
    "temps-workload-netpol": "runtime",
    "temps-workload-node": "runtime",
    "temps-workload-plane": "runtime",
    "temps-workload-volumes": "runtime",
    # traffic
    "temps-edge": "traffic",
    "temps-edge-rules": "traffic",
    "temps-proxy": "traffic",
    "temps-routes": "traffic",
    "temps-shield": "traffic",
    # domain
    "temps-audit": "domain",
    "temps-backup": "domain",
    "temps-backup-core": "domain",
    "temps-blob": "domain",
    "temps-cloud": "domain",
    "temps-cloud-client": "domain",
    "temps-deployments": "domain",
    "temps-dns": "domain",
    "temps-domains": "domain",
    "temps-email": "domain",
    "temps-email-tracking": "domain",
    "temps-environments": "domain",
    "temps-external-plugins": "domain",
    "temps-flags": "domain",
    "temps-geo": "domain",
    "temps-git": "domain",
    "temps-infra": "domain",
    "temps-ingress": "domain",
    "temps-kv": "domain",
    "temps-monitoring": "domain",
    "temps-notifications": "domain",
    "temps-product-telemetry": "domain",
    "temps-project-groups": "domain",
    "temps-projects": "domain",
    "temps-providers": "domain",
    "temps-revenue": "domain",
    "temps-sandbox": "domain",
    "temps-static-files": "domain",
    "temps-status-page": "domain",
    "temps-teams": "domain",
    "temps-vulnerability-scanner": "domain",
    "temps-webhooks": "domain",
    # observability
    "temps-analytics": "observability",
    "temps-analytics-backend": "observability",
    "temps-analytics-events": "observability",
    "temps-analytics-funnels": "observability",
    "temps-analytics-performance": "observability",
    "temps-analytics-session-replay": "observability",
    "temps-deployment-logs": "observability",
    "temps-error-tracking": "observability",
    "temps-log-aggregator": "observability",
    "temps-metrics": "observability",
    "temps-observability": "observability",
    "temps-otel": "observability",
    # ai
    "temps-agents-mcp-proxy": "ai",
    "temps-ai": "ai",
    "temps-ai-agent-cli": "ai",
    "temps-ai-agents": "ai",
    "temps-ai-api-tools": "ai",
    "temps-ai-chat": "ai",
    "temps-ai-gateway": "ai",
    "temps-mcp-server": "ai",
    # data-browser
    "temps-query": "data-browser",
    "temps-query-mongodb": "data-browser",
    "temps-query-postgres": "data-browser",
    "temps-query-redis": "data-browser",
    "temps-query-s3": "data-browser",
    # import
    "temps-import": "import",
    "temps-import-caprover": "import",
    "temps-import-coolify": "import",
    "temps-import-docker": "import",
    "temps-import-dokploy": "import",
    "temps-import-kamal": "import",
    "temps-import-kubernetes": "import",
    "temps-import-portainer": "import",
    "temps-import-types": "import",
    # application
    "temps-cli": "application",
    # example
    "temps-example-plugin": "example",
    "temps-google-indexing-plugin": "example",
    "temps-indexnow-plugin": "example",
    "temps-lighthouse-plugin": "example",
    # test-support
    "temps-docker-capture": "test-support",
}


class CrateMapError(Exception):
    """The workspace or the layer mapping violates a crate-map rule."""


@dataclass
class Crate:
    name: str
    description: str | None
    path: str  # crate directory, relative to the workspace root
    binaries: list[str]
    dependencies: set[str] = field(default_factory=set)  # internal, non-dev
    consumers: set[str] = field(default_factory=set)  # internal, non-dev


def load_metadata(root: Path) -> dict:
    result = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        cwd=root,
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise CrateMapError(f"cargo metadata failed in {root}:\n{result.stderr.strip()}")
    return json.loads(result.stdout)


def build_crates(metadata: dict) -> dict[str, Crate]:
    """Workspace members keyed by name, with internal edges filled in."""
    root = Path(metadata["workspace_root"])
    members = set(metadata["workspace_members"])
    crates: dict[str, Crate] = {}
    for package in metadata["packages"]:
        if package["id"] not in members:
            continue
        manifest_dir = Path(package["manifest_path"]).parent
        description = (package.get("description") or "").strip() or None
        crates[package["name"]] = Crate(
            name=package["name"],
            description=description,
            path=manifest_dir.relative_to(root).as_posix(),
            binaries=sorted(t["name"] for t in package["targets"] if "bin" in t["kind"]),
        )
    for package in metadata["packages"]:
        crate = crates.get(package["name"])
        if crate is None or package["id"] not in members:
            continue
        for dependency in package["dependencies"]:
            target = dependency["name"]
            # Dev-dependencies are test wiring, not an architectural edge.
            if dependency.get("kind") == "dev" or not dependency.get("path"):
                continue
            if target in crates and target != crate.name:
                crate.dependencies.add(target)
                crates[target].consumers.add(crate.name)
    return crates


def validate(crates: dict[str, Crate], layers: dict[str, str]) -> None:
    errors: list[str] = []
    for name in sorted(crates):
        crate = crates[name]
        if crate.description is None:
            errors.append(f"{crate.path}/Cargo.toml: add a one-line `description` to [package] for {name}")
        if name not in layers:
            errors.append(f"{name} has no layer: add it to LAYERS in scripts/crate_map.py")
    for name, layer in sorted(layers.items()):
        if name not in crates:
            errors.append(f"LAYERS lists {name}, which is not a workspace member: remove it")
        if layer not in LAYER_DESCRIPTIONS or layer not in LAYER_TIERS:
            errors.append(f"LAYERS puts {name} in unknown layer {layer!r}")
    if errors:
        raise CrateMapError("\n".join(errors))


def short(name: str) -> str:
    return name.removeprefix("temps-")


def render(crates: dict[str, Crate], layers: dict[str, str]) -> str:
    out = [
        "<!--",
        "SPDX-FileCopyrightText: 2024-2026 Temps Contributors",
        "SPDX-License-Identifier: MIT OR Apache-2.0",
        "-->",
        "",
        "# Crate map",
        "",
        "<!-- Generated by scripts/crate_map.py from `cargo metadata`. Do not edit by hand:",
        "     run `python3 scripts/crate_map.py` and commit the result. -->",
        "",
        f"The workspace has {len(crates)} crates. Each row shows the crate's Cargo `description`,",
        "the workspace crates that depend on it directly (**consumers**, without the `temps-`",
        "prefix; dev-dependencies are not counted) and the binaries it builds.",
        "",
        "To add a crate: give its `Cargo.toml` a one-line `description`, assign it a layer",
        "in `LAYERS` in `scripts/crate_map.py`, then run `python3 scripts/crate_map.py`.",
        "CI runs `python3 scripts/crate_map.py --check`, which fails when this file is",
        "stale, a crate has no description, or a crate has no layer.",
        "",
        "## Layers",
        "",
        "A crate may depend on crates in a lower tier and on peers in its own tier.",
        "[Edges against the layer order](#edges-against-the-layer-order) lists the exceptions.",
        "",
        "| Layer | Tier | Crates | What lives there |",
        "|---|---|---|---|",
    ]
    by_layer: dict[str, list[Crate]] = {layer: [] for layer in LAYER_DESCRIPTIONS}
    for name in sorted(crates):
        by_layer[layers[name]].append(crates[name])
    for layer, description in LAYER_DESCRIPTIONS.items():
        if by_layer[layer]:
            out.append(f"| [{layer}](#{layer}) | {LAYER_TIERS[layer]} | {len(by_layer[layer])} | {description} |")

    for layer, description in LAYER_DESCRIPTIONS.items():
        members = by_layer[layer]
        if not members:
            continue
        out += [
            "",
            f"## {layer}",
            "",
            description,
            "",
            "| Crate | Description | Consumers | Binaries |",
            "|---|---|---|---|",
        ]
        for crate in members:
            consumers = ", ".join(short(c) for c in sorted(crate.consumers))
            consumer_cell = f"**{len(crate.consumers)}**: {consumers}" if crate.consumers else "none"
            binaries = ", ".join(f"`{b}`" for b in crate.binaries) or ""
            description_text = (crate.description or "").replace("|", "\\|")
            out.append(
                f"| [`{crate.name}`](../../{crate.path}) | {description_text} | {consumer_cell} | {binaries} |"
            )

    upward = upward_edges(crates, layers)
    out += [
        "",
        "## Edges against the layer order",
        "",
        "Dependencies on a crate in a higher tier. Each one widens what a change rebuilds;",
        "cut them rather than adding more.",
        "",
    ]
    if upward:
        out += ["| Crate | Depends on |", "|---|---|"]
        for name, dependency in upward:
            out.append(
                f"| `{name}` ({layers[name]}) | `{dependency}` ({layers[dependency]}) |"
            )
    else:
        out.append("None.")
    out.append("")
    return "\n".join(out)


def upward_edges(crates: dict[str, Crate], layers: dict[str, str]) -> list[tuple[str, str]]:
    return [
        (name, dependency)
        for name in sorted(crates)
        for dependency in sorted(crates[name].dependencies)
        if LAYER_TIERS[layers[dependency]] > LAYER_TIERS[layers[name]]
    ]


def generate(root: Path, metadata: dict | None = None) -> str:
    crates = build_crates(metadata if metadata is not None else load_metadata(root))
    validate(crates, LAYERS)
    return render(crates, LAYERS)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--check", action="store_true", help="fail instead of writing when the map is stale")
    parser.add_argument(
        "--metadata",
        type=Path,
        help="read `cargo metadata --format-version 1 --no-deps` output from this file instead of running cargo",
    )
    args = parser.parse_args(argv)

    try:
        metadata = json.loads(args.metadata.read_text(encoding="utf-8")) if args.metadata else None
        expected = generate(args.root, metadata)
    except CrateMapError as error:
        print(f"crate map check failed:\n{error}", file=sys.stderr)
        return 1

    output = args.root / OUTPUT
    if args.check:
        current = output.read_text(encoding="utf-8") if output.exists() else ""
        if current != expected:
            print(
                f"{OUTPUT} is out of date. Run `python3 scripts/crate_map.py` and commit the result.",
                file=sys.stderr,
            )
            return 1
        print(f"crate map OK: {OUTPUT}")
        return 0
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(expected, encoding="utf-8")
    print(f"wrote {OUTPUT}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
