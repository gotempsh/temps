# ADR 108: What logs, uptime and status pages are missing to match Better Stack

## Status

Proposed — 2026-11-19. D1 implemented — 2026-11-19 (see *Settled while
implementing D1*). D2 implemented — 2026-11-20: the email subscription first, then the feed,
webhooks, maintenance windows and page password (see *Settled while
implementing D2*). D3 and D4 implemented together — 2026-11-20: HTTP
assertions, TCP, TLS certificate and DNS checks against any public target;
ICMP, domain registration expiry and browser checks deferred (see *Settled
while implementing D3 and D4*). D5 implemented — 2026-11-20 for monitors
of targets outside Temps, checked from worker nodes with a majority vote
(see *Settled while implementing D5*). D6 to D8 not started.

Related: ADR 021 (multi-node log aggregation, humanized alert notifications),
ADR 025 (unified alarm log), ADR 026 (metric alert label filters), ADR 045 to
047 (global logs stores), ADR 052 and 053 (pluggable health probes,
distributed runtime health).

One of four gap ADRs, against Plausible, PostHog, Better Stack (this one)
and Sentry. The target is 90% of what a typical user relies on. Drafted as
ADR 073; renumbered because 071 to 074 were already taken on `main`.

## Context

Better Stack is three products: log management, uptime monitoring with
incident response, and status pages. Temps has a working version of each, with
a narrower scope.

### What exists

| Area | Where | Today |
|---|---|---|
| Container log collection, search, context, live tail | `crates/temps-log-aggregator` (`/logs/search`, `/logs/context`, `/logs/tail`) | Yes |
| Structured telemetry logs with typed attribute filters | `/otel/logs` in `crates/temps-otel` | Yes |
| Log sources | container stdout/stderr, OTLP (`/otel/v1/logs`), Sentry SDK logs | Three |
| JSON and logfmt line parsing | `crates/temps-log-aggregator/src/parser.rs` | Yes |
| Log storage with retention, compaction, filesystem or S3 | `crates/temps-log-aggregator/src/storage/`, `services/retention.rs` | Yes |
| Metric alerts with static, anomaly, forecast and outlier detectors | `metric_alert_rules`, `crates/temps-otel/src/detectors/` | Yes |
| Uptime monitors | `status_monitors`, `crates/temps-status-page/src/services/health_check_service.rs` | HTTP, own environments only |
| Cron monitors (check-ins, missed run, timeout) | `crates/temps-monitoring/src/cron_monitors/` | Yes |
| Incidents with severity, status and updates | `status_incidents`, `status_incident_updates` | Yes |
| Status pages: groups, custom domains, public incident updates, `summary.json` | `crates/temps-status-page/src/services/public_pages/` | Yes |
| Alarms: acknowledge, silence, resolve, bulk | `alarms`, `crates/temps-monitoring/src/handlers/alarm_handlers.rs` | Yes |
| Notification routes by severity | `notification_routes`, `crates/temps-notifications/src/routing.rs` | Email, Slack, webhook |

### What a monitor is today

`CreateMonitorRequest` takes a name, a required `environment_id`, an interval
(floor 30 seconds) and an optional `check_path`
(`crates/temps-status-page/src/services/types.rs`). The probe is an HTTP `GET`
of that environment's address; a 2xx or 3xx is up, redirects are not followed,
the timeout is 30 seconds. `monitor_type` is stored but only ever `"web"`.
Probes run from the Temps host itself.

So a monitor cannot target anything Temps did not deploy, cannot check
anything but an HTTP status, and cannot tell "the app is down" from "this host
is down".

## Decision

The gaps, most important first.

### D1. Logs: saved searches, alerts on logs, and more sources

**Missing.**

1. **Alerts on logs.** Nothing fires when "payment failed" appears 20 times
   in five minutes, or when error-level lines exceed a rate. Metric alerts
   exist; a log query cannot be the input to one.
2. **Saved searches and views.** A query cannot be named, shared or pinned.
   Every investigation starts from an empty search box.
3. **Sources other than OTLP.** A host, a database, a CDN or an app outside
   Temps can only send logs by speaking OTLP. There is no plain HTTP JSON
   endpoint, no syslog listener, and no documented path for Vector or
   Fluent Bit.
4. **Log-derived metrics and charts.** A count or a numeric field over time
   cannot be charted on a dashboard.

**Shape.**

