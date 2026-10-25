# ADR 056: A Compute Pool That Avoids Collisions and Can Be Changed

- **Status:** Accepted (implemented)
- **Date:** 2026-10-19
- **Builds on:** ADR-020 (multi-node hardening), ADR-048 (mesh), ADR-055 (outbound join)

## Context

Every cluster has one compute pool: the address range its containers use on the
cross-node overlay (`network_config.compute_pool_cidr`). Each node gets a `/24`
of it (`nodes.compute_cidr`), a `temps0` Docker network and bridge on that
`/24`, and routes to every peer's `/24`.

The default pool is `172.20.0.0/16`. That is inside Docker's own default
address pool (`172.17.0.0/12`): `temps-app-network`, sandbox networks, the
preview gateway and every Compose project get a network from it, climbing
172.18, 172.19, 172.20. A worker that has run a few Compose projects therefore
already owns `172.20.0.0/16` and can never bring up the overlay. Found on the
first real outbound join (ADR-055): the agent logged

```
host route preflight: compute pool 172.20.0.0/16 overlaps host route
172.20.0.0/16 on device 'br-f6051ccc7abf'; choose a different cluster pool
```

every 5 seconds, forever. Nothing reached the console, and the console could
not change the pool anyway:

- The preflight compares the **whole pool** with every host route and Docker
  network, although a node only ever routes its own `/24` and its peers'.
- A collision on a worker never leaves the worker (no heartbeat field, no node
  status). On the control plane it is classified as transient, so the console
  shows "a temporary error, see the logs".
- The pool can only be changed with `temps network setup-multi-node
  --compute-pool-cidr` on the control plane host, and only before the first
  `/24` is handed out. A hosted plane has no shell, and the first join hands
  one out immediately, even to a node that then fails to use it.

What already works: a container is attached to `temps0` only if it exists
(`maybe_attach_overlay`), and the proxy reaches remote apps at
`node.data_address():published_port`, not at overlay addresses. A node without
the overlay still runs and serves deployments; it only lacks container-to-
container traffic across nodes (managed services, `*.temps.local`).

## Decision

### 1. A new default, outside the ranges software hands out

New clusters default to **`10.202.0.0/16`** (`/24` per node, 256 nodes), next
to the mesh's `10.201.0.0/16`, so Temps' defaults sit in one block that is
easy to document and to exclude.

It avoids Docker's default pools (`172.17.0.0/12`, `192.168.0.0/16`), the
common Kubernetes ranges (`10.42/16`, `10.43/16`, `10.96/12`, `10.244/16`),
cloud default VPCs (`172.31/16`, `10.0/16`), home routers (`192.168.x`,
`10.0.x`, `10.1.x`), WireGuard/OpenVPN defaults (`10.6`–`10.8`), and
Tailscale (`100.64/10`). No RFC 1918 range is unused by everyone; sections 2–3
make a collision harmless and fixable rather than pretending to avoid it.

Existing clusters keep the pool they stored. Only the column default and the
unconfigured-cluster default change; no row is migrated.

### 2. A collision degrades one node, says why, and is avoided where possible

- **Check only what a node routes.** The route and Docker preflights compare
  host routes and other Docker networks with the node's own `/24`, its peers'
  `/24`s and the control plane's, not with the whole pool. A Compose network
  elsewhere in the pool no longer matters.
