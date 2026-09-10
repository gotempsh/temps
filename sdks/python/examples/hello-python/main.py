# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Example Temps plugin built with the Python SDK.

Demonstrates:
- Defining a manifest with navigation entries
- Handling HTTP requests with auth context
- Subscribing to deployment events
- Serving an embedded React web UI
"""

from __future__ import annotations

import json
import sys

from aiohttp import web

from temps_plugin import (
    PluginContext,
    PluginEvent,
    create_manifest,
    extract_auth_context,
    run_plugin,
)
from temps_plugin.types import EmbeddedAssets, PluginManifest

# Try to load embedded UI assets (generated at build time)
try:
    from _embedded_ui import embedded_assets  # type: ignore[import-not-found]
except ImportError:
    embedded_assets: EmbeddedAssets | None = None  # type: ignore[no-redef]


class HelloPythonPlugin:
    """Example plugin implementation."""

    def manifest(self) -> PluginManifest:
        return (
            create_manifest("hello-python", "0.1.0")
            .display_name("Hello Python")
            .description("Example Python plugin demonstrating the SDK")
            .requires_db(False)
            .add_nav("Hello", "hand", "/hello")
            .add_nav("Projects", "folder", "/projects")
            .event("deployment.succeeded")
            .event("deployment.failed")
            .build()
        )

    def embedded_ui_assets(self) -> EmbeddedAssets | None:
        return embedded_assets

    def handler(self, ctx: PluginContext):
        async def handle(request: web.Request) -> web.Response:
            path = request.path
            headers = dict(request.headers)
            auth = extract_auth_context({k.lower(): v for k, v in headers.items()})

            if path == "/hello":
                return web.json_response(
                    {
                        "message": f"Hello {auth.user_email or 'anonymous'}!",
                        "plugin": ctx.plugin_name,
                        "dataDir": ctx.data_dir,
                    }
                )

            if path == "/projects":
                if ctx.temps:
                    try:
                        projects = await ctx.temps.list_projects()
                        return web.json_response(
                            {
                                "projects": [
                                    {
                                        "id": p.get("id"),
                                        "name": p.get("name"),
                                        "preset": p.get("preset"),
                                    }
                                    for p in projects
                                ]
                            }
                        )
                    except Exception as e:
                        return web.json_response({"error": str(e)}, status=500)
                return web.json_response({"projects": []})

            return web.json_response({"error": "Not found"}, status=404)

        return handle

    async def on_start(self, ctx: PluginContext) -> None:
        print(
            f"[hello-python] Plugin started! Data dir: {ctx.data_dir}",
            file=sys.stderr,
        )

    def on_event(self, _ctx: PluginContext, event: PluginEvent) -> None:
        print(
            f"[hello-python] Received event: {event.event_type} "
            f"(project: {event.project_id})",
            file=sys.stderr,
        )

    async def on_shutdown(self) -> None:
        print("[hello-python] Shutting down gracefully", file=sys.stderr)


if __name__ == "__main__":
    run_plugin(HelloPythonPlugin())
