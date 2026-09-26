# ADR-040: Cloud Telemetry Read Source — Serving Console Queries from Temps Cloud

**Status:** Proposed
**Date:** 2026-09-01
**Author:** David Viejo

> **Numbering note:** ADR-039 (MCP server rebuild) is the most recent committed
> ADR. 030–037 are occupied by in-flight or parked proposals in separate
> worktrees. 040 is the next free number from the global perspective.

> **Amendment (2026-09-01).** After this ADR was drafted, a spike proved out
> the read path end to end against a real ClickHouse deployment and a security
> review of that spike changed the shape of §2 and §4 below. The original
> design in those two sections proposed a bespoke `RemoteTelemetryQuery` trait
> and a hand-designed REST API with five routes. The spike found a simpler
> path: both repositories already depend on the same `clickhouse` Rust client
> at the same version, so Temps Cloud can expose a read-only endpoint that
> speaks that client's own wire protocol instead of a custom API, and the OSS
> side can query it with the *same storage types it already uses locally* —
> zero new query-shape code, on either side. §2 and §4 below describe that
> design, not the original one. Everything else in this ADR — the fidelity-tier
> gate in §1, the routing/no-fallback contract in §3, the signal-by-signal
> scope in §5, and the Errors decision — is unaffected by this change and
> remains as originally decided. Cloud-side implementation details (crate
> names, internal role architecture, tenant-isolation mechanics) are
> intentionally described here only at the contract level the OSS client
> depends on; the private repo carries its own ADR with the implementation.

> **Amendment (2026-09-01, second).** After Phase A shipped, the product owner
> pivoted the *write* model: when an instance is linked to Temps Cloud with
> telemetry enabled, spans for opted-in projects should be written directly to
> Cloud and not stored locally at all, so that a local span store — ClickHouse
> or the TimescaleDB `otel_spans` hypertable — stops being needed. The
> motivation is the instance's resource footprint, not retention. That change
> is specified in **ADR-041 (Cloud-Primary Telemetry Writes)**, which amends
> this ADR rather than replacing it. What changes here: the local-first premise
> in the Context section below, the retention-floor form of the `auto` routing
> policy in §3, and the scope of Phase B. What does **not** change: the fidelity
> tiers and backfill in §1 (Phase A, shipped, and now a hard prerequisite for
> Cloud-primary writes), the storage-reuse design in §2, the no-silent-fallback
> contract and the badge in §3, the Cloud-side read contract in §4, and the
> signal-by-signal scope in §5. Inline notes below mark the specific paragraphs
> ADR-041 supersedes; the original text is left intact rather than rewritten.

---

## Context

### What the Cloud link does today

`feat/cloud-funnel` gave a self-hosted instance an optional link to Temps Cloud:
`CloudLink` (`crates/temps-cloud-client`) holds an instance-scoped, encrypted,
enrollment-derived credential; `CloudService` (`crates/temps-cloud`) exposes it
to the rest of the binary. The link is **write-only**. Spans are offered to the
mirror at ingest time and nowhere else — `OtelService::ingest_spans` builds a
mirror batch, writes locally first, and only calls `link.record(mirror)` after
the local write succeeded (`crates/temps-otel/src/services/otel_service.rs`,
around lines 432–455). There is no backfill, no replay, and no read.

That ordering is deliberate and must not change: local is primary, Cloud is an
optional mirror, and a Cloud problem can never become a local problem.

> **Amended by ADR-041.** This remains the behaviour for projects in the
> default `Local` write mode, which is every project unless an operator
> explicitly opts in. ADR-041 adds an opt-in per-project Cloud-primary write
> mode in which no local span write happens at all and the span write is
> instead durably queued for Cloud. A Cloud problem still cannot become a local
> problem — ingest never blocks and never fails on Cloud availability — but for
> a Cloud-primary project a long enough Cloud outage does become a bounded,
> visible gap in that project's traces. See ADR-041 §2, §3 and §7.

### The finding that shapes this entire ADR

**What is mirrored to Cloud today is not queryable telemetry.** It is a metering
and liveness projection. `cloud_span()` in
`crates/temps-otel/src/services/otel_service.rs` (lines 71–91) constructs every
mirrored span as:

```rust
temps_cloud_protocol::SpanRecord {
    trace_id: link.pseudonymize_telemetry_id("trace", &span.trace_id)?,  // HMAC-SHA256, 64 hex
    span_id:  link.pseudonymize_telemetry_id("span",  &span.span_id)?,   // HMAC-SHA256, 64 hex
    name: "span".to_string(),          // constant — the real name never leaves
    ts_millis: span.start_time.timestamp_millis(),
    duration_ms: span.duration_ms,
    attributes: Default::default(),    // always empty — default-deny at the OSS boundary
}
```

The comments there are explicit about why: span names routinely carry raw URLs,
SQL and identifiers, and arbitrary attributes carry headers and user data, so
the continuous projection ships neither. The Cloud-side table stores exactly
those fields plus metering columns — and notably has **no `project_id`, no
`environment_id`, no `service_name`, and no status/error column** at all
(confirmed by reading the private repo's schema; its exact location is
documented there, not here).

The practical consequence, laid against what the Traces page actually asks for:

| The console needs (`TraceQuery`, `TraceSummary`) | Present in Cloud today |
|---|---|
| `project_id` scoping | **no** — not in the payload or the table |
| `service_name` filter | **no** |
| Real span/operation name | **no** — always the literal `"span"` |
| Error / status filtering, error counts | **no** |
| Span attributes (the trace detail view) | **no** — always empty |
| Real trace ID the user can correlate elsewhere | **no** — HMAC pseudonym |
| Timestamp, duration | yes |

A read path built on top of today's mirror would render a page of identical rows
named `span`, with 64-hex identifiers that match nothing in the user's
application logs, unattributable to any project. That is not degraded data —
it reads as data loss, which is precisely the failure mode CLAUDE.md's
"build as if the user has no one to ask for help" rule exists to prevent.

Equally: **only spans are mirrored.** Metrics, logs, analytics events and error
groups have no write path to Cloud whatsoever. There is nothing to read back for
four of the five pages named in the product goal.

So the honest framing of this work is not "add a query endpoint". It is:

1. a **fidelity decision** on the write path (a data-egress change requiring
   explicit operator consent), then
2. a **read API** on the Cloud side, then
3. a **routing seam** in the OSS backend, then
4. **badge wiring** in the console.

Steps 2–4 are cheap. Step 1 is the one with real consequences, and skipping it
is what would turn this feature into the stub the product owner explicitly
ruled out.

### What the console already commits to

`web/src/components/global/TelemetrySourceBadge.tsx` exists, typechecks, and is
not yet wired to any page. Its contract is treated here as **fixed**; the
backend contract is designed to satisfy it:

```ts
export type TelemetrySourceKind = 'this_instance' | 'temps_cloud'
export type TelemetrySourceStatus = 'live' | 'unavailable'
export interface TelemetrySource {
  kind: TelemetrySourceKind
  region?: string | null   // "eu-1" -> rendered "EU"
  status: TelemetrySourceStatus
}
```

Two statements in that file are load-bearing requirements, not decoration:

- *"Where a telemetry query's page of results came from — never inferred from an
  unrelated setting (e.g. 'Export telemetry to Cloud' being on). The backend sets
  this per response, on the request that actually ran."*
