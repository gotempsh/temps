---
name: start-temps-cluster
description: >
  Start (or restart) a local multi-node Temps cluster using Docker-in-Docker —
  one control plane + 3 worker nodes, each a privileged DinD container running
  its own dockerd + `temps agent`, wired with the real multi-host overlay
  (VXLAN, compute_cidr allocation) via `tools/dev-cluster/` in whichever
  checkout/worktree you run it from. Invoke when the user says "start the temps
  cluster", "spin up a multi-node dev cluster", "test this with multiple
  workers", "bring up worker nodes locally", "docker in dind cluster", or wants
  to verify cross-node behavior (node targeting, cluster DNS, overlay
  networking, node join/mTLS) that a single-node `start-temps` server cannot
  exercise. Distinct from `start-temps` (single-node, native binary, port-slot
  based) and `start-temps-ee` (single-node EE binary) — this is the only path
  that actually has more than one node.
---

# Start a local multi-node Temps cluster (Docker-in-Docker)

Wraps `tools/dev-cluster/` — a docker-compose harness that runs a real Temps
control plane plus 3 worker nodes, each a privileged DinD container, with the
actual overlay network (VXLAN, `compute_cidr` allocation, cluster DNS) wired
up exactly as it would be on real hardware. Use this whenever a fix or feature
needs *more than one node* to prove anything — single-node `start-temps`
cannot exercise cross-node scheduling, the overlay, or cluster DNS at all.

## Topology

```
┌──── temps-underlay (10.42.0.0/24, docker bridge, the "VPC") ────┐
│                                                                  │
│  postgres            10.42.0.5   (TimescaleDB pg18, internal)   │
│  control-plane        10.42.0.10  DinD + `temps serve`           │
│    host ports 80 → 80, 443 → 443 (NOT 8080 — see Gotcha below)  │
│                                                                  │
│  worker-1  10.42.0.21   worker-2  10.42.0.22   worker-3 10.42.0.23│
│    each: privileged DinD + `temps agent`, own dockerd            │
└──────────────────────────────────────────────────────────────────┘

Allocator carves /24s from 172.20.0.0/16, one per worker:
  worker-1 → 172.20.0.0/24
  worker-2 → 172.20.1.0/24
  worker-3 → 172.20.2.0/24

Cross-node container traffic flows over a VXLAN tunnel pinned to the
underlay IPs above. Within-node traffic uses the unchanged
`temps-app-network` Docker bridge (same as single-node temps).
```

The temps source tree is bind-mounted read-write at `/workspace` inside every
DinD container, and `cargo build --bin temps` runs *inside Linux* — no
cross-compiling from macOS. **The cluster builds from whichever checkout you
run `./dev-cluster` in.** `cd` into the right worktree first if you're testing
a fix that isn't on the checkout's current branch.

## Prerequisites

- Docker Desktop (or any Linux Docker host) running — `docker version` must
  succeed.
- ~3 GB free RAM, ~5 GB free disk (compiled binary cache + 4 worker volumes +
  TimescaleDB volume).
- `GeoLite2-City.mmdb` at the **repo root of the checkout you're running
  from** (`<checkout>/GeoLite2-City.mmdb`, gitignored) — the proxy plugin
  refuses to start without it. Download it from
  [MaxMind's GeoLite2 program](https://dev.maxmind.com/geoip/geolite2-free-geolocation-data)
  (free registration required) and place it at the repo root. If you keep
  multiple worktrees of this repo, copying the file between them is faster
  than downloading it again.

## Only one cluster at a time, across the ENTIRE machine

Unlike `start-temps` (which allocates a port "slot" per checkout so many
worktrees run side by side), this harness uses **fixed container names**
(`temps-dev-control-plane`, `temps-dev-worker-1/2/3`, `temps-dev-postgres`)
and fixed host ports (80/443). There is no per-checkout isolation — starting a
second dev-cluster from a different worktree while one is already running
will collide.

**Before running `up`, always check first:**
```bash
docker ps -a --filter "name=temps-dev-" --format "{{.Names}}\t{{.Status}}"
```
If containers are already there and belong to someone else's in-progress
work, don't `down`/`reset` them without asking — treat it like any other
shared, hard-to-reverse state.

There's also a separate mTLS-hardening variant (`docker-compose.harden.yml`,
driven by `e2e-harden.sh`, container prefix `temps-harden-*`, 2 workers only)
used for testing `require_mtls` / node-identity hardening specifically. It's a
different compose project name/port set from the base cluster, but still only
one can run at a time on its own.