- A log alert is a metric alert rule whose series is a count of lines matching
  a saved query per window. It reuses the evaluator, detectors, alarm log and
  notification routes that exist; the new part is the series source. The
  evaluator runs the count on its interval over a bounded window. It never
  inspects lines as they are ingested, so log ingest stays free of per-line
  rule matching.
- A saved search is a per-project row (name, query, filters, time range) with
  the usual permissions. Log alerts reference one.
- An authenticated HTTP ingest endpoint accepting newline-delimited JSON,
  using the same ingest-key model as analytics (ADR 040), the same batching
  and the same per-project ingest budget (`services/ingest_budget.rs`).
  Vector and Fluent Bit can both write to it, which covers syslog and files
  without Temps running a syslog listener. Over-budget batches are rejected
  with `429`, counted, and shown on the pipeline status page.
- Log-derived series become a panel type in the existing dashboard builder.

### Settled while implementing D1

- **Which store a log alert counts (open question 2).** Both, chosen per
  search. A saved search without a text needle is counted in the line
  index plus the unsealed head buffers, exactly. A search with a text
  needle, or an install without a line index, is counted by one bounded,
  newest-first scan of the chunk store that stops at 1,000 matching lines.
  The window is capped at one hour either way, so on a small host a rule
  can never ask for more than an hour of history per evaluation. While a
  live chunk in the window is not in the line index yet (its index write
  failed, or the index was down), the count is scanned instead, so the
  index lagging never reads as fewer lines. A search that names databases
  counts only those, not the project's application lines as well.
- **A capped count is a lower bound.** The evaluator reports it as
  "at least N". It can establish "more than the threshold" but never
  "fewer than": a below-threshold rule on a capped count is undecided and
  keeps its state (and any open alarm) rather than firing or resolving,
  and so is a "more than" rule whose lower bound is still under its
  threshold. A rule "more than the cap" could never fire and is refused
  when saved.
- **Log alerts are static thresholds on a count.** No anomaly or forecast
  detector (the metric history holds no log counts to learn from), no
  `group_by`, no label filters (the saved search filters), no per-series
  alerting, aggregation fixed to `count`. An alert on a numeric log field
  is not built; the dashboard can chart one. Pointing a rule at another
  source or saved search resets its state and resolves the alarms the old
  series left open; switching a log rule to a metric must name the metric.
  Updates to a rule are serialized on its row. A database trigger bumps the
  rule's `config_revision` on every edit that changes a verdict (series,
  detector, threshold, window, duration, enabled), from any writer. An
  evaluation opens an alarm and writes its result only while the revision
  it read is current, and resolves what it opened if the rule changed
  before its write, so a tick that overlapped an edit never puts the old
  configuration's verdict back, per-series rules included. A new revision,
  or a new query in the rule's saved search, restarts the breach timer.
- **Saved searches.** Per project, at most 100, `logs:read` to see and the
  new `logs:write` to change. The time range is one of `15m`, `1h`, `6h`,
  `24h`, `7d`, `30d`; a custom window from the logs page is saved as the
  nearest of those, since a saved search is a question to ask again, not a
  fixed slice of the past. Deleting a search an alert counts is refused
  with `409`, checked under a lock on the search so an alert created at
  the same moment is never cascaded away; a dashboard tile charting a deleted search says so.
  "Shared" means visible to everyone who can read the project's logs;
  there are no private searches.
- **Ingest keys are their own table**, modelled on the analytics ingest
  keys of ADR 040 rather than reusing them: a log key is a different
  capability (`tlk_` prefix, optionally pinned to an environment, with a
  default service), and an analytics key must not be able to write logs.
  Only the SHA-256 of a key is stored; the plaintext is returned once.
  Lookups are cached for 30 seconds; revoking through the API evicts the
  cached entry at once, and a lookup that overlapped the revoke is not
  cached. A revoke waits for the key's batches in flight, then seals and
  closes its streams. The revoked key's gate refuses late batches for five
  minutes and is then forgotten, so rotating keys never grows memory.
- **The receiver is on the public router** (ADR 051 D3): `POST
  /logs/ingest` authenticates by its key, never a session, and is reachable
  on the ingress listener when the console is gated. It is not added to the
  proxy's any-host paths, which would take `/api/logs/ingest` from every
  deployed application's own domain. Key management stays on the admin
  router, and the console builds the collector URL from the public ingress
  URL, not the browser's origin.