- **Nodes report what they occupy.** The agent sends its occupied IPv4 ranges
  (unicast host routes on non-Temps devices, excluding default and `/32`
  routes, plus other Docker networks' subnets) with registration and in its
  heartbeat when they change. Stored per node (`nodes.occupied_ranges`, JSON).
- **Allocation avoids them.** A new `/24` is the lowest free one that overlaps
  no member's occupied ranges and no member's underlay. A node whose current
  `/24` collides with its own occupied ranges and that has **never applied
  it** (not overlay-ready) is given a new one automatically; that is
  auto-remediation and is logged as such.
- **Otherwise the node runs without the overlay and says so.** The agent
  reports `overlay_status` (`ready` / `unavailable`) and `overlay_last_error`
  (stored like `dns_resolver_last_error`). The console shows the node as
  "Cross-node network unavailable" with the exact cause (range, device or
  Docker network name) and the two fixes: change the cluster pool, or move
  that network on the node. Deployments keep working on it. The scheduler
  does not place managed-service members on a node whose overlay is not
  ready.
- **Retry without noise.** The agent backs off exponentially to 5 minutes for
  an unchanged collision and logs it once per change of cause, then at most
  every 5 minutes; it retries immediately when its occupied ranges or its
  allocation change.
- **Control plane side.** `HostRouteCollision` and `DockerCidrCollision` are
  operator-actionable, not transient: the stored setup failure carries the
  real cause and the fix.

### 3. The pool can be changed from the console

- `PUT /api/settings/cluster-network` (`SettingsWrite` plus a sensitive-action
  step-up, audited) and a **Cluster network** card on the Worker Nodes page
  replace the copy-the-CLI-command section of the Cluster DNS card. The CLI keeps working and uses the same service.
- The new pool must be RFC 1918, fit the per-node prefix, not overlap the mesh
  pool (a check that is missing today), any node's underlay, or any node's
  occupied ranges. Each refusal names the node and the range.
- **A change is allowed while no allocation is in use.** An allocation is in
  use once its node, or the control plane, has reported the overlay ready on
  it. Allocations that were handed out but never applied (the node above) do
  not lock the pool: they are re-assigned from the new pool in the same
  transaction, and the nodes pick them up on their next poll.
- **A pool in use** is refused with the list of nodes using it. Moving a live
  overlay means re-addressing `temps0` on every node, recreating containers
  whose DNS server is the old bridge address, and rewriting
  `service_members.compute_ip` and managed-service DNS. That is a separate,
  later decision; this ADR does not pretend to do it.

### 4. Join errors from the same run

Two defects found on the same first outbound join, fixed with this change:

- A join that cannot change the network (`Operation not permitted` from
  netlink) says to run it as root or grant `CAP_NET_ADMIN`, and that a
  container needs the host to grant it.
- Re-running `temps join` on the same machine (same WireGuard key and name)
  while its outbound enrollment is still pending resumes that enrollment
  instead of being refused for a key in use. This is safe: the pairing's join
  token only works over the node API from the reserved mesh address, which
  needs the machine's private key. A new join token is still required and is
  validated and consumed as usual, and it must have been minted by the same
  user as the one that started the enrollment: a public key is not a secret,
  so otherwise any other token holder could cancel a stranger's pending join
  by resuming it. The pairing's previous token is revoked in the same
  transaction.

## Consequences

- New clusters rarely collide; when they do, the affected node keeps serving
  deployments, the console says what collides and how to fix it, and the
  allocator steers new nodes around known collisions.
- A hosted plane can change its pool without a shell, as long as nothing uses
  the overlay yet; which covers every cluster that never got the overlay up.
- Self-hosted clusters keep their stored pool and behaviour. A node that was
  failing the whole-pool preflight may start working, because only its routed
  subnets are checked.
- Live re-addressing of a pool in use remains manual (remove and re-enrol
  nodes) until a follow-up ADR.

## Implementation notes

Where the implementation differs from, or decides beyond, the text above:

- **New default.** The `network_config.compute_pool_cidr` column default is
  `10.202.0.0/16` both in the migration that creates the table (it seeds the
  singleton row from the default, so only fresh databases see it) and in a
  new migration that only `SET DEFAULT`s the column. No row is rewritten.
- **"In use" includes "never reported".** A node with an allocation whose
  agent never reported `overlay_status` (an agent older than this change)
  locks the pool like one reporting `ready`: the server cannot tell whether
  that agent applied it. An allocation is free only when its node reported
  `unavailable` and never `ready` on it (`nodes.overlay_ready_cidr`, sticky
  once set, so a later failure never frees it).
- **Control-plane reservation.** A control-plane subnet reserved but not yet
  reported ready also refuses a change (`409`, "setup in progress"), because
  the control plane may be bringing it up at that moment.
- **Pool checks.** The mesh pool is checked even while the mesh is off (it is
  the range the mesh takes when turned on). "Any node's underlay" covers the
  control plane's address and each node's underlay, mesh and private
  addresses.
- **Allocation order.** Candidates that overlap nobody's occupied ranges come
  first; failing that, one overlapping only other members' ranges (it still
  works on this node); a subnet overlapping the node's own ranges is the last
  resort and is logged.
- **Auto-remediation** runs when a node registers and whenever its reported
  occupied ranges change. It only moves an allocation that is not the one
  reported ready, under the cluster config lock, with a compare-and-swap on
  the old value.
- **Ready is recorded only on the current allocation.** A heartbeat that
  reports the overlay ready on a subnet the node no longer holds (a pool
  change or remediation re-assigned it while the report was in flight) sets
  the status but not `overlay_ready_cidr`; the write is a compare on
  `compute_cidr`, and the pool change holds the node rows locked.
- **Agent retry.** Backoff starts at 5 s and doubles to 5 minutes while the
  inputs (allocation, peers, occupied ranges) stay the same; the agent still
  polls every 30 s to notice a change, which retries at once. A cause is
  logged when it changes, then at most every 5 minutes. A malformed occupied
  range in a report is dropped with a warning rather than failing the
  heartbeat.
- **Control plane.** `HostRouteCollision` and `DockerCidrCollision` record
  their own message (range, owner, fixes) as the setup failure and are
  retried with backoff rather than waiting for a settings change, because the
  fix may be on the host (removing a Docker network).
- **Placement.** The managed-service refusal applies when the cluster's
  members span more than one host and a member's node reports `unavailable`;
  a node that never reported is not refused.
- **Known race.** A node that brings its overlay up on an allocation in the
  same instant a pool change re-assigns it ends up on the new allocation at
  its next poll; the agent tears the old overlay down and rebuilds it. If its
  `temps0` Docker network already exists on the old subnet, the agent reports
  that, with the `docker network rm` fix, as the overlay error.
- **Native transport.** Peer routes a native-transport overlay installs on the
  underlay device are left out of the occupied report (they are Temps'
  own); the on-node preflight is unchanged.

## Amendment 1 — confirm the pool once, from the Worker Nodes page (2026-10-20)

The pool can no longer change in place after a node uses it, and the
default can collide with a range the operator's servers or VPN already use.
So the console asks for it once, before it offers to add the first worker:

- `network_config.compute_pool_confirmed_at` records the first confirmation.
  Saving the pool through `PUT /settings/cluster-network`, changed or not, is
  the confirmation. Clusters that already have a node, or whose control plane
  already holds a subnet, are backfilled as confirmed.
- `GET /settings/cluster-network` reports `pool_confirmed`: confirmed, or in
  use by any member.
- The Worker Nodes page leads with three steps: confirm the container address
  range (or choose another), turn on the private network in one click, and
  copy one command that installs Temps, joins with a single-use enrollment
  token and starts the agent. Every other way to add a node, and the mesh hub,
  cluster network, DNS and trust cards, sit behind collapsed sections.
- Only the console reads the flag. `temps join`, pairing and SSH enrollment
  never check it, so scripted joins and existing installs are unaffected.
