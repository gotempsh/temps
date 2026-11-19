# ADR 074: What error tracking and performance are missing to match Sentry

## Status

Proposed — 2026-11-19

Related: ADR 016 (ClickHouse traces backend), ADR 027 (cross-project trace
linking), ADR 033 (release identity for symbolication), ADR 054 (autofix
incidents in workspaces).

One of four gap ADRs (071 Plausible, 072 PostHog, 073 Better Stack,
074 Sentry). The target is 90% of what a typical user relies on.

**This is the lowest priority of the four.** Ingestion is already compatible
with the official Sentry SDKs, and the daily "an error happened, show me the
stack, let me resolve it" loop works. What is missing is mostly depth, and
only D1 to D3 are worth scheduling now.

## Context

### What exists

The signal-by-signal contract is in `docs/features/error-tracking/page.mdx`
("Signal and product coverage"). In short:

| Area | Where | Today |
|---|---|---|
| DSN ingest from official SDKs: errors, transactions and spans, logs, metrics, web replay, check-ins, feedback, CSP and NEL reports, client reports | `crates/temps-error-tracking/src/sentry/` | Yes |
| Grouping by fingerprint, explicit SDK fingerprints | `error_groups`, `sentry/mapper.rs` | Yes |
| Source maps by Debug ID, late symbolication | `services/source_map_service/` | Yes |
| Group status and assignee | `error_groups.status`, `error_groups.assigned_to` | `unresolved`, `resolved`, `ignored`, `assigned`; assignee is free text |
| Regression detection and alert rules | `services/error_alert_service.rs`, `error_alert_rules` | Yes |
| Releases: counts per signal and example links | `/projects/{project_id}/telemetry-releases` | Correlation only |
| Traces: list, detail, cross-project | `crates/temps-otel` | Yes |
| Operations: p50, p95, p99, tail ratio and error rate per span name | `/otel/span-stats`, `web/src/pages/TraceOperations.tsx` | Yes |
| Anomaly insights correlated with deployments | `crates/temps-otel/src/anomaly/insights.rs` | Yes |
| Web vitals | `crates/temps-analytics-performance` | Yes |
| Query performance for managed databases | `web/src/pages/ServiceQueryPerformance.tsx` | Yes |
| Fix with agent | `crates/temps-error-tracking`, ADR 054 | Yes |

### What the same document lists as outstanding

Release health, profiles, attachments, and minidumps and native crashes. The
envelope reader recognises `session`, `sessions`, `profile`, `profile_chunk`
and `attachment` items (`sentry/envelope.rs`) and skips them.

## Decision

The gaps, most important first.

### D1. Issue workflow

**Missing.** Triage is one group at a time with four states.

1. **Bulk actions.** The API and CLI change one group per call; the document
   says so. Resolving forty groups after a bad deploy is forty calls.
2. **Merge and split.** Two groups that are one bug cannot be combined; a
   group that hides two bugs cannot be divided. The only tool is an SDK
   fingerprint, which changes future events only.
3. **Ignore until.** `ignored` is permanent. No "until it happens 100 more
   times", "until it affects 10 users" or "for a week".
4. **Server-side inbound filters.** Noise (browser extensions, known crawlers,
   a message pattern, an old release) can only be dropped in the SDK with
   `ignoreErrors`. The server stores everything it is sent.
5. **Resolve in the next release.** A group cannot be marked "fixed in the
   next release" and reopened only if it recurs in that release or later.
6. **Assignment that means something.** `assigned_to` is a string. It is not a
   user or team, so there is no "assigned to me" view and no notification on
   assignment. `assigned` is also a status, so an assigned group cannot be
   resolved or ignored without losing its assignee state.
7. **Comments and an activity log** on a group.
8. **Saved searches** over groups.

**Shape.**

- Bulk update takes a list of group ids or a filter, capped per request and
  audit-logged, following the bulk routes alarms already have
  (`/projects/{project_id}/alarms/bulk`).
- Merge is an alias: a table mapping a fingerprint to the surviving group, so
  future events land there and counts move in one transaction. Unmerge removes
  the alias; events keep their original fingerprint, so nothing is lost.
- Ignore conditions are columns on the group (until a time, until a count,
  until a user count), checked where the group counter is already updated.
