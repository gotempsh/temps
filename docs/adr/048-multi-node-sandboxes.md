# ADR-048: Multi-Node Sandboxes

**Status:** Accepted (implemented in #1173)
**Date:** 2026-09-28
**Author:** David Viejo

## Context

Temps sandboxes (`crates/temps-sandbox`) today run exclusively on the
control-plane host. `StandaloneSandboxRegistry`
(`crates/temps-sandbox/src/services/registry.rs`) holds a single
`Arc<dyn SandboxProvider>` plus an in-memory handle map. The registry
references no node concept, and the `sandboxes` entity
(`crates/temps-entities/src/sandboxes.rs`) has no `node_id` column.

Worker nodes already run `temps agent` (`crates/temps-agent`), exposing a
bearer-authenticated HTTP API. Deployments are already placed on workers
using `RemoteNodeDeployer` (`crates/temps-deployer/src/remote.rs`), which
speaks that API over mTLS or bearer auth, selecting a node via the `nodes`
table (`crates/temps-entities/src/nodes.rs`: `id`, `address`,
`private_address`, `status`, `labels`, `capacity`, `role`,
`token_encrypted`, `architecture`). The control plane decrypts
`token_encrypted`, builds a `RemoteNodeDeployer` pointed at `address`, and
calls container lifecycle endpoints.

The worker agent's current container surface covers deployments:
`/agent/containers/deploy`, `/{id}/stop|start|exec|terminal|logs|
logs/stream|stats|info`, `DELETE /{id}`, `/agent/images/import`,
`/agent/images/{name}/exists`, `/agent/images/pull`, `/agent/services/*`.
This is insufficient for full `SandboxProvider` parity (enumerated below).

**Temps Fleet** (`temps-fleet/src/temps_sandbox_runtime.rs`) connects to
`wss://<host>/api/v1/sandboxes/{sbx_id}/agent-runtime` using
`TempsSandboxConnector`, which opens a WebSocket against the control-plane
and speaks the `temps-agent-runtime` SDK protocol. This endpoint must
remain at the same path on the control plane regardless of where the
sandbox actually runs; the control plane proxies to the worker rather than
redirecting the client.

**Strategic motivation.** The AI Autofixer's built-in runner will be
replaced by an "autofix requested" task consumed by Fleet (or any agent)
running in a Temps sandbox on a user's own node — a VPS joined as a worker
via `temps join`. Running sandbox workloads on workers a user controls is
the key primitive enabling that model. The autofix task contract and Fleet
worker registration are out of scope here; this ADR focuses solely on the
multi-node sandbox placement layer.

Four forces drive this ADR:

1. **Scale.** The control-plane host (typical reference: Hetzner cpx22,
   3 vCPU / 4 GB) is a single point of capacity exhaustion. Sandbox
   workloads — long-running AI agent runs, workspace containers — compete
   with the control plane itself for RAM and CPU.

2. **Trust boundary.** Letting users direct sandboxes to their own
   hardware without routing untrusted code through the control-plane host
   is a hard requirement for the bring-your-own-node model.

3. **Operational symmetry.** Deployments already cross nodes. Sandboxes
   should have the same placement model — one decision surface, one auth
   pattern, same `nodes` table — rather than a separate routing concept.

4. **Single-node compat.** An operator running only the control plane
   (the common case today) must see no behavior change.

## Decision

A sandbox can run on any node — the control plane or a worker joined with
`temps join` — and is pinned to that node for its whole lifetime. The
control plane keeps owning the sandbox API, the database row, and every
authorization decision; the worker only executes.

1. `sandboxes.node_id` (nullable, `NULL` = control plane) records where a
   sandbox lives.
2. The worker agent exposes a sandbox host API (`/agent/sandboxes/*`) whose
   handlers replay `SandboxProvider` calls against the worker's own
   `DockerSandboxProvider` — the same implementation the control plane uses
   locally.
3. The control plane drives it through `RemoteSandboxProvider`, an HTTP
   client that implements `SandboxProvider`.
4. `NodeRoutingSandboxProvider` wraps the host provider and dispatches every
   call to the node that owns the sandbox. It is the single
   `Arc<dyn SandboxProvider>` every consumer already holds (ADR-010), so no
   call site changes.
5. An operator allow-list (`allowed_node_ids`, default: every node) and a
   placement policy decide where new sandboxes go. An explicit node request
   never silently falls back to another node.

### 1. Provider abstraction stays in `crates/temps-agents`

`SandboxProvider` stays in `crates/temps-agents/src/sandbox/mod.rs`.
Moving it out would touch every consumer for no user-visible benefit today
(see Option C). The two new pieces live beside the existing providers:
`sandbox/remote.rs` (client + wire types) and `sandbox/node_routing.rs`
(router + database-backed node resolver).

### 2. The worker runs the real Docker provider

Workers run `temps agent` from the same `temps` binary as the control
plane, which already links `temps-agents`. So the worker constructs a
`DockerSandboxProvider` (from its Docker connection and its control-plane
URL) and each `/agent/sandboxes/*` handler deserializes the arguments,
calls that provider, and serializes the result
(`crates/temps-agent/src/sandbox_handlers.rs`). Sandboxes on a worker get
the exact container, network isolation, egress proxy and ownership logic
as local ones; there is no second implementation of sandbox semantics to
keep in sync.

Trust boundary on the worker, in addition to the existing agent bearer +
mTLS middleware:

- **No host paths cross the wire.** `RemoteCreateRequest` carries a
  `label` (validated as one `[A-Za-z0-9_-]{1,64}` path segment), never a
  path. The worker derives the work directory as
  `<agent data dir>/sandboxes/<label>` (`AgentConfig::sandbox_work_root`).
- **Sandbox containers only.** Every handle-based endpoint refuses a handle
  whose container name is not `temps-sandbox-<valid label>`, and then
  ignores the handle's container id: it asks Docker for the container with
  exactly that name, requires it to carry the `sh.temps.sandbox=true` label
  the provider sets on every sandbox, and acts on the id Docker returns. A
  handle with a valid sandbox name and an application container's id
  therefore cannot reach that container, and neither can a name that happens
  to share the prefix (the egress proxy sidecar, an app whose project slug
  starts with `temps-sandbox`); labels starting with `egress-proxy` are
  refused outright. `recover` takes a bare label; the provider adds the
  prefix.
- **Archives hold plain files only.** `write-directory` unpacks into a fresh
  temporary directory under the work root, entry by entry with `unpack_in`,
  and accepts only regular files and directories with relative paths:
  symlinks, hard links, devices, fifos and sparse entries are refused with a
  `400` naming the entry, so nothing in staging can point outside it when
  the provider reads it back. Archives are capped at 100k entries and at the
  upload size once unpacked; permissions, ownership, mtimes and xattrs are
  never applied. Directory walks (the worker's `write_directory` and the
  control plane's `tar_directory`) follow a symlink only while it resolves
  inside the uploaded tree.
- **Bounded uploads.** At most two uploads run at once on a worker; a third
  waits up to 30 s for a slot, then gets a `503`, so parallel 512 MiB uploads
  cannot exhaust its memory.
- **Create never clobbers.** A create is refused (`409`) when the label's
  container is running, belongs to a non-sandbox container, is already being
  created, or when a work directory is left over without a container. A
  stopped sandbox is replaced keeping its volumes and work directory (how a
  sandbox quarantined for a stale isolation policy is recreated). On failure
  only a work directory the request created is removed.
- **No orphans from abandoned creates.** The create runs in its own task.
  If the control plane stops waiting (its create timeout, a dropped
  connection — which cancels the HTTP handler), the worker destroys the new
  container and its work directory as soon as the create finishes.
- **No host paths in errors.** Error messages travel to the control plane
  and on to sandbox owners, so the worker replaces its sandbox work root (and
  every work and staging directory under it) with a placeholder in every
  error body it returns; the full message stays in the worker's log.
- **Same isolation policy as the control plane.** The worker quarantines
  sandboxes created under an older isolation policy at startup, like the
  control plane does, so a policy bump reaches worker sandboxes too.

### 3. Routing by handle, not by registry

`SandboxHandle` gains `node_id: Option<i32>` (serde-defaulted, so
pre-ADR-048 serialized handles read as local) and `SandboxCreateConfig`
gains `node_id`. `NodeRoutingSandboxProvider`:

- dispatches `create` / `create_from_snapshot` by `config.node_id` and
  stamps the resulting handle;
- dispatches every handle-based method by `handle.node_id`;
- sends host-level methods (image status, rootfs, `recover(run_id)`) to the
  local provider — agent runs and the image cache are local;
- overrides every trait method, including defaulted ones, for the same
  reason as `RoutingSandboxProvider`.

A new trait method, `recover_by_name_on(node_id, name)`, lets recovery ask
the owning node; its default ignores the node so single-host providers are
unchanged.

`StandaloneSandboxRegistry` still holds one provider. On a handle-cache
miss it reads the sandbox row's `node_id` (through a required
`SandboxNodeLookup`, so a misconfigured registry can never silently treat
worker sandboxes as local) and recovers from that node. Startup recovers
control-plane sandboxes only; worker sandboxes are recovered lazily.

`DbRemoteNodeResolver` turns a node id into a provider: it loads the node
row, refuses nodes that are not `active`/`draining`/`drained`, decrypts
`token_encrypted`, and builds the client with
`temps_deployments::cluster_ca::build_node_http_client` — the same mTLS
setup remote deployments use. Clients are cached per node and rebuilt when
the node's address, token, the sandbox defaults or the cluster CA change.
The resolver also passes the control plane's configured sandbox defaults
(image, CPU, memory **and network mode**) so a worker sandbox gets the
operator's settings, not the worker's built-in defaults — the worker's
default network mode is `full`, so a missing network mode would silently
undo an operator's `none`. If the settings cannot be loaded the resolver
fails closed instead of falling back to the worker's defaults. Node clients
use a 10 s connect timeout and never follow redirects (a redirect would
replay the bearer token). Sandbox calls carry the node token, environment
variables and file contents, so the resolver refuses nodes whose agent
address is plain `http://`: sandboxes need an `https` (mTLS) node address.
The scheme check is case-insensitive and shared with the mTLS client
builders (`cluster_ca::is_https_address`), so a node the resolver accepts
always gets the cluster CA and client identity.
The agents plugin registers one resolver, shared by the router and the
placement probe, and cached clients of removed or unroutable nodes are
dropped.

The client treats a worker as less trusted than the control plane. Every
response body is read with a byte cap for its operation (exec: two capped
streams plus JSON escaping; read-file: the base64 of the largest readable
file; small calls: 1 MiB; error bodies: 16 KiB), so a misbehaving worker
cannot make the control plane buffer without bound. Transport errors never
include the node's URL (it names an internal address and can reach
non-admin sandbox owners); the full error is logged for the operator.
Worker-written messages are sanitised before they reach an API client or a
terminal: control characters (escape sequences included) are dropped and
the text is capped at 512 characters.

