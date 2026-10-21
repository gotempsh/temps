# ADR-055: A workload plane with pluggable runtime drivers

**Status:** Proposed
**Date:** 2026-10-21 (amended 2026-10-22 after review: DNS claim ownership and backfill, the node stream's change model, admission order, offline restarts, macOS testing, crate layout)
**Author:** David Viejo
**Builds on:** Two ADR numbers were each used twice, so every entry names its
ADR by title and file:
- ADR-010, provider boundary traits
- ADR-011, internal DNS
- ADR-013, sandbox egress
- ADR-040, runtime context
- ADR-045, host Docker socket grant
- ADR-048, multi-node sandboxes
- ADR-051 "Sandbox service access" (`051-sandbox-service-access.md`)
- ADR-051 "Public ingress surface" (`051-public-ingress-surface.md`)
- ADR-052 "Common workload model" (`052-common-workload-model.md`)
- ADR-052 "Pluggable health probes for workers" (`052-pluggable-health-probes-for-workers.md`)
- ADR-053 "Distributed runtime health and worker uptime" (`053-distributed-runtime-health-and-worker-uptime.md`)

In the rest of this document, "ADR-052" alone always means the common
workload model, and "ADR-051" alone means sandbox service access.

**Supersedes:** ADR-052 (common workload model) §5 (limits, placement, worker API, labels)
**Amends:** ADR-040 "workload backend: no single runtime trait"; ADR-010 §1 for `SandboxProvider`
**Depends on:** ADR-052 (common workload model) §1 to §4, which is also Proposed. The two are accepted together, or this ADR absorbs those sections.

---

## Context

Temps runs three kinds of workload, projects, sandboxes and managed
databases, and each one has its own full deployment stack. All three call
bollard directly.

| | Projects | Sandboxes | Databases |
|---|---|---|---|
| Launch | `ContainerDeployer` (`temps-deployer/src/lib.rs:1140`) | `SandboxProvider` (`temps-agents/src/sandbox/mod.rs:805`) | `ExternalService` engines, bollard inline (`externalsvc/postgres.rs:925`) |
| Worker API | `/agent/containers/*` | `/agent/sandboxes/*` | `/agent/services/*` |
| Placement | `NodeScheduler` (`target_nodes`, `target_labels`, anti-affinity) | `services/placement.rs` (allow-list, fewest sandboxes) | `external_services.node_id`, chosen by hand |
| Limits | `ResourceLimits` | flat `cpu_limit`, `memory_limit_mb`, `pids_limit`, `disk_size_mb` | `ServiceResourceLimits` |
| Storage | none for single containers; named volumes for Compose only | bind mount or named volume, ext4 image, snapshots | named volumes, recreate guard |
| Ingress | route table, IP rules, password wall | preview gateway and preview password | none |
| Egress | open | a Node.js sidecar per sandbox | open |
| Internal DNS | `<env>.<project>.temps.local` (multi-A to node proxies) | Docker's embedded DNS | `<svc>.temps.local`, `primary.`/`replica.` |

The rest of the system adds more of the same. Crons are HTTP GETs to a
running deployment (`database_cron_service.rs`). Scale-to-zero has its own
lifecycle trait (`temps-proxy/src/on_demand.rs`). Static sites use
`StaticDeployer`. Compose shells out to the `docker compose` CLI and only runs
on the control plane.

ADR-052 (common workload model) unifies service access, variables, network
rules, limits and placement eligibility for deployments and sandboxes. It
leaves out databases, runtimes, ingress, storage, DNS and new kinds, and it
keeps the code that creates containers separate.

### The cost, with evidence

1. **Database creation exists twice, and the two copies have drifted.** The
   control plane creates database containers in each engine (for example
   the host config at `postgres.rs:925`). The worker creates them in
   `create_service` (`temps-agent/src/service_handlers.rs:276`), one path for
   every engine. The two differ:
   - labels: `temps.*` on the control plane, `sh.temps.*` on the worker
     (`service_handlers.rs:447`);
   - restart policy: `always` against `unless-stopped`
     (`service_handlers.rs:416`);
   - hardening: every control-plane engine (Postgres, Redis, MongoDB,
     MariaDB, RustFS, Garage, S3) sets `no-new-privileges` and
     `pids_limit: 512`. The worker path sets neither, and has no healthcheck.
2. **Backups do not run for databases on worker nodes.** Nothing in
   `temps-backup` reads `external_services.node_id`, and every engine runs
   against the control plane's local Docker. The worker has
   `/agent/services/backup` and `/agent/services/restore`
   (`temps-agent/src/server.rs:204-208`), but nothing outside the agent calls
   them.
3. **Limits are ignored without saying so.** `ResourceLimits.disk_limit_mb` is
   never applied by `DockerRuntime`. A sandbox's `disk_size_mb` is ignored on
   Docker.
4. **`DockerRuntime` ignores `DeployRequest.network_name`** and always uses its
   own network.
5. **Remote sandboxes lose features.**
   - 15 operations are refused with 422
     (`temps-agents/src/sandbox/remote.rs:1164-1600`): PTY, snapshots and
     restore, application networking, the git, model and MCP relays, disk
     resize, image rebuild, workspace volumes and Firecracker.
   - Previews are refused separately, as a 404 in the proxy
     (`temps-proxy/src/preview_auth.rs:566-572`).
6. **Worker public ingress drops secured routes, and sometimes every route.**
   - A route with a redirect, a path route, a sleeping environment, attack
     mode or any security setting is excluded
     (`temps-routes/src/route_table.rs:3469-3497`).
   - Worse, workers receive **no public routes at all** while any one of
     these is turned on anywhere (`temps-routes/src/route_sync.rs:526-536`):
     global rate limiting, security headers, any IP access rule, project IP
     rules, project rate limits, or a custom request-policy provider.

   The policy code lives only in the control-plane proxy.
7. **A sandbox preview without a password is public** to anyone who has its
   id. This is by design today: the 64-bit unguessable `public_id` is the only
   gate (`preview_auth.rs:534-536`).
8. **Each sandbox has its own egress sidecar,** a Node.js process capped at
   128 MB, 64 pids and 0.25 CPU. The cap is an upper bound, not what it
   uses, but on the 1 vCPU / 2 GB target the cost still grows with every
   sandbox instead of once per node.
9. **Internal DNS names have no owner.**
   - `service_endpoints` is unique on `(fqdn, record_type, target_ip)`.
   - On conflict, `DnsRegistry::replace_endpoints_for_owner` upserts and
     overwrites `owner_kind`/`owner_id` (`dns_registry.rs:384-410`). A second
     owner publishing the same name and address silently takes the record
     over, and with a different address the two owners' records are merged
     under one name.
   - The upsert is not an accident. Deployment records are owned by the
     *deployment* (`owner_kind = deployment`, `owner_id = deployment_id`,
     `temps-dns/src/services/deployment_publisher.rs:167-168`), which is a new
     id on every redeploy, while their targets are every node's bridge IP,
     which never change. Every redeploy therefore collides with the previous
     deployment on the same `(fqdn, A, target_ip)`. Before the upsert that
     collision aborted the publish and stalled DNS on redeploy (the comment at
     `dns_registry.rs:384-390`). Ownership cannot be enforced while the owner
     of a stable name is a per-revision id.
   - The namespace is flat, and nothing checks project slugs and service
     names against each other. An environment `primary` in a project `foo`
     publishes `primary.foo.temps.local`, which is also the primary record of
     a clustered Postgres named `foo`.
10. **Removing a node can re-home a database onto the control plane.** Node
    removal refuses while the node holds live sandboxes or serving containers,
    or is the only target of a placement (`node_service.rs:1923-1998`).
    `external_services.node_id` and `service_members.node_id` are
    `ON DELETE SET NULL`, NULL means "control plane", and no removal guard
    counts them. The result is the same silent re-homing the sandbox guard was
    added to prevent.
11. **Every new feature repeats the work.** A function or cron product today
    would need its own launch path, worker endpoints, placement, network rules,
    ingress gate and DNS registration.

### Goals

- A product (projects, sandboxes, databases, and later functions and cron
  jobs) never reimplements deployment, networking, ingress, storage,
  permissions or security.
- Runtimes are pluggable: Docker today, Firecracker and microsandbox for any
  kind, WASM later.
- Multi-node works the same way for every kind, and a product only states
  placement preferences.
- The design meets both hardware targets in `CLAUDE.md`:
  - it runs well on 2 vCPU / 4 GB with the database on the same host;
  - it runs on 1 vCPU / 2 GB with an external database.

  On either, Temps itself (proxy, Postgres when local, the reconciler, image
  builds) competes with user workloads. The plane therefore reserves capacity
  for it (§5) and keeps hot paths free of extra threads.

## Decision

### 1. Four layers

```
┌──────────────────────────────────────────────────────────────────────┐
│ L3 Products     Projects · Sandboxes · Databases · Functions · Cron  │  lifecycle, UX, permissions
├──────────────────────────────────────────────────────────────────────┤
│ L2 Workload plane (temps-workload)                                   │  spec → reconciled instances
│   admission · scheduler · network policy · DNS · ingress endpoints · │
│   volumes · env delivery · security profiles · health · events       │
├──────────────────────────────────────────────────────────────────────┤
│ L1 Runtime drivers   docker · firecracker · microsandbox · wasm      │  run instances on one node
├──────────────────────────────────────────────────────────────────────┤
│ L0 Fabric   nodes · WireGuard/overlay · resolvers · route table and  │  exists today
│             proxy · nftables · artifact distribution                 │
└──────────────────────────────────────────────────────────────────────┘
```

**The rule:** a product never calls bollard, never picks an address or host
port, and never writes nftables rules, route rows or DNS records. It submits a
spec to the plane and reads the status the plane reports.

Each product keeps its own state machine (ADR-052 §6): blue/green and
rollback, suspend and wake, snapshots, database upgrades and failover. Each
product is a controller over the plane, in the way a Kubernetes operator is a
controller over pods.

`temps-workload` is the leaf crate ADR-052 already introduces, and holds the
types from ADR-052 §1 to §3 and §3 here. The plane itself is split across a
few crates by concern (§18).

### 2. Workload objects: spec, status, conditions, events

```
workloads(
  id, owner_kind, owner_id, name,
  spec            jsonb,      -- WorkloadSpec, typed in Rust
  generation      bigint,     -- per workload: bumped on every spec change
  observed_generation bigint, -- last generation the plane fully applied
  change_seq      bigint,     -- global: next value of workload_change_seq on every write
  status          jsonb,      -- WorkloadStatus, typed in Rust
  owner_workload_id,          -- for owned workloads (backup jobs, sidecars)
  finalizers      text[],
  created_at, updated_at, deleted_at
)
workload_instances(
  id, workload_id, revision, node_id, driver, handle,
  address, phase, ready, conditions jsonb, change_seq bigint,
  created_at, updated_at
)
workload_events(
  id, workload_id, instance_id, reason, message,
  count, first_seen_at, last_seen_at
)
```

- **Two different counters.**
  - `generation` is per workload. It answers "has the plane applied this
    spec yet?" by comparing it with `observed_generation`.
  - `change_seq` comes from one Postgres sequence shared by every row the
    plane writes: workloads, instances, endpoints, name claims and rules. It
    is a **resource version**: a node compares it with the copy it holds to
    know whether that copy is current.
  - `change_seq` is **not** the node stream's cursor. Sequence values are
    allocated before commit, so a transaction holding a lower value can
    commit after a reader has already passed a higher one, and a cursor
    built on it skips that row for ever. The stream (§7) uses a commit
    visibility watermark instead.
- **Status is written only when it changes, in batches.** Heartbeat reports
  that repeat the last known state write nothing. This matters most on the
  1 vCPU / 2 GB target, where the database is on another host and every write
  pays a network round trip.
- **Reconciliation acts on current state.** The plane compares desired state
  with actual state and fixes the difference, so applying the same spec twice
  has no extra effect. A crash or a missed notification heals on the next
  pass. The cleanup code for orphans and retained candidates that each product
  carries today goes away.
- **Conditions explain the state.** Each condition has a kind (`Scheduled`,
  `ArtifactReady`, `Running`, `Ready`, `PolicyEnforced`, `LimitEnforced`), a
  status (`True | False | Unknown`), a machine reason, a human message and the
  time of its last change. Messages name nodes and numbers: "0/3 nodes
  eligible: node-a is draining, node-b has 210 MB free, node-c lacks the
  firecracker driver".
- **Events are bounded.** Repeats of the same reason increment `count`.
  Retention is the last 100 per workload and at most 7 days. Every product
  shows the same history view.
- **Ownership and finalizers.** A database owns its volume and its backup
  jobs. Deleting an owner removes what it owns, through one cleanup sweep. A
  finalizer (a role drop, a volume purge, a DNS withdrawal) holds deletion
  until the cleanup has happened.
- **Products can wait synchronously.** `WorkloadPlane::apply(spec)` returns
  the id. `apply_and_wait(spec, condition, timeout)` returns a typed error
  that carries the failing condition's reason and message. A chat that asks
  for a sandbox needs an answer in seconds; it does not poll.

### 3. The spec

```rust
pub struct WorkloadSpec {
    pub owner: WorkloadRef,            // ADR-052 §1; kinds: Deployment | Sandbox | Service | Function | Job
    pub artifact: Artifact,            // OciImage { reference, digest } | WasmComponent { blob, digest } | Rootfs { digest }
    pub isolation: Isolation,          // InHost (wasm, inside the node runtime) | Process | MicroVm
    pub process: Process,              // command, args, workdir, user
    pub env: EnvSpec,                  // ADR-052 §3 layered assembly; delivery Baked | PerExec
    pub resources: Resources,          // §5
    pub volumes: Vec<VolumeMount>,     // §12
    pub ports: Vec<PortSpec>,          // name, container port, protocol, app_protocol (http | tcp | udp)
    pub network: NetworkSpec,          // §9
    pub probes: Probes,                // §13: startup, readiness, liveness
    pub placement: PlacementSpec,      // §8
    pub lifetime: Lifetime,            // LongRunning | RunToCompletion { timeout } | ScaleToZero { idle }
    pub security: SecurityProfile,     // §4
}
```

- **Ports are named and never host ports.** The plane allocates host
  bindings where a driver needs them, the way remote deploys already ask the
  agent to pick a port.
- **Endpoints and names are not part of the spec.** Hostnames (§11) and
  internal DNS names (§10) belong to the product's object (an environment, a
  sandbox, a database, a function) and outlive every revision. They are
  separate objects that point at a workload and revision. This is the
  Kubernetes Service/HTTPRoute split. With that split:
  - blue/green is one write to the endpoint's target revision;
  - a scale-to-zero endpoint keeps its route while no instance exists;
  - a name is never withdrawn and re-claimed during a redeploy.
