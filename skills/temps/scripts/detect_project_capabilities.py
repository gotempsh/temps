#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Read-only framework and Temps observability capability detector."""

from __future__ import annotations

import argparse
import json
import os
import stat
from pathlib import Path


IGNORED_DIRS = {
    ".agents",
    ".claude",
    ".codex",
    ".git",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".venv",
    "build",
    "dist",
    "docs",
    "examples",
    "node_modules",
    "skills",
    "target",
    "test",
    "tests",
    "vendor",
    "__tests__",
}
MAX_FILE_BYTES = 512_000
TEXT_SUFFIXES = {
    ".cjs",
    ".go",
    ".html",
    ".js",
    ".jsx",
    ".mjs",
    ".py",
    ".rb",
    ".rs",
    ".toml",
    ".ts",
    ".tsx",
    ".vue",
    ".yaml",
    ".yml",
}

MANIFEST_NAMES = {
    "Cargo.toml",
    "Gemfile",
    "composer.json",
    "go.mod",
    "package.json",
    "pyproject.toml",
    "requirements.txt",
}

PACKAGE_FRAMEWORK_MARKERS = {
    "nextjs": ("\"next\"",),
    "react": ("\"react\"",),
    "vue": ("\"vue\"", "\"nuxt\""),
    "svelte": ("\"svelte\"", "\"@sveltejs/kit\""),
    "node": ("\"express\"", "\"fastify\"", "\"@nestjs/core\""),
}

FILE_FRAMEWORK_MARKERS = {
    "python": ("pyproject.toml", "requirements.txt"),
    "rust": ("Cargo.toml",),
    "go": ("go.mod",),
    "ruby": ("Gemfile",),
    "php": ("composer.json",),
}

CAPABILITY_MARKERS = {
    "analytics": (
        (
            "@temps-sdk/react-analytics",
            "@temps-sdk/analytics-browser",
            "@temps-sdk/vue-analytics",
            "@temps-sdk/svelte-analytics",
        ),
        ("TempsAnalyticsProvider", "TempsAnalytics", "temps-event-name", "data-temps-event"),
    ),
    "error_tracking": (
        ("@sentry/", "sentry-sdk", "sentry =", "github.com/getsentry/sentry-go"),
        ("sentry_sdk.init", "sentry::init", "sentry.Init", "Sentry.init"),
    ),
    "tracing": (
        (
            "@opentelemetry/",
            "opentelemetry-",
            "opentelemetry_",
            "tracing-opentelemetry",
            "go.opentelemetry.io/otel",
        ),
        (
            "TracerProvider",
            "NodeSDK",
            "configure_opentelemetry",
            "install_batch",
            "OTLPSpanExporter",
            "start_as_current_span",
        ),
    ),
}


def candidate_files(root: Path):
    resolved_root = root.resolve()
    for path in root.rglob("*"):
        if any(part in IGNORED_DIRS for part in path.parts):
            continue
        if not (
            path.name in MANIFEST_NAMES
            or path.suffix in TEXT_SUFFIXES
            or path.suffix == ".csproj"
        ):
            continue
        try:
            relative = path.relative_to(root)
            current = root
            if any((current := current / part).is_symlink() for part in relative.parts):
                continue
            resolved_path = path.resolve(strict=True)
            if not resolved_path.is_relative_to(resolved_root):
                continue
        except (OSError, RuntimeError, ValueError):
            continue
        yield path


def read_candidate(path: Path) -> str | None:
    # Opening a FIFO read-only blocks until a writer connects. Open candidates
    # non-blocking first, then use fstat on the descriptor as the authoritative
    # file-type check before reading any bytes.
    flags = (
        os.O_RDONLY
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_NONBLOCK", 0)
    )
    try:
        descriptor = os.open(path, flags)
    except OSError:
        return None
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > MAX_FILE_BYTES:
            return None
        with os.fdopen(descriptor, "rb", closefd=False) as file:
            contents = file.read(MAX_FILE_BYTES + 1)
        if len(contents) > MAX_FILE_BYTES:
            return None
        return contents.decode("utf-8", errors="ignore")
    except OSError:
        return None
    finally:
        os.close(descriptor)


def detect(root: Path) -> dict[str, object]:
    files: list[tuple[Path, str]] = []
    for path in candidate_files(root):
        text = read_candidate(path)
        if text is not None:
            files.append((path, text))

    frameworks: list[dict[str, str]] = []
    package_manifests = [(path, text) for path, text in files if path.name == "package.json"]
    for name, markers in PACKAGE_FRAMEWORK_MARKERS.items():
        match = next(
            (path for path, text in package_manifests if any(marker in text for marker in markers)),
            None,
        )
        if match:
            frameworks.append({"name": name, "evidence": str(match.relative_to(root))})
    for name, marker_names in FILE_FRAMEWORK_MARKERS.items():
        match = next((path for path, _ in files if path.name in marker_names), None)
        if match:
            frameworks.append({"name": name, "evidence": str(match.relative_to(root))})
    dotnet = next((path for path, _ in files if path.suffix == ".csproj"), None)
    if dotnet:
        frameworks.append({"name": "dotnet", "evidence": str(dotnet.relative_to(root))})

    capabilities: dict[str, dict[str, object]] = {}
    for name, (dependency_markers, initialization_markers) in CAPABILITY_MARKERS.items():
        dependency_evidence: list[str] = []
        initialization_evidence: list[str] = []
        for path, text in files:
            relative = str(path.relative_to(root))
            if path.name in MANIFEST_NAMES and any(marker in text for marker in dependency_markers):
                dependency_evidence.append(relative)
            if path.name not in MANIFEST_NAMES and any(marker in text for marker in initialization_markers):
                initialization_evidence.append(relative)
        dependency_evidence = sorted(set(dependency_evidence))[:5]
        initialization_evidence = sorted(set(initialization_evidence))[:5]
        if initialization_evidence:
            status = "configured"
        elif dependency_evidence:
            status = "partial"
        else:
            status = "missing"
        capabilities[name] = {
            "status": status,
            "dependency_evidence": dependency_evidence,
            "initialization_evidence": initialization_evidence,
        }

    return {
        "root": str(root),
        "frameworks": frameworks,
        "capabilities": capabilities,
        "note": (
            "Static evidence only: partial means a dependency exists without initialization; "
            "configured still requires a real signal before claiming the integration works."
        ),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path.cwd())
    args = parser.parse_args()
    root = args.root.resolve()
    if not root.is_dir():
        parser.error(f"project root is not a directory: {root}")
    print(json.dumps(detect(root), indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
