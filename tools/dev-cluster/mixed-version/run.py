#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
"""ADR-055 phase 3 mixed-version e2e.

A new control plane, an old worker (an older revision, e.g. main) and a
new worker (this branch), as local privileged DinD containers. On each
worker: a deployment, a managed PostgreSQL and a sandbox (all through
/agent/*), and a job cron run (old worker: the agent-api driver; new
worker: the state stream).

Build the two Linux binaries first (see build.sh), then:

    run.py up && run.py test; run.py down

Prints SKIP and exits 0 when Docker or either prebuilt binary is missing.
Environment: DOCKER_HOST (the daemon to use), MIXED_VERSION_API_PORT
(the loopback port the control plane's API is published on, default
18363). Everything carries sh.temps.test=adr055-p3.
"""
import os
import hashlib
import json
import subprocess
import sys
import time
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
D = ["docker"] + (["--host", os.environ["DOCKER_HOST"]] if os.environ.get("DOCKER_HOST") else [])
COMPOSE = D + ["compose", "-f", f"{HERE}/docker-compose.yml"]
API = f"http://127.0.0.1:{os.environ.get('MIXED_VERSION_API_PORT', '18363')}/api"
RESULTS = []
# Names are unique per run, so `test` can be repeated on one cluster.
RUN = str(int(time.time()))[-6:]
# A secret seeded into every test environment. Job runs import it from the
# deployment they run beside; it must reach the job and never the node's
# snapshot file or either side's logs.
SEED = "p3-seeded-" + os.urandom(12).hex()
SEED_SHA = hashlib.sha256(SEED.encode()).hexdigest()


def sh(args, check=True, capture=True, input=None):
    r = subprocess.run(args, capture_output=capture, text=True, input=input)
    if check and r.returncode != 0:
        raise RuntimeError(f"{' '.join(args)} failed ({r.returncode}): {r.stderr.strip()[-2000:]}")
    return r.stdout.strip() if capture else ""


PG_PORT = 20000 + int(RUN) % 20000


def psql(sql):
    return sh(D + ["exec", "-i", "adr055-p3-postgres", "psql", "-U", "temps", "-d", "temps",
                   "-tA", "-v", "ON_ERROR_STOP=1", "-c", sql])


def record(name, ok, detail=""):
    RESULTS.append((name, ok, detail))
    print(f"{'PASS' if ok else 'FAIL'} {name} {detail}", flush=True)


def wait(what, fn, timeout=600, every=5):
    start = time.time()
    last = None
    while time.time() - start < timeout:
        try:
            last = fn()
            if last:
                return last
        except Exception as e:  # noqa: BLE001 -- a harness poll
            last = e
        time.sleep(every)
    raise TimeoutError(f"timed out after {timeout}s waiting for {what} (last: {last})")


class Client:
    def __init__(self):
        self.headers = {"Content-Type": "application/json"}

    def call(self, method, path, body=None, ok=(200, 201, 202, 204), timeout=120):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(API + path, data=data, method=method, headers=self.headers)
        try:
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                raw = resp.read().decode()
                status, hdrs = resp.status, resp.headers
        except urllib.error.HTTPError as e:
            raw, status, hdrs = e.read().decode(), e.code, e.headers
        if status not in ok:
            raise RuntimeError(f"{method} {path} -> {status}: {raw[:1500]}")
        return (json.loads(raw) if raw else None), hdrs


def up():
    sh(COMPOSE + ["up", "-d", "postgres", "control-plane", "worker-old"], capture=False)
    wait("the control plane", lambda: urllib.request.urlopen(API.replace("/api", "") + "/healthz", timeout=5).status < 500, timeout=900)
    print("control plane is up", flush=True)
    wait("worker-old to register", lambda: psql("select id from nodes where name='worker-old' and status='active'"), timeout=600)
    # A second single-use enrollment token, bound to worker-new.
    token = sh(["python3", "-c", "import secrets;print(secrets.token_hex(32))"])
    digest = sh(["python3", "-c", f"import hashlib;print(hashlib.sha256('{token}'.encode()).hexdigest())"])
    psql("insert into node_enrollment_tokens (token_hash,max_uses,used_count,expires_at,bound_node_name,created_at,updated_at) "
         f"values ('{digest}',1,0,now()+interval '1 hour','worker-new',now(),now())")
    sh(D + ["exec", "-i", "adr055-p3-cp", "sh", "-c",
            "umask 077; cat > /run/state-new/join_token.txt && cp /run/temps-bootstrap/control-plane.crt /run/state-new/ && chmod 644 /run/state-new/control-plane.crt"],
       input=token + "\n")
    sh(COMPOSE + ["up", "-d", "worker-new"], capture=False)
    wait("worker-new to register", lambda: psql("select id from nodes where name='worker-new' and status='active'"), timeout=600)
    print("both workers registered", flush=True)