### 4. Allowed nodes

`AgentSandboxSettings.allowed_node_ids: Option<Vec<i32>>`. `None` (the
default) = every node, including the control plane and nodes added later.
`Some(ids)` = only those ids; the control plane is id `0` (the convention
`CONTROL_PLANE_NODE_ID` already uses for services). `Some([])` stops new
sandboxes everywhere — a deliberate operator choice, never a default.

The field is owned by a dedicated endpoint, `PUT /v1/sandboxes/placement`
(admin only, audited as `SANDBOX_PLACEMENT_UPDATED`), which writes it with
`ConfigService::set_sandbox_allowed_node_ids` under the settings-row lock.
Every generic settings save restores the value from the locked row, the
same way the join-token hash and cluster CA are protected: the settings page
round-trips a masked document that never contains the list, and a save built
from an older snapshot must not revert it. Ids of removed nodes are dropped
when the list is read, so a deleted node can never make later saves fail.

Disallowing a node only affects new placements. Existing sandboxes keep
running and every lifecycle operation keeps working; draining a node is the
mechanism for moving work off it.

### 5. Placement

`crates/temps-sandbox/src/services/placement.rs`, a pure `choose` function
plus thin database loaders:

- **Explicit node** (`node: "worker-1"`, `"7"`, `"control-plane"`/`"0"`/
  `"local"`): must exist (`422 sandbox-node-not-found`), be allowed
  (`422 sandbox-node-not-allowed`) and be placeable
  (`422 sandbox-node-offline`). No fallback.
