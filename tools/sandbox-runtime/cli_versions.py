# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Resolve the coding-agent CLI releases a published runtime image bundles.

The Dockerfile pins a version of each CLI (`ARG CODEX_VERSION=...`), which
local and pull-request builds use so they stay reproducible. A published image
instead bundles the latest stable release on the npm registry at build time,
so a new image never ships a CLI older than its build date. The resolved
versions go to `build-args` and to image labels, so every published image
records exactly what it contains.

Safety rules:
  * only a plain `major.minor.patch` version of the expected package is
    accepted, so a registry answer can never inject anything into the build;
  * the result is never older than the Dockerfile pin;
  * when the registry cannot be read, the pin is used and a warning emitted.

Usage: python3 cli_versions.py >> "$GITHUB_OUTPUT"
"""

import json
from pathlib import Path
import re
import sys
import urllib.parse
import urllib.request

PACKAGE = Path(__file__).resolve().parent
REGISTRY = "https://registry.npmjs.org/"
MAX_BYTES = 1024 * 1024
TIMEOUT_SECONDS = 15
# Build argument -> npm package.
CLIS = {
    "CODEX_VERSION": "@openai/codex",
    "CLAUDE_VERSION": "@anthropic-ai/claude-code",
    "OPENCODE_VERSION": "opencode-ai",
}
VERSION = re.compile(r"^(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})$")


def pinned_versions(dockerfile):
    """The `ARG <NAME>=<version>` defaults the Dockerfile declares."""
    pins = {}
    for argument in CLIS:
        match = re.search(rf"^ARG {argument}=(\S+)$", dockerfile, re.MULTILINE)
        if not match or not VERSION.match(match.group(1)):
            raise ValueError(f"Dockerfile must pin {argument} to a major.minor.patch version")
        pins[argument] = match.group(1)
    return pins


def numeric(version):
    return tuple(int(part) for part in version.split("."))


def parse_latest(package, body):
    """The version in a registry `/latest` document, or a ValueError."""
    document = json.loads(body)
    if not isinstance(document, dict) or document.get("name") != package:
        raise ValueError(f"the registry document does not describe {package}")
    version = document.get("version")
    if not isinstance(version, str) or not VERSION.match(version):
        raise ValueError(f"{package} latest version {version!r} is not a stable release")
    return version


class _NoRedirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, message, headers, new_url):
        raise ValueError(f"the registry redirected to {new_url}; redirects are not followed")


def latest_url(package):
    return urllib.parse.urljoin(REGISTRY, urllib.parse.quote(package, safe="@") + "/latest")


def fetch_latest(package):
    url = latest_url(package)
    request = urllib.request.Request(url, headers={"Accept": "application/json", "User-Agent": "temps-image-build"})
    opener = urllib.request.build_opener(_NoRedirects)
    with opener.open(request, timeout=TIMEOUT_SECONDS) as response:
        body = response.read(MAX_BYTES + 1)
    if len(body) > MAX_BYTES:
        raise ValueError(f"the registry answer for {package} is larger than {MAX_BYTES} bytes")
    return parse_latest(package, body)


def resolve(pins, fetch=fetch_latest, warn=lambda message: print(f"::warning::{message}", file=sys.stderr)):
    """Latest release of every CLI, never older than its pin."""
    resolved = {}
    for argument, package in CLIS.items():
        pin = pins[argument]
        try:
            latest = fetch(package)
        except Exception as error:  # noqa: BLE001 - any failure falls back to the pin
            warn(f"Could not resolve the latest {package} release ({error}); bundling the pinned {pin}")
            resolved[argument] = pin
            continue
        resolved[argument] = latest if numeric(latest) >= numeric(pin) else pin
    return resolved


def github_output(resolved):
    """`build_args` and `labels` outputs for docker/build-push-action."""
    labels = {
        "CODEX_VERSION": "sh.temps.cli.codex",
        "CLAUDE_VERSION": "sh.temps.cli.claude-code",
        "OPENCODE_VERSION": "sh.temps.cli.opencode",
    }
    lines = ["cli_build_args<<__CLI_BUILD_ARGS__"]
    lines += [f"{argument}={version}" for argument, version in resolved.items()]
    lines += ["__CLI_BUILD_ARGS__", "cli_labels<<__CLI_LABELS__"]
    lines += [f"{labels[argument]}={version}" for argument, version in resolved.items()]
    lines.append("__CLI_LABELS__")
    return "\n".join(lines) + "\n"


def main():
    pins = pinned_versions((PACKAGE / "Dockerfile").read_text())
    sys.stdout.write(github_output(resolve(pins)))


if __name__ == "__main__":
    main()
