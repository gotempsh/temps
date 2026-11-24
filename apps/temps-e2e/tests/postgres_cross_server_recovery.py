# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Exercise WAL-G recovery with two independent Temps catalogs on a disposable Docker host.

Run from the repository root with TEMPS_BIN pointing to the binary under test.
"""

import http.cookiejar
import json
import os
import re
import secrets
import shutil
import socket
import subprocess
import tempfile
import time
import unittest
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.request import HTTPCookieProcessor, Request, build_opener


def docker(*args, **kwargs):
    try:
        return subprocess.run(
            ["docker", *args],
            check=True,
            capture_output=True,
            text=True,
            timeout=240,
            **kwargs,
        ).stdout.strip()
    except subprocess.CalledProcessError as error:
        error.add_note(error.stderr.strip())
        raise


def sql(container, statement, database="postgres"):
    return docker(
        "exec",
        container,
        "psql",
        "-XqAt",
        "-v",
        "ON_ERROR_STOP=1",
        "-U",
        "postgres",
        "-d",
        database,
        "-c",
        statement,
    )


def wait_for(label, probe, timeout=180):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = probe()
        if result:
            return result
        time.sleep(1)
    raise TimeoutError(f"Timed out waiting for {label}")


def startup_tail(path, password):
    with path.open("rb") as log:
        start = max(0, log.seek(0, os.SEEK_END) - 8192)
        log.seek(start)
        tail = log.read(8192)
        if start:
            _, _, tail = tail.partition(b"\n")
        text = tail.decode(errors="replace").replace(password, "[redacted]")
    # Omit lines that might contain credentials minted by setup or inherited
    # by the process. Keep ordinary bind, migration and startup errors.
    return "\n".join(
        "[redacted]" if re.search(
            r"secret|token|password|credential|authorization|api.?key|private.?key|://[^/ ]+@|tk_",
            line, re.I,
        ) else line
        for line in text.splitlines()[-30:]
    )


def free_address():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return f"127.0.0.1:{sock.getsockname()[1]}"


class StartupDiagnostics(unittest.TestCase):
    def test_failed_server_tail_is_bounded_and_redacted(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "server.log"
            path.write_text("old line\n" * 1000 + "password=private\n"
                            + "setup value temporary-value\nport already in use\n")
            tail = startup_tail(path, "temporary-value")
            self.assertLessEqual(len(tail.splitlines()), 30)
            self.assertIn("port already in use", tail)
            self.assertNotIn("private", tail)
            self.assertNotIn("temporary-value", tail)


class Server:
    def __init__(self, address):
        self.url = f"http://{address}/api"
        self.client = build_opener(HTTPCookieProcessor(http.cookiejar.CookieJar()))

    def api(self, path, body=None, method=None):
        data = json.dumps(body).encode() if body is not None else None
        request = Request(
            self.url + path,
            data=data,
            method=method,
            headers={"Content-Type": "application/json"},
        )
        try:
            with self.client.open(request, timeout=180) as response:
                payload = response.read()
                return json.loads(payload) if payload else None
        except HTTPError as error:
            problem = json.loads(error.read())
            raise RuntimeError(
                f"{request.get_method()} {path}: "
                f"HTTP {error.code}: {problem.get('detail', problem.get('title'))}"
            ) from error

    def completed(self, path, field):
        def probe():
            row = self.api(path)
            state = row[field]
            if state in ("failed", "cancelled", "rolled_back"):
                raise AssertionError(f"{path}: {state}: {row.get('error_message')}")
            return row if state == "completed" else None

        return wait_for(f"{path} to complete", probe, timeout=420)


class PostgresCrossServerRecovery(unittest.TestCase):
    def container(self, port, image, env, *command):
        args = [arg for key in env for arg in ("-e", key)]
        cid = docker(
            "run",
            "-d",
            # WAL-G reaches MinIO by this DNS name from the database container.
            "--name",
            f"recovery-{self.run_id}-{port}",
            "-p",
            f"{free_address()}:{port}",
            *args,
            image,
            *command,
            env={**os.environ, **env},
        )
        self.addCleanup(docker, "rm", "-fv", cid)
        published = docker("port", cid, f"{port}/tcp").splitlines()[0]
        return cid, published

    def setUp(self):
        try:
            docker("info", "--format", "{{.ServerVersion}}")
        except (FileNotFoundError, subprocess.CalledProcessError):
            self.skipTest("Docker is unavailable")
        self.binary = Path(os.environ["TEMPS_BIN"]).resolve(strict=True)
        self.scratch = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.password = secrets.token_hex(24) + "Aa1"
        self.run_id = secrets.token_hex(6)
        self.catalog, self.db_address = self.container(
            5432, "timescale/timescaledb-ha:pg18", {"POSTGRES_PASSWORD": self.password}
        )
        self.store, address = self.container(
            9000,
            "cgr.dev/chainguard/minio@sha256:f2e56aceb4d38ad9b05c901270946e2353b19f86ba4396f25cf75b15b455b38c",
            {"MINIO_ROOT_USER": "test-access", "MINIO_ROOT_PASSWORD": self.password},
            "server",
            "/tmp/backups",
        )
        self.source = {
            "name": "shared",
            "bucket_name": "backups-bucket",
            "bucket_path": "",
            "endpoint": f"http://{address}",
            "region": "us-east-1",
            "access_key_id": "test-access",
            "secret_key": self.password,
            "force_path_style": True,
        }

    def server(self, label):
        database = f"server_{label}"
        # Wait for the final TCP listener, past the image's initialization server.
        wait_for(
            "catalog PostgreSQL",
            lambda: (
                subprocess.run(
                    ["docker", "exec", self.catalog, "pg_isready", "-h", "127.0.0.1"],
                    capture_output=True,
                    check=False,
                    timeout=10,
                ).returncode
                == 0
            ),
        )
        docker("exec", self.catalog, "createdb", "-U", "postgres", database)
        data_dir = self.scratch / database
        data_dir.mkdir()
        shutil.copy("crates/temps-cli/GeoLite2-City.mmdb", data_dir)
        db_url = f"postgresql://postgres:{self.password}@{self.db_address}/{database}"
        env = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith("TEMPS_")
        }
        env.update(
            TEMPS_DATA_DIR=str(data_dir),
            DATABASE_URL=db_url,
            TEMPS_DATABASE_URL=db_url,
            TEMPS_TELEMETRY="0",
        )
        email = "operator@example.invalid"
        subprocess.run(
            [
                self.binary,
                "setup",
                "--non-interactive",
                "--admin-email",
                email,
                "--wildcard-domain",
                "*.example.invalid",
                "--server-ip",
                "127.0.0.1",
                "--skip-ssl",
                "--skip-dns-records",
                "--skip-geolite2-download",
                "--skip-git",
            ],
            env={**env, "TEMPS_ADMIN_PASSWORD": self.password},
            check=True,
            stdout=subprocess.DEVNULL,
            timeout=180,
        )
        # Keep each control plane's unrelated preview supervisor off the shared daemon.
        gateway = json.dumps(
            {"enabled": False, "container_name": f"preview-{label}-{self.run_id}"}
        )
        self.assertEqual(
            sql(
                self.catalog,
                f"""UPDATE settings
            SET data = jsonb_set(data::jsonb, '{{preview_gateway}}', '{gateway}'::jsonb)
            WHERE id = 1 RETURNING id""",
                database,
            ),
            "1",
        )
        address = free_address()
        log_path = data_dir / "startup.log"
        with log_path.open("wb") as log:
            proc = subprocess.Popen(
                [
                    self.binary,
                    "serve",
                    "--role",
                    "console",
                    "--console-address",
                    address,
                    "--proxy-address",
                    address,
                    "--screenshot-provider",
                    "noop",
                ],
                env=env,
                stdout=log,
                stderr=subprocess.STDOUT,
            )


        def stop():
            proc.terminate()
            try:
                proc.wait(timeout=15)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()

        self.addCleanup(stop)
        server = Server(address)

        def login():
            if proc.poll() is not None:
                raise RuntimeError(
                    f"{database} exited during startup ({proc.returncode})"
                )
            try:
                return server.api(
                    "/auth/login", {"email": email, "password": self.password}
                )["success"]
            except URLError as error:
                if not isinstance(error.reason, ConnectionRefusedError):
                    raise
                return False

        try:
            wait_for(f"{database} login", login)
        except (RuntimeError, TimeoutError) as error:
            error.add_note(startup_tail(log_path, self.password))
            raise
        return server

    def postgres(self, server, label):
        name = f"service-{label}-{self.run_id}"
        service = server.api(
            "/external-services",
            {
                "name": name,
                "service_type": "postgres",
                "parameters": {
                    "username": "postgres",
                    "database": "postgres",
                    "password": self.password,
                },
            },
        )
        self.addCleanup(
            server.api, f"/external-services/{service['id']}", method="DELETE"
        )
        return service["id"], f"postgres-{name}"

    def test_older_orphan_snapshot_restores_original_data(self):
        source = self.server("a")
        destination = self.server("b")
        source_id, origin = self.postgres(source, "a")
        target_id, target = self.postgres(destination, "b")

        def networks(container):
            return json.loads(
                docker(
                    "inspect",
                    "--format",
                    "{{json .NetworkSettings.Networks}}",
                    container,
                )
            ).keys()

        for name in networks(origin) - networks(self.store):
            docker("network", "connect", name, self.store)
        writer = source.api("/backups/s3-sources", self.source)["id"]
        reader = destination.api("/backups/s3-sources", self.source)["id"]
        listing = f"/backups/s3-sources/{reader}/backups"

        def backup():
            row = source.api(
                f"/backups/external-services/{source_id}/run",
                {
                    "s3_source_id": writer,
                    "backup_type": "full",
                    "confirm_restart": True,
                },
            )
            source.completed(f"/backups/{row['backup_id']}", "state")

        def snapshots():
            result = destination.api(listing + "?include_s3_scan=true")
            self.assertEqual(result.get("scan_errors", []), [])
            return {
                row["location"]
                for row in result["backups"]
                if row["format"] == "walg" and row["source"] == "s3_scan"
            }

        sql(
            origin,
            """CREATE EXTENSION IF NOT EXISTS pgcrypto;
            CREATE TABLE recovery_probe AS SELECT id, gen_random_bytes(1024) AS payload
            FROM generate_series(1, 512000) id""",
        )
        fingerprint = """SELECT count(*), sum(octet_length(payload)),
            md5(string_agg(md5(payload), '' ORDER BY id)) FROM recovery_probe"""
        expected = sql(origin, fingerprint)
        self.assertEqual(expected.split("|")[:2], ["512000", "524288000"])
        print("Taking the first WAL-G backup of 500 MiB on server A.", flush=True)
        backup()
        self.assertEqual(destination.api(listing)["backups"], [])
        first = snapshots()
        self.assertEqual(len(first), 1, first)
        selected = first.pop()
        self.assertTrue(selected.endswith("_backup_stop_sentinel.json"), selected)
        tracked = source.api(f"/backups/s3-sources/{writer}/backups")["backups"]
        self.assertIn(selected, [row["location"] for row in tracked])

        print(
            "Adding a marker and taking a second backup in the same repository.",
            flush=True,
        )
        sql(origin, "CREATE TABLE newer_marker AS SELECT 1 AS id")
        backup()
        available = snapshots()
        self.assertIn(selected, available)
        self.assertEqual(len(available), 2, available)
        print("Restoring the first snapshot through server B's API.", flush=True)
        run = destination.api(
            f"/external-services/{target_id}/restore",
            {
                "mode": "in_place",
                "s3_source_id": reader,
                "backup_location": selected,
                "backup_engine": "postgres",
                "confirm_cross_service": True,
            },
        )
        restored = destination.completed(f"/restore-runs/{run['id']}", "status")
        self.assertIsNone(restored["source_backup_id"])
        self.assertEqual(restored["source_backup"]["location"], selected)
        self.assertEqual(restored["source_backup"]["s3_source_id"], reader)
        self.assertEqual(sql(target, fingerprint), expected)
        self.assertEqual(
            sql(target, "SELECT to_regclass('public.newer_marker') IS NULL"), "t"
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
