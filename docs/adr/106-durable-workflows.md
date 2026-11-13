<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

<!-- SCOPE: Temporal/Restate-style durable workflows orchestrated by the Temps control plane and executed by the project's own containers through an SDK; one crate (temps-workflows), one API, a versioned executor protocol and SDKs, reusing auth, audit, encryption, blob storage, OTEL, error tracking, alarms and the workload layer. -->

# ADR-106: Durable workflows orchestrated by Temps

- **Status:** Proposed
- **Date:** 2026-11-13
- **Deciders:** Temps maintainers
- **Related:** ADR-034 (feature flags, the reference crate shape), ADR-039 (MCP server), ADR-055 (workload plane), ADR-068 (environment addresses), ADR-076 (keyed ingest), ADR-089 (edge rules, a permission precedent)

## Context

### What applications need

Applications on Temps run long, multi-step work today with ad-hoc tools: a
queue library, a cron that polls a table, a `setTimeout` that dies with the
container. Each of these loses work on a crash or a deploy, and none of them
can be seen from the console. What they need is the same list every time:

- **resume after crashes and deploys** without repeating completed work;
- **retries** with backoff, **timeouts**, and **cancellation**;
- **sleeps** of minutes to days;
- **waiting for an event or an approval**, with a timeout;
- **visibility** (what is running, what failed, why) and **alerts**.

### Use cases

- **Product:** onboarding sequences, dunning, sagas, webhook processing with
  dedupe, chunked imports, media processing.
- **AI:** durable agent loops, human review of AI output, RAG ingestion,
  batch LLM work.
- **Operations:** backfills, tenant provisioning, GDPR flows, runbooks with
  approval, releases, credential rotation.
- **Waits:** approvals, e-signature, bank transfers.

Webhooks, onboarding and AI loops come first; approvals (waiting for an
event) are in M1.

### How the established engines work

- **Temporal**: workers long-poll task queues on the server and replay
  deterministic workflow code against the recorded event history; any
  non-determinism in the code is a bug the history detects.
- **Restate**: the server pushes an invocation to an HTTP handler; the SDK
  journals each step's result on the server before continuing, and a
  re-invocation replays the journal so recorded steps return their result
  without running again.
- **Neither server runs user code.** The server stores history and decides
  what runs next; the user's process executes.

### Constraints

- A 2 vCPU / 4 GB box runs Temps, its database and the user's workloads
  (`AGENTS.md`, Scalability). There is no room for a second engine with its own
  storage.
- Control plane only: nothing on the proxy hot path.
- Safe with several control-plane processes against one database.
- No environment variables for runtime configuration: settings are columns.

## Decision

Temps is the orchestrator (the server role in Temporal/Restate terms).
Workflow code runs in the project's own containers through an SDK, with a
Restate-style push protocol: Temps invokes an HTTP handler in the app, the
SDK journals every step on Temps before continuing, and suspension ends the
request. New pieces are one crate (`temps-workflows`), one API, a versioned
executor protocol and the SDKs. Everything else is existing infrastructure.

### 0. Boundary

`temps-workflows` owns only run state, the journal, waits, leases, the
scheduler and the invoker, and the workflows API. It depends on `temps-core`
traits, `temps-entities`, `temps-authz`/`temps-auth`, `temps-audit` (through
the `AuditLogger` trait) and the metrics interface. It does **not** depend on
`temps-deployments`, the proxy, analytics or error tracking. Everything else
reaches it through `temps-core` traits:

- **Workloads and deployments**: `ExecutorEndpoints` (healthy replicas'
  internal addresses). Workflows never create or scale containers.
- **Drain and deploy warnings**: implemented in deployments on top of the
  workflows API (M2/M3).
- **OTEL and error tracking**: fed by the SDK from inside the app; Temps stores
  only the `trace_id` and the error event reference on the step.
- **Metrics**: written through a `temps-core` sink trait; alerts are ordinary
  alarm rules, with no alerting code in the crate.
