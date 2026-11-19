# ADR 073: What logs, uptime and status pages are missing to match Better Stack

## Status

Proposed — 2026-11-19

Related: ADR 021 (multi-node log aggregation, humanized alert notifications),
ADR 025 (unified alarm log), ADR 026 (metric alert label filters), ADR 045 to
047 (global logs stores), ADR 052 and 053 (pluggable health probes,
distributed runtime health).

One of four gap ADRs (071 Plausible, 072 PostHog, 073 Better Stack,
074 Sentry). The target is 90% of what a typical user relies on.

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

### D5. Probes from more than one place

**Missing.** Every probe runs from the Temps host. A single-node install
cannot report its own outage, and nobody can tell a regional network problem
from a real one.

**Shape.** In a multi-node install, workers are probe locations (ADR 053
already distributes health probing). A monitor is down when a quorum of
locations agree, which removes most false alarms. For a single-node install
the only honest answer is a probe from somewhere else; see *Open questions*.

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
- Whether subscribers (D2) may choose individual components of a page, or only
  the whole page.
