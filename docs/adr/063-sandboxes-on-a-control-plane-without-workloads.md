# ADR 063: Sandboxes on a control plane that runs no workloads

## Status

Accepted — 2026-10-24

Related: ADR-048 (multi-node sandboxes), the `control-plane` serve profile
(`LocalWorkloadPolicy`, `DockerHandle`, `worker_node_required_problem`).

## Context

A hosted control plane runs with `--profile control-plane`: it serves the
console, observability and orchestration, and runs no containers itself.
Applications run on worker nodes that joined with `temps join`.

`serve/console.rs` did not register `AgentsPlugin` or `SandboxPlugin` under
that profile, because the agents plugin builds a sandbox provider on the
local Docker daemon. As a result, nothing served `/v1/sandboxes/*`. The
node page's Sandboxes tab calls `GET /v1/sandboxes/placement/nodes/{node}`.
On every node of a hosted plane, including healthy workers, it showed:

> Could not load sandboxes on this node: Not Found

This was wrong in three ways:

1. **Workers can run sandboxes.** ADR-048 already places standalone
   sandboxes on worker nodes over their mTLS agent API. Workers were never
   the problem; the control plane's own host was.
2. **The error said nothing.** It came from a route that did not exist,
   not from a decision.
3. **`PlatformFeatures.sandboxes` was `false`.** The Sandboxes page told the
   operator the feature does not exist, while the workers they had just
   joined could run sandboxes.

There was a related bug in every profile. The node page addresses the
control plane by its `nodes` row id. `node_sandboxes` only knows the control
plane as `0` / `control-plane`, so the control-plane row's Sandboxes tab
answered "Node '1' does not exist", even on a full install.

## Decision

### 1. Both plugins load in every profile

`AgentsPlugin` and `SandboxPlugin` are registered unconditionally. The
profile decides only which provider runs on **this host**. That decision is
a pure function, `host_sandbox_choice(local_workloads, docker_reachable,
allow_unsandboxed)`:

| local workloads | Docker reachable | `TEMPS_ALLOW_LOCAL_SANDBOX` | host provider |
|---|---|---|---|
| on  | yes | any | Docker (+ Firecracker / microsandbox when set up), unchanged |
| on  | no  | set | unsandboxed `LocalSandboxProvider`, unchanged (dev only) |
| on  | no  | unset | refuse to start, unchanged |
| off | any | any | **`NoLocalSandboxProvider`** |

When local workloads are off, the plugin never connects to Docker. It does
not quarantine containers, probe for Firecracker or microsandbox, or stage
guest agents. **It never falls back to a local or unsandboxed provider**,
whatever the environment says. On a shared, multi-tenant control plane, an
unsandboxed fallback would run untrusted code as the Temps service user,
next to every tenant's credentials.

`NoLocalSandboxProvider` is the `local` half of the existing ADR-048
`NodeRoutingSandboxProvider`. A sandbox with a `node_id` still goes to its
worker exactly as before. Anything addressed to this host fails with the
typed `AgentError::NoLocalSandboxHost`. In practice that means agent runs,
application workspaces, image builds, and legacy rows without a node.

`is_available()` is `false`, and `supports_backend()` is `false` for every
backend. Recovery finds nothing, and releasing orphans does nothing. A
create therefore fails loudly, while a cleanup path stays a quiet no-op.

The agent `AppState` now holds the `DockerHandle` instead of a raw
`bollard::Docker`. The preview-gateway endpoints on a control plane
therefore answer the canonical worker-node-required 409; the plugin no
longer panics in `require_service::<bollard::Docker>()`.

### 2. Placement never chooses the control plane

`PlacementPolicy` gains `control_plane_runs_no_workloads`. The default is
`false`, so a full install is unchanged. `SandboxPlugin` sets it from the
`LocalWorkloadPolicy`. When it is set:

- `allows(0)` is false, whatever the allow-list says. Automatic placement
  always ranks workers, and the control plane is never a candidate.
- An explicit `node: "control-plane"` request fails with
  `SandboxError::NoLocalHost`. That is a 409 telling the caller to leave
  `node` empty or to name a worker.
