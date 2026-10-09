#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Tests for serve-console-from-source.sh, the console proxy E2E uses with a reused binary.

The proxy decides, per request, whether the browser talks to this checkout's
console bundle or to the `temps` binary. A request sent to the wrong side does
not fail loudly: an API call answered with index.html, or a log stream held in
a buffer until it closes, shows up later as an unrelated, flaky browser test.
These tests run the real script against a stand-in for the binary and pin the
routing, the headers the binary sees, and the streaming behaviour the console
depends on (SSE, WebSocket upgrades, large uploads).

Needs nginx; the script installs it with apt when it is missing, which works on
a GitHub runner. Elsewhere the tests skip when nginx is unavailable.
"""

from __future__ import annotations

import base64
import contextlib
import hashlib
import http.client
import json
import os
import shutil
import signal
import socket
import subprocess
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "serve-console-from-source.sh"
WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC11B5B"
UPLOAD_BYTES = 5 * 1024 * 1024


def stop_nginx(pid_file: Path, timeout: float = 10.0) -> None:
    """Stop the nginx master recorded in `pid_file`, if it is running."""
    try:
        pid = int(pid_file.read_text().strip())
    except (FileNotFoundError, ValueError):
        return
    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return
        time.sleep(0.1)
    with contextlib.suppress(ProcessLookupError):
        os.kill(pid, signal.SIGKILL)


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


class Upstream(BaseHTTPRequestHandler):
    """Stands in for `temps serve`: answers only the routes the binary owns."""

    protocol_version = "HTTP/1.1"

    def log_message(self, *_: object) -> None:
        pass

    def reply(self, body: bytes, content_type: str = "application/json") -> None:
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def echo(self) -> None:
        length = int(self.headers.get("Content-Length") or 0)
        received = 0
        while received < length:
            chunk = self.rfile.read(min(65536, length - received))
            if not chunk:
                break
            received += len(chunk)
        self.reply(
            json.dumps(
                {
                    "upstream": True,
                    "method": self.command,
                    "path": self.path,
                    "host": self.headers.get("Host"),
                    "forwarded": [
                        name for name in self.headers if name.lower().startswith("x-forwarded")
                    ],
                    "received": received,
                }
            ).encode()
        )

    def sse(self) -> None:
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.end_headers()
        self.wfile.write(b"data: first\n\n")
        self.wfile.flush()
        time.sleep(3)
        self.wfile.write(b"data: second\n\n")
        self.wfile.flush()
        self.close_connection = True

    def websocket(self) -> None:
        key = self.headers.get("Sec-WebSocket-Key", "")
        accept = base64.b64encode(hashlib.sha1((key + WS_GUID).encode()).digest()).decode()
        self.send_response(101)
        self.send_header("Upgrade", "websocket")
        self.send_header("Connection", "Upgrade")
        self.send_header("Sec-WebSocket-Accept", accept)
        self.end_headers()
        self.wfile.flush()
        # Echo raw bytes: enough to prove the upgraded connection is a tunnel.
        data = self.connection.recv(1024)
        self.connection.sendall(b"echo:" + data)
        self.close_connection = True

    def do_GET(self) -> None:  # noqa: N802 - http.server API
        if self.path == "/api/sse":
            self.sse()
        elif self.path == "/api/ws":
            self.websocket()
        else:
            self.echo()

    def do_POST(self) -> None:  # noqa: N802 - http.server API
        self.echo()


@unittest.skipUnless(
    shutil.which("nginx") or (shutil.which("apt-get") and os.environ.get("CI")),
    "nginx is not installed (the script installs it on CI runners)",
)
class ConsoleProxyTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        # Cleanups, not tearDownClass: unittest skips tearDownClass when
        # setUpClass raises, but always runs registered class cleanups (in
        # reverse order), so a failed or timed-out start never leaves nginx
        # holding its port or the temporary directory behind.
        cls.tmp = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.tmp.cleanup)
        root = Path(cls.tmp.name)
        cls.dist = root / "dist"
        (cls.dist / "static" / "js").mkdir(parents=True)
        (cls.dist / "index.html").write_text("<!doctype html><title>pr console</title>\n")
        (cls.dist / "static" / "js" / "index.0123abcd.js").write_text("console.log('pr')\n")
        (cls.dist / "favicon.svg").write_text("<svg/>\n")
        cls.work = root / "nginx"

        cls.upstream_port = free_port()
        cls.upstream = ThreadingHTTPServer(("127.0.0.1", cls.upstream_port), Upstream)
        cls.addClassCleanup(cls.upstream.server_close)
        threading.Thread(target=cls.upstream.serve_forever, daemon=True).start()
        cls.addClassCleanup(cls.upstream.shutdown)

        # Registered before the script runs: it may start nginx and then fail
        # a later check, or be killed by the timeout before it can stop it.
        cls.addClassCleanup(stop_nginx, cls.work / "nginx.pid")
        cls.port = free_port()
        result = subprocess.run(
            [str(SCRIPT), str(cls.dist), str(cls.port), str(cls.upstream_port), str(cls.work)],
            capture_output=True,
            text=True,
            timeout=300,
        )
        if result.returncode != 0:
            raise AssertionError(
                f"serve-console-from-source.sh failed ({result.returncode}):\n"
                f"{result.stdout}\n{result.stderr}"
            )

    def get(self, path: str, method: str = "GET", body: bytes | None = None):
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        conn.request(method, path, body=body)
        response = conn.getresponse()
        data = response.read()
        conn.close()
        return response, data

    def test_root_and_client_routes_serve_this_checkouts_index(self) -> None:
        index = (self.dist / "index.html").read_bytes()
        for path in ("/", "/projects/demo/settings", "/deep/link/", "/static/js/missing.js"):
            with self.subTest(path=path):
                response, data = self.get(path)
                self.assertEqual(response.status, 200)
                self.assertEqual(data, index)
                self.assertEqual(
                    response.getheader("Cache-Control"), "no-cache, no-store, must-revalidate"
                )

    def test_hashed_assets_are_served_with_their_type_and_immutable_caching(self) -> None:
        response, data = self.get("/static/js/index.0123abcd.js")
        self.assertEqual(response.status, 200)
        self.assertEqual(data, b"console.log('pr')\n")
        self.assertIn("javascript", response.getheader("Content-Type", ""))
        self.assertEqual(response.getheader("Cache-Control"), "public, max-age=31536000, immutable")

    def test_other_bundle_files_revalidate(self) -> None:
        response, _ = self.get("/favicon.svg")
        self.assertEqual(response.status, 200)
        self.assertEqual(response.getheader("Cache-Control"), "public, max-age=0, must-revalidate")

    def test_routes_the_binary_owns_reach_it(self) -> None:
        for path in ("/api", "/api/projects?page=2", "/mcp", "/mcp/tools", "/healthz", "/readyz"):
            with self.subTest(path=path):
                response, data = self.get(path)
                self.assertEqual(response.status, 200)
                payload = json.loads(data)
                self.assertTrue(payload["upstream"])
                self.assertEqual(payload["path"], path)

    def test_lookalike_paths_stay_with_the_console(self) -> None:
        index = (self.dist / "index.html").read_bytes()
        for path in ("/apis", "/api-docs", "/mcpx", "/healthz/extra"):
            with self.subTest(path=path):
                _, data = self.get(path)
                self.assertEqual(data, index)

    def test_binary_sees_the_browsers_host_and_no_forwarding_headers(self) -> None:
        _, data = self.get("/api/whoami")
        payload = json.loads(data)
        self.assertEqual(payload["host"], f"127.0.0.1:{self.port}")
        self.assertEqual(payload["forwarded"], [])

    def test_large_upload_is_passed_through(self) -> None:
        _, data = self.get("/api/upload", method="POST", body=b"x" * UPLOAD_BYTES)
        payload = json.loads(data)
        self.assertEqual(payload["method"], "POST")
        self.assertEqual(payload["received"], UPLOAD_BYTES)

    def test_server_sent_events_are_not_buffered(self) -> None:
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        conn.request("GET", "/api/sse")
        response = conn.getresponse()
        started = time.monotonic()
        first = response.readline()
        elapsed = time.monotonic() - started
        conn.close()
        self.assertEqual(first, b"data: first\n")
        # The upstream holds the second event for 3s; a buffering proxy would
        # deliver nothing until then.
        self.assertLess(elapsed, 2.0)

    def test_websocket_upgrade_is_tunnelled(self) -> None:
        key = base64.b64encode(os.urandom(16)).decode()
        with socket.create_connection(("127.0.0.1", self.port), timeout=10) as sock:
            sock.sendall(
                (
                    "GET /api/ws HTTP/1.1\r\n"
                    f"Host: 127.0.0.1:{self.port}\r\n"
                    "Upgrade: websocket\r\n"
                    "Connection: Upgrade\r\n"
                    f"Sec-WebSocket-Key: {key}\r\n"
                    "Sec-WebSocket-Version: 13\r\n\r\n"
                ).encode()
            )
            handshake = b""
            while b"\r\n\r\n" not in handshake:
                chunk = sock.recv(1024)
                if not chunk:
                    break
                handshake += chunk
            self.assertTrue(handshake.startswith(b"HTTP/1.1 101"), handshake)
            expected = base64.b64encode(hashlib.sha1((key + WS_GUID).encode()).digest())
            self.assertIn(b"Sec-WebSocket-Accept: " + expected, handshake)
            sock.sendall(b"ping")
            self.assertEqual(sock.recv(1024), b"echo:ping")


@unittest.skipUnless(
    shutil.which("nginx") or (shutil.which("apt-get") and os.environ.get("CI")),
    "nginx is not installed (the script installs it on CI runners)",
)
class FailedStartTests(unittest.TestCase):
    def test_a_failed_check_after_start_stops_nginx(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        dist = Path(tmp.name) / "dist"
        dist.mkdir()
        (dist / "index.html").write_text("<!doctype html>\n")
        work = Path(tmp.name) / "nginx"
        self.addCleanup(stop_nginx, work / "nginx.pid")
        port = free_port()
        # Nothing listens upstream, so nginx starts and serves the bundle but
        # the final /healthz check through it fails.
        result = subprocess.run(
            [str(SCRIPT), str(dist), str(port), str(free_port()), str(work)],
            capture_output=True,
            text=True,
            timeout=300,
        )
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("did not reach the binary", result.stderr)
        pid = int((work / "nginx.pid").read_text().strip()) if (work / "nginx.pid").exists() else None
        deadline = time.monotonic() + 10
        while pid is not None and time.monotonic() < deadline:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                break
            time.sleep(0.1)
        else:
            if pid is not None:
                self.fail(f"nginx (pid {pid}) is still running after the script failed")
        with socket.socket() as sock:
            self.assertNotEqual(sock.connect_ex(("127.0.0.1", port)), 0, "console port still open")


class ArgumentTests(unittest.TestCase):
    def run_script(self, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run([str(SCRIPT), *args], capture_output=True, text=True, timeout=30)

    def test_rejects_a_missing_bundle(self) -> None:
        with tempfile.TemporaryDirectory() as empty:
            result = self.run_script(empty, "18081", "18082", f"{empty}/work")
        self.assertEqual(result.returncode, 1)
        self.assertIn("index.html does not exist", result.stderr)

    def test_rejects_non_numeric_ports(self) -> None:
        with tempfile.TemporaryDirectory() as dist:
            Path(dist, "index.html").write_text("x")
            result = self.run_script(dist, "8081;", "18082")
        self.assertEqual(result.returncode, 2)
        self.assertIn("is not a number", result.stderr)

    def test_rejects_wrong_arity(self) -> None:
        self.assertEqual(self.run_script("only-one").returncode, 2)


if __name__ == "__main__":
    unittest.main()