- **Placeable** matches the deploy scheduler: status `active`, a heartbeat
  within 90 s, and not a build-only node (`temps.sh/role: builder`). Rows
  with role `control-plane` are never workers.
- **No node, control plane allowed**: the control plane. This is every
  single-node install, and this path runs no node queries at all.
- **No node, control plane excluded**: the allowed `active` worker with the
  fewest live (non-destroyed) sandboxes, ties broken by lowest id. Live
  sandbox count is used instead of heartbeat capacity because it is what
  sandbox placement actually loads, and it is exact. No eligible node →
  `422 sandbox-no-placement-node`.
- **Not eligible** also: a node whose agent address is `http://` (§3), and a
  node whose sandboxes are being evicted (§7). Placement shows the reason.
- **Probe before use.** A worker chosen for a new sandbox is asked over its
  agent API whether it can run one (`/agent/sandboxes/status`, 5 s). An
  explicit node that fails the probe is a `422` naming the node and the
  reason (Docker unavailable, or an agent too old to host sandboxes —
  upgrade temps on the node); default placement tries the next candidate
  (at most three) and otherwise fails listing why each was skipped. The
  control-plane path still runs no node queries or probes.

Managed AI application workspaces (internal `host_work_dir_override`) always
run on the control plane.

### 6. Worker-agent endpoints (phase 1)

