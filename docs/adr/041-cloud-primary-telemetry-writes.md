# ADR-041: Cloud-Primary Telemetry Writes — Making a Local Span Store Optional

**Status:** Proposed
**Date:** 2026-09-01
**Author:** David Viejo
**Amends:** ADR-040 (Cloud Telemetry Read Source) — see "Relationship to ADR-040"

> **Numbering note.** ADR-040 (`040-cloud-telemetry-read-source.md`) is the most
> recent committed ADR and 041 is the next free number in the committed
> sequence. Two *untracked* drafts in an unrelated local checkout currently use
> the names `040-build-node-decoupling.md` and
> `041-internal-l4-service-proxy.md`; neither has ever been committed to this
> repository. Whichever of those lands second should renumber, following the
> same convention ADR-040's own numbering note established.

---

## Context

### The decision this ADR records

The product owner made an architectural call in conversation that is not
written down anywhere else, across several statements:

1. *"if we have temps cloud write/reads must go to temps cloud"* — when Cloud is
   enabled, it is the default destination for both directions, not an option
   bolted onto a local-first system.
2. *"the goal is to decrease resources needed since clickhouse is resource
   intensive"* — the motivation is footprint reduction on the instance, not
   extra retention. Retention was ADR-040's motivation; this is a different one.
3. *"writes to the cloud remit the need for local writes" → "we need to write
   directly to the cloud, same way we do when using local clickhouse" →
   "clickhouse must not be needed with temps cloud, thats the goal"* — the
   final, repeated form: local span storage must become genuinely unnecessary,
   not merely reduced.
4. *"and keep in mind some users will only have timescaledb locally"* — the
   design must explicitly cover deployments that run no ClickHouse at all.

Everything below is an attempt to deliver (2) and (3) honestly, including the
places where reading the code changed what those statements should mean.

### Three findings that reshape the premise

These came out of reading the code rather than assuming, and each one changes
the shape of the decision.

#### Finding 1 — ClickHouse is already optional, and is already not the default

There is no `require_service`-style hard dependency on a ClickHouse connection
anywhere in `temps-otel`. `OtelPlugin::register_services`
(`crates/temps-otel/src/plugin.rs`, the storage-backend-selection block around
lines 548–640) does this:

- `TimescaleDbStorage` is **always** constructed. It is the sole backend when
  ClickHouse is off, and the inner delegate when ClickHouse is on.
- `read_clickhouse_otel_config_from_env()` (same file, ~line 1219) returns
  `Some(..)` only when `TEMPS_CLICKHOUSE_URL`, `_USER` and `_PASSWORD` are all
  set and non-empty — fail-closed, partial config is treated as disabled.
- When it returns `None`, the plugin logs *"ClickHouse OTel backend disabled
  (TEMPS_CLICKHOUSE_* unset) — using TimescaleDB"* and everything works.

The repository's own `docker-compose.yml` ships **no ClickHouse service at
all**. The same is true across `temps-analytics-events`,
`temps-analytics-backend` and `temps-proxy`: all four domains treat ClickHouse
as an opt-in scale backend behind `ServerConfig::is_clickhouse_enabled()`.

So the TimescaleDB-only population the product owner flagged is not an edge
case and is not missing tracing — **it is the default install, and it already
has fully working local spans, traces, metrics and logs**, stored in the
Postgres `otel_spans` hypertable. Cloud-only OTel is therefore *not* a new
capability for them; claiming otherwise in this ADR would be false.

The consequence for the decision: taken literally, "ClickHouse must not be
needed with Temps Cloud" is already true today, and an ADR that only delivered
that would be a no-op. The instruction only becomes meaningful — and only
delivers the stated resource motivation — when generalised to its actual
intent:

> **No *local span store* is needed when a project's telemetry is Cloud-primary
> — whichever backend that store happens to be.**

For a ClickHouse install that removes a whole service. For the (larger)
TimescaleDB-only population it removes the `otel_spans` hypertable's write
amplification, index maintenance, chunk management and retention sweeps from
the same Postgres that runs the control plane on a 3 vCPU / 4 GB box. Both are
real wins; the second one is the one that reaches most installs.

#### Finding 2 — Cloud is not a superset of the local store, and cannot be

Cloud holds a *projection* of spans, and only spans. Per ADR-040 §1 and
`crates/temps-cloud-protocol/src/messages.rs`, a mirrored `SpanRecord` is either
the `Metered` form (pseudonymised ids, constant name `"span"`, no attributes) or
the `Queryable` form (real ids and names, plus an exact-match, default-deny
attribute allowlist). Metrics, logs, analytics events and error groups have no
Cloud write path whatsoever.

Meanwhile the local `OtelStorage` surface does considerably more than store
spans: facet slot columns and their `ALTER TABLE ... UPDATE` backfills
(`crates/temps-otel/src/services/facet_service.rs`), cross-project trace refs
(ADR-027), health summaries, insights, per-project storage quota, retention.

Therefore "writes go to Cloud instead of local" can only mean **the span write,
for projects that have consented at `Queryable` fidelity**. Any broader reading
is a silent feature removal, which this repository's rules forbid.

#### Finding 3 — the existing spool/flusher cannot be the primary write path as built

The starting proposal for this work was "reuse the existing spool/flusher as a
bounded transient buffer." Reading its actual bounds says that is right in
*shape* and wrong in *sizing*, by orders of magnitude. Measured from the code:

| Property | Value | Where |
|---|---|---|
| Spool capacity | **10,000 spans / 8 MiB**, whichever binds first | `spool.rs`, `DEFAULT_CAPACITY`, `DEFAULT_CAPACITY_BYTES` |
| Overflow policy | drops the **oldest** and counts it | `Spool::push` |
| Persistence | **none** — the spool is a plain in-memory `VecDeque` | `spool.rs` |
| Producer→spool handoff | bounded mpsc of **8 batches**, `try_send`, whole batches dropped when full, drained only by `flush()`/`spooled()` | `link.rs`, `INCOMING_BATCH_CAPACITY`, `CloudLink::record`, `drain_incoming` |
| Submission size | **500 spans**, one in flight at a time | `link.rs`, `BATCH_SIZE` |
| Flush cadence | one `flush()` per **15 s** tick, backing off to 300 s on failure | `flusher.rs`, `BASE_INTERVAL`, `MAX_INTERVAL` |
| Survives restart | only the single in-flight `pending_submission` (≤500 spans), rewritten wholesale into the encrypted enrollment-state file per batch | `link.rs` flush path, `state.rs` |

The load-bearing number: **500 spans per 15 s ≈ 33 spans/second steady state.**
Above that the spool backlogs permanently, then discards — and it discards the
*oldest*, which for a primary path shreds traces mid-flight and renders broken
trees rather than an honest gap.

