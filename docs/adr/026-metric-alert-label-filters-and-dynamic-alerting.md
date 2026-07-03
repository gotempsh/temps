<!--
SCOPE: Observability — label-scoped queries and per-series alert firing for OTel metric dashboards and alert rules.
-->

# ADR-026: Label filters and per-series ("dynamic") alerting for OTel metrics

**Status:** Proposed
**Date:** 2026-07-02
**Author:** David Viejo

## Context

OTel metrics in Temps are stored with label/attribute dimensions (e.g. an
HTTP-style metric carrying `region`, `endpoint`, `status_code`). Dashboards can
already filter a tile to one label value — the "Label filters" panel (key
picker, `=` operator, value autocomplete sourced from real observed values) —
but two capabilities are missing:

**1. Alert rules cannot be scoped to a label value at all.** `metric_alert_rules`
(`temps-entities/src/metric_alert_rules.rs`) has no `label_filters` column.
`MetricAlertEvaluator::evaluate_rule` (`temps-otel/src/services/
metric_alert_evaluator.rs`) always builds a `MetricQuery` with
`label_filters: []` and `group_by: []` (defaulted), so a rule always evaluates
the metric in full aggregate. There's no way to say "alert if p95 latency for
`endpoint=/checkout` exceeds 500ms" — only "alert if p95 latency for the whole
metric exceeds 500ms."

**2. There's no way to see or alert on a metric broken down by a dimension.**
Dashboards can filter to a single label *value*, but can't render "one line per
distinct value of a label" (a breakdown/group-by view). Alerting has the
matching gap one level up: there's no way to say "alert separately for each
`endpoint` that breaches the threshold" — only a single aggregate alarm per
rule. The in-memory evaluator state (`breach_start: HashMap<i32, Instant>`,
`firing: HashMap<i32, i32>`) is keyed by `rule_id` alone, so a rule can have at
most one open alarm at a time.

**What already exists and can be reused (confirmed by reading the code, not
assumed):**
- `MetricQuery` (`temps-otel/src/types.rs`) already has `label_filters:
  Vec<(String, String)>` (AND-combined equality) and `group_by: Vec<String>` (one
  series per distinct label-set), both fully implemented against ClickHouse in
  `temps-otel/src/storage/clickhouse/mod.rs` using bound `attributes[?] = ?`
  parameters (no string interpolation of label values).
- `MetricBucket.series_key: Option<Vec<(String, String)>>` is already returned
  per bucket when `group_by` is set.
- `GET /otel/metrics/label-keys` and `GET /otel/metrics/label-values`
  (`temps-otel/src/handlers/query_handler.rs`) already back the dashboard
  autocomplete, and are already exposed in the generated SDK
  (`listMetricLabelKeysOptions`, `listMetricLabelValuesOptions`).
- The dashboard-side `LabelFilterBuilder`/`LabelFilterRow`/`SuggestCombobox`
  components exist, but only as local functions inside
  `web/src/pages/MetricsExplorer.tsx` — not extracted for reuse.
- `DashboardTile` (`temps-otel/src/services/dashboard_service.rs`) has no
  `label_filters` or `group_by` field yet, even though the query layer
  supports both — dashboards don't currently query with either.

So the backend query engine already supports everything a single filtered or
grouped query needs. The gap is entirely in (a) the alert rule schema and
evaluator not using that engine, and (b) no UI/API surface for either
dashboards or alerts to set a `group_by`.

**Note:** there is a second, older alerting system (`monitoring_alert_rules` /
`AlertEvaluator` in `temps-monitoring`, for infrastructure metrics). This ADR
is scoped entirely to the OTel metric stack (`metric_alert_rules` /
`MetricAlertEvaluator` / `temps-otel`); the infra-alert system is out of scope.

### Rejected alternatives
- **Build a separate query builder for alerts.** Alerts and dashboards would
  drift in filter semantics over time. `MetricQuery` already generalizes both;
  the fix is wiring alert rules to build one, not inventing a second one.
- **Model "alert per label value" as N separately-created alert rules (one per
  known value).** This is what users would have to do manually today. It
  doesn't scale to dynamic label sets (new endpoints, new regions appearing
  over time need a new rule created for each), and produces no single place to
  see "this whole family of per-endpoint alerts."
- **Unbounded per-series alerting with no cap.** Rejected outright — a
  high-cardinality label (e.g. anything approaching per-request identity)
  would produce unbounded alarm fan-out and notification storms. Any
  per-series design needs a hard cardinality cap from day one.

## Decision

**Extend the existing label-filter/group-by query engine to alert rules, and
add an opt-in "dynamic" mode where one rule can fire independently per label
combination, bounded by a cardinality cap.**

### Phase 1 — Label filters on alert rules (scoping, not fan-out)

