# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

import base64
import hashlib
import json
import time
import unittest
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat
from .app import WorkflowApp
from .client import ApiError
from .runtime import Workflow, run_invocation, iso


def request(journal=None):
    now = time.time()
    return dict(protocol_version=1, run_id="synthetic-run", workflow="orders", attempt=1, lease=1,
        now=iso(now), deadline_at=iso(now + 600), lease_expires_at=iso(now + 120), input={"id": 1}, journal=journal or [])


class Recorder:
    def __init__(self):
        self.entries, self.outage = [], False

    async def call(self, method, path, token, body=None, retry=False):
        if self.outage:
            raise ApiError(503)
        if "/journal?" in path:
            return {"entries": self.entries, "next_sequence": None}
        self.entries.append(body)
        return {"entry": body}


class RuntimeTests(unittest.IsolatedAsyncioTestCase):
    async def test_removed_wait_cannot_complete_even_on_unread_page(self):
        async def workflow(ctx):
            return "done"
        for paged in (False, True):
            entry = dict(sequence=0, name="approval", kind="wait", status="pending", attempts=1)
            req = request([] if paged else [entry])
            if paged:
                req["journal_next_sequence"] = 0
            status, result = await run_invocation(Workflow("orders", workflow), req, "token", Recorder())
            self.assertEqual(status, 200)
            self.assertEqual(result["outcome"]["type"], "incompatible")
            self.assertEqual(result["outcome"]["sequence"], 0)

    async def test_replay_pagination_and_mismatch(self):
        calls = []
        async def workflow(ctx):
            return await ctx.step("charge", lambda: calls.append(1) or {"ok": True})
        transport = Recorder()
        status, result = await run_invocation(Workflow("orders", workflow), request(), "token", transport)
        self.assertEqual((status, result["outcome"]["output"]), (200, {"ok": True}))
        replay = request()
        replay["journal_next_sequence"] = 0
        await run_invocation(Workflow("orders", workflow), replay, "token", transport)
        self.assertEqual(calls, [1])
        transport.entries[0]["name"] = "old-name"
        _, result = await run_invocation(Workflow("orders", workflow), replay, "token", transport)
        self.assertEqual(result["outcome"]["type"], "incompatible")

    async def test_sleep_and_outage_are_sticky_even_if_user_catches_them(self):
        async def workflow(ctx):
            try:
                await ctx.sleep("nap", 100)
            except BaseException:
                pass
            return "must not complete"
        transport = Recorder()
        _, result = await run_invocation(Workflow("orders", workflow), request(), "token", transport)
        self.assertEqual(result["outcome"]["type"], "sleep")
        transport.outage = True
        status, _ = await run_invocation(Workflow("orders", workflow), request(), "token", transport)
        self.assertEqual(status, 503)

    async def test_retry_then_failure_and_wait_timeout(self):
        async def fails(ctx):
            def fail():
                raise ValueError("synthetic failure")
            return await ctx.step("fail", fail, retries=1)
        transport = Recorder()
        _, result = await run_invocation(Workflow("orders", fails), request(), "token", transport)
        self.assertEqual(result["outcome"]["type"], "retry")
        _, result = await run_invocation(Workflow("orders", fails), request(transport.entries[:]), "token", transport)
        self.assertEqual(result["outcome"]["type"], "failed")
        async def waits(ctx):
            return await ctx.wait_for_event("approval", "approved")
        pending = dict(sequence=0, name="approval", kind="wait", status="pending", attempts=1, wake_at=iso(time.time()-1))
        _, result = await run_invocation(Workflow("orders", waits), request([pending]), "token", Recorder())
        self.assertEqual(result["outcome"]["output"], {"received": False, "payload": None})

    async def test_auth_checks_scope_body_lease_and_expiry_before_user_code(self):
        private = Ed25519PrivateKey.generate()
        b64 = lambda b: base64.urlsafe_b64encode(b).decode().rstrip("=")
        key = dict(algorithm="ed25519", public_key=b64(private.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)), project_id=1, environment_id=2)
        executed = []
        async def workflow(ctx):
            executed.append(True)
            return ctx.input
        app = WorkflowApp([Workflow("orders", workflow)], "http://example.test/api", invocation_key=key, transport=Recorder())
        body = json.dumps(request()).encode()
        claims = dict(v=1, typ="invoke", pid=1, eid=2, run="synthetic-run", lease=1, exp=time.time()+60, body=hashlib.sha256(body).hexdigest())
        async def invoke(overrides):
            payload = b64(json.dumps({**claims, **overrides}).encode())
            message = "twi1." + payload
            token = message + "." + b64(private.sign(message.encode()))
            events, reads = [], []
            async def receive():
                reads.append(True)
                return {"type": "http.request", "body": body}
            async def send(event):
                events.append(event)
            await app(dict(type="http", method="POST", path="/.well-known/temps/workflows/invoke", headers=[(b"authorization", ("Bearer " + token).encode())]), receive, send)
            return events[0]["status"], reads
        for override in [dict(pid=3), dict(eid=4), dict(exp=0), dict(typ="manifest")]:
            status, reads = await invoke(override)
            self.assertEqual(status, 401)
            self.assertEqual(reads, [])
        for override in [dict(body="wrong"), dict(lease=2), dict(run="other")]:
            self.assertEqual((await invoke(override))[0], 401)
        self.assertEqual(executed, [])
        self.assertEqual((await invoke({}))[0], 200)
        self.assertEqual(executed, [True])


if __name__ == "__main__":
    unittest.main()


class TargetTests(unittest.IsolatedAsyncioTestCase):
    async def test_grant_target_does_not_change_reads(self):
        from .client import Client
        paths = []
        class Transport:
            async def call(self, method, path, token, body=None, **kwargs):
                paths.append(path)
                return {}
        client = Client("https://control.example.test/api", "synthetic", transport=Transport())
        await client.start("orders", target=(2, 4))
        await client.send_event("paid", target=(2, 4))
        await client.get_run("r")
        self.assertEqual(paths, ["/workflows/v1/projects/2/environments/4/runs", "/workflows/v1/projects/2/environments/4/events", "/workflows/v1/runs/r"])
        with self.assertRaises(ValueError):
            await client.start("orders", target=(-1, 4))

class PageBudgetTests(unittest.IsolatedAsyncioTestCase):
    async def test_unreached_pages_are_not_loaded_and_consumed_entries_are_discarded(self):
        from .runtime import Context
        transport = Recorder()
        entry = dict(sequence=0, name="first", kind="step", status="completed", attempts=1, result=7)
        req = request([entry])
        req["journal_next_sequence"] = 1
        async def workflow(ctx):
            value = await ctx.step("first", lambda: self.fail("must replay"))
            self.assertEqual(ctx.journal, {})
            return value
        status, result = await run_invocation(Workflow("orders", workflow), req, "token", transport)
        self.assertEqual(status, 200)
        self.assertEqual(result["outcome"]["type"], "incompatible")
        self.assertEqual(result["outcome"]["sequence"], 1)
        self.assertEqual(transport.entries, [])