- **Auth, audit, `EncryptionService` and blob storage.**
- **Existing deploy paths**: Git, local image upload, registry image. There is
  no workflow-specific deploy.
- **The MCP server and assistant policy, the AI gateway, tunnels.**
- **Managed Postgres** as an optional M3 journal store, through its existing
  credentials.
- **The cron jobs feature** for schedules (M2), or a minimal schedule column.

The SDKs reuse the existing JS SDK, OTEL and error-tracking packages.

### 1. Ownership

- Every workflow belongs to a **project and environment**. Project or
  environment deletion removes it (foreign keys `ON DELETE CASCADE`).
- Cross-project use goes through explicit, revocable, audited **grants** (M3):
  start a workflow, deliver an event.
- An instance-wide **"All workflows"** view (M2).
- A **"Workflow worker"** preset for teams without an app (follow-up).
- Roles: **orchestrator** = the control plane; **executor** = a project
  service through the SDK; **visibility** = existing subsystems.

### 2. Registration

- The environment's workflow settings row names the **host service**; the
  default is the environment's main service (its current deployment).
- The SDK serves `POST /.well-known/temps/workflows/invoke` and
  `GET /.well-known/temps/workflows/manifest`.
- The manifest (workflow names, versions, step names known statically,
  per-workflow concurrency) is read **after each deploy** and cached in
  memory, keyed by deployment id. It is never stored: the deployment is the
  source of truth, and a restart re-reads it.

### 3. Lifecycle

1. **Start**, with an optional idempotency key, unique per environment and
   workflow.
2. The run is `queued` and due now.
3. The invoker claims it with `FOR UPDATE SKIP LOCKED`, takes a **lease with a
   fencing token** (a per-run counter incremented on every claim), and POSTs
   run id, attempt, input, the journal (or a cursor when it is large) and a
   **short-lived signed token** bound to the run and the lease, to a healthy
   replica.
4. The SDK **replays**: recorded steps return their recorded results; a new
   step runs and is recorded **before** the workflow continues
   (`POST /workflow-runs/{id}/steps` with the lease token). A write carrying a
   stale lease is rejected.
5. The request ends on **completion, failure or suspension** (sleep,
   `waitForEvent`, retry backoff). Temps sets `due_at` and the awaited event,
   and releases the lease. No connection stays open.
6. A timer or an event makes the run due again.

The invocation deadline is 15 minutes by default (a settings column).
Heartbeats extend the lease; a dead container's lease expires and the run is
reinvoked.

### 4. Primitives

- `step(name, fn, {retries, timeout})`; `sleep(name, duration)` and
  `sleepUntil(name, date)`; `waitForEvent(name, {event, key, timeout})`;
  `sendEvent(event, key, payload)`;
- child workflows (M2): `ctx.child(name, workflow, input)` starts a run of
  another workflow in the same environment, exactly once per journal entry
  (idempotency key `child:<parent>:<sequence>`), and suspends until it
  finishes. The parent receives `{run_id, status, output, error}` for a
  completed, failed, cancelled or deleted child and decides itself; a child
  failure does not fail the parent. Child waits have no timeout. Cancelling a
  run cancels its descendants; a parent that fails while a child runs does
  not. Event names starting with `temps.` are reserved for Temps;
- cancellation;
- `ctx.now()` and `ctx.random()`, recorded in the journal;
- per-workflow concurrency limits.

**The contract**: step bodies are idempotent, step names are stable, and the
workflow function uses no wall-clock time or randomness outside `ctx`. A
replay that finds a different step than the journal recorded raises
`WorkflowIncompatible { run_id, step, expected, found }`.

### 5. Storage

- **Trait:** `WorkflowStore`, with a scheduling part (claim, lease, release,
  due/wait bookkeeping) and a journal part (append, read, truncate).
- **Default:** the control-plane Postgres. Nothing to provision, one scheduler
  query, one transaction with the run state, covered by the existing backups.
  The tables are `workflow_*` tables in the default schema, not a separate
  Postgres schema (see "Verified against the code", item 13).
