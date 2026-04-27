# Dev Cluster — local 3-worker Temps for distributed development

A docker-compose setup that runs a real Temps control plane plus three
worker nodes on your Mac, with the multi-host overlay (`temps-network`,
VXLAN, compute_cidr allocation) fully wired. Use it to test
distributed/HA features without provisioning real servers.

## Topology

```
┌──── temps-underlay (10.42.0.0/24) — the "VPC" ────┐
│                                                   │
│  postgres                10.42.0.5 (TimescaleDB)  │
│                                                   │
│  control-plane           10.42.0.10               │
│    temps serve  →  http://localhost:8080          │
│                                                   │
│  worker-1                10.42.0.21               │
│  worker-2                10.42.0.22               │
│  worker-3                10.42.0.23               │
│    each: privileged DinD + temps agent            │
│                                                   │
└───────────────────────────────────────────────────┘

Allocator carves /24s from 172.20.0.0/16 → one per worker:
  worker-1 → 172.20.0.0/24
  worker-2 → 172.20.1.0/24
  worker-3 → 172.20.2.0/24

Cross-node container traffic flows over a VXLAN tunnel pinned to the
underlay IPs above. Within-node traffic uses the unchanged
`temps-app-network` Docker bridge.
```

## Prerequisites

- Docker Desktop running on macOS (or any Linux box with Docker).
- ~3 GB free RAM (postgres + control plane + 3 workers ≈ 2 GB working set).
- ~5 GB free disk for the compiled `temps` binary cache + 4 worker
  Docker volumes + a TimescaleDB volume.
- A copy of the MaxMind GeoLite2 City database at the repo root:

  ```bash
  cp ~/.temps/GeoLite2-City.mmdb ./GeoLite2-City.mmdb   # if you've run temps setup before
  # or download from https://dev.maxmind.com/geoip/geolite2-free-geolocation-data
  ```

  The proxy plugin refuses to start without it. The file is gitignored.

## Caveat: web UI

The control plane builds in **debug mode** inside the container, which
skips bundling the React web UI (per `crates/temps-cli/build.rs`). The
HTTP API at `http://localhost:8080/api/...` works fully; the `/`
route serves a placeholder. To get the real web UI, either:

- Build the binary on your Mac in release mode (`FORCE_WEB_BUILD=1
  cargo build --release --bin temps`) and rsync it into the container,
  or
- Run a separate `bun run dev` against `http://localhost:8080` — the
  same flow you'd use without the dev cluster.

Most distributed-feature debugging happens via API + CLI anyway, so the
missing UI is rarely a blocker.

## Quick start

```bash
cd tools/dev-cluster

./dev-cluster up        # ~30s after first build (~5 min for very first build)
                        # prints admin credentials + join token location

# open http://localhost:8080 in your browser

./dev-cluster status    # node + overlay state
./dev-cluster logs control-plane
./dev-cluster shell worker-1   # poke around inside a worker
./dev-cluster restart control-plane  # bounce after a binary rebuild

./dev-cluster down      # stop without losing state
./dev-cluster up        # comes back up in ~10s
./dev-cluster reset     # nuke postgres + worker volumes (fresh slate)
```

## How it works

Each container plays a role:

| Container | Image | Command | Role |
|---|---|---|---|
| `temps-dev-postgres` | `timescale/timescaledb-ha:pg18` | default | persistent DB |
| `temps-dev-control-plane` | `dind-temps` (built locally) | `role-control-plane.sh` | builds binary, runs setup once, runs `temps serve` |
| `temps-dev-worker-{1,2,3}` | same | `role-worker.sh` | builds binary, joins cluster, runs `temps agent` |

The temps source tree is bind-mounted at `/workspace` in every DinD
container. `cargo build --bin temps` runs inside Linux so the resulting
binary's ELF format matches the container's kernel + glibc — you don't
need to cross-compile from macOS.

The cargo registry cache is a named volume shared across all containers,
so the binary only compiles once for the cluster (each container does a
no-op `cargo build` in <1s when its target/ entry is already up to date).

## How the join handshake bootstraps the overlay

On first boot, `role-control-plane.sh`:

1. Runs `temps setup --auto` to create the admin user and seed the
   database.
2. Generates a 32-byte random join token, hashes it, and writes the
   hash directly into the `settings` row's `multi_node.join_token_hash`
   field (the same effect as `POST /settings/join-token/generate`, but
   without needing an admin login).
3. Writes the plaintext token to `.state/join_token.txt`.
4. Starts `temps serve`.

