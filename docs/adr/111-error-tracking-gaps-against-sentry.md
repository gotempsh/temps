# ADR 111: What error tracking and performance are missing to match Sentry

## Status

Proposed — 2026-11-19. Being implemented in the order of the Decision
section. D1.1 (bulk actions), D1.2 (merge and split), D1.3 (ignore
until), D1.4 (inbound filters), D1.5 (resolve in the next release),
D1.6 (assignment), D1.7 (comments and activity) and D1.8 (saved searches)
have shipped, so D1 is complete. D2.1 (performance issues), D2.2 (trends
and regressions after a deployment), D2.3 (Apdex), D2.4 (pages joined to
backend requests) and D2.5 (breakdown inside an operation) have shipped, so
D2 is complete. D3.1 (session counters), D3.2 (the release page),
D3.3 (the crash-free alert) and D3.4 (releases tied to deployments) have
shipped, so D3 is complete. D4 (attachments) and D5 (links to issue trackers
and source control) have shipped; D6 and D7 are not scheduled. The questions they settled are recorded under "Settled while
implementing".

Related: ADR 016 (ClickHouse traces backend), ADR 027 (cross-project trace
linking), ADR 033 (release identity for symbolication), ADR 054 (autofix
incidents in workspaces).

One of four gap ADRs (108 Better Stack, 109 Plausible, 110 PostHog,
111 Sentry). The target is 90% of what a typical user relies on.

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
| Group status and assignee | `error_groups.status`, `error_groups.assigned_to` | `unresolved`, `resolved`, `ignored`, `assigned`; assignee is free text (before D1.6) |
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
3. **Ignore until.** `ignored` has no condition. (This said `ignored` was
   permanent; the code reopens an ignored group on its next event, exactly
   as it reopens a resolved one.) No "until it happens 100 more times",
   "until it affects 10 users" or "for a week", and no "forever".
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
  given how little it adds for web services. Settled: built small (see
  D3.4 under "Settled while implementing").

## Settled while implementing

### D1.1 Bulk actions

- `POST /projects/{project_id}/error-groups/bulk` takes either explicit
  `group_ids` or a `filter` (status, environment, event window, text
  search), never both. At most 1,000 groups per request; a filter reports
  how many matching groups are `remaining` for a follow-up request. An id
  from another project fails the whole request before anything is written.
- One audit entry per request, with the selection and the groups changed.
  `status_change` alert rules are evaluated for the changed groups with one
  query per rule, and at most 50 notifications are delivered per request;
  the groups beyond that are not recorded as fired.

### D1.2 Merge and split

- The alias table is the existing `error_group_fingerprints`: a merge
  repoints the merged groups' fingerprints at the survivor and records
  where each came from (`merged_from_group_id`), so ingest needs no change
  to follow it. Merged groups keep their events and `total_count`
  (`merged_into_group_id` marks them and hides them from lists); the
  survivor adds their counts. Unmerge is the exact reverse; events
  ingested while merged stay in the survivor with the count that covers
  them.
- Merges are one level deep: a merged-away group cannot be merged again
  and a group that absorbed others cannot be merged away. That keeps
  unmerge unambiguous and lets ingest follow `merged_into_group_id` once
  when an event read the alias just before a merge committed, so counts
  stay exact under that race.
- Neither operation changes a status, so a merge cannot reopen a group or
  fire an alert. A retried SDK event after a merge resolves through its
  retry receipt to the group it was first stored in, and is not counted
  again.
- Split moves named fingerprints to a new group that starts at zero
  events; stored events stay where they are. A fingerprint that arrived
  through a merge belongs to that merge and cannot be split out.

### D1.3 Ignore until

- A plain ignore keeps today's behaviour (the next event reopens it), so
  no stored group changes meaning. Conditions are columns on the group:
  `ignore_until`, `ignore_until_event_count` (stored as the absolute
  `total_count` that reopens it), `ignore_until_user_count` with
  `ignore_user_count`, and `ignore_forever`. A group with conditions stays
  ignored until any one is met.