Every one of those choices is correct for what it was built to be. `spool.rs`'s
own doc comment says so: *"Roughly a few MB of spans — enough to ride out a
short outage, small enough to be irrelevant on a 4 GB box"*, and dropping the
oldest is right *"because during an incident the newest telemetry is the useful
telemetry."* Both statements are premised on local storage remaining
authoritative. Remove that premise and they invert.

`MirrorHealth`'s operator-facing copy is premised on it too, literally:
*"Source telemetry remains in local Temps storage."* Under this pivot that
sentence becomes false, and a false status string is worse than no status.

**Conclusion: the transport work is the hard part of this ADR, not the routing
work.** Promoting the spool to a primary write path without fixing durability,
throughput and overflow semantics would be a data-loss machine with a
reassuring status page.

### The three deployment shapes this design must cover

| Shape | Population | Today | After this ADR |
|---|---|---|---|
| **A. No Cloud link** | The ~286 self-hosted installs; the default | Local spans in Timescale or ClickHouse | **Byte-for-byte unchanged.** Non-negotiable. |
| **B. ClickHouse + Cloud** | Operators who opted into CH for scale, then linked | Local-first, Cloud mirror | Per-project cutover to Cloud-primary; CH keeps serving pre-cutover history until they choose to decommission |
| **C. TimescaleDB only + Cloud** | The default shape, linked | Local-first in `otel_spans`, Cloud mirror | Same mechanism; the biggest relative win, because it unloads the control-plane Postgres |

### Local span readers that are not the Traces page

If spans stop being written locally and only the three query endpoints from
ADR-040 §5 are routed, these silently return nothing:

- `HealthComputeService` — `crates/temps-otel/src/services/health_service.rs`
  calls `query_spans` to compute health summaries.
- `CrossProjectTraceService` — `crates/temps-otel/src/services/cross_project.rs`
  calls `get_trace` per referenced project (ADR-027).
- `TraceReader` — `crates/temps-otel/src/services/trace_reader.rs`, the
  storage-agnostic read contract the AI debugging chat uses for trace tools.
- The unified Observe page — `crates/temps-observability/src/service.rs` calls
  `query_spans`.

"The AI chat quietly stopped being able to see traces" is exactly the failure
mode CLAUDE.md's *build as if the user has no one to ask for help* rule exists
to prevent. Routing must be installed where all of these inherit it.

### Forces

1. **The instance keeps working when Cloud is down.** `temps-app`'s governing
   invariant. This ADR does not repeal it; it is precise about what "working"
   covers (see Decision §7).
2. **Instances that never link must be completely unaffected.** The self-hosted
   installs are the funnel; breaking them to build the paid product is the worst
   possible trade.
3. **Every write to Cloud goes through `POST /v1/telemetry`.** Tenant
   resolution, quota reservation, metering and retry idempotency all live in
   that gateway (`temps-app` ADR 0015). There is no raw ClickHouse write surface
   and there must never be one.
4. **No new environment variables for configuration.** Per-entity state goes on
   entity rows; instance-wide operator settings go on the singleton `settings`
   row behind `ConfigService`.
5. **Consent is per project and default-deny.** `projects.cloud_telemetry_fidelity`
   defaults to `metered` and every failure path resolves to `metered`.
6. **Unconfigured features onboard rather than disappear**, and every failure
   path surfaces a specific, actionable state.
7. **Small resource footprint.** The reference deployment is 3 vCPU / 4 GB. Any
   new durable queue must be bounded in bytes, not just in rows.
8. **Never serve local rows under a Cloud label or vice versa** (ADR-040 §3).
   This pivot does not weaken that contract; it removes most of the situations
   in which the question arises.

---

## Decision

### 0. Scope and naming

Introduce a per-project **telemetry write mode**, orthogonal to (but gated on)
the fidelity tier ADR-040 Phase A already shipped:

```
TelemetryWriteMode ::= Local  (default — today's behaviour, unchanged)
                     | Cloud  (opt-in, per project)
```

This is deliberately *not* called "disable ClickHouse". ClickHouse is one of two
possible local backends and is already optional; the property being introduced
is "this project's spans are not stored on this instance at all".

**Scope is spans only.** Metrics, logs, analytics and errors keep their current
local write paths untouched, because Cloud has no write path for them
(Finding 2). Extending the mode to other signals is Phase C/D work under
ADR-040's existing phasing and requires the same fidelity/consent treatment.

### 1. State model: a per-project write mode plus an append-only interval ledger

**`projects.cloud_telemetry_write_mode`** — enum, default `local`, not
encrypted (it is not a secret), changeable at runtime via the API/UI, audit
logged like every other project write. Same placement and rationale as
`cloud_telemetry_fidelity`.

**Hard gate, enforced in the service layer, not by UI convention:** setting
`write_mode = cloud` is rejected unless *all* of

- the project's `cloud_telemetry_fidelity` is `queryable`,
- the link is `Linked` (not `AwaitingEnrollment`, not `CredentialRejected`), and
- `CloudFeatureSwitches.telemetry` is on.

At `Metered` fidelity a Cloud-primary project would store nothing readable
anywhere — real spans discarded locally, unreadable placeholders in Cloud. That
is the single worst configuration this system can be in, so it must be
structurally unreachable rather than merely discouraged. Lowering fidelity back
to `metered` while `write_mode = cloud` is likewise rejected with a message
naming the write mode as the thing to change first.

**`project_telemetry_write_intervals`** — a small append-only ledger:

```
(project_id, mode, effective_from, effective_to NULL-until-closed, reason)
```

One row per contiguous period during which a project's spans went to one place.
`reason` is an enum (`operator`, `cloud_disconnected`, `quota_exhausted`,
`credential_rejected`, `queue_overflow_spill`) so the console can explain a flip
the operator did not make.

Why a ledger rather than a single `cutover_at` timestamp: the mode genuinely can
flip more than once (operator changes their mind; Cloud is disconnected; the
plan allowance is exhausted — §7). A single timestamp models the common case and
silently mis-routes every other one. The common case is just a one-row ledger,
so this costs nothing where it does not matter and is correct where it does.

**Instance-level derived signal, read-only:** `local_span_store_required`,
computed — never stored — as *"any project is `local`, **or** any project's
ledger still has a `local` interval whose data is within local retention"*. This
is the value that actually answers the operator's question "can I stop running
ClickHouse now?", and it is a derivation rather than a switch precisely so it
cannot drift out of sync with the projects it summarises.

### 2. The write path

`OtelService::ingest_spans` already resolves a per-project
`CloudTelemetryPolicy` for the batch through `CloudPolicyCache` — one lookup per
*distinct* project, TTL-cached, never per span. Extend that policy with the
write mode and partition the batch:

- Spans whose project is `Local` → `storage.store_spans(..)` exactly as today,
  then `link.record(..)` after the local write succeeds, exactly as today.
  Ordering unchanged, retry behaviour unchanged, `Metered` projection byte
  identical.