Each worker's `role-worker.sh`:

1. Waits for the join token file to appear.
2. Calls `temps join <CONTROL_PLANE_URL> <TOKEN> --private-address
   <WORKER_UNDERLAY_IP>`. This hits `POST /api/internal/nodes/register`
   on the control plane, which (per the auto-allocate-on-register
   feature) immediately assigns a `compute_cidr` to the new node and
   persists `underlay_address`.
3. Starts `temps agent`. The agent's `network_sync` background loop
   then polls `GET /api/internal/nodes/{id}/network/peers`, sees an
   allocation, and calls `NetworkManager::bootstrap` — which creates
   the kernel bridge, vxlan device, FDB entries, routes, and nftables
   rules. From this point cross-node container traffic flows over the
   overlay.

## Verifying multi-host networking

After `./dev-cluster up`:

```bash
# Open a shell on worker-1, deploy a container on the per-node bridge,
# then confirm worker-2 can reach it via the overlay IP.
./dev-cluster shell worker-1
docker run -d --rm --name target --network temps-overlay --ip 172.20.0.50 nginx:alpine
exit

./dev-cluster shell worker-2
docker run --rm --network temps-overlay --ip 172.20.1.50 alpine sh -c \
    'apk add -q curl && curl -sf http://172.20.0.50/ | head -c80'
# → "<!DOCTYPE html>..." means the overlay is working
```

If that succeeds you've validated: bridge creation, VXLAN encapsulation
between node 1 and node 2, FDB entries, route table installation, and
the dual-attach the deployer does (the new container is on
`temps-app-network` AND `temps-overlay`).

## Resetting state

- `./dev-cluster down` — stops services. Postgres data, the cargo
  cache, and each worker's docker storage are kept. Restart is fast.
- `./dev-cluster reset` — runs `docker compose down -v` and removes
  `.state/`. Next `up` starts entirely from scratch (re-runs `setup`,
  re-mints join token, re-allocates CIDRs).

## Troubleshooting

**Build hangs at "ensuring temps binary is up to date".** First-ever
build inside the dev cluster compiles the workspace from scratch inside
Linux, ~3-5 minutes on Apple Silicon. Subsequent builds are seconds.
Watch with `./dev-cluster logs worker-1`.

**Workers stuck in "waiting for join token".** The control plane's
`setup --auto` failed or hasn't finished migrations.
`./dev-cluster logs control-plane` will show the cause. Common causes:
postgres not ready, port 8080 already in use on your Mac, encryption
key conflict (clear with `./dev-cluster reset`).

**`compute_cidr` is NULL on a worker after registering.** The
auto-allocator ran but failed (most likely pool exhausted). Check
`./dev-cluster logs control-plane` for the allocator warning. Each
worker gets a /24 from `172.20.0.0/16`, so there's room for 256 of
them — exhaustion shouldn't happen here.

**Cross-node ping fails.** Run `./dev-cluster status` to see the
overlay state on every worker. If the bridge or vxlan device is
missing on one of them, look at that worker's logs — the
`network_sync` loop logs every poll, and `NetworkManager::bootstrap`
errors include enough context to identify the failure.

**"docker daemon is not running"**. Start Docker Desktop. The script
checks `docker version` before doing anything destructive.

## Testing the HA DNS path (ADR-011)

The `test-ha-dns.sh` script validates the internal DNS layer end-to-end
against the running dev cluster:

```bash
./tools/dev-cluster/test-ha-dns.sh
```

What it checks:

1. The DNS schema (`service_endpoints`, `node_dns_state`,
   `dns_generation`) is in place on the control plane.
2. Each worker has registered a `node_dns_state` row.
3. After inserting a synthetic 3-member Postgres-cluster shape into
   `external_services` + `service_members` + `service_endpoints`, the
   cluster-wide generation advances and VIP records appear.
4. A worker container can resolve `<svc>.temps.local` against its
   per-node Hickory resolver.
5. Deleting the parent `external_services` row leaves orphan DNS records
   that the janitor's NOT EXISTS query reaps.

The script uses synthetic DB inserts rather than the full
`bunx @temps-sdk/cli` flow so it stays self-contained — it doesn't need
to know admin credentials or the current CLI argument shape. To test
the real end-to-end path (control-plane manager → DnsRegistry →
resolver), create a Postgres HA cluster through the web UI and watch
`service_endpoints` populate as the lifecycle hooks fire.

Exit codes: `0` ok, `2` cluster not running (skipped), anything else =
a check failed (read the log).