- They are evaluated in the statement that already increments the group's
  counter. New distinct users are counted exactly in
  `error_group_ignore_users`, written only while a user condition is
  active and capped by the threshold (at most 10,000); the rows are
  deleted when the ignore ends. Users are keyed by the first of user id,
  email, username, IP address and visitor.
- Every status write sets the conditions (an ignore records them, any
  other status clears them), and ignoring an ignored group with conditions
  replaces them. A group that stays ignored fires no alerts; when it comes
  back it is a regression like any other.

### D1.4 Inbound filters

- Five rule kinds, per project, in `error_inbound_filters`: browser
  extensions, web crawlers and localhost (built in, at most one of each),
  and error message and release (a case-insensitive glob, `*` and `?`
  only, at most 200 characters). No regular expressions: a glob cannot be
  written to take super-linear time, and its matching is bounded by
  cutting the text to 1,024 characters. At most 50 rules per project.
- Ingest reads an immutable snapshot of every enabled rule behind an
  `ArcSwap`, keyed by project: no lock and no database call per event. A
  write through the API swaps the snapshot before it returns; other
  processes pick a change up within 15 seconds from a `COUNT(*)` and
  `MAX(updated_at)` check, and reload only when it differs.
- Drops are counted per rule in an atomic counter that survives a reload,
  and added to `dropped_count` and `last_dropped_at` every 15 seconds in
  one statement; a failed flush keeps the counts for the next one. A crash
  loses at most those 15 seconds of counts. Reads add the unflushed counts.
- Filters apply to error events only (store endpoint and envelope `event`
  items), after the project is resolved and before symbolication or
  storage. A dropped event is answered as accepted, so the SDK neither
  retries it nor records it as lost; the envelope response counts it in
  `x-temps-sentry-errors-filtered`. Sentry instead reports a filtered event
  as a client outcome; Temps has no server-side outcomes to report it to.

### D1.5 Resolve in the next release

- Releases are ordered in a new per-project table, `error_releases`
  (version, `released_at`, `first_seen_at`, `deployment_id`). `released_at`
  is the creation time of the first deployment of the project whose commit
  SHA equals the release, which is the release Temps injects for git
  deployments (ADR 033); a release with no matching deployment (set in the
  SDK, or an image tag) is ordered by when it was first seen. It is written
  once and never moves. The event's own deployment, from its DSN, is kept
  as a link only, never as the order, because a DSN can outlive the
  deployment it was issued for. A migration registers the releases of the
  last 90 days of stored error events.
- Ingest registers a release once per process: a bounded in-memory set
  (20,000 entries) remembers what is registered, and the insert runs
  outside the event's transaction, so a retried or rolled-back event cannot
  leave the set ahead of the table. Releases longer than 250 characters are
  not registered.
- Resolving in the next release records the project's newest release at
  that moment (`resolved_in_release_after_id`), after registering the
  commit SHAs of the project's 20 most recent ready deployments, so a
  release that is deployed but has not reported an event yet counts as the
  current one. A project with no release at all cannot use it: the request
  fails and asks for a plain resolve.
- The group stays resolved for events from that release, from an older
  one, and for events with no release (as Sentry does); the first event
  from a newer release reopens it as a regression. The check is one more
  branch in the statement that already counts the event. Any other status
  write clears the condition; resolving again re-bases it on the current
  release.
- "Newest" is project-wide, not per environment: a release first deployed
  to staging counts as current for production too, which can only keep a
  group resolved longer, never reopen it early.

### D1.6 Assignment

- The assignee is `assignee_user_id` or `assignee_team_id` on the group, at
  most one of them (a check constraint), and no longer a status. The new
  route is `PUT /projects/{project_id}/error-groups/{group_id}/assignee`;
  `GET /projects/{project_id}/error-group-assignees?q=` returns at most 20
  users and 20 teams to pick from.
- **Upgrade.** Each stored free-text `assigned_to` is matched to the
  active user with that email, case-insensitively, and then cleared. A value
  that matches no user is kept in `assigned_to`, read-only, so nothing typed
  by hand is lost. Groups in the `assigned` status move to `unresolved`: an
  assigned group was an open issue someone was working on, and
  `unresolved` is the open state; the assignee stays on the row. `down`
  writes matched users back as their email but cannot restore the status.
