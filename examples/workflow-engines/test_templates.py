# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Exercise engine persistence and the authenticated public gateway on owned resources."""
import base64
import ipaddress
import json
import os
from pathlib import Path
import secrets
import shutil
import subprocess
import tempfile
import time
import unittest
import urllib.request
import urllib.error
import uuid

ROOT = Path(__file__).parent.resolve()


class EngineTemplates(unittest.TestCase):
    def exercise(self, engine):
        if not shutil.which("docker") or subprocess.run(["docker", "info"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
            print("Docker unavailable; skipping workflow engine template integration")
            return
        project = "temps-engine-test-" + uuid.uuid4().hex[:12]
        env = dict(os.environ, ENGINE_USER="operator", ENGINE_PASSWORD=secrets.token_urlsafe(32), DATABASE_PASSWORD=secrets.token_urlsafe(32))
        networks = subprocess.check_output(["docker", "network", "ls", "-q"], text=True).split()
        used = []
        if networks:
            for network in json.loads(subprocess.check_output(["docker", "network", "inspect", *networks])):
                used.extend(ipaddress.ip_network(config["Subnet"]) for config in (network.get("IPAM", {}).get("Config") or []) if config.get("Subnet"))
        subnet = next(str(candidate) for candidate in ipaddress.ip_network("198.18.0.0/15").subnets(new_prefix=28) if not any(candidate.overlaps(existing) for existing in used if existing.version == 4))
        with tempfile.TemporaryDirectory() as temp:
            override = Path(temp) / "network.json"
            override.write_text(json.dumps({"networks": {"default": {"ipam": {"config": [{"subnet": subnet}]}}}}))
            command = ["docker", "compose", "--project-name", project, "-f", str(ROOT / f"{engine}.compose.yaml"), "-f", str(override)]
            def run(*args, capture=False, timeout=300):
                return subprocess.run([*command, *args], env=env, cwd=ROOT, check=True, timeout=timeout, stdout=subprocess.PIPE if capture else None, text=True)
            try:
                config = json.loads(run("config", "--format", "json", capture=True).stdout)
                for name, service in config["services"].items():
                    self.assertTrue(service.get("mem_limit"), name)
                    if name != "gateway":
                        self.assertFalse(service.get("ports"), f"{name} must stay private")
                run("up", "--build", "-d", timeout=600)
                address = run("port", "gateway", "8080", capture=True).stdout.strip()
                base = "http://" + address
                auth = "Basic " + base64.b64encode((env["ENGINE_USER"] + ":" + env["ENGINE_PASSWORD"]).encode()).decode()
                def request(path, authenticated=False):
                    headers = {"Authorization": auth} if authenticated else {}
                    try:
                        with urllib.request.urlopen(urllib.request.Request(base + path, headers=headers), timeout=5) as response:
                            return response.status
                    except urllib.error.HTTPError as error:
                        status = error.code
                        error.close()
                        return status
                deadline = time.monotonic() + 180
                while time.monotonic() < deadline:
                    try:
                        if request("/healthz") == 200:
                            break
                    except OSError:
                        pass
                    time.sleep(1)
                else:
                    self.fail(f"{engine} did not become healthy")
                self.assertEqual(request("/"), 401)
                self.assertEqual(request("/", True), 200)
                if engine == "restate":
                    ingress = "http://" + run("port", "gateway", "8081", capture=True).stdout.strip()
                    with self.assertRaises(urllib.error.HTTPError) as rejected:
                        urllib.request.urlopen(ingress + "/unregistered/handler", timeout=5)
                    self.assertEqual(rejected.exception.code, 401)
                    rejected.exception.close()
                    run("exec", "-T", "restate", "sh", "-ec", "printf persistence > /restate-data/template-test-marker")
                    run("restart", "restate")
                    self.assertEqual(run("exec", "-T", "restate", "cat", "/restate-data/template-test-marker", capture=True).stdout, "persistence")
                else:
                    # Schema initialization must be safe on the retained database too.
                    run("run", "--rm", "schema", capture=True)
                    run("run", "--rm", "--entrypoint", "temporal", "namespace", "operator", "namespace", "create", "--namespace", "persistence-test")
                    run("restart", "postgres", "temporal")
                    deadline = time.monotonic() + 60
                    while time.monotonic() < deadline:
                        attempt = subprocess.run([*command,"run","--rm","--entrypoint","temporal","namespace","operator","namespace","describe","--namespace","persistence-test"],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=30)
                        if attempt.returncode == 0:
                            break
                        time.sleep(1)
                    else:
                        self.fail("Temporal namespace did not survive restart")
            except Exception:
                logs = subprocess.run([*command, "logs", "--tail", "30", "--no-color"], env=env, capture_output=True, text=True, timeout=30)
                output = logs.stdout + logs.stderr
                for name in ["ENGINE_PASSWORD", "DATABASE_PASSWORD"]:
                    output = output.replace(env[name], "***")
                print(output, flush=True)
                raise
            finally:
                run("down", "--volumes", "--remove-orphans", "--rmi", "local")

    def test_restate(self):
        self.exercise("restate")

    def test_temporal(self):
        self.exercise("temporal")


if __name__ == "__main__":
    unittest.main()
