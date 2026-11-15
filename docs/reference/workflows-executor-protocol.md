<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# Workflows executor protocol, version 1

The wire contract between Temps (the orchestrator) and a workflow SDK running
inside a deployment (the executor). Decision record:
[ADR-106](../adr/106-durable-workflows.md). Rust types:
`crates/temps-workflows/src/protocol.rs`. Reference SDK:
`sdks/node/packages/workflows`.

Status: **version 1, stable within the version.** Fields are additive: an
executor ignores fields it does not know, and Temps ignores unknown fields in
what an executor sends. Anything else (a renamed or removed field, a changed
meaning, a new required field) is version 2.

## Roles

- **Temps** stores runs and their journals, decides when a run is due, leases
  it to one invocation, and sends that invocation to one replica of the
  environment's current deployment, on an internal address (the container on
  the Docker network, or a worker node's data address). Never a public URL.
- **The executor** is an HTTP handler in the user's app. It replays the
  journal, runs the next step(s), records each step before continuing, and
  reports how the invocation ended. It keeps no state between invocations.

No connection stays open while a run sleeps or waits.

## Versioning

Every request either way carries the header
`temps-workflows-protocol: 1`. The manifest states the version the executor
speaks (`protocol_version`). Temps refuses a manifest of another version, and
the run is not invoked: the failure names both versions.

## Authentication

### Invocation token (Temps → executor, and the executor's callbacks)

Every request Temps sends carries `Authorization: Bearer <token>`:

```
twi1.<base64url(claims JSON)>.<base64url(Ed25519 signature)>
```

The signature covers the ASCII bytes `twi1.<base64url(claims JSON)>`.
base64url has no padding. Claims:

| Claim | Type | Meaning |
|---|---|---|
| `v` | int | Claims version, `1`. |
| `typ` | `"invoke"` \| `"manifest"` | What the token authorizes. |
| `pid`, `eid` | int | Project and environment. |
| `run` | UUID | The run (`invoke` only). |
| `lease` | int | The fencing token of this invocation (`invoke` only). |
| `exp` | int | Expiry, Unix seconds: the invocation deadline plus 60 s. |
| `body` | hex | SHA-256 of the exact request body (`invoke` only). |

The executor **must** reject a request, before running any user code, when:

1. the token does not verify against Temps' public key;
2. `pid` and `eid` are not the executor's own project and environment (the
   key is the instance's, so a token Temps signed for another environment is
   validly signed: its executor could forward it);
3. `exp` is in the past;
4. `typ` does not match the endpoint;
5. for `invoke`, `body` is not the SHA-256 of the received body, or `run` and
   `lease` differ from the body's `run_id` and `lease`.