- **A batch is admitted whole.** One malformed line refuses the batch with
  its line number, and the ingest budget is charged and enforced per
  batch, so neither stores part of one. Storage is at least once, like the
  collectors that send to it: a storage error part-way stores the lines
  before it, they are counted against the key, and a resend can store them
  twice. Making a resend idempotent needs a batch identity that Vector and
  Fluent Bit do not send. Limits:
  5 MiB per request, 10,000 lines, 256 KiB per line, 16 service names per
  key (further names fold into the key's default service). A timestamp
  more than 7 days old or 5 minutes ahead becomes arrival time and is kept
  as an attribute.
- **Dashboard tiles.** A tile's `source` is `metric` (the default, so
  existing layouts are unchanged) or `logs` with a saved search and
  `count` or `<fn>:<attribute>`, the attribute in the line index's key
  grammar. A count series is at most 240 buckets, a numeric one at most 60
  and needs the line index and no text needle. Buckets sit on the index's
  epoch-aligned boundaries whatever the requested start, and a numeric
  bucket counts only lines that carry the field, so no field is a gap, not
  zero. A scanned count stops at 5,000 lines and reports from when it
  counted, so a chart never draws zero for a period nobody counted.
- **Console.** Saved searches and ingest keys are reached from the logs
  page header, scoped to the service the page is filtered to. A log alert
  is a choice in the existing alert form. There is no CLI for alert rules
  of any kind yet, so log alerts are created in the console or the API;
  the CLI gains `temps logs saved` and `temps logs keys`.

### D2. Status page subscribers and scheduled maintenance

**Missing.** A visitor can only learn about an incident by loading the page.
No email subscription, no feed, no webhook, no announced maintenance window.

**Shape.**

- **Email subscription.** A subscribe form on the public page, double opt-in
  by a confirmation link, an unsubscribe link in every message. It sends
  through the existing transactional email stack (`crates/temps-email`),
  honours its suppression list, and onboards rather than hides when no email
  provider is configured: the page settings show "No email provider is
  configured" with a link to add one.
- **What is sent.** Only what the page already shows: a public incident
  update, and the resolution. The allowlist rule in the status pages document
  applies unchanged; a subscriber never receives a private update or an
  internal title.
- **The public page stays static.** It is plain HTML with no JavaScript and is
  served from a snapshot. The subscribe form is a normal form post to a
  rate-limited public route; rendering the page still runs no query.
- **Feeds and webhooks.** An Atom feed generated from the same snapshot, and
  per-page webhook subscribers.
- **Maintenance windows.** A scheduled entry with a start and an end, shown as
  upcoming and then in progress, announced to subscribers, and excluded from
  the uptime percentage. Alarms for the covered monitors are silenced for the
  window using the existing `silenced_until`.
- **Subscriber data.** Addresses are personal data: stored per page, deletable
  individually and with the page, never shown to other subscribers, exportable
  by the page owner.
- **Access control.** An optional password on a page, for a status page shared
  with customers only.

### Settled while implementing D2

The email subscription shipped first, on its own; the feed, webhooks,
maintenance windows and page password follow separately.

- **Whole page only.** A subscriber subscribes to a page, not to chosen
  components (the open question below). A component filter needs a choice
  in the form, which a page with no JavaScript can only offer as a long
  list of checkboxes, and it is easy to add later without migrating
  anything.
- **What is announced, and from where.** Notices are derived from the
  published snapshot, the same allowlisted data the page renders: one
  notice per public update, plus one for the resolution of an incident with
  none. Only updates published after subscriptions were switched on and
  within the last 24 hours are announced, so switching on never mails out
  the page's history and a long outage of the sender does not send a
  backlog. A notice whose update stops being public (made private, page
  unpublished, subscriptions off) before delivery finishes is cancelled.
- **Double opt-in that a link scanner cannot complete.** Opening a
  confirmation or unsubscribe link shows a button; only the POST acts. A
  confirmation link is valid for 48 hours and works once; a repeated
  request sends a new one at most every 10 minutes. An address that is
  never confirmed is deleted 48 hours after its last request. The public
  answer is the same for a new, pending or confirmed address.
- **Tokens.** Only SHA-256 hashes are stored. The confirmation token is
  random; the unsubscribe token is an HMAC of a per-subscriber nonce under
  the server key, so every notice carries the link without the token being
  stored.
- **Limits.** 10,000 subscribers per page, at most 1,000 of them pending;
  10 subscription requests per client per 10 minutes on the public routes.
  The dispatcher runs every 20 seconds and sends at most 200 mails per run
  across all pages, in batches of 50 recipients, walking a cursor over
  subscriber ids so each recipient gets a notice at most once, across
  restarts too. A recipient whose send fails three times is skipped.
