# temps-network

Multi-host container networking for Temps. Gives a worker node the kernel +
Docker plumbing so containers on different hosts can reach each other by IP.

## Status

Foundational — Phase 1+3 of the multi-host networking plan. Compiles on
Linux and macOS; kernel data-plane is Linux-only. Not wired into
`temps-agent` yet (next branch).

## Two-network model (zero-downtime upgrade)

This crate creates a **second** Docker network, `temps-overlay`, alongside
the existing `temps-app-network` that all current Temps containers run on.
Existing containers are never moved or restarted — they keep using
`temps-app-network` exactly as they always did.

When multi-host networking is enabled on a node:

- Containers attach to **both** networks. Each container ends up with two
  veth interfaces and two IPs:
  - **eth0** on `temps-app-network` (existing behavior, default route,
    same-node service discovery via Docker's embedded DNS)
  - **eth1** on `temps-overlay` (new, used for cross-node traffic over
    the VXLAN tunnel managed by this crate)
- Same-node container-to-container traffic continues to flow through
  `temps-app-network` unchanged.
- Cross-node traffic uses `temps-overlay`. The kernel's routing table
  decides per-packet, based on destination CIDR.

This is the same pattern Docker Swarm uses for service containers
(ingress overlay + bridge). It's additive: nothing about
`temps-app-network`, `DEPLOYMENT_MODE`, or existing container lifecycle
changes. Single-host clusters never see `temps-overlay` at all.

## What it does

Each container ends up with two interfaces:

```
┌─────────────────── Node A (underlay 10.0.0.1) ───────────────────┐
│                                                                  │
│   container "api"                                                │
│     ├─ eth0 → temps-app-network (172.18.0.5)  ← unchanged        │
│     └─ eth1 → temps-overlay     (172.20.1.10) ← managed by us    │
│                       │                                          │
│                  (this crate owns everything below)              │
│                       │                                          │
│   br-temps0 (172.20.1.1/24)  ◄── docker network "temps-overlay"  │
│       │                          pinned via bridge.name driver opt
│   vxlan-temps0 (vni 42, port 4789, nolearning)                   │
│       │ FDB: 00:00:00:00:00:00 → 10.0.0.2                        │
│       │ MTU: 1450  (underlay 1500 - VXLAN 50)                    │
│   eth0 (underlay) ───────────────────┐                           │
└──────────────────────────────────────┼───────────────────────────┘
                                       │ UDP/4789
┌──────────────────────────────────────┼───────────────────────────┐
│   eth0 (underlay 10.0.0.2) ◄─────────┘                           │
│   vxlan-temps0 (same vni, same port) ─┐                          │
│   br-temps0 (172.20.2.1/24)  ◄────────┴── docker net temps-overlay
│     ├─ eth1 → temps-overlay     (172.20.2.10)                    │
│     └─ eth0 → temps-app-network (172.18.0.7) ← unchanged         │
│   container "worker"                                             │
└──────────────────────────────────────────────────────────────────┘
```

Routes installed by `bootstrap`:
- on A: `172.20.2.0/24 dev vxlan-temps0`  (peer's compute_cidr)
- on B: `172.20.1.0/24 dev vxlan-temps0`

The Docker network `temps-app-network` is left entirely alone. Existing
containers see no behavior change. New containers that opt into the
overlay get an additional `eth1`.

## Components

| File | Purpose |
|---|---|
| `src/config.rs` | `NetworkConfig`, `NodeAlloc`, `Peer`, `Transport` — pure data, serde-able. |
| `src/diff.rs` | `PeerDiff` / `RouteDiff` — pure-logic reconciliation algorithm. |
| `src/error.rs` | `NetworkError` — typed errors with full context. |
| `src/manager.rs` | `NetworkManager` — public lifecycle (`bootstrap`, `reconcile_peers`, `teardown`). |
| `src/docker.rs` | `bollard`-based Docker network creation pinned to our bridge. |
| `src/linux/` | Linux kernel data-plane (`bridge`, `vxlan`, `route`, `firewall`, `sysctl`). |

## Transports

- **`Transport::Vxlan { vni, port }`** — recommended default. Encapsulates
  inter-container traffic in UDP, works through cloud firewalls, costs
  ~50 bytes per packet.
- **`Transport::Native`** — no encapsulation. Use only when nodes share an
  L2 segment or a cloud private network where you can install host routes
  for each peer's compute CIDR. Zero overhead.

## Usage sketch

```rust
use temps_network::{NetworkConfig, NetworkManager, NodeAlloc, Peer};

let mgr = NetworkManager::new(NetworkConfig::default())?;

// On node startup, the control plane hands us our allocation + peer list:
mgr.bootstrap(alloc, peers).await?;

// Whenever the cluster changes:
mgr.reconcile_peers(new_peers).await?;

// When the node leaves:
mgr.teardown().await?;
```

All operations are idempotent; partial failures can be retried.

## Tests

### Unit tests (always run)

24 tests in `src/{config,diff,manager,linux/firewall}.rs` cover:

- Config validation: bridge name length, MTU bounds, port zero, missing
  underlay, bridge addr outside CIDR, overlapping peer CIDRs, self-peer
  rejection, JSON roundtrip.
- Diff algorithm: empty inputs, single add/remove, replace on underlay
  change, replace on CIDR change, idempotent on unchanged peers, order
  independence, route diff.
- Manager: config validation at construction, `UnsupportedPlatform` on
  non-Linux, `Debug` impl.
- Firewall script generation: bridge name + CIDR + masquerade clauses.

```bash
cargo test -p temps-network --lib
```

### Kernel integration tests (require Linux + privileged container)

`tests/it_kernel.rs` — gated behind the `integration_kernel` feature and
`target_os = "linux"`. Each test asserts kernel state with `ip`, `bridge`,
`nft`, and `/proc/sys/...`:

| Test | What it proves |
|---|---|
| `bootstrap_creates_all_kernel_state` | bridge + vxlan + addr + MTU + route + FDB + nftables all materialized |
| `bootstrap_is_idempotent` | re-running with same args is a no-op |
| `reconcile_peers_adds_new_peer` | new peer FDB + route added without disrupting existing |
| `reconcile_peers_removes_peer` | removed peer FDB + route gone, others untouched |
| `reconcile_peers_noop_on_unchanged` | identical peer list returns `false` |
| `teardown_removes_everything_and_is_idempotent` | full cleanup, callable twice |
| `bootstrap_creates_docker_network` | docker network pinned to our bridge with right CIDR/MTU |
| `docker_cidr_collision_is_detected` | refuses to corrupt state when CIDR is taken |
| `invalid_config_rejected_before_kernel_calls` | validation runs before any I/O |
| `bridge_address_outside_cidr_rejected` | per-allocation validation works |
| `bootstrap_only` | helper for the cross-host harness |

### Run the kernel tests

Single-node (everything except cross-host ping):

```bash
crates/temps-network/tests/dind/run-single.sh
```

Two-node (full cross-host ping over VXLAN):

```bash
crates/temps-network/tests/dind/run.sh
```

Both scripts spin up privileged Docker-in-Docker containers, mount the
workspace, install a Rust toolchain (cached), then run the gated tests.
Skip gracefully when Docker isn't available; fail loudly when any
assertion breaks.

### Manual PoC scripts

Independent of Rust — run on two real Linux hosts to validate the design
itself:

```bash
# On host A:
sudo PEER_UNDERLAY=10.0.0.2 PEER_CIDR=172.20.2.0/24 \
     LOCAL_CIDR=172.20.1.0/24 LOCAL_BRIDGE_IP=172.20.1.1 \
     scripts/network-poc/node-up.sh

# On host B:
sudo PEER_UNDERLAY=10.0.0.1 PEER_CIDR=172.20.1.0/24 \
     LOCAL_CIDR=172.20.2.0/24 LOCAL_BRIDGE_IP=172.20.2.1 \
     scripts/network-poc/node-up.sh

# Verify:
docker run -d --rm --network temps-overlay --ip 172.20.1.10 nginx:alpine     # host A
docker run --rm --network temps-overlay --ip 172.20.2.10 alpine \
    sh -c "ping -c3 172.20.1.10"                                       # host B
```

## What's not yet wired up

This branch lands the standalone crate + tests + PoC. Subsequent branches
will add:

- Schema migration (`compute_cidr`, `underlay_address` columns on `nodes`)
- Control-plane allocator (`ComputeNetworkAllocator`)
- Server endpoint that broadcasts the peer list
- `temps-agent` startup hook calling `NetworkManager::bootstrap`
- `temps-deployer` **also** attaching new containers to `temps-overlay`
  (additive — existing `temps-app-network` attachment stays as primary)
- `temps network status / peers / diag` CLI subcommands

## Design notes

- **Why VXLAN, not WireGuard or IP-in-IP?** Universal: every cloud allows
  UDP/4789, every kernel since 2012 supports it, no key management. Native
  routing for Hetzner-private-network deployments is opt-in via
  `Transport::Native`.
- **Why FDB via `bridge fdb` shell instead of netlink?** rtnetlink's FDB
  surface is awkward for the all-zero-MAC multicast trick we need.
  `bridge` is part of `iproute2` which is universal.
- **Why nftables shell-out instead of `rustables`?** Our rule set is six
  lines. The crate dependency would be larger than the rule body. We use
  one dedicated table (`temps_network`) so cleanup is trivially atomic.
- **Why no IPAM here?** Per-container IP assignment is Docker's job —
  we hand it the per-node CIDR and it picks. The control plane handles
  per-node CIDR allocation.