All `POST`, JSON bodies, same auth middleware as every agent route. The two
upload routes (`write-file`, `write-directory`) accept up to 512 MiB (uploads
are base64 in JSON); every other route keeps axum's default 2 MiB cap:

| Path | `SandboxProvider` method |
|------|--------------------------|
| `/agent/sandboxes` | `create` |
| `/agent/sandboxes/exec` | `exec` / `exec_as_root` / `exec_as_user` without a line callback |
| `/agent/sandboxes/exec-stream` | `exec_streamed`, and the other exec variants with a line callback (NDJSON stream) |
| `/agent/sandboxes/alive` | `is_alive` |
| `/agent/sandboxes/read-file` | `read_file` |
| `/agent/sandboxes/write-file` | `write_file` |
| `/agent/sandboxes/write-directory` | `write_directory` |
| `/agent/sandboxes/kill-processes` | `kill_processes` |
| `/agent/sandboxes/destroy` | `destroy` (+ removes the worker work dir) |
| `/agent/sandboxes/stop` | `stop` |
| `/agent/sandboxes/start` | `start` |
| `/agent/sandboxes/recover` | `recover_by_name` |
| `/agent/sandboxes/status` | `is_available` + `image_status` |

A worker without these routes answers a bare 404; the client reports "the
agent on this node does not support sandboxes; upgrade temps on the node"
instead of a generic failure. The worker keeps the meaning of provider
errors over HTTP (404 container missing, 400 invalid request, 503 Docker
unavailable, 500 the operation failed, 409 a create that would replace a
live sandbox, 422 a feature not available on workers), and the client maps
them back: 404 with a body becomes `SandboxNotFound` naming the container
and node, 400 a validation error, 401/403 "the node rejected the control
plane's credentials; re-join it". Every worker route is documented in the
agent's OpenAPI document.

`read-file` on a worker is limited to 100 MiB; bigger files are refused
with a `400` naming the sandbox, path and limit before they are buffered.