- Spans whose project is `Cloud` → enqueued to the durable outbox (§3). **No
  local span write happens for these.**

Both partitions are handled in the same request; a batch that mixes projects is
normal and must not be a special case.

**What an OTLP 2xx means changes, and this must be stated in the API docs.**
Today it means "committed to local storage". For a Cloud-primary project it
means "committed to this instance's durable telemetry outbox". That is strictly
weaker than today and strictly stronger than fire-and-forget — which is why §3
is not optional.

**Ingestion never blocks on Cloud.** The enqueue is a local durable write; the
shipping is a background worker. A Cloud outage cannot add latency to an OTLP
request, cannot consume an ingest permit, and cannot produce a 5xx to the
customer's exporter. This is the same property `store_with_retry`'s deliberately
short 225 ms worst-case backoff exists to protect, and it is preserved.

### 3. Resilience: promote the spool to a durable, bounded outbox

Finding 3 says the current transport is unfit for this role. Five changes, each
tied to a specific measured deficiency.

#### 3a. Durability — reuse the existing Postgres outbox pattern, do not invent a file queue

The queue must survive a `temps serve` restart: upgrades, deploys, OOM kills and
crashes are ordinary events, and losing every buffered span on each one is not a
primary write path.

**Chosen: a Postgres outbox table**, modelled directly on
`crates/temps-analytics-events/src/services/ch_fanout.rs`, which already solves
exactly this shape in this codebase — `events_ch_outbox`, a claim/deliver
worker with `batch_size`, `max_attempts`, dead-lettering, a retention sweep and
a `Notify` wake-up so a fresh enqueue does not wait out a poll interval.

Reasons this beats a bespoke on-disk queue:

- Postgres is unconditionally present. `TEMPS_DATABASE_URL` is bootstrap
  configuration; every deployment shape has it, including the ClickHouse-less
  default.
- The pattern, its cursor semantics, its dead-letter behaviour and its
  operational shape already exist here and are proven in production for
  analytics fan-out.
- Transactional ack removes the "shipped but not marked" double-send window
  that a file queue has to solve with fsync discipline and a compaction story.
- A new encrypted-file durability primitive would be a new corruption surface on
  the exact code path whose entire purpose is not losing data. It would also
  need encryption at rest (the payload is real span data at `Queryable`
  fidelity); rows in the existing database inherit the deployment's existing
  posture instead.

**Be honest about what this costs and what it means.** This is a local write. It
is not the local *span store*: one narrow append-only table, no facet slot
columns, no hypertable chunks, no per-attribute indexes, no retention scan, and
rows are deleted on ack rather than retained for the retention window. The
remaining amplification is a small fraction of `otel_spans` and is bounded by
the queue cap. But the ADR must not claim "no local writes" — it delivers "no
local span store", which is the property that produces the resource win.

The existing in-memory `Spool` is **not** deleted. It stays exactly as it is for
`Local`-mode projects, where it is still a best-effort mirror and its current
sizing is still correct. Cloud-primary spans take the durable path. Two
mechanisms, two roles, neither pretending to be the other.

#### 3b. Throughput — drain until idle, not one batch per tick

The precedent already exists in this repository:
`crates/temps-otel/src/services/cloud_backfill.rs` loops `link.flush()` until
`FlushOutcome::Idle` rather than waiting out an interval, which is how the
backfill achieves usable throughput over the same transport. The Cloud-primary
worker does the same: drain until idle or until a failure, then back off on the
existing curve.

That alone lifts the ceiling from ~33 spans/s to roughly 500 spans per
round-trip. If that is still short of the target load, the next lever is a
bounded number of concurrent in-flight submissions — which requires confirming
that Cloud's `/v1/telemetry` idempotency and metering tolerate concurrent
submissions from one instance. **Open question for the Cloud side; do not assume
it.** Sequential drain must be proven sufficient before concurrency is added.

#### 3c. Remove the lossy producer handoff

`INCOMING_BATCH_CAPACITY = 8` with `try_send` drops **whole batches** when the
channel is full, and the channel is only drained by `flush()` or a status read.
For a mirror that is an acceptable degradation; for a primary path it is
unaccounted loss at the very first hop. The Cloud-primary enqueue writes to the
outbox on the ingest path (bounded by the existing ingest permit) rather than
through a channel that can silently discard.

#### 3d. Reverse the overflow policy for Cloud-primary, and record the gap

Dropping the *oldest* is right for a liveness mirror. For a primary path it
produces the worst possible artefact: partial traces, where some spans of a
trace shipped and others were discarded, rendering as a broken tree that looks
like an instrumentation bug rather than an outage.

For the Cloud-primary queue, at the cap, **reject the newest at the boundary and
record a contiguous gap window** `(project_id, from, to, dropped_spans,
reason)`. A visible, bounded hole with a start and an end is honest and
diagnosable. A shredded history is neither.

The cap itself is an operator setting on the singleton `settings` row (per the
no-env-var rule), expressed in **bytes** and not only rows, with a default sized
so that a full queue is a fraction of a 4 GB box's disk and cannot fill it. It
must be surfaced next to the queue depth, not buried.

#### 3e. Status must stop asserting local authority

`MirrorHealth`'s messages say *"Source telemetry remains in local Temps
storage"*. For Cloud-primary projects that is false. The status type becomes
mode-aware and its Cloud-primary rendering says what is actually true: how many
spans are queued, how old the oldest unshipped span is, whether any gap windows
exist and when.

This is where `temps-app`'s failure-policy boolean — *is telemetry shipping* —
stops being informational and becomes load-bearing, because it is now the only
signal distinguishing "captured" from "gone".

### 4. Deployment shape A — no Cloud link: nothing changes

`write_mode` defaults to `local`. With no project in `Cloud` mode the partition
in §2 has an empty Cloud side, the outbox worker is never spawned, no new query
runs on the ingest path, and no new table is written. The migration is two
additive, defaulted columns plus one empty table.

**Acceptance test, in the same style as Phase A's `Metered` payload test:** an
instance with no Cloud link produces a behaviourally identical span-ingest path
before and after this change, asserted rather than assumed.

### 5. Deployment shape B — ClickHouse present, Cloud enabled

**The write path switches per project**, not per instance. An operator can flip
one project, watch it, and flip the rest — which matters because this is a
one-way door for the window it covers.

**What happens to the ClickHouse they already have.** Nothing, automatically. It
keeps serving reads of everything written before the cutover, which is real data
the operator paid to collect. Concretely:

- The instance never deletes local span data as part of a mode change.
- The console surfaces `local_span_store_required` (§1) on the Cloud settings
  page with the specific reason it is still true: *"3 projects still write spans
  to this instance"*, or *"all projects are Cloud-primary, but local history
  from before 12 Aug is still within your 30-day local retention"*.
- **Only when that derived signal goes false** does the console say the local
  span store can be decommissioned — with the explicit warning that
  decommissioning destroys the local historical copy permanently, and a direct
  pointer to the migration tool.