- *"Temps Cloud did not respond to this query. Results are not shown rather than
  silently substituting local data under a Cloud label."*

The second one is a hard architectural constraint: **the backend must never
serve local rows under a Cloud label, and must never serve Cloud rows under a
local label.** A design that "falls back for resilience" is a design that lies.

### Current storage abstractions (verified by reading the code)

| Surface | File | Shape | Error type | Implementations |
|---|---|---|---|---|
| `OtelStorage` | `crates/temps-otel/src/storage/mod.rs` | ~30 methods spanning traces, metrics, logs, insights, quota, retention, ingest-error reporting. Reads *and* writes. | `OtelError` (`StorageResult<T>`) | `TimescaleDbStorage`, `ClickHouseOtelStorage`, `MockOtelStorage` |
| `AnalyticsEvents` | `crates/temps-analytics-events/src/services/traits.rs` | 12 read-only query methods, each taking a `*Spec` value-type. Writes deliberately excluded. | `EventsError` | `AnalyticsEventsService` (Timescale), `ClickHouseEventsBackend` |
| `AnalyticsBackend` | `crates/temps-analytics-backend/src/traits.rs` | `name()` + `health_check()` only. | `AnalyticsBackendError` | `TimescaleBackend`, `ClickHouseBackend` |
| Error tracking | `crates/temps-error-tracking/src/**` | **No trait at all.** `ErrorCRUDService`, `ErrorAlertService`, `ErrorAnalyticsService`, `ErrorIngestionService`, `SourceMapService` are plain structs over `Arc<DatabaseConnection>`. | per-service enums | Sea-ORM/Postgres only |

Two things follow from this table.

First, the "abstract ClickHouse out of analytics" work is **already done** at the
query level. `events_service.rs` has no direct ClickHouse calls left outside the
trait implementations; handlers depend on `Arc<dyn AnalyticsEvents>`. The same is
true of traces via `Arc<dyn OtelStorage>`. What is *not* abstracted is one level
lower: `temps-otel` and `temps-analytics-backend` each construct their own
`clickhouse::Client`, each read their own `TEMPS_CLICKHOUSE_*` configuration, and
each own a separate DDL/migration runner. "Change the ClickHouse implementation"
is therefore a two-crate edit today, with no compiler guarantee that the two stay
consistent. That duplication is the real target of the owner's instruction — and,
per the amendment above, it turns out to be the *whole* target: because both
`ClickHouseOtelStorage` and `ClickHouseBackend`/`ClickHouseEventsBackend` are
already implemented purely in terms of a `clickhouse::Client`, giving Cloud a
wire-compatible endpoint (§4) means Cloud can be served by the *same* structs,
constructed a second time against a different client, rather than by a new trait
implementation. §2 develops this.

Second, `AnalyticsBackendError::BackendUnavailable { backend, reason }` already
exists and is exactly the right precedent for how an unavailable source should be
reported — a named backend plus a reason, not a generic failure.

### Forces

1. **The browser must never talk to ClickHouse or hold Cloud credentials.** Every
   cross-repo call is OSS-instance-backend → Cloud API, authenticated with the
   existing instance-scoped encrypted credential. Non-negotiable.
2. **No second auth mechanism.** `CloudClient` already authenticates every call
   with `bearer_auth(<instance token>)` against `/v1/...`; Cloud resolves that
   same token to a tenant for every request. The read path reuses both the
   credential and the resolution.
3. **Tenant scope is resolved server-side, from the authenticated token, never
   from anything the client supplies** — an existing rule in the private repo's
   own architecture, documented there. Cloud's ClickHouse enforces this with a
   per-tenant credential and a row-level policy; a read path is only safe to add
   because that isolation already exists and was independently verified (§4).
4. **The cloud must never break an instance that never opts in.** Local storage,
   local retention and local query paths behave exactly as before.
5. **Each crate owns its error enum; no shared error types across domains**
   (CLAUDE.md). `OtelError` and `EventsError` are structurally different and are
   not trivially unifiable.
6. **One domain per crate; cross-domain calls go through service traits.**
7. **No new runtime configuration as environment variables.** Anything an
   operator tunes per project is a column on the relevant entity row, encrypted
   via `EncryptionService` if sensitive.
8. **Unconfigured features onboard rather than disappear**, and every failure
   path surfaces a specific, actionable state.
9. **Scalability:** the instance runs on ~3 vCPU / 4 GB. A Cloud query must never
   block the local query path, and must have a bounded timeout and bounded memory.

---

## Decision

### 1. Cloud read-back is gated on a per-project telemetry **fidelity tier**

Introduce an explicit, opt-in fidelity setting rather than silently widening what
the mirror ships.

```
TelemetryFidelity ::= Metered   (default, today's behaviour, unchanged)
                    | Queryable (opt-in, per project)
```

Stored as a column on the project row — `projects.cloud_telemetry_fidelity` —
per the no-env-var rule, so it is per-project, changeable at runtime through the
API/UI, and audit-logged for free. It is not sensitive, so it is not encrypted;
the allowlist column below is likewise not a secret.

**`Metered` (default).** Exactly what ships today: HMAC trace/span IDs, constant
`name: "span"`, empty attributes. Answers "is my instance alive and how much am
I being billed for". **Not readable back** — the read API reports this project as
unreadable with a reason and a setup path.