- **Compatibility.** The update request's `assigned_to` still works: an
  email assigns that user (an unknown email is refused), an empty string
  unassigns. A write of status `assigned` is stored as `unresolved`. A list
  filtered by `status=assigned` returns nothing; use `assignee=` instead.
- A user assignee must be able to open the project (the same team-access
  check the API applies to requests). Any team can be assigned; whether a
  team has access to the project is not checked, because a team without
  project access can still be the owner of a problem.
- **Notification.** A change to a new assignee is mailed through the
  transactional email path (email providers only, the recipients only),
  in the background, when an email provider is configured; a team's
  members are mailed, at most 50. A failed or missing mail never fails the
  assignment. `status_change` alert rules that list `assigned` still fire on
  a new assignee, so existing rules keep their meaning. There is no in-app
  notification yet.
- "Assigned to me" is `assignee=me` on the list: groups assigned to the
  caller or to a team the caller is a member of. Bulk assignment is not
  offered.

### D1.7 Comments and activity

- One table, `error_group_activity`, holds comments and history: a `kind`
  (`comment`, `status_changed`, `regressed`, `assigned`, `merged`,
  `unmerged`, `split`), the actor, a comment `body`, and typed `details`
  (JSONB). It is deleted with its group or project. A deleted user's entries
  stay, with no actor.
- Entries are written when a group changes, never per event. A bulk change
  writes all its entries in one statement. An event writes an entry only when
  it reopens a resolved or ignored group; the reopen is claimed inside the
  ingest transaction and a retried event is not stored again, so a retry
  cannot log a second regression.
- History entries are written after the change commits, outside its
  transaction, the same way as audit logs: a failed write is logged and the
  change still succeeds. The conservative alternative, failing the change,
  would let a log table block triage.
- Nothing is backfilled: the log starts at the upgrade.
- Comments are plain text, at most 10,000 characters, trimmed, never empty.
  Only the author edits a comment; the author or an administrator deletes
  it. A deployment token cannot comment, because a comment needs an author.
  Comment writes are audit-logged by id, never with their text. Assignment
  entries store the assignee's name as it was when assigned.
- No mentions and no notification on a comment yet: a mention needs the
  same "who may be told" check as assignment and is left for later.

### D1.8 Saved searches

- A saved search is a row of `error_saved_searches`: an owner, a project, a
  name unique per owner ignoring case, `shared`, and the list filters as
  columns (status, environment, assignee, text, sort). Columns rather than
  JSON so a deleted environment clears that filter (`ON DELETE SET NULL`)
  instead of leaving a search that points at nothing. A search goes with its
  owner and its project.
- The time range is not saved: a search answers "which groups", and the
  range stays whatever the viewer has open. A saved absolute range would go
  stale the day after it was saved.
- `assignee: me` is stored as written and resolved for the viewer, so a
  shared "assigned to me" search shows each person their own groups.
- Private by default. Shared searches are visible to everyone who can open
  the project. Only the owner changes a search; the owner or an
  administrator deletes a shared one. Someone else's private search is a
  404, administrators included.
- Saving, changing and deleting a search needs `error_tracking:write`, the
  same as other triage writes, though a private search affects only its
  owner; the conservative choice keeps one permission for every write here.
  A deployment token cannot save one, because a search needs an owner.
- At most 50 saved searches per user per project, enforced under a per-owner
  transaction lock so concurrent requests cannot pass it. Writes are
  audit-logged with the name and `shared`, not the filters.
- The console's error list gains a text search box and an "Ignored" status
  option, so every filter a saved search can hold is visible and editable
  on the page that applies it.

### D2.1 Performance issues

- **One table.** Performance issues are rows of `error_groups` with
  `issue_type = 'performance'` (every existing row is `error`). This answers
  the open question the conservative way for the inbox: one list, one
  workflow, one set of alert rules, and Fix with agent with no new surface.
  The list filters by `issue_type`; the issue page shows the evidence (the
  pattern, the spans involved, their time, a link to the trace) above the
  usual tabs.
