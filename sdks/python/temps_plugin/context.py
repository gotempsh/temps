# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""PluginContext — provides access to platform client and plugin metadata.

Mirrors: sdks/node/packages/plugin-sdk/src/context.ts
"""

from __future__ import annotations

from temps_plugin.client import TempsClient


class PluginContext:
    """Context object passed to plugin lifecycle hooks and handlers."""

    def __init__(
        self,
        *,
        plugin_name: str,
        data_dir: str,
        auth_secret: str,
        client: TempsClient | None = None,
    ) -> None:
        self.plugin_name = plugin_name
        self.data_dir = data_dir
        self.auth_secret = auth_secret
        self.temps = client

    @property
    def has_client(self) -> bool:
        """Whether the platform client is available."""
        return self.temps is not None