**Reconciliation with what already shipped — no new tooling is needed here:**

- `temps backfill cloud-telemetry` (ADR-040 Phase A,
  `crates/temps-otel/src/services/cloud_backfill.rs`) is exactly the migration
  path for pre-cutover local history. It is already cursor-based, resumable,
  `--dry-run`-capable, refuses at `metered` fidelity, and — importantly for this
  ADR — already reads from **both** local sources (`CloudBackfillSource` has a
  Postgres `otel_spans` arm and a ClickHouse `spans` arm). It needs **no
  functional change**; it needs to be *surfaced* at the decommission decision
  point, where an operator would otherwise not think to look for it. The
  existing `CloudTelemetryBackfillCard` in the project settings UI is where that
  progress already shows.
- `temps-app` ADR 0019's on-demand per-project telemetry deletion is the reverse
  valve and is unaffected by this pivot. It remains the only mechanism that can
  actually retract data from Cloud, and this ADR does not duplicate or replace
  it.
- **Do not build an automatic "cut over and migrate" button.** Backfill is a
  paid, one-way, potentially large egress. It stays an explicit, out-of-process,
  operator-driven command with a `--dry-run` that reports rows and estimated
  metered bytes first, exactly as ADR-040 §1 specified and for the same reason.

### 6. Deployment shape C — TimescaleDB only, Cloud enabled

Mechanically identical to shape B. The differences are in framing and in what
must *not* be claimed:

- This is **not** a new capability. Per Finding 1 these instances already have
  working local tracing in `otel_spans`. The ADR would be lying if it presented
  Cloud-only OTel as "tracing for people who couldn't have it".
- It is the **largest relative win**, because the store being unloaded is the
  same Postgres that runs the control plane, the proxy's settings reads, the
  deployment tables and the queue. Removing span writes, span indexes,
  hypertable chunk maintenance and the span retention sweep from that database
  is a direct improvement to everything else the instance does.
- Their pre-cutover history is in `otel_spans`, and the backfill's Postgres arm
  already handles it. Same migration path, same tool, no new code.
- **No `require_service` blocker exists.** The concern that `temps-otel` might
  hard-require a ClickHouse connection was checked and is unfounded: the plugin
  requires only a `DatabaseConnection`, and `TimescaleDbStorage` is
  unconditionally constructed. There is nothing to unblock. The plugin does need
  a real Cloud-primary mode in the sense that its *span-write* call site becomes
  conditional — but no part of it assumes ClickHouse exists.

One property worth stating because it makes several failure paths tractable:
**because `TimescaleDbStorage` is always constructed, a working local span sink
always exists**, even on an instance whose operator decommissioned ClickHouse.
It is slower and not sized for scale, but it is never absent. §7 relies on this.

### 7. What happens when Cloud goes away

Three distinct cases, each with an explicit answer.

#### 7a. Cloud is transiently unreachable

The outbox retains and the worker retries on the existing backoff curve. No
local span write. The status surfaces queue depth and oldest-unshipped age. This
is the case §3 is sized for, and it is the reason the queue must be durable and
byte-bounded rather than a 10,000-span in-memory buffer.

If the outage outlasts the queue cap, the gap window from §3d is recorded and
shown. **Those spans are genuinely not captured.** That is a real cost, it is
accepted deliberately for opted-in projects, and it is honest and visible rather
than silent — which is the whole difference between a degraded feature and a
bug.

#### 7b. Cloud refuses for a reason only the operator can fix

`Unavailable::QuotaExhausted`, `NotEntitled`, or `LinkStatus::CredentialRejected`
are not transient. Retrying until the queue overflows would convert a billing or
enrolment state into data loss.

**This is the sharpest new risk in the whole design.** Today `QuotaExhausted`
degrades the *mirror* to sampling while local keeps everything —
`MirrorHealth::Degraded`'s own message says *"Sampling until {resets_at}; raise
the cap or upgrade to keep full fidelity."* Under Cloud-primary the same
response would be sampling away the only copy.

Decision: a sustained refusal **closes the current `cloud` interval and opens a
`local` one** with `reason = quota_exhausted` (or `credential_rejected`), so
span writes resume to the local store immediately, and the console says exactly
that: *"Cloud ingest allowance exhausted. Spans for 4 projects are being stored
on this instance until 1 Oct. Raise the allowance or accept local storage."*
The operator's declared `write_mode` is unchanged — intent is preserved — and
the mode returns to `cloud` automatically when Cloud starts accepting again,
closing that `local` interval in turn. The ledger (§1) is what makes this
representable; a single cutover timestamp could not express it.

The queued spans are flushed to the local store rather than dropped, bounded by
the queue cap. This is the one place a local write on a Cloud-primary path is
correct, and it is why §6's "a local sink always exists" matters.

**This partially undercuts the resource goal, and the ADR says so plainly:** an
operator who is one quota event away from needing the local store cannot safely
decommission it. The decommission guidance in §5 must therefore be conditioned
on headroom against the plan allowance, not only on the write mode. The
alternative — dropping data instead of falling back — would be a cleaner story
and a worse product.

#### 7c. Cloud is disabled or disconnected by the operator

Turning off `CloudFeatureSwitches.telemetry`, or `DELETE /cloud`, while any
project is Cloud-primary:

- **The disconnect flips every Cloud-primary project back to `write_mode =
  local` in the same transaction**, closing their `cloud` intervals with
  `reason = cloud_disconnected` and opening `local` ones. Local span writes
  resume immediately. There is never a state in which the instance is storing
  spans nowhere.
- **`disconnect()` attempts a bounded final drain of the outbox** (the flusher
  already has this shape at shutdown, `SHUTDOWN_FLUSH_TIMEOUT`), and whatever it
  cannot ship is written to the local store rather than discarded. Those are
  real spans and the local store is about to be primary again.
- **Data already in Cloud is not repatriated.** It stays in Cloud, readable only
  while a link exists and only within the plan's retention window. Disconnecting
  therefore makes that window unreadable from this instance.

**So: is "ClickHouse is not needed" a real architectural property, or just a
default?** The precise answer, stated so it cannot be misread:

> It is a real property of the **write path while the link is active**: a
> Cloud-primary project's spans are never written to any local span store, and
> an operator can decommission ClickHouse on that basis. It is **not** a
> permanent property of the instance: the local store is the unconditional
> fallback the moment Cloud stops accepting writes, whether that is a quota
> event or the operator's own disconnect. An instance that has decommissioned
> ClickHouse falls back to TimescaleDB, which is always present.

The disconnect confirmation dialog must state both halves — how many projects
flip back, and how much Cloud-held history becomes unreadable (count and date
range) — as primary copy, not a footnote. Losing visibility of a year of traces
because a button said only "Disconnect" is precisely the "no one to ask for
help" failure.