- **Filing goes through ingest.** A finding becomes an ordinary event with
  a grouping key (detector, service, root operation, normalized pattern) in
  place of the computed fingerprint, and a dedup key (the trace plus the
  grouping key) stored like a Sentry event id. A trace read twice therefore
  counts once, and the count of an issue is the number of traces it was
  found in. Inbound filters do not apply: they are for SDK noise.
- **Detectors**, with fixed thresholds: N+1 (the same normalized statement 5
  or more times under one parent), slow query (a database span of 1 s or
  more), consecutive HTTP (3 or more non-overlapping client calls of 50 ms
  or more under one parent), large payload (a body of 1 MB or more by the
  OpenTelemetry size attributes). Per-project thresholds are not offered
  yet; a column can add them without changing what is stored.
- **Budget.** A background loop every 5 minutes, never at ingest. Each tick
  takes the 20 projects checked longest ago, reads each one's 20 slowest
  root traces in the window since its last check (at most 200 traces a
  tick, 2,000 spans a trace) and never reaches back more than 15 minutes, so
  a busy project is sampled rather than queued. Traces younger than a
  minute wait for the next tick. Every console instance runs the loop;
  instances that read the same trace are deduplicated by the dedup key.
- **Status.** `performance_scan_state` records per project where the scan
  stopped and what it found;
  `GET /projects/{project_id}/performance-issues/status` reports it with a
  `configured` flag, the reason when nothing has been checked, and the
  detector list, so the console can explain an empty list.

### D2.3 Apdex

- **On the existing report.** The score is a field of `/otel/span-stats`
  rows, not a new endpoint: `apdex`, the threshold it used, and the
  satisfied, tolerating and frustrated counts. Both backends count in the
  same aggregate as the percentiles (`countIf` in ClickHouse, `FILTER` in
  TimescaleDB), so the score is exact and costs no extra pass.
- **Threshold per request, not per project.** `apdex_threshold_ms`
  defaults to 500 ms and is validated (greater than 0, at most ten
  minutes); a value out of range is a 400 that names the parameter. One
  threshold per project, or per operation, would be a column; it is not
  added until someone needs a stored target, because the console keeps the
  chosen threshold in the page address.
- **Tolerating means up to 4T**, the Apdex definition. The console marks
  scores under 0.70 as a warning and under 0.50 as a problem.
- **Not a sort key yet.** Ranking by Apdex needs a sort field in both
  backends; the existing p95 and p99 rankings answer "who is slow" until
  then.

### D2.2 Trends and regressions after a deployment

- **Hourly rollups in Postgres.** `operation_rollups` keeps one row per
  project, service, operation and complete hour (calls, errors, average,
  p50, p95, p99), written from the span-stats report by a background job
  every 10 minutes. The job takes the 50 projects rolled up longest ago,
  rolls at most 6 hours of each per tick, starts 24 hours back on the first
  run or after a long outage, and waits 5 minutes after an hour ends so
  late spans land. Rolling an hour again replaces its rows. Rows older than
  30 days are deleted in batches of 5,000. The rollup is the same on both
  span backends because it goes through the storage trait.
- **Cap and overflow.** At most 200 operations per project and hour are
  kept, those with the most total time. `operation_rollup_state` records
  how many of the last hour did not fit; the trend and regression responses
  report it, and the console says so under the trend. The cap bounds the
  table at about 200 × 24 × 30 = 144,000 rows per project.
- **Regression model.** The baseline is the week before the deployment went
  live (`deployments.ready_at`); the after window is the next 24 complete
  hours, clipped to now. The hour the deployment went live in is on
  neither side. Each side's value is the median hourly p95; the baseline
  band is `robust_band` (median and scaled MAD), the anomaly detectors'
  model, with the scale floored at 5% of the median. An operation regressed
  when the after value is more than 3 scales above the median and at least
  20% higher, improved in the mirror case. Fewer than 8 baseline hours
  (`MIN_BASELINE_SAMPLES`) or 2 after hours is reported as insufficient,
  not judged. Thresholds are fixed; a column can make them per project.
