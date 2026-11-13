# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

import asyncio
import inspect
import secrets
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Awaitable, Callable
from urllib.parse import quote
from .client import ApiError


def timestamp(value):
    return datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp()


def iso(value):
    return datetime.fromtimestamp(value, timezone.utc).isoformat().replace("+00:00", "Z")


def name(value, limit=200):
    if not isinstance(value, str) or not value.strip() or len(value) > limit:
        raise ValueError(f"Workflow name must contain 1–{limit} characters")


@dataclass(frozen=True)
class Workflow:
    name: str
    run: Callable[["Context"], Awaitable]
    version: str | None = None
    concurrency: int | None = None

    def __post_init__(self):
        name(self.name, 128)
        if self.concurrency is not None and not 1 <= self.concurrency <= 1_000_000:
            raise ValueError("Workflow concurrency must be in 1..1000000")


class Control(BaseException):
    def __init__(self, outcome=None, status=200):
        self.outcome, self.status = outcome, status


class StepFailed(Exception):
    def __init__(self, sequence, step_name, message):
        self.sequence = sequence
        super().__init__(f"Workflow step '{step_name}' failed: {message}")


class Context:
    def __init__(self, request, token, transport):
        self.input = request.get("input")
        self.run_id, self.workflow = request["run_id"], request["workflow"]
        self.invocation = request["attempt"]
        self.request, self.token, self.transport = request, token, transport
        self.journal = {entry["sequence"]: entry for entry in request["journal"]}
        self.sequence, self.control = 0, None
        self.cursor = request.get("journal_next_sequence")
        self.started, self.server_now = time.monotonic(), timestamp(request["now"])
        self.deadline = timestamp(request["deadline_at"])
        self.path = "/workflow-runs/" + quote(self.run_id, safe="")

    def clock(self):
        return self.server_now + time.monotonic() - self.started

    def stop(self, outcome=None, status=200):
        if self.control is None:
            self.control = Control(outcome, status)
        raise self.control

    async def call(self, method, suffix, body=None):
        try:
            return await self.transport.call(method, self.path + suffix, self.token, body, retry=True)
        except ApiError as error:
            if error.status == 409:
                self.stop(status=202)
            if error.status == 0 or error.status >= 500:
                self.stop(status=503)
            raise

    async def load(self):
        # Initial page is already present; later pages are fetched on demand.
        return

    async def page_for(self, sequence):
        while self.cursor is not None and sequence >= self.cursor:
            cursor = self.cursor
            page = await self.call("GET", f"/journal?from={cursor}")
            following = page.get("next_sequence")
            if following is not None and (following <= cursor or following > 100000):
                self.stop(status=503)
            self.journal = {entry["sequence"]: entry for entry in page["entries"]}
            self.cursor = following

    async def next(self, step_name, kind):
        name(step_name)
        if self.control:
            raise self.control
        sequence = self.sequence
        self.sequence += 1
        await self.page_for(sequence)
        entry = self.journal.pop(sequence, None)
        if entry and (entry["name"] != step_name or entry["kind"] != kind):
            self.stop({"type": "incompatible", "sequence": sequence,
                "expected": f"{entry['kind']} '{entry['name']}'", "found": f"{kind} '{step_name}'"})
        return sequence, entry

    def yield_if_late(self, sequence):
        margin = min(30, max(0, (self.deadline - self.server_now) / 4))
        if self.clock() >= self.deadline - margin:
            self.stop({"type": "retry", "sequence": sequence, "at": iso(self.clock())})

    async def record(self, sequence, step_name, kind, status="completed", attempts=1, **fields):
        response = await self.call("POST", "/steps", dict(sequence=sequence, name=step_name,
            kind=kind, status=status, attempts=attempts, **fields))
        entry = response["entry"]
        return entry

    async def step(self, step_name, fn, *, retries=3, timeout=None, initial_backoff=1, max_backoff=300):
        sequence, entry = await self.next(step_name, "step")
        if entry and entry["status"] != "pending":
            if entry["status"] == "failed":
                raise StepFailed(sequence, step_name, entry.get("error", "Step failed"))
            return entry.get("result")
        self.yield_if_late(sequence)
        attempt = (entry["attempts"] if entry else 0) + 1
        started = time.monotonic()
        try:
            async def execute():
                value = fn()
                return await value if inspect.isawaitable(value) else value
            result = await asyncio.wait_for(execute(), timeout) if timeout is not None else await execute()
        except Exception as error:
            message = str(error)[:16000]
            final = attempt > retries
            saved = await self.record(sequence, step_name, "step", "failed" if final else "pending", attempt,
                error=message, duration_ms=round((time.monotonic() - started) * 1000))
            if saved["status"] == "completed":
                return saved.get("result")
            if saved["status"] == "failed":
                raise StepFailed(sequence, step_name, saved.get("error") or message) from None
            self.stop({"type": "retry", "sequence": sequence,
                "at": iso(self.clock() + min(max_backoff, initial_backoff * 2 ** min(attempt - 1, 30)))})
        saved = await self.record(sequence, step_name, "step", attempts=attempt, result=result,
            duration_ms=round((time.monotonic() - started) * 1000))
        if saved["status"] == "failed":
            raise StepFailed(sequence, step_name, saved.get("error", "Step failed"))
        return saved.get("result")

    async def sleep(self, step_name, seconds):
        await self.sleep_until(step_name, iso(self.clock() + seconds))

    async def sleep_until(self, step_name, until):
        sequence, entry = await self.next(step_name, "sleep")
        if entry and entry["status"] == "completed":
            return
        wake = entry.get("wake_at") if entry else until
        if self.clock() >= timestamp(wake):
            await self.record(sequence, step_name, "sleep")
        else:
            if not entry:
                await self.record(sequence, step_name, "sleep", "pending", wake_at=wake)
            self.stop({"type": "sleep", "sequence": sequence, "until": wake})

    async def wait_for_event(self, step_name, event, *, key=None, timeout=None):
        name(event)
        if event.startswith("temps."):
            raise ValueError("temps. events are reserved")
        sequence, entry = await self.next(step_name, "wait")
        if entry and entry["status"] == "completed":
            return entry.get("result")
        until = entry.get("wake_at") if entry else (iso(self.clock() + timeout) if timeout is not None else None)
        if not entry:
            await self.record(sequence, step_name, "wait", "pending", wake_at=until)
        elif until and self.clock() >= timestamp(until):
            saved = await self.record(sequence, step_name, "wait", result={"received": False, "payload": None})
            return saved.get("result")
        self.stop({"type": "wait", "sequence": sequence, "event": event, "key": key, "timeout_at": until})

    async def send_event(self, step_name, event, payload=None, *, key=None):
        name(event)
        if event.startswith("temps."):
            raise ValueError("temps. events are reserved")
        sequence, entry = await self.next(step_name, "send_event")
        if not entry:
            await self.record(sequence, step_name, "send_event", event={"name": event, "key": key, "payload": payload})

    async def child(self, step_name, workflow, input=None):
        name(workflow, 128)
        sequence, entry = await self.next(step_name, "child")
        if not entry:
            entry = await self.record(sequence, step_name, "child", "pending", child={"workflow": workflow, "input": input})
        if entry["status"] == "completed":
            return entry.get("result")
        self.stop({"type": "wait", "sequence": sequence, "event": "temps.child", "key": entry["result"]["run_id"], "timeout_at": None})

    async def now(self, step_name="now"):
        sequence, entry = await self.next(step_name, "now")
        if not entry:
            entry = await self.record(sequence, step_name, "now", result=iso(self.clock()))
        return entry["result"]

    async def random(self, step_name="random"):
        sequence, entry = await self.next(step_name, "random")
        if not entry:
            entry = await self.record(sequence, step_name, "random", result=secrets.randbits(53) / 2 ** 53)
        return entry["result"]