**Deliberately not built in v1: automatic export-on-disconnect.** Pulling a
plan's full retention window back down is an unbounded, metered read triggered
by a button whose meaning today is "stop paying". A separate, explicit,
resumable *export Cloud telemetry to this instance* action is the right shape;
it is Phase C. Until it exists, the disconnect flow is honest about the loss
rather than pretending it can be avoided.

### 8. Reads: this simplifies ADR-040 Phase B rather than complicating it

For a Cloud-primary project there is no local copy of post-cutover data, so
there is no local-vs-Cloud *policy* decision to make about it. The routing
question collapses from "compare the local retention floor against the requested
window, and the fidelity, and the capability" to a single lookup against the
interval ledger:

- Window entirely inside a `cloud` interval → serve from Cloud.
- Window entirely inside a `local` interval → serve locally.
- Window straddles intervals → **clamp to the newest interval it touches and
  report `window_clamped_at`**, exactly as ADR-040 §3 already specifies. No
  merging, for exactly ADR-040's reasons (the badge can only name one source;
  pagination across two stores is not coherently solvable with cursors).

This **retires ADR-040's Open Question 2** ("what is the local retention floor,
per project, as a queryable value?") for Cloud-primary projects: the ledger
holds exact, cheap, stored boundaries where "the earliest span I still hold" was
neither cheap nor exact on both backends. Projects that stay `Local` keep
ADR-040's original `auto` policy unchanged.

The no-silent-fallback contract, the `TelemetrySourceDescriptor` on every
response, the badge, the 503/502/409/200 status mapping and the "never serve
local rows under a Cloud label" invariant are all **unchanged**. This ADR
narrows when the routing decision is hard; it does not touch what the routing
decision is allowed to do.

