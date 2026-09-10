# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Protocol helpers for the Temps plugin handshake.

Mirrors: sdks/node/packages/plugin-sdk/src/protocol.ts
"""

from __future__ import annotations

import json
import sys
from typing import Any

from temps_plugin.types import AuthContext, PluginManifest

# Header constants (set by the Temps host on proxied requests)
HEADER_USER_ID = "x-temps-user-id"
HEADER_USER_EMAIL = "x-temps-user-email"
HEADER_USER_ROLE = "x-temps-user-role"
HEADER_REQUEST_ID = "x-temps-request-id"

# WebSocket channel path
PLUGIN_CHANNEL_PATH = "/_temps/channel"


def emit_manifest(manifest: PluginManifest) -> None:
    """Emit the manifest JSON to stdout (handshake phase 1)."""
    line = json.dumps(manifest.to_dict(), separators=(",", ":"))
    sys.stdout.write(line + "\n")
    sys.stdout.flush()


def emit_ready(has_ui: bool) -> None:
    """Emit the ready message to stdout (handshake phase 2)."""
    msg: dict[str, Any] = {"type": "ready", "ready": True, "has_ui": has_ui}
    line = json.dumps(msg, separators=(",", ":"))
    sys.stdout.write(line + "\n")
    sys.stdout.flush()


def extract_auth_context(headers: dict[str, str]) -> AuthContext:
    """Extract authentication context from request headers."""
    return AuthContext(
        user_id=headers.get(HEADER_USER_ID),
        user_email=headers.get(HEADER_USER_EMAIL),
        user_role=headers.get(HEADER_USER_ROLE),
        request_id=headers.get(HEADER_REQUEST_ID),
    )
