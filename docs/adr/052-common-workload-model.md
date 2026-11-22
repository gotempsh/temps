# ADR-052: A common workload model for deployed containers and sandboxes

**Status:** Proposed
**Date:** 2026-10-18
**Author:** David Viejo
**Superseded in part by:** [ADR-055](./055-workload-plane-and-runtime-drivers.md) §5 (limits, placement, worker API, labels); §1 to §4 are accepted together with it, or absorbed into it
**Builds on:** ADR-011 (internal DNS), ADR-013 (sandbox egress credential proxy), ADR-036 (persistent workspace sandboxes), ADR-048 (multi-node sandboxes), ADR-050 (microsandbox backend), ADR-084 (sandbox service access)

---

## Context

Temps runs two kinds of container on behalf of a user, and they have grown
two full stacks that answer the same questions differently.

| Question | Deployed containers | Sandboxes |
|---|---|---|
| How is one launched? | `ContainerDeployer::deploy_container(DeployRequest)` in `temps-deployer`: image, env, secret files, networks, `ResourceLimits`, restart policy, labels | `SandboxProvider::create(SandboxCreateConfig)` in `temps-agents`, fed by `CreateSandboxRequest` in `temps-sandbox`: image, env, cpu, memory, pids, disk, ports, backend, node |
| Where does it run? | `NodeScheduler` (round robin, least loaded, anti-affinity); `deployment_containers.node_id` | `services/placement.rs` (operator allow-list, fewest sandboxes); the sandbox row |
| How does a worker run it? | `/agent/containers/*` on `temps agent`, spoken by `RemoteNodeDeployer` | the sandbox host API of ADR-048 §6, spoken by the remote sandbox provider |
| Which services may it use? | `project_services`, resolved by `ExternalServiceManager::get_runtime_env_vars` | `sandbox_services` (ADR-051 §3b), resolved by `SandboxAccessEngine::issue` |
| What credentials does it get? | the cluster administrator's, naming a database per project environment | a scoped role limited to a database of its own (ADR-051 §2) |
| How do variables reach it? | assembled by `env_resolver` / `workflow_planner`, set at container creation; secrets as `/run/secrets` files | resolved server side at every command, redacted from output (`is_secret_variable`) |
| What can it reach? | the `temps` network and, across nodes, the flat overlay with `*.temps.local` DNS (ADR-011); nftables allows whole node CIDRs | one internal network per sandbox holding only linked service containers, and a per-sandbox egress proxy (ADR-013) |
| What limits it? | `ResourceLimits` | `cpu_limit`, `memory_limit_mb`, `pids_limit`, `disk_size_mb` |
| How is it seen? | `sh.temps.*` labels, container logs, stats | sandbox events, exec output, no shared labels |

Both stacks also share one protection that already crosses the line: cloud
metadata egress is blocked by one nftables table for every routed container
network (`metadata_egress.rs`). It is the exception. Everything else above is
built twice.

The duplication has a concrete cost, seen in the last weeks:

1. **Features land in one stack.** Scoped credentials, per-service engines and
   the link table (ADR-051) were written for sandboxes only. A deployed app
   still connects as the cluster administrator. The two code paths for "give
   this workload access to a database" now diverge in what they grant.
2. **A sandbox cannot reach a service on another node**, although a project can,
   because the overlay and DNS belong to the deployment stack and the remote
   sandbox provider refuses application networking (ADR-048 §6).
3. **Network policy would have to be written twice.** Projects have no
   per-workload rules at all, and sandboxes have structure instead of rules.
   Neither can express "this workload may reach that service on that port".
4. **The console duplicates its surfaces.** Connecting a service to a project
   and connecting one to a chat's sandbox are different components over
   different endpoints for one idea.
5. **Fixes have to be remembered twice.** `runtime_environment` handed a
   sandbox the production credentials of its project, which is the mistake the
   deployment path makes by design.

The two kinds are not the same thing, and this ADR does not pretend they are:
a deployment is an immutable image rolled out blue/green behind health checks;
a sandbox is a mutable workspace that suspends on idle, takes commands, has
snapshots and may run in a microVM. What they share is **what the container is
connected to and what it is allowed to do**.

## Decision

Introduce a **workload model**: one description of identity, connections,
variables, network permissions, limits and placement that both kinds use. The
lifecycle drivers (deploy pipeline, sandbox registry) stay separate and become
two implementations of it, not one merged runtime.

### 1. Subject and kind

A **workload** is anything Temps runs on a user's behalf. It has a kind and a
subject that owns its connections:

| Kind | Subject | Lifecycle driver |
|---|---|---|
| `deployment` | a project environment | `temps-deployments` / `temps-deployer` |
| `sandbox` | a sandbox | `temps-sandbox` / `temps-agents` |