- **Isolation is named for what contains the code.** `InHost` (WASM) runs
  inside the node runtime process, so an escape from the WASM runtime lands
  in the agent. `Untrusted` workloads may only use `InHost` with an explicit
  operator setting on the node.
- **Every instance is labelled.** It carries `sh.temps.kind`,
  `sh.temps.workload_id` and `sh.temps.revision` next to the existing
  `sh.temps.*` labels, restating ADR-052 §5. Logs, metrics, the
  metadata-egress rules and the reconciler find containers by these labels.

Image building stays with the products that build (projects, functions).
The plane only consumes finished artifacts.

### 4. Admission: one gate for every spec

`apply` is the only way to create or change a workload. Every spec passes
through one function, in this order:

1. defaults for the kind;
2. the security profile, chosen together with the host Docker socket grant
   (ADR-045). `HostPrivileged` depends on the grant, so the grant is checked
   while the profile is chosen, not after it:
   - a spec for a granted project gets `HostPrivileged`;
   - a spec that asks for `HostPrivileged` without the grant is refused;
   - every other spec gets its kind's profile, and the socket is never
     mounted for it;
3. image allow-lists (`TEMPS_ALLOWED_*_DOCKER_IMAGES`, unchanged);
4. quotas;
5. validation.

A rejection is a typed error that names the rule and the workload.

