# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Embedded UI serving.

Mirrors: sdks/node/packages/plugin-sdk/src/ui.ts
"""

from __future__ import annotations

from aiohttp import web

from temps_plugin.types import EmbeddedAssets


def _parse_content_type(value: str) -> tuple[str, str | None]:
    """Split 'text/html; charset=utf-8' into ('text/html', 'utf-8').

    aiohttp's ``web.Response`` requires charset to be passed separately.
    """
    parts = [p.strip() for p in value.split(";")]
    mime = parts[0]
    charset: str | None = None
    for part in parts[1:]:
        if part.lower().startswith("charset="):
            charset = part.split("=", 1)[1].strip()
    return mime, charset


def serve_embedded_ui(path: str, assets: EmbeddedAssets) -> web.Response:
    """Serve an embedded UI asset, with SPA fallback."""

    # Redirect /ui to /ui/
    if path == "/ui":
        raise web.HTTPFound("/ui/")

    if not path.startswith("/ui/"):
        return web.Response(text="Not Found", status=404)

    relative_path = path[len("/ui/") :] or "index.html"

    # Block path traversal
    if ".." in relative_path:
        return web.Response(text="Forbidden", status=403)

    # Exact match
    file = assets.get(relative_path)
    if file:
        cache = (
            "public, max-age=31536000, immutable"
            if file.immutable
            else "no-cache, no-store, must-revalidate"
        )
        content_type, charset = _parse_content_type(file.content_type)
        return web.Response(
            body=file.content,
            content_type=content_type,
            charset=charset,
            headers={
                "Content-Length": str(len(file.content)),
                "Cache-Control": cache,
            },
        )

    # SPA fallback for paths without file extensions
    last_segment = relative_path.rsplit("/", 1)[-1]
    if "." not in last_segment:
        index = assets.get("index.html")
        if index:
            content_type, charset = _parse_content_type(index.content_type)
            return web.Response(
                body=index.content,
                content_type=content_type,
                charset=charset,
                headers={
                    "Content-Length": str(len(index.content)),
                    "Cache-Control": "no-cache, no-store, must-revalidate",
                },
            )

    return web.Response(text="Not Found", status=404)