`WorkloadRef { kind, id }` is the identity everything below attaches to. It
exists as a type in a new leaf crate, `temps-workload`, with no dependency on
either driver. Both drivers, `temps-providers` (access engines) and
`temps-network` (enforcement) depend on it; it depends on none of them.

### 2. Service access: one engine, any subject

`SandboxAccessEngine` (`temps-providers/src/sandbox_engines.rs`) becomes
`ServiceAccessEngine`. Its input names a subject instead of a sandbox:

```rust
pub struct AccessRequest {
    pub service: ExternalServiceRef,
    pub subject: WorkloadRef,
    pub namespace: String,       // project slug + environment, or sandbox namespace
    pub scope: CredentialScope,  // Scoped | Administrator
    pub parameters: ServiceParameters,
}
```

* `Scoped` is what sandboxes get today: a role, ACL user or bucket
  credential limited to a database or prefix of the subject's own.
* `Administrator` is what deployments get today. It is kept, named, and
  produced by the same engine, so there is one place that knows how to build
  `POSTGRES_URL` or `REDIS_URL` for each service type, instead of the
  `get_runtime_env_vars` implementations and the sandbox engines each doing it.
* The set of services a subject may use stays in its link table.
  `project_services` and `sandbox_services` remain separate until §6 phase 3
  needs one owner for network rules; merging them earlier would only move
  columns around (`database_provisioning_mode`, `custom_database_name` have no
  sandbox meaning).
* Deployments are **not** switched to `Scoped` by this ADR. Doing so changes
  what an app can do (migrations that need superuser, extensions), so it is
  opt-in per environment, a column on the environment row, decided in phase 5.

### 3. Variables: one assembly, two delivery modes

Variables are assembled in layers by a single function, in this precedence
(later wins): platform-provided, service access (§2), project and environment
variables, per-workload overrides. Secret classification (`is_secret_variable`,
today only in `temps-ai-chat`) moves to `temps-workload` and is used by both.

What differs is **delivery**, and it is a property of the kind, not of the
variable:

| Delivery | Used by | Behaviour |
|---|---|---|
| `Baked` | deployments | environment set at container creation; secrets as read-only files under `/run/secrets` (mode 0400), never in `docker inspect` |
| `PerExec` | sandboxes | resolved server side for every command, redacted from output, never stored in the container's environment |

The sandbox rule exists because the sandbox runs unreviewed code that can read
its own environment; the deployment rule exists because a deployment restarts
without a control plane present. Neither is changed. The point of the shared
assembly is that a new service type, or a new layer, is written once.

### 4. Network permissions: one rule model

Both kinds get the same, explicit model for what a workload may reach. It is
data, not structure:

```
network_policy_rules(
  id, subject_kind, subject_id,
  direction      -- egress | ingress
  peer_kind      -- service | workload | cidr
  peer_ref       -- service id, WorkloadRef, or CIDR
  port_start, port_end, protocol,
  action         -- allow | deny
  origin         -- link | operator | system
  created_by, created_at, updated_at
)
```

* **A service link creates a rule.** Linking a service to a project or a
  sandbox inserts an `origin = link` allow rule for that service's address and
  ports, and unlinking removes it. "Connected" and "permitted" cannot drift
  apart, because they are the same row.
* **Defaults differ by kind.** A sandbox is default-deny: it reaches only what
  a rule allows. A deployment is default-allow, as today, and an environment
  may opt into default-deny with a flag on its row. Existing projects change
  nothing.
* **Enforcement is nftables, per node, fail-closed.** A reconciler in
  `temps-network` compiles the rules for the workloads on its node into sets
  keyed by workload identity (not IP literals, which change on restart). The
  drop for a restricted workload is installed **before** the overlay's
  node-CIDR allow, so a missing or failed rule blocks traffic instead of
  exposing it. It reconciles from an `updated_at` cursor (O(changes), not a
  table scan). The metadata-egress table keeps its own, earlier, priority.
* **DNS follows policy.** The per-node `*.temps.local` resolver (ADR-011)
  answers a restricted workload only for names its rules allow, so an
  unreachable service is also undiscoverable.
* **Reaching a service on another node** is then the same operation for both
  kinds: allow rule, overlay route, DNS name. A sandbox joins the overlay as a
  restricted workload; the control plane dials the node's private address to
  provision credentials. The remote sandbox provider stops refusing
  application networking once it can attach a sandbox under these rules, and
  continues to refuse it where it cannot.
