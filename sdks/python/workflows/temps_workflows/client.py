# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

import asyncio
import json
import math
import urllib.error
import urllib.request
from urllib.parse import quote


class ApiError(Exception):
    def __init__(self, status: int, title: str = "Workflow API request failed"):
        super().__init__(f"{title} (HTTP {status})")
        self.status, self.title = status, title


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class Transport:
    """Bounded HTTP calls. Only executor callbacks are retried automatically."""
    def __init__(self, api_url: str, timeout: float = 30):
        if not math.isfinite(timeout) or timeout <= 0:
            raise ValueError("Workflow HTTP timeout must be positive")
        self.api_url, self.timeout = api_url.rstrip("/"), timeout

    async def call(self, method: str, path: str, token: str, body=None, retry=False):
        attempts = 4 if retry else 1
        for attempt in range(attempts):
            try:
                return await asyncio.to_thread(self._call, method, path, token, body)
            except ApiError as error:
                if error.status not in (0, 502, 503, 504) or attempt == attempts - 1:
                    raise
                await asyncio.sleep(0.25 * 2 ** attempt)

    def _call(self, method, path, token, body):
        encoded = None if body is None else json.dumps(body, allow_nan=False).encode()
        request = urllib.request.Request(self.api_url + path, data=encoded, method=method,
            headers={"authorization": "Bearer " + token, "content-type": "application/json", "temps-workflows-protocol": "1"})
        try:
            with urllib.request.build_opener(NoRedirect).open(request, timeout=self.timeout) as response:
                data = response.read(128 * 1024 * 1024 + 1)
                if len(data) > 128 * 1024 * 1024:
                    raise ApiError(502, "Workflow API response exceeds 128 MiB")
                return json.loads(data)
        except urllib.error.HTTPError as error:
            try:
                title = json.loads(error.read(65536)).get("title", "Workflow API request failed")
            except (ValueError, OSError):
                title = "Workflow API request failed"
            raise ApiError(error.code, title) from None
        except (OSError, ValueError):
            raise ApiError(0, "Workflow API unavailable") from None


class Client:
    def __init__(self, api_url: str, token: str, *, transport=None):
        if not token:
            raise ValueError("A workflow credential is required")
        self.transport, self.token = transport or Transport(api_url), token

    async def start(self, workflow: str, input=None, *, idempotency_key=None, target=None):
        return await self.transport.call("POST", target_path(target, "runs"), self.token,
            {"workflow": workflow, "input": input, "idempotency_key": idempotency_key})

    async def get_run(self, run_id: str):
        return await self.transport.call("GET", "/workflows/v1/runs/" + quote(run_id, safe=""), self.token)

    async def wait_for_run(self, run_id: str, *, timeout: float = 60, interval: float = 1):
        """Read until terminal, with a deadline covering requests and polling.

        Raises TimeoutError on expiry; cancelling the coroutine stops polling.
        """
        if any(not math.isfinite(value) or value <= 0 for value in (timeout, interval)):
            raise ValueError("Workflow polling timeout and interval must be finite and positive")
        async with asyncio.timeout(timeout):
            while True:
                run = await self.get_run(run_id)
                if run["status"] in ("completed", "failed", "cancelled"):
                    return run
                await asyncio.sleep(interval)

    async def send_event(self, name: str, payload=None, *, key=None, target=None):
        return await self.transport.call("POST", target_path(target, "events"), self.token,
            {"name": name, "key": key, "payload": payload})


def target_path(target, operation):
    """Optional (project_id, environment_id) destination, requiring an exact grant."""
    if target is None:
        return f"/workflows/v1/{operation}"
    project, environment = target
    if any(type(value) is not int or value <= 0 for value in target):
        raise ValueError("Workflow target IDs must be positive integers")
    return f"/workflows/v1/projects/{project}/environments/{environment}/{operation}"