- **Switching on needs a working sender.** Subscriptions cannot be enabled
  until an email provider exists, the sender address is at a verified
  domain, and Temps knows a public URL for the links (a custom domain of the
  page, or the external or public ingress URL). Each missing piece is shown
  with a link to where it is fixed, and the capability endpoint reports it.
- **Who sees addresses.** Listing, exporting and deleting subscribers need
  `status_page:write`, not read. The export is CSV served with `no-store`,
  with formula characters neutralised. Audit entries for settings changes,
  deletions and exports never contain an address.
- **Custom domains.** A verified page host additionally forwards
  `POST /subscribe` and `GET`/`POST` on `/confirm/{token}` and
  `/unsubscribe/{token}`, and its CSP gains `form-action 'self'`.
- **Mail.** Sent through `crates/temps-email`, so its suppression list
  applies, with open and click tracking off. No `List-Unsubscribe` header
  yet: the email service only passes `X-Temps-*` headers through, so adding
  it needs a change there first.

The second part, settled the same way:

- **Feed.** The newest 50 public updates, resolutions and maintenance
  announcements, built with the snapshot, with opaque entry ids derived
  from the page token and the event, so an id reveals no database key.
- **Maintenance suppresses alerts at the source.** Instead of writing
  `silenced_until` on each monitor, which an operator's own silence would
  then have to be merged with and restored after, outage detection skips a
  monitor while a running window covers it. A cancelled or ended window
  needs no undo. Checks taken inside a window are left out of the uptime
  of every page showing the monitor (only monitors still on that page
  count), from the stored checks, so nothing is rewritten.
- **Maintenance rules.** A window cannot be backdated (5 minutes of slack
  for "now"), lasts at most 7 days, starts at most a year ahead, and a page
  has at most 20 open ones. It is announced once, when scheduled; edits and
  cancellations change the page but are not mailed, which avoids a mail per
  typo. Once started, its start and monitors are fixed, so what was
  excluded from uptime stays explainable. Cancelling a running window ends
  it now.
- **Password.** An argon2 hash on the page row and a signed cookie (an HMAC
  under a key derived from the server key, binding the page, a password
  version and an expiry of 7 days), so checking a visitor needs no query
  and no per-visitor state. Every password change bumps the version and
  signs everyone out. While protected, the feed is not served (a reader
  cannot sign in) and the JSON is not shared cross-origin. Wrong passwords
  are limited to 5 per client per 15 minutes and at most 2 verifications
  run at once; a burst beyond that is told to retry rather than queued.
  Confirmation and unsubscribe links work without the password, so nobody
  is stuck on a list. A custom host forwards only a well-formed access
  cookie, never the rest of the visitor's cookies.
- **Webhooks.** Managed by the page owner (`status_page:write`), at most 10
  per page, `https://` only, to a public address checked when saved and
  again, pinned, on every delivery, with no redirects and a 10 second
  timeout. The body carries only what the page shows. The signing secret
  is shown once and stored encrypted; the signature is
  `t=<unix seconds>,v1=<hex HMAC-SHA256 of "t.body">`. Three attempts, 1 and
  10 minutes apart. A delivery is cancelled when the event leaves the page
  or the webhook is paused; resuming catches up the last 24 hours. No test
  send and no secret rotation yet: delete and re-add the webhook.
- **Schema names.** The webhook types are `StatusPageWebhook*` because the
  plugin OpenAPI merge keeps the last schema of a name, and a plain
  `WebhookResponse` silently replaced the project webhooks' schema.

### D3. Check types

**Missing.** One check exists: HTTP `GET`, 2xx or 3xx is up.

**Shape.** `monitor_type` becomes a real discriminator with a typed spec per
kind, in this order:

1. **HTTP assertions**: expected status codes, a keyword that must or must not
   appear in the body, a response-time threshold that marks the monitor
   degraded, request method, headers and body.
2. **TCP port**: a connection succeeds within a timeout. Useful for the
   databases and queues Temps already runs.
3. **TLS certificate and domain expiry** as a monitor, so it appears on a
   status page and in uptime history. Certificate expiry alarms exist for
   Temps-issued certificates; this covers any host.
4. **DNS**: a record resolves to an expected value.
5. **ICMP ping.**
6. **Browser transaction** (a scripted page flow). Heavy: a headless browser
   does not fit beside user workloads on a 1 vCPU / 2 GB host. If built, it
   runs in a sandbox on demand, never as a resident process, and is off unless
   the operator enables it.