async def run_invocation(workflow, request, token, transport):
    context = Context(request, token, transport)

    async def heartbeat():
        interval = max(1, (timestamp(request["lease_expires_at"]) - context.server_now) / 3)
        while True:
            await asyncio.sleep(interval)
            try:
                reply = await context.call("POST", "/heartbeat", {})
                if reply["cancelled"]:
                    context.stop(status=202)
            except Control:
                return
            except Exception:
                continue

    pulse = asyncio.create_task(heartbeat())
    try:
        await context.load()
        async with asyncio.timeout(max(0.001, context.deadline - context.clock())):
            output = await workflow.run(context)
        if context.control:
            raise context.control
        return 200, {"outcome": {"type": "completed", "output": output}}
    except Control as signal:
        return signal.status, {"outcome": signal.outcome} if signal.outcome else {}
    except TimeoutError:
        return 503, {}
    except StepFailed as error:
        if context.control:
            signal = context.control
            return signal.status, {"outcome": signal.outcome} if signal.outcome else {}
        return 200, {"outcome": {"type": "failed", "sequence": error.sequence, "error": str(error)[:16000]}}
    except Exception as error:
        if context.control:
            signal = context.control
            return signal.status, {"outcome": signal.outcome} if signal.outcome else {}
        return 200, {"outcome": {"type": "failed", "error": str(error)[:16000]}}
    finally:
        pulse.cancel()
        await asyncio.gather(pulse, return_exceptions=True)
