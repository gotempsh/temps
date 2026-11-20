# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

import asyncio
import base64
import hashlib
import json
import time
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
from .client import Transport
from .runtime import run_invocation


def unbase64(value):
    return base64.urlsafe_b64decode(value + "=" * (-len(value) % 4))


class WorkflowApp:
    """ASGI endpoint, with project/environment-bound Ed25519 invocation authentication."""
    def __init__(self, workflows, api_url, token=None, *, invocation_key=None, transport=None):
        self.workflows = {workflow.name: workflow for workflow in workflows}
        if len(self.workflows) != len(workflows):
            raise ValueError("Workflow names must be unique")
        self.transport, self.token = transport or Transport(api_url), token
        self.key, self.pinned = invocation_key, invocation_key is not None
        self.refreshed = 0
        self.key_lock = asyncio.Lock()
        if not token and not invocation_key:
            raise ValueError("A workflow credential or pinned invocation key is required")

    async def claims(self, token):
        if len(token) > 8192:
            raise ValueError("Invalid token")
        async with self.key_lock:
            if self.key is None:
                self.key = await self.transport.call("GET", "/workflows/v1/invocation-key", self.token, retry=True)
                self.refreshed = time.monotonic()
            for attempt in range(2):
                try:
                    version, payload, signature = token.split(".")
                    if version != "twi1" or self.key["algorithm"] != "ed25519":
                        raise ValueError("Invalid token")
                    Ed25519PublicKey.from_public_bytes(unbase64(self.key["public_key"])).verify(
                        unbase64(signature), (version + "." + payload).encode("ascii"))
                    break
                except Exception:
                    if attempt or self.pinned or time.monotonic() - self.refreshed < 60:
                        raise ValueError("Invalid token") from None
                    self.refreshed = time.monotonic()
                    self.key = await self.transport.call("GET", "/workflows/v1/invocation-key", self.token, retry=True)
            claims = json.loads(unbase64(payload))
            if claims.get("v") != 1 or claims.get("exp", 0) <= time.time() or claims.get("pid") != self.key["project_id"] or claims.get("eid") != self.key["environment_id"]:
                raise ValueError("Invalid token")
            return claims

    async def __call__(self, scope, receive, send):
        if scope["type"] != "http":
            raise ValueError("Mount WorkflowApp on HTTP routes")
        path = scope["path"]
        invoke = path == "/.well-known/temps/workflows/invoke"
        manifest = path == "/.well-known/temps/workflows/manifest"
        if not invoke and not manifest:
            return await self.respond(send, 404, {"title": "Workflow endpoint not found"})
        if scope["method"] != ("POST" if invoke else "GET"):
            return await self.respond(send, 405, {"title": "Method not allowed"})
        headers = dict(scope["headers"])
        try:
            auth = headers.get(b"authorization", b"").decode().split()
            if len(auth) != 2 or auth[0].lower() != "bearer":
                raise ValueError("Missing token")
            token = auth[1]
            claims = await self.claims(token)
            if claims.get("typ") != ("invoke" if invoke else "manifest"):
                raise ValueError("Wrong token type")
        except ValueError:
            return await self.respond(send, 401, {"title": "Invalid invocation token"})
        except Exception:
            return await self.respond(send, 503, {"title": "Invocation key unavailable"})
        if manifest:
            return await self.respond(send, 200, {"protocol_version": 1, "sdk": "temps-workflows-python/0.1.0",
                "workflows": [{"name": w.name, "version": w.version, "concurrency": w.concurrency} for w in self.workflows.values()]})
        body = bytearray()
        try:
            async with asyncio.timeout(30):
                while True:
                    message = await receive()
                    if message["type"] == "http.disconnect":
                        return
                    body.extend(message.get("body", b""))
                    if len(body) > 64 * 1024 * 1024:
                        return await self.respond(send, 413, {"title": "Invocation body exceeds 64 MiB"})
                    if not message.get("more_body", False):
                        break
        except TimeoutError:
            return await self.respond(send, 408, {"title": "Invocation body timed out"})
        if claims.get("body") != hashlib.sha256(body).hexdigest():
            return await self.respond(send, 401, {"title": "Invocation body does not match token"})
        try:
            request = json.loads(body)
            if claims.get("run") != request["run_id"] or claims.get("lease") != request["lease"]:
                return await self.respond(send, 401, {"title": "Invocation run or lease does not match token"})
            if request["protocol_version"] != 1:
                raise ValueError("Unsupported protocol")
            workflow = self.workflows.get(request["workflow"])
            if workflow is None:
                return await self.respond(send, 200, {"outcome": {"type": "failed", "error": "Workflow is not defined in this deployment"}})
            status, response = await run_invocation(workflow, request, token, self.transport)
        except (KeyError, TypeError, ValueError):
            return await self.respond(send, 400, {"title": "Invalid invocation request"})
        await self.respond(send, status, response)

    @staticmethod
    async def respond(send, status, value):
        await send({"type": "http.response.start", "status": status, "headers": [(b"content-type", b"application/json"), (b"temps-workflows-protocol", b"1")]})
        await send({"type": "http.response.body", "body": json.dumps(value, allow_nan=False).encode()})