* **Backends that cannot enforce fail the link.** Firecracker and microsandbox
  networking differ from Docker bridges. A backend that cannot apply a rule
  returns a typed error and the link is not created; it never degrades to
  "connected but unrestricted".
* **HTTP-level egress stays separate.** The sandbox credential proxy (ADR-013)
  keeps hostnames and credential injection; extending hostname allow-lists to
  deployments is a later decision, not part of this one.

### 5. Limits, placement, worker API, labels

* **`ResourceLimits`** (cpu, memory, pids, disk) moves to `temps-workload`
  and is the one type both kinds pass to a driver. A backend that cannot
  enforce a limit (the disk size on Docker, today) says so in a typed error
  instead of ignoring it silently.
* **Placement** splits into the part that is shared and the part that is not.
  *Eligibility* (node is active, not draining, allowed by the operator, role
  and labels match) is one function used by `NodeScheduler` and
  `services/placement.rs`. *Strategy* (replicas and anti-affinity for
  deployments; one node, fewest sandboxes, for sandboxes) stays with each
  driver.
* **The worker API** converges on one container lifecycle surface for the
  operations both need (create from a spec, start, stop, remove, exec, logs,
  stats, info). Sandbox-only operations (filesystem, snapshots, terminal
  agent) are additions beside it. ADR-048 §6 is written against this surface
  instead of defining a second one.
* **Labels.** Every workload container carries `sh.temps.kind =
  deployment|sandbox` next to the existing `sh.temps.*` labels, so logs,
  metrics, the metadata-egress rules and the policy reconciler find them the
  same way.

### 6. What stays different, on purpose

* **Lifecycle.** Deployments are immutable, rolled out blue/green, gated on
  health checks, and keep history. Sandboxes are mutable, suspend on idle,
  wake on access, snapshot. Neither learns the other's state machine.
* **Isolation backends.** Firecracker and microsandbox are sandbox backends;
  deployments run on Docker.
* **Exposure.** Deployments are served by the proxy under project domains.
  Sandbox previews go through the preview gateway behind a password.
* **Credential posture.** A sandbox never receives production credentials
  (ADR-051 §1). The shared engine does not change that: the sandbox kind
  only ever asks for `Scoped`.
* **Promotion.** A sandbox becomes a deployment by building its files into an
  image (`temps_deploy_sandbox_source`), not by changing its kind.

### 7. Consent, authorization, audit

Nothing here widens what a user may do. Creating, changing or removing a rule
needs the permission that links the service today (`ExternalServicesWrite`,
plus `SandboxesExec` for sandboxes) and is audit-logged with the rule, its
subject and its origin. Rules with `origin = link` are not editable on their
own: they change by linking and unlinking. Operator and system rules need an
administrator.

## Phases

Each phase ships on its own, changes no behaviour for existing deployments, and
leaves the tree compiling.

1. **Types and engine.** Create `temps-workload` (`WorkloadRef`,
   `ResourceLimits`, `CredentialScope`, secret classification). Rename the
   sandbox access engine to `ServiceAccessEngine`, add the subject and scope,
   and route the deployment path's `get_runtime_env_vars` through it with
   `Administrator`. Pure refactor; the existing tests of both paths must pass
   unchanged.
2. **Shared variable assembly.** One layered assembly with the two delivery
   modes; `env_resolver` and the sandbox command path call it. Adds the
   `sh.temps.kind` label.
3. **Network rules.** The `network_policy_rules` table, link-created rules,
   the node reconciler and its fail-closed ordering, policy-aware DNS. First
   consumer: a control-plane sandbox reaching a service on a worker, tested on
   two real nodes. The decision on merging the two link tables is taken here,
   with the rules in front of us.
4. **Placement and worker API.** Shared eligibility; the common container
   surface on `temps agent`; remote sandboxes on worker nodes attach under
   rules.
5. **Scoped credentials for deployments.** Per-environment opt-in, with a
   migration checklist for apps that need more than their own database.

A single console surface ("Connections": services and network permissions of
a project environment or a sandbox) replaces the two connect components during
phase 3, and shows an onboarding state, not a missing control, when a feature
is not configured.

## Migration and rollback

The risk in this ADR is not the new code, it is changing what running
deployments and existing sandboxes do. The rules below are constraints on how
every phase is built, and a phase that cannot meet them is split or dropped.

**Principles**

1. **Expand, then contract, in separate releases.** A schema change adds
   (a table, a nullable or defaulted column) and nothing reads it until a later
   change. Nothing is dropped or renamed in the release that introduces its
   replacement. Every migration ships a working down migration.
2. **The old path stays authoritative until the new one is proven.** A new
   resolver, assembler or reconciler first runs next to the old one, compares
   results and logs differences (`shadow`), and only then takes over
   (`enforce`). The mode is a column on the row it governs (environment,
   sandbox, node), changed through the API and audit-logged, never an
   environment variable.