- **Opt-in managed Postgres journal per environment (M3):** scheduling state
  always stays central. Write order: the journal entry first, idempotent,
  keyed by (run, step, lease), then the run state. A crash between the two
  writes is tested.
- **Guardrails:**
  - an inline payload cap (256 KiB, a settings column), with blob offload above
    it and a typed oversize error above the blob cap or without blob storage;
  - the journal is a **plain table**, deleted with its run, in batches, by
    per-environment retention on finished runs (a hypertable was checked and
    does not fit, item 12);
  - per-environment quotas (active runs, steps per run, journal bytes) with
    typed errors that block **new** runs only, never running ones;
  - journal size as a metric.
- **Tables** (M1 migration):
  - `workflow_runs`: id (UUID), project, environment, workflow name, status,
    input, output or error (encrypted), idempotency key, parent run, deployment
    id at start, attempt, lease token, lease expiry, fencing counter, `due_at`,
    awaited event name and key, consecutive lease expiries, timestamps;
  - `workflow_steps`: run, sequence, name, kind, attempts, status, result or
    error (encrypted), duration, `trace_id`, error reference, timestamps;
  - `workflow_events`: environment, event name, key, payload (encrypted),
    consumed-by run, timestamps;
  - `workflow_environment_settings`: host service, limits, retention, quotas,
    and a nullable managed-store column (unused until M3);
  - `workflow_credentials`: the per-environment app credential, hashed.
- **Sensitive data:**
  - **Payloads** are inputs, outputs, step results, event payloads and error
    messages. Everything else (names, statuses, counts, timestamps, keys) is
    plaintext metadata. Names and keys must contain no personal data; the SDK
    warns in development when a key looks like an email address.
  - **Layer 1 (M1)**: always AES-256-GCM through `EncryptionService` with
    **associated data** binding the ciphertext to its project, environment,
    run, step and field, including blob uploads. There is no plaintext option.
    `EncryptionService` gains `encrypt_with_aad`/`decrypt_with_aad` (item 6).
  - **Layer 2 (M2)**: an optional SDK codec with an app-held key; Temps then
    only ever sees the codec's ciphertext.
  - **Layer 3 (M3)**: per-project wrapped keys with crypto-shredding, added to
    `EncryptionService` itself.
  - **Reveal** needs `WorkflowPayloadsRead` and is audited. Payloads are masked
    by default and never appear in URLs, lists, exports, telemetry, error
    extras, logs or notifications.
  - **Secrets are never payloads**: steps read them from env or the vault.
  - **In transit**: orchestrator-to-executor invocations use the node's data
    address (WireGuard when the mesh is up); SDK-to-Temps journal writes use
    `TEMPS_API_URL`, the instance's external URL, over TLS when it is `https`
    (item 3).
  - **Deletion**: retention, the delete-run API, project deletion. Backups keep
    ciphertext until they expire.

### 6. Scheduling

- `FOR UPDATE SKIP LOCKED` in bounded batches, so several control-plane
  processes share the work without coordination.
- `LISTEN/NOTIFY` (`workflow_runs_due`) wakes the scheduler on a start, an
  event or a release; a slow fallback poll (default 5 s) catches missed
  notifications and timers. Each tick is O(due rows), through a partial index
  on `due_at` of runnable runs.
- A **fixed-size invoker pool** (default 16 per process) bounds concurrent
  invocations; the claim batch never exceeds free invoker slots. An
  invocation mostly waits on the executor's reply, so the pool costs
  connections and a few tasks, not CPU.
- **Backpressure, never drops:** a run over its workflow's concurrency limit,
  or over the environment's, stays `queued` with `due_at` unchanged.

### 7. Routing and topology

- Invocations go to `ExecutorEndpoints`: the healthy replicas' internal
  addresses (WireGuard for workers), never public URLs. The invoke endpoint in
  the SDK rejects requests without a valid signed token.