## Quick start

```bash
cd <checkout>/tools/dev-cluster
./dev-cluster up          # first run: ~5-10 min (compiles the workspace inside Linux)
                           # subsequent runs: ~10-30s (cargo build no-ops)
```

`up` blocks until: control plane's TCP listener is up, all 3 workers have
registered as `nodes` rows, and every worker has an allocated `compute_cidr`.
It prints admin credentials and the join-token path on success.

```bash
./dev-cluster status              # node + overlay state across the cluster
./dev-cluster logs control-plane   # or worker-1/worker-2/worker-3/postgres
./dev-cluster shell worker-1       # shell into a worker's DinD host
./dev-cluster restart control-plane  # bounce after a binary rebuild
./dev-cluster down                # stop, KEEP volumes (fast restart)
./dev-cluster reset               # stop + DELETE all volumes (fresh slate, asks to confirm)
```

## Gotcha: the script says port 8080, the cluster actually serves 80/443

`docker-compose.yml` publishes `80:80` and `443:443` (production-shaped —
`TEMPS_ADDRESS=0.0.0.0:80`), matching what `e2e-harden.sh` and every API
recipe below assume (`http://localhost/...`). But `dev-cluster`'s own `up`
command waits on `/dev/tcp/127.0.0.1/8080` and prints `web UI:
http://localhost:8080` — that's stale/drifted from the compose file and will
never resolve. **Use port 80, not 8080**, for every API/browser interaction.
If `./dev-cluster up` looks stuck on "waiting for control plane", check
whether it's actually already up on the right port before assuming a real
hang:
```bash
curl -s -o /dev/null -w '%{http_code}\n' http://localhost/api/health
```
If that returns a real HTTP status, the cluster is up — the wait loop itself
is just polling the wrong port and will eventually time out at 20 minutes
regardless. Don't wait for it; move on once curl confirms readiness.

Login: `admin@local.dev` / password on line 2 of `.state/admin.txt`.

```bash
J=/tmp/tc_cookies.txt
PW=$(sed -n '2p' tools/dev-cluster/.state/admin.txt)
curl -s -c $J -X POST http://localhost/api/auth/login -H 'Content-Type: application/json' \
  -d "{\"email\":\"admin@local.dev\",\"password\":\"$PW\"}" -o /dev/null -w "login -> %{http_code}\n"
```

## Verifying the overlay actually works (bare check, no API)

The README documents this network as `temps-overlay` — that name is wrong.
Verified live: the actual Docker network on every worker is named **`temps0`**
(`docker network ls` inside a worker shows it). Use `temps0`, not
`temps-overlay`, or `docker run` fails with "network temps-overlay not
found".

```bash
./dev-cluster shell worker-1
docker run -d --rm --name target --network temps0 --ip 172.20.1.50 nginx:alpine
exit

./dev-cluster shell worker-2
docker run --rm --network temps0 --ip 172.20.0.50 alpine sh -c \
  'apk add -q curl && curl -sf --max-time 5 http://172.20.1.50/ | head -c150'
# "<!DOCTYPE html>..." => bridge + VXLAN + FDB + routes + dual-attach all confirmed working
```

(IPs above match worker-1 → `172.20.1.0/24`, worker-2 → `172.20.0.0/24` — the
allocator assigns CIDRs in registration order, not worker-number order, so
confirm actual assignments with `./dev-cluster status` rather than assuming
worker-N always gets the Nth-numbered /24.)

## Recipe: deploying across specific nodes (multi-replica / anti-affinity)

```bash
NIDS=$(curl -s -b $J http://localhost/api/internal/nodes | \
  python3 -c "import sys,json;print(','.join(str(n['id']) for n in json.load(sys.stdin)['nodes']))")

PID=$(curl -s -b $J -X POST http://localhost/api/projects -H 'Content-Type: application/json' \
  -d '{"name":"cluster-test","directory":".","main_branch":"main","preset":"dockerfile","storage_service_ids":[],"source_type":"docker_image"}' \
  | python3 -c "import sys,json;print(json.load(sys.stdin).get('id',''))")
EID=$(curl -s -b $J http://localhost/api/projects/$PID/environments | \
  python3 -c "import sys,json;print(json.load(sys.stdin)[0]['id'])")

curl -s -b $J -X PUT http://localhost/api/projects/$PID/environments/$EID/settings -H 'Content-Type: application/json' \
  -d "{\"replicas\":2,\"target_nodes\":[$NIDS],\"anti_affinity\":true,\"exposed_port\":8080}"

curl -s -b $J -X POST http://localhost/api/projects/$PID/environments/$EID/deploy/image -H 'Content-Type: application/json' \
  -d '{"image_ref":"nginxinc/nginx-unprivileged:alpine","health_check_path":"/"}'
```

