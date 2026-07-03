---
title: "ADR-027: Cross-project trace linking"
status: Proposed
date: 2026-07-02
author: David Viejo
---

# ADR-027: Cross-project trace linking

**Status:** Proposed
**Date:** 2026-07-02
**Author:** David Viejo

## Context

### The fragmentation problem

Temps records OpenTelemetry spans with a mandatory `project_id` derived exclusively from the ingest credential — never from the OTLP payload. Two token types are accepted on the trace ingest path:

- **API keys (`tk_`)**: require an explicit `X-Temps-Project-Id` header (`ingest/auth.rs:190-194`).
- **Deployment tokens (`dt_`)**: carry `project_id` from the database record at issuance (`ingest/auth.rs:314-318`).

Service tokens (`si_`, role `metrics_ingest`) cannot ingest traces. `do_ingest_traces` calls `resolve_ingest_context`, which calls `authenticate()` — a function that accepts only `tk_` and `dt_` prefixes. `si_` tokens route exclusively to `do_ingest_service_metrics` and are irrelevant to this ADR.

This ingest-time binding is the security boundary and it is correct. The consequence is that when two Temps-deployed applications call each other over HTTP with W3C `traceparent` propagation, their spans share an identical 128-bit `trace_id` but land under different `project_id` values in storage.

Every trace query pairs `trace_id` with `project_id` as a hard first filter. Both storage backends enforce this:

- **TimescaleDB** (`timescaledb.rs`, multiple query sites): `WHERE project_id = $N` is the first parameterized predicate before `trace_id`, timestamps, or any optional filter. The `otel_spans` hypertable is indexed on `(project_id, trace_id, span_id)`.
- **ClickHouse** (ADR-016, `clickhouse/mod.rs`, multiple query sites): `WHERE project_id = ? AND trace_id = ?` is the leading clause. The `spans` table has `ORDER BY (project_id, trace_id, span_id)` and the `trace_summaries_mv` AggregatingMergeTree has `ORDER BY (project_id, trace_id)` — the sort key cannot be efficiently scanned across projects.

The `OtelStorage` trait (`storage/mod.rs`) exposes no method that accepts `trace_id` without `project_id`. `get_trace(project_id, trace_id)` is the only trace-detail path. Every HTTP handler applies `permission_guard!(auth, OtelRead)` then `project_scope_guard!(auth, project_id)` before calling into the service layer. The frontend (`TraceDetail.tsx`, `TracesList.tsx`) receives `project` as a required prop and passes it as a required query/path parameter to every SDK call; there is no global trace route.

The result: a distributed trace spanning two projects appears as two unrelated, incomplete waterfalls with no navigational link between them. A latency spike in a downstream service is invisible to the user viewing the upstream project's trace.

### Do cross-project traces occur today?

Not automatically. The per-deployment OTel sidecar (`sidecar/mod.rs:123-129`) injects `OTEL_EXPORTER_OTLP_ENDPOINT` — a standard OpenTelemetry SDK environment variable — pointing to the project's own collector, configured with an API key scoped to that single project. No `traceparent` header is injected into outbound HTTP requests by Temps; that is the instrumented application's responsibility. Cross-project propagation therefore requires an explicit developer action: configure the application SDK to read and forward the `traceparent` header on calls to services in other Temps projects.

This is rare today but is the correct behaviour per W3C Trace Context Level 2 and is the standard expectation in distributed-system observability. As usage of multi-project deployments grows the problem becomes more acute.

### Current auth model summary

`AuthContext::is_scoped_to_project()` (`context.rs:323`) returns `true` for any `project_id` when the caller is a human user, API key, or CLI session — "Not a deployment token… no per-project confinement at this layer" (comment at `context.rs:310-319`). The `project_scope_guard!` macro is therefore a no-op for these callers. This is the intentional OSS model: any holder of `OtelRead` can query any project's telemetry. Per-project user ACLs are an EE/RBAC concern not present in the OSS codebase.

The established precedent for cross-project endpoints is `list_all_conversations` (`temps-ai-chat/handlers.rs:478`), which returns conversations from every project, guarded by `permission_guard!(auth, ProjectsRead)` followed by `deny_deployment_token!(auth)`. This ADR follows the same two-guard pattern, using `OtelRead` as the domain-appropriate permission.

### Alternatives considered

