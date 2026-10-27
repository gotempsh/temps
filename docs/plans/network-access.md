# Network access for workloads: open by default, restrictable per workload

**Status:** Draft for review
**Date:** 2026-10-26
**Builds on:** ADR-052 §4 (one rule model for network permissions), ADR-055 §9 (node enforcement), ADR-051 (sandbox access to linked services)
**Target:** a branch after #1632 merges

## 1. What we want

1. **Open by default.** A project's workloads (deployments, services, functions, jobs) reach every internal Temps service without configuration, as today.
2. **Restrict a workload when the user asks.** Any one workload can be narrowed to an explicit list:
   - specific managed databases or storage;
   - specific other workloads running in Temps;
   - CIDR blocks, optionally with ports and protocol.

   This works over TCP (any port) and HTTP.
3. **Restrict who can connect to a workload.** A workload can also be narrowed on the receiving side: "only `api` may reach `postgres-main`" (§4.14).
4. **Sandboxes:**
   - internet access on/off, on by default;
   - access to selected Temps services, whichever node the sandbox and the service run on.

   The two settings are independent. Internet off with database access on must work.
5. **Chosen at creation, the same way everywhere.** Network access is part of creating a workload: a project and its environments, a managed service, a sandbox, a function, a job. One field in every create request, one shared form section, and editable afterwards from the same card (§4.6).
6. **Stable.** A failure never silently opens a restricted workload, and never breaks an open one.
7. **Explained.** When something is not enforced, fails, or blocks traffic, the console says what happened, on which node, and how to fix it.

## 2. What #1632 already has

| Piece | Where | State |
|---|---|---|
| Rule model, keyed by workload identity: `subject_kind` (deployment, sandbox, service, function, job) × `peer_kind` (service, workload, cidr, host), with ports, protocol, allow/deny and origin | `crates/temps-entities/src/network_policy_rules.rs`, migration `m20261022_000007_network_policy.rs` | Built |
| Per-workload egress/ingress profile: `open`, `restricted`, `none` | `network_policy_profiles` | Built |
| Linking a service to a project or sandbox creates the allow rule (database triggers, `origin = link`) | same migration | Built |
| Compile rules into one nftables transaction; a workload's addresses are a set keyed by identity | `crates/temps-workload-netpol/src/nft.rs`, `identity.rs` | Built |
| Node enforcer loop with `legacy` → `shadow` → `enforce`; applies only when the transaction changed; leaving `enforce` removes the tables | `crates/temps-workload-netpol/src/enforcer.rs` | Built |
| DNS follows policy: the per-node resolver answers a restricted workload only for allowed names | `crates/temps-workload-netpol/src/dns.rs` (`DnsPolicy`) | Built |
| Sandbox HTTP egress proxy with host/CIDR allow-list; private, loopback and metadata addresses always refused | `crates/temps-workload/src/egress.rs`, `crates/temps-workload-netpol/src/egress.rs` | Built |
| Scoped credentials per sandbox (own database and role, never the admin password) for Postgres, MariaDB, MongoDB, Redis, S3 | `crates/temps-providers/src/sandbox_*.rs` | Built, control plane only |
| Settings → Network policy page, plus a per-node mode switch | `web/src/pages/settings/NetworkPolicyPage.tsx`, `web/src/components/nodes/NodeWorkloadSettings.tsx` | Built |

## 3. Gaps

1. **Workers enforce nothing.** Only node 0 runs an enforcer (`node_supports_enforcement` in `crates/temps-network-policy/src/service.rs`). On a worker, "restricted" is stored but has no effect, and the console says so only on the node page.
2. **Pass failures are invisible.** A failed enforcer pass is a `warn!` log line (`enforcer.rs`, in `run`). Nothing records it, so the console cannot show it.
3. **Kernel drift goes unnoticed.** The enforcer compares the new transaction with the last one it compiled, not with what is in the kernel. A Docker restart or firewall reload that flushes the table leaves the node unenforced until the policy happens to change.
4. **Sandboxes reach only control-plane services.** `require_local` (`sandbox_engines.rs`) refuses worker-hosted services. The Postgres cluster path only counts members on node 0. A sandbox on a worker cannot reach anything internal.
5. **Internet on/off is not a clear control.** The sandbox egress policy is `internet | restricted`; "off" means Restricted with an empty list.
6. **Controls live in one global page.** Restricting a workload means going to Settings, picking the subject, and adding raw rules. There is no "Network access" control on the project, environment, service or sandbox page.
7. **No feedback on blocked traffic.** When a rule blocks a connection, the user sees a timeout in their app and nothing in Temps.

## 4. Design

> **Datapath decided: eBPF, Linux only (§4.15).** Sections 4.1–4.14 describe behaviour and were first written against the nftables enforcer in #1632. Where they say "nftables set", "chain" or "transaction", read the eBPF equivalent in the §4.15 mapping table; the behaviour, guarantees and tests stay the same.

### 4.1 Semantics

One profile per workload decides internal reach. One flag decides the internet.

| Profile | Internal destinations | Internet | Default for |
|---|---|---|---|
| `open` | everything | yes | deployments, services, functions, jobs |
| `restricted` | only what its rules and links allow | per the `internet` flag | sandboxes |
| `none` | nothing | no | (opt-in) |

- **New column:** `network_policy_profiles.internet BOOLEAN NOT NULL DEFAULT true`. It is used only when the profile is `restricted`.
- **Always allowed under every profile:** the node's DNS resolver, the workload's own ingress, and system peers (the egress proxy, the preview gateway). These are `origin = system` rules, never shown as editable.
- **Always refused, even under `open`:** metadata addresses (169.254.169.254 and friends). This keeps the existing metadata table's priority.
- **Sandbox migration:** the existing sandbox egress policy becomes `internet = true` (Internet) or `internet = false` plus `host` rules (Restricted). The data migration maps rows 1:1, and the old fields stay readable until it has run.

### 4.2 What a rule can point at

| User intent | Rule | Enforced by |
|---|---|---|
| "this database" | `peer_kind = service`, service id | eBPF. Every member's address maps to the service's identity in `ipcache`, refreshed on each change, so a cluster stays reachable after a failover |
| "that other app / function / job" (TCP or HTTP on its port) | `peer_kind = workload`, `deployment/<env>`, `function/<id>`, … plus an optional port | eBPF |
| "this CIDR" | `peer_kind = cidr`, plus optional ports and protocol | eBPF (one `ipcache` identity per distinct prefix) |
| "this external hostname" | `peer_kind = host`, a pattern | sandboxes: the egress proxy. Other workloads: addresses learned by the node resolver (§4.13) |

HTTP to a Temps service needs no layer-7 matching. An allow on the workload's HTTP port is the same thing at layer 4. `host` rules exist for external names, where addresses are not known in advance.

**Peer address sets.** For a `service` peer the set holds:

- each member's overlay address, which is what Cluster DNS (`<svc>.temps.local`, `primary.<svc>.temps.local`) resolves to;
- the worker data address plus published port, for paths that dial the published port.

Both are rebuilt every pass from `service_members` and `nodes`, the same sources `control_plane_member_dial_host` uses.

### 4.3 Enforcement on workers (gap 1)

Each agent runs the same `NodeEnforcer`, fed by a new `AgentPolicySource`:

- **Delivery:** `GET /agent/v1/network-policy` on the control plane, over the agent's existing mTLS identity. It returns the node's mode plus a snapshot limited to:
  - the node's own workloads' policies;
  - the address sets of the peers those policies name, which may live on other nodes.

  A worker never receives the whole cluster's rule table.
- **Change propagation: push first, poll as a safety net.** Address churn is handled by propagating changes fast, not by keeping addresses stable (§4.8, and §9 on how Calico and Cilium do the same).
  - The control plane sends a `network_policy` hint on the existing desired-state stream whenever anything in a node's snapshot changes: a rule, a profile, or a peer's addresses. Peer addresses change when an instance starts, stops or moves.
  - The agent then fetches the snapshot at once.
  - The response carries a revision and digest. The agent sends `If-None-Match` and gets `304` when nothing changed.
  - A 15 s poll remains for missed hints.
  - Target: a change on one node is applied on every affected node within 2 s.
- **Memory bound:** O(local workloads + their peers), not O(cluster).
- **Applying:** the eBPF executor (§4.15) on Linux workers. A worker that fails a requirement check (cgroup v2, BTF, capabilities, the systemd cgroup driver) reports `unavailable` with the reason and fix (§5).
- **Cross-node traffic:** filtered on the **source** node by the `connect` hook, by destination identity. The destination node needs no rule while the destination's ingress is `open`; when it is restricted, the destination node filters too (§4.14).

### 4.4 Sandboxes on any node (gap 4)

ADR-052 says a sandbox joins the overlay as a restricted workload. With §4.3 that becomes possible:

1. **Attach.** A sandbox with at least one internal allow rule is attached to the node's overlay network as a second interface. It is attached only when the node is in `enforce` and its last pass succeeded. The nftables egress chain for the sandbox's addresses allows exactly its peer sets and drops everything else on the overlay.
2. **Internet** keeps going through the per-sandbox egress proxy on the internal sandbox network, unchanged. `internet = false` makes the proxy refuse everything except `host` rules.
3. **DNS:** the sandbox resolves through the node resolver under `DnsPolicy`. Allowed services resolve; everything else gets NXDOMAIN.
4. **Credentials** stay scoped as today:
   - Drop `require_local`. The control plane provisions the sandbox's database and role by dialling the worker data address, which works since the fixes in #1632 (`worker_data_address`, `control_plane_member_dial_host`).
   - `POSTGRES_HOST` becomes `primary.<svc>.temps.local` for clusters, which the role reconciler keeps on the current primary, and `<svc>.temps.local` for single instances.
5. **Fail closed:** if the node cannot enforce, the link is refused with a typed error naming the node and the fix. It never degrades to "connected but unrestricted".

**Alternative considered: TCP forwarding through the egress proxy.** It works without nftables, but needs a second policy engine, has to re-implement failover following, and gives a different behaviour from deployments. We keep it out unless a backend (microVM) cannot attach to the overlay. That backend already maps policy through `microsandbox.rs`.

### 4.5 Status and errors as data (gaps 2, 7)

**New table `network_policy_node_status`,** one row per node, written by every pass:

| Column | Meaning |
|---|---|
| `node_id`, `mode` | the mode in force |
| `state` | `legacy`, `shadow_ok`, `enforced`, `failed`, `unavailable` |
| `last_pass_at`, `last_applied_at`, `applied_digest` | freshness |
| `last_error`, `last_error_at`, `consecutive_failures` | the most recent failure |
| `unavailable_reason` | e.g. "the host uses cgroup v1" |
| `shadow_would_block` | JSONB, at most 50 entries: subject → peer, port, count. Only in shadow |
| `blocked_counts` | JSONB per restricted workload: drops in the last pass window, from the eBPF `counters` map (§4.15) |
| `blocked_samples` | JSONB, the last 50 blocked flows on the node: time, source identity, destination address, port, protocol, and the Temps service or workload that owns the destination when known |

- **Write cost:** one upsert per node per pass, and only when something in the row changed.
- **Workers:** they report the row through the existing heartbeat. No new connection.
- **Blocked traffic is counted, and sampled at a fixed rate.** Nothing is logged per packet (hot-path rule):
  - the counters live in a per-CPU eBPF map and the agent reads them once per pass;
  - the programs write blocked-flow samples to a ring buffer, rate-limited in the program to 5 per second per node; the agent keeps the latest 50;
  - at saturation, samples beyond the rate are dropped and counted, so memory stays constant.

  This is the bounded version of Cilium's Hubble flow records (§9): enough to say "api tried to reach 10.0.5.4:6379, which is redis-cache".
- **Status for the console:** `GET /workloads/{kind}/{id}/network-access` returns a typed status:

```rust
pub struct NetworkAccessStatus {
    pub profile: Profile,                 // open | restricted | none
    pub internet: bool,
    pub enforced: bool,                   // true only if the node's last pass applied it
    pub node: NodeRef,
    pub reason: Option<String>,           // why not enforced, when `enforced` is false
    pub setup_path: Option<String>,       // console path that fixes it
    pub last_pass_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub blocked_last_hour: Vec<BlockedPeer>, // peer, port, count
    pub rules: Vec<RuleView>,             // including link rules and dangling ones
}
```

### 4.6 Set at creation, the same way for every kind

Network access is part of the workload spec, so it is chosen when the workload is created and goes through the same admission as everything else.