Heartbeat checks already exist as cron monitors.

Response bodies are read up to a fixed limit for keyword checks and are never
stored, in line with the rule that probe bodies are not published.

### D4. Monitors for targets outside Temps

**Missing.** A monitor needs an `environment_id`. A third-party API, a
marketing site on another host, or a dependency cannot be monitored.

**Shape.** A monitor has either an environment or an explicit target (URL, or
host and port). Explicit targets go through the same public-target validation
the probe already applies (`PublicProbeTarget`), so a monitor cannot be
pointed at internal addresses. D3 and D4 are one change to the monitor model
and should ship together: most of D3's non-HTTP kinds have few targets until
D4 exists.

### Settled while implementing D3 and D4

One change, as planned: a monitor watches either an environment or an
explicit target.

- **A new discriminator, not `monitor_type`.** `status_monitors.check_kind`
  (`environment`, `http`, `tcp`, `tls`, `dns`) with a typed spec in
  `check_spec`, and a database constraint that an environment monitor has no
  target and a target monitor has a target, a spec and no environment.
  `monitor_type` is a free-form legacy string that existing rows and clients
  read; target monitors set it to their kind and nothing branches on it.
- **Assertions apply to target monitors only.** Environment monitors keep
  their built-in rule (`GET`, 2xx or 3xx); giving them the same assertions is
  a follow-up, since their URL and path are managed by deployments.
- **Shipped kinds:** HTTP (method, expected codes or classes such as `2xx`,
  keyword present or absent in the first 256 KiB, degraded-after threshold,
  headers and body), TCP connect, TLS certificate (publicly trusted chain for
  the host; degraded inside the warning period, default 14 days, down when
  expired or untrusted) and DNS (A, AAAA, CNAME, MX, TXT, NS; any listed
  value matching is up). Redirects are not followed, so the configured
  status is judged on the first answer.
- **Deferred:** ICMP needs raw sockets (`CAP_NET_RAW`), which the binary
  does not hold by default; domain registration expiry needs RDAP lookups
  against registries, a new outbound dependency; browser transactions are
  not built (*Not doing*: no resident headless browser).
- **Secrets in requests.** Headers and body are encrypted at rest with
  `EncryptionService` in one column; only header names are stored and shown
  in plaintext. An update keeps them unless it replaces them, and they are
  dropped when the kind changes away from HTTP. Hop-by-hop and
  framing headers (`Host`, `Content-Length` and the like) are refused, and so
  are credentials in the URL.
- **Public targets, twice.** On save an IP literal, or a name that resolves,
  must be public; a name that does not resolve yet is accepted, so a monitor
  can be created before its DNS. Every probe resolves again and pins the
  connection to the checked address (`PublicProbeTarget`). DNS checks use
  the system resolver and never echo a private A or AAAA answer.
- **Footprint.** 50 target monitors per project, the existing 30-second
  interval floor and 0.5 to 30-second timeouts, and the scheduler's existing
  sweep and concurrency limit of 10: no new task or resident process.
- **Incidents without an environment.** A target monitor's outage opens an
  incident and notifies like any monitor; deployment alarms are skipped,
  since there is no deployment. Target monitors can be selected on a status
  page.
- **Audit and API.** Writes are audited with the target host only, never the
  path, query or headers. Creation and changes have their own endpoints
  (`POST /projects/{id}/monitors/targets`,
  `PATCH /projects/{id}/monitors/{monitor_id}/target`); listing, history and
  deletion reuse the monitor endpoints. The schema names are
  `MonitorCheckKind`, `DnsCheckRecordType` and `CheckRequestHeader` because
  the plugin OpenAPI merge keeps one schema per name and `CheckKind`,
  `DnsRecordType` and `RequestHeader` already existed.

### D5. Probes from more than one place

**Missing.** Every probe runs from the Temps host. A single-node install
cannot report its own outage, and nobody can tell a regional network problem
from a real one.

**Shape.** In a multi-node install, workers are probe locations (ADR 053
already distributes health probing). A monitor is down when a quorum of
locations agree, which removes most false alarms. For a single-node install
the only honest answer is a probe from somewhere else; see *Open questions*.

### Settled while implementing D5

- **Per monitor, off by default.** `status_monitors.probe_from_nodes`, for
  monitors of targets outside Temps only (a database constraint). Turning it
  on is the operator's choice: it sends the check, and its decrypted request
  headers, to the cluster's workers.
