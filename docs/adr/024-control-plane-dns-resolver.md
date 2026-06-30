---
title: "ADR-024: Control-plane DNS resolver for single-node *.temps.local resolution"
status: Proposed
date: 2026-06-29
author: David Viejo
---

# ADR-024: Control-plane DNS resolver for single-node *.temps.local resolution

**Status:** Proposed
**Date:** 2026-06-29
**Author:** David Viejo
**Companion:** ADR-011 (per-node Hickory DNS resolver design this extends)

## Context

ADR-011 shipped a per-node Hickory DNS resolver embedded inside `temps-agent`. It listens on `<bridge_gateway>:53` (and, on workers, `127.0.0.53:53`), is seeded from `service_endpoints` via a long-poll sync loop, and gives containers access to `*.temps.local` names with no host-port NAT. The resolver boots inside `spawn_resolver()` (`crates/temps-agent/src/network_sync.rs`) only after the multi-node overlay allocates a bridge address — so on a single-node control-plane install, where no overlay is bootstrapped and no worker nodes join, the resolver never starts at all.

The result is that a **single-node Temps install** — the most common self-hosted configuration — cannot resolve `*.temps.local` names inside containers. Every service deployed on the control plane (databases, apps, sidecars) must reach each other through host-port mappings or pre-resolved IP env vars. This is the same problem ADR-011 solved for multi-node clusters; the fix just never reached the control plane itself.

Empirically (dev-cluster, 2026-06-29): a worker-local container resolves `production.echo.temps.local` → the worker's resolver, while a control-plane-local container has `HostConfig.Dns` empty and the same lookup fails with `EAI_AGAIN`. The control plane has **no `:53` listener** at all.

The gap matters for three concrete use cases:

1. **App containers connecting to a local managed database.** Today the connection string encodes a host-port like `172.17.0.3:5432`. When the database container is recreated the IP changes and the env var is stale until the app is redeployed. With DNS, `pg-mydb.temps.local:5432` is stable across recreations.
2. **Inter-service communication on a single host.** An app calling an internal API currently needs the API's Docker-bridge IP wired in as an env var. DNS removes this fragility.
3. **Configuration parity with multi-node.** The same FQDN naming scheme documented in ADR-011 should work on a single-node install from day one, so operators can adopt FQDNs without depending on a second worker being attached.

### What already exists (and must be reused)

The full resolver machinery is present:

- `DnsResolverConfig::new(node_id, node_token, control_plane_url, bridge_gateway, snapshot_dir)` — `crates/temps-dns-resolver`
- `DnsResolverHandle::start(config)` — `crates/temps-dns-resolver/src/handle.rs` — loads disk snapshot, spawns sync loop, binds sockets
- `overlay_dns_slot: Arc<RwLock<Option<IpAddr>>>` on the deployer's `DockerRuntime` — `crates/temps-deployer/src/docker.rs` — the existing hook for wiring a dynamic resolver IP into every container's `/etc/resolv.conf` (`HostConfig.dns`)
- `ensure_network` (`temps_network::docker`) — creates `temps-app-network` on first deploy
- The control plane is the authoritative source for `service_endpoints` and already hosts the `/api/dns/sync` endpoint that workers poll

### How it composes with Docker's embedded DNS