**Option A — Full unified waterfall from day one.** Assemble a merged, cross-project span tree via a fan-out to each contributing project's `get_trace()` at query time. This is the highest-value end state, but it requires: (1) a discovery index to know which projects hold spans for a given `trace_id` before any fan-out can be issued; (2) a new `CrossProjectTraceDetail` UI component that handles multi-project span annotation, colour-coding, and the waterfall timeline; (3) resolving the "orphaned parent span" UX when only some projects' spans are available. Building the unified waterfall without first shipping the discovery index delays user value by 3–4 additional weeks. The index is the prerequisite for both the navigation and the unified view — shipping navigation first delivers immediate value.

**Option B — Per-project opt-in sharing with redacted tombstones.** Add a `cross_project_trace_sharing` boolean column to `projects` (default `false`); fan-out only to opted-in projects; redact non-sharing projects to an opaque tombstone. This is the most defensible model from a privacy standpoint in a multi-team shared self-hosted installation, but it introduces significant friction: a cross-project trace produces silent gaps until each project admin opts in. Since the OSS model already grants any `OtelRead`-holding user access to all projects' telemetry (and there is no per-project user membership system), opt-in is a safety valve for an EE/RBAC concern that does not yet exist. It is appropriate as a future hardening step (Phase 3) rather than a Phase 1 requirement.

**Option C — Pure navigation links only, no merged view ever.** Cheapest to build, but "click to switch project and lose context" is precisely the broken state users want fixed. Acceptable only as Phase 1, not as the final state.

**Selected approach — phased: discovery index in Phase 0, follow-navigation in Phase 1, unified waterfall in Phase 2.** Each phase is independently shippable and produces user value. Phase 0 is the prerequisite for Phases 1 and 2. Without the index, per-`trace_id` project discovery would require a full-table scan on both storage backends at query time; the index makes the lookup sub-millisecond and backend-agnostic.

## Decision

Implement cross-project trace linking in three phases. Phases 0 and 1 are committed and ship together in one PR. Phase 2 is the target end state, confirmed at Phase 1 review. Phase 3 is deferred hardening.

**Explicit scope boundary:** This ADR covers OTel span traces only. GenAI trace linking (`get_genai_trace_spans`, `get_genai_trace_events`) and cross-project metric or log correlation are explicitly out of scope and deferred to a future ADR.

### 1. Auth model

The cross-project trace endpoints enforce two guards, applied in this order:

1. `permission_guard!(auth, OtelRead)` — the same global permission required to query per-project traces.
2. `deny_deployment_token!(auth)` — machine credentials bound to a single project are categorically blocked. A deployment token must never discover another project's existence through a global endpoint, even when only project names (not spans) are returned.

Note: the `list_all_conversations` precedent applies `permission_guard!` before `deny_deployment_token!`. Both orderings are functionally correct (any caller must pass both). This ADR adopts the same order as the precedent for consistency.

No `project_scope_guard!` is applied to cross-project endpoints — that macro is a no-op for non-DT callers and would be misleading here. No new permission (`CrossProjectOtelRead` or similar) is introduced — that would be premature abstraction with no evidence of need.

**Visibility model in OSS:** Any human user or API key holding `OtelRead` can discover which projects share a `trace_id` and retrieve the merged span set from all of them. This is explicitly documented in the endpoint OpenAPI description and is consistent with the existing OSS model where `project_scope_guard!` is a no-op for users. Project names like `payment-service` or `auth-staging` reveal service topology; this is a documented, accepted trade-off in the OSS global-observability model, not a regression. Operators who consider this unacceptable in a multi-team deployment should wait for Phase 3 opt-out or enable EE RBAC.

When EE per-project user membership is added, a third check would go between the two existing guards — filtering the fan-out result to projects the caller is a member of — without changing the endpoint contract. The Phase 2 fan-out naturally supports this insertion point.

**Audit logging:** Every call to both new cross-project endpoints must emit an audit log record **before** the fan-out queries execute. The audit record must include: `user_id`, `trace_id`, and for the Phase 2 unified endpoint, the list of `project_id` values to be queried. Logging after the fact means failed fan-out attempts go unrecorded. The existing `audit_service` in `AppState` is used; audit failure is logged at ERROR level and does not fail the request.

Note: the existing per-project query handlers in `query_handler.rs` do not audit-log trace reads. These new endpoints add auditing because they expose cross-project data; back-filling audit logs on existing per-project handlers is out of scope.