An exec with a line callback (`exec_streamed`, which detached jobs use)
goes through `exec-stream`: the worker answers `200` with newline-delimited
JSON frames (`stdout`/`stderr` lines as the command produces them, a
`heartbeat` after 15 s of silence, and one final `exit` or `error` frame
carrying the status the same failure gets on the other routes). Callbacks
therefore see output live, as for a local sandbox, and the call has no
total timeout — a dev server can run for days — only a 60 s idle timeout
that heartbeats keep from firing on a quiet command. A frame carries at most
64 KiB of output: a longer line is sent as continuation frames (`more: true`)
and rebuilt on the control plane, which caps one line at the 16 MiB stream
limit and refuses any frame over its cap before parsing it. The worker
applies backpressure (a bounded frame channel) rather than buffering when the
control plane reads slowly. Each streamed command runs with a
`TEMPS_SANDBOX_EXEC_ID` environment variable that its processes inherit; if
the connection drops before the command exits (a killed job, a lost link,
a control-plane restart), the worker stops every process carrying that id —
SIGTERM, then SIGKILL after 5 s — running the cleanup as the command's own
user, since sandboxes drop the capability root would need to read another
user's process environment. Errors in the stream are redacted and sanitised
like error bodies. An exec without a callback still uses `exec`, which
returns when the command finishes. Both return at most the last 16 MiB of
each stream, marking what was dropped, and the worker's Docker provider
enforces that bound while it reads Docker's output. A background job keeps
the same bounded tail of each stream on any node, so one noisy command
cannot exhaust either side's memory. Features not yet available
on workers fail with an explicit message naming the node: interactive
terminal, retained agent runtime, snapshots (take and restore), disk
resize, workspace volumes, the Firecracker backend, application service
networking, the git, model and harness MCP relays, and rebuilding the
sandbox image. They return `422 sandbox-unsupported-on-worker-node`, naming
the feature and the node. Rebuilding the sandbox image runs on the control
plane only; worker nodes keep the image they already have until it is
rebuilt there (the rebuild says so). Snapshotting a
worker sandbox is refused (`422 sandbox-snapshot-on-worker-node`) before the
snapshot flow scrubs credentials or stops the sandbox. Worker sandboxes get
no preview URL template or routes (the console explains why) until preview
routing reaches workers.

### 7. Offline and deleted nodes

- Operations on a sandbox whose node is offline, pending, deleted or
  unreachable return `503` with the node named
  (`AgentError::SandboxNodeUnavailable` → `SandboxError::Unavailable`). So
  does a reachable worker answering 502/503 (for example its Docker daemon
  is down) or 504. "Sandbox not found" is only returned when the node
  reports the container missing.
- Destroying a single worker sandbox whose node is unreachable, or does not
  answer within 30 s, keeps the sandbox and returns `503`
  (`sandbox-node-unreachable`) naming the node. Marking it destroyed would
  leave its container running with nothing tracking it. The message says to
  retry once the node is back, or to evict the node if it is gone for good.
  Control-plane sandboxes keep the old behaviour: the row is marked
  destroyed even if Docker fails.
- Control-plane startup recovers control-plane sandboxes only; worker
  sandboxes are recovered lazily on first use, so an unreachable worker
  cannot add a timeout per sandbox to startup. The expiry sweeper leaves a
  worker sandbox on an unreachable node `running` and retries on the next
  sweep rather than marking it stopped while it still runs. A worker gets
  one 30 s chance per sweep: once it fails to answer, its other expired
  sandboxes wait for the next sweep, so a hung worker cannot stall expiry
  for the rest of the cluster.
  Control-plane sandboxes keep the old behaviour.
- **Status writes never resurrect a row.** Lifecycle calls read a row, wait
  on the provider (a worker can take tens of seconds), then write the row.
  Every such write (expiry sweep, pause, resume, wake, restart, resize,
  timeout extension, application rebuild/restore/runtime update) only
  applies while the row is still in the status the caller expects, so a
  destroy or eviction that lands in between can no longer be overwritten
  with `stopped` or `running`; the caller then reports the sandbox as gone
  and removes any compute it just created.
- If a worker create fails, its cleanup destroy is bounded at 30 s and, if
  it times out, logs the node, sandbox and cleanup command for the operator.
- `sandboxes.node_id` is `REFERENCES nodes(id) ON DELETE SET NULL`, so
  destroyed sandbox rows never block removing a node. Removing a node that
  still hosts live sandboxes is refused (`409`, `NodeError::HasLiveSandboxes`)
  before the row is touched, so a live sandbox is never silently re-homed to
  the control plane. The check and the delete run in one transaction that
  first locks the node row `FOR UPDATE`, which conflicts with the key-share
  lock a concurrent sandbox insert takes through the foreign key; an insert
  that loses that race fails as `sandbox-node-not-found`. The drain status
  reports `remaining_sandboxes` and only allows removal at zero, since
  draining does not move sandboxes.