- Orchestration is **always central** in the control plane. Code runs wherever
  the workload layer placed it: on a control-plane-only install, the control
  plane; with workers, the workers plus the control plane **only if it runs
  local workloads** (`LocalWorkloadPolicy`, item 2).
- **No schedulable node**: runs stay `queued`; the API reports why
  (`SchedulingCapability::reason`) and the UI says so.
- **Agents get no workflow logic.** Nothing changes in `temps agent`.
- **Node failure**: the lease expires and the run is reinvoked; recorded steps
  are skipped. Fencing tokens protect against network splits: a write from the
  old lease holder is rejected once a new lease was granted.
- **SDK journal writes** use the address apps already use for OTEL
  (`TEMPS_API_URL`, item 3). One round trip per step.
- **Control plane down**: apps keep serving; in-flight invocations retry their
  journal write briefly, then stop; timers wait until it is back.
- No node affinity in M1.

### 8. Deploys

- **Default, resume on latest**: a new deployment's code replays the old
  journal. A mismatch gives `WorkflowIncompatible { run_id, step, expected,
  found }`, and the run fails with that error, keeping its journal, so an
  operator can retry from a step or cancel.
- **Drain (M3)** and **deploy warnings (M2)** live in deployments, on top of the
  workflows API.

### 9. Failure semantics

- Step bodies run **at least once**; their results are recorded **exactly
  once** (the journal key is (run, sequence), and a second write of a recorded
  step returns the recorded value).
- Retries back off exponentially with a cap.
- Failed runs keep their journal.
- Repeated failed invocations or lease expiries (default 8 in a row, the
  `stuck_after_failures` settings column) mark a run `stuck`, which feeds a
  metric and an alarm template. With the backoff capped at 5 minutes, 8 covers
  a redeploy or a short executor outage without parking the run.

### 10. Visibility

- SDK spans carry `temps.workflow.name`, `temps.workflow.run_id`,
  `temps.workflow.step.name` and `temps.workflow.step.attempt`.
- Errors are tagged with the run.
- Metrics, and alarm templates (failure rate, stuck runs, queue growth) as
  ordinary `metric_alert_rules` (item 4).
- The journal is the source of truth.

### 11. Credentials

- **Workflow credential** (per environment, shown once, stored as a SHA-256
  hash, create and rotate): start runs, send events, read the status and the
  output of runs it started.
- **Invocation token**: an HMAC token over (run, lease, expiry) under an
  `EncryptionService::derive_subkey` key; it authorizes that run's journal and
  outcome only, until the lease ends.
- **API keys and MCP**: their scopes plus the assistant policy.

### 12. SDKs

TypeScript first, then Python and Go, then Rust. A versioned protocol spec
(`docs/reference/workflows-executor-protocol.md`) and a shared conformance
suite. No polling, no sandbox.

### 13. Temps' own deployments

Temps' own deployment pipeline is not moved onto the engine yet.

### 14. UI (follow-up)

- A **Workflows** sidebar entry in every project, always visible, with an
  onboarding state.
- Pages: a list with stats; workflow detail with runs, charts, alarm templates
  and settings; run detail with a timeline, trace and error links, reveal, and
  actions; an approvals inbox; settings; command palette entries.
- Reuses the templates and `RefreshButton`.

### 15. API and CLI (follow-up for the CLI)

`temps workflows list|runs|get [--reveal]|watch|start|cancel|retry
[--from-step]|resume|send-event|init|dev`, all with `--json`.

### 16. AI (follow-up)

- Operations for the assistant and MCP: list workflows, list runs, get run,
  start run (idempotency key required), cancel run, retry from step, send
  event, diagnose run. MCP reaches them through its virtual CLI over the API,
  not per-feature tools (item 8).
- The assistant policy classifies each operation; cancel and delete need
  confirmation; payloads only with permission plus an explicit reveal, marked
  untrusted.
- Durable agent loops; approvals; AI-gateway cost tracking.

M1 classifies every new operation for the assistant (reads in the read
allowlist, everything else excluded with a reason) because the console's
policy test requires it.

### 17. Onboarding (follow-up)