- **Asked on each check, not scheduled on the workers.** The control
  plane's scheduler keeps the schedule; for each check it asks up to four
  online workers (active, a heartbeat in the last three minutes, a stored
  token), one per `region` label first, over the authenticated agent API,
  and probes itself at the same time. ADR 053's leased assignments fit
  probes of a node's own containers; an outbound check needs no state on
  the worker, and asking per check keeps one schedule, one stale-write
  fence and one place where results are recorded. The load is bounded by
  the existing sweep concurrency of 10, so at most 40 node requests are in
  flight; each agent runs at most 16 checks at once and refuses more.
- **One probe implementation.** The specs, the public-address policy and
  the probes moved to a database-free crate, `temps-uptime-probe`, which the
  status page plugin and the node agent share. A worker validates the check
  exactly as the control plane does on save and resolves the target through
  its own resolver, so a target that is private from where a worker stands
  is not dialled from there. A worker applies the default outbound policy:
  private networks the operator trusts on the control plane are not trusted
  on workers.
- **Secrets travel sealed.** A check with headers or a body is sent only
  sealed to the node's key (`NodeSealing`), never in plain over an
  `http://` agent address; a node that cannot receive it sealed is listed as
  not answering. A check without either is sent sealed when it can be and
  plain otherwise: the target and the check are shown to anyone who can
  read the monitor.
- **Majority of those that answered.** Down when more than half of the
  answering locations say down; degraded when more than half say down or
  degraded; otherwise operational, so a tie is not a majority and, with the
  control plane and one worker, both must agree. A location that does not
  answer is not counted, and the control plane always answers. The recorded
  response time is the median.
- **Where each answer is kept.** The latest answers of every location go on
  the monitor row (`location_results`, replaced on each check and cleared on
  each edit), not on `status_checks`, a compressed hypertable: one row per
  monitor stays bounded and nothing changes in history storage. Writing them
  does not move `updated_at`, which the scheduler compares to discard checks
  of edited monitors. A down or degraded check's message names up to three
  failing locations; per-location history is not kept.
- **Environment monitors** are checked from the control plane only: their
  address can be internal to the cluster, and a worker's view of it says
  nothing about what visitors see. A follow-up can extend the switch to them.
- **Discoverability.** `GET /projects/{id}/monitors/probe-locations`
  returns the locations a check would use, and `configured: false`, a
  reason and `/settings/nodes` when no worker is online; the monitor dialog
  shows the switch in that onboarding state rather than hiding it.
- **Single-node installs** still cannot report their own outage. An external
  probe offered by Temps Cloud stays the open question below; the docs point
  such installs at an external checker.

### D6. On-call schedules and escalation

**Missing.** A notification route sends to its providers once. There is no
rotation, no "if nobody acknowledges in ten minutes, tell the next person",
and no repeat until acknowledged. Alarms already record who acknowledged and
when, which is the signal escalation needs.

**Shape.** An escalation policy is an ordered list of steps (who, after how
long) attached to a route. A schedule is a rotation of users with overrides.
The escalation timer is a durable job, so it survives a restart.

### D7. Paging channels

**Missing.** Email, Slack and webhook only. No SMS, voice call or mobile push,
and no Microsoft Teams, Discord or Telegram.

**Shape.** Hand off rather than rebuild: first-class PagerDuty and Opsgenie
providers cover phone and SMS for teams that already page, at a fraction of
the cost of operating telephony. Chat providers are small additions to the
existing provider model. Native SMS and voice need a carrier account and come
last.

### D8. Incident timeline and post-mortems

**Missing.** An incident has status updates. It has no merged timeline of the
alarms, deployments and acknowledgements around it, and no post-mortem
document.

## Not doing

- **A syslog listener in the Temps binary.** A collector writing to the HTTP
  endpoint of D1 covers it without another open port.
- **A resident headless browser.** See D3.
- **Our own telephony** before D7's handoff providers exist.

## Open questions

- For single-node installs, whether Temps Cloud should offer an external probe
  (D5). It is the only way such an install learns it is unreachable, and it is
  a hosted dependency that a self-hosted product otherwise avoids.
- Whether log alerts (D1) should evaluate against the line index or the chunk
  store. The answer decides how far back a window can reach on a small host.
- ~~Whether subscribers (D2) may choose individual components of a page, or
  only the whole page.~~ Settled: the whole page (see *Settled while
  implementing D2*).
