# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""TempsClient — async WebSocket client for platform queries.

Mirrors: sdks/node/packages/plugin-sdk/src/client.ts
"""

from __future__ import annotations

import asyncio
import json
import uuid
from typing import Any, Callable

import aiohttp

from temps_plugin.types import ChannelRequest, ChannelResponse, PluginEvent


class TempsClient:
    """Client for communicating with the Temps host via the WebSocket channel."""

    def __init__(self, ws: aiohttp.web.WebSocketResponse) -> None:
        self._ws = ws
        self._pending: dict[str, asyncio.Future[ChannelResponse]] = {}
        self._event_handlers: list[Callable[[PluginEvent], None]] = []
        self._reader_task: asyncio.Task[None] | None = None

    def start_reader(self) -> None:
        """Start the background message reader task."""
        self._reader_task = asyncio.get_event_loop().create_task(self._read_loop())

    async def _read_loop(self) -> None:
        """Read messages from the WebSocket and dispatch them."""
        try:
            async for msg in self._ws:
                if msg.type == aiohttp.WSMsgType.TEXT:
                    self._dispatch(msg.data)
                elif msg.type in (
                    aiohttp.WSMsgType.CLOSE,
                    aiohttp.WSMsgType.CLOSING,
                    aiohttp.WSMsgType.CLOSED,
                ):
                    break
        except Exception:
            pass

    def _dispatch(self, raw: str) -> None:
        """Dispatch a raw JSON message."""
        try:
            data = json.loads(raw)
        except json.JSONDecodeError:
            return

        # Response to a pending request
        if "id" in data and "id" in data and data["id"] in self._pending:
            resp = ChannelResponse(
                id=data["id"],
                result=data.get("result"),
                error=data.get("error"),
            )
            fut = self._pending.pop(data["id"])
            if not fut.done():
                fut.set_result(resp)
            return

        # Event from the host
        if "event_type" in data:
            event = PluginEvent(
                id=data.get("id", ""),
                event_type=data["event_type"],
                timestamp=data.get("timestamp", ""),
                project_id=data.get("project_id"),
                data=data.get("data", {}),
            )
            for handler in self._event_handlers:
                try:
                    handler(event)
                except Exception:
                    pass

    def on_event(self, handler: Callable[[PluginEvent], None]) -> None:
        """Register an event handler."""
        self._event_handlers.append(handler)

    async def request(self, method: str, params: dict[str, Any] | None = None) -> Any:
        """Send a request to the host and wait for the response."""
        req_id = str(uuid.uuid4())
        req = ChannelRequest(id=req_id, method=method, params=params or {})

        loop = asyncio.get_event_loop()
        fut: asyncio.Future[ChannelResponse] = loop.create_future()
        self._pending[req_id] = fut

        payload = json.dumps(
            {"id": req.id, "method": req.method, "params": req.params},
            separators=(",", ":"),
        )
        await self._ws.send_str(payload)

        try:
            resp = await asyncio.wait_for(fut, timeout=10.0)
        except asyncio.TimeoutError:
            self._pending.pop(req_id, None)
            raise TimeoutError(f"Request {method} timed out")

        if resp.error:
            raise RuntimeError(f"Channel error: {resp.error}")
        return resp.result

    async def list_projects(self) -> list[dict[str, Any]]:
        """Query the host for the list of projects."""
        result = await self.request("list_projects")
        return result if isinstance(result, list) else []

    async def list_deployments(
        self, project_id: int, *, limit: int = 20
    ) -> list[dict[str, Any]]:
        """Query the host for deployments of a project."""
        result = await self.request(
            "list_deployments", {"project_id": project_id, "limit": limit}
        )
        return result if isinstance(result, list) else []

    async def close(self) -> None:
        """Close the WebSocket connection."""
        if self._reader_task:
            self._reader_task.cancel()
        try:
            await self._ws.close()
        except Exception:
            pass