- **The spec.** `NetworkSpec` (`crates/temps-workload/src/spec.rs`) already carries `egress: Option<EgressProfile>`. It gains two fields:
  - `internet: Option<bool>`, the §4.1 flag (`None` takes the kind's default);
  - `allow: Vec<NetworkAllow>`, one entry per allowed peer: a service id, a `WorkloadRef` plus an optional port, a CIDR plus optional ports and protocol, or (sandboxes only) a host pattern.
- **One request shape.** Every product create request carries the same optional `network_access` object, mapped onto `NetworkSpec`:
  - project create, which applies to every environment it creates;
  - environment create, which can override its project's default;
  - managed service create;
  - sandbox create (`CreateSandboxRequest`), whether standalone or inside a project;
  - functions and jobs, including a `network:` block in `.temps.yaml`.
- **Admission** (`temps-workload-plane`) fills in the kind's default and validates:
  - **Defaults:** `open` for deployments, services, functions and jobs. For sandboxes, `restricted` with `internet = true`, plus the services the project links.
  - **Validation:** a `host` entry on a non-sandbox workload, a peer that does not exist, or a profile the chosen runtime cannot enforce each fail the create with a typed 400 naming the field. The workload is not created half-configured.
- **Persistence.** The profile row and the `origin = operator` rules are written in the same transaction as the workload's own row. A create that fails leaves no orphan rules.
- **Project default, environment override.** A deployment's subject is its environment. When an environment has no profile row, it reads its project's row (new subject kind `project`). With no project row either, it gets the kind default. Changing the project's setting therefore changes every environment that has not overridden it, and the card says which environments override it.
- **One form section.** A shared `NetworkAccessFields` component is used in New project, environment create, Create service and Create sandbox. It shows "Open (default)", Restricted or None; the Internet switch; and the allowed list. After creation, the same component is the body of the "Network access" card (§7).
- **Runtimes that cannot enforce.** The WASM driver runs guests in-process with WASI preview 1, which has no sockets, so it is effectively `none`; admission records that instead of accepting `open` it cannot provide. The microVM driver maps the policy through `microsandbox.rs`; a rule it cannot express fails the create.

### 4.7 Where the code goes

| Crate | Change |
|---|---|
| `temps-workload` | `NetworkSpec.internet`, `NetworkSpec.allow`, the `NetworkAllow` type (§4.6) |
| `temps-netpol-ebpf` (new, `no_std`) | the eBPF programs: cgroup socket and skb hooks for containers, TC programs for Firecracker TAPs (§4.15) |
| `temps-workload-plane` | admission defaults and validation per kind; profile and rules written with the workload; the scheduler places restricted workloads only on enforcing nodes |
| `temps-workload-netpol` | the `aya` loader and executor behind `choose_executor`: pinned maps and links, the double-buffered policy swap, the `bpf_link` and map-digest drift check, counters and samples (§4.5), `temps netpol boot` (§4.15), passes triggered by Docker events, the Firecracker TAP attachment (§4.12), the per-node source-subnet check (§4.15), pass outcomes exposed for status. The nftables enforcer is deleted after phase 2 (§11) |
| `temps-network-policy` | node-scoped snapshot endpoint for agents and the `network_policy` stream hint (§4.3), precedence (§4.9), per-workload trial mode (§4.10), the `network_policy_node_status` table, `GET /workloads/{kind}/{id}/network-access`, the shadow → enforce gate, the `project` subject with inheritance |
| `temps-workload-node` / `temps-agent` | `AgentPolicySource` and the enforcer loop on workers; status and the applied snapshot revision reported in the heartbeat; reacting to the desired-state stream's policy hint; the node-local address allocator with cool-down and its state file (§4.8, gap 4); `temps join` installs the `temps netpol boot` unit on enforcing nodes |
| `temps-netpol-probe` | the node resolver applies `DnsPolicy` to sandboxes on the overlay and learns addresses for `host` rules (§4.13); the egress proxy honours `internet = false`; the conformance suite (§4.12) |
| `temps-workload-docker` | `container_create_body` places every container under its workload slice (`cgroup_parent`, §4.15), sets its Temps-assigned address (`EndpointIpamConfig.ipv4_address`, §4.8) and attaches a sandbox to the overlay as a second interface, only when its node enforces (§4.4); Temps networks created from now on get Docker's `IPRange` fence (§4.8) |
| `temps-workload-env` | sandbox service variables use `primary.<svc>.temps.local` for clusters and `<svc>.temps.local` for single instances |
| `temps-providers` (`sandbox_*.rs`) | remove `require_local`; provision through the worker data address |
| `temps-agents` (`sandbox/firecracker.rs`) | `restricted` uses the rule model on the VM's TAP instead of falling back to no network (§4.12) |
| `temps-workload-ingress` | the HTTP ingress (the proxy) is a `system` source for workloads with restricted ingress (§4.14); its own endpoint and access policies are unchanged |
| `temps-workload-activator`, `temps-workload-volumes`, `temps-workload-wasm` | unchanged, apart from WASM reporting `none` at admission (§4.6) |
| `web` | `NetworkAccessFields`, the "Network access" card, the node enforcement flow, the §5 messages |

### 4.8 Restarts, reboots and changing addresses

**What changes when a machine restarts:**

| Thing | Survives a reboot? | Notes |
|---|---|---|
| nftables tables | No | Kernel state. Temps does not rely on the distro's `nftables.service` |
| Container IP addresses | Usually the same, not guaranteed | Docker re-requests the previous address when a container starts, but restart order is not fixed, so another container can take it first. A recreated container (redeploy, failover) almost always gets a new one |
| veth interface names | No, always new | The anti-spoofing bindings (`binding.rs`) are keyed by them |
| Container start | Before the agent | Containers with a restart policy start as soon as Docker does, possibly seconds before the agent's first pass |

**Why rules survive address changes in steady state.** Rules never contain addresses. They name identities (`sandbox/7`, `deployment/12`, `service/3`), and each identity is an nftables set of its current addresses. The enforcer identifies containers from their plane labels on every pass and rewrites the sets. A container that comes back with a new IP is in the right set on the next pass, and its old IP leaves the set at the same moment, so an address reused by another workload is judged by the new owner's rules, never the old one's.

**Three gaps this design closes:**

1. **The window before a pass.** Waiting up to 15 s for the next pass is too long for a container that just started.
   - The enforcer subscribes to Docker events (container `start` and `die`, network `connect` and `disconnect`) and runs a pass immediately, aiming for under a second.
   - The 15 s loop stays as the safety net for missed events.
2. **The window at boot.** Persisting the last full ruleset and reloading it at boot is **wrong**: its sets hold the old addresses and its bindings name veths that no longer exist, so it could judge a workload by another one's rules. Instead:
   - A **base table with no addresses or interface names** is loaded at boot by a systemd oneshot ordered `Before=docker.service` (`temps-netpol-base.service`, installed by `temps join` on nodes in `enforce`).
   - It holds one rule: traffic entering a Temps-managed bridge from a container port is dropped unless its source is in `@identified`, which starts empty. DNS to the node resolver is allowed.
   - The agent's first pass fills `@identified` and the real chains in one transaction.
   - Effect: on an enforcing node, workloads, open ones included, have no outbound traffic for the seconds between Docker starting them and the agent's first pass. Nothing is ever open that should be restricted.
   - Without systemd, the agent loads the base table first thing at startup, and the node page states that the window exists.
   - Nodes in `legacy` or `shadow` install nothing, so their boot is unchanged.
3. **Address churn on the overlay.** Other nodes' rules hold overlay addresses for cross-node peers, so a peer that restarts with a new address leaves those sets stale until they are updated.
   - Handled by push propagation (§4.3): the peer's node reports the new address, the control plane hints every node whose snapshot names that peer, and they update within about 2 s.
   - **The residual risk is on the open side, and bounded.** Until the update lands, a restricted workload elsewhere still allows the peer's *old* address. If another workload on the peer's node picks up that address within the window, the restricted workload could reach it for up to about 2 s.
   - Mitigations:
     - removals propagate before additions: when an instance stops, its node reports the address gone immediately (Docker `die` event), so the old address normally leaves every set before Docker can hand it to anyone else;
     - ~~Docker's default IPAM hands out addresses serially~~ **disproved by the prototype (§4.16, case 13):** Docker reuses a freed address at once. Temps networks therefore delay reuse (serial IPAM, or a node-local allocator with a 5-minute cool-down);
     - a destination that is itself restricted for ingress would also refuse the connection (§4.14).
   - **Test:** the e2e test below asserts that the old address is gone from every node's sets within the target.
   - **Considered, not chosen: leasing overlay addresses** (identity → IP, containers created with a fixed `EndpointIpamConfig.ipv4_address`). It would keep addresses stable across restarts, but it adds a lease table, reaping and IPAM conflicts with Docker. Neither Kubernetes plugin does it; pod IPs change on every recreate there too. We revisit it only if propagation cannot meet the 2 s target.

**4. Address reuse (found by the prototype, §4.16).** Docker hands a freed address to the next container that asks, at once. The prototype showed a restricted workload admitting a newcomer as the address's previous owner because its `ipcache` entry was still there. Every Kubernetes plugin avoids this by owning address allocation, so an address is only freed after its policy entry is gone (§9). Temps does the same on its own networks:

- **Temps assigns the addresses.** Each node already owns a /24 of the compute pool (`network_config.subnet_prefix_len`). The agent picks every Temps container's address on Temps networks from that subnet and passes it in `EndpointIpamConfig.ipv4_address`. Compose services get the same, injected per service.
- **A container only gets an address Docker picked if it joined without one.** That happens in two ways, and neither needs an outage to handle:
  1. *A Temps code path that forgot to pass an address.* That is our bug. A test asserts that every container create and network connect onto a Temps network carries a Temps-assigned address.
  2. *Someone running `docker run --network <temps network>` by hand.* That needs Docker access on the node, which is already root: such a person can bypass any policy. Handling it is defense in depth, not the guarantee.
- **The agent disconnects containers Temps did not create.** It watches Docker's network `connect` events. A container on a Temps network that Temps did not create is disconnected at once and listed on the node page: "container X joined the Temps network outside Temps and was disconnected; it cannot be identified." It never gets an identity.
- **Docker's own picks are fenced off on networks created from now on.** A new Temps network gets an `IPRange` covering the top eighth of the node's subnet, which Temps never assigns (layout in §4.15, "Settings").
  - Docker accepts `IPRange` only when a network is created. Adding it to an existing network means recreating the network, and every container on it loses its overlay connection for a second or two.
  - **Existing networks are therefore left alone.** Enabling enforcement never forces a network outage.
  - The fence is added the next time the network is recreated anyway: an MTU change already goes through `recreate_for_new_mtu` (`crates/temps-network/src/docker.rs`), which reattaches every container with its address and aliases.
  - The node page also offers it as an explicit action, "Add the address fence", which states the brief overlay outage before it runs.
- **Sized like Kubernetes: a /24 per node and at most 110 containers on it.** Kubernetes gives each node a /24 (`--node-cidr-mask-size`, default 24) and caps it at 110 pods (kubelet `--max-pods`, default 110). GKE states the rule behind it: each node gets the smallest range holding **twice** its maximum pods, so addresses are not reused straight away as pods come and go (§9). Temps copies both defaults:
  - the per-node subnet stays the cluster-wide `network_config.subnet_prefix_len`, default 24, as today, so existing clusters change nothing;
  - a new per-node `max_workloads` (default 110) caps the Temps containers on a node, sandboxes included. The scheduler treats it as capacity and skips a full node. When no node has room, a create fails with a typed error naming every candidate node and its limit, the equivalent of a Kubernetes pod staying `Pending` with a reason;
  - `max_workloads` is refused when twice its value does not fit in the node's Temps-assigned range. At /24 that range holds 222 addresses, so 110 fits and 111 does not. Raising the limit needs a larger per-node subnet, set before nodes join, as in Kubernetes.
- **Round-robin, oldest freed first.** The allocator moves past the last address it gave out instead of reusing the lowest free one, like Kubernetes' standard `host-local` allocator. When it has to reuse, it takes the address freed longest ago.
- **Acknowledgement is the hard rule; time is extra margin.** A freed address is never reused until every node whose snapshot held it has acknowledged a revision without it (the heartbeat carries the applied snapshot revision). The allocator also prefers addresses freed at least `address_cooldown_secs` ago (default 300 s). If churn leaves nothing that old, it takes the oldest acknowledged address rather than failing a start. The AWS VPC CNI uses a 30 s cool-down for the same reason (§9). With the 2× headroom, a node at its 110-container limit can replace up to 110 containers in 5 minutes before the time preference ever gives way.
- **Freed on remove, not on stop.** A stopped container keeps its address, so a restart comes back on the same one. The address enters the cool-down when the container is removed (redeploy, scale-down, failover).
- **The state survives restarts.** Assignments and cool-downs are written to the agent's state directory and checked against `docker inspect` at start, so neither an agent restart nor a reboot can hand out an address early.
- **The stop event stays the fast path.** The agent still deletes a removed container's `ipcache` entry the moment Docker reports it, before anything else. The allocator is the guarantee; the event is the speed.
- **Cross-node:** a peer's address can only change owner on its own node and only after the cool-down, which is far longer than the 2 s propagation target. The one remaining hole is the one Cilium has in native routing: a receiving node that has heard nothing for longer than the cool-down (§4.15, "Identity across nodes").

**What the user sees.** The node page shows "Rules applied 2 s after boot" and the time of the last pass. If the agent does not complete a pass within 60 s of boot, the node page and every restricted workload's card show: "worker-2 restarted and has not applied network policy yet; workloads on it have no outbound traffic." The offered action is to check the agent; the base table guarantees the safe direction meanwhile.

**Tests:**

- unit: a container that returns with a new IP moves sets in one pass, and its old IP is gone from every set;
- unit: the base table contains no address and no interface name;
- e2e: reboot a worker (restart its container in the dev cluster) and assert that no restricted workload completes a connection to a disallowed peer between Docker start and the agent's first pass;
- e2e: restart a peer on worker-2 so it gets a new address; a restricted workload on worker-1 that is allowed to reach it reconnects within 2 s, and the old address is in no set afterwards;
- unit: the allocator never returns an address inside its cool-down or before every holder acknowledged its removal, and moves round-robin;
- unit: allocator state restored after a restart matches `docker inspect`, and an address in use by an unknown container is never assigned;
- conformance (case 13): a container outside Temps on a Temps network is refused by a restricted workload, and a removed workload's address is never admitted as its old identity;
- e2e: a start/stop churn loop on worker-2 for 10 minutes; no restricted workload anywhere admits a connection under a previous owner's identity.

### 4.9 Precedence

Rules from different origins are evaluated in a fixed order, so an operator guardrail cannot be undone by a project owner. This is what Calico's tiers and Kubernetes' `AdminNetworkPolicy` exist for (§9).

1. `system` rules: DNS to the node resolver, the workload's own egress proxy and preview gateway, the host's health checks. Always allowed.
2. The metadata block: always refused.
3. `operator` **deny** rules written by an instance admin (for example, "nothing reaches `10.0.0.0/8` except what is listed below").
4. `link` and `operator` **allow** rules.
5. The profile's default: everything for `open`, internal nothing (plus the internet if `internet` is on) for `restricted`, nothing for `none`.

- **Who writes what:** project owners can write allow rules for their own workloads, and deny rules that only narrow them. Deny rules that apply across projects need instance admin.
- **Explanation on the card:** a blocked or allowed destination names the step that decided it ("blocked by the instance rule 'no internal ranges'").
- **Test:** an operator deny beats a link allow for the same peer, on every backend (§4.12).

### 4.10 Trying a restriction on one workload

Per-node shadow mode (§6) is for rolling out enforcement. Users need to try a restriction on **one** workload before committing to it, as with Calico's staged policies and Cilium's audit mode (§9).

- **Trial state:** the profile gains a `trial` flag. A workload in `restricted` + `trial` keeps working as `open`. The enforcer compiles its restricted rules as "would block" counters and samples instead of drops.
- **Card:** "Trying restricted access: in the last hour this app would have been blocked from 2 destinations", with the list and an "Allow this" button for each, then "Enforce now".
- **No timer:** a trial never turns itself into enforcement; the user decides.
- **Tests:** a workload in trial is never blocked; its would-block samples match what enforcing would drop, compared on the same traffic in the dev cluster.

### 4.11 Failsafe exceptions

No rule, profile, bug or boot base table may cut the node off from being managed. Calico calls these failsafe ports (§9).

- **Never matched by any Temps chain:** the node's own traffic (only traffic entering from container ports is ever filtered), SSH, the agent's mTLS API, WireGuard, and the node resolver.
- **Enforced in code:** the compiler rejects a ruleset that would match those ports or the node's own addresses, with a typed error, and the unit tests assert every chain is scoped to container ports.
- **Recovery:** if the agent is broken, `temps agent netpol reset` removes Temps' tables by hand, and the node page documents it.

### 4.12 Backends: one rule model, one compiler each, one conformance suite

The host sees each backend's traffic in a different place, so no single kernel hook (nftables or eBPF) covers them all. Consistency comes from our layer: the rule model, one compiler per backend with the same semantics, and one test suite all of them must pass.

| Backend | Where the host sees its traffic | Compiler | State |
|---|---|---|---|
| Docker container | a veth on a Docker bridge; the processes are host processes | eBPF cgroup programs on `temps.slice` (§4.15) | prototyped (§4.16); replaces the built nftables compiler in phase 2 |
| Firecracker microVM | a TAP device; the guest runs its own kernel, so its sockets are not on the host | eBPF TC programs on the TAP, identity by ifindex (§4.15) | **to build**: today `restricted` falls back to no network (`sandbox/firecracker.rs`) |
| microsandbox (libkrun) | no interface: the guest's traffic goes through a user-space network stack inside the VMM, which opens ordinary host sockets | the SDK's own network policy (`microsandbox.rs`), enforced inside that stack | built |
| WASM (wasmtime) | none: WASI preview 1 has no sockets | none needed; admission records `none` (§4.6) | built |

**Shared semantics:** denies, then allows, then the profile default (§4.9); the host is always allowed in; DNS handled so a restricted workload resolves exactly its allowed names. Each compiler documents how it maps these; `microsandbox.rs` already does.

**Conformance suite** (in `temps-netpol-probe`, extending its two-node tests). The same policy is applied to a workload on each backend, and each must give an identical reachability matrix:

- the linked peer is reachable, and an unlinked one is blocked;
- an allowed CIDR is reachable, and the same CIDR on another port is blocked;
- an operator deny beats a link allow;
- metadata is blocked;
- allowed names resolve and others do not;
- internet on and off behave the same;
- after a restart with a new address, the same matrix holds within 2 s;
- a reused address is never admitted as its previous owner (case 13 of §4.16);
- a workload with no identity entry is denied, and a host process is never affected;
- shadow and trial count without blocking.

A backend that fails a case cannot accept that kind of rule: admission refuses it with a typed error naming the backend and the rule (§4.6). This is how "consistent" is guaranteed rather than assumed.

### 4.13 Hostname rules for workloads other than sandboxes

Sandboxes enforce `host` rules in their HTTP egress proxy. For deployments, functions and jobs, putting a proxy in front of all their traffic is a bigger change (ADR-052 deferred it). Cilium's `toFQDNs` shows a cheaper way (§9):

- the node resolver already answers the workload's queries;
- when a query matches one of the workload's `host` rules, the resolver adds the answered addresses to `ipcache` under that rule's identity, with the record's TTL (clamped to between 30 s and 1 h);
- entries expire on their own, and the set is bounded per workload (at most 256 addresses; beyond that the oldest leave first, counted and shown);
- a workload that bypasses the resolver (a hard-coded IP) gets no access through a `host` rule, which is the safe direction.

Decided: built this way, without a proxy (§11). It is its own phase (§8, phase 7) because it puts the resolver in the decision path for those workloads.

### 4.14 Ingress rules: who may connect to a workload

Decided in review: in scope for this plan. A workload's **ingress** profile (the `ingress` column of `network_policy_profiles`, already in the schema) can be `restricted`. Then only listed sources and system sources may open connections to it.

**Semantics:**

- `open` (default for every kind): anyone the network lets through, as today.
- `restricted`: only:
  - `system` sources: the node's own HTTP ingress (the proxy) for workloads it routes to; the control plane's address for managed services (health checks, the data browser, metrics, backups); the host's health probes;
  - `link` sources: linking `api` to `postgres-main` writes the egress allow on `api` **and** the ingress allow on `postgres-main`, in the same row-trigger transaction, so "connected" and "permitted" stay one decision on both ends;
  - `operator` allow rules naming a source workload, service or CIDR, with optional ports.
- `none`: only `system` sources.
- Precedence (§4.9) applies on this side too: system, then operator denies, then allows, then the profile default.

**Enforcement on the receiving node:**

- A chain on traffic forwarded **to** the restricted workload's addresses, accepting `ct state established,related` first, then the allowed source sets, and dropping the rest.
- Local sources are trustworthy because of the anti-spoofing bindings (`binding.rs`).
- **Remote sources are trustworthy only with the WireGuard mesh.** WireGuard's cryptokey routing only accepts packets from a peer whose source address is inside that peer's allowed range, so a remote node cannot claim another node's workload address. Without the mesh, overlay source addresses can be forged on the underlay. A restricted ingress rule naming a remote source is then reported as **not enforced: needs the WireGuard mesh** (§5), never silently trusted.
- **Published ports keep the real source.** Remote traffic to a published port goes through Docker's DNAT, which keeps the source address. Docker's user-land proxy only handles loopback and hairpin traffic, and those sources are `system` or local anyway.
- Remote source sets come from the same node-scoped snapshot and push hints as egress (§4.3).

**Backends:** Docker and Firecracker use the receiving chain on the veth or TAP. microsandbox's SDK policy has ingress rules (`microsandbox.rs` already keeps the host allowed in ahead of every ingress deny). The conformance suite (§4.12) gains the ingress cases.

**What the user sees:** a "Who can connect" section on the same card: "Open: anything on the network" (the default), or "Only these", with the linked services filled in. Each is marked when it can't be enforced, with the reason (for example, the mesh is off).

**Tests:**

- unit: a link writes both the egress and the ingress allow, and removing it removes both;
- unit: a remote source is reported unenforced without the mesh;
- e2e: with ingress restricted on a database on worker-2, a linked app on worker-1 connects while an unlinked app on worker-3 is refused;
- e2e: the data browser and health checks keep working through the system rule;
- conformance: the ingress matrix on every backend.

### 4.15 Datapath: eBPF on Linux

**Decided in review:** network policy is enforced by eBPF programs, on Linux nodes only. The rule model, profiles, precedence, API and console above do not change; this section replaces the nftables datapath underneath them.

#### Containers: cgroup hooks, one decision per connection

- **Placement.** Every Temps container is created under a per-workload systemd slice: `temps.slice/temps-<kind>-<id>.slice/docker-<container>.scope`, through `HostConfig.cgroup_parent` in `container_create_body`. The agent creates the workload's slice **before** starting its container.
- **Identity comes from the cgroup, not the IP.** The programs read the ancestor cgroup id at the workload-slice level (`bpf_get_current_ancestor_cgroup_id` / `bpf_skb_ancestor_cgroup_id`) and look it up in `cgroup_identity`. Because the slice exists before the container starts, its entry is in the map before the first packet:
  - **no start-up race**;
  - **IP changes do not matter for local workloads**;
  - **spoofing a local source is impossible.**
- **Hooks, all attached once to `temps.slice` (multi-attach applies to every descendant, including containers created later):**
  - `cgroup/connect4` and `connect6`: TCP connects and connected UDP. The verdict is made once per connection. A denied connect returns `EPERM`, so the app gets "Operation not permitted" at once instead of a timeout.
  - `cgroup/sendmsg4` and `sendmsg6`: unconnected UDP. DNS to the node resolver is a `system` allow.
  - `cgroup_skb/ingress`: per packet, with a fast path. Packets for the workload's own established sockets pass (checked through the socket: `bpf_sk_fullsock` and `bpf_tcp_sock` state). Only new inbound connections to a listening socket are judged against the ingress rules (§4.14), with the source identity looked up by address.
  - `cgroup_skb/egress`: a backstop that drops anything from a restricted workload that did not go through the socket hooks (for example, raw sockets; Temps already drops `NET_RAW`).
- **Failsafe by construction (§4.11).** The programs only see processes under `temps.slice`. sshd, the agent, WireGuard and every other host process are outside it, so no rule or bug can cut the node off.

#### MicroVMs and microsandbox

- **Firecracker:** TC programs (TCX on kernels ≥ 6.6, `clsact` otherwise) on the VM's TAP, per packet, with the identity looked up by ifindex. Established traffic is checked against the kernel's conntrack through the `bpf_skb_ct_lookup` kfunc (≥ 5.19).
- **microsandbox:** unchanged. Its own SDK policy (`microsandbox.rs`), since its traffic never crosses a host interface.

#### Maps

Pinned under `/sys/fs/bpf/temps/netpol/`:

| Map | Type | Key → value |
|---|---|---|
| `cgroup_identity` | hash | workload slice cgroup id → identity, egress and ingress profile, `internet`, `trial` |
| `ipcache_v4`, `ipcache_v6` | LPM trie | address prefix → identity, for: remote workloads, service members, the control plane, CIDR rule prefixes (one identity per distinct prefix), and DNS-learned addresses (§4.13; expiry handled in user space) |
| `published` | hash | (address, port, protocol) → identity, for services reached on a worker's published port, where many services share one node address |
| `policy_a`, `policy_b` | hash | (direction, subject, peer, protocol, port) → verdict and rule id. Port 0 means any port. A port range is expanded into exact keys up to 1,024 ports; a wider range is a typed error at save |
| `config` | array | active policy map index, node mode (`legacy`, `shadow`, `enforce`) |
| `counters` | per-CPU array | (identity, direction, verdict) → packets and connections, read once per pass |
| `samples` | ring buffer, 256 KiB | blocked-flow samples, rate-limited in the program by a token bucket (5 per second per node) |

- **Lookup order** follows the precedence in §4.9. Precedence is resolved when the policy is compiled, so the program does at most three lookups: exact port, then any port, then the profile default.
- **Addresses outside every prefix:** an address not in `ipcache` is `world` (the internet, decided by the `internet` flag), except private ranges, which are "unknown internal" and denied to restricted workloads unless a CIDR rule allows them.
- **CIDR rules and workloads:** a CIDR rule matches destinations that are not known workloads. Reaching a workload needs a workload or service rule, the same guidance Kubernetes gives for `ipBlock`.

#### Updates, restarts, reboots

- **Atomic policy swap.** The agent fills the inactive policy map, flips the index in `config` (one 4-byte write, atomic for readers), then clears the old map. This replaces the single `nft -f` transaction.
- **Address changes:** `ipcache` and `published` are updated incrementally, removals first (§4.8).
- **Agent restart:** programs, maps and attachments (`bpf_link`) are pinned, so they survive an agent restart and keep enforcing. The new agent reopens the pins and reconciles in place.
- **Reboot:** bpffs does not survive a reboot. `temps netpol boot`, run by a systemd oneshot ordered `Before=docker.service`, loads the programs and attaches them to `temps.slice` with an empty `cgroup_identity`. In `enforce`, an unknown workload slice is denied, so nothing is open before the agent's first pass. As in §4.8, open workloads too have no outbound traffic for those few seconds.
- **Drift:** each pass checks through the `bpf_link` query that the programs are still attached to `temps.slice`, and compares a digest of the map contents. It re-attaches or re-fills when they differ, and counts each re-apply (§5).
- **Shadow and trial:** the programs evaluate as usual, count "would block", and return allow when the node is in `shadow` or the identity has `trial` set.

#### Identity across nodes

On the same node, identity comes from the cgroup and addresses do not matter. A packet from another node is judged by looking its source address up in `ipcache`, which is how Cilium works in native-routing mode. That mode has a documented policy bypass: a receiving node with a stale map admits a newcomer on a reused address as the address's previous owner (§9). Four rules close it for Temps:

1. **Addresses change owner only on their own node, and only after the cool-down** (§4.8, gap 4). Each node owns a /24 of the compute pool, and only its agent assigns addresses in it.
2. **Every holder must acknowledge before reuse.** A freed address stays quarantined until every node whose snapshot held it reports, in its heartbeat, an applied revision without it. A node with a stale map has not acknowledged, so the address it would misjudge is never handed out. While the control plane is unreachable, no acknowledgements arrive, so nothing is reused: the safe direction.
3. **A node's addresses can only come from that node.** The receiving node checks that a cross-node packet's source address lies in the /24 of the node it came from:
   - VXLAN transport: a TC program on the VXLAN device compares the tunnel's outer source with the node that owns the inner source's /24. This needs the device in collect-metadata mode; see §12.
   - native transport: a strict reverse-path check against the per-node routes.
4. **A node that was dropped is distrusted until it catches up.** A node with no heartbeat for 10 minutes is removed from the holder sets, so its peers' addresses can be reused. When it comes back, every remote entry in its `ipcache` is treated as unknown until it has applied a fresh snapshot. Its restricted workloads refuse remote peers for those seconds, and its node page says why. Open workloads are unaffected.

**Cost and bounds:** with at most 110 live containers in a 222-address range, at least 112 addresses are always free or waiting for acknowledgement. Only a holder that never acknowledges can exhaust them. When addresses waiting for acknowledgement pass half of what is free, the node page names the node holding them back. If they run out, a container start fails with a typed error naming the node, the subnet and the holder, never with a silent reuse.

**Later, identity in the packet** (not in the current phases). Cilium carries the sender's identity in the VXLAN header's 24-bit network identifier. Temps already uses VXLAN between nodes, so a TC program could set that field to the source identity, and the receiver would read it instead of `ipcache`. Native transport would keep the rules above. Worth doing if the rules above prove costly in practice.

#### Settings

Anything an operator can change is a column, never an environment variable. Everything not listed here is a fixed constant.

**Per workload** (set at create, editable on the card):

| Setting | Values | Default |
|---|---|---|
| `egress` profile | `open` / `restricted` / `none` | `open` |
| `ingress` profile | `open` / `restricted` / `none` | `open` |
| `internet` | on / off; only meaningful when egress is `restricted` | on |
| `trial` | on / off: evaluate and report, never block | off |
| rules | service, workload, CIDR, hostname (phase 7); with port and TCP/UDP | none |

New projects and environments are always `open` (§11, decision 3). An environment inherits its project's setting unless it overrides it.

**Per node** (`nodes` row, node page):

| Setting | Values | Default | Notes |
|---|---|---|---|
| `network_policy_mode` | `legacy` / `shadow` / `enforce` | `legacy` | Exists. `enforce` is allowed only after 10 minutes in `shadow` with clean passes, and once every Temps container is under `temps.slice` (§11, decision 10) |
| `max_workloads` | 8 up to half the node's Temps-assigned range | 110 | Like kubelet `--max-pods`. Temps containers on the node, sandboxes included; the scheduler skips a full node (§4.8, gap 4) |
| `netpol_ipcache_max` | 1,024–262,144 | 65,536 | Maximum known addresses. Takes effect on the next agent start, since map sizes are fixed at creation |
| `netpol_policy_max` | 1,024–262,144 | 65,536 | Maximum rule entries per policy copy. Applied on the next policy swap |

The two size settings are shown as advanced. Maps do not preallocate, so a typical node uses well under 1 MB whatever the maximum.

**Per cluster** (`network_config` row, Network settings page):

| Setting | Values | Default | Notes |
|---|---|---|---|
| `subnet_prefix_len` | exists | 24 | The per-node subnet, like Kubernetes' `--node-cidr-mask-size`. Fixed once nodes have joined |
| `address_cooldown_secs` | 60–3,600 | 300 | The preferred minimum time before a freed address is reused. The hard rule is acknowledgement by every node holding it (§4.8, gap 4) |
| `node_distrust_after_secs` | 120–3,600 | 600 | A node silent this long stops counting as a holder, so its peers' addresses can be reused. When it returns, its restricted workloads refuse remote peers until it has applied a fresh snapshot ("Identity across nodes") |

Every change is audit-logged like any other write.

**Address layout of a node's subnet** (fixed). For the default /24, e.g. `10.202.1.0/24`:

| Range | Use |
|---|---|
| `.1` | bridge gateway and the node resolver, as today |
| `.2`–`.223` | Temps-assigned, round-robin, oldest freed first (222 addresses: twice the 110-container default, as in Kubernetes) |
| `.224`–`.254` | Docker's own picks (`IPRange` `.224/27`), never given an identity; only on networks created from now on (§4.8) |

For other prefix sizes, Docker's range is always the top eighth of the subnet.

**Fixed constants:**

| Constant | Value |
|---|---|
| Slice per workload | `temps.slice/temps-<kind>-<id>.slice` |
| Pins | `/sys/fs/bpf/temps/netpol/` |
| Blocked-flow samples | 5 per second per node, the latest 50 kept, 256 KiB ring buffer |
| Port ranges in a rule | at most 1,024 ports; wider is a validation error |
| Shadow time before `enforce` | 10 minutes |
| Agent safety pass | every 15 s; Docker events trigger passes immediately |
| Failsafe | processes outside `temps.slice` (sshd, the agent, WireGuard) are never filtered; DNS to the node resolver is always allowed |
| Denied connect | returns `EPERM` |

The host requirements below are checked, not configured.

#### Requirements, checked at agent start

The agent checks each of these and reports `unavailable` with the exact reason and fix (§5) when one is missing:

- Linux with cgroup v2 (the default on current distributions);
- kernel BTF (`/sys/kernel/btf/vmlinux`);
- `CAP_BPF`, `CAP_NET_ADMIN` and `CAP_SYS_ADMIN`, or root;
- Docker using the systemd cgroup driver.

The minimum kernel for the container path is 5.10; the socket hooks are far older, and BTF is the real requirement. Firecracker needs 5.19 for the conntrack kfunc. An example message: "worker-2 cannot enforce network policy: the host uses cgroup v1. Boot with `systemd.unified_cgroup_hierarchy=1`."

#### Memory

Every hash map is created with `BPF_F_NO_PREALLOC`, so memory follows the actual number of entries. The prototype measured a preallocated 64k-entry policy hash at 5.5 MB per copy (§4.16). At the default maximum sizes (64k `ipcache` entries, 64k policy entries, the per-CPU counters, a 256 KiB ring buffer), a full node stays under 16 MB of kernel memory, and a typical node with a few hundred rules uses well under 1 MB. Sizes are columns on the node row, not environment variables. On the 1 vCPU / 2 GB target this is well inside budget, and the per-connection design costs less CPU than per-packet filtering.

#### Build and ship

- **Crates:** programs are written in Rust with `aya-ebpf`, in a new `no_std` crate `temps-netpol-ebpf`, compiled for `bpfel-unknown-none` with `bpf-linker`. The loader uses `aya` in `temps-workload-netpol`, behind the existing executor interface (`choose_executor`).
- **Single binary:** the bytecode is embedded with `include_bytes_aligned!`, so Temps stays one binary, with no clang or libbpf at runtime. CO-RE relocations use the kernel's BTF.
- **Build environment:** the release build needs nightly Rust and `bpf-linker`. CI builds the object once and checks that its embedded digest matches.

#### Mapping from the nftables wording above

| Earlier sections say | With eBPF |
|---|---|
| a workload's address set | a `cgroup_identity` entry for local workloads; `ipcache` and `published` entries for remote ones |
| anti-spoofing bindings (`binding.rs`) | not needed for containers: identity is the cgroup. Firecracker keeps an ifindex → identity check |
| one `nft -f` transaction | the double-buffered policy map swap |
| drop-rule counters and NFLOG samples | `counters` and `samples` |
| boot base table | `temps netpol boot` attaching with an empty identity map |
| kernel table drift | `bpf_link` attachment check and map digest |
| `host` rule sets with TTL (§4.13) | `ipcache` entries with a user-space TTL |

**What happens to the nftables code in #1632:** removed after phase 2 (§11, decision 8).

### 4.16 Prototype first (phase 0)

Before building phases 1–8 on eBPF, prove the container path on the maintainer test machine (Linux 7.0, BTF, cgroup v2, Docker with the systemd driver):

1. two containers under their own workload slices, programs attached to `temps.slice`;
2. `connect4` allows the linked peer and returns `EPERM` for an unlinked one; `cgroup_skb/ingress` refuses an unlinked source and lets replies through;
3. restart a container with a new IP: no change in verdicts; restart the agent: enforcement continues from the pins; reboot: nothing open before the agent's first pass;
4. measurements against no policy and against the nftables enforcer: connect latency, throughput, CPU per 100k connections, memory, time to apply 10, 100 and 1,000 identities.

#### Results (2026-10-26)

**Setup:**

- Prototype in Rust: `aya-ebpf` 0.2 for the programs, `aya` 0.14 for a small loader CLI. Not in the repo yet.
- Programs: `cgroup/connect4` and `cgroup_skb/ingress`.
- Maps, pinned: `cgroup_identity`, `ipcache` (LPM), `policy`, `config`, `counters`, `samples`.
- Both programs attached once to a parent slice through pinned `bpf_link`s.
- Three containers under their own child slices: A (restricted egress), B (nginx, restricted ingress) and C (open).

**Functional matrix, `enforce` mode. Every case behaved as designed:**

| # | Case | Result |
|---|---|---|
| 1 | A → B:8080, allowed by a rule | connects |
| 2 | A → C:8080, no rule | `Operation not permitted` immediately |
| 3 | A → 1.1.1.1:80, `internet` on | connects |
| 4 | C → B:8080, B's ingress restricted, no rule | dropped (client times out) |
| 5 | host process → B:8080 | dropped |
| 6 | same, after a `system` rule for the bridge gateway | connects |
| 7 | A → 1.1.1.1:80, `internet` off | `EPERM` |
| 8 | A → C in `shadow` mode | connects, counted as "would block", sampled |
| 11 | a workload slice with no `cgroup_identity` entry | `EPERM` (fails closed) |
| 12 | a host process outside the parent slice → internet | connects (failsafe, §4.11) |

**Counters and samples.** They matched the cases one for one. Each blocked-flow sample carried the subject and peer identities, and none were dropped.

**Agent restart: verified.** The loader process exits after every command, yet enforcement never stopped. The pinned links and maps carry it on their own.

**Reboot: not tested yet.** It needs the maintainer's go-ahead to reboot the machine. Covered in phase 2.

**IP change: two findings that change the design.**

- **Egress is unaffected, as expected.** After restarting A with a new address (case 9 reran case 1), A → B was still decided by A's cgroup identity.
- **Ingress at a restricted destination depends on `ipcache`.**
  - A's new address was not in `ipcache` yet, so B refused A until the entry was written. That fails closed, which is the right direction (case 9 timed out; case 10 connected after the update).
  - **A stale entry fails open (case 13).** Docker 29 handed A's *old* address to the very next container started, which was outside the Temps slices. B admitted it as A, because `ipcache` still mapped the address to A's identity.
  - Removing the stale entry closed the hole (case 14).
  - This disproves the guess in §4.8 that Docker hands out addresses serially. It reuses a freed address at once.
- **Design changes this forces (applied to phase 2):**
  1. **Removals are synchronous on the Docker `die` / network `disconnect` event.** The agent deletes a local endpoint's `ipcache` entry before doing anything else with the event. The 15 s pass only catches what events missed.
  2. **Temps assigns addresses on its own networks**, round-robin from the node's /24 with a cool-down, disconnects containers Temps did not create, and fences Docker's own picks on networks created from now on (§4.8, gap 4). This is what every Kubernetes plugin does by owning allocation (§9). It is node-local, with no cluster lease table, so the reasons for rejecting leases in §4.8 do not apply. Docker's serial-allocation IPAM option was considered and not chosen: it only makes reuse less likely.
  3. **Only Temps containers join Temps networks.** A container on a Temps network that is not under `temps.slice` is reported as drift on the node page ("container X joined network temps without Temps; it cannot be identified, and restricted workloads refuse it"). Its address never gets an identity.
  4. **Test:** the conformance suite (§4.12) replays case 13 and asserts that the reused address is refused.

**Measurements** (`kernel.bpf_stats_enabled=1`, whose own accounting adds a few tens of ns to each figure):

| What | Result |
|---|---|
| `connect4`, per connect | 1.07 µs (8,000 connects, a mix of allowed-by-rule and internet) |
| `cgroup_skb/ingress`, per packet | 0.51 µs (79,989 packets) |
| `ipcache` update, through `bpftool batch` | 10 entries: 18 ms; 100: 19 ms; 1,000: 30 ms; 10,000: 111 ms. About 15 ms of each is process start-up, and each line reopens the pin, so in-process updates on a held map fd are faster |
| `ipcache` delete, 10,000 entries | 111 ms |
| `policy` map, 64k entries, hash, preallocated | 5.5 MB of kernel memory per copy, so 11 MB for the double buffer |
| `cgroup_identity`, 4k entries | 324 KB |
| `samples` ring buffer, 256 KiB | 270 KB |
| `ipcache`, LPM, no preallocation | grows with entries |

**Changes from the measurements:**

- **The ingress program does too much per packet.** It looks up the cgroup identity before checking TCP flags. Phase 2 checks first for a new inbound connection (SYN without ACK, or the socket's state) and returns before any map lookup otherwise. Established packets then cost only the parse.
- **Policy maps use `BPF_F_NO_PREALLOC`.** Memory then follows the number of rules instead of the maximum size (see "Memory" in §4.15).
- **Attach mode.** With `bpf_link`, `aya`'s `CgroupAttachMode::AllowMultiple` is refused by `bpf_link_create` on this kernel, while `Single` works. Links coexist with systemd's own per-slice programs (`sd_fw_ingress`, `sd_devices`) either way. Use `Single`.

**Not measured yet:**

- throughput, and latency against no policy and against the nftables enforcer. A shell loop of `wget` measures process start-up, not the hook. Phase 2 runs `wrk` against the conformance setup;
- CPU per 100k connections;
- the reboot test.

The per-invocation costs above are already well below the cost of the connect system call itself, which takes tens of microseconds.

The test machine was cleaned afterwards: no programs, pins, slices or containers left.

### 4.17 Names across nodes: Cluster DNS

Rules that point at a service (§4.2) are only useful if the workload can reach that service by name. A multi-node cluster was tested: a control plane and two workers on WireGuard, with applications and databases on every node. Every application container on every node resolved and connected to every database, and a restricted application was refused by its own rule. Getting there exposed three problems, now fixed:

1. **Switching Cluster DNS needed a control-plane restart.** Workers re-read the setting on every network sync, but the control plane read it once at boot. Its resolver now follows the setting the same way: it starts when the setting is turned on and stops when it is turned off. A failed settings read keeps the current state.
2. **Running containers silently kept the old nameservers.** A container's nameservers are fixed when it is created, so turning the setting on only reaches new containers. Each agent now reports, on its heartbeat, the deployments whose containers lack its resolver. The control plane checks its own daemon. `GET /cluster/dns/status` lists those applications by project and environment, but only current deployments. The Cluster DNS card lists them with a Redeploy action, and an affected application's page shows a banner. Databases are not listed, because they do not look up other services by name.
3. **It was off by default, even with workers.** A cluster with workers needs names: a database on one node is not reachable by address from another node's bridge. The first worker to join now turns it on, unless an operator has already set the switch, in either direction. That choice is recorded (`cluster_dns.operator_chosen`) and the automatic change is audited (`CLUSTER_DNS_ENABLED_FOR_WORKER`).

## 5. What the user sees when something is wrong

Every row below is rendered where the user works: the workload's "Network access" card, the node page, and the Network policy page. Nothing fails silently.

| Situation | What the console says | Fix offered |
|---|---|---|
| Workload is `restricted` on a node in `legacy` or `shadow` | "Restricted, but worker-1 does not enforce network policy yet. This app can still reach everything." | Button: "Turn on enforcement for worker-1" (opens the node's shadow → enforce flow) |
| Node cannot enforce | "worker-2 cannot enforce network policy: the host uses cgroup v1." (or: no kernel BTF, missing capabilities, Docker's cgroupfs driver) | The exact fix for that requirement (e.g. boot with `systemd.unified_cgroup_hierarchy=1`, or set Docker's `exec-opts` to `native.cgroupdriver=systemd`), then "Re-check". The reason comes from the agent's probe, not a guess |
| Last pass failed | "Applying network policy on worker-3 failed 2 min ago: `<nft error>`. The previous rules are still in force." | The error text, a link to the node's logs, and "Retry now" |
| Agent has not received policy for longer than 3 passes | "worker-3 has not received policy updates for 5 min. It keeps enforcing the last rules it got." | Agent connectivity check from the node page |
| Kernel table missing or different from the applied one (drift) | "Network policy on worker-1 was removed outside Temps (for example, by a firewall reload). Re-applied at 14:02." | Informational, with a count of re-applies, so a recurring cause gets noticed |
| Shadow mode would block traffic | "In shadow mode, 3 connections from `api (production)` to `10.0.5.4:6379` would be blocked." | One click: "Allow this", which creates the rule, or "Keep restricted" |
| Rule points at a deleted service or workload | Rule shown as "dangling: service 42 no longer exists" | "Remove rule" |
| `host` rule added to a non-sandbox workload | Validation error at save: "Hostname rules apply to sandboxes only; use a CIDR or a Temps service." | Points to the right rule types |
| Linking a service to a sandbox on a node that cannot enforce | 409: "Cannot connect `postgres-cluster` to sandbox `sbx_…`: worker-3 does not enforce network policy, and a sandbox is never connected without it." | "Turn on enforcement for worker-3", or "Move the sandbox to a node that enforces" |
| Restricted workload being scheduled with no enforcing node eligible | The deployment fails with: "No eligible node enforces network policy; `api` is restricted." | Lists the nodes and their state, with "Turn on enforcement" |
| Connections blocked at runtime | "12 connections blocked in the last hour to `10.0.0.9:5432`" (and the service name when the address belongs to one) | "Allow `postgres-main`" when the address maps to a Temps service |
| Service name does not resolve inside a restricted workload | Card explains: "Names of services this workload is not allowed to reach do not resolve." | Same "Allow" action |
| A rule the workload's backend cannot enforce | 400 at save or create: "microsandbox cannot enforce a port range on a CIDR rule for sandbox `sbx_…`." | Lists the rule types that backend supports, or suggests a Docker sandbox |
| Trial mode findings | "Trying restricted access: 2 destinations would have been blocked in the last hour." | "Allow this" for each, then "Enforce now" |
| Blocked by an instance rule | "Blocked by the instance rule 'no internal ranges', set by an admin." | Who to ask; instance admins get "Edit rule" |
| Restricted ingress names a remote source, mesh off | "Only linked apps may connect to `postgres-main`, but worker-1 and worker-2 talk without the WireGuard mesh, so connections from worker-1 cannot be verified and are not filtered." | "Turn on the WireGuard mesh" (link to the network settings) |
| Sample buffer saturated | "Showing the last 50 blocked connections; 1,204 more were counted but not sampled." | Informational |

## 6. Stability

1. **Per-node rollout, gated.**
   - The mode switch only allows `enforce` after the node has run in `shadow` for at least 10 minutes with all passes succeeding.
   - Every `shadow_would_block` entry for an `open` workload must be zero. An open workload must never be affected.
   - The console shows the gate's progress.
2. **Atomic.** Policy changes go into the inactive policy map, and one 4-byte write to `config` switches to it (§4.15). Readers see the old policy or the new one, never a mix.
3. **Last known good.** If the policy source fails (control plane unreachable, a query error), the enforcer keeps the installed ruleset and records `failed`. It never removes rules because it could not read new ones.
4. **Fail closed for new restricted workloads.** A restricted workload that starts on a node with no current snapshot gets the deny-all base chain until its first successful pass. It does not get open access.
5. **Drift check.**
   - Each pass checks through the `bpf_link` query that the programs are still attached to `temps.slice`, compares a digest of the map contents with `applied_digest`, and re-attaches or re-fills when they differ.
   - Re-applies are counted and shown (§5).
   - A container on a Temps network that is not under `temps.slice`, or holds an address Temps did not assign, is reported as drift.
6. **Restarts and reboots** are covered in §4.8 and §4.15. In short: nothing address-bound is persisted across a reboot. `temps netpol boot` attaches the programs with an empty identity map before Docker starts, so nothing restricted is open between boot and the agent's first pass.
7. **Agent restarts reconcile in place.** Programs, maps and links are pinned, so enforcement continues while the agent is down; the new agent reopens the pins and reconciles. Verified by the prototype (§4.16): the loader exits after every command and enforcement never stopped. This matches how Calico and Cilium treat agent restarts (§9).
8. **Failsafe exceptions** keep the node manageable whatever the rules say (§4.11).
9. **One-step rollback.** Setting the node to `legacy` detaches the programs and removes the pins on the next pass. Temps-assigned addresses stay as they are; they are harmless without enforcement.
10. **Bounded cost.**
   - A pass is O(local instances), with endpoint inspects cached by endpoint id.
   - The snapshot is limited to the node.
   - The status upsert happens only on change.
   - Blocked-flow samples are a 50-entry ring per node at most 5 per second.
   - On the 1 vCPU / 2 GB target this adds one loop per agent (event-driven, with a 15 s safety poll) and a few small queries on the control plane per node.
11. **Upgrade safety.**
   - Every node stays in `legacy`, and every existing workload stays `open`.
   - Sandboxes keep their current isolation until a node is put in `enforce`.
   - An upgrade test restores a database from the previous release and asserts that no row or container changed.

## 7. Console

- **"Network access" card** on each place where the task happens:
  - the project environment page (deployments);
  - the service page;
  - the function and job pages;
  - the sandbox page.
- **Card contents:**
  - the profile switch: "Open: reaches all internal services" (the default), Restricted, or None;
  - an Internet on/off switch, shown when restricted;
  - the allowed list: databases and Temps services picked by name, plus CIDRs, plus hostnames for sandboxes;
  - links to the services that created each link rule;
  - the enforcement state of the node, with the §5 messages and fixes;
  - blocked connections in the last hour.
- **Sandbox page:** the "Outbound network" card becomes the Internet switch plus hostnames. A "Temps services" list with "Connect a database" replaces the chat-only linking. This also fixes the standalone sandbox page having no way to link a service.
- **Node page:**
  - the shadow → enforce flow with the gate's progress;
  - the last pass, the last error and re-applies;
  - would-block entries with "Allow this".
- **Settings → Network policy** stays as the overview of all rules across workloads.

## 8. Phases and estimate

Days are engineering days for one person, including tests and review rounds, not CI or merge waits. They assume the test environments in §13.

| Phase | Ships | Tests | Days |
|---|---|---|---|
| 0. eBPF prototype | §4.16 on the test machine. **Done** except the reboot test, throughput and latency against no policy and nftables, and CPU per 100k connections | the §4.16 checks and measurements, written into this document | 1 left |
| 1. Status as data | `network_policy_node_status`, pass outcomes recorded (node 0 first), the `network-access` status endpoint, cards in read-only mode with the §5 messages | unit: status transitions per pass outcome; handler: 401, 403, typed body; frontend: every §5 message renders from a status fixture | 3–4 |
| 2. eBPF datapath and worker enforcement | see the breakdown below | see the breakdown below | 15–20 |
| 3. Controls, at creation and after | `NetworkSpec.internet` and `allow`; `network_access` on every create request; admission defaults and validation; `NetworkAccessFields` in New project, environment create, Create service and Create sandbox; the card editor; project default with environment override; migration of the sandbox egress policy; the shadow → enforce gate | unit: profile semantics table (§4.1), admission defaults per kind, every validation error names its field, a failed create leaves no rules, environment inherits then overrides its project; migration maps every existing sandbox policy 1:1; frontend: the shared fields in each create form and on the card; the acceptance pass (§13) | 5–7 |
| 4. Sandboxes on any node | overlay attach under enforce, `require_local` removed, `primary.<svc>.temps.local` in sandbox variables, fail-closed link refusal | e2e: a sandbox on worker-3 queries a cluster on workers 1–2 with internet off; failover moves the primary and the sandbox reconnects; a link on a non-enforcing node is refused with the typed error | 3–4 |
| 5. Blocked-traffic feedback and trial mode | eBPF counters and rate-limited samples surfaced, per-workload trial mode, precedence explained on the card, "Allow this" from samples, trial and shadow findings | unit: counter aggregation, sample ring bound and saturation count, address → service mapping, precedence order; e2e: a blocked connection appears within one pass; a workload in trial is never blocked | 3–4 |
| 6. Firecracker and conformance | TC programs on the Firecracker TAP; the conformance suite against Docker, Firecracker and microsandbox, skipping gracefully where a backend is missing (CI wiring out of scope, §11) | the §4.12 matrix on every backend; admission refuses a rule a backend failed | 5–7 |
| 7. Hostname rules beyond sandboxes | the resolver adds answered addresses to `ipcache` with TTL and a per-workload bound | unit: TTL clamping, bound and eviction; e2e: a deployment allowed `api.example.com` reaches it and not a hard-coded IP | 3–4 |
| 8. Ingress rules | ingress profiles enforced on the receiving node, link triggers writing both directions, the mesh requirement for remote sources, "Who can connect" on the card | the §4.14 tests and the ingress conformance cases | 4–5 |
| — Removal and hardening | the nftables enforcer deleted (§11, decision 8); final soak and fixes | the full §13 plan once more | 2–3 |
| **Total** | | | **about 45–60** |

**Phase 2 breakdown:**

| Part | Tests | Days |
|---|---|---|
| `temps-netpol-ebpf` crate; nightly and `bpf-linker` in the release build; bytecode embedded with a digest check | the embedded object's digest matches the build; the binary loads it on the test machine | 2–3 |
| Programs: `connect4/6`, `sendmsg4/6`, `cgroup_skb` ingress and egress, IPv4 and IPv6, ingress returning before any map lookup unless the packet opens a connection | program tests (§13, layer 2) | 3–4 |
| Loader: pinned maps (hash maps with `BPF_F_NO_PREALLOC`), double-buffered policy, drift check, `temps netpol boot` and its systemd unit, requirement checks | unit: swap order, digest; e2e: agent kill, Docker restart and reboot on the VM cluster | 3 |
| Containers under per-workload slices (Docker executor and agent); the "redeploy before enforce" gate (§11, decision 10) | unit: every Temps container create carries the slice; e2e: an old container blocks enforce and is listed | 1–2 |
| Address allocator (round-robin, acknowledgement, cool-down preference); `max_workloads` as scheduler capacity with its typed "no node has room" error; disconnecting containers Temps did not create; the `IPRange` fence on new networks and the "Add the address fence" node action; Compose addresses injected; per-node source-subnet check | the §4.8 allocator tests; conformance case 13; the churn e2e | 2 |
| `AgentPolicySource`, node-scoped snapshot endpoint, `network_policy` stream hint, heartbeat status and applied revision; distrust of a node that was dropped (§4.15) | unit: snapshot scoping (a worker never gets foreign rules), last-known-good on source error; e2e: partition a worker past 10 minutes and back | 3–4 |
| Multi-node e2e on the VM cluster | a restricted deployment on worker-1 reaches its linked database on worker-2 and not an unlinked one; reboot a worker and assert no open window | 2 |

**The first useful slice is phases 1–3** (about 25–30 days): restricted workloads enforced on every Linux node and configurable at creation. Phases 4–8 follow one at a time.

**Risks to the estimate:**

- The Colima dev cluster runs workers as Docker-in-Docker without systemd, so the eBPF path probably cannot run there; multi-node eBPF tests need the VM cluster (§13). If VMs on the test machine do not work either, add 3–5 days.
- Nightly Rust and `bpf-linker` in the release pipeline may take a few rounds. The eBPF object is architecture-independent, so it is built once.
- Injecting Temps-assigned addresses into Compose deployments is unexplored.
- Firecracker TC is not covered by the prototype.

Each phase is shippable on its own, and nodes stay in `legacy` until an operator moves them.

## 9. Prior art: Kubernetes network policy

Kubernetes defines the `NetworkPolicy` API and enforces nothing itself; the network plugin does. Calico and Cilium are the two references. Details below are from general knowledge of these projects unless a source is given, not checked line by line against their current docs.

| Topic | Kubernetes / Calico / Cilium | What we take |
|---|---|---|
| Semantics | Everything is allowed until a policy selects a pod; then that direction is default-deny for it, and allow rules add up. Core has no deny, no hostnames, no status | Same "open until restricted" model (§4.1), plus deny rules and an explicit order (§4.9) |
| Identity | Calico: labels → sets of IPs. Cilium: labels → a numeric security identity, with an IP → identity map in BPF | Identity-keyed sets (already built) |
| IP churn | Pod IPs change on every recreate. Neither plugin keeps IPs stable; both propagate changes by watching the datastore, in under a second | Push propagation over the desired-state stream (§4.3); overlay IP leases dropped (§4.8) |
| A new pod before its policy exists | Calico: traffic from an interface its agent has not programmed falls through to a final drop. Cilium can hold pod networking until the first policy is computed | The address-free base table and Docker-event passes (§4.8) |
| Agent restart | Rules stay in the kernel (iptables or pinned BPF maps); the agent reconciles in place | Verified for our enforcer (§6, item 7) |
| Trying before enforcing | Calico staged policies; Cilium audit mode | Per-workload trial mode (§4.10), alongside per-node shadow |
| Visibility | Calico flow logs; Cilium Hubble: per-flow records with identities and verdicts | Counters plus rate-limited samples (§4.5) |
| Hostnames | Cilium `toFQDNs`: a DNS proxy learns IPs from answers, for the TTL | Resolver-learned sets for non-sandbox workloads (§4.13) |
| Node safety | Calico failsafe ports (SSH, BGP, API server) | Failsafe exceptions (§4.11) |
| Operator vs user | Calico tiers; `AdminNetworkPolicy` | Precedence by origin (§4.9) |
| The known trap | A plugin without policy support (Flannel) accepts policies and silently enforces nothing | Our per-node "enforced or not, and why", and refusing to restrict where nothing enforces (§5) |

### How the plugins handle address reuse

Every plugin that identifies traffic by address must stop an address from changing owner while some node still maps it to the old one. They use three layers:

| | Same node | Across nodes |
|---|---|---|
| **Cilium** | The plugin also allocates addresses, so it removes the policy entry before returning the address to the pool. Local identity comes from the endpoint the packet left, not its address | Tunnel mode and WireGuard: the sender's identity travels in every packet. Native routing: `ipcache` lookup by address, with a documented bypass when a node's map is stale and an address is reused; mitigated by per-connection mutual authentication (window up to the certificate rotation period, 30 minutes by default) |
| **Calico** | Allocates addresses in blocks per node and removes the endpoint before releasing them; each pod's interface only accepts its own address | Address sets synced from the datastore. No documented cool-down found |
| **AWS VPC CNI** | A deleted pod's address waits in a 30 s cool-down before reuse, so every node finishes updating its rules first. Security groups for pods add a 60 s cool-down (30 s minimum) for ARP caches | Covered by the cool-down |
| **`host-local` allocator** (Antrea, kube-router, Flannel) | Round-robin: continues after the last address given out, so a freed address returns only after the range wraps | Same |
| **Service meshes** (Istio, Linkerd, Consul) | Identity is the workload's mTLS certificate; address reuse cannot cause a wrong verdict | Same; the cost is a proxy per workload or per node |
| **Temps** (§4.8 gap 4, §4.15) | Temps assigns addresses round-robin with a 5-minute cool-down and fences Docker's allocator; local identity from the cgroup | Reuse waits for every holder's acknowledgement; per-node source-subnet check; a dropped node is distrusted until it catches up; identity in the VXLAN header later |

Sources: [Amazon EKS best practices: VPC CNI](https://docs.aws.amazon.com/eks/latest/best-practices/vpc-cni.html); [amazon-vpc-resource-controller-k8s PR #359](https://github.com/aws/amazon-vpc-resource-controller-k8s/pull/359/files); [a write-up of the Cilium native-routing bypass](https://hackmd.io/@youngnick/SJFj0UZha).

### How Kubernetes sizes node address ranges

| | Per-node range | Per-node limit | Rule |
|---|---|---|---|
| Kubernetes (controller manager and kubelet defaults) | /24 (`--node-cidr-mask-size`) | 110 pods (`--max-pods`) | — |
| GKE | the smallest range holding 2× the limit: /24 for 110, /23 for 256 | 110 by default, up to 256 | "twice the maximum" so addresses are reused less often during churn |
| Cilium cluster pool | /24 per node (`clusterPoolIPv4MaskSize`) | the kubelet's | — |
| Temps | /24 (`subnet_prefix_len`) | 110 (`max_workloads`) | at least 2× the limit, enforced when the limit is set |

Sources: [GKE: configure maximum Pods per node](https://cloud.google.com/kubernetes-engine/docs/how-to/flexible-pod-cidr); [GKE: avoiding the IP_SPACE_EXHAUSTED error](https://cloud.google.com/blog/products/containers-kubernetes/avoiding-the-gke-ip_space_exhausted-error). The Kubernetes and Cilium defaults are from general knowledge.

### What we take from Cilium, and what we do not

Cilium's rules are what keep it correct; its implementation is built for Kubernetes nodes with more memory than our 2 GB target.

| Take | In this design |
|---|---|
| Policy names identities, never addresses | §4.1, §4.2 |
| eBPF maps for identities, addresses and policy | §4.15 |
| Local identity from where a packet came from, not its source address | from the cgroup instead of a program on each interface: as strong and cheaper |
| An address is freed only after its identity is forgotten | Temps-assigned addresses with cool-down and acknowledgement (§4.8) |
| Push propagation | §4.3 |
| Audit mode | shadow and trial (§4.10) |
| Hubble-style flow visibility | counters and samples (§4.5) |
| FQDN rules learned from DNS | §4.13 |
| Identity carried in the VXLAN header | later (§4.15) |

| Do not take | Why |
|---|---|
| Owning the container network: programs on every interface, its own routing, replacing kube-proxy | Docker networking stays as it is; per-connection hooks cost less than per-packet programs, and we do not maintain a network stack |
| Being the network plugin | Docker has no equivalent interface; assigning addresses at create gives the one guarantee we need from it |
| A separate agent of hundreds of MB, an etcd or CRD store, Envoy on every node | Breaks the 2 GB target. The enforcer lives in the agent and the control plane holds the state |

### Other plugins considered

| Plugin | Why not the model |
|---|---|
| Calico | Equally sound, and its pluggable datapaths match §4.12, but identity is by address everywhere; Cilium's endpoint identity is the closer fit for cgroup hooks |
| AWS VPC CNI network policy agent | Tied to AWS networking; we take its cool-down |
| Antrea | Needs Open vSwitch on every node |
| kube-router | Light, but no hostname rules or per-flow visibility |
| Flannel, Weave Net | Flannel enforces nothing; Weave Net is archived |
| Service meshes | Strongest identity, but a proxy per workload costs too much memory; a per-node proxy may return for HTTP rules after phase 7 |

## 10. Choice of datapath

The first draft recommended nftables. After review, eBPF was chosen (§4.15) for these reasons:

- **Identity from the cgroup.** A container is identified by its workload slice, not its address. That removes the start-up race, IP-change staleness and spoofing for local workloads, which were the hardest parts of §4.8 with nftables.
- **Failsafe by construction.** Programs attached to `temps.slice` cannot affect host processes.
- **Per-connection cost.** `connect` hooks decide once per connection and return `EPERM`, a clear error instead of a timeout.
- **Agent restarts.** Pinned programs and links keep enforcing while the agent restarts.
- **Visibility.** Counters and a rate-limited ring buffer come directly from the program.

**Costs we accept:**

- nightly Rust and `bpf-linker` in the release build;
- Linux-only enforcement, with BTF and cgroup v2 required (§4.15 lists the checks);
- privileged tests on a real kernel;
- microsandbox still needs its SDK policy, because its traffic never crosses a host interface.

The prototype (§4.16) checks these reasons with measurements before the main phases start.

## 11. Decisions from review

1. **Ingress rules: in scope.** Designed in §4.14, built as phase 8.
2. **Hostname rules for deployments, functions and jobs: yes,** through resolver-learned address sets (§4.13), as phase 7. No HTTP proxy in front of deployments.
3. **Default for new projects and environments: always `open`.** No instance-wide or per-project "restricted by default" switch.
4. **Enforcement on workers: Linux only.** macOS remains a development setup in which only node 0 enforces, inside the Docker VM (`DockerVmNft`).
5. **CI for the microVM backends is out of scope for this plan.** The conformance suite runs wherever a backend is available and skips gracefully elsewhere; how CI provides KVM is decided separately.
6. **The datapath is eBPF, on Linux only** (§4.15). It is prototyped and measured on a maintainer-run Linux machine with KVM first (§4.16).
7. **Choose for the long term.** Where two designs both work, pick the one that serves the future, not the one that is quicker to finish now. This is why eBPF was chosen over the already-built nftables enforcer: it is the direction the ecosystem has taken (Cilium as the default datapath on the major managed Kubernetes offerings, and Calico's eBPF dataplane), and the cgroup design removes problems nftables could only mitigate.
8. **The nftables enforcer is removed.** There is one datapath and one conformance target. Hosts without cgroup v2 or BTF report `unavailable` with the fix (§4.15). The nftables code in #1632 stays until the eBPF executor replaces it in phase 2, then it is deleted.
9. **Denied connects return `EPERM`.** The app gets "Operation not permitted" at once. Ingress denials stay silent drops, since there is no local socket to return an error to.
10. **Existing containers block `enforce` until redeployed.** A node cannot enter `enforce` until every Temps container on it runs under `temps.slice`. The node page lists the containers that need a redeploy, with a "Redeploy" action for each.
11. **Follow Cilium's model, not its implementation** (§9). Identity-based policy, eBPF maps, push propagation, audit mode and flow visibility, on cgroup hooks inside the existing agent rather than a network stack of our own.
12. **Temps assigns addresses on its own networks** (§4.8, gap 4): round-robin from the node's /24, a 5-minute cool-down, reuse only after every holder acknowledges. Containers Temps did not create are disconnected from Temps networks. Docker's own picks are fenced into the top eighth of the subnet on networks created from now on; existing networks are not recreated for it, so enforcement never forces a network outage. Found necessary by the prototype (§4.16, case 13).
13. **A node that was dropped is distrusted until it catches up** (§4.15). Restricted workloads fail closed on remote peers; open workloads are unaffected.
14. **Identity in the VXLAN header is deferred.** The acknowledgement rule closes the known hole; carrying identity in the packet is revisited if that rule proves costly.
15. **Address sizing copies Kubernetes:** a /24 per node (`subnet_prefix_len`, unchanged) and at most 110 containers per node (`max_workloads`, like kubelet `--max-pods`), keeping the range at least twice the limit as GKE does. Larger subnets per node (/21, /18, /16) were considered and not chosen: a Linux bridge holds at most 1,023 ports anyway, and larger subnets need a much larger pool that collides with common networks.


## 12. Open questions

To settle during phase 2:

1. **Collect-metadata mode on the VXLAN device.** The per-node source-subnet check (§4.15, rule 3) needs the tunnel's outer source, which a TC program reads only from a collect-metadata VXLAN device. To verify that switching the existing device does not disturb the overlay; otherwise the check falls back to a reverse-path check on the per-node routes.
2. **User Compose files that set their own addresses on a Temps network.** Proposed: refused at deploy with a typed error naming the service and the network, since an address Temps did not assign cannot carry an identity.

## 13. Test plan

Seven layers, cheapest first. Anything that needs Docker, root or a real kernel skips gracefully when unavailable, per CLAUDE.md; nothing is marked `#[ignore]`.

| Layer | Where | What |
|---|---|---|
| 1. Unit | in-file Rust tests; run on macOS and in CI | rule → map-entry compiler and precedence (§4.9); allocator cool-down, acknowledgement and round-robin (§4.8); snapshot scoping; status transitions; admission validation (every error names its field); sandbox policy migration 1:1; last-known-good on source failure; distrust after a node is dropped |
| 2. Program tests | Linux as root (the test machine; skips elsewhere) | load the real programs into a throwaway cgroup and drive them with real sockets: allow, `EPERM`, internet on and off, unknown slice denied, shadow and trial; drive the ingress program with crafted packets through `BPF_PROG_TEST_RUN` |
| 3. Conformance | the test machine, against Docker, Firecracker and microsandbox | the §4.12 matrix plus the 14 prototype cases (§4.16), including case 13 (address reuse); IPv6; UDP and DNS; port ranges; a host process is never affected |
| 4. Multi-node e2e | the VM cluster below | a restricted deployment on worker-1 reaches its linked database on worker-2 and not an unlinked one; a sandbox on worker-2 with internet off reaches only the Postgres cluster it was given; a peer restarted with a new address is reachable again within 2 s and its old address is in no map; an open workload is never blocked, and shadow reports zero "would block" for it |
| 5. Failure | the VM cluster | reboot a worker: nothing restricted is open before the agent's first pass; kill the agent: enforcement continues; restart Docker: drift detected and re-applied; control plane down: the last policy stays, and no address is reused; a worker cut off past 10 minutes: distrusted until it catches up, and its node page says why; a 10-minute start/stop churn loop: no connection is ever admitted under a previous owner's identity |
| 6. Performance | a 1 vCPU / 2 GB VM | `wrk` throughput and latency with no policy, eBPF and nftables; CPU per 100k connections; kernel and agent memory with a few hundred rules; time to apply 1,000 identities. Results written into §4.16 |
| 7. Release safety | dev cluster and the VM cluster | upgrade test: restore a database from the previous release, assert no row or container changed, every node `legacy`, every workload `open`; frontend Bun tests for every §5 message and each create form; a written acceptance pass of console scenarios (written with phase 3); one node in `shadow` with real workloads for a few days before anything is enforced |

**Environments:**

- **The Colima dev cluster** keeps testing everything except the datapath (API, snapshots, console, admission) with nodes in `legacy` or `shadow`. Its Docker-in-Docker workers have no systemd, so workload slices and the programs probably cannot run there.
- **A VM cluster on the maintainer-run Linux machine with KVM:** three VMs (control plane and two workers, or one control plane and three workers when sandbox placement is tested), each with systemd, cgroup v2 and BTF, joined with `temps join`. VMs can be rebooted and partitioned freely; the host itself is never rebooted without the maintainer's go-ahead.
- **The machine itself** runs layers 2, 3 and 6.

**Acceptance:** the maintainer runs the acceptance pass at the end of phases 2 and 3.