**Binding requirement on where the decorator is installed.** ADR-040 §2's
`CloudRoutedOtelStorage` must wrap the storage at the plugin's
`context.register_service(storage.clone())` call site — not only inside the
three query handlers. Otherwise `HealthComputeService`, `CrossProjectTraceService`,
`TraceReader` (and therefore the AI chat's trace tools) and
`temps-observability` all keep reading a store that no longer has the data, and
each one degrades to empty results with no error and no badge. Handler-level
routing additionally attaches the source descriptor to the response; that is a
presentation concern layered on top, not the routing seam itself.

**Honest capability regression: span facets.** Facets are slot columns on the
local span table plus `ALTER TABLE ... UPDATE` backfills
(`facet_service.rs`); Cloud has no counterpart, and the `Queryable` projection
ships only allowlisted attributes. For a Cloud-primary project, facet
registration must return `configured: false` with a reason naming the write mode
and a setup path back to it — never accept a facet that will silently never
populate.

### 9. Surfaces (Feature Discoverability)

- **Project settings — canonical.** The write-mode control sits directly beside
  the fidelity control, because one gates the other. It renders **even when
  Cloud is not linked**, in onboarding state: what it would do ("stop storing
  this project's spans on this instance"), what is missing ("not linked to Temps
  Cloud"), and a link to `/settings/cloud`. It never disappears.
- **Cloud settings page.** Aggregate view: how many projects are Cloud-primary,
  `local_span_store_required` with its specific reason, queue depth, oldest
  unshipped span age, gap windows, and the decommission guidance when and only
  when it is true.
- **Traces page.** ADR-040's `TelemetrySourceBadge` (already wired in Phase B),
  plus a persistent banner while the queue is backed up or a gap window
  intersects the displayed range. A gap must be visible on the page where its
  absence would otherwise be misread as "nothing happened".
- **Capability endpoint.** Extend `GET /cloud/capability` (which already returns
  `configured` / `reason` / `setup_path`) rather than adding a parallel one, so
  the client can distinguish "not built" from "not set up" without inferring
  from errors.
- **CLI parity.** Per CLAUDE.md, API-client commands go in `apps/temps-cli`
  (`@temps-sdk/cli`), never as Rust subcommands: `temps cloud telemetry
  write-mode [get|set]` and `temps cloud telemetry status`. `temps backfill
  cloud-telemetry` correctly stays a Rust subcommand — it is a server-lifecycle
  operation over local data, not an API client call.

---

## Alternatives Considered

### Option A: Leave ADR-040 as-is — local-first with a Cloud mirror, Cloud used only for extended retention

- **Pros:** zero new risk; the invariant is trivially preserved; no transport
  work; ADR-040 Phase B ships as already designed.
- **Cons:** delivers none of the stated motivation. Local ClickHouse (or the
  `otel_spans` hypertable) keeps running at full cost, which is the specific
  thing the product owner asked to eliminate. Cloud remains a strictly additive
  expense on top of the local footprint rather than a replacement for it.
- **Rejected.** It is the status quo, and the status quo is what was ruled out.

### Option B: An instance-wide switch instead of a per-project mode

- **Pros:** simplest possible model; directly achieves "the local span store is
  unused" in one flip; no partial-cutover confusion; the derived
  `local_span_store_required` signal becomes trivially `!cloud_primary`.
- **Cons:** consent for span egress is already per project
  (`cloud_telemetry_fidelity`), and this decision is strictly downstream of that
  consent — an instance-wide write mode would either force every project to
  `Queryable` or create projects that are Cloud-primary at `Metered`, which is
  the unreachable-by-design worst state from §1. It also makes the cutover
  all-or-nothing on a one-way door, with no way to trial one project.
- **Rejected.** The granularity must match the consent it depends on.

### Option C: Dual-write with a drastically shortened local retention (hours, not days)

- **Pros:** every local consumer (health, AI trace tools, cross-project linking,
  Observe, facets) keeps working unchanged. A recent-window safety net survives
  a Cloud outage. Storage volume still drops by one to two orders of magnitude.
  No new durability primitive needed.
- **Cons:** it does not remove the write path, the index maintenance or the
  hypertable/part churn, which on a 3 vCPU box is most of the actual CPU and IO
  cost — the volume was never the binding constraint. It keeps two stores
  holding the same window, which is exactly the situation ADR-040's badge exists
  to disambiguate. And it is not what was asked for.
- **Rejected as the primary design — but explicitly retained as the de-risking
  fallback.** If §3's durability and throughput work does not hold up under real
  load, shortening local retention is the smallest change that recovers most of
  the resource win without betting recent telemetry on Cloud reachability. Named
  here so it is a considered fallback rather than a panic.

### Option D: Have the instance write directly to Cloud's ClickHouse, bypassing `/v1/telemetry`

- **Pros:** removes the gateway hop; native batch inserts; no protocol
  projection layer; would trivially exceed any throughput target.
- **Cons:** tenant resolution, quota reservation, metering and retry idempotency
  all live in the gateway (`temps-app` ADR 0015). A write bypass is
  simultaneously a billing bypass and an isolation bypass. It also destroys the
  premise of ADR 0018's read proxy security argument, which turns on the tenant
  credential being INSERT-capable and the read-only check being the only thing
  standing between a read endpoint and a write. And it would move the
  fidelity/allowlist projection — the entire consent mechanism, which lives in
  `cloud_span()` on the instance — out of the only place it can be enforced.
- **Rejected categorically.** There is no raw ClickHouse write surface and there
  must never be one.

### Option E: A bespoke encrypted on-disk queue file for the outbox

- **Pros:** no local database write amplification at all, which is the purest
  reading of "clickhouse must not be needed"; independent of Postgres
  availability; naturally byte-bounded.
- **Cons:** a new durability primitive — fsync discipline, corruption recovery,
  compaction, crash-consistency, encryption at rest for real span payloads — on
  the one code path whose entire purpose is not losing data. The existing
  encrypted enrollment-state file is emphatically not a candidate: it is
  rewritten wholesale on every flush and is a credential store, so growing it to
  hold the outbox would rewrite the whole queue per batch.
- **Rejected in favour of the proven `ch_fanout` outbox pattern.** Reconsider
  only if Postgres write amplification measurably matters, at which point the
  worker interface stays the same and only the queue implementation changes.

### Option F: Keep the existing in-memory spool as the primary buffer, unchanged

- **Pros:** zero transport work; ships immediately; the shape is already right.
- **Cons:** the numbers from Finding 3 — ~33 spans/s steady state, 10,000 spans
  / 8 MiB, nothing survives a restart, whole batches dropped at the producer
  handoff, and oldest-first eviction that shreds traces rather than producing a
  clean gap.
- **Rejected.** This is the difference between a design and a data-loss
  incident, and it is the single most important finding in this ADR.

### Option G: Point the customer's OTLP exporters straight at Temps Cloud

- **Pros:** zero instance cost, no queue, no outbox, no throughput ceiling; the
  instance stops being in the telemetry data path entirely.
- **Cons:** every application would need a Cloud-scoped ingest credential
  distributed to it, multiplying the credential surface across the customer's
  whole fleet. The instance would lose the ability to apply the fidelity
  projection and the attribute allowlist, because that runs inside `cloud_span()`
  on the instance — the consent mechanism would simply cease to exist. It also
  discards the instance's own ingest auth (`si_`/`tk_` keys), rate limiting and
  project resolution.
- **Rejected.** The instance being in the path is what makes consent
  enforceable.

---

## Consequences

### Positive

- The resource motivation is actually delivered: a fully cut-over instance
  stores no spans locally, so ClickHouse can be decommissioned outright (shape
  B) or the `otel_spans` hypertable stops growing on the control-plane Postgres
  (shape C). The span retention job stops running in both.
- The win reaches the majority population. Because the default install has no
  ClickHouse, framing this as "no local span store" rather than "no ClickHouse"
  means shape C — the common case — benefits rather than being a footnote.
- ADR-040 Phase B's routing gets *simpler*, not harder: an exact interval ledger
  replaces a retention-floor estimate, and ADR-040's Open Question 2 is retired
  for Cloud-primary projects.
- The `Queryable` fidelity tier gains a second, stronger reason to exist. It was
  an opt-in for longer retention; it becomes the gate on a genuine architectural
  change, which makes its consent copy easier to justify to the operator.
- The durable outbox is reusable. Logs, metrics and analytics (ADR-040 Phases
  C/D) need exactly the same at-least-once shape, and Phase D was already going
  to reuse `ch_fanout`'s outbox. This converges rather than diverges.
- Every failure state is nameable and visible: queue depth, oldest unshipped
  age, gap windows with start and end, and the specific reason for any mode flip
  the operator did not make.

### Negative

- **The instance depends on Cloud reachability for its own recent telemetry.**
  This is the real cost and it should not be softened. For an opted-in project,
  a Cloud outage that outlasts the queue cap produces a permanent hole in that
  project's traces. A self-hosted operator who chose self-hosting specifically
  to not depend on anyone else's uptime is trading exactly that away, for that
  signal, for that project. It is why the mode is per project, default off, and
  gated on an explicit consent tier.
- **Trace visibility latency regresses.** Today a span is queryable immediately
  after the local write. Cloud-primary spans are queryable only after an enqueue,
  a drain cycle and a round-trip. Even with drain-until-idle that is seconds, not
  milliseconds. Near-real-time trace debugging — watching a request you just
  made — is materially worse, and this is probably the most user-visible
  downside of the whole design.
- **OTLP 2xx means something weaker.** "Committed to the durable outbox", not
  "committed to storage". Stronger than fire-and-forget, weaker than today, and
  it must be documented rather than discovered.
- **Span facets stop being available** for Cloud-primary projects, with no Cloud
  counterpart in sight.
- **Cross-project trace linking (ADR-027) spans two sources** when a linked
  project is Cloud-primary and another is not. v1 resolves each project's
  segment against its own ledger and renders what it can, marking segments it
  could not resolve — it does not silently omit them.
- **The local store usually cannot actually be deleted.** Postgres stays (it
  holds the outbox and everything else), and `TimescaleDbStorage` stays as the
  §7 fallback. What can genuinely be decommissioned is ClickHouse as a separate
  service, and the `otel_spans` growth. Promising "no local storage" would be
  false advertising; promising "no local span store while the link is healthy"
  is true.
- **A partial cutover yields zero resource win.** One project left in `Local`
  mode keeps the entire local span store running. Operators will reasonably
  believe they have saved something before they have, which is why
  `local_span_store_required` is derived and prominent rather than implied.
- **Two-repo coordination gets more expensive.** A bug in Cloud's ingest path
  now loses a project's telemetry outright instead of losing a mirror, and only
  the OSS half is publicly testable.

### On the governing invariant — stated precisely, because this is the part most likely to be misread

`temps-app`'s invariant is **"when every cloud service is down, the customer's
instance keeps working."** This ADR does not repeal it. It is precise about what
"working" covers for a Cloud-linked instance:

**Unchanged when Cloud is down.** The proxy serves traffic. Deployments run.
Databases run. Backups run to their configured destination. The console loads.
OTLP ingest returns 2xx and does not block, stall, or add latency. Local
projects' telemetry is stored exactly as before. Nothing crashes and nothing
requires a restart to recover.

**Degraded when Cloud is down, for opted-in projects only.** Recent traces for
Cloud-primary projects queue durably; past the queue cap they are lost, and the
loss is shown as a bounded gap with a start and an end. Reading Cloud-held
history returns ADR-040's explicit 503 with a region and a reason, never local
rows under a Cloud label and never an empty 200.

**What would violate the invariant, and is therefore forbidden.** Blocking or
failing ingest on Cloud availability. Crashing or refusing to start without
Cloud. Losing data silently. Leaving the console in a permanent loading state.
Requiring a restart to notice recovery. Making a Cloud outage degrade any
non-telemetry subsystem.

The invariant is about the *instance* continuing to function, not about every
optional signal being captured under all conditions. Relocating one opted-in
signal's durability to Cloud, with a bounded local buffer and an honest visible
gap, is a degradation *of that signal* — which is what "they degrade to the free
product" already contemplates — and not a runtime dependency of the instance.

### Risks

- **Queue overflow that looks like nothing happened.** If the gap ledger or the
  banner slips a phase, the failure mode is an invisible hole. Mitigation: they
  ship in the same phase as the write-mode switch, not after it, and the
  acceptance test asserts a gap is recorded and rendered.
- **A buggy quota fallback loses data while the status reads healthy.**
  §7b's automatic mode flip is the mechanism preventing quota exhaustion from
  becoming data loss; if it fails to fire, everything looks fine and nothing is
  stored. Mitigation: an explicit test that a `QuotaExhausted` response closes
  the `cloud` interval and resumes local writes, and that the console says so.
- **Decommission, then disconnect.** An operator who removes ClickHouse and
  later disconnects Cloud falls back to TimescaleDB span storage at a volume it
  was not sized for on that box. Mitigation: both the decommission guidance and
  the disconnect confirmation must say this in plain terms.
- **Backfill direction confusion.** `temps backfill cloud-telemetry` moves
  *local history up to Cloud*. On a Cloud-primary project an operator may
  reasonably expect it to bring Cloud data down. Mitigation: the command refuses
  with a message naming the direction and pointing at the (Phase C) export
  action, rather than doing nothing useful.
- **Fidelity downgrade racing a Cloud-primary write.** ADR-040 Open Questions 9
  and 10 (cache invalidation and purging already-buffered `Queryable` records on
  downgrade) become sharper here, because the outbox is durable and survives
  restarts — a downgrade must purge or re-project the *persisted* queue, not
  just the in-memory spool. Mitigation: the write-mode gate in §1 already
  forbids downgrading fidelity while `write_mode = cloud`, which closes the
  worst version of this race by construction.
- **Throughput target unproven against real Cloud.** The ~33 spans/s ceiling is
  measured from this repository's code; what `/v1/telemetry` will actually
  sustain from one instance, and whether concurrent submissions are safe for its
  idempotency and metering, is not established here. Mitigation: Phase B1's
  acceptance criteria are load-based and must be met before any project can be
  set Cloud-primary.

---

## Relationship to ADR-040

ADR-040 stays in force. This ADR changes one premise inside it and narrows one
section; everything else is unaffected.

| ADR-040 element | Status after this ADR |
|---|---|
| §1 fidelity tiers, attribute allowlist, backfill (**Phase A, shipped**) | **Unchanged.** This ADR builds on it and makes `Queryable` a hard prerequisite for the new write mode. |
| §2 reuse of ClickHouse-backed storage types against a Cloud-pointed client | **Unchanged.** Still the read design. |
| §3 no-silent-fallback contract, badge, status mapping, straddle clamping | **Unchanged.** Reaffirmed. |
| §3 `auto` routing policy (retention floor comparison) | **Narrowed.** For Cloud-primary projects it is replaced by the interval ledger (§8 here). For `Local` projects it is unchanged. |
| §4 Cloud-side read proxy contract | **Unchanged.** |
| §5 signal scope (traces/span stats in v1) | **Unchanged.** This ADR's write-mode change is likewise spans-only. |
| Context: *"local is primary, Cloud is an optional mirror, and a Cloud problem can never become a local problem"* | **Amended.** True for `Local`-mode projects; for Cloud-primary projects Cloud is primary for the span write, bounded by the durable outbox and an honest gap. |
| Open Question 2 (local retention floor as a queryable value) | **Retired for Cloud-primary projects**; still open for `Local` ones. |
| Open Questions 9 and 10 (cache invalidation, purging buffered `Queryable` records) | **Still binding, and sharper** — the queue is now durable. Partly mitigated by §1's gate. |
| **Phase B** as scoped in ADR-040 | **Superseded by Phase B1/B2 below.** |
| Phases C and D (logs, metrics, analytics) | **Unchanged**, and now share the outbox this ADR builds. |

An amendment note pointing here is added to ADR-040 in its existing amendment
style, at the top and at the specific places that state the local-first premise.
ADR-040's original text is not rewritten.

---

## Implementation Notes

### Phase A — prerequisite, already shipped

ADR-040 Phase A (`projects.cloud_telemetry_fidelity`,
`cloud_telemetry_attribute_allowlist`, the extended `SpanRecord`,
`temps backfill cloud-telemetry`, backfill progress + Console card) plus
`temps-app` ADR 0019's deletion endpoint. **Nothing new is required here.** This
ADR's §1 gate depends on all of it.

### Phase B1 — durable transport (must land and be load-tested before any project can be Cloud-primary)

**Affected crates:**

- `temps-entities` / `temps-migrations` — the span outbox table (project id,
  serialized `temps_cloud_protocol::SpanRecord`, enqueue time, attempt count,
  delivery state, dead-letter marker), modelled on `events_ch_outbox`.
- `temps-cloud-client` — a durable-queue-backed shipping worker alongside the
  existing `Spool`/`flusher` (which stay, unchanged, for `Local`-mode mirroring);
  drain-until-idle per `cloud_backfill.rs`'s precedent; byte-bounded cap read
  from the singleton `settings` row via `ConfigService`; gap-window recording;
  mode-aware `MirrorHealth` rendering that no longer asserts local authority.
- `temps-config` / `temps-entities` — outbox byte cap on the `settings` row. **No
  environment variable.**

**Acceptance criteria — load-based, not coverage-based:**

- Sustains a stated spans/second rate for a stated duration with Cloud healthy,
  with the queue draining to empty. The rate must be justified in the PR against
  expected instance load, per the Scalability rules.
- With Cloud stubbed down: zero loss until the byte cap; at the cap, exact drop
  accounting and a gap window with a correct start and end.
- Restart mid-outage: everything enqueued before the restart still ships after
  it. This is the property the current spool does not have.
- Producer handoff: no whole-batch drops on the ingest path.
- Bounded memory throughout; the queue's cost is disk, not RAM.

**Migration:** yes (one new table). **Breaking changes:** none.

### Phase B2 — write mode, routing, surfaces

**Affected crates:**

- `temps-entities` / `temps-migrations` — `projects.cloud_telemetry_write_mode`
  (enum, default `local`); `project_telemetry_write_intervals` (append-only
  ledger); gap-window records.
- `temps-otel` — the §1 gate in the project settings write path (reject
  `cloud` without `queryable` + linked + telemetry-on; reject a fidelity
  downgrade while `cloud`); `ingest_spans` partitioning by write mode;
  `CloudPolicyCache` extended to carry the write mode with the same TTL and the
  same fail-safe direction (an unresolvable project resolves to `local` +
  `metered`, so a lookup failure can only ever be *safer*); facet registration
  returning `configured: false` for Cloud-primary projects.
- `temps-otel` plugin — install ADR-040's `CloudRoutedOtelStorage` at the
  `register_service` call site so health, cross-project, `TraceReader` and
  `temps-observability` inherit routing. This is the requirement whose omission
  silently empties four features.
- `temps-cloud` — `disconnect()` and the telemetry feature switch flip
  Cloud-primary projects back to `local` transactionally and drain the outbox
  with a bounded final attempt, spilling to the local store rather than
  dropping; `CloudStatus` gains write-mode counts, queue depth, oldest
  unshipped age, gap windows and `local_span_store_required` with its reason;
  `GET /cloud/capability` extended rather than duplicated.
- `web/` — the write-mode control beside the fidelity control in project
  settings, rendering in onboarding state when unlinked; the Cloud settings
  aggregate view and decommission guidance; the Traces backlog/gap banner; the
  disconnect confirmation stating both halves of the loss. Regenerate
  `web/src/api/client/`.
- `apps/temps-cli` — `temps cloud telemetry write-mode [get|set]` and
  `temps cloud telemetry status`, via `bun run spec:update && bun run
  generate:api`.

**Tests that encode the invariants:**

- An instance with no Cloud link has a behaviourally identical span-ingest path
  before and after (shape A regression guard).
- `write_mode = cloud` is rejected at `metered` fidelity, when unlinked, and
  when the telemetry switch is off — with distinguishable, actionable errors.
- Lowering fidelity while `write_mode = cloud` is rejected and names the write
  mode as the thing to change.
- A Cloud-primary project's ingest performs **no** local span write; a
  `Local` project in the same batch performs exactly one.
- `DELETE /cloud` with Cloud-primary projects: all flip to `local` in one
  transaction, the ledger closes correctly, un-shippable queued spans land in
  the local store, and local writes resume on the very next ingest.
- A `QuotaExhausted` response closes the `cloud` interval, resumes local writes,
  and surfaces the reason; recovery reopens a `cloud` interval.
- A query straddling two ledger intervals is clamped and reports
  `window_clamped_at`; it never merges sources.
- Every span reader routed through the decorator (health, cross-project,
  `TraceReader`, observability) returns Cloud-served data for a Cloud-primary
  project rather than empty.
- Facet registration on a Cloud-primary project returns `configured: false` with
  a setup path, not success.

**Migration:** yes (one column, one ledger table, gap records — all additive,
all defaulted). **Breaking changes:** none to the wire API; the OTLP 2xx
*semantic* change is documented.

**Security review: required before merge.** `security-auditor` must sign off on:
the §1 gate being unbypassable (Cloud-primary at `Metered` must be unreachable
through every write path, including direct PATCH); the outbox holding real
`Queryable` span data at rest inside the existing database being consistent with
the deployment's posture; the disconnect path not stranding or leaking queued
spans; the ledger not becoming an information-disclosure channel across
projects; and the interaction with ADR-040 Open Questions 9/10 now that the
buffer is durable.

### Phase C — export back, and other signals

- **Export Cloud telemetry to this instance**: explicit, resumable,
  operator-driven, the inverse of `temps backfill cloud-telemetry`. Required
  before disconnect can be anything other than "you lose visibility of that
  window".
- Logs and metrics Cloud-primary modes, once ADR-040 Phase C gives them a Cloud
  write path at all. They reuse this ADR's outbox and this ADR's ledger; they
  need their own fidelity/consent treatment, because log bodies are a strictly
  larger egress question than spans.
- Analytics (ADR-040 Phase D) likewise, and it already planned to reuse the
  `ch_fanout` outbox this ADR generalises.

---

## References

- `crates/temps-otel/src/plugin.rs` — storage-backend selection (~548–640) and
  `read_clickhouse_otel_config_from_env` (~1219): the proof that ClickHouse is
  optional, fail-closed, and that `TimescaleDbStorage` is always constructed
- `docker-compose.yml` — ships no ClickHouse service; the default install is
  TimescaleDB-only
- `crates/temps-otel/src/services/otel_service.rs` — `cloud_span()` (fidelity
  projection, ~71–155) and `ingest_spans()` (~521–576, local-first ordering and
  the `link.record(mirror)` call site this ADR partitions)
- `crates/temps-cloud-client/src/spool.rs` — `DEFAULT_CAPACITY` (10,000),
  `DEFAULT_CAPACITY_BYTES` (8 MiB), oldest-first eviction, no persistence
- `crates/temps-cloud-client/src/link.rs` — `BATCH_SIZE` (500),
  `INCOMING_BATCH_CAPACITY` (8), `record()`, `drain_incoming()`, `flush()`, and
  the `pending_submission` persistence path
- `crates/temps-cloud-client/src/flusher.rs` — `BASE_INTERVAL` (15 s),
  `MAX_INTERVAL` (300 s), `SHUTDOWN_FLUSH_TIMEOUT`
- `crates/temps-cloud-client/src/status.rs` — `MirrorHealth` and the
  "Source telemetry remains in local Temps storage" copy this pivot invalidates
- `crates/temps-cloud-protocol/src/lib.rs` — `Capability`, `Unavailable`
  (`QuotaExhausted`, `NotEntitled`, `Degraded`) and the forward-compatible
  `#[serde(other)]` negotiation
- `crates/temps-analytics-events/src/services/ch_fanout.rs` — the outbox
  claim/deliver worker pattern this ADR reuses for durability
- `crates/temps-otel/src/services/cloud_backfill.rs` — the drain-until-idle
  precedent, and `CloudBackfillSource`'s Postgres and ClickHouse arms
- `crates/temps-otel/src/services/facet_service.rs` — local slot columns, the
  capability with no Cloud counterpart
- `crates/temps-otel/src/services/health_service.rs`,
  `crates/temps-otel/src/services/cross_project.rs`,
  `crates/temps-otel/src/services/trace_reader.rs`,
  `crates/temps-observability/src/service.rs` — the span readers that are not the
  Traces page
- `crates/temps-cloud/src/handler.rs` and `src/service.rs` —
  `/cloud/capability`, `/cloud/status`, `PATCH /cloud/features`, `DELETE /cloud`,
  `CloudCapability { configured, reason, setup_path }`, `SETUP_PATH`
- `crates/temps-config/src/service.rs` — the singleton `settings` row and its
  cache, the correct home for the outbox byte cap
- ADR-040 — cloud telemetry read source; this ADR amends its write-path premise
- ADR-027 — cross-project trace linking
- ADR-016 — ClickHouse traces backend (why ClickHouse exists as an option)
- `temps-app` ADR 0015 — instance-scoped telemetry gateways; why every write
  goes through `POST /v1/telemetry`
- `temps-app` ADR 0018 — read-only ClickHouse query proxy (contract level)
- `temps-app` ADR 0019 — on-demand per-project telemetry deletion; the reverse
  valve, unaffected by this pivot