3. **Existing isolation is never removed by this ADR.** The per-sandbox
   internal network, the egress proxy and the metadata-egress table all stay
   under the new rules. Rules add a second barrier; they replace none.
4. **Existing deployments change nothing by default.** Environments get
   `network_policy_mode = open`; deployments keep `Administrator` credentials;
   `Baked` delivery is untouched. A deployment only changes behaviour when
   someone turns a setting on for that environment.
5. **No user data moves.** Linking, unlinking and rule changes never create,
   copy, rename or drop a database, bucket or key. Removing a link removes
   access only (as in ADR-051).
6. **Mixed versions are normal.** Workers upgrade after the control plane, and
   sometimes weeks later. Every new field is optional and ignored when
   unknown, a worker advertises what it supports in its heartbeat, and the
   control plane falls back to the old call when a worker does not. Old worker
   endpoints are removed no earlier than two releases after the replacement
   ships.

**Per phase**

| Phase | Schema | Behaviour change for existing workloads | Proof before it merges | Rollback |
|---|---|---|---|---|
| 1. Types and engine | none | none | a golden test per service type: the variable map the new engine builds for `Administrator` equals what `get_runtime_env_vars` builds today, key for key; the sandbox engines' existing tests pass untouched | revert the commit |
| 2. Variable assembly, `sh.temps.kind` | none | none; containers created earlier lack the label, and every consumer treats a missing label as `deployment` or by its existing sandbox marker, so nothing restarts | the same golden comparison for the assembled variable set (precedence, secret files, redaction) | stop writing the label; consumers already tolerate its absence |
| 3. Network rules | one new table, empty; one defaulted column on environments and on sandboxes | none for deployments (`open`). Sandboxes keep today's structural isolation; rules only add the cross-node path, which does not exist yet | an upgrade test: restore a database from the previous release with projects, links and sandboxes, run the migration, assert no row changed and no container was touched; a shadow-mode run on real links showing the ruleset it would install; a test that a restricted workload cannot reach an unlinked service | set the mode back to `off` (rules stop being installed and the table is ignored); down migration drops the table and columns |
| 4. Placement and worker API | none | none; new endpoints sit beside the old, used only for workers that advertise them | a mixed-version test (new control plane, old worker) that deploys and runs a sandbox | stop advertising the capability; the control plane returns to the old calls |
| 5. Scoped deployment credentials | one defaulted column on environments | none until an operator enables it for one environment | a preflight that creates the scoped role and connects as it before anything is switched; the environment keeps the administrator credentials until that succeeds | flip the column back; the scoped role is left in place and unused, no data is touched |

**The table merge is not part of these phases.** `project_services` and
`sandbox_services` stay as they are. If phase 3 shows that one owner is needed,
it is done as its own change: create the new table, dual-write both, backfill
idempotently, switch reads behind a mode column, and drop the old tables no
earlier than the following release. Until then there is no migration of link
data at all.

**What this ADR will not do.** It does not change a default for an existing
environment, switch a deployment's credentials, remove an endpoint in the
release that adds its replacement, or move or copy user data.

## Consequences

**Positive**

* A new service type or credential rule is written once.
* Sandboxes and projects reach services on any node the same way.
* "What can this workload reach?" has one answer, one table and one audit
  trail, and the console can show it.
* Deployments can later drop the cluster administrator's credentials without a
  second implementation.

**Negative**

* A new crate and a rename across `temps-providers`, `temps-ai-chat`,
  `temps-sandbox` and `temps-deployments`; phase 1 touches many files for no
  visible change.
* Network rules are a new correctness surface. A wrong ordering or a failed
  reconcile is a tenant-isolation bug, not a cosmetic one. Mitigated by
  fail-closed ordering, a drift check that reports unenforced rules, and tests
  that a workload cannot reach an unlinked service.
* Enforcement is Linux-only. Local development on macOS relies on the
  container VM; behaviour there must be documented, not assumed.

**Risks**

* The shared engine could blur the line between `Scoped` and `Administrator`.
  The type carries it, a sandbox request for `Administrator` is rejected at
  the boundary, and a test pins it.
* Policy-aware DNS adds a lookup dependency to the resolver; it must stay O(1)
  per query from an in-memory set refreshed by the same cursor.

## Not decided here

* Whether `project_services` and `sandbox_services` merge into one table
  (revisit in phase 3).
* Hostname-level egress allow-lists for deployments.
* Whether a project environment, rather than the project, owns the link, which
  would let two environments use different services.
* The rule UI for operators beyond the Connections surface.
