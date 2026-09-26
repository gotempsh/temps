#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Exercise a real stateless server against an explicitly disposable test DB/S3.

Pass --binary and --env-json (a private JSON map of the installation env).
Requires a fresh test database and existing test bucket. This starts only its
own children, deletes only its own temporary scratch dirs, and leaves external
DB/S3 intact for inspection. Never pass production credentials.
"""

import argparse
import http.cookiejar
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import urllib.error
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--env-json", type=Path, required=True)
    parser.add_argument("--disposable-test-instance", action="store_true", required=True)
    parser.add_argument("--worker-domain")
    parser.add_argument("--worker-http-port", type=int)
    parser.add_argument("--worker-https-port", type=int)
    parser.add_argument("--worker-ca", type=Path)
    parser.add_argument("--worker-marker")
    parser.add_argument("--worker-node-id", type=int)
    parser.add_argument("--verify-api-path", action="append", default=[],
                        help="Authenticated API read that must be identical after replacement")
    args = parser.parse_args()
    worker_values = [args.worker_domain, args.worker_http_port, args.worker_https_port,
                     args.worker_ca, args.worker_marker, args.worker_node_id]
    if any(worker_values) and not all(worker_values):
        parser.error("worker verification requires domain, HTTP/HTTPS ports, CA, marker and node ID")
    if any(not path.startswith("/") or path.startswith("//") for path in args.verify_api_path):
        parser.error("verification paths must be relative API paths starting with one slash")
    supplied = json.loads(args.env_json.read_text())
    if supplied.get("TEMPS_STATELESS") != "true":
        parser.error("TEMPS_STATELESS=true is required")
    base = supplied["TEMPS_MANAGEMENT_URL"].rstrip("/")
    if not base.startswith("http://127.0.0.1:"):
        parser.error("test must use a loopback management URL")
    run = Path(tempfile.mkdtemp(prefix="temps-stateless-replacement-"))
    env = os.environ.copy()
    env.update(supplied)
    env["TEMPS_DATA_DIR"] = str(run / "scratch-a")
    fixture = run / "object.txt"
    fixture.write_text("durable object survives control-plane scratch loss\n")
    aws_env = env.copy()
    aws_env.update(AWS_ACCESS_KEY_ID=supplied["TEMPS_LOG_S3_ACCESS_KEY_ID"],
                   AWS_SECRET_ACCESS_KEY=supplied["TEMPS_LOG_S3_SECRET_ACCESS_KEY"],
                   AWS_DEFAULT_REGION=supplied.get("TEMPS_LOG_S3_REGION", "us-east-1"), AWS_PAGER="")
    aws = ["aws"]
    if supplied.get("TEMPS_LOG_S3_ENDPOINT"):
        aws.extend(["--endpoint-url", supplied["TEMPS_LOG_S3_ENDPOINT"]])
    object_path = "screenshots/stateless-replacement-e2e.txt"
    object_key = f"instances/{supplied['TEMPS_INSTANCE_ID']}/static-assets/paths/{object_path}"
    subprocess.run(aws + ["s3", "cp", str(fixture), f"s3://{supplied['TEMPS_LOG_S3_BUCKET']}/{object_key}", "--only-show-errors"],
                   env=aws_env, check=True, timeout=60)
    command = [str(args.binary.resolve()), "serve", "--profile", "control-plane", "--role", "console",
               "--console-address", supplied["TEMPS_CONSOLE_ADDRESS"]]
    processes = []
    handles = []

    def start(label, overrides=None):
        child_env = env.copy()
        child_env.update(overrides or {})
        handle = (run / f"{label}.log").open("wb")
        handles.append(handle)
        process = subprocess.Popen(command, env=child_env, stdout=handle, stderr=subprocess.STDOUT)
        processes.append(process)
        return process

    def ready(process):
        until = time.monotonic() + 240
        while time.monotonic() < until:
            if process.poll() is not None:
                raise AssertionError(f"server exited {process.returncode}; inspect private logs in {run}")
            try:
                with urllib.request.urlopen(base + "/readyz", timeout=2) as response:
                    if response.status == 200:
                        return
            except (OSError, urllib.error.HTTPError):
                pass
            time.sleep(0.5)
        raise AssertionError(f"server never became ready; inspect {run}")

    cookies = http.cookiejar.CookieJar()
    client = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(cookies))

    def api(path, body=None):
        request = urllib.request.Request(base + "/api" + path,
            data=None if body is None else json.dumps(body).encode(),
            headers={"Content-Type": "application/json"})
        with client.open(request, timeout=20) as response:
            return json.load(response)

    def verify_worker(stage):
        if not args.worker_domain:
            return
        for protocol, port in [("http", args.worker_http_port), ("https", args.worker_https_port)]:
            command = ["curl", "--noproxy", "*", "--silent", "--show-error", "--max-time", "10",
                       "--resolve", f"{args.worker_domain}:{port}:127.0.0.1",
                       f"{protocol}://{args.worker_domain}:{port}/", "--write-out", "\n%{http_code}"]
            if protocol == "https":
                command.extend(["--cacert", str(args.worker_ca)])
            result = subprocess.run(command, capture_output=True, text=True, timeout=15, check=True)
            body, status = result.stdout.rsplit("\n", 1)
            assert status == "200" and args.worker_marker in body, (stage, protocol, status)
        print(f"PASS: real worker HTTP + verified HTTPS {stage}", flush=True)

    try:
        first = start("first")
        ready(first)
        api("/auth/login", {"email": supplied["TEMPS_ADMIN_EMAIL"],
                            "password": Path(supplied["TEMPS_ADMIN_PASSWORD_FILE"]).read_text().strip()})
        original = api("/projects")
        saved_reads = {}
        for path in args.verify_api_path:
            with client.open(base + "/api" + path, timeout=20) as response:
                saved_reads[path] = response.read()
        verify_worker("before replacement")
        with client.open(base + "/api/files/" + object_path, timeout=20) as response:
            assert response.read() == fixture.read_bytes()
        features = api("/platform/features")
        assert features["stateless"] and not features["deployments_local"]
        assert not features["persistent_workspaces"] and not features["external_plugins"]
        print("PASS: stateless console ready, authenticated, truthful capabilities", flush=True)
        second = start("second-owner", {"TEMPS_DATA_DIR": str(run / "competing")})
        assert second.wait(timeout=30) != 0
        assert "Another control plane owns" in (run / "second-owner.log").read_text()
        print("PASS: overlapping owner rejected", flush=True)
        first.kill()
        first.wait(timeout=10)
        verify_worker("while control plane is stopped")
        shutil.rmtree(run / "scratch-a")
        env["TEMPS_DATA_DIR"] = str(run / "scratch-b")
        replaced_at = time.time()
        replacement = start("replacement")
        ready(replacement)
        assert api("/projects") == original, "original session or database state changed"
        for path, before in saved_reads.items():
            with client.open(base + "/api" + path, timeout=20) as response:
                assert response.read() == before, f"durable API read changed after replacement: {path}"
        if saved_reads:
            print(f"PASS: {len(saved_reads)} durable API reads unchanged after scratch deletion", flush=True)
        verify_worker("after replacement")
        if args.worker_node_id:
            from datetime import datetime
            for _ in range(45):
                node = api(f"/internal/nodes/{args.worker_node_id}")
                heartbeat = node.get("last_heartbeat")
                if (heartbeat and datetime.fromisoformat(heartbeat).timestamp() >= replaced_at
                        and node.get("public_ingress_running")
                        and node.get("public_ingress_certificate_count", 0) > 0):
                    break
                time.sleep(1)
            else:
                raise AssertionError("real worker did not resume heartbeats and certificate reporting")
            print("PASS: real worker resumed heartbeats and TLS certificate reporting", flush=True)
        with client.open(base + "/api/files/" + object_path, timeout=20) as response:
            assert response.read() == fixture.read_bytes()
        assert not (run / "scratch-b" / "auth_secret").exists()
        assert not (run / "scratch-b" / "encryption_key").exists()
        print("PASS: SIGKILL + disk deletion preserved authenticated session and database reads", flush=True)
        print("PASS: S3-backed file remained readable through the replacement API", flush=True)
        replacement.terminate()
        replacement.wait(timeout=40)
        for name, override, expected in [
            ("wrong-key", {"TEMPS_ENCRYPTION_KEY": "22" * 32}, "injected encryption key"),
            ("wrong-auth", {"TEMPS_AUTH_SECRET": "33" * 32}, "injected auth secret"),
            ("wrong-instance", {"TEMPS_INSTANCE_ID": "wrong-replacement-instance"}, "replacement identity"),
        ]:
            process = start(name, override)
            assert process.wait(timeout=60) != 0
            assert expected in (run / f"{name}.log").read_text(), name
            print(f"PASS: {name} fails closed", flush=True)
        print(f"Evidence logs (private): {run}")
    finally:
        for process in processes:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=30)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=10)
        for handle in handles:
            handle.close()


if __name__ == "__main__":
    main()