- Inbound filters are per-project rules evaluated at ingest, before storage.
  They are a bounded, precompiled list held in memory and swapped on change,
  so the ingest path takes no lock and makes no database call per event.
  Dropped events are counted per rule and shown, so a filter that is eating
  real errors is visible.
- Assignee becomes a reference to a user or a team, and stops being a status.
- "Resolve in the next release" needs releases to be ordered. ADR 033 gives a
  release an identity; the order comes from the deployment that first carried
  it, which Temps knows because it performed the deployment.

### D2. Performance product

**Missing.** Latency per operation exists. What turns it from a table into a
product does not:

1. **Performance issues.** Nothing detects an N+1 query, a slow database
   span, a chain of sequential HTTP calls that could be parallel, or an
   oversized payload, and files it as an issue next to errors.
2. **Trends and regressions per operation.** The operations table is one
   window. There is no "p95 of `GET /checkout` over the last two weeks" and no
   "this endpoint got 40% slower after deployment 812". Anomaly insights
   correlate with deployments, per service and metric, not per operation.
3. **A threshold-based satisfaction score** (Apdex, or the share of users
   having a slow experience) per operation.
4. **Front end joined to back end.** Web vitals and traces are separate
   products. A slow page load does not link to the backend trace that served
   it.
5. **Breakdown inside an operation**: which query, which downstream call,
   which span attribute accounts for the time.

**Shape.**

- Detectors run on stored traces in a background job over recent, sampled
  traces, with a fixed budget per tick. They do not run at ingest. A detected
  pattern becomes a group with its own type in the existing error groups
  model, so it gets the workflow of D1, alert rules and Fix with agent with no
  extra surface.
- Operation trends come from a small pre-aggregated series per operation and
  interval. Span names are user-controlled, so the number of tracked
  operations per project is capped, with the overflow counted and reported.
- Regression after a deployment compares the windows before and after a
  deployment marker, reusing the existing anomaly band model.

### D3. Release health

**Missing.** Temps can say which errors, logs and spans carry a release
version. It cannot say whether a release is healthy: no sessions, no
crash-free rate, no adoption.

**Shape.**

- Store `session` and `sessions` items as counters per project, release,
  environment and hour (started, errored, crashed, abnormal; distinct users
  as an estimate). Individual sessions are not kept. The row count is bounded
  by releases times hours, and old releases roll up.
- A release page: crash-free sessions and users, adoption over time,
  comparison with the previous release, new and regressed issues in it.
- An alert when the crash-free rate of the newest release falls below a
  threshold, through the existing alert rules.
- A release is tied to the deployment that shipped it, so the release page
  links to the deployment and its rollback.

Release health matters most for mobile and desktop apps, where a release
cannot be rolled back. For web services deployed by Temps, a bad release is
already visible as a failed health check and a spike in errors, which is part
of why this ranks third.

### D4. Attachments

**Missing.** Attachment items are framed and dropped. Storing them needs the
blob store, a per-event and per-project size quota, and deletion with the
event. Feedback screenshots depend on this.

### D5. Links to issue trackers and source control

**Missing.** No "create a GitHub or GitLab issue from this group", no Jira or
Linear link, no two-way status. Git providers are already connected for
deployments, so GitHub and GitLab issues are the cheap first step. Suspect
commits (which commit touched the failing lines) and ownership rules (route a
group by file path) build on the same connection.

### D6. Profiling

**Missing.** No profile storage, aggregation or flame graphs. A new signal
with its own storage and its own volume problem.

### D7. Native and mobile crashes

**Missing.** No minidump processing, no dSYM or ProGuard symbolication, no
mobile replay. This is the largest item and the furthest from what Temps
deploys, which is web services. It comes last and may never be worth building.

## Not doing

- **The Sentry management API** (issues, alerts, organisations). Compatibility
  is at the SDK ingest boundary. Tools that drive Sentry's own API are out of
  scope.
- **Sentry's grouping algorithm versions.** Temps grouping stays the native
  policy; D1's merge and split are the correction mechanism.

## Open questions

- Whether performance issues (D2) belong in the error groups table or in a
  parallel one. One table gives one inbox; it also mixes two kinds of event
  with different detail views.
- Whether D3 should wait until there is demand from mobile or desktop users,
  given how little it adds for web services.