- **Eviction.** `POST /v1/sandboxes/placement/nodes/{node}/evict` (admin,
  audited as `SANDBOX_NODE_EVICTED`) destroys every live sandbox on a worker,
  from all owners, up to 8 at a time. Container destroys are best-effort — a
  node that is gone for good cannot answer — and the rows are marked
  destroyed regardless, so the operator can always clear and remove a dead
  node. Each container destroy gets 30 s. The node is written off, and the
  remaining destroys skip the container call, when a call times out or
  reports it unavailable and either the node has not answered any call in
  this eviction, or it has now failed 8 calls in a row (it froze part-way).
  So a node that accepts connections but never answers, or stops answering
  mid-eviction, costs about one more 30 s round, not a lifecycle timeout per
  sandbox; one slow call from a node that is answering does not write it
  off. Sandboxes whose container the node did not confirm removing are
  still destroyed, but reported separately (`containers_unconfirmed` in the
  response and audit record): those containers may still be running on the
  node, and nothing in Temps lists them any more. Each entry carries a
  `cleanup_command` to run on the node if it comes back
  (`docker ps -aq --filter name=temps-sandbox-<id> | xargs -r docker rm -f`,
  which also removes the egress proxy and does nothing if they are already
  gone); the console shows them with copy buttons
  and the CLI prints them. Reasons that come from a node are stripped of
  control characters and capped at 512 characters. Every sandbox is attempted
  even if some fail; the eviction runs detached from the request, so it
  finishes and is audited even if the client disconnects. Rows that could
  not be destroyed return `503` (`sandbox-node-eviction-incomplete`) naming
  each one, with the `destroyed`, `containers_unconfirmed` and `failed`
  lists as problem members so the console and CLI can show the cleanup
  commands; rerunning the eviction retries only what is left. The audit
  record also lists the owners of the destroyed sandboxes. While an
  eviction runs the node is cordoned — placement reports it as not eligible
  and an explicit create on it is refused — and a second eviction of the
  same node is a `409` (`sandbox-node-eviction-in-progress`). The eviction
  is still audited if refreshing the node afterwards fails. The control
  plane cannot be evicted (`400`).
  It is a sensitive action, checked before anything is destroyed, with the
  same policy as draining a node: a browser session of a user with MFA
  enrolled needs a recent step-up (`428`); sessions without MFA, API keys
  and CLI tokens are allowed, and the policy logs the reduced assurance.
- **Evicting during a create.** A worker sandbox whose row was destroyed
  while its container was still being created is removed as soon as the
  create returns, and the create fails with an explanation, so no container
  outlives its row. An eviction that lands after that re-check still removes
  the container (its handle is registered by then), but the create has
  already reported the sandbox as running; the next request for it returns
  not found.

### 8. API

- `POST /v1/sandboxes` accepts optional `node`.
- Sandbox responses gain `node_id` (`null` = control plane) and `node_name`
  (`"control-plane"` for control-plane sandboxes).
- `GET /v1/sandboxes/placement` (sandbox readers): `allowed_node_ids` and
  every node with `allowed`, `eligible`, `status`, `reason`,
  `live_sandboxes`.
- `PUT /v1/sandboxes/placement` (admins): `{ "allowed_node_ids": [0, 3] | null }`.
  The member is required (`null` = every node), so a malformed body cannot
  silently allow every node; unknown or duplicate ids are a `400`.
- `GET /v1/sandboxes/placement/nodes/{node}?page=&page_size=` (admins): the
  node's placement row plus one page (default 20, max 100, newest first) of
  the live sandboxes on it from all owners, each with
  `owner_user_id`/`owner_email`, and the real `total`. Owner-only details
  (preview password hint, source repository URL) are omitted for sandboxes
  the caller does not own. `{node}` takes the same forms as `create --node`;
  an unknown node is a `404`.
- `POST /v1/sandboxes/placement/nodes/{node}/evict` (admins): destroy every
  live sandbox on a worker (see §7).

### 9. CLI (`apps/temps-cli`)

