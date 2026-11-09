# ADR 062: One host, several clusters' overlays

## Status

Accepted — 2026-11-09 (drafted 2026-10-24)

Related: ADR 048 (multi-node overlay), ADR 056 (occupied ranges and
auto-remediation), ADR 059 (configurable overlay name, a join that looks
first), ADR 060 (re-joining the same control plane).

## Context

A worker that used to belong to another cluster keeps that cluster's overlay:
the Docker network `temps0` (with its containers still attached), the bridge
`br-temps0` and the VXLAN device `vxlan-temps0`. ADR 059 lets the new cluster
build `temps1` beside it. It still failed, forever, on a real host:

```
bootstrap failed: vxlan transport error on 'vxlan-temps1': VXLAN device
'vxlan-temps0' already holds vni=42, port=4789 on this host ...
Two clusters' overlays cannot run on one host at once
```

and the console showed the node's subnet moving on every retry
(`Automatically moved from 10.202.5.0/24 to 10.202.6.0/24: 10.202.5.0/24
overlaps host route 10.202.5.0/24 on device 'br-temps1'`).

Three defects combine:

1. **Every cluster uses VNI 42 on UDP 4789.** The control plane stores
   `network_config.vxlan_vni`/`vxlan_port`, but only its own overlay reads
   them; every agent builds `NetworkConfig::default()`, which is 42/4789. The
   kernel allows one VXLAN device per VNI and port, so a second cluster on the
   same host cannot exist, whatever its names.
2. **The node reports its own bridge as someone else's network.** The agent
   collects its occupied ranges (ADR 056) before it polls, with the names of
   the manager it built or, when it has none (after a restart, or when the
   bootstrap failed), `overlay_name::current()`. That is only set after a
   successful reconcile, so it falls back to `temps0`: the node's own
   half-built `br-temps1` is not "owned", its route is reported as occupied,
   and the control plane's auto-remediation moves the allocation. The bridge
   also kept every address it was ever given (`bridge::ensure` only adds), so
   each move left one more route behind to report next time.
3. **The error does not say what to do.** "If 'vxlan-temps0' belongs to an
   overlay this host no longer serves, delete it" leaves the operator to find
   out whether it does, what is attached to it, and how to remove all of it.

The requirement: several clusters' overlays on one host work by default; when
something still blocks it, the message names exactly what and gives the
commands, plus an alternative that does not need a shell on the host.

## Decision

### 1. Each cluster has its own VXLAN network ID

- `network_config.vxlan_vni` is the cluster's VXLAN network ID ("network ID"
  in the console) and `vxlan_port` its UDP port. A CHECK keeps the VNI in
  `1..=16777215` (24 bits).
- **New clusters get a random one** in `4096..=16777215` (never 42, never a
  small value another tool is likely to pick). The `network_config` row is
  seeded by the migration that creates it, so "new" is decided by a later
  migration (`m20261109_000001_cluster_vxlan_network_id`): the seeded 42 is
  replaced by a random VNI only on a brand-new database, one with no user, no
  node and no control-plane overlay allocation, which is the state right
  after a first boot's migrations (no migration creates a user).
- **Existing clusters keep 42.** Any install that already ran keeps its
  network ID, whether or not it ever added a worker, so an upgrade changes
  nothing about a running cluster's network or a self-hosted install that
  never uses this. Only an operator moves a cluster off 42 (§4). The
  migration's `down` puts the cluster back on 42, which binaries older than
  this assume for every agent.
- The probability that two clusters draw the same random VNI is about one in
  sixteen million.
- **The control plane sends it.** The peer list gains
  `vxlan: { vni, port }`. The agent uses what it is sent; a payload without
  the field (a control plane older than this) means 42/4789, exactly as
  before. Agents validate the range and refuse the payload otherwise; the
  port must also be unprivileged (`1024..=65535`), because the mesh firewall
  admits that UDP port from mesh peers.
- **A change is reconciled like any other.** When the VNI or port in the
  payload differs from the one the overlay was built with, the agent rebuilds
  its manager; `vxlan::ensure` sees its own device with another topology and
  replaces it (delete, create, re-enslave, re-populate the FDB). The control
  plane's peer reconciler ends when the stored VNI or port no longer matches
  its overlay, and its setup watcher builds it again on the new one. Peers on
  different VNIs cannot exchange overlay traffic, so cross-node traffic stops
  for at most one agent poll (30 s) during a change. Deployments, which reach
  apps through published ports, are unaffected.

### 2. Mixed versions: report what is followed, gate on it

An agent older than this ignores `vxlan` and stays on 42. Two rules keep that
from silently splitting a cluster:

- **Agents report the VNI they follow.** The heartbeat carries
  `overlay_vxlan_vni`: the VNI of the last peer list the network-sync loop
  acted on (42 when the control plane sent none). It is sent from the first
  poll on, also by a node without an allocation, so a node shows it can
  follow before it needs to. Stored as `nodes.overlay_vxlan_vni` (NULL: never
  reported).
- **A cluster only moves when every node can follow.** Changing the VNI is
  refused while any node has never reported one, naming those nodes and the
  fix (upgrade Temps on it and restart its agent, or remove it). Offline
  nodes count: they apply the new VNI when they come back, if they reported
  one before.