| Profile | For | Rules |
|---|---|---|
| `HostPrivileged` | ADR-045 granted projects only | today's grant semantics, decided by admission instead of the deployer |
| `Trusted` | deployments | today's hardened host config: `cap_drop ALL`, `no-new-privileges`, pids 512, init, read-only secrets |
| `Data` | databases | `Trusted`, plus a fixed restart policy, an engine healthcheck and a recreate guard on the volume. The pids cap is set per engine, starting from today's 512, because it counts threads and MongoDB and MariaDB use one thread per connection |
| `Untrusted` | sandboxes, functions | `Trusted`, plus: egress at most `restricted`, never `Administrator` credentials (ADR-051), and safety caps (pids, a memory ceiling) that hold regardless of §5. Prefer a microVM where one is available |

The profiles are modelled on Kubernetes' Pod Security levels (privileged,
baseline, restricted). Applying the profile in one place is what removes
defect 1: both database paths get the same host config, because neither path
builds one.

### 5. Resources: optional requests and limits

```rust
pub struct Resources {
    /// Reserved capacity. The scheduler only counts it on nodes that enforce reservations.
    pub requests: Option<ResourceAmounts>,
    /// Hard cap, applied by the driver.
    pub limits: Option<ResourceAmounts>,
}
pub struct ResourceAmounts { pub cpu_millis: Option<u32>, pub memory_mb: Option<u32>, pub disk_mb: Option<u32> }
```

- **Neither set (the default):** the workload is best effort and is placed by
  observed load, using today's `LeastLoaded` logic with the 0.90 threshold.
- **A limit set:** it is always applied. A driver that cannot apply it sets
  `LimitEnforced = False` with the reason ("docker: disk limits need an
  XFS/overlay2 project quota on this host"). It is never ignored silently.
- **A request set:** it is recorded, and only counted by the scheduler on
  enforcing nodes.
- **`nodes.resource_policy = best_effort | enforced`** (default
  `best_effort`). The control plane is node 0 and has the same column. It is
  changed through the API and audit-logged.
  - On an enforcing node, a workload is placed only if the sum of
    reservations stays within capacity. A workload without a request is
    counted at its kind's default, and the console marks that value as
    "assumed".
  - The setting is per node because a cluster mixes the two cases: a node
    for databases wants no overcommit, while a node for sandboxes or previews
    wants density.
- **Temps reserves capacity for itself.** Each node has a `system_reserved`
  amount (cpu, memory), stored on the node row with a default per role. The
  initial defaults below are estimates. Phase 5 replaces them with values
  measured on both hardware targets:

  | Role | Default reserve | Covers |
  |---|---|---|
  | control plane, local database | 0.5 vCPU, 1 GB | proxy, Postgres, reconciler, builds |
  | control plane, external database | 0.25 vCPU, 512 MB | proxy, reconciler, builds |
  | worker | 0.25 vCPU, 256 MB | agent, resolver, egress proxy |

  - The reserve is a **scheduling deduction**. Schedulable capacity is the
    node's capacity minus the reserve, in both `best_effort` (where the 0.90
    load threshold applies to that remainder) and `enforced` mode.
  - It is not CPU priority. Temps does not run the local PostgreSQL: the
    operator starts it (`docs/installation/page.mdx`), so Temps cannot place
    it in a cgroup. The proxy and the reconciler share one process, so
    neither can be given priority over the other.
  - What protects Temps from its workloads is the opposite direction:
    workload containers get a lower CPU weight than the host's default, so
    under contention the proxy and the database win. This works on every
    install, because Temps creates the workload containers.
  - Image builds keep their existing per-node slots (`build_limits`). On a
    control plane with two cores or fewer, the default is one concurrent
    build.
- **Security caps are separate.** The `Untrusted` caps from §4 hold even when
  the user sets nothing, and the console labels them as security limits.
  Existing deployments keep their configured values as limits.

### 6. Runtime drivers

```rust
#[async_trait]
pub trait RuntimeDriver: Send + Sync {
    fn capabilities(&self) -> &DriverCapabilities;
    async fn prepare(&self, artifact: &Artifact) -> Result<PreparedArtifact, DriverError>;
    async fn create(&self, spec: &InstanceSpec, attach: &Attachments) -> Result<InstanceHandle, DriverError>;
    async fn start(&self, handle: &InstanceHandle) -> Result<(), DriverError>;
    async fn stop(&self, handle: &InstanceHandle) -> Result<(), DriverError>;
    async fn remove(&self, handle: &InstanceHandle, purge: bool) -> Result<(), DriverError>;
    async fn inspect(&self, handle: &InstanceHandle) -> Result<InstanceStatus, DriverError>;
    async fn logs(&self, handle: &InstanceHandle, query: LogQuery) -> Result<LogStream, DriverError>;
    async fn stats(&self, handle: &InstanceHandle) -> Result<InstanceStats, DriverError>;
    // Gated on capabilities; default bodies return DriverError::Unsupported.
    async fn exec(&self, handle: &InstanceHandle, cmd: ExecSpec) -> Result<ExecStream, DriverError>;
    async fn snapshot(&self, handle: &InstanceHandle) -> Result<SnapshotRef, DriverError>;
}

pub struct DriverCapabilities {
    pub artifacts: ArtifactKinds,            // oci, wasm, rootfs
    pub isolation: Isolation,
    pub network_attachment: AttachmentKind,  // DockerNetwork | Tap | SdkPolicy | InHost (WASI socket allow-list) | None
    pub volumes: VolumeKinds,                // named, block image, preopened dir
    pub exec: bool,
    pub snapshot: bool,
    pub resize_disk: bool,
    pub limits: LimitSupport,                // cpu, memory, pids, disk
}
```

- **Runtime, network and storage are separate plug points.** This is the
  Kubernetes CRI / CNI / CSI split, done as Rust traits rather than external
  processes.
  - A driver produces an attachment point: a Docker network (Docker), a TAP
    device (Firecracker), the SDK's network policy (microsandbox), or an
    in-process socket allow-list (WASM).
  - `NetworkAttacher` (§9) applies addressing, DNS and policy to that point.
  - `VolumeProvider` (§12) supplies volumes.

  Today every sandbox backend implements its own networking, which is why
  policy cannot be uniform. A new driver should only have to start processes.

  For Docker, the attachment point is a **Docker network**, not a namespace
  wired by hand. The attacher chooses which networks a container joins and
  installs nftables rules around them. Leaving Docker networking would break
  what depends on Docker's embedded DNS today: the preview gateway's
  `temps-sandbox-<sid>` names (`temps-preview-gateway/src/main.rs:42`,
  resolved at `:241-243`) and Compose service names.
- **Capabilities are filters, not hints.** If a spec needs a capability the
  driver lacks, such as network policy on a driver with no attachment point,
  the instance is never placed on that driver. ADR-052 §4's rule applies to
  every kind: there is no "connected but unrestricted".
- **Sandbox-only operations stay sandbox-specific.** File access, PTY, and
  the model, git and MCP relays become an optional `SandboxExt` trait that
  drivers may implement. `SandboxProvider` becomes a facade inside the sandbox
  product over `RuntimeDriver + SandboxExt`.
  - ADR-010's "one provider trait per domain" moves down a level: the
    provider trait is `RuntimeDriver`.
  - The eval harness and CI guard move with it.
- **The drivers, and where each comes from:**
  - `docker`: extracted from the creation code in `DockerRuntime` and
    `DockerSandboxProvider`.
  - `firecracker` and `microsandbox`: the existing backends.
  - `local`: dev only, explicit opt-in, as today.
  - `wasm` (wasmtime): later, in phase 10.

  Firecracker for deployments needs no new driver, only a different isolation
  request.
- **Two levels of "pluggable".** A *driver* runs instances on one node. A
  *target* is where nodes come from. Today the only target is Temps' own
  nodes. A Kubernetes cluster or a cloud provider could later be a target that
  looks to the scheduler like one large node with its own placement. That is
  not decided here.

ADR-040 said there would be no single runtime trait. This ADR reverses that
at the driver level, on purpose: a fourth product cannot be built cheaply
while every product talks to its own backend family.

### 7. Node runtime and one desired-state stream

- **Every node runs the same node runtime,** including the control plane as
  node 0. It holds the local drivers, attachers and volume providers, and
  reconciles the instances assigned to its node. Local and remote stop being
  different code paths, which removes the three worker APIs and the drift in
  defect 1.
  - On the control plane, the node runtime is hosted by `temps serve`, the
    process that already places containers. It is never hosted by
    `temps proxy` in the split topology (ADR-017).
