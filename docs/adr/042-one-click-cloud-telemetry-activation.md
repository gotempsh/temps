# ADR-042: One-Click Cloud Telemetry Activation — Bulk Write-Mode Switch and Online Backfill

**Status:** Proposed
**Date:** 2026-09-01
**Author:** David Viejo
**Builds on:** ADR-040 (Cloud Telemetry Read Source), ADR-041 (Cloud-Primary Telemetry Writes)

> **Numbering note.** ADR-041 (`041-cloud-primary-telemetry-writes.md`) is the
> most recent committed ADR; 042 is the next free number in the committed
> sequence.

---

## Context

### The problem, concretely

ADR-041 gave every project a `cloud_telemetry_write_mode`. ADR-040 gave the
instance a way to ship pre-existing local history to Cloud so that flipping a
project to Cloud-primary does not leave a hole. Both work. Neither scales past
one project at a time:

- **Switching** is `PATCH /otel/cloud-telemetry/projects/{project_id}`
  (`crates/temps-otel/src/handlers/cloud_telemetry_handler.rs:358` →
  `TelemetryWriteModeService::set_write_mode`,
  `crates/temps-otel/src/services/telemetry_write_mode.rs:476`). One call, one
  project.
- **Backfilling** is `temps backfill cloud-telemetry --project <id> --from --to`
  (`crates/temps-cli/src/commands/cloud_telemetry_backfill.rs`), a separate
  binary invocation, deliberately single-project (`:63-65`: *"Required — this
  command egresses data, so it never operates on 'all projects' implicitly"*),
  and documented as requiring `temps serve` to be **stopped** (`:25-31`).

An operator with 40 projects therefore performs 40 API calls, then stops their
server, then runs 40 CLI invocations with hand-computed windows, then starts
their server. That is not an activation flow. It is a migration project.

### The product requirement

> *"Temps Cloud must be able to be activated without downtime from the UI. The
> only requirement is for the user to pay — if it takes more time then the
> server must only show the progress + ETA."*

Payment is the authorization. There is no separate "review this estimate and
click confirm" step between paying for Temps Cloud and the instance's projects
actually being on Cloud, history included. Downtime is not acceptable, and a
long-running activation must be *visible* — progress and an ETA — rather than
silent.

### Finding 1 — "requires the server stopped" is not the constraint it claims to be

The CLI's stated reason (`cloud_telemetry_backfill.rs:25-31`) is that it
*"drives the same `CloudLink` state file the running server uses for its live
mirror. Two processes writing that file will interleave submissions. **Nothing
is lost — Cloud's idempotency covers it** — but the run's own counters stop
being trustworthy."*

That is an accuracy caveat, not a correctness lock. There is **no OS-level file
lock anywhere**. The CLI constructs its own `CloudLink` over the same path
(`cloud_telemetry_backfill.rs:464-479` → `crates/temps-cloud-client/src/link.rs:230`,
`data_dir/cloud-link/state.json`), and every guard inside `CloudLink` is
in-process only: `flush_lock: tokio::sync::Mutex<()>` (`link.rs:165`),
`spool: Mutex<Spool>` (`link.rs:151`), `state: RwLock<Option<EnrollmentState>>`
(`link.rs:141`).

**Moving the backfill into the running server therefore eliminates the
documented hazard outright** — one process, one `CloudLink`, one writer to
`state.json`, trustworthy counters. The downtime requirement was an artefact of
the tool being a second process, not a property of the work.

### Finding 2 — but the naive in-process port is broken, for a different reason

The hazard that replaces it has never been observed because the two paths never
coexisted. `ship_batch`
(`crates/temps-otel/src/services/cloud_backfill.rs:549-609`) does:

```
link.record(projected);              // → the *shared* spool
loop { link.flush() }                // → drains the *entire* spool
if link.spooled() > 0 { Err(ShipmentRefused { .. }) }
```

On a live instance the mirror flusher is continuously recording Local-mode
spans into that same spool. So an in-process backfill would (a) count live
mirror traffic as its own shipped spans, and (b) **fail spuriously** on a busy
instance, because `spooled()` may never reach zero within
`MAX_FLUSHES_PER_BATCH`. This is the real engineering prerequisite, and it is
small: the backfill needs its own submission handle so its counters and its
drain-complete test observe only its own batches.

### Finding 3 — parallelism across projects is forbidden, not merely unwise

ADR-041 §3b is explicit
(`docs/adr/041-cloud-primary-telemetry-writes.md:346-352`): the next throughput
lever after drain-until-idle is *"a bounded number of concurrent in-flight
submissions — which requires confirming that Cloud's `/v1/telemetry` idempotency
and metering tolerate concurrent submissions from one instance. **Open question
for the Cloud side; do not assume it.** Sequential drain must be proven
sufficient before concurrency is added."*

So "back-fill 4 projects at once" is not a tuning knob this ADR may turn. What
makes 40 projects feel like one action is **the queue**, not parallelism.

### Finding 4 — activation already has automatic side effects

`POST /cloud/enroll` (`crates/temps-cloud/src/handler.rs:208-258`) does not just
store a credential. It already provisions the managed backup source as part of
enrollment and audits the outcome (`ManagedBackupOutcome::Provisioned`,
`cloud.backup_credential.issued`). Auto-enqueuing telemetry activation on the
same event is consistent with an established pattern, not a new one.

### Forces

- Egress costs the customer real money. The single-project CLI's `--dry-run`
  requirement exists for that reason and must not be casually deleted.
- But payment *is* consent for the activation the customer just bought. Asking
  them to re-authorize the thing they paid for is friction, not safety.
- A self-hosted operator has nobody to ask. A multi-hour activation that shows
  a spinner and no ETA is indistinguishable from a hang.
- Live outbox shipping for already-Cloud projects
  (`crates/temps-cloud-client/src/outbox_worker.rs`) is a **primary write path**
  under ADR-041. A bulk backfill must never starve it.

---

## Decision

### 1. Two entry points, one machinery

There is exactly one bulk-job engine. Two triggers create jobs on it, and they
differ **only** in whether a human confirms the estimate:

| | (a) Purchase-triggered | (b) Operator-triggered |
|---|---|---|
| Fires on | successful `POST /cloud/enroll` | explicit console/CLI action |
| Project scope | every eligible project, automatically | an explicit list, or `all` |
| Window | everything local storage holds | operator-chosen, defaulting to all |
| Estimate computed | **yes** (ETA, audit, anomaly detection) | yes |
| Estimate **gates the start** | **no** — payment is the authorization | **yes** — two-phase confirm |
| Cancellable | yes | yes |

Rationale for the asymmetry: (a) is the customer receiving what they just
bought; re-prompting is friction with no decision behind it. (b) is an existing
paid customer spending *more* money on a later, separate act (new projects,
retrying skipped ones), with no fresh payment event attached — the guardrail
still earns its place there.

### 2. The work runs inside `temps serve`, and downtime is not required

A single `CloudBulkActivationWorker` task, spawned by `OtelPlugin` alongside the
outbox worker. Being in-process is what makes the CLI's stated hazard
disappear: one `CloudLink`, one `state.json` writer (Finding 1).

The pre-existing `temps backfill cloud-telemetry` CLI **stays**, unchanged, as
the offline/recovery tool for the case where the server cannot run. Its
"stop the server first" warning remains correct *for it*, because it remains a
second process.

**Prerequisite (Finding 2):** `ship_batch` must submit through a handle scoped
to the backfill, so `spooled()`/`shipped` reflect only backfill batches. Without
this the worker is non-deterministic on a live instance and must not ship.

### 3. Concurrency model: one shipper, sequential projects, explicit yielding

- **Submission concurrency is 1, globally.** Mandated by ADR-041 §3b
  (Finding 3), not chosen for convenience. Projects are processed one at a
  time, in ascending project id, so ordering is explicable to the operator.
- **The live outbox always wins.** Between each backfill batch the worker
  releases its permit and yields, so `outbox_worker`'s drain-until-idle cycle
  interleaves. Cloud-primary live writes are a primary path; a historical
  backfill is not, and must degrade in its favour.
- **Read/projection may be pipelined** (fetch batch N+1 while N is in flight)
  since that touches only local storage and the projection allowlist. It is an
  optimisation, explicitly not required for correctness.
- **`rate_limit_spans_per_sec`** — already a parameter of
  `backfill_cloud_telemetry_window` — is set from an operator setting on the
  singleton `settings` row (per the no-env-var rule), defaulting to unthrottled.

### 4. Eligibility and skipping

A project enters a job only if `set_write_mode`'s existing Cloud-primary gates
would pass (`telemetry_write_mode.rs:484-512`). Otherwise it is recorded as
`skipped` with the machine-readable reason, and the job continues:

- `cloud_telemetry_fidelity` is not `queryable` → `skipped:
  fidelity_not_queryable`. **The job does not silently raise fidelity.** That is
  a separate decision with its own cost, and it must not happen as an invisible
  consequence of paying.
- Instance-wide conditions (`NotLinked`, `CredentialRejected`,
  `TelemetryExportDisabled`) are **not** per-project skips — see §7.

Skipped projects are listed in the UI with their reason and a direct link to the
setting that unblocks them, then retried via entry point (b).

### 5. Order of operations per project: switch first, then backfill

`set_write_mode(project, Cloud)` runs synchronously and immediately when the
project is dequeued; the backfill of its history follows. Justification:

- The switch is cheap, atomic, reversible, and egresses nothing.
- Switching first means new spans go to Cloud from that instant, while history
  arrives behind them. `project_telemetry_write_intervals` records the exact
  boundary, so reads across the seam remain correct and explicable — this is
  precisely what ADR-041's append-only ledger exists for.
- The inverse order (backfill, then switch) would keep writing new spans locally
  for the entire duration of the backfill, growing the very window the backfill
  is trying to close. It never converges on a busy project.

### 6. Estimate, ETA, and progress

`estimate_backfill` (`cloud_backfill.rs:286`) is run per project at enqueue
time. It is cheap relative to the send — an exact `count_spans_window` plus a
`ESTIMATE_SAMPLE_SIZE`-span projection — and it runs on **both** paths. On path
(a) its output is not a gate; it is the input to three things:

1. **ETA.** `remaining_spans / observed_throughput`, where throughput is an
   EWMA over acknowledged batches. Before the first batch acknowledges there is
   no measured rate, and the UI says **"estimating…"** rather than inventing a
   number. The ETA is rendered coarsely (minutes/hours) because a
   false-precision countdown that jumps is worse than an honest range.
2. **Audit.** Every project still emits the existing
   `CloudTelemetryBackfillAudit` record via `record_backfill_audit`, so the
   pre-send estimate and the post-send actual are both on the record for a
   customer disputing an invoice.
3. **Anomaly detection.** If a project's shipped bytes exceed its estimate by
   more than a bounded factor, the job pauses that project and surfaces it,
   rather than running away with the customer's money. An estimate that is
   wrong by an order of magnitude means a bug, and a bug that costs money should
   stop.

Per-project progress reuses `CloudBackfillProgressService`
(`crates/temps-otel/src/services/cloud_backfill_progress.rs`) and its existing
`cloud_telemetry_backfills` row — **not** a parallel progress surface.

### 7. Failure handling: skip-and-continue, never roll back the switch

**Per-project failures** (read error, projection failure, per-project quota,
byte-budget anomaly) mark that project `failed` with its truncated reason
(`truncate_failure_reason`, `cloud_backfill_progress.rs:61`) and the job moves
on. The cursor is persisted, so a retry resumes rather than re-ships.

**The mode switch is never rolled back on backfill failure.** Reverting a
project to `local` after some spans have already shipped Cloud-primary splits
that project's history across both stores and writes a false boundary into
`project_telemetry_write_intervals`. ADR-041 §7c already owns credential loss
via `CloudTelemetryFallback` (`link.rs:172`); that mechanism, not this job, is
the correct actor. A failed backfill leaves a *known, recorded, retryable* hole
in history — which is honest — instead of a silently bisected timeline.

**Instance-wide failures abort the whole job.** `NotLinked`,
`CredentialRejected`, and `TelemetryExportDisabled` are properties of the link,
not of project 17. Continuing would fail the remaining 23 projects identically
and bury the one real cause under 23 duplicate errors. The job transitions to
`aborted` with a single actionable reason and a resume affordance.

**Cancellation** sets `cancel_requested_at` and is honoured at the next chunk
boundary. Because the cursor is durable, cancel is clean — no partial batch, no
lost position, and resume costs nothing already paid for.

**Restart** resumes automatically, without reconfirmation. The spend was already
authorized (by payment on path (a), by the confirm step on path (b)), and the
cursor guarantees resumption does not re-ship what already shipped. Requiring a
human to re-approve after every server restart would make a long activation
effectively impossible to complete unattended — the opposite of the
requirement.

### 8. Data model

Two new tables, following `project_telemetry_write_intervals`' shape:

**`cloud_telemetry_bulk_jobs`**
`id` (uuid) · `trigger` (`purchase` | `operator`) · `requested_by_user_id`
(nullable — `purchase` has no operator) · `status` (`pending` | `running` |
`completed` | `completed_with_failures` | `aborted` | `cancelled`) ·
`estimated_spans` · `estimated_bytes` · `spans_shipped` · `bytes_shipped` ·
`plan_hash` (nullable; set only on the `operator` path) · `created_at` ·
`started_at` · `completed_at` · `cancel_requested_at` · `abort_reason`.

**`cloud_telemetry_bulk_job_projects`**
`job_id` · `project_id` · `status` (`pending` | `switching` | `backfilling` |
`done` | `failed` | `skipped`) · `skip_reason` · `window_from` · `window_to` ·
`estimated_spans` · `estimated_bytes` · `spans_shipped` · `last_error` ·
`started_at` · `completed_at`. UNIQUE `(job_id, project_id)`.

**One additive column** on `cloud_telemetry_backfills`: nullable `bulk_job_id`.
Its UNIQUE `(project_id)` constraint is preserved — a project has one live
backfill, whoever started it — and the existing per-project progress surface is
reused rather than duplicated.

At most one job may be `running` at a time; a second request while one is
running returns the in-flight job's id rather than queueing a competing one.

### 9. API surface

```
POST   /otel/cloud-telemetry/bulk-jobs/estimate     → per-project + total estimate, plan_token
POST   /otel/cloud-telemetry/bulk-jobs              → { batch_id }   (operator path; requires plan_token)
GET    /otel/cloud-telemetry/bulk-jobs/{batch_id}   → status, per-project rows, ETA
GET    /otel/cloud-telemetry/bulk-jobs/current      → the running job, or 200 + null
POST   /otel/cloud-telemetry/bulk-jobs/{batch_id}/cancel
```

Permission: `Role::PlatformAdmin`, matching the existing instance-wide check at
`cloud_telemetry_handler.rs:533`. The purchase-triggered job is created
**internally** by the enroll path — it has no public POST, because there is no
caller for it other than enrollment itself.

`plan_token` is an opaque, short-lived handle over the exact project set and
windows that were estimated. If the project set changed between estimate and
submit, the token no longer matches and the operator re-estimates. This is what
makes "you confirmed *this* bill" true rather than approximate.

Every `utoipa::path` must declare its `params(...)` for `{batch_id}` — a missing
path param silently generates `never` in the TypeScript SDK.

### 10. CLI parity

Per the repo rule, all new endpoints get parity in `apps/temps-cli`
(`@temps-sdk/cli`), never as Rust subcommands:

```
bunx @temps-sdk/cli cloud-telemetry bulk-switch --all [--from <ts>] [--to <ts>] [--yes] [--watch]
bunx @temps-sdk/cli cloud-telemetry bulk-switch --project 4 --project 9 --yes
bunx @temps-sdk/cli cloud-telemetry bulk-status [--watch]
bunx @temps-sdk/cli cloud-telemetry bulk-cancel <batch_id>
```

`bulk-switch` calls `estimate`, prints the per-project table and the total, and
requires confirmation unless `--yes`. `--watch` polls the status endpoint and
renders the same progress and ETA the UI shows. The Rust
`temps backfill cloud-telemetry` binary subcommand is untouched and remains the
offline recovery tool.

### 11. Surfaces (Feature Discoverability)

- **`web/src/components/observe/CloudTelemetryWriteStatusCard.tsx`** is the
  instance-wide home for this. It gains a persistent "Cloud telemetry
  activation" section, **always rendered**:
  - *Job running* → overall percent, spans shipped / total, **ETA**, current
    project, and a Cancel control.
  - *Job finished with skips or failures* → the list, each with its reason and a
    direct link to the setting that unblocks it, plus a Retry that starts an
    operator-path job scoped to exactly those projects.
  - *No job, Cloud linked* → a "Switch all projects to Cloud" button opening the
    estimate/confirm dialog.
  - *Cloud not linked* → the button is **visible and disabled**, stating that
    Temps Cloud is not connected, what this action would do, and linking to the
    Cloud setup page. It does not disappear.
- **`web/src/pages/settings/OtelPipelineStatusPage.tsx`** links to the running
  job.
- **`web/src/components/project/settings/CloudTelemetryBackfillCard.tsx`** shows
  when a project's backfill is part of a bulk job, so per-project state is never
  mysteriously "already running".

---

## Alternatives Considered

### Option A: Enqueue backfill spans into the existing `SpanOutbox`

Let the historical spans go through `crates/temps-cloud-client/src/outbox.rs`
and be shipped by the one existing worker.

- **Pros:** exactly one shipper by construction; ADR-041's sequential-drain
  invariant preserved for free; durability, retry, and dead-lettering all
  inherited.
- **Cons:** fatal. The outbox is bounded by an operator byte cap
  (`outbox.rs:236`), and at capacity `enqueue` **rejects the newest and records
  a gap window** (ADR-041 §3d). A 40-project historical backfill would fill that
  cap and cause the *live primary path* to fabricate gaps. Historical data
  would be manufacturing holes in live data. Separating the two priority
  classes is a larger change than the one this ADR proposes.

### Option B: Keep the CLI, add a `--all` flag

- **Pros:** almost no new code.
- **Cons:** does not satisfy the requirement at all. It still needs the server
  stopped (downtime), still needs shell access (not "from the UI"), and still
  has no progress surface a paying customer can see. It also deletes the exact
  guardrail its own source comment defends, without replacing it.

### Option C: Confirm-before-start on the purchase path too

- **Pros:** one code path; the cost guardrail is uniform.
- **Cons:** contradicts the requirement. The customer has already paid for this
  specific outcome; a modal asking them to authorize the thing they just bought
  is friction dressed as safety. The estimate is still computed and audited —
  it just does not block.

### Option D: Parallel backfill across N projects

- **Pros:** wall-clock activation time drops roughly N-fold.
- **Cons:** directly violates ADR-041 §3b, which forbids concurrent in-flight
  submissions until the Cloud side confirms `/v1/telemetry`'s idempotency and
  metering tolerate them. Concurrency is available *later*, as a single
  constant, once that question is answered — and the design here is structured
  so that is a one-line change.

### Option E: Switch all projects immediately, backfill lazily on first read

- **Pros:** activation appears instantaneous.
- **Cons:** replaces a bounded, observable job with an unbounded, invisible one.
  Cost becomes unpredictable and unattributable, and a read that triggers an
  hours-long backfill is a worse experience than a progress bar. It also makes
  "is my history there yet?" permanently unanswerable.

---

## Consequences

### Positive

- Cloud activation becomes one action with no downtime, satisfying the
  requirement literally.
- The `state.json` interleaving hazard the CLI warns about is *removed*, not
  worked around, because there is only ever one writer (Finding 1).
- Live Cloud-primary writes keep priority; a backfill can only ever slow itself.
- Every send is still estimated, audited, and attributable per project. Payment
  changes who authorizes, not whether it is recorded.
- Skipped and failed projects are visible with reasons and one-click retry,
  instead of being discovered later as missing history.
- Cancel and resume are both cheap and lossless, because the cursor was already
  designed to be.

### Negative

- Two new tables and a background worker, on an instance that already runs the
  outbox worker.
- Sequential shipping means a 40-project activation on a large history is slow.
  The ETA makes it *honest*, not *fast*. Faster requires ADR-041 §3b's open
  question to be answered by the Cloud side first.
- `ship_batch` must be modified before any of this can ship (Finding 2). That is
  a change to a path ADR-041 also depends on, and needs its own test.
- The purchase-triggered path can spend a customer's money with no human in the
  loop on this instance. That is the requirement, and the anomaly guard in §6 is
  the mitigation — but it is a real change in posture and is recorded here as
  such.

### Risks

- **Estimate skew.** `estimate_backfill` extrapolates from a 1,000-span sample.
  A project with wildly heterogeneous span sizes will produce a poor estimate,
  which means a poor ETA and a possibly spurious anomaly pause. Mitigation: the
  anomaly factor is generous and pauses rather than aborts, and the ETA is
  rendered coarsely.
- **Ledger correctness across the seam.** Switching before backfilling means
  reads must span "history still arriving locally" and "new spans in Cloud"
  simultaneously. This relies entirely on ADR-041's interval ledger being
  correct. It needs an explicit integration test, not an assumption.
- **Activation on a very large instance.** 40 projects × long retention could
  run for many hours. Resume-on-restart is what makes that survivable; if
  resume is buggy, the failure mode is re-shipping (costly) or stalling
  (silent). Both must be tested directly.
- **Enroll-path coupling.** Adding an automatic job to `POST /cloud/enroll`
  means a bug in job creation could fail an enrollment. Job creation must be
  fire-and-forget with respect to enroll's response: enrollment succeeds, and
  the job's own failure surfaces on the status card.

---

## Implementation Notes

**Affected crates:** `temps-otel` (worker, services, handlers, entities),
`temps-cloud-client` (`ship_batch`'s submission handle), `temps-cloud` (enroll
hook), `temps-entities` + migrations, `web`, `apps/temps-cli`.

**Migration needed:** yes — two new tables, one nullable column on
`cloud_telemetry_backfills`. All additive; an instance with no Cloud link is
behaviourally unchanged, which must be asserted in the same style as ADR-041 §4.

**Breaking changes:** no. `PATCH /otel/cloud-telemetry/projects/{project_id}`,
`temps backfill cloud-telemetry`, and the existing per-project progress surface
all keep their current behaviour.

**Phasing:**

1. **P0 — prerequisite.** Give `ship_batch` a backfill-scoped submission handle
   so counters and the drain-complete test ignore live mirror traffic
   (Finding 2). Test: a backfill completes correctly while the mirror flusher is
   actively recording.
2. **P1 — engine.** Tables, `CloudBulkActivationWorker`, sequential execution,
   cursor persistence, cancel, resume-on-restart. Test: kill and restart
   mid-job; assert no re-shipping and no lost position.
3. **P2 — operator path.** Estimate/confirm endpoints, `plan_token`, CLI
   parity, status card UI with ETA.
4. **P3 — purchase path.** Auto-enqueue from `POST /cloud/enroll`, alongside
   the existing managed-backup provisioning, with its own audit action
   (`cloud.telemetry_activation.started`).
5. **P4 — guards.** Byte-budget anomaly pause, skipped-project retry affordance.

**Security review:** required. This path spends customer money without a human
confirm on the purchase trigger, and `plan_token` is an authorization artefact.
`security-auditor` sign-off before P3 ships.

---

## Open Questions

1. **Anomaly factor.** What multiple of the estimate should pause a project?
   Too tight and normal skew pauses valid work; too loose and it never fires.
   Needs a number from real backfill data, not a guess.
2. **Concurrency, later.** Once ADR-041 §3b's Cloud-side question is answered,
   what is the right in-flight submission count? The design isolates it to one
   constant; the value is deferred.
3. **Rate-limit default.** Unthrottled is fastest and matches "activate now",
   but a large backfill competes with the instance's own read IO. Should the
   purchase path default to a modest throttle for the first N minutes?

---

## References

- ADR-040 — Cloud Telemetry Read Source (`040-cloud-telemetry-read-source.md`)
- ADR-041 — Cloud-Primary Telemetry Writes (`041-cloud-primary-telemetry-writes.md`),
  especially §3b (submission concurrency), §3d (gap windows), §7c (fallback)
- `crates/temps-otel/src/services/cloud_backfill.rs`
- `crates/temps-otel/src/services/cloud_backfill_progress.rs`
- `crates/temps-otel/src/services/telemetry_write_mode.rs`
- `crates/temps-cloud-client/src/link.rs`, `outbox.rs`, `outbox_worker.rs`
- `crates/temps-cli/src/commands/cloud_telemetry_backfill.rs`
- `crates/temps-cloud/src/handler.rs` (enroll path)