def login():
    c = Client()
    password = sh(D + ["exec", "adr055-p3-cp", "sed", "-n", "2p", "/run/temps-bootstrap/admin.txt"])
    _, hdrs = c.call("POST", "/auth/login", {"email": "admin@local.dev", "password": password})
    cookies = [h.split(";")[0] for h in hdrs.get_all("Set-Cookie") or []]
    c.headers["Cookie"] = "; ".join(cookies)
    key, _ = c.call("POST", "/api-keys", {"name": f"adr055-p3-e2e-{RUN}", "role_type": "admin", "permissions": None, "expires_at": None})
    c.headers = {"Content-Type": "application/json", "Authorization": f"Bearer {key['api_key']}"}
    return c


def capabilities(node_id):
    return psql(f"select coalesce(capacity::jsonb->'node_capabilities','null'::jsonb) from nodes where id={node_id}")


def exercise(c, worker, node_id, expect_stream):
    tag = worker.replace("worker-", "") + "-" + RUN
    caps = capabilities(node_id)
    record(f"{worker}: capability advertisement", ("state_stream" in caps) == expect_stream, f"node_capabilities={caps}")

    # 1. A deployment pinned to the worker (deploy path: /agent/*).
    project, _ = c.call("POST", "/projects", {"name": f"p3-{tag}", "directory": ".", "main_branch": "main",
                                           "preset": "dockerfile", "storage_service_ids": [],
                                           "source_type": "docker_image"})
    pid = project["id"]
    envs, _ = c.call("GET", f"/projects/{pid}/environments")
    eid = envs[0]["id"]
    c.call("PUT", f"/projects/{pid}/environments/{eid}/settings", {"target_nodes": [node_id], "exposed_port": 8080})
    c.call("POST", f"/projects/{pid}/env-vars", {"key": "P3_SEEDED_SECRET", "value": SEED, "environment_ids": [eid]})
    dep, _ = c.call("POST", f"/projects/{pid}/environments/{eid}/deploy/image", {"image_ref": "nginxinc/nginx-unprivileged:alpine"})
    did = dep.get("id") or dep.get("deployment_id")
    try:
        state = wait(f"deployment {did} on {worker}", lambda: (lambda s: s if s in ("completed", "failed", "cancelled") else None)(
            psql(f"select state from deployments where id={did}")), timeout=900, every=5)
        on_node = psql(f"select count(*) from deployment_containers where deployment_id={did} and node_id={node_id} and deleted_at is null")
        record(f"{worker}: deployment via /agent/*", state == "completed" and on_node not in ("", "0"), f"deployment={did} state={state} containers_on_node={on_node}")
    except Exception as e:  # noqa: BLE001
        record(f"{worker}: deployment via /agent/*", False, str(e))

    # 2. A managed PostgreSQL on the worker. The control plane picks a free
    # port on its own host, which says nothing about the worker's: a service
    # left by an earlier run would hold it there, so each run picks its own.
    try:
        svc, _ = c.call("POST", "/external-services", {"name": f"p3-pg-{tag}", "service_type": "postgres", "parameters": {"database": "app", "username": "app", "port": str(PG_PORT)}, "node_id": node_id})
        sid = svc["id"]
        status = wait(f"postgres {sid} on {worker}", lambda: (lambda s: s if s in ("running", "failed", "error", "stopped") else None)(
            c.call("GET", f"/external-services/{sid}")[0]["service"].get("status")), timeout=600)
        running = sh(D + ["exec", f"adr055-p3-{worker}", "docker", "ps", "--format", "{{.Names}}"]).split()
        on_worker = any(name.endswith(f"p3-pg-{tag}") for name in running)
        record(f"{worker}: database via /agent/services", status == "running" and on_worker,
               f"service={sid} status={status} on_worker={on_worker}")
    except Exception as e:  # noqa: BLE001
        record(f"{worker}: database via /agent/services", False, str(e))

    # 3. A sandbox on the worker. A worker with no sandbox image builds it
    # on the first create, which takes longer than the control plane waits
    # for (the worker then discards that sandbox). So when the first create
    # fails, wait for the image to exist on the worker and create once more.
    try:
        attempts = []
        sb = None
        for attempt in (1, 2):
            try:
                sb, _ = c.call("POST", "/v1/sandboxes", {"node": str(node_id), "name": f"p3-sb-{tag}-{attempt}"}, timeout=300)
                break
            except Exception as e:  # noqa: BLE001
                attempts.append(f"attempt {attempt}: {str(e)[:300]}")
                if attempt == 2:
                    raise RuntimeError("; ".join(attempts)) from e
                wait(f"the sandbox image on {worker}", lambda: sh(
                    D + ["exec", f"adr055-p3-{worker}", "docker", "image", "ls", "-q", "ghcr.io/gotempsh/temps-sandbox-node"]),
                    timeout=1200, every=10)
        sbid = sb["sandbox"]["id"]
        status = wait(f"sandbox {sbid}", lambda: (lambda s: s if s in ("running", "ready", "failed", "error") else None)(
            c.call("GET", f"/v1/sandboxes/{sbid}")[0]["sandbox"].get("status")), timeout=300)
        on_node = psql(f"select node_id from sandboxes where public_id='{sbid}'")
        container = "temps-sandbox-" + sbid.removeprefix("sbx_")
        running = sh(D + ["exec", f"adr055-p3-{worker}", "docker", "ps", "--format", "{{.Names}}"]).split()
        on_worker = container in running
        record(f"{worker}: sandbox via /agent/sandboxes",
               status in ("running", "ready") and on_node == str(node_id) and on_worker,
               f"sandbox={sbid} status={status} node_id={on_node} container_on_worker={on_worker} retries={len(attempts)}")
    except Exception as e:  # noqa: BLE001
        record(f"{worker}: sandbox via /agent/sandboxes", False, str(e))

    # 4. A job cron run, placed by the environment's target_nodes.
    try:
        cid = psql("insert into crons (project_id,environment_id,path,schedule,created_at,updated_at,next_run,mode,command,"
                   "concurrency_policy,retry_limit) values "
                   f"({pid},{eid},'job:p3-hello','*/1 * * * *',now(),now(),now(),'job',"
                   "'[\"sh\",\"-c\",\"test \\\"$(printf %s \\\"$P3_SEEDED_SECRET\\\" | sha256sum | cut -c1-64)\\\" = " + SEED_SHA + " && echo hello-from-a-job\"]'::json,'forbid',0) returning id").splitlines()[0]

        def finished():
            row = psql(f"select w.id, i.node_id, i.driver, i.phase from workloads w join workload_instances i on i.workload_id=w.id "
                       f"where w.spec->'labels'->>'sh.temps.environment'='{eid}' order by i.id desc limit 1")
            if not row:
                return None
            wid, node, driver, phase = row.split("|")
            return (wid, node, driver, phase) if phase in ("succeeded", "failed") else None
        wid, node, driver, phase = wait(f"job run of cron {cid}", finished, timeout=420)
        want = "node-stream" if expect_stream else "agent-api"
        record(f"{worker}: job cron run ({want})", phase == "succeeded" and node == str(node_id) and driver == want,
               f"cron={cid} workload={wid} node={node} driver={driver} phase={phase}")
    except Exception as e:  # noqa: BLE001
        events = psql("select e.reason || ': ' || left(e.message, 300) from workload_events e join workloads w on w.id=e.workload_id "
                      f"where w.spec->'labels'->>'sh.temps.environment'='{eid}' order by e.id")
        record(f"{worker}: job cron run", False, f"{e}; events: {events}")