- **One stream per node.**
  - Endpoint: `GET /internal/nodes/{id}/state?since=W`, a long-poll returning
    typed objects, plus per-kind acks.
  - It reuses the transport of `/routes/snapshot` and `/dns/changes`, but not
    their change model. `DnsRegistry::get_changes_since`
    (`dns_registry.rs:606-643`) answers **every** change with a full
    snapshot, because deleted rows leave no tombstone. A per-node delta
    stream has to solve that and three more problems, below.
  - Workload instances come first. Routes and DNS move onto the stream in
    later phases instead of each keeping its own cursor and ack table.
- **The change log.** The stream reads one table, not the object tables:

  ```
  node_state_changes(
    id          bigserial,
    node_id     int NULL,      -- NULL: every node
    object_kind text,          -- workload | instance | dns_claim | dns_endpoint | network_rule | ...
    object_id   bigint,
    xid         xid8 NOT NULL DEFAULT pg_current_xact_id(),
    created_at  timestamptz
  )
  ```

  A row carries no payload. It marks one object dirty for one node (or for
  all of them). It is written **in the same transaction** as the object
  change, by triggers on the object tables, so no code path can change an
  object without telling the nodes.
- **Reads are level-triggered.** For every object marked in the window, the
  server reads the object's *current* state and sends:
  - `upsert`, with the current object, when it still exists and is still in
    the node's view;
  - `remove` otherwise.

  The node never applies a historical state, so the order of the markers
  inside a window does not matter.
- **Tombstones.** A deleted object's marker resolves to `remove`, because the
  object is gone. The marker *is* the tombstone, so deletes reach nodes
  without keeping deleted rows around.
- **Node reassignment.** An instance moved from node A to node B no longer
  matches A's view, so a filter on `node_id = A` would never tell A to stop
  it. The trigger on `workload_instances` therefore writes a marker for
  `OLD.node_id` **and** `NEW.node_id` whenever `node_id` changes. A's marker
  resolves to `remove`, B's to `upsert`.
- **Out-of-order commits: a visibility watermark.** The cursor is not
  `change_seq` (§2) but a transaction-id watermark.
  - On each read the server takes `W = pg_snapshot_xmin(pg_current_snapshot())`.
    Every transaction with an id below `W` has finished, committed or rolled
    back.
  - A read returns the markers with `since <= xid < W` for the node, and the
    node stores `W` as its next `since`.
  - A transaction still in flight has an id of at least `W`, so its markers
    are picked up by a later read, whatever order the transactions commit
    in. Nothing is skipped; a long transaction only delays the stream.
  - Plane transactions are short. The watermark's lag behind the newest
    marker is exported as a metric, and a lag above 30 s is logged at `warn`
    with the oldest in-flight transaction's age.
  - `xid8` is 64-bit and epoch-extended, so it does not wrap.
  - A re-scan window was rejected: it trades a correctness guarantee for a
    tunable, and still loses a marker whose transaction outlives the window.
- **Rows without a node.** Some objects have no `node_id`:
  - `workloads`: marked for every node that hosts one of its instances
    (the trigger joins `workload_instances`).
  - `dns_name_claims` and DNS endpoints: one marker with `node_id NULL`.
    Every node's resolver serves the whole `*.temps.local` zone, so every
    node needs every name.
  - `network_policy_rules`: marked for every node hosting an instance of the
    rule's subject.
  - A node that receives a new instance learns its workload and that
    workload's rules from the instance's `upsert`: the server expands it.
    A node therefore never misses a rule written before its instance landed.
- **The full snapshot stays.** The server answers with the node's whole view
  and the current `W` when:
  - `since = 0` (first connection, or a wiped node);
  - `since` is below the retention horizon (markers older than the lowest
    cursor acked by any active node are deleted, and never kept more than
    24 h);
  - `since` is above the database's current `xmax`, which means the
    database was restored from a dump and transaction ids restarted.

  A full snapshot is always correct, so every doubt resolves to one.
- Work is proportional to changes: one indexed range scan on
  `(node_id, xid)` per read, never a table scan.
- **The local snapshot never holds secret values.**
  - A worker keeps its last received state on disk so it can restart
    instances while the control plane is unreachable. That file holds specs
    and secret references only, with mode 0600 and owned by the agent.
  - Secret values travel only in the instance-create call. They live where
    they live today: the per-instance `/run/secrets` files, mode 0400.
  - Neither the stream nor its logs carry secret values. A test asserts that
    a seeded secret never appears in the snapshot file.
- **Interactive operations use one RPC surface:**
  `/agent/instances/{id}/{exec,logs,stats,files,attach,snapshot}`, served by
  the node runtime and gated on the instance's driver capabilities. Remote
  sandboxes get previews, snapshots and PTY because they run on the same code
  as local ones (defect 5).
- **Instance status rides on the heartbeat,** batched.
- **Workers keep running without the control plane, with one limit.**
  `Baked` delivery (ADR-052 §3) and the local snapshot let a worker keep its
  instances running while the control plane is down:
  - **Restarting an existing instance works.** The container, its
    environment and its `/run/secrets` bind mount are all still on the node.
    Docker's restart policy and the node runtime both restart it with no call
    to the control plane.
  - **Recreating an instance needs the secret values**, and the snapshot
    does not have them. The node first reuses the instance's existing
    secret files: they are kept per instance under the data directory and
    survive host reboots (`temps-deployer/src/docker.rs:1171-1178`).
  - When those files are gone (the instance is new to this node, or its
    directory was removed), the node does not start the instance with
    missing secrets. It reports `Running = False` with reason
    `SecretsUnavailable` and a message naming the instance and the
    unreachable control plane, and retries when the stream reconnects.
- **Artifact distribution is a plane service.** The existing
  `ensure_worker_image_on_remote` (pull from a registry, or push from the
  control plane) is generalised to WASM components and rootfs images.

### 8. Scheduling and placement

```rust
pub struct PlacementSpec {
    pub nodes: Option<Vec<NodeId>>,     // required set; None = any eligible node
    pub labels: Option<LabelSelector>,  // stacks with `nodes`; today's target_labels semantics
    pub spread: bool,                   // replicas on different nodes (today's anti_affinity)
    pub replicas: u32,
}
```

- **The scheduler first filters, then ranks.**
  - Filter (shared by every kind): node status, draining, the operator
    allow-list, role (including `temps.sh/role=dedicated`), labels,
    architecture, driver capability, isolation, volume location, the socket
    grant, and capacity on enforcing nodes.
  - Rank (a strategy per kind):

    | Kind | Strategy |
    |---|---|
    | Deployments | least loaded, spread |
    | Sandboxes | fewest live sandboxes |
    | Databases | pinned, with anti-affinity across cluster members |
    | Jobs | the owner's node, then least loaded |

  - **Placements in flight count.** Observed load arrives with the
    heartbeat, seconds late. Instances that are placed but not yet reported
    are added to the node's load at their limit (or the kind's default), so
    a burst of sandbox creations does not all land on the node that looked
    idle. Sandbox placement already does this with live counts.

- **One placement field for every kind.**
  - Deployments keep `target_nodes`, `target_labels` and `anti_affinity`,
    with environment settings overriding project settings, so production can
    be pinned while previews run anywhere.
  - A sandbox's requested node becomes a set of one.
  - A database's `node_id`, and each cluster member's, becomes a set of one.
- **Pinning is strict.** If no pinned node is eligible, the workload stays
  `Scheduled = False` with a reason. It never moves elsewhere quietly.
  Pinning to a few nodes is how a workload gets failover.
- **Volumes pin.** A workload with a node-local volume is pinned to that
  node. The console shows "pinned by volume `<name>`", so the user can see why
  it cannot move.