**Use `nginxinc/nginx-unprivileged:alpine`, not plain `nginx:alpine`.** Every
deployed container runs `cap_drop: ALL` + `no-new-privileges`; stock nginx
needs to `chown`/bind a privileged port and crashes under that hardening.
`preset` must be `"dockerfile"` (not `"docker"` — invalid) even for
`source_type: "docker_image"`. Pre-pull the image on every node first
(`docker exec temps-dev-worker-N docker pull <image>`) if `ensure_image_on_remote`
isn't wired for your image, or the deploy will fail on nodes without it cached.

## Recipe: pinning a database service to one node, an app to another

```bash
SID=$(curl -s -b $J -X POST http://localhost/api/external-services -H 'Content-Type: application/json' \
  -d '{"name":"test-db","service_type":"postgres","node_id":<worker-1-node-id>,"topology":"standalone","parameters":{"database":"app","username":"app_user","password":"<pick-one>"},"members":[]}' \
  | python3 -c "import sys,json;print(json.load(sys.stdin).get('id',''))")
```
Then create the app project with `"storage_service_ids":[$SID]` (this is the
link mechanism — set at project creation, there is no later update endpoint
for it) and set `target_nodes` to a *different* node than the one above, to
exercise cross-node service linking. Note a project can only link one service
per `service_type` (e.g. one postgres, one redis) — linking a second postgres
service to the same project fails.

## Recipe: toggling cluster DNS

Cluster DNS defaults **off**. The setting is a **top-level `cluster_dns`
object with an `enabled` field** — NOT `multi_node.cluster_dns_enabled` (that
key doesn't exist in the real schema; writing it via raw SQL succeeds
silently but does nothing, and disappears on the next control-plane restart
since settings get re-normalized from the typed Rust struct on load).
Verified live:
```bash
docker exec temps-dev-postgres psql -U temps -d temps -c \
  "update settings set data = jsonb_set(COALESCE(data::jsonb,'{}'),'{cluster_dns,enabled}','true'::jsonb)::json where id=1;"
docker exec temps-dev-postgres psql -U temps -d temps -tAc \
  "select data::jsonb->'cluster_dns' from settings where id=1;"   # => {"enabled": true}
```