- No special deploy path: Git, local image upload (it exists, item 11) or a
  registry image.
- Flow: an example, then `init`, then deploy, then "1 workflow detected" and
  **Run now**, then a live timeline and alarm templates.
- The worker preset.
- M2: the in-memory runner and `dev` over tunnels.

## Verified against the code

Each assumption of the design was checked against the repository on
2026-11-13. Where the design was wrong, the decision above already carries the
correction.

1. **Workload layer, healthy replicas, internal addresses.** An application's
   replicas are the `deployment_containers` rows of the environment's
   `current_deployment_id` (`workload_instances` serves ADR-055 workloads such
   as job crons, not app replicas). The proxy routes to rows with
   `deleted_at IS NULL` and `status` running or NULL
   (`RouteTable::load_deployment_containers`); it does not filter on
   `health_state`, which is `healthy | unhealthy | unknown` and only set when a
   probe is configured. `ExecutorEndpoints` therefore takes the same rows and
   excludes `unhealthy`. The address is built the way the route table builds
   it: a remote container is `nodes.data_address()` (the mesh WireGuard
   address when the registered address is not private) plus its `host_port`; a
   local one is `RuntimeContext::resolve_service_endpoint(container_name,
   container_port, host_port)`.
2. **Whether the control plane is schedulable.** It is not a database setting:
   `LocalWorkloadPolicy::local_workloads_enabled` comes from the serve profile
   (`--profile control-plane` disables it), and `SchedulingCapability`
   (`temps-nodes`) combines it with the active worker count into
   `schedulable` and a `reason`. The control plane is never a `nodes` row. Since
   no container is ever placed on a control plane without local workloads,
   `ExecutorEndpoints` respects the setting by construction, and reports the
   capability's reason when it has no endpoint.
3. **How apps on worker nodes reach the control plane.** Through
   `TEMPS_API_URL = {external_url}/api` (`ConfigService::get_external_url_or_default`),
   with `OTEL_EXPORTER_OTLP_ENDPOINT = ${TEMPS_API_URL}/otel` authenticated by
   the deployment token (`TEMPS_API_TOKEN`), and `SENTRY_DSN` for error
   ingest, all set by `env_resolver.rs`. That is the external URL, not a
   WireGuard address, so the design's "between nodes, traffic goes over
   WireGuard" holds for invocations only; journal writes are protected by TLS
   on the external URL and by the invocation token.
4. **The metrics interface and alarm rules.** There are two. `temps_metrics::MetricsStore`
   (`service_metrics`) is keyed by a source kind (database, deployment,
   container, node) and backs `monitoring_alert_rules`; it has no project
   dimension. Project alarm rules are `metric_alert_rules` (metric name, label
   filters, group-by, `max_series`), evaluated by `temps-otel` over OTEL
   metrics (`otel_metrics`, written through `OtelStorage::store_metrics`, keyed
   by `project_id`). `temps-core` had no metrics trait. Workflow metrics
   therefore go through a new `temps_core::ProjectMetricsSink` that `temps-otel`
   implements, so alarm templates are plain `metric_alert_rules` on
   `temps.workflow.*` metrics with `environment`, `workflow` and `outcome`
   labels.
5. **Permissions and roles.** The enum is `temps_authz::permissions::Permission`
   (re-exported by `temps-auth`), with `domain:action` strings, a `from_str`,
   an `all()` list and per-role lists in `Role::permissions`; project team roles
   map through `temps-teams/src/role_permissions.rs` (viewer, deployer, admin,
   owner). The `{Domain}{Read|Write|Create|Delete}` convention fits the
   proposed names unchanged (`WorkflowPayloadsRead` is domain
   `WorkflowPayloads`). "Read-only roles" are `Reader`, and `PlatformAdmin`
   (read-only on project resources); `ApiReader` has a minimal fixed set and
   gets nothing. Team roles: viewer reads; deployer adds start, write and
   events; admin adds delete, settings and payload reveal. Deployment tokens map
   only an explicit list of permissions, so they cannot reach the workflows
   API.
