# ADR 074: What error tracking and performance are missing to match Sentry

## Status

Proposed — 2026-11-19. Being implemented in the order of the Decision
section. D1.1 (bulk actions), D1.2 (merge and split), D1.3 (ignore
until), D1.4 (inbound filters), D1.5 (resolve in the next release) and
D1.6 (assignment) have shipped; the questions they settled
are recorded under "Settled while implementing".

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
  given how little it adds for web services.

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