### 2. Phase 0 — Postgres discovery index (prerequisite, ship first)

Add one Postgres control table. This is backend-agnostic: created regardless of whether the OTel backend is TimescaleDB (default) or ClickHouse (ADR-016 toggle). This follows ADR-016's core principle: mutable control-plane metadata belongs in Postgres; high-volume telemetry belongs in the selected backend.

```sql
-- Migration: m20261001_000001_cross_project_trace_refs.rs
CREATE TABLE cross_project_trace_refs (
    trace_id    TEXT        NOT NULL,
    project_id  INTEGER     NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    first_seen  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT cross_project_trace_refs_pkey PRIMARY KEY (trace_id, project_id)
);

CREATE INDEX cross_project_trace_refs_by_trace
    ON cross_project_trace_refs (trace_id, first_seen DESC);

CREATE INDEX cross_project_trace_refs_by_age
    ON cross_project_trace_refs (first_seen);
```

Sea-ORM entity: `cross_project_trace_refs::Model { trace_id: String, project_id: i32, first_seen: DateTimeUtc }` in `temps-entities`. The table is append-only after the initial insert; no `ActiveModel` mutations are needed. Project deletion cascades via `ON DELETE CASCADE`, automatically removing orphaned hint rows.

**Ingest hook:** In `do_ingest_traces` (`ingest_handler.rs`), after `otel_service.ingest_spans(spans).await?` succeeds, collect distinct `trace_id` values from the decoded batch (a `HashSet` dedup — typically one to three distinct values per OTLP request). Pass them through a bounded `tokio::sync::mpsc` channel (capacity 1,000) to a dedicated hint-writer task that issues a single multi-row `INSERT … ON CONFLICT DO NOTHING`. When the channel is full, the hint write is **dropped** (not back-pressured onto the ingest path) and a counter-metric is incremented. Errors from the channel send are warned and swallowed; hint loss is acceptable because: (a) the underlying span is still stored correctly; (b) a subsequent OTLP batch for the same `(trace_id, project_id)` pair will re-insert. The hint write never blocks the OTLP HTTP response.

The bounded channel (capacity 1,000) is the Phase 0 safety requirement. Unbounded `tokio::spawn` for hint writes is explicitly rejected: under sustained high ingest rates, queued tasks would exhaust memory. Dropping writes when the channel is full is the correct degradation — users may temporarily not see cross-project links, but span storage is unaffected.

`do_ingest_logs` and `do_ingest_metrics` are not modified — the discovery index is trace-only.

**TTL cleanup:** Add `Job::PruneStaleTraceHints` to the existing job scheduler, running daily:
```sql
DELETE FROM cross_project_trace_refs WHERE first_seen < NOW() - INTERVAL '90 days'
```
90 days matches the OTel retention in both the TimescaleDB hypertable retention policy and the ClickHouse `spans` table `TTL toDateTime(start_time) + INTERVAL 90 DAY` (ADR-016). **Coupling note:** if an operator changes the OTel backend TTL (e.g. to 30 days), the cleanup job interval must be updated in tandem. This is documented in the operator guide; there is no automatic sync mechanism.

**Size estimate:** Each row is approximately 50 bytes (32-char hex `trace_id` + `i32` + `timestamptz`). An installation generating one million distinct traces per day across ten projects with a 90-day window accumulates roughly 4.5 million rows — approximately 225 MB on a standard B-tree index. Cross-project propagation requires explicit SDK configuration by developers, so the table will be sparse on most installations. A primary-key lookup on `(trace_id)` plus a single Postgres JOIN is sub-millisecond at this row count; no hash partitioning is required at this scale. If row counts exceed 50 million (approximately 550K distinct cross-project traces per day across a 90-day window), converting to a TimescaleDB hypertable partitioned by `first_seen` is the Phase 3 follow-up.

### 3. Phase 1 — Follow navigation (cross-project banner and discovery endpoint)

**New endpoint — sibling project discovery:**

```
GET /otel/traces/cross-project/{trace_id}
```

Auth: `RequireAuth` + `permission_guard!(auth, OtelRead)` + `deny_deployment_token!(auth)`.

Path parameter: `trace_id: String` — validated against a 32-character lowercase hexadecimal regex before any database access, to prevent injection and garbage queries.

Query parameter: `exclude_project_id: Option<i32>` — the caller's own project, excluded from results so the UI does not surface a self-link.