- If automatic placement finds no worker that can take the sandbox, it
  fails with `SandboxError::NoWorkerNodeForSandboxes`. This error goes
  through `temps_core::worker_node_required_problem`, so it carries:
  - status 409;
  - `error_code: WORKER_NODE_REQUIRED`;
  - `setup_path: /settings/nodes`;
  - a detail that names every worker and why it was passed over (offline,
    build-only, excluded by placement settings…), or says none has joined.

  This is the same Problem every other "needs a worker" path returns, so the
  console renders it with the same **Add worker node** action.
- Application workspaces (`host_work_dir_override`) need a directory on this
  host. They fail with `SandboxError::NoLocalHost` rather than being
  "placed" on a host that cannot run them.
- `describe()` lists the control plane with `eligible: false` and the
  reason "this control plane runs no workloads; sandboxes run on worker
  nodes".

If a worker answers that a feature is unsupported there (ADR-048 phase 1),
the hint no longer sends the caller to `--node control-plane`. It now says
that this works only on an install whose control plane runs workloads.

### 3. What the control-plane node's Sandboxes tab shows

`node_sandboxes` resolves the control plane's `nodes` row (role
`control-plane`) as the control plane. This fixes the tab in every profile.
The tab shows:

- **Full install:** unchanged. It lists the sandboxes on this host and says
  whether the host accepts new ones.
- **No local workloads:** a statement, not an error: "This control plane
  runs no workloads, so sandboxes never run here; they run on worker
  nodes". It links to Settings → Nodes. Any rows left from before the
  instance was re-profiled stay listed, so they can still be destroyed.

The panel never shows a bare HTTP reason phrase:

- A worker-node-required Problem renders the shared `WorkerNodeRequiredAlert`.
- A response with no Problem detail is replaced with a sentence about what
  failed and what to try.

### 4. `PlatformFeatures` tells the truth

- `sandboxes` means "the sandbox API is served and sandboxes can run on some
  node". It is `true` in both profiles.
- New field `sandboxes_local` means "sandboxes, AI agent runs and
  application workspaces can run on this host". It is `true` on a full
  install and `false` on a control plane.

The console reads `sandboxes_local` to explain the control-plane tab. It
reads `sandboxes` to decide whether to query the API at all.

### 5. Local assumptions are gated, and a full install is unchanged

When local workloads are off, `SandboxPlugin` initialization skips recovery
of control-plane sandbox rows and the Firecracker/microsandbox VM
reconciliation, and logs one line saying so. Two things still run, because
both only touch rows and workers:

- releasing orphaned agent-run rows;
- the expiration sweeper, which stops expired sandboxes on workers.

On a full install, `local_workloads` is true. Every branch above takes its
pre-existing path, and the placement flag keeps its default of `false`.

## Not in scope: agent runs on workers

AI agent runs (workflows, autofix, scheduled agents) and application
workspaces still need a host that runs workloads. On a control plane they
fail with a typed error before any repository is cloned.

`SandboxRegistry::require_local_host`, called by the executor and the
autofixer, carries the error onto the run record:

> … this control plane runs no workloads … AI agent runs, autofix and
> application workspaces cannot run on worker nodes yet.

Moving them onto workers is ADR-048 phase 2/3 work, and none of it is small
or safe to improvise:

- the executor clones the repository on the control plane and bind-mounts it
  into the sandbox;
- it reads results back with `git diff` on the control plane;
- it relies on workspace volumes, the git/model/harness-MCP relays and the
  retained agent runtime, none of which a worker serves yet (ADR-048 §phase 1
  limits);
- a sync-back endpoint would extract an untrusted tarball on the control
  plane.

Each of those needs its own design and security review.

## Consequences

- A hosted plane's worker nodes show their sandboxes. Creating a sandbox
  there works, and the control plane is never chosen.
- A hosted plane with no usable worker answers a create with a 409 that
  links to Settings → Nodes, instead of a 404.
- Scheduled and triggered agent runs on a hosted plane now fail visibly with
  a reason. Before, they were dropped silently because the plugin that
  consumes the jobs was absent.
- Application harness turns fail with the typed `NoLocalHost` error instead
  of the gateway's generic "fails closed".
- The OpenAPI `PlatformFeatures` schema gains `sandboxes_local`. Clients
  that ignore unknown fields are unaffected.