- **Deployment markers, not a deployment model.** Regressions are computed
  on request for one deployment, not stored or alerted on. A deployment
  that never went live is a 400 that says so; one from another project is a
  404 naming both IDs.

### D2.4 Pages joined to backend requests

- **By path and time, not by trace id.** A page load carries no trace id: a
  navigation sends no `traceparent`, and the proxy only reads one on an
  inbound request. Carrying one would need the analytics SDK to read a
  `Server-Timing: traceparent` header the backend would have to set, a new
  column on `performance_metrics` and a change to analytics ingest. The
  conservative option is a join that needs none of that: the server spans
  of the same project whose `url.path` (or `http.target`, query string
  ignored) equals the page's path, in the same window. It answers how much
  of the p75 time to first byte the backend explains and links the slowest
  requests; it is not a one-to-one link, and the console and docs say so.
  A trace id on page loads remains a follow-up.
- **Bounded on request.** `GET /otel/pages/backend` reads the page's web
  vitals with one aggregate over `performance_metrics` and at most 500
  server spans per attribute looked up (two exact attribute filters), over
  a window of at most 7 days (24 hours by default), and returns the 10
  slowest. It stores nothing.

### D2.5 Breakdown inside an operation

- **Exclusive time from a sample of traces.** `GET /otel/operations/breakdown`
  reads at most 20 traces (2,000 spans each) that contain the operation,
  from the latest 500 calls in a window of at most 7 days (24 hours by
  default). Every span under each call is charged its duration minus the
  union of its own children's intervals, clipped to it, and the times are
  summed per (service, span name, category). The operation's own exclusive
  time is reported separately. Parallel calls each keep their time, so
  shares can exceed 100%; that is stated in the response docs, the console
  and the CLI. A call of the operation nested in another is counted once,
  through the outer call.
- **Category from attributes.** `database` (`db.system`), `messaging`,
  `http` (a client span with an HTTP method or URL), `rpc`, else by span
  kind. At most 25 groups are returned, the rest summed into one row.
- **Samples.** `recent` (default) reads the latest calls; `slow` reads only
  calls at or over the window's p95 from the span-stats report. Span names
  are used as they are: OpenTelemetry names are low cardinality by
  convention, and statement normalization stays in the performance-issue
  detectors.
- **Split by an attribute.** An optional `attribute` groups the calls by
  the value of that attribute on the operation's own spans (at most 20
  values, the rest summed), which answers "which tenant, route or model is
  the slow one" without a new index.

### D3.1 Session counters

- **Counted as Relay counts them.** `session` updates and `sessions`
  aggregates become hourly counters per project, SDK environment (the
  string the SDK sends, empty when none), release and the hour the session
  *started*, so every update of a session lands in the same row: `init`
  starts a session; errors or an error status mark it errored, once per
  session id; crashed and abnormal are counted as they arrive; every update
  counts its user, and a crash its user as crashed. Aggregates add their
  totals, errored exactly. The nil UUID is not a user.
- **Estimates, not sets.** Distinct users, errored sessions and crashed
  users are HyperLogLog sketches of 256 one-byte registers (about 6.5%
  standard error), hashed with the first eight bytes of SHA-256 so they
  merge across processes and Temps releases. A database function merges
  them on upsert; reads stream the rows and merge in memory, so a read holds
  three sketches per release whatever the window. No session or user id is
  stored.
- **Off the hot path.** The envelope handler queues one delta per session
  with `try_send` into a bounded channel (10,000); a full queue drops and
  counts, and the response says how many (`x-temps-sentry-sessions-queued`,
  `-dropped`). One task sums the deltas in memory (at most 5,000 keys) and
  writes them every 10 seconds as multi-row upserts of 200 keys. A failed
  write is retried on the next two flushes, then given up and counted.
  Counts of a project deleted since they were queued are dropped by the
  statement itself. A process exit can lose the last 10 seconds.
- **Counted only on acknowledgement.** Sessions carry no id to deduplicate
  a retried envelope, so they are queued only when the envelope is answered
  200; a 5xx and the SDK's retry cannot count them twice. Envelopes resolved
  by host without a DSN leave them uncounted, as before.