Response 200:
```json
{
  "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736",
  "siblings": [
    { "project_id": 42, "project_name": "payment-service", "first_seen": "2026-07-01T14:23:11Z" }
  ]
}
```

An empty `siblings` array is the normal single-project case. The endpoint never returns 404 for an unknown `trace_id` — it returns an empty list, which the UI silently ignores. Project names are included in the response because they are the user-facing label for navigation. The topology-disclosure trade-off is documented in Section 1.

Implementation: `CrossProjectTraceService::find_sibling_projects` queries:
```sql
SELECT r.project_id, p.name AS project_name, r.first_seen
FROM cross_project_trace_refs r
JOIN projects p ON p.id = r.project_id
WHERE r.trace_id = $1
  AND ($2::integer IS NULL OR r.project_id != $2)
ORDER BY r.first_seen ASC
```
This is a primary-key lookup on `(trace_id)` plus one JOIN — sub-millisecond at any realistic table size. No change to the `OtelStorage` trait, `TimescaleDbStorage`, or `ClickHouseOtelStorage`.

**UI change in `TraceDetail.tsx`:**

After the primary `getTraceOptions` query resolves and spans are rendered, fire a secondary non-blocking `useQuery` using the generated `crossProjectTraceOptions` hook. The query is enabled only when `!!traceId && spans.length > 0` to avoid delaying the waterfall render. `retry: false` ensures a failure does not retry-spam and surfaces no error to the user.

When `siblings.length > 0`, render a dismissible info banner above the `SpanWaterfall`:

```tsx
<div className="flex items-center gap-2 rounded-md border border-blue-200 bg-blue-50 px-3 py-2 text-sm ...">
  <InfoIcon className="h-4 w-4 shrink-0" />
  <span>This trace also has spans in:</span>
  {siblings.map((s) => (
    <Link key={s.project_id} to={`/projects/${s.project_id}/observability/traces/${traceId}`}>
      {s.project_name}
    </Link>
  ))}
</div>
```

Navigating the link takes the user to the existing per-project `TraceDetail` for the sibling project, which enforces its own `project_scope_guard!` at the backend. Zero DOM nodes are rendered when `siblings` is empty.

**Stale hint UX (mandatory in Phase 1):** When a sibling banner link is clicked, the user lands in the sibling project's `TraceDetail`. If the sibling's spans have expired (hint row exists but `get_trace` returns an empty vec), `TraceDetail.tsx` must display a "Spans for this trace are not available or have expired" message rather than an empty waterfall. This empty-trace state is a Phase 1 requirement, not optional. The current component's behaviour when a valid `trace_id` returns zero spans must be verified and, if undefined, added as part of Phase 1.

### 4. Phase 2 — Unified cross-project waterfall (planned, confirm at Phase 1 review)

**Two new storage trait methods added to `OtelStorage`:**

```rust
async fn find_trace_projects(
    &self,
    trace_id: &str,
) -> StorageResult<Vec<TraceProjectEntry>>;

async fn get_trace_across_projects(
    &self,
    trace_id: &str,
    project_ids: &[i32],
) -> StorageResult<Vec<SpanRecord>>;
```

`get_trace_across_projects` has a default trait implementation that fans out to parallel `get_trace(project_id, trace_id)` calls via `futures::future::join_all`, capped at 20 projects and 10,000 total spans. Only `find_trace_projects` requires backend-specific implementations.

For both TimescaleDB and ClickHouse: `find_trace_projects` reads from the `cross_project_trace_refs` Postgres control table — backend-agnostic. `get_trace()` already queries each backend's spans table with `WHERE project_id = ? AND trace_id = ?`, and the fan-out default implementation reuses this path without any schema change. The `trace_summaries_mv` AggregatingMergeTree (`ORDER BY (project_id, trace_id)`) is intentionally not used for cross-project assembly: its sort key cannot be efficiently scanned across projects, and the Postgres `cross_project_trace_refs` table provides O(1) project discovery via primary-key lookup instead.

**Truncation strategy:** When the 20-project or 10,000-span cap is hit, the fan-out drops projects by `first_seen DESC` (most recent first; oldest projects are dropped first). Within each retained project, all available spans are included up to the per-project slot of `10,000 / retained_project_count`. This preserves root-to-leaf span chains within included projects. Projects that were excluded contribute a `truncated_projects: Vec<i32>` field in the response root; `truncated: bool` signals any truncation. The caps are not yet server-configurable; that is deferred to post-Phase 2 based on real-world usage patterns.

