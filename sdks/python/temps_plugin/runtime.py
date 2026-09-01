# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Plugin runtime — handles the full lifecycle of a Temps external plugin.

Mirrors: sdks/node/packages/plugin-sdk/src/runtime.ts

Lifecycle:
1. Parse CLI arguments (--socket-path, --auth-secret, --data-dir)
2. Emit manifest to stdout (handshake phase 1)
3. Start aiohttp server on Unix domain socket
4. Emit ready to stdout (handshake phase 2)
5. Accept WebSocket connection from host on /_temps/channel
6. Create PluginContext with TempsClient
7. Call plugin.on_start()
8. Serve plugin routes, health, events, and embedded UI
9. Handle SIGTERM for graceful shutdown
"""

from __future__ import annotations

import asyncio
import json
import os
import signal
import sys
from pathlib import Path
from typing import Any, Awaitable, Callable, Protocol

from aiohttp import web

from temps_plugin.client import TempsClient
from temps_plugin.context import PluginContext
from temps_plugin.protocol import (
    PLUGIN_CHANNEL_PATH,
    emit_manifest,
    emit_ready,
    extract_auth_context,
)
from temps_plugin.types import (
    EmbeddedAssets,
    PluginArgs,
    PluginEvent,
    PluginManifest,
)
from temps_plugin.ui import serve_embedded_ui

# Type aliases for plugin callbacks
RequestHandler = Callable[[web.Request], Awaitable[web.Response]]


class TempsPlugin(Protocol):
    """Protocol that plugin implementations must satisfy."""

    def manifest(self) -> PluginManifest: ...
    def handler(self, ctx: PluginContext) -> RequestHandler: ...
    def embedded_ui_assets(self) -> EmbeddedAssets | None: ...

    async def on_start(self, ctx: PluginContext) -> None: ...
    def on_event(self, ctx: PluginContext, event: PluginEvent) -> None: ...
    async def on_shutdown(self) -> None: ...


def run_plugin(plugin: TempsPlugin) -> None:
    """Run a Temps plugin. This is the main entry point."""
    asyncio.run(_run_plugin_async(plugin))


async def _run_plugin_async(plugin: TempsPlugin) -> None:
    # 1. Parse CLI arguments
    args = _parse_args(sys.argv[1:])
    os.makedirs(args.data_dir, exist_ok=True)

    manifest = plugin.manifest()

    # 2. Emit manifest (handshake phase 1)
    emit_manifest(manifest)

    # Ensure socket directory exists and clean up stale socket
    socket_dir = str(Path(args.socket_path).parent)
    os.makedirs(socket_dir, exist_ok=True)
    if os.path.exists(args.socket_path):
        os.unlink(args.socket_path)

    # Channel connection tracking
    channel_connected: asyncio.Future[TempsClient] = (
        asyncio.get_event_loop().create_future()
    )
    context: PluginContext | None = None
    plugin_ready = False

    # Embedded UI
    embedded_assets = plugin.embedded_ui_assets()
    has_ui = embedded_assets is not None

    # Plugin request handler (set after initialization)
    plugin_handler: RequestHandler | None = None

    # -- aiohttp request handlers --

    async def handle_health(_request: web.Request) -> web.Response:
        return web.json_response({"status": "ok", "plugin": manifest.name})

    async def handle_ws_channel(request: web.Request) -> web.WebSocketResponse:
        ws = web.WebSocketResponse()
        await ws.prepare(request)

        client = TempsClient(ws)
        client.start_reader()

        if not channel_connected.done():
            channel_connected.set_result(client)

        # Keep the WebSocket open until it closes
        await client._reader_task
        return ws

    async def handle_events(request: web.Request) -> web.Response:
        try:
            data = await request.json()
            event = PluginEvent(
                id=data.get("id", ""),
                event_type=data.get("event_type", ""),
                timestamp=data.get("timestamp", ""),
                project_id=data.get("project_id"),
                data=data.get("data", {}),
            )
            if context:
                plugin.on_event(context, event)
            return web.json_response({"ok": True})
        except Exception as e:
            return web.json_response(
                {"error": "Invalid event payload", "detail": str(e)},
                status=400,
            )

    async def handle_ui(request: web.Request) -> web.Response:
        if embedded_assets is None:
            return web.Response(text="Not Found", status=404)
        return serve_embedded_ui(request.path, embedded_assets)

    async def handle_catch_all(request: web.Request) -> web.Response:
        path = request.path

        # UI redirect
        if path == "/ui":
            raise web.HTTPFound("/ui/")

        # Embedded UI
        if path.startswith("/ui/") and embedded_assets:
            return serve_embedded_ui(path, embedded_assets)

        # Not initialized yet
        if not plugin_ready or plugin_handler is None:
            return web.Response(text="Plugin initializing", status=503)

        # Delegate to plugin handler
        return await plugin_handler(request)

    # 3. Build and start the aiohttp app
    app = web.Application()
    app.router.add_get(manifest.health_path, handle_health)
    if manifest.health_path != "/health":
        app.router.add_get("/health", handle_health)
    app.router.add_get(PLUGIN_CHANNEL_PATH, handle_ws_channel)
    app.router.add_post("/_events", handle_events)
    # Catch-all for plugin routes and UI
    app.router.add_route("*", "/{path_info:.*}", handle_catch_all)

    runner = web.AppRunner(app)
    await runner.setup()
    site = web.UnixSite(runner, args.socket_path)
    await site.start()

    # 4. Emit ready (handshake phase 2)
    emit_ready(has_ui)

    # Set up signal handlers early so SIGTERM works during any phase
    shutdown_event = asyncio.Event()

    def _signal_handler() -> None:
        shutdown_event.set()

    loop = asyncio.get_event_loop()
    for sig in (signal.SIGTERM, signal.SIGINT):
        loop.add_signal_handler(sig, _signal_handler)

    # 5. Wait for the host to connect via WebSocket channel
    # If SIGTERM arrives during this wait, we abort and shut down.
    client: TempsClient | None = None
    try:
        channel_or_shutdown = asyncio.ensure_future(channel_connected)
        shutdown_task = asyncio.create_task(shutdown_event.wait())
        done, _ = await asyncio.wait(
            {channel_or_shutdown, shutdown_task},
            timeout=30.0,
            return_when=asyncio.FIRST_COMPLETED,
        )
        if shutdown_task in done:
            # SIGTERM received during channel wait
            channel_or_shutdown.cancel()
            await _shutdown(plugin, manifest, None, runner, args)
            return
        if channel_or_shutdown in done:
            client = channel_or_shutdown.result()
        else:
            # Timeout — no host connected
            channel_or_shutdown.cancel()
            shutdown_task.cancel()
            print(
                "[temps-plugin] Warning: platform channel not connected, "
                "running without platform data access",
                file=sys.stderr,
            )
    except Exception:
        print(
            "[temps-plugin] Warning: platform channel not connected, "
            "running without platform data access",
            file=sys.stderr,
        )

    # 6. Create PluginContext
    ctx = PluginContext(
        plugin_name=manifest.name,
        data_dir=args.data_dir,
        auth_secret=args.auth_secret,
        client=client,
    )
    context = ctx

    if client:
        client.on_event(lambda event: plugin.on_event(ctx, event))

    # 7. Call plugin.on_start()
    try:
        await plugin.on_start(ctx)
    except Exception as e:
        print(f"[temps-plugin] Error in on_start: {e}", file=sys.stderr)
        raise

    # 8. Mount the plugin handler
    plugin_handler = plugin.handler(ctx)
    plugin_ready = True

    print(
        f"[temps-plugin] {manifest.display_name or manifest.name} "
        f"v{manifest.version} running on {args.socket_path}",
        file=sys.stderr,
    )

    # 9. Graceful shutdown -- wait until signal
    await shutdown_event.wait()

    await _shutdown(plugin, manifest, client, runner, args)


async def _shutdown(
    plugin: TempsPlugin,
    manifest: PluginManifest,
    client: TempsClient | None,
    runner: web.AppRunner,
    args: PluginArgs,
) -> None:
    """Run graceful shutdown sequence."""
    print(f"[temps-plugin] Shutting down {manifest.name}...", file=sys.stderr)
    try:
        await plugin.on_shutdown()
    except Exception as e:
        print(f"[temps-plugin] Error during shutdown: {e}", file=sys.stderr)

    if client:
        await client.close()

    await runner.cleanup()

    # Clean up socket file
    if os.path.exists(args.socket_path):
        try:
            os.unlink(args.socket_path)
        except OSError:
            pass


def _parse_args(argv: list[str]) -> PluginArgs:
    """Parse CLI arguments."""
    socket_path: str | None = None
    auth_secret: str | None = None
    data_dir: str | None = None
    database_url: str | None = None

    i = 0
    while i < len(argv):
        arg = argv[i]
        next_val = argv[i + 1] if i + 1 < len(argv) else None

        if arg == "--socket-path" and next_val:
            socket_path = next_val
            i += 2
        elif arg == "--auth-secret" and next_val:
            auth_secret = next_val
            i += 2
        elif arg == "--data-dir" and next_val:
            data_dir = next_val
            i += 2
        elif arg == "--database-url" and next_val:
            database_url = next_val
            i += 2
        else:
            i += 1

    if not socket_path:
        raise SystemExit("Error: --socket-path is required")
    if not auth_secret:
        raise SystemExit("Error: --auth-secret is required")
    if not data_dir:
        raise SystemExit("Error: --data-dir is required")

    return PluginArgs(
        socket_path=socket_path,
        auth_secret=auth_secret,
        data_dir=data_dir,
        database_url=database_url,
    )