```
bunx @temps-sdk/cli sandbox create --node <name|id|control-plane>
bunx @temps-sdk/cli sandbox nodes [list]            # placement state
bunx @temps-sdk/cli sandbox nodes show <node>       # admin: sandboxes on a node (--page)
bunx @temps-sdk/cli sandbox nodes set <nodes…>      # admin: allow exactly these
bunx @temps-sdk/cli sandbox nodes allow <nodes…>    # admin: add to the list
bunx @temps-sdk/cli sandbox nodes deny <nodes…>     # admin: remove from the list
bunx @temps-sdk/cli sandbox nodes allow-all         # admin: every node (default)
bunx @temps-sdk/cli sandbox nodes deny-all          # admin: no new sandboxes anywhere
bunx @temps-sdk/cli sandbox nodes evict <node>      # admin: destroy all sandboxes on a node
```

Node references follow the server's rules: `0`/`control-plane`/`local` is the
control plane, a bare number is an id, anything else a name. Changes that
stop the control plane from taking sandboxes print a warning, because default
placement then moves to workers. `--json` is accepted on every subcommand.
`sandbox list` shows a NODE column and `sandbox show` a Node field.

### 10. Web UI

- AI Workflows → Sandbox (`/agent-sandbox/sandbox`): a "Sandbox nodes" card with one checkbox per node
  (status and live sandbox count shown) and "Allow every node". With only
  the control plane it says how to add a worker (`temps join`) and links to
  Nodes — it is never hidden.
- Sandbox detail: a Node fact linking to the node page (plain text for users
  who cannot open node pages). Worker sandboxes show why they have no preview
  URL instead of hiding the card.
- Sandbox list: a node badge, linking to the node page, on worker-hosted
  sandboxes (omitted on the control plane, where it would be noise).
- The "Sandbox nodes" card is read-only for non-admins, with a note saying
  why, instead of a Save button that fails.
- Node page (`/settings/nodes/{id}`): flat "Containers | Sandboxes" tabs with
  counts (`?tab=sandboxes` deep-links). The Sandboxes tab lists every live
  sandbox on the node with its owner (paged), says whether the node accepts
  new sandboxes, links to the placement settings, and offers "Destroy all"
  (eviction, with a confirmation). A sandbox links to its page only for its
  owner, since that page is owner-scoped. When sandboxes are what blocks
  removing the node, the page says so and links to the tab; a failed removal
  shows the server's reason.
- The create-sandbox docs panel shows `sandbox nodes` and
  `sandbox create --node`.

### 11. Security

Security-auditor sign-off is required before merge. Items for review:

- The new worker sandbox host API (§2, §6) and its path/label/handle
  validation.
- Credential transit: create-time env vars travel control plane → worker
  over the same authenticated channel deployments use, and land in the
  worker's Docker environment. A compromised worker sees the secrets of the
  sandboxes it hosts, not others — the existing deployment threat model.
- The control plane never trusts the worker for authorization: sandbox
  ownership is checked against the database row before any call reaches a
  worker.
- Egress: the worker runs the same Docker provider, so worker sandboxes get
  the provider's own isolated network and egress proxy. Its relays point at
  the worker's configured control-plane URL; verify end to end that model
  and git relays work from worker sandboxes before advertising them.

### 12. Migration

`m20260928_000001_add_node_id_to_sandboxes`: nullable `node_id` with the FK
above and a partial index on non-null values. Existing rows stay `NULL`
(control plane). No settings migration — `allowed_node_ids` is
serde-defaulted.

### 13. Testing

Unit: placement decisions (default, least-loaded, no eligible node, explicit
by name/id, every rejection), allow-list validation, router dispatch
(create/exec/destroy follow the node; an unreachable node is an error with
nothing run locally; recovery asks the owning node and stamps it), wire
types (label/handle validation, legacy handle deserialization, tar
round-trip, refusal of links, devices and escaping paths), the worker router
(every sandbox route behind auth; only the two upload routes accept bodies
over 2 MiB), client response caps, URL stripping and message sanitising,
the resolver (status gating, https-only, fail-closed settings, cache
rebuild and pruning), conditional status writes, eviction cordon and
serialisation, the placement probe, CLI node resolution and output, and the
console's placement card and node Sandboxes tab.

End to end, automated in the multinode scenario that CI runs on every pull
request (`apps/temps-e2e`, `multinode-join-scenario`): placement API, create
on a named worker (container exists on that worker, not the control plane),
exec, file write/read, pause/resume, control-plane restart, snapshot refused
on a worker, disallowed node, default placement with the control plane
excluded, offline node (`503` naming it), node removal refused while a
sandbox lives on it, destroy (container and work dir gone) and eviction.