**New endpoint:**

```
GET /otel/global/traces/{trace_id}
```

Auth: `permission_guard!(auth, OtelRead)` + `deny_deployment_token!(auth)`.

`CrossProjectTraceService::get_unified_trace` orchestrates: (1) audit log written **before** fan-out begins; (2) index lookup via `find_trace_projects`; (3) parallel `get_trace_across_projects` capped at 20 projects and 10,000 spans; (4) project-name resolution via Postgres `projects` table; (5) annotation of each `SpanRecord` with `project_id` + `project_name`; (6) merge sorted by `start_time ASC`.

Response type `UnifiedTrace` carries: `trace_id`, `projects: Vec<ProjectRef>`, `spans: Vec<AnnotatedSpanRecord>`, `start_time`, `end_time`, `total_duration_ms`, `span_count`, `error_count`, `truncated: bool`, `truncated_projects: Vec<i32>`.

**UI changes for Phase 2:**

- Extract `buildSpanTree()` and `flattenTree()` from `TraceDetail.tsx` to `web/src/utils/spanTree.ts` (shared utility).
- New `CrossProjectTraceDetail.tsx`: route `/traces/global/:traceId`, fetches `GET /otel/global/traces/{trace_id}`, renders spans through the shared `buildSpanTree + SpanWaterfall`, adds a project badge column to each row (deterministic colour hash of `project_id`), and a project legend above the waterfall. If `truncated: true`, renders a callout listing `truncated_projects`. Span detail panel adds a "View in project" link to `/projects/{project_id}/observability/traces/{trace_id}`.
- Update the Phase 1 sibling banner to also link to `/traces/global/{traceId}` in addition to individual per-project links.

**W3C parent pointer semantics across projects:** A span in Project B whose `parent_span_id` points to a span in Project A has the correct `parent_span_id` value in storage verbatim (confirmed in `decode.rs`). When the merged flat list is passed to `buildSpanTree()`, parent references resolve correctly so long as both projects' spans are included. Spans whose `parent_span_id` is non-empty but absent from the merged result set (parent expired, in a non-queried project, or in a truncated project) are treated as root nodes and annotated in the UI as "parent span not available in this view".

### 5. Phase 3 — Hardening and opt-out (deferred)

- Add `cross_project_trace_sharing: bool NOT NULL DEFAULT true` column to `projects` via Sea-ORM migration. `CrossProjectTraceService` filters out projects where this is `false` before issuing fan-out calls; those projects contribute an opaque `has_redacted_spans: bool` flag to the response root rather than a per-project tombstone (to avoid count-based topology inference). Toggle exposed via `PATCH /api/projects/{id}` and project settings UI. Default `true` is consistent with the current OSS global-observability model. Operators who want private-by-default should flip the column default; this is a migration-level change that can be revisited when EE RBAC lands.
- Convert `cross_project_trace_refs` to a TimescaleDB hypertable (partitioned by `first_seen`) if row counts exceed 50 million.
- Cross-project GenAI trace linking (extends the same index and fan-out to `get_genai_trace_spans`).
- Optional back-fill job: batch `SELECT DISTINCT trace_id, project_id FROM otel_spans` (or ClickHouse equivalent) into `cross_project_trace_refs ON CONFLICT DO NOTHING`. Operator-triggered via an admin endpoint with progress reporting, rate-limited, not an automatic migration.

## Consequences

### Positive

- Developers can follow a distributed trace across projects without manually noting a `trace_id` and switching projects. The Phase 1 banner surfaces the link automatically.
- The discovery index (`cross_project_trace_refs`) is the prerequisite for the Phase 2 unified waterfall. Phase 1 ships the infrastructure; Phase 2 builds on top with no schema changes.
- The index is backend-agnostic: it lives in Postgres regardless of whether the OTel backend is TimescaleDB or ClickHouse (ADR-016 toggle). Neither `TimescaleDbStorage` nor `ClickHouseOtelStorage` requires changes for Phases 0 and 1.
- All existing per-project endpoints, storage schemas, query paths, and SDK types are unchanged. No breaking changes to existing SDK consumers.
- Auth model is correct by construction: the Phase 1 discovery endpoint exposes only project names (not spans); span access in the sibling project still passes `project_scope_guard!` at that project's `get_trace` handler.
- The two-guard pattern (`permission_guard! + deny_deployment_token!`) is validated in production via `list_all_conversations`.
- Graceful degradation everywhere: hint loss on ingest is warned and swallowed; a failed cross-project discovery query renders nothing in the UI; the stale-hint UX is explicitly handled (see Phase 1 requirements).