The executor reads the public key, and the project and environment its
tokens must name, with the environment's [workflow
credential](#workflow-credential-the-app--temps) at
`GET {TEMPS_API_URL}/workflows/v1/invocation-key` (`401` without a valid
one):

```json
{
  "algorithm": "ed25519",
  "public_key": "<base64url, 32 bytes>",
  "key_id": "<16 hex>",
  "project_id": 12,
  "environment_id": 34
}
```

The executor verifies the token, including `pid` and `eid`, before it reads
the request body.

An executor may cache the key and refetch it when a token fails to verify
with the cached one (`key_id` changes when the key does). The key is derived
from the instance's master key, so every control-plane process signs alike.

The executor sends the token it was invoked with on its callbacks for that
run. Temps accepts it for that run and that lease only.

### Workflow credential (the app → Temps)

The app-facing endpoints (start a run, read a run, send an event) take
`Authorization: Bearer twf_...`, a credential an operator issues per
environment (`POST .../workflows/credential`) and puts in the app as
`TEMPS_WORKFLOWS_TOKEN`. Temps stores only its SHA-256.

## Endpoints the executor serves

Both are relative to the replica's base URL.

### `GET /.well-known/temps/workflows/manifest`

Token `typ: "manifest"`. Response (at most 1 MiB, within 10 s):

```json
{
  "protocol_version": 1,
  "sdk": "@temps-sdk/workflows/0.1.0",
  "workflows": [
    { "name": "send-invoice", "version": "3", "concurrency": 5 }
  ]
}
```

`concurrency` (1 to 1,000,000; a manifest outside that range is refused)
caps the runs of that workflow executing at once in the environment; the environment's `max_concurrent_runs` caps them all. Temps
caches the manifest per environment and deployment (10 minutes, 30 s after a
failure) and drops it when a deployment changes or an invocation cannot
connect.

### `POST /.well-known/temps/workflows/invoke`

Token `typ: "invoke"`. Body:

```json
{
  "protocol_version": 1,
  "run_id": "0d6c...",
  "workflow": "send-invoice",
  "attempt": 1,
  "lease": 4,
  "now": "2026-11-13T10:00:00Z",
  "deadline_at": "2026-11-13T10:15:00Z",
  "lease_expires_at": "2026-11-13T10:02:00Z",
  "input": { "order": 1 },
  "journal": [
    { "sequence": 0, "name": "charge", "kind": "step", "status": "completed",
      "attempts": 1, "result": { "charge": "ch_1" }, "error": null, "wake_at": null }
  ],
  "journal_next_sequence": null
}
```

- `now` is Temps' clock. Replays must not depend on the replica's.
- `journal` is in sequence order from 0, decrypted. When it does not fit in
  one page (1000 entries or 8 MiB), `journal_next_sequence` is set. Fetch the
  rest with `GET {TEMPS_API_URL}/workflow-runs/{run_id}/journal?from=<n>`,
  repeating while the page's `next_sequence` is set.
- The executor must finish by `deadline_at`, and heartbeat before
  `lease_expires_at` if it runs that long. It should stop starting new steps
  shortly before the deadline (the TypeScript SDK: 30 s, or a quarter of the
  window when that is shorter) and report `retry` from the next step, so the
  run continues in a fresh invocation.

The executor answers in one of two ways:

- **`200`** with an [outcome](#outcomes) body `{"outcome": {...}}`, at most
  twice the environment's `max_payload_bytes` plus 64 KiB; or
- **`202`** with any body, then reports the outcome later with
  `POST /workflow-runs/{run_id}/outcome`, before the deadline.

Any other status, a timeout, a body over the limit or a body that does not
parse counts as a failed invocation (see [Failures](#failures)). An executor
that cannot reach Temps during the invocation (its journal read or a step
write fails with a network error or a `5xx`) answers `503`: the run is
retried with backoff rather than reported failed. A `404` is
reported as "is the SDK handler mounted?".

## Endpoints Temps serves for the executor

Relative to `TEMPS_API_URL` (the `/api` root the app already uses for
telemetry). All take the invocation token, and all except the journal read
and heartbeat require the token's lease to be the run's current lease while
the run is `running`. Errors are RFC 7807 problem documents. A request
with a stale lease gets `409` with title `Stale Lease`, and the executor
must stop the invocation: it answers the invoke request `202` and
reports no outcome, since the run now belongs to another lease; an invalid or
expired token gets `401` with title `Invalid Invocation Token`.

### `GET /workflow-runs/{run_id}/journal?from=<n>`

Returns `{ "entries": [JournalEntry...], "next_sequence": <n> | null }`.

### `POST /workflow-runs/{run_id}/steps`

Records one journal entry **before** the workflow continues past it.

```json
{
  "sequence": 1, "name": "email", "kind": "step", "status": "completed",
  "attempts": 1, "result": { "sent": true }, "error": null, "wake_at": null,
  "duration_ms": 412, "trace_id": "4bf9...", "error_ref": null, "event": null
}
```

- `sequence` must be the next free sequence, or an existing entry's.
  Re-recording an existing entry with another `name` or `kind` is
  `409 incompatible`.
- An entry is final once `completed` or `failed`. Writing a final entry
  again returns the recorded one with `already_recorded: true`, and the
  executor must continue with **that** value, not its own. This is what
  makes a step's result exactly-once while its body is at-least-once.
- A `pending` entry may be rewritten by a later attempt (a failed attempt
  that will be retried, a `sleep` or `wait` that has not resolved).
- Rules by `kind`:

| `kind` | Recorded as | Notes |
|---|---|---|
| `step` | `pending` (attempt failed, will retry), `completed`, `failed` | `error` holds the message. |
| `sleep` | `pending` with `wake_at`, then `completed` | The executor completes it on the first invocation whose `now` is at or past `wake_at`. An operator's resume moves `wake_at` to now. |
| `wait` | `pending` with `wake_at` = the timeout (optional), then `completed` | Temps completes it with `{"received": true, "payload": ...}` when the event arrives, while the run is still suspended on it. One event completes at most one wait of a run, and only an event sent after the run started. A result that would exceed `max_journal_bytes_per_run` fails the run (`error_kind` `quota_exceeded`). At the timeout the run is invoked with the entry still `pending`, and the executor records `{"received": false, "payload": null}`. |
| `now` | `completed` with the timestamp | Never `pending`. |
| `random` | `completed` with the value | Never `pending`. |
| `send_event` | `completed` with `event` set | Never `pending`. Only this kind carries `event`; Temps delivers the event once, when the entry is first recorded. Event names starting with `temps.` are reserved and rejected. |
| `child` | `pending` with `child` = `{"workflow": ..., "input": ...}` and no `result`; Temps completes it | Temps starts a run of `workflow` in the same environment, exactly once per entry (a rewrite finds the child it started; truncating this entry for an operator retry creates a new child), and records `{"run_id": ...}` as the entry's result. When the child finishes Temps completes the entry with `{"run_id", "status", "output", "error"}`, where `status` is `completed`, `failed`, `cancelled` or `deleted` (removed before it finished). The executor never records a child entry final. Only this kind carries `child`. |

- `trace_id` (≤ 64 characters) and `error_ref` (≤ 128) are metadata linking
  to the attempt's span and error-tracking event.
- Limits per run: `max_steps_per_run` entries and `max_journal_bytes_per_run`
  bytes (`429` with the quota named), `max_payload_bytes` per value (`413`).
  A rewritten `pending` entry is charged for its latest result and error
  only.
- Event names are 1 to 200 characters of `[A-Za-z0-9._:/-]`; keys 1 to 256
  characters without control characters.
- Request bodies carrying payloads (this route, the outcome, starting a run)
  are read up to the largest `max_payload_bytes` an environment may set
  plus room for an error, an event and the envelope, and only after the
  token is checked.

Response:

```json
{ "entry": { ...JournalEntry }, "already_recorded": false, "lease_expires_at": "..." }
```

### `POST /workflow-runs/{run_id}/heartbeat`

Body `{}`. Extends the lease by the environment's `lease_secs` and returns
`{ "lease_expires_at": "...", "cancelled": false }`. When an operator has
cancelled the run, `cancelled` is `true`: stop at the next step boundary.

### `POST /workflow-runs/{run_id}/outcome`

The callback form of a `200` invoke reply, for an executor that answered
`202`. Body `{"outcome": {...}}`; response `{ "status": "<run status>" }`.

## Outcomes

Tagged by `type`:

| `type` | Fields | Effect |
|---|---|---|
| `completed` | `output` | The run completes. An output over `max_payload_bytes` fails the run with `payload_too_large`. |
| `failed` | `error`, `sequence`? | The run fails, keeping its journal. With `sequence`, the step at that sequence is named as the cause. |
| `sleep` | `sequence`, `until` | The run sleeps until `until`. |
| `wait` | `sequence`, `event`, `key`?, `timeout_at`? | The run waits for the event (matching `key` when set). An event sent before the run suspended, and not yet consumed, wakes it at once. With `event` = `temps.child` and `key` = the child's run id, the run waits for the child its pending `child` entry at `sequence` started, with no timeout; a child that already finished wakes it at once. Other `temps.` names are rejected. |
| `retry` | `sequence`, `at` | A step attempt failed; the run is retried at `at` (clamped to now). The step's attempts and backoff are the SDK's policy. |
| `incompatible` | `sequence`, `expected`, `found` | Replay found a different step than the journal recorded. The run fails with `WorkflowIncompatible`, keeping its journal so an operator can retry from a step or cancel. |

Every outcome releases the lease. The `sequence` of `sleep` and `wait` must
name the `pending` entry recorded for it (a `child` entry for a wait on
`temps.child`).

Cancelling a run cancels its unfinished descendants. A cancelled or failed
child wakes its parent like a completed one, and the parent decides what to
do with the `status`.

## Failures

A failed invocation (cannot connect, timeout, a status other than `200` and
`202`, a malformed or oversize reply, a lease that expires without an
outcome) is an infrastructure failure, distinct from a step failing (which
the executor reports as `retry` or `failed`). The run is retried with
exponential backoff (`min(300, 5 × 2^min(n−1, 6))` seconds after the
n-th consecutive failure). After `stuck_after_failures` consecutive failures
(default 8) the run is `stuck` until an operator resumes it.

A run whose workflow is at its `concurrency` limit is given back without
spending an attempt and is due again 2 s later, with the reason on the run.

When nothing can be invoked (no deployment, no healthy replica, a Compose
deployment with no `host_service` chosen), the run stays `queued`, is
re-checked every 30 s, spends no attempt, and the reason is shown on the run
and in the environment's workflow overview.

## Payloads

Inputs, outputs, step results, errors and event payloads are JSON. Temps
encrypts them at rest (AES-256-GCM, bound to the project, environment, run
and field), keeps those up to `inline_payload_limit_bytes` in the row and
offloads larger ones (up to `max_payload_bytes`) to blob storage. The
executor always sends and receives plaintext JSON over the internal network,
authenticated as above. Temps never writes a payload to its logs.

## Conformance

An executor conforms to version 1 when it passes the conformance suite in
`sdks/node/packages/workflows` (replay, retries, suspend and resume,
incompatibility, stale-lease rejection, idempotent start), which runs against
both a fake orchestrator and the real engine.

## Optional application payload encryption

The TypeScript SDK accepts the same `codec` on `WorkflowsClient` and
`createWorkflowHandler`. `aesGcmCodec({ activeKeyId, keys })` uses 32-byte
application-held keys and AES-256-GCM; keys never go to Temps. Its versioned
JSON envelope carries `codec`, `key_id`, `nonce`, `data`, and `tag`. Keep old keys
in the key ring while retained runs or events can still reference them. A
missing key or failed authentication rejects the invocation before user code
runs; it is retryable, never silently treated as plaintext.

Inputs, outputs, ordinary step results, deterministic clock/random results,
event payloads and child inputs are encoded on write. The executor decodes
journal pages, event payloads inside wait results, and child output/error
fields while preserving their orchestration metadata. Error strings use the
`temps-codec-v1:` prefix followed by an encoded JSON envelope; messages are
bounded before encryption so server limits cannot truncate ciphertext. Server
errors remain readable metadata. The app-held codec also redacts application
error messages and stacks from the SDK's tracing and error-tracking hooks.
Application code remains responsible for what it logs independently.

Every participant sending payloads to a codec-enabled workflow must use the
same codec, including event publishers and parent/child workflows. Management
API inputs and cron inputs are not automatically encoded by Temps: those
callers must supply an encoded envelope. Layer-1 encryption and quota checks
still apply to the encoded values; envelope overhead counts toward limits.

## Cross-environment grants

Grants belong to the destination environment. Its operator creates an exact
(source environment, operation, resource name) grant under
`/projects/{project_id}/environments/{environment_id}/workflows/grants`.
Operations are `start` and `event`; there are no wildcard or payload-read grants.
The destination project must own the destination environment.

A source app uses its existing workflow credential with
`POST /workflows/v1/projects/{project_id}/environments/{environment_id}/runs`
or `.../events`. Authorization is checked for every request. Revocation rejects
new calls; requests already authorized and runs already started may finish.
Reading a destination run still requires a credential or management permission
for the destination.

The TypeScript client's start/send options accept
`target: { projectId, environmentId }`; Python accepts
`target=(project_id, environment_id)`; Go has `StartIn` and `SendEventTo`.
Rust uses an optional `Target`. The ordinary read methods remain scoped to the
credential's own environment.

## Project encryption keys

New payloads use a random project key, wrapped by the instance master key.
The stored ciphertext prefix `pk1:<key UUID>:` selects that key; associated
data still binds project, environment, run and field. The wrapped key is read
on use, without a process cache. Older unprefixed ciphertext remains readable
with the instance key.

The project key list returns only key IDs and creation/shredding timestamps.
Shredding requires both workflow-write and run-delete permission, the exact
key ID in `confirm_key_id`, and creates an audit record. It replaces the wrapped
key with a tombstone. Subsequent decryption returns 410; operations that already
loaded the key may finish. New writes create a new project key. Shredding does
not erase legacy instance-key payloads, and a backup containing the wrapped key
can restore access. Include key-bearing backups in an erasure policy.

## Deployment drain

`drain_timeout_secs` is an environment setting: zero (the default) disables
drain, and 1–3600 enables it. Before promotion, Temps pauses new claims for the
environment and waits for currently running invocations to finish. Sleeping,
waiting and queued runs are retained and replay on the new deployment.

The pause is a fenced, renewable 90-second database lease. A cancelled or
crashed deployment cannot leave claims paused indefinitely; an old deployment
cannot release a newer deployment's pause. A timeout or lost lease fails
promotion and leaves the previous deployment active. The console exposes this
under the environment's Durable workflows settings.

## Local development

`temps workflows dev --project-id <id> --environment-id <id> --port 3000`
connects an already-running local SDK server to an **undeployed, unprotected
preview environment**. Configure that SDK with the instance's API URL and the
preview environment's workflow credential. The CLI itself uses the normal
management login and requires workflow-write plus payload-read permission.

This is an outbound polling tunnel: no public local port, inbound firewall rule,
or forwarding URL is needed. The CLI forwards only the manifest and invocation
protocol paths to `127.0.0.1`. It sends the signed invocation token, never its
management credential, to the local server. Executor callbacks use the ordinary
workflow API. Start runs and send events from the console or an SDK as usual.
Restart the CLI after changing the workflow manifest.

Requests and responses are encrypted with instance-key associated data while
queued centrally, so different control-plane processes can serve polling and
invocation. A session expires after 60 seconds without a successful poll.
Disconnect removes its pending messages; crashed sessions are removed by the
lease reaper. There are at most 16 sessions, four requests per session, and
8 MiB per tunnel message. Larger development payloads must be reduced or run
on a deployed executor. Local execution retains the normal at-least-once step
contract: disconnection may cause the engine to retry an invocation.

## SDK integration points

| Language | Source package | HTTP integration |
|---|---|---|
| TypeScript | `sdks/node/packages/workflows` | Fetch handler and Node adapters |
| Python | `sdks/python/workflows` | `WorkflowApp` ASGI application |
| Go | `sdks/go/workflows` | `NewHandler` / `Handler` implementing `http.Handler` |
| Rust | `crates/temps-workflows-sdk` | `Executor::router()` mounted in Axum |

Python workflows are async functions; synchronous blocking work must be moved
off the event loop. Go step functions must honor their context cancellation.
Rust step futures are cancelled on timeout. All step bodies must be idempotent.
The Go and Rust executors pin the environment-bound invocation key at startup;
recreate the handler after instance signing-key rotation.

## Deployment-triggered runs

A source deployment can declare up to 16 distinct workflow names:

```yaml
deploymentWorkflows:
  - after-deploy
  - warm-cache
```

Temps adds a persisted **Start Deployment Workflows** job after successful
promotion. It starts each named workflow with
`{ "deployment_id": ..., "project_id": ..., "environment_id": ... }` as input.
The idempotency key includes the deployment ID and workflow name, so retrying
the job after a lost response returns the earlier run. A new deployment gets
new runs. Invalid names and duplicates are rejected before any start.

The deployment remains successful if this optional job fails; its job log
identifies the failure and can be retried. Workflows run independently and
appear in All workflows. A job that begins after its deployment has already
been superseded skips its starts. Once started, runs follow the ordinary
replay-on-current-deployment contract; they are not pinned to an old image.
Image-only deployments without a source tree do not read this configuration.

## Managed PostgreSQL journal

Workflow settings can select `managed_store_service_id`, a PostgreSQL service
linked to the project. The default is the control-plane database. This selection
applies to new runs; each run pins its journal service until deletion. Keep that
service linked, available and backed up alongside the control-plane database.
The settings screen lists linked PostgreSQL services and checks connectivity
before saving the choice.

Scheduling, leases, quotas and a metadata index remain central. Encrypted
results and errors are stored in the managed database's plain
`temps_workflow_journal_v1` table. Under the central run lock, Temps commits an
immutable revision remotely before committing the central index and run state.
Repeating the same step attempt recovers the first remote result, including when
the central transaction never committed. Payload pages are bounded before remote
reads. Managed operations share four connection slots and a 15-second deadline;
an unavailable journal returns a retryable 503 instead of re-executing a recorded
step without its result.

A revision retains the currently indexed predecessor while reclaiming older
attempts of that step. Run deletion queues durable remote cleanup, retried by
retention even after the project is removed. A service cannot be deleted while
runs or cleanup records still reference it. After an uncertain write, uploaded
blobs are retained until run cleanup because the remote entry may reference them.
Project-key shredding also makes encrypted payloads in managed journals
unreadable; restore and backup limitations are the same as for the default store.

## Separate Temporal and Restate engines

The project template catalog includes **Temporal Workflow Engine** and
**Restate Workflow Engine**. These deploy the upstream engines as Compose
applications; they do not share Temps workflow credentials, journals, grants,
or SDKs. Their images are pinned by digest and the source template includes
schema initialization, health checks and explicit CPU/memory limits. Running
services have aggregate limits of about 1.2 GiB for Temporal or 832 MiB for
Restate, in addition to Temps and application workers. Allocate capacity for
that extra engine before deploying it.

The template generates `ENGINE_PASSWORD` and, for Temporal,
`DATABASE_PASSWORD` as secret project variables. The console username defaults
to `admin`. Retrieve the password through the normal authorized project-secret
flow, use the Temps HTTPS URL, and rotate it in project variables. Only the
Nginx gateway is publicly routed. Its health probe is unauthenticated; other
requests require Basic Auth. Restate's second gateway route (`gateway-8081`)
is its authenticated ingress endpoint. Temporal's gRPC endpoint stays private
at `temporal:7233`; add workers to the same Compose application/network.
Do not expose its unauthenticated gRPC listener as a public port.

Named volumes hold Restate's data and Temporal's PostgreSQL data. Keep the
same project/environment and preserve the database password on redeploy.
Back up these volumes using the upstream engine's supported backup procedure;
Temps control-plane backups alone do not contain their history. Deleting a
volume destroys that history. The integration tests verify authentication,
restart persistence and repeatable Temporal schema initialization, and the
Compose translator test verifies compatibility with the default capability
profile. Neither template requests host networking, host bind mounts or extra
container capabilities.

## CLI and assistant operations

`temps workflows` exposes `list`, `runs`, `get`, `diagnose`, `watch`, `start`,
`cancel`, `retry`, `resume`, `delete`, `send-event`, `init`, and `dev`.
Management commands take explicit `--project-id` and `--environment-id`.
`runs` is paginated (20 by default, at most 100); `watch` has a bounded deadline
and emits JSON lines. Starts require `--idempotency-key`; cancellation, retry,
resume and deletion require `--yes`. `get --reveal input|output|error|step_result|step_error`
requires the API's payload permission and audits the read; step reveals also
require `--sequence`. Returned payloads are untrusted application data.

The assistant and MCP virtual CLI discover the management API under Workflows.
Reads and `diagnose_workflow_run` do not decrypt payloads. Starts, events,
cancellation, retry, resume, deletion and explicit reveal use the shared
propose/confirm flow. Starts require a stable, nonempty idempotency key even
though the general HTTP API permits one to be omitted. Configuration,
credentials, grants and key shredding remain operator-console actions.

`temps workflows init <new-directory>` writes a Node worker with a durable
step and sleep, a health endpoint, an SDK-authenticated invocation endpoint,
a non-root Dockerfile and a test. It never overwrites an existing directory.
Select **Durable workflow worker** when creating the service; this persists
as the ordinary Dockerfile preset, with port 3000. Configure `TEMPS_API_URL`
and the environment's `TEMPS_WORKFLOWS_TOKEN` as runtime values before starting
the worker. Do not bake the token into the image or source tree.

Before the SDK is published, build and pack `sdks/node/packages/workflows`,
then use `init --sdk-package <npm-pack-tarball>`. The tarball is copied into
the new project and referenced locally, so development does not require a
registry release. Without that option, the scaffold requires the published
`@temps-sdk/workflows@0.1.0` package.


### Public ingress protection

The main Temps proxy and edge proxy return 404 for the reserved
`/.well-known/temps/workflows` namespace before hostname routing, wake-up,
static serving or cache access. The guard also covers encoded paths,
normalized separators and mount prefixes. There is no header, forwarded IP,
workflow credential or signed-in-user exemption on public ingress.

The orchestrator invokes deployed replicas directly, or uses the authenticated
development tunnel. SDK invocation-signature, environment, body, expiry and
lease checks remain mandatory. This ingress rule does not firewall published
container ports or other routes that bypass Temps; those require network
isolation. Applications must not alias executor handlers onto public routes.

The guard performs no I/O, allocation, locking or background work on the
per-request path. Ordinary paths use a byte scan; normalization uses at most
8 KiB of stack storage and eight decoding passes. Unusual paths exceeding
those bounds are refused rather than partially normalized.

Successful replay must consume every recorded journal entry. Returning while
entries remain (including an unread continuation page) reports `incompatible`,
so removing an approval wait cannot silently complete a run. Child creation and
its journal entry commit together under the parent lease; descendant cancellation
serializes against child creation. Managed-journal replay preserves central
scheduling overrides made by an operator resume.

Deleting a run queues blob cleanup durably in the same database transaction.
Retention retries a bounded batch of pending cleanup, acknowledging each only
after blob storage confirms deletion. An unavailable store leaves work queued.