**Two restarts are required after this, not zero:**
1. `docker restart temps-dev-control-plane` — a raw SQL write bypasses
   `ConfigService`'s in-memory settings cache; the control plane won't see
   the change until it re-reads settings on startup (there's no live
   invalidation path for a write that didn't go through the settings API).
2. `docker restart temps-dev-worker-1 temps-dev-worker-2 temps-dev-worker-3`
   (or at least whichever workers are relevant to your test) — **the per-node
   DNS resolver is only spawned once, during initial agent startup.** An
   agent that was already running when you flip `cluster_dns.enabled` will
   keep operating in "disabled" mode indefinitely; it does not poll for this
   setting changing live. This is a known gap (tracked as part of ADR-024,
   proposed not yet implemented) — don't lose time assuming a stuck deploy
   means the fix under test is broken; check `docker logs <worker> | grep -i
   resolver` for `"DNS resolver started"` before assuming anything else is
   wrong.

After both restarts, worker containers hit a **transient DinD
`containerd: timeout waiting for containerd to start` stall** fairly often
on a busy host — this is the same known containerd-stall gotcha as during
initial `up`. Don't machine-gun `docker restart` on it; one extra restart is
fine, but if it's still stalling after that just wait — it clears on its own
within roughly a minute. Confirm real recovery via a fresh heartbeat, not
just "container status is Up":
```bash
docker exec temps-dev-postgres psql -U temps -d temps -tAc \
  "select extract(epoch from now() - last_heartbeat) from nodes where name='worker-2';"
# a small number (single-digit seconds) means it's actually back
```

If a deployment fails with `"Worker route/DNS propagation did not complete
within 10s ... N active node(s) have never ACKed a DNS generation at all"`,
that message IS the diagnosis — it's telling you exactly this: enable
cluster DNS, then restart the agents, in that order, before deploying.

## Gotcha: creating an external service (database) pinned to a fresh worker

If a worker has never had ANY container deployed to it before, `POST
/api/external-services` with `node_id` pointing at that worker can fail with:
```
Docker responded with status code 404: failed to set up container networking: network temps-app-network not found
```
Regular app deployments to a node appear to ensure this bridge network
exists first; external-service creation on a remote node does not (this
looks like a real, narrow gap — worth a separate bug report, distinct from
whatever cross-node fix you're testing). Workaround to unblock testing:
```bash
docker compose -f docker-compose.yml -p temps-dev-cluster exec -T worker-1 \
  docker network create temps-app-network
```
If the first creation attempt fails this way, it also leaves an orphaned
container behind holding the service's name (Docker created the container
before the network-attach step failed) — the retry will then fail differently
with a container-name conflict. Clean up before retrying:
```bash
docker compose -f docker-compose.yml -p temps-dev-cluster exec -T worker-1 \
  docker rm -f <service-name>
```

## Teardown

- `./dev-cluster down` — stops containers, **keeps** postgres data / cargo
  cache / worker docker volumes. Next `up` is fast (~10-30s).
- `./dev-cluster reset` — `docker compose down -v` + removes `.state/`.
  Prompts for confirmation (deletes postgres data, worker volumes, admin
  credentials). Next `up` starts entirely from scratch: re-runs setup, mints a
  new admin password, re-allocates CIDRs, new node IDs.

## Troubleshooting

- **Build hangs at "ensuring temps binary is up to date"** — first-ever build
  compiles the whole workspace inside Linux, 5-10 min on Apple Silicon.
  `./dev-cluster logs control-plane` to watch progress.
- **Workers stuck "waiting for join token"** — control plane's `setup --auto`
  hasn't finished (postgres not ready, migrations running, or an encryption
  key conflict from a previous partial `reset`). Check
  `./dev-cluster logs control-plane`; `./dev-cluster reset` if it's a stale
  key conflict.
- **`compute_cidr` is NULL on a worker after registering** — allocator ran but
  failed; check the control-plane log for an allocator warning. Pool is
  `172.20.0.0/16` (room for 256 workers), so exhaustion shouldn't happen here.
- **Cross-node ping fails** — `./dev-cluster status` shows overlay state per
  worker. Missing bridge/vxlan device on one worker → check that worker's log;
  `network_sync` logs every poll and `NetworkManager::bootstrap` errors are
  descriptive.
- **`./dev-cluster up` fails with `Conflict. The container name "/temps-dev-postgres" is already in use`** — a stray container from an earlier run under a different (mismatched) compose project name. Verified live: this compose file declares `name: temps-dev-cluster`, but `docker compose ls -a` can show an existing project literally named `temps-dev` holding the same fixed container/volume names (volumes and container names are hardcoded in the compose file, not templated with the project name, so two different project labels can collide on the same names). Fix: `docker rm -f temps-dev-postgres` (safe — it's disposable dev-cluster scratch data, distinct from the regular single-node dev DB container `temps-dev-db`, which you must NOT touch) and re-run `up`. The "volume ... already exists but was created for project X (expected Y)" warnings that print alongside this are non-fatal and can be ignored — named volumes are reused regardless of which project label created them; only the container name collision actually blocks startup. If you need to run raw `docker compose` commands directly instead of via the `./dev-cluster` wrapper, pass `-p temps-dev-cluster` explicitly to avoid re-triggering this drift.
- **A worker container looks wedged / repeats "containerd" errors in logs** —
  transient containerd stall on a busy host. `docker restart
  temps-dev-worker-N` (plain `docker restart`, not `docker compose restart`,
  which can no-op — check `StartedAt` via `docker inspect` if unsure it
  actually bounced). If a worker container has fully exited (not just
  looping in its own retry log) with `failed to save daemon pid to disk:
  process with PID N is still running`, `docker restart` may need a second
  attempt — a clean `Exited` state followed by `docker start` gets a fresh
  PID namespace and usually clears it.
- **`docker compose restart` seems to do nothing** — known no-op risk in this
  harness; use `docker restart <container-name>` instead when you need a real
  bounce.
- **Need mTLS / node-identity hardening specifically, not general multi-node**
  — use `tools/dev-cluster/e2e-harden.sh` (2-worker `docker-compose.harden.yml`
  variant, separate container prefix `temps-harden-*`, port 80 via
  `-p temps-harden-test`) instead of this base cluster.