### Negative

- **Hints can go stale.** When OTel TTL expires spans after 90 days, the corresponding `cross_project_trace_refs` row persists until the daily cleanup job runs. A user clicking a Phase 1 banner link into Project B may find the "Spans expired" message. This is addressed by the mandatory empty-trace handler in Phase 1.
- **No back-population of existing traces.** Spans ingested before this feature ships have no hint rows. Operators with existing multi-project traces will see no cross-project links until new traces arrive. The operator guide must document this gap; back-fill is an optional Phase 3 operator-triggered job, not automatic.
- **Discoverability depends on developer instrumentation.** The Temps sidecar does not auto-inject W3C `traceparent` into outbound HTTP requests. Cross-project trace propagation requires an explicit SDK-level configuration by the application developer. Many users will never see the banner. This is a developer education gap documented in the onboarding guide and SDK documentation.
- **Ingest overhead on all installations.** The bounded-channel hint write runs for every trace ingest batch on every installation, even single-project setups where it always produces duplicate-conflict no-ops. The channel capacity (1,000) and drop behaviour prevent memory exhaustion. At extreme ingest rates, the drop counter serves as the signal that hint writes are being shed.
- **Phase 2 fan-out caps are not configurable.** The 20-project and 10,000-span caps are hard-coded in the initial implementation. A truncated waterfall loses observability if the `truncated` flag is not prominently surfaced; the UI must show `truncated_projects` explicitly. Making the caps server-configurable is deferred to post-Phase 2.

### Risks

- **Project-name topology disclosure.** Any `OtelRead`-holding user can discover which projects share a `trace_id` through the Phase 1 discovery endpoint. Project names (e.g. `payment-service`, `auth-staging`) reveal internal service topology. This is consistent with the pre-existing OSS model — not a regression — but it may surprise operators running multi-team self-hosted installations. Documentation must explicitly state this at the endpoint level and in the operator guide. Phase 3 opt-out mitigates this for installations that want it.
- **TTL coupling.** The cleanup job interval (90 days) is matched to the OTel backend retention (ADR-016). If an operator changes the OTel backend TTL to a shorter value (e.g. 30 days), stale hints will point to expired spans for up to 90 days. The operator guide must document that the cleanup schedule must be kept in sync with OTel backend TTL. There is no automatic enforcement.
- **RBAC migration path.** When EE per-project membership is added, the current `OtelRead` global permission exposes cross-project data to users who may only be members of some projects. The guard-insertion point is documented in Section 1 (a third check between `deny_deployment_token!` and the fan-out). No existing endpoint contract changes.
- **GenAI cross-project traces not covered.** `get_genai_trace_spans` and `get_genai_trace_events` are project-scoped and not addressed by this ADR. A cross-project GenAI trace link banner would show inconsistent behaviour compared to the standard trace banner (one works, one does not). The operator guide must document this asymmetry until Phase 3 closes the gap.

### Neutral

- No change for single-project installations beyond the bounded-channel hint write, which is always a no-op at the Postgres primary key after the first occurrence.
- Insights, health summaries, and quota rows (ADR-016 control-store) are unaffected.
- The `trace_summaries_mv` AggregatingMergeTree (ADR-016) is not used for cross-project assembly and is not modified.
- Metrics and logs remain project-scoped in all phases; cross-project metric correlation (e.g. metric spikes across services) is deferred to Phase 4 or later if demand justifies it.

## Phased plan

**Phase 0 — Discovery index (~2 days, ships first):**
1. Sea-ORM migration `m20261001_000001_cross_project_trace_refs.rs`: create `cross_project_trace_refs` + two secondary indexes.
2. `temps-entities`: `cross_project_trace_refs::Model` entity.
3. `temps-otel/src/cross_project/service.rs`: `CrossProjectTraceService` with `record_hint(trace_ids: HashSet<String>, project_id: i32)` (multi-row `INSERT ON CONFLICT DO NOTHING`) and `find_sibling_projects(trace_id, exclude_project_id)`.
4. `temps-otel/plugin.rs`: spawn hint-writer task consuming a bounded `mpsc::channel(1000)`. Register `CrossProjectTraceService`; inject `Arc<DatabaseConnection>` (already available in `OtelAppState`).
5. `ingest_handler.rs`: after `ingest_spans` succeeds, dedup `trace_id`s from the batch, send on the bounded channel (non-blocking; drop and warn if full).
6. Job scheduler: add `Job::PruneStaleTraceHints` variant running the 90-day `DELETE`.