- Add `label_filters` (jsonb, default `[]`) to `metric_alert_rules`. Format:
  `[["endpoint", "/checkout"], ["region", "eu-west"]]`, AND-combined. Empty = matches
  everything (today's behavior, so this is backward compatible).
- Thread `rule.label_filters` into every `MetricQuery` the evaluator builds
  (the current-value query, the anomaly baseline query, and the chart-SVG
  query for notification emails) in `metric_alert_evaluator.rs`.
- Extend `CreateMetricAlertRequest`/`UpdateMetricAlertRequest` and
  `OtelMetricAlertRuleResponse` with `label_filters`. Validate: max 10 pairs,
  keys pass the existing `validate_metric_name` check, values capped at 500
  chars.
- Extract `LabelFilterBuilder` (+ `LabelFilterRow`, `SuggestCombobox`) out of
  `MetricsExplorer.tsx` into `web/src/components/metrics/LabelFilterBuilder.tsx`
  so `MetricAlertForm.tsx` can reuse the exact same filter-building UI and
  autocomplete behavior dashboards already have.
- Give `DashboardTile` a `label_filters` field too (jsonb layout column, no
  migration needed) and wire `MetricTile.tsx` to pass it through — this closes
  the gap where the query engine supports filters but tiles don't use them yet.

Result of Phase 1: one alert can be scoped to one label value, and dashboard
tiles gain the same filtering the widget-config screenshot implied but tiles
don't yet actually apply.

### Phase 2 — Group-by / breakdown view on dashboards (read-only, no alerting yet)

- Add `group_by: Vec<String>` to `DashboardTile` (max 2 keys — more than that
  is unreadable in a chart). Wired straight to the existing `MetricQuery.
  group_by`.
- Chart renders one line per distinct `series_key`, capped at 20 series
  (sorted by value, "N more not shown" note beyond the cap), labeled by its
  joined `key=value` pairs.
- Tile editor gets a "Break down by" control (label-key multi-select, sourced
  from the existing label-key endpoint) with an inline cardinality warning
  when a candidate key has many distinct values.

### Phase 3 — Per-series ("dynamic") alerting

This is the "alert generally, but also about specific values" capability.

- Add `group_by` (jsonb `Vec<String>`), `dynamic_alerts` (bool, default
  `false`), and `max_series` (int, default 20, hard cap 100) to
  `metric_alert_rules`.
- When `dynamic_alerts = false` (default — today's behavior, unaffected): if
  `group_by` is also set, the evaluator still queries grouped but collapses to
  a single aggregate value via `max(|value|)` across series before evaluating
  — i.e. "alert me if **any** series breaches" without per-series fan-out.
  This gives the "alert generally" case a useful teeth-first default even
  without opting into dynamic mode.
- When `dynamic_alerts = true`: the evaluator runs one state machine per
  distinct `series_key`, keyed by `(rule_id, series_key)` rather than
  `rule_id` alone (implemented as a second pair of maps —
  `breach_start_series`/`firing_series` — so static rules keep their existing,
  untouched code path). Each breaching series fires its own alarm via the
  existing `AlarmService::fire_alarm`, with the series identified in the
  alarm title/message (`"{rule.name} [endpoint=/checkout, region=eu-west]"`)
  and in `metadata.series_key`. Series that stop appearing in query results,
  or that recover, resolve independently via `AlarmService::resolve_alarm`.
- **Cardinality guard:** if a query returns more distinct series than
  `max_series`, only the top `max_series` by `|value|` are evaluated/tracked;
  the rest are dropped from consideration for that tick and a warning is
  logged. Alarms are never created for series beyond the cap.
- **Notification grouping:** if more than `grouped_notification_threshold`
  (constant, default 5) series transition to firing in the same evaluation
  tick, send one grouped notification ("N series of `{metric}` breached
  simultaneously") instead of N individual notifications. Below the
  threshold, notify per-series as today.
- `last_state`/`last_value` on the rule row represent the aggregate view
  ("firing" if any series is firing; `last_value` = max breaching value)
  rather than per-series detail — full per-series state is exposed via a new
  `firing_series: Vec<FiringSeriesEntry>` field on the rule response,
  snapshotted from the evaluator's in-memory firing map at read time.
- Anomaly detection (`detection_kind = 'anomaly'`) is **not** combined with
  `dynamic_alerts` in this phase — the handler rejects that combination with
  400. Per-series baselines multiply the baseline-cache/query cost by series
  count and need their own design pass once real usage data exists.

### Phase 4 — UI

- `MetricAlertForm.tsx`: add the (now-shared) label filter section from Phase
  1, plus a "Group by / Alert per series" section — label-key multi-select
  (max 2), an "Alert per series" toggle (`dynamic_alerts`), and a "max series
  to track" number input (1–100, default 20) shown only when the toggle is on.
- Alert detail/list view: when a rule is dynamic and has open series, render a
  "Firing instances" table (series label, current value, linked alarm) so
  users can see and independently acknowledge/resolve each breaching series,
  with a "showing N of M (cap exceeded)" note when applicable.
- `MetricsExplorer` per-metric alert-status tooltip: for dynamic rules, show
  "N of M series breaching" instead of a flat firing/ok state.

## Consequences

### Positive
- Alerts and dashboards converge on one label-filter/group-by query model
  instead of alerts staying permanently aggregate-only.
- Users get both ends of the ask: a single aggregate alert ("tell me if
  anything is wrong") and, opt-in, per-value alerts ("tell me specifically
  which endpoint/region is wrong") from the same rule definition, without
  hand-creating one rule per known label value.
- All schema changes are additive (`#[serde(default)]` / column defaults) —
  existing rules and dashboard tiles parse unchanged and keep current
  behavior with `label_filters = []`, `group_by = []`, `dynamic_alerts =
  false`.
- Reuses proven, already-shipped plumbing: `MetricQuery` filtering,
  `AlarmService` fire/resolve, and the dashboard label autocomplete API — this
  is largely wiring, not new infrastructure.

### Negative / risks
- **Cardinality is the main operational risk.** A label with high cardinality
  (anything approaching per-entity/per-request identity) used as `group_by`
  with `dynamic_alerts = true` can still produce many simultaneous alarms even
  under the cap (up to `max_series`, up to 100). The cap bounds worst case but
  doesn't prevent a poorly-chosen `group_by` from being noisy — this is
  primarily a UX/guidance problem (advisory warning at rule-creation time),
  not something the backend can fully prevent.
- In-memory per-series firing state (`breach_start_series`/`firing_series`) is
  lost on restart, same as today's aggregate state — for dynamic rules this
  means all currently-open per-series alarms need their `for_duration_secs`
  timer to re-accumulate after a restart. Should be mitigated by reloading
  open alarms for dynamic rules from the `alarms` table on evaluator startup
  (mirroring the pattern `AlertEvaluator` in `temps-monitoring` already uses).
- `last_state`/`last_value` on the rule row becomes a lossy aggregate summary
  once a rule is dynamic (can't represent "3 of 5 series firing" in two
  scalar columns) — accepted for Phase 3, with a `series_states` jsonb column
  as a possible Phase 5 follow-up if the aggregate view proves insufficient.
- `series_key` values are user/application-originated (they come from
  whatever the instrumented app sent as label values) and must be treated as
  untrusted wherever they're rendered — chart SVG generation already has an
  `svg_escape` helper in `metric_alert_evaluator.rs` that must be applied to
  series labels before embedding them in notification charts. Flag this
  specifically for `security-auditor` review before Phase 3 ships.

### Neutral
- The two-map (`breach_start`/`firing` vs. `breach_start_series`/
  `firing_series`) approach means static and dynamic rules run through
  slightly different evaluator code paths rather than one unified path keyed
  by an always-present series key. Simpler to reason about and lower risk to
  existing (static) rules than unifying now; can be revisited if the
  duplication becomes a maintenance burden.

## Phased plan

1. **Phase 1:** `label_filters` column + entity + handler/service validation +
   evaluator wiring (3 `MetricQuery` sites) + extracted `LabelFilterBuilder` +
   `MetricAlertForm` filter section + `DashboardTile.label_filters` +
   `MetricTile` wiring.
2. **Phase 2:** `DashboardTile.group_by` + multi-series chart rendering + tile
   editor "break down by" control + cardinality warning.
3. **Phase 3:** `group_by`/`dynamic_alerts`/`max_series` columns + per-series
   evaluator state machine + cardinality cap + grouped notifications +
   `firing_series` response field + startup reload of open dynamic alarms.
4. **Phase 4:** alert form dynamic-alert controls + "Firing instances" UI +
   per-metric alert-status tooltip update.

## Open questions
- Exact default/max for `grouped_notification_threshold` — start as a
  hardcoded constant (5); promote to a per-rule or per-project column only if
  operators ask for control over it.
- Whether `series_states` (full per-series state as a jsonb column, superseding
  the lossy `last_state`/`last_value` aggregate) is worth doing in Phase 3 or
  deferred — leaning deferred until the "Firing instances" UI (which reads
  from the in-memory snapshot, not the row) shows it's actually needed for
  something the row itself must expose (e.g. externally-polled integrations
  that only read the rule row, not a sub-resource).
- Whether advisory cardinality warnings at rule-creation time (querying
  `list_metric_label_values` for the chosen `group_by` key and warning if the
  observed distinct count is high) should block creation or only warn —
  proposed: warn only (via a `warnings: Vec<String>` field on the create
  response), since the label set observed at creation time may not reflect
  runtime reality.
- Anomaly detection + `dynamic_alerts` combination: revisit once Phase 3 has
  real usage data on per-series query/baseline cost.

## References
- `crates/temps-entities/src/metric_alert_rules.rs`
- `crates/temps-otel/src/services/{metric_alert_evaluator,metric_alert_service,dashboard_service}.rs`
- `crates/temps-otel/src/{types.rs,handlers/{metric_alert_handler,query_handler}.rs}`
- `crates/temps-otel/src/storage/clickhouse/mod.rs` (label_filters/group_by query implementation)
- `web/src/pages/{MetricsExplorer,MetricAlertForm,DashboardView}.tsx`
- `web/src/components/metrics/MetricTile.tsx`
- Related: ADR-021 (humanized alert notifications), ADR-025 (unified alarm log — per-series alarms still flow through the same `alarms` table and `AlarmService`)