6. **`EncryptionService`.** One AES-256-GCM master key; `encrypt`/`decrypt`
   take no associated data; no per-project keys; `derive_subkey(domain)`
   derives HMAC keys. M1 adds `encrypt_with_aad`/`decrypt_with_aad` (same
   `nonce || ciphertext` base64 format) to the service itself. Per-project keys
   remain M3 and need new key storage.
7. **Blob storage.** `temps-blob`'s `BlobService` is project-scoped S3 on a
   RustFS container, connected lazily, and can be unavailable; there is no core
   trait. M1 adds `temps_core::PayloadBlobStore`, implemented by `temps-blob`
   under a reserved `.temps/workflows/` prefix, and treats it as optional: with
   no blob store, a payload over the inline cap is a typed
   `PayloadTooLarge` error. Those objects are ciphertext under associated data,
   so a blob-API reader of the project sees nothing usable.
8. **MCP server and assistant policy.** `temps-mcp-server` exposes a virtual CLI
   over the REST API (`temps`, `temps_write`, `confirm_action`), with
   propose-then-confirm writes (single-use tokens, 5 minutes) behind
   `mcp_server.enabled`; there are no per-feature MCP tools. The assistant's
   API tools use the allowlists in `temps-ai-api-tools/src/allowlists.rs`, and
   the console test `every_operation_is_classified` requires every OpenAPI
   operation to be allowlisted or excluded with a reason.
9. **Cron jobs.** `crons` rows are per environment, `http` (a GET on the running
   deployment with `CRON_SECRET`) or `job` (a command in the image), with a
   concurrency policy and retry limit. Starting a workflow on a schedule needs a
   new cron mode or a schedule column (M2); nothing in M1 depends on it.
10. **Tunnels.** `temps-ingress` provides Cloudflare Tunnel and Tailscale
    connectors that bring traffic *into* the cluster. Nothing connects the
    control plane *to* a developer's machine, so `dev` over tunnels (M2) needs
    new work; it stays an open question.
11. **Local image upload** exists: the CLI's local-image deploy
    (`apps/temps-cli/src/commands/deploy/deploy-local-image.ts`), the remote
    deployment upload handler and the `verify_local_image` job. §17 needs no
    separate deployments feature.