- **Bounds.** At most 100 session updates and 1,000 aggregate rows per
  envelope; more, or a malformed session, is reported as unsupported and
  never rejects the envelope's events. Sessions that started more than 5
  days ago or over an hour in the future, releases over 250 characters and
  environments over 64 are refused (counted as dropped). Counters are kept
  90 days, deleted in batches of 5,000 rows; old releases are not rolled up
  further, since one row per release, environment and hour is already
  small.

### D3.2 The release page

- **On the existing releases page.** Errors → Releases opens with release
  health: crash-free sessions (`1 − crashed / sessions`) and users,
  adoption (the release's share of the sessions started in the last 24
  hours, in the same environment filter), sessions, estimated users and the
  deployment, for windows of 1, 7 or 14 days (`GET
  /projects/{id}/release-health`, paginated 20, at most 100). A trend sheet
  draws one release per hour (`/release-health/series`). With no session
  ever received it says so and links to the SDK setup.
- **Not built:** comparison with the previous release and the new and
  regressed issues of a release. The error list already filters by
  release, and a comparison needs an agreed "previous" across
  environments; both are follow-ups.

### D3.3 Crash-free alert

- **Through metric alert rules, not a new rule type.** Every five minutes
  the crash-free rates of releases with a session in the last hour are
  published over the trailing 24 hours as project metrics
  (`release.crash_free_sessions`, `release.crash_free_users`,
  `release.sessions`, labelled `release` and `environment`; at most 1,000
  releases per tick, the busiest first). A rate needs 20 sessions or users,
  so one crash in three sessions does not page anyone. Static and anomaly
  detectors, grouping by release, cooldowns and notifications are the
  existing ones.
- **Never created silently.** The page's "Alert on a crash-free drop"
  opens the metric alert form prefilled (minimum over 15 minutes, below
  99% for 10 minutes, one alert per release); the user saves it. Without
  the metrics store nothing is published and startup logs why.

### D3.4 Releases tied to deployments

- A release seen in sessions is registered in `error_releases` like the
  release of an event (once per process), so it is ordered for "resolve in
  the next release" and linked to the first deployment whose commit SHA it
  is, or the DSN's deployment. The page links that deployment, whose page
  has the rollback. A release Temps did not deploy says so.
- The open question whether D3 should wait for mobile or desktop demand was
  settled by building it at this size: counters only, no new store and no
  new alerting system.

### D4 Attachments

- **Plain files only.** An attachment item whose `attachment_type` is
  `event.attachment` (the default) is kept; an `attachment_type` that is not a
  string is unsupported. Minidumps, Apple crash reports and
  Unreal files belong to D7 and stay unsupported. An attachment never rejects
  its envelope: one that is too large, past the count, or has no event or
  feedback to belong to is reported unsupported, and the events are stored.
- **Limits.** 2 MiB per file and 4 MiB or 8 files per event or feedback, under
  the envelope's own 2 MiB body and 10 MiB decompressed limits. The 4 MiB
  counts files earlier envelopes stored for the same owner, so a retry that
  adds files cannot pass it. Each project
  has a quota of 256 MiB of attachments until the admin changes it (0 to
  100 GiB, a column on `error_attachment_usage`, audited). Lowering it deletes
  nothing: new attachments are dropped until use falls under it.
- **Off the hot path.** Attachments are queued only when the envelope is
  answered 200, after their event or feedback is stored, so an SDK retry after
  a failure stores nothing twice. A file's identity is its owner, name and
  SHA-256, checked under the project's usage row lock before any limit, so a
  resent file is a duplicate even once the quota is full (a resend of a write
  that never finished completes it, charged once, and renews its reservation;
  one the sweeper already claimed for removal, `removing_at`, is left to the
  sweeper and is never marked stored), and a different file
  sent later for the same owner is new; its display position is the owner's
  next one.
  Ingest does one `try_send` into a queue bounded at 256 envelopes and 16 MiB;
  past either bound the batch is dropped and counted
  (`x-temps-sentry-attachments-dropped`). One task reserves each file against
  the quota in a transaction, writes it to the project's blob storage, then
  marks the row stored. Envelopes without a DSN (tunneled) carry no
  attachments.