- **Pinning is recommended, not imposed.**
  - `WorkloadPlane::explain(spec)` is a scheduler dry run. It returns the
    recommended node set and the reasons: the same node as a linked database,
    the image already cached, a matching architecture, headroom.
  - Creation forms preselect that set and show the reasons; "Any node" stays
    one click away.
  - An existing unpinned workload gets a recommendation card ("api moved
    between 3 nodes in 7 days; its database is on node-a"), never an automatic
    migration.
- **Drain and removal protect every kind.** Today removal refuses for
  sandboxes, serving containers and single-node placements. It is extended
  to every workload whose last eligible node is the one being removed,
  databases included (defect 10). The refusal lists the workloads; the
  operator repins or stops them.

### 9. Networking

- **Every instance gets an identity on the overlay.** It is attached to its
  node's `compute_cidr` network through `NetworkAttacher`, and policy is keyed
  by workload identity, never by literal IPs.
- **ADR-052 §4's `network_policy_rules` apply to every kind,** including
  databases, functions and jobs. Links create rules. Rules are allow and deny,
  explicit, and shown in the console as the effective policy.
- **Egress profiles:**

  | Profile | Reaches | Default for |
  |---|---|---|
  | `open` | anything (today's behaviour) | deployments |
  | `restricted` | linked services, plus allowed CIDRs and hostnames | sandboxes, functions |
  | `none` | nothing | |

  The cloud metadata block (`metadata_egress.rs`) keeps its own, earlier
  priority for every profile.
- **One egress proxy per node, not per sandbox.**
  - It identifies the workload by source address, from the same identity map
    the reconciler uses.
  - It applies the workload's hostname and CIDR rules, keeps the public-IP
    check of today's sidecar, and keeps the model, git and MCP relays.
  - It runs inside the node runtime (Rust, the same binary), not as a
    Node.js container. This replaces a sidecar per sandbox (defect 8) with
    one bounded component per node.
  - **How it reaches sandboxes is decided in phase 7, by measurement.** Each
    sandbox sits on its own internal network today, which has no route out.
    There are two candidates:
    - The proxy joins every restricted network. This is simple, but every
      network adds a bridge and its own firewall rules, so it is measured at
      50 sandboxes on the 1 vCPU / 2 GB target.
    - Restricted sandboxes share one internal network, where nftables blocks
      member-to-member traffic and the proxy is the only other member.
  - It is the one place in this ADR where security trades directly against
    memory. Phase 7 must prove that one workload cannot impersonate another.
    Spoofed source addresses are dropped by the attacher's anti-spoofing rule
    (source address bound to the container's interface) before they reach
    the proxy.
- **Attachment per driver:**

  | Driver | Attachment |
  |---|---|
  | Docker | a Docker network on the node (§6), with nftables rules installed around it |
  | Firecracker | TAP device |
  | microsandbox | the SDK network policy |
  | wasm | an in-host socket allow-list compiled from the same rules |

  An attacher that cannot enforce a rule fails the attachment with a typed
  error.

### 10. Internal DNS

- **The plane publishes the records.** They come from each workload's ports
  and its ready instances. Products choose a name and nothing else. One
  publisher, driven by status changes, replaces the deployment publisher, the
  Postgres role reconciler's registration and the 300 s standalone sweep.
- **Names are claimed.**
  - A new `dns_name_claims(fqdn, owner_kind, owner_id, target_workload_id)`
    table, unique on `fqdn`, gives every name exactly one owner (defect 9).
  - The owner is the product object (§3), so a name survives redeploys.
    Only its target changes. The registry's upsert stops overwriting
    `owner_kind`/`owner_id` on conflict and refuses instead.
  - A conflicting claim is refused with a typed error naming both owners:
    "environment `primary` in project `foo` would shadow database `foo`'s
    primary name".
  - Every existing name keeps its form, because apps have them baked into
    environment variables.
  - New kinds get their own subzones (`<fn>.<project>.fn.temps.local`,
    `<sandbox>.sbx.temps.local`), so they cannot collide with existing names.
- **Deployment names move to the environment owner (phase 0).**
  - `<env>.<project>.temps.local` is published today with
    `owner_kind = deployment, owner_id = deployment_id`, a new owner on every
    redeploy. Refusing a cross-owner write while that is true would refuse
    every redeploy, because the new deployment collides with the previous one
    on the same `(fqdn, A, bridge IP)`.
  - Phase 0 therefore publishes these records, and claims the name, as
    `owner_kind = environment, owner_id = environments.id`.
  - The alternative, "treat a newer deployment of the same environment as the
    same owner", was rejected:
    - the registry would have to resolve deployment → environment on every
      conflicting write, a product lookup inside the DNS write path;
    - the owner would still change on every redeploy, so the claim row would
      too, and the claim would not mean what §3 says it means;
    - a late teardown of an old deployment (`delete_by_owner(deployment, old)`)
      could still withdraw the environment's live name.
  - With a stable owner, the publish's delete-own-then-insert never collides
    with itself, and the upsert that hid the collision is no longer needed.
  - Migration: existing `deployment` rows of an environment's current
    deployment are rewritten to the environment owner; rows of any other
    deployment are deleted (the upsert had already overwritten them in
    practice, since every deployment of an environment publishes the same
    bridge IPs). The `deployment` value stays allowed by the CHECK
    constraint for one release, so a rolled-back binary can still publish.
- **Rows under a held name belong to the holder.** When an owner publishes a
  name it holds, rows under that name carrying another owner are leftovers
  (from a rolled-back binary, or written before claims existed) and are
  replaced, with one exception: the losers recorded below.
- **Backfill: a deterministic winner, and the loser is reported.**
  - The migration builds one claim per distinct `fqdn` in
    `service_endpoints`. Where rows of several owners share an `fqdn`, the
    winner is chosen by, in order:
    1. owner kind: `service_member` and `service_role` (managed databases
       and services) first, then `node` and `static`, then
       `environment`/`deployment`;
    2. the oldest row (`created_at`);
    3. the lowest row `id`.
  - Databases win because their records point at container addresses that
    change on restart: a refused database publish would leave its name on a
    dead address. An environment's records point at node proxies, which do
    not change on redeploy, so a refused environment publish keeps resolving
    exactly as it does today. Database names are also the ones baked into
    other apps' variables.
  - Every loser gets a row in `dns_name_conflicts(fqdn, owner_kind, owner_id,
    holder_kind, holder_id, first_seen_at, last_seen_at)`.
  - **Nothing is removed.** The loser's existing rows stay in the zone, so
    the merged answer clients get today does not change. The winner's
    publishes leave a recorded loser's rows alone.
  - **The loser's next publish is refused** with the typed conflict error.
    Its transaction rolls back, so its existing rows stay as they are, and
    `last_seen_at` is bumped.
  - The conflict ends when one side goes: the loser's records are deleted
    (its rows and conflict row go with them), or the winner releases the
    name (deleting its records releases its claims), after which the loser's
    next publish claims the name.
  - A new conflict after phase 0 behaves the same way: the publish is
    refused, nothing is written to the zone, and a conflict row is recorded.
- **Conflicts are visible in three places:**
  - logs: a `warn` naming the name and both owners when a conflict is first
    recorded, `debug` on repeats, so a route reload does not flood the log;
  - `temps doctor`: a check that lists every row of `dns_name_conflicts`
    with both owners and the fix ("rename the environment or the service");
  - the console: the cluster DNS status (`GET /api/cluster/dns/status`)
    returns `name_conflicts`, shown on the DNS status page.
- **Creation is not refused yet.** In phase 0 the product object is still
  created; its name is refused when it is first published. Refusing at
  creation needs each product to know its future names before it publishes
  them, which arrives with the unified publisher in phase 7.
- **Two record modes per port:**

  | Mode | Answer | Default for | Why |
  |---|---|---|---|
  | `proxy` | today's multi-A to every node's internal proxy | `http` ports | the name survives redeploys, even for clients that cache DNS forever |
  | `direct` | addresses of ready instances, plus SRV records | `tcp`/`udp` ports | non-HTTP traffic cannot go through the HTTP proxy |

  `direct` covers databases today and lets a deployment that serves raw TCP
  (a broker image) be addressed by name for the first time.
- **Short names inside a group.** Compose services reach each other as
  `db:5432`, and a project group may want the same.
  - Each group gets a zone, `<group>.grp.temps.local`, holding the members'
    short names.
  - The zone is set as the members' DNS search domain (`dns_search` on
    Docker, `search` in the guest's `resolv.conf`), so `db` resolves to
    `db.<group>.grp.temps.local` for members only.
  - Short names follow policy like any other name.
- **Only ready instances are in the answer** (§13).
- **Answers prefer the same node.** When the asking node has a ready
  instance, its records come first. This is where pinning pays off: an app
  pinned next to its database never leaves the host.
- **Answers depend on policy.** The resolver identifies who is asking by
  source address. A restricted workload only resolves names its rules allow;
  everything else is NXDOMAIN. Answers come from an in-memory set behind an
  `ArcSwap`, O(1) per query.
- **Restricted workloads get no external lookups.** The resolver answers
  only `*.temps.local` for them. External hostnames are resolved only by the
  egress proxy, which checks the allow-list and the public-IP rule. Without
  this, forwarding external names would let a sandbox leak data through DNS
  queries past the egress proxy. Phase 0 tests whether Docker's embedded DNS
  on the sandbox internal networks forwards external names today.
- **Every driver points workloads at the same resolver,** through the
  attacher:

  | Driver | Resolver setting |
  |---|---|
  | Docker | `dns=` the bridge gateway (the agent already does this for databases) |
  | Firecracker | the guest's `resolv.conf` pointing at the TAP gateway |
  | microsandbox | the SDK's DNS setting |
  | wasm | in-host lookups, with no packets sent |

  The same names resolve on every backend and every node.
  - Sandbox containers set no `dns` option today, so they get Docker's
    embedded resolver. The preview gateway keeps resolving sandbox container
    names through Docker until phase 8 moves sandbox previews onto endpoints.
- **Delivery:** each node's Hickory resolver (ADR-011) stays and moves onto
  the single state stream (§7).

### 11. Ingress: endpoints and access policies

```rust
/// Owned by the product object (environment, sandbox, database, function),
/// not by a workload revision (§3).
pub struct Endpoint {
    pub owner: ProductRef,
    pub target: EndpointTarget,       // Workload { workload_id, revision, port } | StaticBundle | Activator
    pub hosts: Vec<HostClaim>,        // generated, project domain, custom domain, or preview
    pub path_prefix: Option<String>,  // ADR-050 path routes
    pub protocol: EndpointProtocol,   // Http | TlsPassthrough | Tcp
    pub access: Vec<AccessPolicy>,    // evaluated in order; all must pass
}
pub enum AccessPolicy {
    Public, Password(PasswordRef), TempsLogin { permission: Permission },
    SignedLink, IpRules(IpRuleSetRef), RateLimit(RateLimitRef), Geo(GeoRef), Captcha,
}
```

- **Hostname claims are separate from routing,** as in the Kubernetes Gateway
  API. Who may claim a hostname is decided once, extending ADR-051's reserved
  platform hosts. Policies are typed, never annotations.
- **The project password wall and the sandbox preview gate become two
  policies on the same model.** The preview gateway's TCP splice stays as the
  transport for sandbox previews.
- **Switching targets is one write.**
  - Blue/green changes `target.revision`.
  - A scale-to-zero endpoint targets the `Activator` while no instance runs,
    and the activator holds the request until an instance is `Ready`.
- **One evaluator, compiled into the route snapshot.** Policies compile into
  the snapshot behind the existing `ArcSwap`, so the request path takes no
  locks and does no I/O. Both the control-plane proxy and worker ingress run
  the evaluator, which removes defect 6, including the global switch that
  sends workers no public routes.
  - Every policy must therefore be decidable from the snapshot alone.
    Workers have no database (`temps-agent/src/internal_proxy.rs:14`).
- **`TempsLogin` uses a signed grant, not the console session.**
  - The console session cannot be used. It needs database lookups on every
    request (`temps-auth/src/auth_service.rs:344`, `:354`), and its cookie is
    host-only with `SameSite=Strict`, so it never reaches an app hostname.
  - Instead, the endpoint redirects to the console. The console checks the
    user's permission on the resource and redirects back with a short-lived
    grant signed for that host and resource. The proxy verifies the
    signature against a key in the snapshot and sets a cookie for that host.
  - This generalises the sandbox preview's existing session grant
    (`verify_session_grant`, `temps-proxy/src/proxy.rs:5313`).
  - Revoking a user or a permission puts the affected grant ids in the
    snapshot until they expire, so revocation takes effect without a
    database call.
- **Password checks are rate-limited before they run.** Argon2 verification
  is deliberately expensive. On a 1 vCPU node, unthrottled guesses become a
  denial of service, so the per-IP and per-endpoint failure limits apply
  before the hash is computed. Password hashes reach workers only inside the
  route snapshot.
- **Defaults per product:**

  | Product | Default access |
  |---|---|
  | Projects | `Public`, as today |
  | Sandboxes | `TempsLogin` (or a password), never anonymous by default. Existing sandboxes keep their current setting |
  | Databases | no HTTP endpoint. An optional public endpoint, which admission refuses without TLS **and** at least one `IpRules` policy. Hostname routing on a shared port uses `TlsPassthrough`, which only works for PostgreSQL clients that negotiate TLS directly (`sslnegotiation=direct`, PostgreSQL 17 and later); other clients get a dedicated `Tcp` port per database |
  | Functions | a public HTTP trigger |
- **Public DNS for endpoints:** on a domain Temps manages, records are created
  through the DNS providers (ADR-031). Otherwise the console shows the record
  the user must create. A pinned workload's public name can point only at the
  pinned nodes' ingress.

### 12. Storage

- **`Volume` is a first-class resource:** id, owner, backing (a Docker named
  volume, an ext4 image, a preopened directory), the node it lives on, size,
  and retain-on-delete. `VolumeProvider` creates it for the driver's
  `VolumeKinds`.
- **Single-container deployments gain opt-in volumes,** which pin them (§8).
  Compose volumes keep their existing security checks.
- **Snapshot and restore are volume capabilities.** Sandbox snapshots
  (ADR-037) and the database recreate guard keep working on top of them.
- **Backups become jobs.** A backup is a `RunToCompletion` workload owned by
  the database, with affinity to the volume's node. That makes backups work
  for databases on worker nodes (defect 2) without a separate remote dispatch.

### 13. Health

- **Three probes, each optional, using ADR-052's probe kinds** (`http`,
  `tcp`, `process`, `exec`):

  | Probe | Question | What it controls |
  |---|---|---|
  | `startup` | still booting? | the grace period before the other probes count |
  | `readiness` | should it get traffic? | membership in routes and DNS |
  | `liveness` | should it be restarted? | restarts |

  A database in a long recovery, or a waking sandbox, is not ready but must
  not be killed.
- **Liveness is off unless configured.** A wrong liveness probe causes a
  restart loop, and under load it can restart every replica at once. So it
  has no default; an operator turns it on per workload.
- **Exits and hangs are handled separately.** The driver's restart policy
  handles a process that exits. A liveness probe handles one that is
  running but stuck. They never both act on the same failure.
- **Readiness is the only way into routes and DNS,** for every kind.
- Health state is recorded as `Ready` and `Running` conditions. The ADR-053
  runtime-health engine stays the node-local executor.

### 14. Products

| Product | Keeps | Uses from the plane |
|---|---|---|
| Projects | build pipeline, blue/green (new revision → wait for `Ready` → point the environment's endpoints and names at it → retire the old revision), rollback, history | everything |
| Sandboxes | suspend/wake policy, snapshots UX, relays, `PerExec` delivery, workspace linking | everything, plus `SandboxExt` |
| Databases | engines (init, provisioning, upgrade state machine, clustering), access engines (ADR-052 §2) | everything; backups as jobs |
| Cron (new) | schedule and run history | `RunToCompletion` workloads |
| Functions (new) | upload → artifact (a bundled image, or a WASM component) | `ScaleToZero` workloads and endpoints |

- **Cron adopts the Kubernetes CronJob semantics:**
  - a concurrency policy (`allow | forbid | replace`);
  - a deadline for starting a missed run, so a node that was down does not
    fire every missed run at once;
  - a retry limit;
  - a maximum run time;
  - a time-to-live for finished runs.

  Today's HTTP-GET cron stays as the `http` mode. The new `job` mode runs the
  project's image with a command.
- **One activator for scale-to-zero.** It wakes `ScaleToZero` workloads on
  the first request and absorbs both `on_demand.rs` and sandbox
  wake-on-access.
- **Compose is translated** into one workload per service, reusing the parser
  the Compose security checks already have, so Compose services get policy,
  placement and multi-node. The CLI path stays until the translation matches
  it on the existing Compose tests.
- **Static sites are not compute.** They are an endpoint backend
  (`StaticBundle`) in §11 with no driver.

### 15. Authorization and audit

- Products keep `permission_guard!` and audit logging in their handlers. The
  plane adds no user-facing write API.
- An admin-only, read-only "Workloads" view shows specs, conditions, events
  and effective policy across kinds.
- The plane audit-logs the changes it makes itself: system-origin rules,
  rescheduling, and refusals at admission.
- Changing `nodes.resource_policy`, operator network rules or hostname claims
  needs an administrator.

### 16. What this takes from Kubernetes, and what it does not

**Taken:**
- desired state with reconciliation that acts on current state
- spec, status and generation
- conditions and events
- the kubelet-style node agent
- the runtime / network / storage split
- a scheduler that filters, then ranks
- requests and limits, made optional here
- admission and pod-security-style profiles
- owner references and finalizers
- separate startup, readiness and liveness probes
- stable names in front of ready instances
- Gateway API hostname claims and typed policies
- CronJob semantics

**Not taken:**

| Kubernetes | Here |
|---|---|
| etcd, an API server, informer caches | Postgres, NOTIFY and generation cursors, plus one filtered stream per node |
| a generic user-facing resource API | users see products; the plane is internal |
| out-of-process plugin protocols | compiled Rust traits |
| Deployment → ReplicaSet → Pod | product object → workload (with a revision) → instances |
| user-defined pods and init containers | one main process, plus sidecars only the plane defines |
| allow-only NetworkPolicy | explicit allow and deny, generated from links |
| virtual service IPs | DNS plus the proxy |
| labels as the only way to link objects | foreign keys for correctness, labels for discovery |
| eventual consistency everywhere | `apply_and_wait` with typed failures |
| autoscaling, a service mesh, namespaces as tenancy | not now |

### 17. Testing policy on macOS

Network policy must be testable, ingress and egress, with real packets, on a
macOS development host. Developers and CI on Apple Silicon run Docker in a
Linux VM (colima, Docker Desktop, OrbStack) and microsandbox through libkrun.

- **The ruleset is applied where the bridges are.** The attacher compiles
  rules into one nftables ruleset and hands it to an `NftExecutor`:
  - `HostNft` runs `nft -f -` on the host. It is the production path on
    Linux, where the agent and `dockerd` share a kernel.
  - `DockerVmNft` runs the same ruleset inside the Docker daemon's own
    network namespace, through a helper container (`network_mode: host`,
    `cap_add: [NET_ADMIN]`, the ruleset piped to `nft -f -`). On macOS every
    Docker bridge lives in the VM, so this installs exactly what `HostNft`
    installs on Linux, in the same place relative to the bridges.
  - The executor is chosen by where the daemon runs, the same test
    `DockerRuntime` already makes for host-level facts: a local `unix://`
    socket on Linux uses `HostNft`, anything else uses `DockerVmNft`.
- **The same ruleset everywhere.** There is no macOS variant of a rule. A
  test that passes against colima exercises the ruleset Linux nodes get.
- **The end-to-end suite** creates real containers on a workload network
  and asserts with real connections:
  - egress: a restricted workload reaches a linked peer on the allowed port,
    and not on another port, not an unlinked peer, and not the internet;
  - ingress: a workload with an ingress rule accepts its allowed peer and
    drops everyone else;
  - default-allow (`open`) workloads keep today's reachability;
  - removing the rules restores today's behaviour (rollback).
- **microsandbox:** rules compile to the SDK's network policy. The
  translation is unit-tested; the end-to-end tests run when the microsandbox
  runtime is installed, and skip with a message otherwise, like the Docker
  tests do when no daemon answers.
- Tests skip gracefully when no Docker daemon answers, never `#[ignore]`.

### 18. Crate layout

The plane is split by concern, so each part can be tested on its own and a
product depends only on what it uses:

| Crate | Holds | Depends on |
|---|---|---|
| `temps-workload` | `WorkloadRef`, `WorkloadSpec`, `Resources`, `SecurityProfile`, conditions, events, `RuntimeDriver` and its errors | nothing in Temps (leaf) |
| `temps-workload-env` | layered variable assembly, secret classification, `Baked`/`PerExec` delivery (ADR-052 §3) | `temps-workload` |
| `temps-workload-netpol` | the rule model, the nftables compiler keyed by workload identity, `NftExecutor`s, the microsandbox policy translation | `temps-workload` |
| `temps-workload-ingress` | endpoints, hostname claims, access policies and the compiled evaluator (no I/O, no locks) | `temps-workload` |
| `temps-workload-volumes` | the `Volume` resource and `VolumeProvider` | `temps-workload` |
| `temps-workload-docker` | the Docker `RuntimeDriver` and the Docker attachment | `temps-workload`, bollard |
| `temps-workload-plane` | workload objects, admission, scheduler, reconciler, events, `apply` and `apply_and_wait` | the crates above, `temps-entities` |

- The concern crates never depend on each other. The plane composes them.
- Only `temps-workload-plane` touches the database.
- Products (projects, sandboxes, databases, cron) depend on
  `temps-workload-plane`, never on a driver crate.

## Scalability

| Path | Kind | Expected load | Memory bound | At saturation |
|---|---|---|---|---|
| Access-policy evaluation in the proxy | hot | every request, 100k+/s | compiled into the route snapshot; no per-request allocation beyond today | no locks or I/O; rate-limit state uses the existing atomic counters; password checks are rate-limited before argon2 runs |
| Internal DNS resolver | hot | every lookup from every workload | in-memory zone plus an identity map, one entry per instance on the node | O(1) answers; a stale snapshot keeps answering while the stream reconnects |
| Per-node egress proxy | hot | outbound connections from restricted workloads | a fixed connection limit and buffer pool per node | new connections are refused with a logged, counted reason, never queued without bound |
| Node state stream | control plane | changes only | the node's own instances and rules | long-poll backs off; workers keep running from their local snapshot |
| Reconciler and scheduler | control plane | on spec or status change | bounded per pass | passes are proportional to changes; no table scans on a timer |
| Status and event writes | control plane | on change only, batched per heartbeat | one batch per node per heartbeat | with an external database, writes are coalesced so a slow round trip delays status, never the instances |
| Events | control plane | on condition changes | 100 per workload, 7 days | the oldest are dropped; repeats increment a count |

## Phases

Each phase ships on its own, changes nothing for existing workloads by
default, and leaves the tree compiling.

A concern crate (§18) may land before the phase that wires it in, with its
own tests and end-to-end suite, as long as nothing in a product calls it
yet. The network-policy compiler and its macOS suite (§17) are expected to
land this way, ahead of phase 7.

**The first consumer is a new feature, not a migration.** Databases have the
worst drift, but they hold user data. The plane is proven first on cron's
`job` mode, which nothing depends on yet, before anything existing moves
onto it.

0. **Fixes and golden tests (no new model).**
   - DNS name ownership (§10):
     - deployment records and claims move to the environment owner;
     - `dns_name_claims` and `dns_name_conflicts`, backfilled with the
       deterministic winner of §10;
     - the registry refuses a cross-owner write instead of taking the record
       over;
     - conflicts shown by logs, `temps doctor` and the DNS status endpoint.
   - Node removal refuses while the node hosts managed databases or cluster
     members.
   - A test of external-name forwarding on the sandbox internal networks.
   - A golden capture of the exact container-create request each of the three
     stacks builds today.
1. **Types and the Docker driver.** Add `WorkloadSpec`, `Resources`,
   `SecurityProfile` and `RuntimeDriver` to `temps-workload`, beside ADR-052's
   phase 1 and 2 types. Extract the Docker driver from the creation code in
   `DockerRuntime` into `temps-workload-docker` (§18). The golden tests must reproduce today's requests byte for
   byte.
2. **Workload objects, admission, conditions and events. First consumer:
   cron `job` mode,** on the control plane. It runs the project's image with a
   command as a `RunToCompletion` workload, with the CronJob semantics of
   §14.
3. **Node runtime and the state stream,** with the control plane as node 0.
   - The stream implements §7 in full: the change log, tombstones,
     reassignment removals and the visibility watermark, each with its own
     test.
   - `job` runs can be placed on workers. Until phase 5 they are placed by
     `NodeScheduler` as it is: a run is placed like a one-replica deployment
     of its environment, with the environment's (or else the project's)
     `target_nodes` and `target_labels`, the same eligibility filter (status,
     draining, dedicated role, labels, architecture, socket grant) and the
     `LeastLoaded` strategy. The unified `PlacementSpec` replaces it in
     phase 5.
   - `/agent/*` endpoints remain the fallback for workers that do not
     advertise the stream.
4. **Databases onto the plane.** Both creation paths (control plane and
   worker) go through admission and the one driver, ending the drift of
   defect 1. Containers carry both label sets.
5. **Deployments onto the plane.**
   - `DeployImageJob` submits revisions; blue/green stays in the product.
   - Also lands here: optional `Resources`, `nodes.resource_policy`,
     `nodes.system_reserved`, the unified `PlacementSpec`, `explain` and the
     pinning recommendations.
6. **Sandboxes onto drivers.** The Docker, Firecracker and microsandbox
   backends become drivers plus `SandboxExt`, and `SandboxProvider` becomes
   the product facade. Remote sandboxes reach parity.
7. **Network policy for every kind** (ADR-052 phase 3).
   - The per-node egress proxy, including how it reaches sandboxes (§9).
   - The unified DNS publisher, group short names, policy-aware answers and
     no external lookups for restricted workloads.
8. **Ingress endpoints and access policies.** The evaluator runs in worker
   ingress, `TempsLogin` grants work there, sandbox previews move onto
   endpoints, and new sandboxes default to `TempsLogin`.
9. **Volumes for deployments; backups as jobs** on the volume's node.
10. **Functions and the remaining drivers:**
    - functions, with the activator (absorbing `on_demand.rs`);
    - the WASM driver;
    - Firecracker for deployments;
    - Compose translation.

## Migration and rollback

ADR-052's principles apply unchanged:
- add first, and remove only in a later release;
- the old path stays authoritative until the new one is proven, using
  `shadow` then `enforce` modes stored as columns on the governed row and
  audit-logged;
- existing isolation is never removed;
- existing deployments change nothing by default;
- no user data moves;
- mixed versions are normal, and old worker endpoints are removed no earlier
  than two releases after their replacement.

Every mode column below takes `legacy | shadow | enforce` and defaults to
`legacy`. Each is changed through the API by an administrator, and every
change is audit-logged.

| Phase | Schema | Behaviour change for existing workloads | Proof before it merges | Rollback |
|---|---|---|---|---|
| 0 | `dns_name_claims` and `dns_name_conflicts`, backfilled from `service_endpoints`; `deployment` records rewritten to `environment` | a new conflicting name is refused when first published; existing conflicts are reported, not removed, and the loser's next publish is refused while its records stay. Redeploys keep their name, because the owner no longer changes. Node removal refuses where it used to re-home a database | an upgrade test with a seeded collision (the winner is deterministic, the loser's rows survive, its next publish is refused, the conflict is listed); a redeploy test (two deployments of one environment publish without refusal); a removal test with a database on the node | down migration restores `deployment` owners from `environments.current_deployment_id` and drops both tables; the removal guard is a pure refusal |
| 1 | none | none | golden create requests for every stack, byte for byte | revert |
| 2 | `workloads`, `workload_instances`, `workload_events`, `workload_change_seq` | none; `job` mode is new and opt-in per cron | a crash test: kill the server mid-run and show the next reconcile pass finishes or cleans up the run | disable `job` mode; `http` crons are untouched |
| 3 | none | none; the stream runs beside the old endpoints | a mixed-version test (new control plane, old worker) creating a database, a deployment and a sandbox; the snapshot file contains no seeded secret | stop advertising the capability |
| 4 | `external_services.workload_mode` | none in `legacy` or `shadow`. In `enforce`, a worker database gains the control-plane host config (pids cap, `no-new-privileges`, healthcheck, labels) **only when it is next recreated**, and the change is listed before the operator confirms the recreate | shadow mode computes the spec for every existing database and logs differences from what is running | set the database's mode back to `legacy`; the engines call the old code path |
| 5 | `environments.workload_mode`, `nodes.resource_policy` (default `best_effort`), `nodes.system_reserved` (default per role) | none; current limits are carried over as limits, and pins as `PlacementSpec` | a shadow scheduler run comparing its placements with `NodeScheduler`'s for every environment | set the environment's mode back to `legacy` |
| 6 | `nodes.sandbox_driver_mode` | none; previews, snapshots and PTY become available on remote sandboxes in `enforce` | the ADR-010 eval harness passes for every driver | set the node's mode back to `legacy`; sandbox calls return to the old providers |
| 7 | per ADR-052 phase 3, plus `nodes.network_policy_mode` | none for deployments (`open`); sandboxes keep their internal network and add the shared proxy only in `enforce` | a two-node test that a restricted workload cannot reach an unlinked service, cannot spoof another workload's source address, and cannot resolve external names; the §17 end-to-end suite passing on Linux (`HostNft`) and on macOS (`DockerVmNft`); the egress-proxy reach measured at 50 sandboxes on the small target | set the node's mode back to `legacy`; sidecars remain available until the contract release |
| 8 | endpoint and policy tables, `environments.ingress_mode` | none; existing routes compile to the same decisions | a differential test: every request in the proxy test corpus gets the same decision from the old and new evaluators | set the environment's mode back to `legacy` |
| 9 | volume tables | none until a volume is added | a backup and restore of a database on a worker node | stop scheduling backup jobs; the local path is unchanged |
| 10 | per product | none; new products only | per product | per product |

## Consequences

**Positive**
- A product is a controller and a UX, not a deployment stack. Functions and
  cron jobs become small.
- Multi-node, policy, ingress gates, DNS and storage work the same way for
  every kind and every driver.
- Every workload states why it is in its current state, through the same
  conditions and events.
- How the eleven defects in Context are resolved:

  | Defects | Resolved by |
  |---|---|
  | 9, 10 | direct fixes in phase 0 |
  | 1–6, 8 | the shared admission, driver, node runtime, evaluator and egress proxy |
  | 7 | a safer default for new sandboxes only; existing ones keep their setting |
  | 11 (the cost of every new feature) | the plane itself |

**Negative**
- This is the largest refactor in the codebase. It touches `temps-deployer`,
  `temps-deployments`, `temps-agents`, `temps-sandbox`, `temps-providers`,
  `temps-agent`, `temps-routes`, `temps-dns` and `temps-proxy`.
- Phases 0, 1, 3 and 4 ship no visible feature beyond fixes. Phase 2 ships
  only cron's `job` mode, and the rest of the value arrives from phase 5 on.
- One shared plane is one shared failure domain. A reconciler bug affects
  every kind, not one. This is mitigated by shadow modes, golden tests and the
  rule that workers keep running from their local snapshot.
- Network and DNS enforcement needs a Linux kernel. On macOS the kernel that
  matters is the one in Docker's VM, so enforcement and its tests run there
  (§17); enforcement on the macOS host itself is not a goal.

**Risks**
- **Per-node egress proxy identity.** Spoofing must be impossible before
  sidecars are retired (phase 7 proof).
- **Policy-aware DNS** puts an identity lookup on every query. It must stay
  in memory and O(1).
- **Compose translation** may not cover every Compose feature. The CLI path
  stays until the translation is proven equal.
- **Shared admission** could weaken `Untrusted` if defaults are edited
  carelessly. The profile rules are pinned by tests, and a sandbox request for
  `Administrator` credentials is rejected at the boundary (ADR-052).

## Not decided here

- Kubernetes or a cloud provider as a target (§6).
- Out-of-process drivers through the external plugin socket.
- Whether sandboxes default to a microVM whenever a node offers one.
- An L4 proxy mode for TCP endpoints, so `direct` DNS answers are not
  needed for apps whose instances move.
- Hostname egress allow-lists for deployments (still open from ADR-052).
- Merging `project_services` and `sandbox_services` (still open from
  ADR-052).
- Autoscaling.