**Phase 1 — Follow navigation (~2 days, requires Phase 0):**
1. `query_handler.rs`: add `GET /otel/traces/cross-project/{trace_id}` handler with `permission_guard!(auth, OtelRead)` + `deny_deployment_token!(auth)`, hex-format validation on `trace_id`, `exclude_project_id` query param. Audit log written before query.
2. OpenAPI schema: `CrossProjectTraceResponse`, `CrossProjectSiblingRef`. Register in `ApiDoc`.
3. SDK regen (`bun run openapi-ts` after API key mint per `reference_sdk_codegen_api_key.md`).
4. `TraceDetail.tsx`: `crossProjectTraceOptions` query (enabled after spans load, `retry: false`), dismissible info banner with sibling project links.
5. `TraceDetail.tsx`: add "Spans for this trace are not available or have expired" empty-trace state for the case where a valid `trace_id` returns zero spans.
6. Integration test: ingest spans for one `trace_id` under two project IDs, assert sibling endpoint returns the second project, assert deployment token receives 403, assert per-project endpoints are unaffected, assert empty-trace state renders correctly for expired sibling.

**Phase 2 — Unified waterfall (~3–4 days, confirm at Phase 1 review):**
1. `OtelStorage` trait: add `find_trace_projects` (required, per-backend) and `get_trace_across_projects` (default impl via `join_all` over `get_trace`, capped at 20 projects and 10,000 spans with per-project slot allocation).
2. Implement `find_trace_projects` in `TimescaleDbStorage` and `ClickHouseOtelStorage` (both read Postgres `cross_project_trace_refs`).
3. `CrossProjectTraceService::get_unified_trace`: audit log first → index lookup → parallel fan-out → project-name annotation → truncation handling.
4. `query_handler.rs`: add `GET /otel/global/traces/{trace_id}` handler.
5. OpenAPI: `UnifiedTrace`, `AnnotatedSpanRecord`, `ProjectRef`. SDK regen.
6. `web/src/utils/spanTree.ts`: extract `buildSpanTree` + `flattenTree` from `TraceDetail.tsx`.
7. `CrossProjectTraceDetail.tsx`: new component at route `/traces/global/:traceId`, project badge column, project legend, truncation callout listing `truncated_projects`, "parent span not available in this view" annotation, "View in project" link in span detail.
8. Update Phase 1 banner to also link to `/traces/global/{traceId}`.

**Phase 3 — Hardening and opt-out (deferred, no timeline committed):**
- See Section 5 above.

## Open questions

1. **Back-fill gating.** Should Phase 1 ship with an optional admin CLI subcommand for back-filling `cross_project_trace_refs` from existing `otel_spans`? The back-fill is expensive on large installs (full `otel_spans` hypertable scan) but immediately valuable for teams with existing multi-project traces. Recommendation: make it opt-in via an admin endpoint with progress reporting, not an automatic migration, and ship it in Phase 3.
2. **Phase 3 opt-out default.** Should `cross_project_trace_sharing` default `true` (all projects visible, opt-out) or `false` (private by default, opt-in)? `true` is consistent with the current OSS global-observability model; `false` is safer for future multi-team deployments. Recommendation: `true` for Phase 3 with documentation, revisit when EE RBAC lands.
3. **Phase 2 fan-out cap configurability.** Should the 20-project and 10,000-span caps be configurable per-installation via `server_config` before Phase 2 ships, or added post-Phase 2 based on real-world usage patterns? Recommendation: add post-Phase 2; hard-code for initial release, then instrument the drop counter to drive the decision.

## Security review (2026-07-03)

A `security-auditor` pass on the implementation returned **APPROVED-WITH-NITS** (no Critical/High). The auth model was confirmed correct: both endpoints enforce `permission_guard!(OtelRead)` then `deny_deployment_token!` before any DB access, all SQL is parameterized, and the ingest hint channel is bounded/`try_send` (DoS-safe). Outcomes:

- **Existence disclosure via `has_redacted_spans` (MEDIUM — accepted, documented here).** When any contributing project has `cross_project_trace_sharing = false`, its name and spans are fully suppressed, but the response's `has_redacted_spans: true` flag reveals that *some* opted-out project participated in the trace. This is an accepted trade-off: the flag is required for the "some spans are hidden" UX, and existence-without-identity is consistent with the OSS global-observability posture (§1). Opt-out guarantees *identity + data* privacy, not *existence* privacy. A future EE RBAC mode may gate the flag behind org-admin.
- **Fan-out amplification (MEDIUM — follow-up).** `get_unified_trace` issues up to 20 parallel `get_trace` calls with no per-user/endpoint rate limit beyond global middleware, so a looping `OtelRead` caller can cause 20× query amplification. Tracked as a follow-up: add per-user rate limiting on `/otel/global/traces/{trace_id}` (and the sibling endpoint) before it is exposed on a public listener. The amplification is bounded (≤20 projects/≤10k spans per request).
- **Ingest hint validation (LOW — fixed in this change).** `do_ingest_traces` now `filter`s hint `trace_id`s through `is_valid_trace_id` before they reach `cross_project_trace_refs`, eliminating a storage-pollution vector.
- **Audit-failure observability (LOW — follow-up).** Cross-project read audit failures are `warn!`-logged and non-fatal (standard pattern); add a counter-metric so unaudited windows can alert.

## References

- [ADR-016: ClickHouse as the OTel telemetry backend](016-clickhouse-traces-backend.md) — spans schema `ORDER BY (project_id, trace_id, span_id)`, `trace_summaries_mv`, control-state-in-Postgres principle; ClickHouse `spans` `TTL toDateTime(start_time) + INTERVAL 90 DAY`.
- [ADR-012: ClickHouse as an external analytics backend](012-clickhouse-analytics-backend.md) — config toggle, CH client, migration runner; principle that relational/mutable bits stay in Postgres.
- [ADR-023: AI debugging conversations](023-ai-debugging-conversations.md) — global cross-project endpoint precedent (`list_all_conversations` at `temps-ai-chat/handlers.rs:478`); `permission_guard!(auth, ProjectsRead)` + `deny_deployment_token!` pattern for cross-project access.
- `crates/temps-otel/src/ingest/auth.rs` — token-type to `project_id` mapping; `authenticate()` handles only `tk_` and `dt_` (trace ingest path); `authenticate_any()` adds `si_` routing for metrics only.
- `crates/temps-otel/src/ingest/decode.rs` — `trace_id` and `span_id` hex-encoded verbatim from OTLP protobuf; `parent_span_id` preserved as-is.
- `crates/temps-otel/src/handlers/ingest_handler.rs` — `do_ingest_traces` and `resolve_ingest_context`; ingest stamps `ctx.project_id` onto all spans; `do_ingest_service_metrics` is the only path reachable by `si_` tokens.
- `crates/temps-otel/src/storage/mod.rs` — `OtelStorage` trait; `get_trace(project_id, trace_id)` signature; no cross-project methods exist prior to this ADR.
- `crates/temps-otel/src/storage/timescaledb.rs` — `get_trace` implementation: `WHERE project_id=$1 AND trace_id=$2`.
- `crates/temps-otel/src/storage/clickhouse/mod.rs` — ClickHouse `get_trace` implementation: `WHERE project_id = ? AND trace_id = ?`.
- `crates/temps-otel/src/sidecar/mod.rs:123-129` — per-deployment sidecar injects `OTEL_EXPORTER_OTLP_ENDPOINT` (standard OTel SDK env var) pointing to the project's own collector; no cross-project `traceparent` injection.
- `crates/temps-auth/src/context.rs:323` — `is_scoped_to_project()` returns `true` for any project for non-deployment-token callers.
- `crates/temps-auth/src/permission_guard.rs` — `deny_deployment_token!`, `permission_guard!`, `project_scope_guard!` macro definitions.
- `web/src/pages/TraceDetail.tsx` — `buildSpanTree()`, `flattenTree()`, `SpanWaterfall` component; single-project waterfall assumptions; empty-trace state behaviour to be verified in Phase 1.
- `web/src/pages/TracesList.tsx` — `queryTraceSummariesOptions` with required `project_id`; no cross-project list path.