### 14. Phasing

**Phase 1 (this change):** everything above.

**Phase 2:**
- Interactive terminal and retained agent runtime on workers — the control
  plane proxies `/v1/sandboxes/{id}/terminal` and `/agent-runtime`
  WebSockets to the worker, so Fleet's `TempsSandboxConnector` keeps using
  the control-plane URL unchanged.
- Preview URLs for worker sandboxes (the preview gateway resolves
  `temps-sandbox-<id>` through Docker DNS on the control plane).
- Snapshots, disk resize and workspace volumes on workers.

**Phase 3 (own ADRs):** Firecracker on workers; the autofix task contract
consumed by Fleet in worker sandboxes.

## Consequences

### Positive

- Sandboxes can run on user-controlled hardware (a VPS joined as a worker),
  which is the primitive the Fleet-based autofix flow needs.
- One sandbox implementation: workers run the same provider as the control
  plane, so isolation and behavior are identical wherever a sandbox lives.
- No call-site churn: routing happens inside the provider every consumer
  already holds.
- Single-node installs see no behavior change and no extra node queries.
- Every placement failure is a typed error naming the node and the fix.

### Negative

- Phase 1 lacks terminal, agent runtime, preview URLs and snapshots on
  workers; each fails loudly rather than silently.
- Worker and control plane must run a version with the sandbox host API; an
  older worker yields an explicit "upgrade temps on the node" error.

### Risks

- **Orphans after partial failure.** The row is written before the remote
  create. If the control plane gives up on a create (timeout, dropped
  connection), the worker destroys the container itself (§2), and
  cleanup-on-failure runs through the router with a bounded deadline. A
  control-plane crash in the middle of a create drops the connection, so
  the worker removes the container; it can still leave a row without a
  container (it reports not found, and destroy or eviction clears it). A
  worker that itself crashes mid-create can leave a container without a
  row; a worker-side reconciliation sweep is the follow-up for that case.
- **Containers left on a node evicted while unreachable** keep running if
  the node comes back; the eviction lists each one with its cleanup command.
- **Worker compromise** exposes the env of the sandboxes it hosts (see §11).

## Alternatives Considered

### Option A: Re-implement sandbox semantics as raw container endpoints

Add worker endpoints that manipulate containers directly and implement
sandbox behavior (networks, egress proxy, ownership fix-ups) on the control
plane over them. Rejected: it duplicates ~9k lines of Docker provider logic
across a network boundary and guarantees drift.

### Option B: Run the Docker provider on the worker (chosen)

The worker binary is the control-plane binary, so the provider is already
there; the only cost is a thin serialization layer and the version-skew
error above.

### Option C: Move `SandboxProvider` out of `temps-agents` now

Cleaner crate boundary, but touches every consumer for no user-visible gain.
Deferred until a second consumer outside `temps-agents` appears.

### Option D: Per-sandbox allow-lists

The need is an operator policy ("only these nodes run sandboxes"); explicit
per-sandbox choice is already `--node`. Rejected as premature.

### Option E: Store allowed nodes as node labels

Would reuse `nodes.labels`, but the control plane has no node row, and the
dev harness documents that labels are only read at first join. A settings
field plus a dedicated endpoint is simpler and auditable.

## References

- ADR-008: In-Sandbox PTY Agent (phase 2 terminal proxy)
- ADR-009: Sandbox API Versioning (optional field is non-breaking)
- ADR-010: Provider Boundary Traits
- ADR-013: Sandbox Egress Credential Proxy
- ADR-020: Multi-Node Deployment Hardening (worker auth reused)
- ADR-029: Firecracker Sandbox Backend
- ADR-036: Persistent Workspace Sandboxes
- ADR-037: Sandbox Snapshots
- `crates/temps-agents/src/sandbox/{mod,remote,node_routing}.rs`
- `crates/temps-agent/src/sandbox_handlers.rs`
- `crates/temps-sandbox/src/services/placement.rs`
- `crates/temps-sandbox/src/handlers/placement.rs`
- `temps-fleet/src/temps_sandbox_runtime.rs` (must keep working unchanged)
