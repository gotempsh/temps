# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

import asyncio
import unittest
from temps_workflows import Client


class FakeTransport:
    def __init__(self, states):
        self.states = iter(states)
        self.calls = []

    async def call(self, *args):
        self.calls.append(args)
        return {"id": "run-test", "status": next(self.states, "running")}


class ClientTests(unittest.IsolatedAsyncioTestCase):
    async def test_poll_terminal(self):
        for terminal in ("completed", "failed", "cancelled"):
            fake = FakeTransport(["running", terminal])
            client = Client("https://example.invalid/api", "test-token", transport=fake)
            self.assertEqual((await client.wait_for_run("a/b", interval=.001))["status"], terminal)
            self.assertEqual(fake.calls[0][1], "/workflows/v1/runs/a%2Fb")
            self.assertEqual(len(fake.calls), 2)

    async def test_deadline_and_cancellation(self):
        client = Client("https://example.invalid", "test-token", transport=FakeTransport([]))
        with self.assertRaises(TimeoutError):
            await client.wait_for_run("run-test", timeout=.01, interval=10)
        task = asyncio.create_task(client.wait_for_run("run-test"))
        await asyncio.sleep(0)
        task.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await task

    async def test_validation(self):
        client = Client("https://example.invalid", "test-token", transport=FakeTransport([]))
        for value in (0, -1, float('inf'), float('nan')):
            with self.assertRaises(ValueError):
                await client.wait_for_run("run-test", timeout=value)
            with self.assertRaises(ValueError):
                await client.wait_for_run("run-test", interval=value)

    async def test_start_and_event_target(self):
        fake = FakeTransport([])
        client = Client("https://example.invalid", "test-token", transport=fake)
        await client.start("hello", {"x": 1}, target=(1, 2), idempotency_key="request-test")
        await client.send_event("ready", {"ok": True}, target=(1, 2))
        self.assertEqual(fake.calls[0][1], "/workflows/v1/projects/1/environments/2/runs")
        self.assertEqual(fake.calls[0][3]['idempotency_key'], 'request-test')
        self.assertEqual(fake.calls[1][1], "/workflows/v1/projects/1/environments/2/events")
