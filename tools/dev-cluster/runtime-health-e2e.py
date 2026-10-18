# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Disposable Docker-free control plane and two mTLS worker runtime-health E2E.

Build the Linux binary once in the dev-cluster image, then pass that image and
its target volume. All mutable resources have a random per-run Compose prefix;
only the caller's prebuilt target volume is reused read-only. No global prune.
"""

import argparse
import http.cookiejar
import http.client
import json
import os
from pathlib import Path
import subprocess
import signal
import socket
import tempfile
import time
import urllib.error
import urllib.request
import uuid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True)
    parser.add_argument("--target-volume", help="External Linux target volume when the binary is under /workspace/target")
    parser.add_argument("--binary", default="/workspace/target/fast/temps")
    parser.add_argument("--scenario", choices=("all", "cohost"), default="all", help="Run the complete matrix or just co-location after basic enrollment")
    args = parser.parse_args()
    if args.binary.startswith("/workspace/target/") and not args.target_volume:
        parser.error("--target-volume is required for a binary under /workspace/target")
    def interrupted(signum, _frame):
        raise SystemExit(128 + signum)
    signal.signal(signal.SIGTERM, interrupted)
    root = Path(__file__).resolve().parents[2]
    prefix = "temps-runtime-e2e-" + uuid.uuid4().hex[:10]
    octet = 100 + int(uuid.uuid4().hex[:2], 16) % 100
    subnet = f"10.197.{octet}"

    def docker(*command, data=None, capture=True, timeout=120):
        return subprocess.run(["docker", *command], input=data, capture_output=capture,
                              text=True, check=True, timeout=timeout).stdout

    def wait(description, action, predicate=bool, timeout=180):
        deadline = time.monotonic() + timeout
        last = None
        while time.monotonic() < deadline:
            try:
                last = action()
                if predicate(last):
                    print(f"PASS {description}", flush=True)
                    return last
            except (urllib.error.URLError, TimeoutError, ConnectionError, http.client.HTTPException, subprocess.CalledProcessError):
                pass
            time.sleep(2)
        raise AssertionError(f"Timed out: {description}; last bounded result: {str(last)[:256]}")

    # Docker-in-Docker VMs can allocate a port already used on the host by
    # another daemon. Reserve host-free loopback ports before publishing them.
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        cp_port = listener.getsockname()[1]
        with socket.socket() as cohost_listener:
            cohost_listener.bind(("127.0.0.1", 0))
            cohost_port = cohost_listener.getsockname()[1]

    with tempfile.TemporaryDirectory(prefix=prefix) as scratch:
        os.chmod(scratch, 0o700)
        volumes = {name: {} for name in ["postgres", "bootstrap", "cp", "worker1", "worker2", "home1", "home2", "docker1", "docker2"]}
        if args.target_volume:
            volumes["target"] = {"external": True, "name": args.target_volume}
        common = {
            "TEMPS_DATA_DIR": "/var/lib/temps", "TEMPS_LOG_LEVEL": "info",
            "TEMPS_NETWORK_NAME": "temps-app-network", "TEMPS_TELEMETRY": "0",
            "DEV_CLUSTER_STATE_DIR": "/run/temps-bootstrap",
            "DEV_CLUSTER_PREBUILT_TEMPS_BIN": args.binary,
        }
        mounts = [str(root) + ":/workspace:ro", "bootstrap:/run/temps-bootstrap"]
        if args.target_volume:
            mounts.append("target:/workspace/target:ro")
        cp_env = dict(common, TEMPS_DATABASE_URL=f"postgres://temps:temps@{subnet}.5:5432/temps",
                      TEMPS_ADDRESS="0.0.0.0:80", TEMPS_TLS_ADDRESS="0.0.0.0:443",
                      TEMPS_SERVE_PROFILE="control-plane", TEMPS_DISABLE_HTTPS_REDIRECT="true",
                      DEV_CLUSTER_USE_ENROLLMENT_TOKEN="true", DEV_CLUSTER_ENROLLMENT_NODE_NAME="worker-1",
                      DEV_CLUSTER_ENABLE_DNS="true", DEV_CLUSTER_GENERATE_TLS_CERT="true",
                      DEV_CLUSTER_TLS_HOST="control-plane.temps.test", DEV_CLUSTER_WILDCARD_DOMAIN="*.temps.test",
                      DEV_CLUSTER_EXTERNAL_URL="https://control-plane.temps.test")
        services = {
            "postgres": {"image": "timescale/timescaledb-ha:pg18", "environment": {
                "POSTGRES_DB": "temps", "POSTGRES_USER": "temps", "POSTGRES_PASSWORD": "temps"},
                "networks": {"underlay": {"ipv4_address": subnet + ".5"}},
                "volumes": ["postgres:/home/postgres/pgdata"],
                "healthcheck": {"test": ["CMD", "pg_isready", "-U", "temps", "-d", "temps"], "interval": "2s", "timeout": "2s", "retries": 60}},
            # Override the DinD entrypoint: this process has neither dockerd,
            # a socket mount nor a privileged container. It starts empty.
            "control-plane": {"image": args.image, "entrypoint": ["bash"],
                "command": ["-c", "set -e; install -m 0644 /workspace/crates/temps-cli/GeoLite2-City.mmdb /var/lib/temps/GeoLite2-City.mmdb; if [ -x /usr/bin/docker ]; then mv /usr/bin/docker /run/unused-docker-cli; fi; exec bash /workspace/tools/dev-cluster/role-control-plane.sh"],
                "environment": cp_env, "ports": [f"127.0.0.1:{cp_port}:80"],
                "networks": {"underlay": {"ipv4_address": subnet + ".10"}},
                "volumes": mounts + ["cp:/var/lib/temps"],
                "depends_on": {"postgres": {"condition": "service_healthy"}}},
        }
        for number in (1, 2):
            services[f"worker-{number}"] = {
                "image": args.image, "privileged": True, "cgroup": "private",
                "command": ["bash", "/workspace/tools/dev-cluster/role-worker.sh"],
                "environment": dict(common, ROLE="worker", WORKER_NAME=f"worker-{number}",
                    WORKER_UNDERLAY_IP=f"{subnet}.{20 + number}",
                    CONTROL_PLANE_URL="https://control-plane.temps.test",
                    TEMPS_AGENT_ADDRESS="0.0.0.0:3100", TEMPS_OVERLAY_NETWORK="temps0",
                    DEV_CLUSTER_CONTROL_PLANE_CA="/run/temps-bootstrap/control-plane.crt"),
                "ports": [f"127.0.0.1:{cohost_port}:80"] if number == 1 else [],
                "extra_hosts": {"control-plane.temps.test": subnet + ".10"},
                "networks": {"underlay": {"ipv4_address": f"{subnet}.{20 + number}"}},
                "volumes": mounts + [f"worker{number}:/var/lib/temps", f"home{number}:/root", f"docker{number}:/var/lib/docker"],
            }
        compose_file = Path(scratch) / "compose.json"
        compose_file.write_text(json.dumps({"services": services, "volumes": volumes, "networks": {
            "underlay": {"driver": "bridge", "ipam": {"config": [{"subnet": subnet + ".0/24"}]}}}}))
        os.chmod(compose_file, 0o600)

        def compose(*command, **options):
            return docker("compose", "-p", prefix, "-f", str(compose_file), *command, **options)

        def execute(service, *command, **options):
            return compose("exec", "-T", service, *command, **options)

        jar = http.cookiejar.CookieJar()
        client = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))
        base = None

        def api(path, body=None, method=None):
            request = urllib.request.Request(base + "/api" + path,
                data=None if body is None else json.dumps(body).encode(), method=method,
                headers={"Content-Type": "application/json"})
            with client.open(request, timeout=30) as response:
                content = response.read(2 * 1024 * 1024)
                return json.loads(content) if content else None

        def ready():
            with client.open(base + "/readyz", timeout=5) as response:
                return response.status == 200 and response.read(64).strip() == b"ready"

        try:
            compose("up", "-d", "postgres", "control-plane", capture=False)
            address = compose("port", "control-plane", "80").strip()
            base = "http://" + address
            wait("Docker-free control plane starts with no deployments", lambda: ready())
            execute("control-plane", "bash", "-c", "test ! -S /var/run/docker.sock && ! pgrep -x dockerd && ! command -v docker")
            credentials = execute("control-plane", "cat", "/run/temps-bootstrap/admin.txt").splitlines()
            api("/auth/login", {"email": credentials[0], "password": credentials[1]})
            credentials.clear()
            assert int(execute("postgres", "psql", "-U", "temps", "-d", "temps", "-tAc", "SELECT COUNT(*) FROM runtime_health_assignments").strip()) == 0
            # Both nodes consume this fresh, short-lived enrollment token;
            # runtime traffic then uses separate issued node identities.
            execute("postgres", "psql", "-U", "temps", "-d", "temps", "-v", "ON_ERROR_STOP=1", "-c",
                    "UPDATE node_enrollment_tokens SET max_uses=2,bound_node_name=NULL WHERE used_count=0")
            compose("up", "-d", "worker-1", "worker-2", capture=False)
            nodes = wait("two distinct mTLS worker nodes enroll", lambda: [row for row in api("/internal/nodes")["nodes"] if row["id"] > 0],
                         lambda rows: len(rows) == 2 and all(row["status"] == "active" and row["address"].startswith("https://") and row.get("architecture") for row in rows))
            ids = [row["id"] for row in nodes]
            for number in (1, 2):
                worker = f"worker-{number}"
                execute(worker, "docker", "build", "-t", "localhost:5000/runtime-worker:test", "-", data=
                        'FROM busybox:1.37.0\nUSER 65534:65534\nCMD ["sleep", "86400"]\n', timeout=180)
                # Bare HTTP and missing node credentials cannot reach the
                # runtime protocol. This never submits a probe command.
                execute(worker, "bash", "-c",
                        "code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 3 http://127.0.0.1:3100/agent/runtime-health || true); test \"$code\" != 200")
            project = api("/projects", {"name": "runtime-worker-" + uuid.uuid4().hex[:8], "source_type": "docker_image",
                "preset": "dockerfile", "directory": ".", "main_branch": "main", "storage_service_ids": [], "automatic_deploy": False})
            project_id = project["id"]
            environment = api(f"/projects/{project_id}/environments")[0]
            environment_id = environment["id"]
            path = f"/projects/{project_id}/environments/{environment_id}"
            api(path + "/settings", {"replicas": 2, "target_nodes": ids, "anti_affinity": True,
                "exposed_port": None, "health": {"type": "process", "stable_for": 2, "interval": 2, "timeout": 2, "retries": 2}}, "PUT")
            deployment = api(path + "/deploy/image", {"image_ref": "localhost:5000/runtime-worker:test"})
            completed = wait("remote-only portless deployment completes", lambda: api(f"/projects/{project_id}/deployments/{deployment['id']}"),
                             lambda row: row["status"] in ("completed", "failed", "cancelled"))
            assert completed["status"] == "completed", "remote worker deployment failed"
            summary = wait("both remote replicas report fresh healthy process probes", lambda: api(path + "/runtime-health"),
                           lambda row: row["state"] == "healthy" and len(row["replicas"]) == 2 and all(replica["state"] == "healthy" for replica in row["replicas"]))
            assert summary["portless"] is True
            assert summary["coverage"]["availability_percent"] == 100
            execute("control-plane", "bash", "-c", "test ! -S /var/run/docker.sock && ! pgrep -x dockerd && ! command -v docker")
            if args.scenario == "all":
                for number in (1, 2):
                    execute(f"worker-{number}", "docker", "build", "-t", "localhost:5000/runtime-http:test", "-", data=
                            'FROM busybox:1.37.0\nRUN mkdir /www && chmod 777 /www && echo healthy > /www/health\nUSER 65534:65534\nEXPOSE 8080\nCMD ["httpd", "-f", "-p", "8080", "-h", "/www"]\n', timeout=180)
                api(path + "/settings", {"exposed_port": 8080, "health": {
                    "type": "http", "path": "/health", "interval": 2, "timeout": 2, "retries": 2}}, "PUT")
                web = api(path + "/deploy/image", {"image_ref": "localhost:5000/runtime-http:test"})
                completed = wait("remote HTTP deployment completes", lambda: api(f"/projects/{project_id}/deployments/{web['id']}"),
                                 lambda row: row["status"] in ("completed", "failed", "cancelled"))
                assert completed["status"] == "completed"
                wait("remote node-local HTTP probes become healthy", lambda: api(path + "/runtime-health"), lambda row: row["state"] == "healthy")
                candidate = execute("worker-1", "docker", "ps", "-q", "--filter", "label=sh.temps.deploy_id=" + str(web["id"])).strip()
                assert candidate and "\n" not in candidate
                execute("worker-1", "docker", "exec", candidate, "rm", "/www/health")
                wait("remote HTTP failure becomes unhealthy", lambda: api(path + "/runtime-health"), lambda row: row["state"] == "unhealthy")
                wait("remote failure delivers one application alarm", lambda: api(f"/projects/{project_id}/alarms?alarm_type=runtime_probe_failed")["items"],
                     lambda rows: len(rows) == 1 and rows[0]["status"] == "firing")
                assert execute("worker-1", "docker", "inspect", "--format", "{{.State.Running}} {{.RestartCount}}", candidate).strip() == "true 0"
                execute("worker-1", "docker", "exec", candidate, "touch", "/www/health")
                wait("remote HTTP recovery resolves the same alarm", lambda: api(f"/projects/{project_id}/alarms?alarm_type=runtime_probe_failed")["items"],
                     lambda rows: len(rows) == 1 and rows[0]["status"] == "resolved")
                api(path + "/settings", {"health": {"type": "tcp", "interval": 2, "timeout": 2, "retries": 2}}, "PUT")
                tcp = api(path + "/deploy/image", {"image_ref": "localhost:5000/runtime-http:test"})
                completed = wait("remote TCP deployment completes", lambda: api(f"/projects/{project_id}/deployments/{tcp['id']}"),
                                 lambda row: row["status"] in ("completed", "failed", "cancelled"))
                assert completed["status"] == "completed"
                wait("remote node-local TCP probes become healthy", lambda: api(path + "/runtime-health"),
                     lambda row: row["state"] == "healthy" and row["probe"]["kind"] == "tcp")
                compose("stop", "worker-1", capture=False)
                lost = wait("node loss becomes unknown without borrowing the other node's success", lambda: api(path + "/runtime-health"),
                            lambda row: row["state"] == "unknown" and any(replica["state"] == "healthy" for replica in row["replicas"]))
                assert lost["coverage"]["unknown_seconds"] > 0
                wait("monitoring loss raises a separate runtime alarm", lambda: api(f"/projects/{project_id}/alarms?alarm_type=runtime_monitoring_lost")["items"],
                     lambda rows: any(row["status"] == "firing" for row in rows), timeout=180)
                compose("start", "worker-1", capture=False)
                wait("agent restart establishes a fresh session and recovers remote observations", lambda: api(path + "/runtime-health"),
                     lambda row: row["state"] == "healthy", timeout=180)
                wait("monitoring-loss alarm resolves after fresh recovery", lambda: api(f"/projects/{project_id}/alarms?alarm_type=runtime_monitoring_lost")["items"],
                     lambda rows: rows and all(row["status"] == "resolved" for row in rows))
                compose("restart", "control-plane", capture=False)
                wait("control-plane restart accepts only fresh reporting-session observations", lambda: api(path + "/runtime-health"),
                     lambda row: row["state"] == "healthy", timeout=180)
                print("PASS distributed runtime health, history and alarms; no control-plane Docker", flush=True)
            # Move the same control-plane identity onto worker-1 while its
            # enrolled agent keeps running. Both processes now share Docker,
            # but retain separate embedded/agent execution ownership.
            for name in ("auth_secret", "encryption_key"):
                secret = execute("control-plane", "cat", "/var/lib/temps/" + name)
                execute("worker-1", "bash", "-c", "umask 077; cat > /var/lib/temps/" + name, data=secret)
                secret = None
            compose("stop", "control-plane", capture=False)
            for number in (1, 2):
                execute(f"worker-{number}", "bash", "-c",
                        "sed 's/" + subnet + ".10[[:space:]]*control-plane.temps.test/" + subnet + ".21 control-plane.temps.test/' /etc/hosts > /run/runtime-health-hosts; cat /run/runtime-health-hosts > /etc/hosts")
            execute("worker-1", "install", "-m", "0644", "/workspace/crates/temps-cli/GeoLite2-City.mmdb", "/var/lib/temps/GeoLite2-City.mmdb")
            compose("exec", "-d", "worker-1", "bash", "-c",
                    'exec env "$@" /usr/local/bin/temps serve > /proc/1/fd/1 2>/proc/1/fd/2', "cohost",
                    "TEMPS_DATABASE_URL=" + cp_env["TEMPS_DATABASE_URL"],
                    f"TEMPS_ADDRESS={subnet}.21:80", f"TEMPS_TLS_ADDRESS={subnet}.21:443",
                    "TEMPS_SERVE_PROFILE=full", "TEMPS_DISABLE_HTTPS_REDIRECT=true")
            base = "http://" + compose("port", "worker-1", "80").strip()
            wait("co-located control plane starts alongside its enrolled agent", lambda: ready())
            wait("co-located and remote agents reconnect to the moved control plane", lambda: [row for row in api("/internal/nodes")["nodes"] if row["id"] > 0],
                 lambda rows: len(rows) == 2 and all(row["status"] == "active" for row in rows))
            # The embedded deployment path pulls external images itself. The
            # remote-only path used the workers' preloaded caches, so provide
            # an actual fixture registry before exercising local placement.
            execute("worker-1", "docker", "run", "-d", "--name", "runtime-e2e-registry",
                    "-p", "127.0.0.1:5000:5000", "registry:2", timeout=180)
            wait("co-located fixture registry accepts image pulls", lambda: execute("worker-1", "docker", "push", "localhost:5000/runtime-worker:test"))
            project = api("/projects", {"name": "cohost-worker-" + uuid.uuid4().hex[:8], "source_type": "docker_image",
                "preset": "dockerfile", "directory": ".", "main_branch": "main", "storage_service_ids": [], "automatic_deploy": False})
            project_id = project["id"]
            environment_id = api(f"/projects/{project_id}/environments")[0]["id"]
            path = f"/projects/{project_id}/environments/{environment_id}"
            api(path + "/settings", {"replicas": 3, "target_nodes": [0, *ids], "anti_affinity": True,
                "exposed_port": None, "health": {"type": "process", "stable_for": 2, "interval": 2, "timeout": 2, "retries": 2}}, "PUT")
            deployment = api(path + "/deploy/image", {"image_ref": "localhost:5000/runtime-worker:test"})
            completed = wait("co-located local/agent/remote placements deploy", lambda: api(f"/projects/{project_id}/deployments/{deployment['id']}"),
                             lambda row: row["status"] in ("completed", "failed", "cancelled"))
            assert completed["status"] == "completed", "co-located deployment failed: " + json.dumps(completed.get("failure"))[:1600]
            wait("embedded and co-located agent engines both report without duplicate ownership", lambda: api(path + "/runtime-health"),
                 lambda row: row["state"] == "healthy" and len(row["replicas"]) == 3 and all(replica["state"] == "healthy" for replica in row["replicas"]), timeout=180)
            owners = execute("postgres", "psql", "-U", "temps", "-d", "temps", "-tAc",
                "SELECT COUNT(DISTINCT execution_owner::text) FROM runtime_health_assignments a JOIN runtime_health_revisions r ON r.id=a.revision_id WHERE r.project_id=" + str(project_id))
            assert int(owners.strip()) == 3
            print("PASS co-located serve/agent and embedded/remote execution in one cluster", flush=True)
        except BaseException:
            # Only component diagnostics from this run, never enrollment or
            # bootstrap logs that may contain private credentials.
            for service in ("control-plane", "worker-1", "worker-2"):
                try:
                    lines = compose("logs", "--no-color", "--tail", "100", service).splitlines()
                    for line in lines:
                        if "runtime health" in line.lower() or "runtime_health" in line.lower():
                            print(line[-1200:], flush=True)
                except (subprocess.CalledProcessError, subprocess.TimeoutExpired):
                    pass
            raise
        finally:
            # Compose project identity restricts removal to this run. The
            # external prebuilt target volume is never removed by down -v.
            compose("down", "--volumes", "--remove-orphans", capture=False, timeout=180)


if __name__ == "__main__":
    main()