**`Queryable` (opt-in).** Extends `temps_cloud_protocol::SpanRecord` with fields
that make a span renderable. All additions are `#[serde(default)]` so an older
gateway or an older instance keeps working — the protocol's stated compatibility
rule ("the cloud must be able to accept a batch from an instance several versions
behind without a translation table") is preserved.

Added fields:

| Field | Type | Notes |
|---|---|---|
| `project_ref` | `String` | **Pseudonymous.** `pseudonymize_telemetry_id("project", project_id)`. A stable scoping key that does not disclose the project's *name*, and that no third party observing the payload can correlate to a local project. It is **not** a secret from Cloud itself: the HMAC key is the instance's own bearer token, which Cloud issued and receives on every request, and the input is a small integer — so the key holder can enumerate `HMAC(token, "project\0" \|\| i)` and invert every `project_ref` in the tenant. That is acceptable (Cloud is the tenant's own provider, not a third party) but must not be described as if Cloud were blinded. The trace/span pseudonyms below are different: their 128-bit random inputs are not enumerable. |
| `service_name` | `Option<String>` | Operator-visible in the consent copy. Real value — a service name the user cannot filter on is not a Traces page. |
| `name` | `String` | The real span name at `Queryable`, still `"span"` at `Metered`. |
| `span_kind` | `Option<String>` | Low cardinality, enum-like. |
| `status_code` | `Option<String>` | `OK`/`ERROR`/`UNSET`. Required for error counts and the error filter. |
| `parent_span_id` | `Option<String>` | Required to render a trace tree. |
| `environment` | `Option<String>` | |
| `attributes` | `BTreeMap<String,String>` | **Default-deny allowlist**, see below. |

**Trace and span IDs are shipped in the clear at `Queryable` fidelity.** This is
the single most consequential call in this ADR and it is made deliberately.

HMAC pseudonymisation is deterministic, so a pseudonymised trace tree *would*
still render and a lookup-by-trace-ID *would* still work (hash the query term).
What it destroys is everything the feature exists for: the ID displayed to the
user is a 64-hex value that appears nowhere in their application logs, cannot be
pasted into another tool, cannot be correlated with an error group or a log line
that carries the real ID, and breaks cross-project trace linking (ADR-027). A
user who cuts over to Cloud retention specifically to investigate an old
incident would be handed identifiers that match nothing. That is a defect, not a
privacy win.

A trace ID is a random 128-bit value with no intrinsic meaning; its sensitivity
comes from correlation against data the tenant already owns. Shipping it is
therefore acceptable — **but only behind an explicit per-project opt-in with
consent copy that says plainly what leaves the instance.** `Metered` fidelity
keeps HMAC pseudonymisation unchanged, and remains the default for every
existing and future link.

**Attribute egress is default-deny with an operator-editable allowlist.** Column
`projects.cloud_telemetry_attribute_allowlist` (`Vec<String>`, default empty).
Only exact-match keys on the list are shipped; everything else is dropped at the
instance before the batch is built. Empty list means no attributes leave, which
is the current behaviour and the safe default even after opting into
`Queryable`. This keeps the "arbitrary span attributes routinely contain
headers, SQL and user identifiers" hazard closed by construction rather than by
operator diligence.

**Backfill is required, not optional.** Raising fidelity only affects spans
ingested *after* the change, which would leave a permanent hole between "link
established" and "fidelity raised" — exactly the window an operator cutting over
to Cloud retention wants to read. Add a one-shot, out-of-process CLI subcommand
modelled directly on the existing `temps backfill clickhouse` helper
(`crates/temps-analytics-events/src/services/ch_backfill.rs`), which already
solves this shape: cursor-based over `(timestamp, id)`, batched, resumable, safe
to re-run, and deliberately run outside `temps serve` so it never contends with
live ingest for row locks.

```
temps backfill cloud-telemetry --project <id> --from <ts> --to <ts> [--dry-run]
```

Idempotency on the Cloud side is by `submission_id` (already bound to the
authenticated instance and payload digest by the metering layer) plus dedup on
`(trace_id, span_id, ts)`. `--dry-run` reports the row count and estimated
metered bytes before anything egresses, because "how much will this cost and what
exactly am I sending" must be answerable before the send, not after the invoice.

CLI parity note: per CLAUDE.md, the API-client commands for the new endpoints
belong in `apps/temps-cli` (`@temps-sdk/cli`). `backfill` is a server-lifecycle
operation on local data and correctly stays a Rust subcommand alongside the
existing `temps backfill clickhouse`.

### 2. The shared abstraction: the same storage types, pointed at a second connection

This is the direct answer to the second instruction — make traces and analytics
use *the same* abstraction so it can be changed atomically. The spike (see the
amendment above) found a materially simpler way to get there than the
`RemoteTelemetryQuery`-trait design this section originally proposed.

#### Rejected: merge `OtelStorage` and `AnalyticsEvents` into one trait

A single `TelemetryStore` covering both would be a ~42-method trait. Three
independent reasons to reject it:

- **Cloud can only implement a strict subset.** Cloud has no metrics, no logs, no
  insights, no quota rows, no retention job, no ingest-error table, no analytics
  events. A merged trait forces ~30 `Unsupported` stubs into the Cloud
  implementation. A trait that is mostly unimplemented is not an abstraction; it
  is a compile-time-checked list of things that do not work.
- **It breaks the error-ownership rule.** `OtelError` and `EventsError` are
  different enums owned by different crates, and CLAUDE.md forbids shared error
  types across domains. A merged trait forces one enum on both, or an
  `anyhow`-shaped lowest common denominator — both are explicitly prohibited.
- **It breaks the crate-boundary rule.** Traces and analytics are two domains.
  Fusing their read surfaces into one trait means one crate owns both, and every
  future analytics query signature change recompiles and re-reviews the tracing
  path for no reason.

#### Rejected (originally chosen, superseded by the spike): a bespoke `RemoteTelemetryQuery` trait and Cloud API client

The ADR originally specified a new `temps-telemetry-source` crate owning a
narrow `RemoteTelemetryQuery` trait, implemented by a `CloudTelemetryQuery`
client calling five hand-designed REST routes on Cloud (§4, original text). This
is a reasonable design and remains a fallback if the approach below ever proves
too permissive — but it duplicates query-building logic that already exists:
every query `RemoteTelemetryQuery` would need to express is a query
`ClickHouseOtelStorage`/`ClickHouseBackend` already knows how to build for the
*local* case. Superseded by the design below, which reuses that logic instead of
re-expressing it behind a new trait.

#### Chosen: reuse the existing ClickHouse-backed storage types against a second, Cloud-pointed client

`ClickHouseOtelStorage` (`temps-otel`) and `ClickHouseBackend`/
`ClickHouseEventsBackend` (`temps-analytics-backend`/`temps-analytics-events`)
are already implemented purely in terms of a `clickhouse::Client` — none of them
know or care whether that client points at a local ClickHouse or somewhere else.
Temps Cloud now exposes a read-only endpoint that speaks the same HTTP interface
the `clickhouse` crate already uses (§4). So "Cloud as a telemetry source" does
not need a new trait, a new query language, or a new client type: it needs the
*same struct*, constructed a second time, against a `clickhouse::Client` built by
`CloudLink::clickhouse_query_client()` (`crates/temps-cloud-client`) instead of
against local connection settings. Zero new query-shape code on either side of
the Cloud boundary; every future query `OtelStorage`/`AnalyticsEvents` grows,
Cloud can already answer (subject to the read-only proxy accepting it) with no
Cloud-side release.

This does **not** mean handing out the Cloud-pointed instance as a raw
`Arc<dyn OtelStorage>` — its write, retention, quota and insight methods would
forward straight into the proxy's read-only enforcement and simply fail. A
routing wrapper still gates it to reads only; that's Tier 2, unchanged in shape
from the original design:

```rust
// crates/temps-otel/src/storage/routed.rs
pub struct CloudRoutedOtelStorage {
    local: Arc<dyn OtelStorage>,
    cloud: Arc<dyn OtelStorage>,   // the SAME impl type, built from a Cloud-pointed client
    router: Arc<SourceRouter>,
}
impl OtelStorage for CloudRoutedOtelStorage {
    /* every write/retention/quota/insight method delegates unconditionally to
       `local`; only the read methods this feature covers (§5) consult
       `router` and call `cloud` when it resolves to Cloud */
}

// crates/temps-analytics-events/src/services/routed.rs
pub struct CloudRoutedAnalyticsEvents {
    local: Arc<dyn AnalyticsEvents>,
    cloud: Arc<dyn AnalyticsEvents>,
    router: Arc<SourceRouter>,
}
impl AnalyticsEvents for CloudRoutedAnalyticsEvents { /* same router shape */ }
```

`source.rs` and `policy.rs` from the original Tier 1 design are still needed —
`TelemetrySourceKind`/`TelemetrySourceStatus`/`TelemetrySourceDescriptor`
(serializing exactly to the badge's `TelemetrySource`) and `SourcePreference`/
`SourceRouter` (the routing decision, §3). What's gone is `remote.rs` (the
bespoke trait) and `clickhouse.rs` (a new shared connection abstraction) —
neither is needed because both domains already own their own
`clickhouse::Client`-based implementation, and the Cloud-pointed instance is
just another value of that same type.

**Why this satisfies "changeable atomically together", concretely.** Both
decorators are constructed from a client built by the *same*
`CloudLink::clickhouse_query_client()` call and the *same* `Arc<SourceRouter>`,
and both depend on the same `TelemetrySourceDescriptor`. Therefore:

- Changing how Cloud is reached (its URL, its auth scheme, its read-only
  enforcement) is one edit to `CloudLink::clickhouse_query_client()` and applies
  to both domains simultaneously — it is not possible to migrate traces and
  leave analytics behind.
- Changing the routing policy (retention floor, preference semantics, failure
  handling) is one edit to `policy.rs` and applies to both by construction.
- Changing the badge wire shape is one edit to `source.rs`, and the OpenAPI
  schema for every covered endpoint moves together.

That is the atomicity that was asked for, achieved with less new code than the
original design, because it reuses rather than re-expresses the existing
ClickHouse query-building logic.

**Error handling keeps crate ownership intact.** A `TelemetrySourceError` (or
equivalent) is still introduced, minimally, to express "the Cloud-pointed client
failed" distinctly from "the local backend failed" — each domain crate adds
exactly one variant to its own enum and one `From` impl, same as originally
specified:

```rust
// temps-otel
#[error("Telemetry source unavailable: {0}")]
OtelError::TelemetrySource(#[from] TelemetrySourceError)

// temps-analytics-events
#[error("Telemetry source unavailable: {0}")]
EventsError::TelemetrySource(#[from] TelemetrySourceError)
```

No shared error enum crosses a domain boundary; each crate still owns its own and
maps to `Problem` in its own handler module, exactly as the rule requires.

**`AnalyticsBackend` gains a third implementation.** `CloudBackend` implements
`name() -> "temps_cloud"` and `health_check()`, which is what drives the badge's
`status` field and reuses the existing
`AnalyticsBackendError::BackendUnavailable { backend, reason }` precedent.

### 3. Routing rule, and the no-silent-fallback contract

**Request.** Every covered endpoint accepts an optional `source` query parameter:

```
?source=local | cloud | auto      (default: auto)
```

> **Narrowed by ADR-041.** The retention-floor comparison below assumes local
> always holds at least a recent window of the data. That assumption does not
> hold for a project in ADR-041's Cloud-primary write mode, which has no local
> copy of post-cutover data at all. For those projects `auto` resolves against
> an exact per-project write-mode interval ledger instead of a retention-floor
> estimate (ADR-041 §1 and §8), which also retires Open Question 2 below for
> them. For `Local`-mode projects the policy below is unchanged.

**`auto`.** Serve from local unless *all* of the following hold, in which case
serve from Cloud: the requested window starts before the local retention floor
for that project; the project's fidelity is `Queryable`; the link is `Linked`
(not `CredentialRejected`, not `AwaitingEnrollment`); and Cloud capability
reports the window as covered. `auto` may resolve to local at any point *before
it has committed to a source* — and the response then truthfully reports
`this_instance`. That is a routing decision, not a fallback.

**`cloud` never falls back.** Once the request has committed to Cloud, a Cloud
failure is reported as a Cloud failure. Local rows are never returned under a
Cloud label. This is the component's stated contract and it is enforced server-
side, not by client convention.

**Straddling windows are not merged.** A query whose window crosses the local
retention floor is served from exactly one source, and the response reports which
one plus `window_clamped_at`, so the UI can say "showing 30 Aug onward from this
instance; earlier data is in Temps Cloud" instead of silently presenting a
partial page as complete. Merging two stores with different fidelity under one
badge is precisely the mislabelling the badge exists to prevent, and a merged
result cannot be paginated coherently anyway.

**Success response shape.** Every covered endpoint already returns a named
`{ data, count }`-style struct (verified: `TracesResponse`, `TraceSummariesResponse`,
`LogsResponse`, `SpanStatsResponse`, `OtelMetricsResponse` — none returns a bare
array). So the change is purely additive: one new field.

```rust
#[derive(Debug, Serialize, ToSchema)]
pub struct TraceSummariesResponse {
    pub data: Vec<TraceSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    /// Which store served THIS response. Set by the backend on the request
    /// that actually ran; never inferred by the client.
    pub source: TelemetrySourceDescriptor,   // NEW
}
```

Existing clients that ignore the new field are unaffected. Both generated clients
(`web/src/api/client/`, `apps/temps-cli/src/api/`) must be regenerated.

**Failure response shape.** When the committed source is Cloud and Cloud does not
answer, the endpoint returns a non-2xx RFC 7807 `Problem` — *not* a 200 with an
empty body, which would be a success response for a failed query — and carries the
source descriptor as a Problem extension member so the client can render the
badge without inferring anything:

```
HTTP/1.1 503 Service Unavailable
Content-Type: application/problem+json
Retry-After: 30

{
  "type": "https://temps.sh/probs/telemetry-source-unavailable",
  "title": "Temps Cloud Unavailable",
  "status": 503,
  "detail": "Temps Cloud (eu-1) did not respond within 10s. No results are shown rather than substituting local data under a Cloud label.",
  "instance": "/otel/traces",
  "telemetry_source": { "kind": "temps_cloud", "region": "eu-1", "status": "unavailable" },
  "retry_after_secs": 30
}
```

Status code mapping — exhaustive, no catch-all arm:

| Condition | Status | Notes |
|---|---|---|
| Cloud unreachable / timed out | 503 | `Retry-After` set |
| Cloud returned 429 | 503 | `Retry-After` echoed from Cloud |
| Cloud response unparsable | 502 | version skew or a bug; distinct from "down" |
| Credential rejected (`LinkStatus::CredentialRejected`) | 502 | actionable: re-enroll. Detail says so. |
| Not linked / fidelity is `Metered` / project never mirrored | 409 | not a failure — a configuration state. Carries `setup_path`. |
| Window predates Cloud's earliest data | 200 | empty `data`, `source.status: "live"`. Honest: Cloud answered, and the answer is "nothing". |

The 409 case matters for CLAUDE.md's onboarding rule: "not set up yet" must be
distinguishable from "broken", and it links straight to the settings page that
raises fidelity.

The response body on any failure path **never contains rows**.

**Timeouts and blast radius.** The Cloud read reuses `CloudClient`'s existing
10 s `REQUEST_TIMEOUT`. A Cloud query is issued only on the request that asked
for it, never speculatively, never in parallel with the local query "just in
case" (that would double egress and double cost for a result thrown away). The
local query path is untouched when `source` resolves to local.

### 4. Cloud-side contract (what the OSS client depends on)

Implemented and security-reviewed as a spike in the private repo; described here
only at the level the OSS client actually relies on. Internal placement,
role/process architecture and tenant-isolation mechanics belong to that repo's
own ADR — referenced here by outcome, not by file path.

**The shape: a read-only wire-protocol proxy, not a hand-designed REST API.**
`POST /v1/telemetry/query` accepts a request in exactly the shape the `clickhouse`
Rust crate's HTTP client already sends (query text, `database`, `default_format`
and a small allowlist of other ClickHouse settings) and forwards it to Cloud's
ClickHouse, scoped to the caller's tenant, returning ClickHouse's own response
format unmodified. `CloudLink::clickhouse_query_client()` builds a
`clickhouse::Client` pointed at this endpoint — no new query DSL, no per-question
route, and no Cloud release required when the OSS side needs a query shape it
didn't need before.

**Auth.** The instance's existing enrollment-derived token — the same credential
already used for `POST /v1/telemetry` — carried in the `Authorization: Basic`
header the `clickhouse` client sends (its username field is ignored). No new
credential, no new mechanism, no browser-facing token. This is what makes the
"browser never holds Cloud credentials" constraint structural rather than a
convention: the OSS *backend* holds the token and builds the client; the browser
never sees it.

**Read-only is enforced Cloud-side, twice, independently — this is the entire
security boundary this design places on the Cloud side**, since (unlike the
original REST design) there is no per-endpoint shape constraining what a query
can ask for: the statement's leading keyword must be `SELECT`/`WITH`/`SHOW`/
`DESCRIBE`/`EXPLAIN` after comment-stripping, *and* `readonly=1` is injected into
every forwarded query's settings regardless of what the caller sent. Reviewed and
verified against a live ClickHouse deployment: the tenant's underlying
credential *is* grantable `INSERT` (required for the normal write/mirror path),
so this check is what stops the read endpoint from becoming a write bypass — not
a nice-to-have.

**Isolation.** The query is forwarded using ClickHouse credentials scoped to
whichever tenant the instance's token resolves to server-side — never anything
the caller names. Verified live: one tenant's token cannot read another
tenant's rows, cannot see another tenant's presence in shared system tables, and
the write grant a tenant does hold is refused specifically by the read-only
check above, not by accident of scope.

**Cost bounding.** Every forwarded query carries fixed `max_execution_time`,
`max_rows_to_read`, `max_result_rows` and `max_memory_usage` settings (Cloud-side
constants, provisional — nothing prices a *read* today), plus a per-tenant
concurrency limit. **Known gap, required before this leaves spike status:** the
concurrency limit as implemented is per-process, so its real ceiling scales with
however many replicas of Cloud's data-plane role are running — it needs to be
backed by a ClickHouse-side quota object bound to the tenant's own credential
(the same layer the row-policy isolation above already lives at) before this is
more than a spike. Read volume is also unmetered in v1, deliberately (temps-app's
rule is never to auto-charge on drift, and there's no usage data yet to price
reads from) — but unmetered is not the same as unbounded, and the quota above is
what makes that distinction real.

**Response.** ClickHouse's own HTTP response, unmodified — whatever format the
requesting `clickhouse::Client` asked for. No `source`/`region`/pagination
envelope is added at this layer; that's the routing layer's job (§3), sitting on
the OSS side where the badge's `TelemetrySourceDescriptor` is actually assembled.
Cloud does not need to know about the badge at all — it only needs to answer
queries correctly, scoped, bounded, and read-only.

### 5. Signal-by-signal scope for v1

| Signal | Mirrored today | v1 read-back | Badge in v1 | Why |
|---|---|---|---|---|
| **Traces** | spans, `Metered` only | **yes**, after §1 | live, both states | The end-to-end deliverable |
| **Span stats** | derived from spans | **yes** | live, both states | Same rows, same API |
| **Logs** | nothing | no | `this_instance` | No write path; Phase C |
| **Metrics** | nothing | no | `this_instance` | No write path; Phase C |
| **Analytics** | nothing | no | `this_instance` | No write path; Phase D |
| **Errors** | nothing | **out of scope**, see below | `this_instance` | Structural, not schedule |

Rendering `this_instance` on the not-yet-covered pages is not a stub: it is
**true**, it is set by the backend on the request that ran, and it makes the
concept discoverable everywhere it will eventually apply. The badge never
disappears, and it never claims a capability that does not exist.

**Errors are deliberately out of scope, and the reason is structural.** An error
group is not an append-only telemetry signal. It is a mutable, stateful entity:
assignee, status, snooze, resolution, linked release, source-map association —
with foreign keys into Postgres tables (`users`, `projects`, releases, source
maps) that exist only on the instance. Asking "which store served this error
group" is the wrong question: the events are immutable and could live anywhere,
but the triage state is local and mutable by definition, and no badge can
honestly label a row that is half local and half remote.

Making errors readable from Cloud therefore requires a genuine prior refactor —
splitting an error into (a) an immutable event stream that can be mirrored, and
(b) mutable local triage state that cannot — and, before that, introducing the
first storage trait that domain has ever had (`ErrorStore`), since
`crates/temps-error-tracking` today has no abstraction at all. **That is a
separate ADR with a named prerequisite, not a phase of this one.** Attempting it
here would double this ADR's blast radius and couple a trace-retention feature to
an error-tracking refactor.

---

## Consequences

### Positive

- Cloud retention becomes genuinely usable: an operator whose local retention
  expired can still open the Traces page and read history, from the same console,
  with no new tool and no credential handling.
- The badge tells the truth on every response, because the backend sets it on the
  request that ran. There is no client-side inference and no code path that can
  mislabel a result.
- The failure state is explicit, specific and actionable — a 503 with a region, a
  reason and a `Retry-After`, or a 409 with a `setup_path` — rather than a silent
  substitution or a spinner that never resolves.
- ClickHouse connection, configuration and health stop being duplicated across
  `temps-otel` and `temps-analytics-backend`. "Change the ClickHouse
  implementation" becomes a one-crate edit.
- Traces and analytics are wired to one routing seam, one remote trait and one
  source descriptor, so they cannot drift; a contract change breaks both builds
  at once instead of silently migrating one.
- The data-egress increase is opt-in, per project, default-off, attribute
  default-deny, and fully described to the operator before it happens
  (`--dry-run` reports rows and metered bytes).
- Cloud's read path reuses the existing credential, the existing tenant resolver
  and the existing row-policy isolation. No new trust boundary is created — the
  existing one is used for one more verb.

### Negative

- `Queryable` fidelity is a real, meaningful increase in what leaves a
  self-hosted instance: real span names, real service names, real trace IDs, and
  allowlisted attributes. That is the honest cost of the feature and it cannot be
  designed away, only made explicit and opt-in.
- Two fidelity tiers means two shapes of Cloud data, indefinitely. Spans mirrored
  before an operator opts in stay unreadable forever unless they run the backfill,
  and the backfill only reaches back as far as *local* retention still holds.
  There is a permanent, explainable hole for anyone who links, waits past local
  retention, and only then raises fidelity.
- The decorator pattern adds an indirection to every read on both domains, even
  for instances that never link. It is a branch on an `Arc` and is negligible, but
  it is one more layer in the stack trace of every telemetry query.
- Four of the five named pages ship with a badge that reads `this_instance`
  unconditionally in v1. Truthful, but a reviewer skimming the UI will reasonably
  ask why Analytics has a badge that never changes.
- The OSS repo now carries a client for a Cloud endpoint that only exists in a
  private repo. A contract change on either side is a two-repo coordination, and
  only the OSS half is publicly testable.

### Risks

- **Fidelity is a one-way door in practice.** Once spans with real names and IDs
  are in Cloud, lowering fidelity back to `Metered` does not retract them.
  Mitigation: the consent copy must say so explicitly, and the settings UI must
  offer a "delete telemetry from Cloud for this project" action in the same place.
  If that deletion path does not exist yet in `temps-app`, it must ship in the
  same phase as `Queryable`.
- **Cost surprise from deep pagination.** A user clicking to page 40 of a 90-day
  window issues 40 metered Cloud queries. Mitigated by cursor pagination,
  `max_rows_to_read`, and opt-in totals — but reads are unbilled in v1 precisely
  because the access pattern is not yet understood. Revisit before pricing them.
- **Read load degrading ingest.** The read proxy and telemetry ingest currently
  share a process and a ClickHouse cluster on the Cloud side. Mitigated by a
  separate pool, a separate semaphore and a separate rate limiter — but splitting
  reads onto their own scaling unit must be pre-planned (the private repo's own
  ADR covers this) and triggered on a defined metric, not on an outage.
- **Badge trust erosion.** The whole feature rests on the badge being
  unconditionally accurate. A single code path that returns local rows with
  `kind: "temps_cloud"` — for example a well-meaning future `try_cloud_then_local`
  helper — destroys its value permanently. Mitigation: an explicit test asserting
  that a failing remote never yields a 2xx with rows, and a comment on
  `SourceRouter` stating the invariant.
- **Version skew across two repos.** An OSS instance several versions ahead of a
  gateway, or vice versa. Mitigated by `#[serde(default)]` on every added protocol
  field and by the existing capability-negotiation pattern (`Capability`,
  `Unavailable`, `#[serde(other)] Unknown`), which this design extends rather than
  bypasses. A new `Capability::TelemetryQuery` variant should gate the read path.

---

## Alternatives Considered

### Option A: Merge `OtelStorage` and `AnalyticsEvents` into one `TelemetryStore` trait

- **Pros:** literally one abstraction, so "change them atomically" is trivially
  satisfied; one place to add Cloud.
- **Cons:** ~42 methods, of which Cloud can implement ~5, leaving ~37
  `Unsupported` stubs. Forces one error enum across two domains, which CLAUDE.md
  prohibits. Fuses two domains into one crate, violating the crate-boundary rule.
  Every analytics signature change would recompile and re-review the tracing path.
- **Rejected.** The shared-abstraction requirement is real, but it is satisfied by
  sharing the *routing, transport, source identity and remote contract* rather
  than the domain query surfaces. §2 gets the same atomicity guarantee without
  any of these costs.

### Option B: Browser queries Temps Cloud directly with a short-lived scoped token

- **Pros:** removes a proxy hop; the OSS backend does no work; Cloud can serve
  the console directly and scale reads independently of any instance.
- **Cons:** violates the explicit non-negotiable constraint. Puts a Cloud-scoped
  credential in the browser, where XSS in any console page becomes tenant-wide
  telemetry exfiltration. Requires CORS on a metered data-plane endpoint. Splits
  the console's auth model in two. Makes the badge unverifiable, since the OSS
  backend no longer knows what served the page.
- **Rejected** on the constraint alone; the security cost independently confirms
  it.

### Option C: Ship the read path over the existing `Metered` mirror, no fidelity tier

- **Pros:** no data-egress change, no consent copy, no security review, no
  protocol change. Shippable immediately.
- **Cons:** returns rows all named `"span"` with HMAC identifiers and no project
  attribution — see the table in Context. It cannot filter by service, cannot show
  an error count, cannot render a trace detail view, and hands the user
  identifiers that match nothing they own. It would be read as data loss.
- **Rejected.** This is the shape the "no stub" instruction rules out, and it
  would burn user trust in Cloud retention on first contact.

### Option D: Add Cloud as a third `OtelStorage` implementation

- **Pros:** no new crate; slots into the existing plugin wiring; `Arc<dyn
  OtelStorage>` already flows everywhere.
- **Cons:** `OtelStorage` includes `store_metrics`, `store_spans`, `archive_logs`,
  `apply_retention`, `upsert_insight`, `check_quota`, `record_ingest_error` and
  more — roughly 20 methods Cloud must not implement. Worse, choosing Cloud as
  *the* storage would mean the instance stops serving local reads entirely, when
  the requirement is to serve *either* per query. Storage selection is
  process-wide; source selection is per-request. They are different axes.
- **Rejected as originally framed, but closer to §2's final design than it looks
  in hindsight.** §2 (post-amendment) does construct a Cloud-pointed instance of
  the *same* `ClickHouseOtelStorage` type this option describes — the difference
  that keeps this option's cons from applying is that it's never handed out as a
  raw `Arc<dyn OtelStorage>`. The decorator gates it to read-only routing, so the
  ~20 methods Cloud must not serve are simply never called on it, rather than
  needing to be implemented as `Unsupported`. The two independent axes this
  option conflates — storage selection (process-wide) vs. source selection
  (per-request) — remain independent in the chosen design; what changed is only
  *how* the Cloud side of that second axis is implemented.

### Option E: Merge local and Cloud results for windows that straddle the retention floor

- **Pros:** the nicest user experience — one continuous timeline, no visible seam.
- **Cons:** the badge can only name one source, so a merged page is unlabelable
  without lying. The two sides have different fidelity (`Metered` rows have no
  name and no attributes), so merged rows would be visibly inconsistent.
  Pagination across two stores with different sort stability is not coherently
  solvable with cursors.
- **Rejected for v1.** §3 clamps the window and reports `window_clamped_at`
  instead, which is honest and lets the UI offer an explicit "view earlier data in
  Temps Cloud" action. Revisit only if the badge is redesigned to express a
  composite source.

### Option F: Give Cloud's read path its own dedicated scaling unit now

- **Pros:** perfect isolation of read load from ingest from day one.
- **Cons:** a new process to deploy, monitor, scale and pay for, before a single
  byte of read traffic exists. Premature.
- **Deferred, not rejected.** The private repo's implementation keeps the read
  proxy behind its own crate/module boundary precisely so this becomes a mounting
  change later rather than a rewrite.

---

## Open Questions for the Implementer

1. **Does `temps-app` have a per-project telemetry deletion path?** The
   fidelity-is-a-one-way-door risk is only acceptable if the operator can delete
   what they sent. If no such endpoint exists, it must ship with `Queryable`, not
   after it. Confirm before Phase A.
2. **What is the local retention floor, per project, as a queryable value?**
   `SourceRouter`'s `auto` decision needs it. `OtelStorage::get_storage_quota` and
   `apply_retention` know about retention, but whether an exact "earliest span I
   still hold" timestamp is cheaply available on both backends needs confirming;
   if not, `auto` should fall back to "the configured retention window" and say so.
3. **Does the `project_ref` pseudonym survive re-enrollment?**
   `pseudonymize_linked_id` derives its HMAC key from the instance token. A
   re-enroll issues a new token, which would change every `project_ref` and orphan
   all previously mirrored data. This is already true for `trace_id`/`span_id`
   today, but it becomes user-visible once data is readable. Decide: derive the
   pseudonym key from a separate, enrollment-stable secret, or accept and document
   that re-enrollment starts a new readable history.
4. **`Capability::TelemetryQuery` negotiation.** Confirm whether the read path
   should be gated on a new `Capability` variant (recommended) and, if so, that
   older gateways decline it cleanly rather than 404-ing in a way the client
   reports as "unavailable" instead of "not supported".
5. **Rate-limit and quota semantics for reads on a `QuotaExhausted` tenant.**
   Ingest degrades to head-sampling when over quota. Should reads be blocked,
   throttled, or unaffected? Blocking reads on a tenant that has already paid for
   the retained data seems wrong; confirm with the billing rules. Related and now
   confirmed via the spike's security review: the per-tenant read concurrency
   limit as built is process-local rather than backed by a ClickHouse-side quota
   object, so it doesn't hold under horizontal scaling of Cloud's data plane —
   this needs the same real quota mechanism before general availability, not
   just before this specific question is answered.
6. **Where does the fidelity opt-in UI live** — project settings, or the Cloud
   link settings page? It is a per-project setting on a global link, so it needs a
   surface in both, with one of them canonical.
7. **Capability negotiation has no endpoint yet.** The originally-specified
   `GET /v1/telemetry/capability` (readable?, why not, region, retention window,
   fidelity, per-project earliest timestamp) was part of the REST design §4
   superseded; the wire-protocol proxy that replaced it doesn't inherently
   provide this. `SourceRouter`'s `auto` decision (§3) and the fidelity/onboarding
   UI both need *some* way to ask "can I read this project from Cloud right now,
   and if not, why". Decide whether this is a small dedicated endpoint kept
   alongside the proxy, or answerable via a bounded query through the proxy
   itself (e.g. a capability-relevant system table read) — the latter avoids a
   second Cloud route but couples capability semantics to what the read-only
   query surface happens to expose.
8. **`query_id` collision safety and the read-only enforcement's two layers**
   were both independently verified against a live ClickHouse deployment during
   the spike's security review and are implemented — not open questions, noted
   here so an implementer picking up Phase B doesn't re-litigate them. What
   *is* still open, beyond the quota gap in #5: whether upstream ClickHouse
   error bodies (which currently pass through close to verbatim) need further
   sanitization before this leaves spike status, and whether the per-tenant
   concurrency bound needs to vary by plan tier once reads are metered.
9. **The fidelity/allowlist write path MUST invalidate the policy cache in the
   same request.** Not a question — a binding requirement on the Phase B
   endpoint that does not exist yet, recorded here so it cannot be forgotten
   when it is built. `CloudPolicyCache::invalidate(project_id)` and
   `invalidate_all()` (`crates/temps-otel/src/services/cloud_fidelity.rs`) are
   implemented and registered as a service by `temps-otel`'s plugin
   specifically so the settings path can call them, and today they have **zero
   production callers** — Phase A ships no write path, so the only way to change
   fidelity is a direct `UPDATE`. The moment a PATCH handler exists, omitting
   the `invalidate` call means a fidelity *downgrade* keeps shipping `Queryable`
   spans — real names, real trace IDs, allowlisted attributes — for up to
   `CLOUD_POLICY_CACHE_TTL` after the operator believes they stopped it, with a
   UI that already says `metered`. Upgrades are self-correcting and downgrades
   are not, which is exactly the wrong asymmetry for a consent control. The same
   applies to editing `cloud_telemetry_attribute_allowlist`: removing a key must
   take effect on the next batch, not one TTL later. A test asserting that the
   PATCH handler's effect is visible on the immediately following projection is
   the acceptance criterion.
10. **Lowering fidelity, or removing an allowlist key, MUST purge or re-project
    what is already buffered.** Also a requirement rather than a question, and
    the second half of the same hole. Invalidating the cache only governs spans
    projected *after* the change; records already projected at `Queryable` sit
    in `Spool` and, because the link persists its in-flight batch, in the
    `pending_submission` file that survives a process restart
    (`crates/temps-cloud-client/src/link.rs`). `Spool::clear` exists and is
    wired to link-wide telemetry *revocation*, but there is no per-project
    equivalent — so "I turned it off" today would still let queued spans ship
    minutes later, and a restart does not stop them either. Phase B's write path
    must, for the affected project: drop or re-project its buffered spool
    records, and drop or re-project the persisted `pending_submission` if it
    contains any of them. Re-projection (downgrading the buffered records to the
    new fidelity) is preferable to dropping, since the metering/liveness signal
    is not the part being retracted — but dropping is acceptable and shipping
    them unchanged is not. Note this is bounded mitigation, not retraction:
    anything already acknowledged by Cloud is subject to the deletion path in
    Open Question 1, which is the only mechanism that can actually take data
    back.

---

## Implementation Notes

### Phase A — Fidelity, protocol, backfill (prerequisite for everything else)

**Affected crates:**
- `temps-cloud-protocol` — extend `SpanRecord` with `project_ref`,
  `service_name`, `span_kind`, `status_code`, `parent_span_id`, `environment`;
  all `#[serde(default)]`. Add `Capability::TelemetryQuery`.
- `temps-entities` / `temps-migrations` — `projects.cloud_telemetry_fidelity`
  (enum, default `metered`) and `projects.cloud_telemetry_attribute_allowlist`
  (text array, default empty). Neither is a secret; neither is encrypted.
- `temps-otel` — `cloud_span()` becomes fidelity-aware. At `Metered` its output
  is **byte-identical to today** (this must have a test). At `Queryable` it emits
  the extended record with allowlist-filtered attributes.
- `temps-cloud-client` — `pseudonymize_telemetry_id("project", …)` usage;
  surface fidelity in `CloudFeatureSwitches`/`LinkStatus` reporting.
- `temps-cli` (Rust subcommand) — `temps backfill cloud-telemetry`, modelled on
  `crates/temps-analytics-events/src/services/ch_backfill.rs`. Cursor-based,
  resumable, `--dry-run` reporting rows and estimated metered bytes.
  Audit-logged at start and at the terminal state
  (`CLOUD_TELEMETRY_BACKFILL_STARTED` / `..._FINISHED`): the run is a paid,
  one-way write, and the per-project progress row is `UNIQUE (project_id)`, so
  without accumulating audit entries a second run erases the only record that
  the first one happened.

**Migration:** yes (two additive columns, both defaulted).
**Breaking changes:** no. Every protocol addition is `#[serde(default)]`; default
fidelity reproduces current behaviour exactly.
**Security review:** **required before merge.** This phase changes what leaves a
self-hosted instance. `security-auditor` must sign off on: the fidelity default,
the attribute allowlist being default-deny and exact-match, the clear-text trace
ID decision, the consent copy, and the deletion path from Open Question 1.

### Phase B — The read path, end to end (the deliverable)

> **Superseded by ADR-041 Phases B1/B2.** The read-path work below is still
> required and its shape is unchanged, but ADR-041 re-scopes this phase: the
> durable-transport work (B1) must land and be load-tested before any project
> can be set Cloud-primary, and the routing work (B2) additionally covers the
> write-mode switch, the interval ledger, and installing the routing decorator
> at the plugin's `register_service` call site rather than only in the query
> handlers — otherwise the health, cross-project, `TraceReader` and
> observability span readers silently return nothing. See ADR-041's
> Implementation Notes.

**Status:** the Cloud-pointed client half of this phase (the `temps-cloud-client`
bullet below) is done — built, security-reviewed and remediated as the spike
referenced in the amendment. The routing/decorator/badge-wiring half is not yet
started.

**New, small module** (not a new crate — see §2's amendment): `source.rs` and
`policy.rs`, most naturally added to `temps-cloud-client` next to the client
built in the completed spike, or to a thin shared location both `temps-otel` and
`temps-analytics-events` already depend on if one exists — `TelemetrySourceKind`/
`TelemetrySourceStatus`/`TelemetrySourceDescriptor` and `SourcePreference`/
`SourceRouter` as laid out in §2.

**Affected crates:**
- `temps-cloud-client` — done: `CloudLink::clickhouse_query_client()` builds a
  `clickhouse::Client` authenticated with the existing instance token, pointed at
  the Cloud read proxy (§4). Read-side variants exist on the relevant error type
  for "Cloud query client unavailable" distinct from write-path errors.
- `temps-cloud` — expose the query client through `CloudService`; register it in
  the plugin so `require_service` can hand it to the decorators.
- `temps-otel` — `CloudRoutedOtelStorage`; `OtelError::TelemetrySource` variant
  and its `From`; `source` field on `TracesResponse`, `TraceSummariesResponse`,
  `SpanStatsResponse`; `source` query param on `/otel/traces`,
  `/otel/traces/{trace_id}`, `/otel/span-stats`; `From<OtelError> for Problem`
  extended with the exhaustive status mapping from §3, including the
  `telemetry_source` extension member.
- `temps-analytics-backend` — `CloudBackend: AnalyticsBackend`, implementing
  `name()`/`health_check()` against the same Cloud-pointed client.
- `temps-analytics-events` — `CloudRoutedAnalyticsEvents` wired but routing
  every method to `local` (no analytics data exists in Cloud yet);
  `EventsError::TelemetrySource` variant. This is what makes Phase D a
  data-plane change rather than a code change.
- `web/` — wire `TelemetrySourceBadge` into the Traces list, trace detail and
  span-stats headers; read `source` from the response and
  `problem.telemetry_source` from the error. Add the fidelity opt-in surface
  with onboarding copy per CLAUDE.md (state what is missing, show what it would
  do, link to the settings page). Regenerate `web/src/api/client/`.
- `apps/temps-cli` — CLI parity for the new `source` query parameter on the OSS
  instance's own API (the CLI never talks to Cloud directly, per this repo's CLI
  rules). `bun run spec:update && bun run generate:api`; never hand-write
  `openapi.json`.

**Tests that must exist (these encode the invariants, not just coverage):**
- A failing Cloud query with `source=cloud` never produces a 2xx containing rows.
- A failing Cloud query with `source=cloud` never produces a response labelled
  `this_instance`.
- `source=local` never issues a Cloud request (assert on a mock that records
  calls).
- `Metered` fidelity produces a byte-identical mirror payload to the pre-change
  implementation.
- A `Metered` project returns 409 with a `setup_path`, not 503 and not empty
  success.
- A straddling window is clamped and reports `window_clamped_at`.
- Lowering a project's fidelity through the write path is visible to the *next*
  projection, with no TTL delay (Open Question 9 — the handler calls
  `CloudPolicyCache::invalidate`).
- Lowering a project's fidelity, or removing an allowlist key, leaves no
  already-buffered `Queryable` record for that project in either the in-memory
  spool or the persisted `pending_submission` (Open Question 10).

**Migration:** none.
**Breaking changes:** none (additive response field, additive query param).

### Phase C — Logs and metrics

Requires a mirror write path for logs and metrics, which does not exist. Logs
carry free-text bodies and are a strictly larger egress question than spans;
metrics are low-risk by comparison (bounded label cardinality is already a
hot-path rule) and are the better first extension. Extend
`RemoteTelemetryQuery` with `metrics`/`logs` methods — a single-crate change that
both decorators pick up — and flip the badge on those pages. Same fidelity gate,
same allowlist model, same security review.

### Phase D — Analytics

Analytics events carry URLs, referrers, user agents, visitor IDs and custom
properties — PII by construction, and a materially larger consent question than
spans. Deliberately not shipped alongside trace egress. The mechanism is already
present and should be reused rather than reinvented: `ch_fanout.rs`'s outbox
(`events_ch_outbox`) is exactly the at-least-once fan-out shape a Cloud mirror
needs, and `ch_backfill.rs` is the backfill shape. Phase D adds a Cloud sink to
that outbox behind its own fidelity tier, then routes
`CloudRoutedAnalyticsEvents`'s read methods — which are already wired in Phase B —
at the new remote methods.

### Errors — separate ADR

Prerequisite, in order: (1) introduce an `ErrorStore` trait over
`crates/temps-error-tracking`, which today has no storage abstraction at all;
(2) split an error into an immutable event stream and mutable local triage state;
(3) only then decide whether "which store served this" is a coherent question for
that domain. Until then the Errors page renders `this_instance`, which is true.

---

## References

- `crates/temps-otel/src/services/otel_service.rs` — `cloud_span()` (lines 71–91,
  the pseudonymised/stripped mirror projection) and `ingest_spans()`
  (lines ~432–455, local-first ordering, `link.record(mirror)` at ingest only)
- `crates/temps-otel/src/storage/mod.rs` — `OtelStorage`, `StorageResult<T>`
- `crates/temps-otel/src/handlers/query_handler.rs` — `TracesResponse`,
  `TraceSummariesResponse`, `SpanStatsResponse` (all already `{data, …}`
  envelopes, so `source` is additive)
- `crates/temps-analytics-events/src/services/traits.rs` — `AnalyticsEvents`, and
  the doc comment establishing the value-type query pattern
- `crates/temps-analytics-events/src/services/ch_backfill.rs` — the cursor-based,
  out-of-process, idempotent backfill pattern reused in Phase A
- `crates/temps-analytics-events/src/services/ch_fanout.rs` — the outbox fan-out
  pattern reused in Phase D
- `crates/temps-analytics-backend/src/traits.rs` and `src/error.rs` —
  `AnalyticsBackend`, and `BackendUnavailable { backend, reason }` as the
  precedent for reporting an unavailable source
- `crates/temps-error-tracking/src/**` — confirmed absence of any storage
  abstraction, the basis for the Errors scoping decision
- `crates/temps-cloud-client/src/lib.rs` — `CloudClient`, `bearer_auth` calling
  convention, `CloudError`, `REQUEST_TIMEOUT`
- `crates/temps-cloud-client/src/link.rs` — `CloudLink`, `configure()`,
  `status()`, `health()`, `pseudonymize_telemetry_id()`
- `crates/temps-cloud-client/src/status.rs` — `LinkStatus`, `MirrorHealth`
- `crates/temps-cloud-protocol/src/messages.rs` — `SpanRecord`, `TelemetryBatch`,
  `IngestAck`
- `crates/temps-cloud-protocol/src/lib.rs` — `Capability`, `Unavailable`,
  forward-compatible `#[serde(other)] Unknown` negotiation
- `web/src/components/global/TelemetrySourceBadge.tsx` — the fixed UI contract
- ADR-041 — cloud-primary telemetry writes; amends this ADR's local-first write
  premise, narrows §3's `auto` policy, and re-scopes Phase B
- ADR-027 — cross-project trace linking (why clear-text trace IDs matter)
- ADR-017 — split proxy/console process model, the general pattern this design's
  Cloud-side counterpart follows (implementation details in the private repo)
