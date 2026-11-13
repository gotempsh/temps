# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Loopback-only fixture server for the shared real-engine conformance suite."""
import asyncio
import json
import os
from datetime import datetime
from temps_workflows import Workflow, WorkflowApp

effects = {}
variant = "a"
gate = None


def effect(key):
    effects[key] = effects.get(key, 0) + 1
    return effects[key]


async def replay(ctx):
    a = await ctx.step("a", lambda: effect("conf-replay/a"))
    await ctx.sleep("pause", 1.5)
    b = await ctx.step("b", lambda: effect("conf-replay/b"))
    now = datetime.fromisoformat((await ctx.now()).replace("Z", "+00:00"))
    return {"a": a, "b": b, "now": now.isoformat(timespec="milliseconds").replace("+00:00", "Z"), "random": await ctx.random()}


async def retry(ctx):
    def body():
        attempt = effect("conf-retry/flaky")
        if attempt < 3:
            raise ValueError(f"attempt {attempt} failed")
        return {"attempt": attempt}
    return await ctx.step("flaky", body, retries=3, initial_backoff=1)


async def exhausted(ctx):
    def body():
        effect("conf-retry-exhausted/always-fails")
        raise ValueError("the upstream is down")
    return await ctx.step("always-fails", body, retries=1, initial_backoff=1)


async def suspend(ctx):
    approval = await ctx.wait_for_event("approval", "conf.approved", key=ctx.input["key"], timeout=ctx.input.get("timeoutMs", 3600000)/1000)
    return {"approval": approval, "after": await ctx.step("after", lambda: effect("conf-suspend/after"))}


async def incompatible(ctx):
    name = "one" if variant == "a" else "uno"
    await ctx.step(name, lambda: effect("conf-incompatible/" + name))
    await ctx.sleep("gap", 1.5)
    return "done"


async def gated(ctx):
    async def body():
        entered = effect("conf-gated/entered")
        await gate.wait()
        return {"entered": entered}
    return await ctx.step("gated", body)


async def idempotent(ctx):
    return await ctx.step("once", lambda: effect("conf-idempotent/once"))


async def parent(ctx):
    before = await ctx.step("before", lambda: effect("conf-parent/before"))
    return {"before": before, "child": await ctx.child("square", "conf-child", ctx.input)}


async def child(ctx):
    def body():
        effect("conf-child/square")
        if ctx.input.get("fail"):
            raise ValueError("the child cannot square that")
        return ctx.input["n"] ** 2
    return await ctx.step("square", body, retries=0)


async def main():
    global gate
    gate = asyncio.Event()
    workflows = [Workflow("conf-" + name, fn) for name, fn in [("replay", replay), ("retry", retry), ("retry-exhausted", exhausted), ("suspend", suspend), ("incompatible", incompatible), ("gated", gated), ("idempotent", idempotent), ("parent", parent), ("child", child)]]
    app = WorkflowApp(workflows, os.environ["TEMPS_API_URL"], os.environ["TEMPS_WORKFLOWS_TOKEN"])

    async def connection(reader, writer):
        global gate, variant
        try:
            raw = await asyncio.wait_for(reader.readuntil(b"\r\n\r\n"), 5)
            lines = raw.decode("latin1").split("\r\n")
            method, path, _ = lines[0].split()
            headers = {key.lower(): value for line in lines[1:] if ":" in line for key, value in [line.split(":", 1)]}
            length = int(headers.get("content-length", "0"))
            if length > 64*1024*1024:
                raise ValueError("oversized fixture request")
            body = await asyncio.wait_for(reader.readexactly(length), 5)
            status, response = 200, b"{}"
            if path.startswith("/__fixture/"):
                action = path.removeprefix("/__fixture/")
                if action == "reset":
                    effects.clear(); variant = "a"; gate = asyncio.Event()
                elif action == "gate":
                    gate.set()
                elif action.startswith("variant/"):
                    variant = action.split("/")[1]
                response = json.dumps(effects).encode()
            else:
                messages = []
                async def receive():
                    return {"type": "http.request", "body": body}
                async def send(message):
                    messages.append(message)
                await app({"type": "http", "method": method, "path": path, "headers": [(k.encode(), v.strip().encode()) for k, v in headers.items()]}, receive, send)
                status = messages[0]["status"]
                response = b"".join(m.get("body", b"") for m in messages)
            writer.write(f"HTTP/1.1 {status} Response\r\nContent-Type: application/json\r\nContent-Length: {len(response)}\r\nConnection: close\r\n\r\n".encode() + response)
            await writer.drain()
        finally:
            writer.close()
            await writer.wait_closed()
    server = await asyncio.start_server(connection, "127.0.0.1", int(os.environ["EXECUTOR_PORT"]))
    async with server:
        await server.serve_forever()

asyncio.run(main())