def test():
    c = login()
    old = int(psql("select id from nodes where name='worker-old'"))
    new = int(psql("select id from nodes where name='worker-new'"))
    exercise(c, "worker-old", old, expect_stream=False)
    exercise(c, "worker-new", new, expect_stream=True)
    status, _ = c.call("GET", "/node-state/status")
    record("stream status endpoint", bool(status.get("configured")), json.dumps(status))
    snap = sh(D + ["exec", "adr055-p3-worker-new", "sh", "-c", "stat -c '%a %n' /root/.temps/node-runtime/state.json 2>&1"], check=False)
    record("worker-new snapshot is 0600", snap.startswith("600"), snap)
    # The job runs above proved the seeded secret reached the job (its
    # command compares a hash of the variable). Nothing on disk or in the
    # logs of the node or the control plane may hold the value itself.
    hits = sh(D + ["exec", "adr055-p3-worker-new", "sh", "-c",
                   f"wc -c < /root/.temps/node-runtime/state.json; grep -c '{SEED}' /root/.temps/node-runtime/state.json || true"],
              check=False).split()
    record("worker-new snapshot holds no seeded secret", len(hits) == 2 and hits[1] == "0",
           f"snapshot_bytes={hits[0] if hits else '?'} seeded_matches={hits[1] if len(hits) > 1 else '?'}")
    for name in ("adr055-p3-cp", "adr055-p3-worker-new", "adr055-p3-worker-old"):
        logs = subprocess.run(D + ["logs", name], capture_output=True, text=True)
        count = (logs.stdout + logs.stderr).count(SEED)
        record(f"{name} logs hold no seeded secret", logs.returncode == 0 and count == 0, f"seeded_matches={count}")
    failed = [r for r in RESULTS if not r[1]]
    print(f"\nSUMMARY: {len(RESULTS) - len(failed)} passed, {len(failed)} failed", flush=True)
    return 1 if failed else 0


def down():
    sh(COMPOSE + ["down", "-v", "--remove-orphans"], check=False, capture=False)


def prerequisites():
    """Why this run cannot happen here, or None."""
    if subprocess.run(D + ["info"], capture_output=True).returncode != 0:
        return "no reachable Docker daemon (set DOCKER_HOST)"
    for volume in ("adr055-p3-old-target", "adr055-p3-new-target"):
        r = subprocess.run(D + ["run", "--rm", "-v", f"{volume}:/t:ro", "--entrypoint", "test",
                                "adr055-p3-dind:latest", "-x", "/t/debug/temps"], capture_output=True)
        if r.returncode != 0:
            return f"no prebuilt binary in volume {volume} (run build.sh first)"
    return None


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else "test"
    if cmd in ("up", "test"):
        reason = prerequisites()
        if reason:
            print(f"SKIP mixed-version e2e: {reason}", flush=True)
            sys.exit(0)
    if cmd == "up":
        up()
    elif cmd == "test":
        sys.exit(test())
    elif cmd == "down":
        down()
