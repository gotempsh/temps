# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Protocol types matching the Rust plugin SDK.

Mirrors: crates/temps-core/src/external_plugin/manifest.rs
         crates/temps-core/src/external_plugin/channel.rs
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any


class NavSection(str, Enum):
    """Where a navigation entry appears in the Temps sidebar."""

    PLATFORM = "platform"
    SETTINGS = "settings"


@dataclass
class NavEntry:
    """A navigation entry for the plugin UI."""

    label: str
    icon: str
    path: str
    section: NavSection = NavSection.PLATFORM
    order: int = 50

    def to_dict(self) -> dict[str, Any]:
        return {
            "label": self.label,
            "icon": self.icon,
            "path": self.path,
            "section": self.section.value,
            "order": self.order,
        }


@dataclass
class PluginManifest:
    """Plugin manifest sent to the host during handshake."""

    name: str
    version: str
    display_name: str | None = None
    description: str | None = None
    nav: list[NavEntry] = field(default_factory=list)
    requires_db: bool = False
    health_path: str = "/health"
    events: list[str] = field(default_factory=list)

    def to_dict(self) -> dict[str, Any]:
        return {
            "type": "manifest",
            "name": self.name,
            "version": self.version,
            "display_name": self.display_name or self.name,
            "description": self.description or "",
            "nav": [n.to_dict() for n in self.nav],
            "requires_db": self.requires_db,
            "health_path": self.health_path,
            "events": self.events,
        }


@dataclass
class PluginArgs:
    """CLI arguments passed by the Temps host."""

    socket_path: str
    auth_secret: str
    data_dir: str
    database_url: str | None = None


@dataclass
class PluginEvent:
    """An event delivered from the Temps platform."""

    id: str
    event_type: str
    timestamp: str
    project_id: int | None = None
    data: dict[str, Any] = field(default_factory=dict)


@dataclass
class AuthContext:
    """Authentication context extracted from request headers."""

    user_id: str | None = None
    user_email: str | None = None
    user_role: str | None = None
    request_id: str | None = None


# -- Channel message types (WebSocket) --


@dataclass
class ChannelRequest:
    """A request from the plugin to the host via the WebSocket channel."""

    id: str
    method: str
    params: dict[str, Any] = field(default_factory=dict)


@dataclass
class ChannelResponse:
    """A response from the host to the plugin via the WebSocket channel."""

    id: str
    result: Any = None
    error: str | None = None


# Embedded UI types


@dataclass
class EmbeddedFile:
    """A file embedded into the binary at build time."""

    content: bytes
    content_type: str
    immutable: bool = False


# Type alias for the embedded assets map
EmbeddedAssets = dict[str, EmbeddedFile]