12. **Hypertables.** The existing hypertables (`otel_*`, `proxy_logs`,
    `service_metrics`) hold append-only, loss-tolerant telemetry with one global
    time retention (`add_retention_policy`). The journal is updated in place
    (attempts, status), must live as long as its run (a run can sleep for
    days), needs a unique `(run_id, sequence)` key for idempotent writes (a
    hypertable's unique index must include its partition column), and is
    retained per environment on finished runs. Chunk-drop retention would delete
    live runs' journals. `workflow_steps` is a plain table.
13. **A separate `workflows` Postgres schema.** Every migration, the schema guard,
    post-migration index maintenance and the test harness's schema-per-test
    fallback resolve tables in `current_schema()`; no table lives in a second
    schema. The tables are `workflow_*` tables in the default schema, which
    keeps the same ownership boundary (one crate writes them, no foreign keys
    into them from other features) without breaking those tools.
14. **Naming.** "Workflows" already names AI-agent workflows
    (`.temps/workflows/*.yaml`, `/projects/{id}/workflows/dry-run`,
    `workflow_memory`) and the deployment pipeline's
    `workflow_execution_service`. The durable workflows API lives under
    `/projects/{project_id}/environments/{environment_id}/workflows`, which does
    not collide; the console naming is an open question.
15. **Several control-plane processes.** The stateless control plane exists, and
    `LISTEN/NOTIFY` through a `sqlx::PgListener` is the established pattern
    (settings cache, route table). `SKIP LOCKED` claims make the scheduler safe
    across processes.
16. **Audit.** `AuditOperation::user_id` is optional, so writes made with the
    workflow credential are audited with the credential's id and no user.

## Alternatives

- **Embed Temporal.** Rejected: a separate server with its own persistence,
  history service and matching service is too heavy for a 4 GB box.
- **Embed Restate.** Rejected: its own storage engine beside Postgres, and its
  licence.
- **Managed Temporal or Restate templates.** Accepted as a complement (M3) for
  teams that already standardised on them.
- **Polling workers** (Temporal-style long-poll from the app). Deferred: push
  needs no worker process and fits scale-to-zero.
- **Scheduler in node agents.** Rejected: agents stay free of workflow logic,
  and one central scheduler is simpler to fence.
- **A container per step.** Deferred: start-up cost per step, and it is the
  container-orchestration problem the workload plane already solves.
- **Deterministic replay of the whole function** (Temporal's model). Rejected:
  journaling step results tolerates ordinary code between steps and needs no
  sandbox.
- **Telemetry only** (trace long jobs, no engine). Rejected: it explains a
  failure but resumes nothing.
- **A DBOS-style library writing to the app's own database.** Rejected: no
  central visibility, alerts or cross-replica scheduling, and every app needs a
  database.
- **Managed database as the only store.** Opt-in only (M3).
- **All state in each project's database.** Rejected: the scheduler would query N
  databases.
- **Redis or another separate store.** Rejected: one more stateful service to
  run, back up and size.
- **`temps-queue` durable jobs.** Rejected as the engine: they carry internal
  commands with no journal, waits or leases visible to users.

## Consequences

- **Load.** Per step, one HTTPS round trip from the app and one indexed write
  (insert or idempotent no-op) plus a lease check on the run row. Per
  invocation, one claim batch query, one HTTP request to a replica and one
  release. The scheduler's idle cost is one indexed query per poll interval
  (5 s) per process. Nothing touches the proxy hot path.
- **Memory.** Bounded by the invoker pool: at most `pool_size` invocations in
  flight per process (default 8), each holding one journal page (bounded by
  the per-run step quota and the inline cap). The manifest cache holds one
  small manifest per environment with workflows, evicted when its deployment
  changes. Metrics are aggregated in memory under bounded labels and flushed
  every 30 s.
- **Saturation.** When the pool is full the scheduler claims nothing more; due
  runs wait in `queued`, `due_at` unchanged, and queue depth and age grow (both
  metrics, both alarm templates). Over a concurrency limit, runs wait. Over a
  quota, a new start is rejected with a typed error; running runs continue.
  Nothing is dropped.
- **Database growth.** Journal rows and encrypted payloads grow with use;
  retention deletes finished runs in batches, per environment, and the journal
  bytes per project is a metric.
- **Security.** Payloads are always encrypted with associated data; reveal is
  permissioned and audited; invocation tokens are scoped to one run and one
  lease.
- **Operational.** One more background loop per control-plane process; it is
  inert when no environment has workflow runs.

## Rollout

- **M1 (this ADR):** the core engine, the TypeScript SDK, permissions,
  `ExecutorEndpoints`, metrics and alarm templates; then, in follow-up PRs, the
  console UI, the CLI, the assistant and MCP operations, `init` and the worker
  preset.
- **M2:** "All workflows", `dev`, Python and Go SDKs, child workflows, cron
  schedules, deploy warnings, the SDK codec.
- **M3:** grants, drain, the Rust SDK, per-project keys, the managed journal,
  deployment runs, engine templates.

## Open questions

- Should workers reach the control plane on an internal address rather than the
  external URL (item 3)? How should the journal-write retry window relate to
  the lease length? Do tunnels fit `dev` (item 10)? Per-project keys.
- Defaults for the assistant's `start_run`; identity checks on approvals;
  default quotas; payload reveal for developers outside production; grant
  granularity.
- Codec key rotation; the default host service; deadline and heartbeat
  defaults; retention defaults; starts on stopped environments.
- Should the app-facing subset also accept the deployment token through a new
  deployment-token permission, instead of only the workflow credential?
- The console name, given "workflows" already names AI-agent workflows
  (item 14).
- Cloud pricing; the Restate licence.
