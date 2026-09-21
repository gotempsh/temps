# HTTP checks in Temps

The standalone `temps-credential-checks` crate owns detection and verification
traits. This module supplies encrypted persistence, a public HTTPS transport,
project authorization, scheduling, audit events, and notification delivery.

## API

All routes are under `/api/projects/{project_id}`:

| Method | Route | Purpose |
| --- | --- | --- |
| GET / POST | `/http-checks` | Paginated list / create |
| PUT / PATCH / DELETE | `/http-checks/{check_id}` | Replace configuration / enable or pause / delete |
| POST | `/http-checks/{check_id}/run` | Run now, limited to once per 30 seconds |
| GET | `/http-checks/presets` | Reviewed HTTP recipes |
| GET | `/http-checks/capabilities` | Detection coverage and notification setup state |
| GET | `/env-vars/{env_var_id}/history` | Paginated variable activity and verification history |
| POST | `/env-vars/{env_var_id}/detect` | Local candidate detection; never sends credentials |

Read endpoints require environment-read permission. Creating/replacing checks,
manual verification, detection, and deletion require environment-write and
secret-read permissions. All routes enforce project scope and project access.
Pausing/resuming requires environment-write permission.

Configuration and inline credentials are encrypted and write-only. Environment
references use the latest stored value server-side. Responses contain results and
scheduling metadata, never credentials or remote response bodies. The generated
OpenAPI schema describes the complete recipe and result types.

## Per-variable automation and history

Every five seconds the scheduler scans up to 20 unprocessed variables under row
locks. Recognition uses the standalone catalog and automatic preset policy; the
scan marker and encrypted check commit together. Existing variables are included.
Changes invalidate the scan marker. Provider changes replace the automatic recipe,
unrecognized replacements remove it, and paused automatic checks stay paused.
Custom checks take precedence over automatic checks. No general page-level check
configuration is needed: the variable name and Checks cell open its detail page
at `/projects/{slug}/environment-variables/{id}`. Check configuration is a separate
page at the `/checks` suffix; both routes support direct links and browser history.

Database triggers record creation, value rotation (without values), settings
changes, check lifecycle events, and completed verification results. Existing
variables receive a tracking-start event rather than fabricated past activity.
History survives check deletion, is scoped to the variable/project, and is deleted
with the variable. API responses and history never include plaintext or ciphertext.

## Scheduling and alerts

The default interval is one day; configurable intervals range from five minutes
to seven days. Up to four checks execute concurrently per process. Database
leases coordinate workers across processes and expire after 60 seconds.
Configuration changes and environment-value rotation invalidate outstanding
leases and schedule a fresh check; deletion cascades from project or variable.

Alerts fire on a change in non-healthy finding codes. Repeating the same warning
is deduplicated, crossing another expiry threshold produces a new alert, and
recovery produces a notification. An inconclusive result retries after five
minutes and must occur twice before alerting. Failed notification delivery is
retried after five minutes. Delivery has at-least-once semantics: a process crash
between sending an alert and recording its fingerprint can cause a duplicate.
No external notification is sent until notification providers are configured.

## Destination policy

Requests use HTTPS GET or HEAD, validated public DNS addresses pinned for the
request, no redirects or inherited proxy, a 15-second overall transport timeout,
and a 64 KiB response limit. Private/self-hosted endpoints must be exposed through
a public HTTPS hostname to be checked. Automatic selection is limited to reviewed public issuer presets. Custom and
self-hosted destinations require configuration on the variable.

## Verification

- `cargo test --lib -p temps-credential-checks`
- `cargo test --lib -p temps-monitoring`
- `cargo test -p temps-monitoring --test http_checks`
- `E2E_BASE_URL=http://localhost:3024 bunx playwright test --project=chromium-anonymous environment-variable-table.spec.ts` (from `web`)