- **An old agent in a cluster that is not on 42 is told so.** A heartbeat
  that reports an overlay but no `overlay_vxlan_vni`, in a cluster whose VNI
  is not 42, is stored as overlay *unavailable* with "this node's agent
  predates per-cluster network IDs: it runs the overlay on network ID 42,
  this cluster uses N; upgrade Temps on it". That covers an old agent joining
  a new cluster (random VNI), which the gate cannot prevent. The scheduler
  already keeps cross-host managed-service members off such a node (ADR 056).

### 3. A blocked network ID names its holder and the fix

When creating the VXLAN device finds another device holding the VNI and
port, the agent (and the control plane, which shares the code) inspects the
holder, best-effort:

- the device, and whether its name follows the overlay naming
  (`vxlan-<name>` → Docker network `<name>`, bridge `br-<name>`, ADR 059);
- the bridge's IPv4 subnets;
- the Docker network's subnets and attached containers (Docker may be
  unreachable from where this runs; then only the device facts are given and
  the message says Docker could not be checked).

The error becomes `NetworkError::VxlanIdInUse` and reads, for the reported
host:

> VXLAN network ID 42 (UDP port 4789) is already used on this host by
> 'vxlan-temps0', so this cluster's overlay ('vxlan-temps1') cannot use it.
> 'vxlan-temps0' belongs to another cluster's overlay (Docker network
> 'temps0', 10.202.0.0/24); containers web-1, worker-1 are still
> attached. If that cluster is gone, remove it: `docker rm -f web-1
> worker-1 && docker network rm temps0 && ip link delete vxlan-temps0 &&
> ip link delete br-temps0`. If it still runs, give this cluster its own
> network ID in Settings → Worker Nodes.

Commands are only printed for names in a shell-safe alphabet, and long
container lists are replaced by `docker ps -aq --filter network=<name>`.
The message travels the existing path (heartbeat `overlay.last_error`, node
overlay status, the console's "Cross-node network unavailable" banner). On
the control plane the variant is reported as an operator-actionable setup
failure, retried with backoff, like a subnet collision.

The console recognises this failure by the fixed phrase the message is built
around (`temps_network::vxlan_holder::reports_id_in_use`), which also
matches the wording agents older than this used; both sides live in this
repository, so the coupling is tested in one place.

### 4. "Give this cluster its own network ID"

The cluster's network ID and port are always shown in Settings → Worker
Nodes → Cluster network, with this action beside them; the banner below
offers it where the failure is reported.

`POST /settings/cluster-network/network-id` (`SettingsWrite` plus a
sensitive-action step-up, audited as `CLUSTER_NETWORK_ID_CHANGED`) assigns a
fresh random VNI under the `network_config` row lock, after the gate in §2.
The cluster-network status reports the VNI, the port, whether it can change
and why not, and per node the VNI it follows and whether its overlay is
blocked by a network ID in use. The Worker Nodes page shows the button in the
"Cross-node network unavailable" banner when a node reports that, with the
refusal reasons in place of the button when the gate does not pass.

### 5. The node owns its own overlay, whatever happened before

- Occupied ranges are collected after the poll, and the devices and Docker
  networks left out of them are those of every overlay this node runs or is
  bringing up: the manager's names, the names in the allocation just polled
  (or restored), and `overlay_name::current()` only when neither is known.
  A failed bootstrap or a restart can no longer turn the node's own bridge
  into a collision.
- The control plane ignores, in a node's occupied ranges, routes on that
  node's own overlay devices and its own Docker network (its override, else
  the cluster name). That protects clusters whose agents predate this.
- `bridge::ensure` removes IPv4 addresses on the overlay bridge other than
  the allocation's, logging each. The bridge is Temps' own (`br-<name>` of
  this overlay), and a Docker network on an old subnet is refused by the
  preflight before the bridge is touched, so nothing still using the old
  address loses it.

### 6. `temps join` and `agent service install` look for it first

The host preflight (ADR 059) lists the host's VXLAN devices and inspects
each like §3. If one other than this node's own (`vxlan-<name it will use>`,
or its previous name on a re-join of the same cluster) holds the VNI and
port this cluster uses, it prints the same message as a warning before the
token is spent. The VNI comes from `GET /internal/nodes/plane-identity`,
which now also returns `vxlan_vni` and `vxlan_port` (not secret: they are on
every overlay packet); from the agent's last network snapshot for a service
install; otherwise 42/4789. It never blocks the join.

## Consequences

- New clusters coexist on one host with any other cluster's overlay, live or
  left behind, without anyone deleting anything.
- Existing clusters stay on 42 until an operator gives them their own network
  ID, which needs every node upgraded; the console says which are not.
- A leftover overlay that blocks the default ID is described with the exact
  containers and commands, both on the node and before a join.
- Still unsupported: two clusters' **agents** on one host at once. The
  firewall table (`inet temps_network`) and `TEMPS_OVERLAY_FORWARD` are still
  one per host and rendered for the overlay the running agent serves.
- Transport `native` is unchanged; the VNI only matters for VXLAN.