Containers on `temps-app-network` always have `nameserver 127.0.0.11` (Docker's embedded resolver) in `/etc/resolv.conf`. That resolver (a) answers same-network container names itself and (b) forwards everything else to whatever `HostConfig.dns` is set to. The Hickory resolver is plugged in as that forwarder: `127.0.0.11` forwards `*.temps.local` to it, it answers from `service_endpoints` and forwards public names to the upstream resolvers. Docker's DNS is **layered, not replaced**. The only difference between a working worker container and a non-working control-plane container today is whether `HostConfig.dns` is populated.

### Constraints

- **Zero breaking changes.** Existing single-node installs must work identically when the resolver fails to start (e.g. `:53` occupied). Existing multi-node clusters must behave identically to today. No migration, no required config change, no new environment variable.
- **Reuse existing machinery.** Do not reinvent the sync protocol, the resolver crate, or the deployer DNS slot.
- **Bind on the bridge gateway IP, not the host loopback.** The bridge gateway (e.g. `172.19.0.1`) is private to the Docker bridge and cannot collide with `systemd-resolved`'s `127.0.0.53` reservation. Containers see this IP as their default gateway and can reach it without any special routing.
- **The control plane is the zone source directly.** It is the authoritative `service_endpoints` database; it does not need to long-poll itself.

## Decision

**Start a `DnsResolverHandle` inside `temps serve`, bound to the control plane's `temps-app-network` bridge gateway IP on port 53, reading `service_endpoints` directly from the local database. Wire the resolver's gateway IP into the deployer's existing `overlay_dns_slot` so containers deployed on the control plane receive the resolver as their first nameserver.**

This is purely additive: the resolver only starts if `temps-app-network` exists and its gateway IP is reachable; it degrades silently and non-fatally if the bind fails; it requires no configuration change and alters no existing code path.

### 1. Zone source: read the database directly (not self-HTTP)

Two approaches are possible for supplying zone data to the control-plane resolver:

- **Option A — DB-direct**: a thin zone source queries `service_endpoints` directly from the `DatabaseConnection` and hands records to the resolver's in-memory zone, refreshing on a configurable interval (default matching the worker poll cadence).
- **Option B — Self-HTTP**: the control-plane resolver acts as an ordinary worker client, authenticating to `/api/dns/sync` at `localhost` with a minted bearer token.

**Decision: Option A (DB-direct).**

The control plane *is* the database. Routing its own DNS reads through an HTTP layer it also hosts adds a circular bootstrap dependency: the HTTP server must be fully up and the sync endpoint reachable before the resolver can serve its first query. With DB-direct, the resolver loads `service_endpoints` before the first HTTP handler is registered, exactly as the disk snapshot does for worker nodes on restart. Option B also requires minting a durable bearer token for the control plane itself, storing it securely, and passing it through the DI graph — complexity with no isolation benefit when the process is already trusted to own the database. DB-direct reuses the same `service_endpoints` schema from ADR-011; no new columns or tables are needed.

### 2. Bind address: the `temps-app-network` bridge gateway IP

`DnsResolverConfig` already accepts a `bridge_gateway: IpAddr`. On workers this is the overlay bridge address. On the control plane it is the Docker-assigned gateway for `temps-app-network` (e.g. `172.19.0.1`), obtained by inspecting the network via `bollard` after the network exists.

The control-plane resolver binds **only** on `<bridge_gateway>:53`. It does **not** bind `127.0.0.53:53`, because:

- `127.0.0.53:53` is owned by `systemd-resolved` on most Linux distributions; binding it unconditionally (as the ADR-011 worker path does) causes a silent collision on a real control-plane host.
- Containers on `temps-app-network` route through the bridge gateway, so `<bridge_gateway>:53` is already reachable from inside every container without any additional configuration.
- The host's own name resolution is already handled by the system resolver; the control-plane resolver is for container-to-container traffic only.

### 3. Bootstrap sequence in `temps serve`

The resolver is started where the deployer's `DockerRuntime` is constructed (the deployer plugin / serve startup), before it is registered in the DI container, so `overlay_dns_slot` is populated before the first container is deployed.

1. Construct `DockerRuntime` as today.
2. Ensure `temps-app-network` exists (idempotent; moving this to startup makes the gateway IP available immediately).
3. Inspect `temps-app-network` to read its gateway as `bridge_gateway: IpAddr`.
4. Construct `DnsResolverConfig` for node `0` with the DB-direct zone source and `listen_addrs = vec![bridge_gateway:53]` (no `control_plane_url` / `node_token` — DB-direct bypasses the HTTP sync loop).
5. Call `DnsResolverHandle::start(config)`. On success, set `overlay_dns_slot = Some(bridge_gateway)`. On failure (bind error, Docker unavailable), log a WARN and continue — the slot stays `None` and the deployer falls back to Docker's embedded DNS, exactly as today.

Steps 2–5 are gated on Docker being available (already an optional dependency), so this path is safe when Docker is absent.

### 4. `overlay_dns_slot` wiring is unchanged

`DockerRuntime` already reads `overlay_dns_slot` and passes its value as the Docker `HostConfig.dns` list. No change to this logic. A statically-configured `dns_servers` still takes precedence. The deployer is unaware of whether the slot was populated by the agent's overlay path or the new control-plane path.

### 5. Graceful degradation

The entire bootstrap is wrapped in a fallible chain. Any failure — Docker not running, network inspection returning no IPAM config, port 53 already bound — results in: a structured WARN (`"control-plane DNS resolver unavailable: {reason}; containers will use Docker embedded DNS"`), `overlay_dns_slot` left `None`, and all subsequent deploys proceeding without DNS injection, identical to current behavior on every single-node install. No `Result` is propagated; the resolver is a best-effort enhancement, not a required service.

### 6. Multi-node: no change

On a multi-node install, `temps-agent` continues to run `spawn_resolver()` on each worker. The control plane *also* runs its own resolver (via this ADR), serving control-plane-local containers from the same authoritative DB. The two resolvers are independent and serve the same `*.temps.local` zone — the same "full zone per node" design from ADR-011. No existing multi-node code path is modified.

### 7. Level-2 follow-up (out of scope)

This ADR covers the single-node case: all deployed containers live on the control plane's Docker bridge, so the control-plane resolver's A records point to addresses reachable from those containers. In a **mixed topology** where workloads land on the control plane *and* on remote workers, a control-plane-local container resolving a worker-side service may receive an overlay IP that is **not reachable** from the control plane's plain Docker bridge, because the control plane is not joined to the WireGuard overlay.

**Resolving cross-node names from control-plane-local containers requires the control plane to join the WireGuard overlay as a node.** That is a separate, meaningful change (touching `temps-network`, WireGuard key management, the overlay bootstrap) and is explicitly deferred to a follow-up ADR. Until then, control-plane-local containers should continue to use host-port env vars for cross-node services, as they do today. **This ADR does not make that situation worse** — it only adds resolution that is fully correct for the single-node case and harmless (resolves but, for cross-node targets, the same un-routability that exists today) for the mixed case.

## Consequences

### Positive

- Single-node installs gain stable `*.temps.local` DNS for container-to-container communication with no operator action.
- Connection strings for local managed databases can use FQDNs (`pg-mydb.temps.local`) instead of fragile bridge IPs, surviving container recreation.
- Reuses `DnsResolverHandle`, `DnsResolverConfig`, `overlay_dns_slot`, and `service_endpoints` verbatim — no new protocol, no new schema, no new crate.
- The deployer/serve startup is the single integration point; nothing in the HTTP server, proxy, or auth layers changes.
- Graceful degradation preserves full backward compatibility on every existing install.

### Negative

- Ensuring `temps-app-network` exists now happens at startup rather than lazily on first deploy — a minor, idempotent behavioral change (Docker is contacted at startup).
- The resolver runs as an in-process Hickory server consuming a few MB RSS on top of the existing `temps serve` footprint.
- Cross-node resolution from control-plane-local containers is not solved here (level-2 follow-up).

### Risks

- **Port 53 conflict on unconventional hosts.** If something else already owns the bridge gateway IP's `:53`, the bind fails silently and the feature is simply unavailable; the WARN log is the operator's signal.
- **Docker network IPAM variability.** If Docker assigns no gateway, the inspection step finds no IP, logs a WARN, and skips resolver startup.
- **DB-direct zone reader adds a query path at serve startup.** Consistent with other components that read the DB directly; it is not an HTTP handler, so it does not violate the Handler → Service → Data layering.

## Alternatives considered

- **Bind on `127.0.0.53:53` (host loopback).** Collides with `systemd-resolved` on a typical host; the bind would fail silently on most installs. Rejected as the primary bind.
- **Bind on `0.0.0.0:53`.** Requires elevated privileges, collides with any system resolver, and exposes the internal zone on the host's public interfaces. Rejected on security grounds.
- **Self-HTTP zone source.** Circular boot dependency + a durable bearer token for the control plane. Rejected in favor of DB-direct (§1).
- **Opt-in flag (`--enable-cp-dns` / settings column).** Rejected: the feature is entirely additive and degrades silently when unavailable, so detection-based activation (bind succeeds → on, fails → off) is strictly safer than a flag that leaves every single-node install without DNS until an operator discovers it. The "config as entity-row column" principle applies to *runtime behavior changes*, not to infrastructure that auto-detects and degrades.
- **Separate `temps dns-resolver` process.** Adds operational complexity (a second process to manage). Unnecessary given the in-process Hickory model from ADR-011.

## Implementation notes

**Affected crates:**
- `crates/temps-dns-resolver` — add a `DatabaseConnection`-backed zone source for the control-plane (DB-direct) path; let `DnsResolverConfig`/`start` select DB-direct vs. HTTP-sync mode.
- `crates/temps-deployer` and/or `crates/temps-cli` (serve startup) — ensure the app network, inspect its gateway IP, start the resolver, and populate `overlay_dns_slot`; add a small `inspect_network_gateway(docker, network_name) -> Option<IpAddr>` helper.

**Migration needed:** No. **Breaking changes:** No. **No new tables, migrations, or environment variables.**

## References

- ADR-011 — Internal DNS for HA Database Resolution (per-node Hickory resolver, `service_endpoints` schema, naming scheme); this ADR extends it to the control plane.
- `crates/temps-dns-resolver/src/handle.rs` — `DnsResolverHandle::start()`.
- `crates/temps-agent/src/network_sync.rs` — `spawn_resolver()` (worker path, unchanged by this ADR).
- `crates/temps-deployer/src/docker.rs` — `overlay_dns_slot`, its read at deploy time into `HostConfig.dns`, and `ensure_network`.
