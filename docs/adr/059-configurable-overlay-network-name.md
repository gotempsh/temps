# ADR 059: A configurable overlay network name, and a join that looks first

## Status

Accepted — 2026-10-23

Related: ADR 048 (multi-node overlay), ADR 055 (workers join outbound only),
ADR 056 (a compute pool that avoids collisions).

## Context

Every host in a cluster carries the overlay as three objects with fixed names:
the Docker network `temps0`, the Linux bridge `br-temps0` it is pinned to, and
the VXLAN device `vxlan-temps0` (VNI 42, UDP 4789). The names were constants,
read in a dozen places as `NetworkConfig::default().docker_network_name` or
the literal `"temps0"`.

A host that was a worker of another cluster still has that cluster's `temps0`,
on that cluster's subnet. When it joins a new cluster:

- the agent finds a `temps0` that does not match its new allocation and stops
  with an `InterfaceConflict` whose fix was "remove it", which would cut the
  containers still attached to it;
- `temps join` only found out after it had spent the enrollment token
  (mesh-enroll or register consume it), so a second attempt needed a new one;
- a `temps join` without root got as far as the WireGuard setup, failed with
  `Operation not permitted`, and had already used the token;
- the previous cluster's agent config was silently replaced;
- ports 80/443, held by the previous cluster's agent, made the new agent's
  ingress and internal proxy fail to bind, visible only in its logs.

## Decision

### 1. One name, three host objects

`temps_network::overlay_name` derives the three names from one *overlay
network name*: `<name>`, `br-<name>`, `vxlan-<name>`. A name is 1–9
characters, starts with a lowercase letter and uses only `[a-z0-9_-]`: nine is
what `vxlan-` leaves of Linux's 15-byte interface-name limit, and the narrow
alphabet is safe in nft/iptables scripts and Docker names. The database
enforces the same pattern with CHECK constraints.

Each process that reconciles an overlay records the names it runs with
`overlay_name::set_current`: the agent's network sync and the control plane's
own overlay setup. Everything else in that process (attaching containers,
reading the overlay gateway for DNS, the internal proxy's bridge address,
the deployer's overlay attach, `temps doctor`) reads
`overlay_name::current()` instead of assuming `temps0`. Unset, it is `temps0`,
so single-host installs and old agents behave exactly as before.

### 2. Stored, not configured by environment

- `network_config.docker_network_name` (NOT NULL, default `temps0`): the
  cluster's name. Changed through `PUT /settings/cluster-network`
  (`overlay_network_name`), validated, audited
  (`CLUSTER_OVERLAY_NETWORK_NAME_CHANGED`).
- `nodes.overlay_network_name` (nullable): a node's override, set at
  registration when `temps join` found the cluster's name taken.

The peer list's `alloc` carries the effective name (override, else cluster).
The field is optional both ways: an old agent ignores it and keeps `temps0`;
a new agent reading an old control plane gets no field and keeps `temps0`.

### 3. A taken name gets a sibling, never a deletion

Temps never removes a network, bridge or VXLAN device it did not create for
the overlay it is running. When the host already has a network of the
preferred name that is not this node's, `temps join` picks the first free
name (`temps1`, `temps2`, ...; a network *or* a leftover `br-`/`vxlan-`
device of that name counts as taken) and registers with it. The new overlay
is built beside the old one.

A network counts as this node's own when the saved agent config is for the
same control plane (its URL, or the address `temps join` was given, now saved
as `join_url`) and the network carries the subnet the agent last applied
(its network snapshot). A rejoin of the same cluster therefore keeps its
overlay.

### 4. Renaming a live overlay

The VNI and UDP port are cluster-wide, and the kernel allows one VXLAN device
per VNI/port pair, so an overlay under a new name cannot come up while the
old VXLAN device exists. When the agent's name changes (in-process, or across
a restart according to its last snapshot for the same node), it releases
only its own previous overlay's VXLAN device and the peer routes through its
previous bridge, then builds the new one. The previous bridge and Docker
network stay: containers on them keep local connectivity and lose cross-node
traffic until they are redeployed, and the agent says so. A VXLAN device it
cannot prove is its own (another cluster's, still running) is never touched:
creating the new device fails with an error naming the holder and both
fixes. VNI and port stay cluster-wide; they are not made per node.

The control plane applies a renamed cluster name the next time it sets up its
overlay. It does not release its previous VXLAN device; if that device still
holds the VNI, setup fails with the holder named.

### 5. `temps join` looks before it spends the token

`commands::host_preflight` gathers the host's state (privileges, saved agent
config, Docker networks, interfaces, listening sockets and their processes)
and decides with a pure function, before mesh-enroll or register:

| Finding | `temps join` | `agent service install` |
|---|---|---|
| Linux without root or `CAP_NET_ADMIN` | stops: "run it with sudo"; token untouched | warning (install also requires root) |
| Saved config for another control plane | stops unless `--force` | — (the saved join is the one installed) |
| Overlay name taken by a network not this node's | picks a free name, warns naming the network, its subnet and the choice | warns; renames nothing |
| Port 80/443 (or the fixed ingress ports) held | warns with the address and process | warns, ignoring the agent being replaced |

Relay and pairing joins carry no comparable control-plane address, so the
saved-config check does not run for them.

## Consequences

- A host can serve as a worker of a new cluster without deleting what a
  previous one left; the leftover is reported, not destroyed.
- Operators can rename the overlay, at the cost of moving containers by
  redeploying them.
- The firewall table (`inet temps_network`) and `TEMPS_OVERLAY_FORWARD` stay
  single per host: one agent runs per host, and it renders them for the
  overlay it runs. Running two clusters' agents on one host at once remains
  unsupported.
- The console does not yet show or edit the name; the API does.