- **Owner.** The envelope's event id, else its only event, else its only
  feedback, resolved through `error_event_source_keys` or the feedback's
  source id, and only under the same environment and deployment as the DSN
  that sent the file, so one DSN cannot attach files to another scope's event. The owner's timestamp is kept so the sweeper can find events that
  expired.
- **Lifetime.** An attachment lives as long as what it came with. Every ten
  minutes a bounded sweeper deletes those whose event expired with its chunk,
  writes that never finished (after an hour), attachments of deleted feedback,
  and every attachment of a deleted project (soft-deleted counts). Deleting a
  feedback deletes its attachments. An object that cannot be deleted keeps its
  row, so the next pass retries; each pass pages past such rows instead of
  stopping at them, and deleted projects are visited a page per pass from
  where the last pass stopped. Deleting rows takes the project's usage row
  lock first, as a reservation does, so the two cannot deadlock. Rolling the
  migration back refuses while any attachment is listed, since dropping the
  rows would strand their objects.
  A write that failed or timed out deletes its object before its row, since a
  store may have kept a file it never acknowledged; until then the row counts
  against the quota and the sweeper retries it as an unfinished write. The
  uploader and sweeper hold the service weakly and end when it is dropped. Without blob storage nothing is stored, and
  the usage endpoint says so and links `/storage`.
- **Serving.** PNG, JPEG, GIF and WebP are served inline so the console can
  show a feedback screenshot. Everything else is served as
  `application/octet-stream` to save, with `nosniff` and a sandbox CSP, so a
  stored HTML or SVG file never runs in the console's origin. File names lose
  directories and control characters.

### D5 Links to issue trackers and source control

- **GitHub and GitLab, through the project's own repository.** Opening an
  issue and reading commits use the repository and git provider connection
  the project already deploys from, with its credentials. Other providers and
  projects without a repository answer "not connected" with a link to
  `/git-providers` or the project's git settings, and the console shows that
  onboarding state instead of hiding the panels. Jira, Linear and any other
  tracker are plain links: an http(s) URL without credentials, at most ten
  links per group.
- **One-way and on request.** Temps never closes, reopens or comments on an
  issue, and an issue's state never resolves a group. The state of a GitHub or
  GitLab issue is read when it is linked and again when someone presses
  refresh; a read that fails keeps the last state and is counted in the
  answer. There is no webhook and no polling job.
- **One open issue per group and repository.** Opening one takes a
  transaction-scoped advisory lock on the group, so two people pressing the
  button at once get one issue and a conflict, and an open issue already
  linked in the same repository is a conflict too. A closed one can be
  followed by a new one.
- **What the issue says.** Title (the group's, or the one given, at most 255
  characters), exception type and message, first and last seen, event count,
  up to ten application frames shown as repository paths, and a link back to the
  group. Text is escaped as Markdown. Nothing from the request, the user, tags
  or breadcrumbs is copied out: the repository's audience may be wider than
  the project's.
- **Suspect commits are not blame.** They are the newest five commits that
  touched the file of the latest event's innermost in-app frame that maps to a
  repository path (webpack, `app://` and `file://` prefixes and the project's
  source root removed; dependency directories, URLs and paths with `..`
  ignored), read at the deployment's commit, or the default branch when the
  event has none. Line-level blame would need one provider call per frame and
  is left out. When nothing matches, the answer says why.
- **Ownership rules assign new groups only.** A project has at most 50
  ordered rules of a path pattern (`src/billing/` a directory, `*.py` a file
  name anywhere, `**` any depth) and an owner. When a group is created, its
  frames are tried innermost first against the rules in order; the first match
  assigns the group if it is still unassigned, and the activity names the
  rule. Existing groups are never reassigned and no mail is sent. Every owner
  must be able to open the project when the rules are saved. The rules are
  held in memory and reloaded on every save, so ingest makes no extra query
  for a project without rules.
- **Assistant.** The console assistant may read the repository status, the
  linked issues, suspect commits and the ownership rules. Opening, linking,
  unlinking and refreshing issues and replacing the rules stay with people.
