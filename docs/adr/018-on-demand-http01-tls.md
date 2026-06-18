---
title: "ADR-018: On-demand HTTP-01 TLS certificate issuance"
status: Accepted
date: 2026-06-18
author: David Viejo
---

# ADR-018: On-demand (lazy) HTTP-01 TLS certificate issuance

**Status:** Accepted
**Date:** 2026-06-18
**Author:** David Viejo

---

## Context

### The sslip.io gap

QuickStart mode (`--mode=local` / `--mode=quick` in `scripts/deploy.sh`) sets
`external_url` to a `*.<ip>.sslip.io` pattern. The instance therefore names all
app routes as `<app>.<ip>.sslip.io`. These hostnames **cannot** get HTTPS today
for two structural reasons:

1. A wildcard cert (`*.<ip>.sslip.io`) requires ACME DNS-01, which requires
   writing a TXT record on the `<ip>.sslip.io` zone. sslip.io is read-only; the
   zone is derived from the hostname itself. DNS-01 is impossible, and the CLI
   already enforces this: `crates/temps-cli/src/commands/domain.rs:479-488`
   rejects wildcard+http-01 at the CLI layer.
2. Per-hostname certs (`myapp.<ip>.sslip.io`) *can* be certified via HTTP-01
   (port 80, token served at `/.well-known/acme-challenge/{token}`), but app
   hostnames are not known at install time. There is no mechanism today that
   provisions a cert the first time a hostname is routed.

The existing HTTP-01 infrastructure is already complete:

- Challenge serving: `crates/temps-proxy/src/proxy.rs:1080-1129`
  (`handle_acme_http_challenge`) intercepts `/.well-known/acme-challenge/{token}`
  before the HTTPS redirect at lines `2992-3025`, serving the key authorization
  from `domains.http_challenge_token` / `domains.http_challenge_key_authorization`.
- Cert loading: `crates/temps-proxy/src/server.rs:48-119`
  (`certificate_callback` / `DynamicCertLoader`) fires on every TLS handshake,
  calls `CertificateLoader::load_certificate(sni)` at `tls_cert_loader.rs:29-50`,
  which looks up the `domains` table for an active cert. When none exists it
  returns `Ok(None)` and the handshake proceeds without a server certificate,
  causing a TLS error at the client.

The **missing piece** is the connection between "cert not found" and "start
provisioning." `CertificateLoader::load_certificate` at `tls_cert_loader.rs:36`
returns `Ok(None)` with no side-effect. No provisioning is triggered; no error
is recorded; the client sees a generic TLS failure.

### Why observability is a first-class requirement

Caddy's on-demand TLS is the canonical precedent, but it has a known operational
weakness: when issuance fails the operator sees only a TLS handshake error at the
client. There is no structured log event, no dashboard entry, no CLI command to
inspect the reason. A misconfigured domain, an expired ACME account, an LE rate
limit breach, or a DNS propagation delay all produce the same symptom. Self-
hosters who hit the sslip.io shared rate-limit ceiling (described below) will be
stuck with no signal.

**On-demand TLS is only worth building if every issuance attempt — successful or
not — produces a visible, queryable record with the full error chain.** This ADR
treats observability as the primary design constraint, not an afterthought.

### Let's Encrypt rate limits on sslip.io (verified June 2026)

Let's Encrypt computes the "registered domain" for rate-limit accounting using
the **Public Suffix List (PSL)** — the eTLD+1. **`sslip.io` is NOT on the PSL.**
Therefore the registered domain for any `<ip>.sslip.io` hostname is **`sslip.io`
itself**, not `<ip>.sslip.io`. Two consequences, both verified against the LE
docs and the sslip.io maintainers' README (cunnie/sslip.io), not assumed:

1. **The per-registered-domain bucket is shared globally** across every sslip.io
   user on the internet. It is NOT isolated per-IP — `<ip>.sslip.io` does not get
   its own bucket because it is not a registered domain.
2. **The cap is NOT the default 50/week.** The sslip.io maintainers have
   repeatedly negotiated elevated limits with Let's Encrypt; the shared
   `sslip.io` bucket currently sits at **~250,000 certificates/week**. This makes
   normal use viable — the "you'll exhaust 50 slots instantly" framing is wrong.

The limits that DO bite per-instance are the per-hostname ones, which the PSL
status does not change:

- **5 duplicate certificates per exact hostname per week.**
- **5 failed authorizations per hostname per hour.**

**Implication for this design:** the shared 250k/week bucket is not the binding
constraint for a single instance. The real risks are (a) being a bad neighbor on
a shared community resource by churning certs for ephemeral hostnames, and (b)
hitting the 5-failed-auth/hour or 5-duplicate/week wall when a single hostname
flaps (redeploy loops, misconfigured envs). Both point to the same rule: **do not
issue on-demand certs for high-cardinality, ephemeral per-deployment hostnames.**
See §2.

### Relationship to ADR-017

ADR-017 (split proxy + console) assigns `TlsRenewal scheduler` to the console
process (see its §4 background-worker table). On-demand issuance is triggered
from the proxy's `certificate_callback` and therefore runs in the proxy process.
Any coordination between the two processes goes through the `domains` table
(PostgreSQL), not shared memory. This ADR is compatible with both all-in-one
and split topologies.

---

## Decision

Introduce **on-demand HTTP-01 TLS** as an opt-in feature of the Temps proxy.
When enabled, the proxy's `certificate_callback` path in
`crates/temps-proxy/src/server.rs` is extended to trigger provisioning for
allowlisted hostnames that have no active cert, rather than silently failing.
Every attempt — successful or not — is persisted as a structured record
queryable from the console UI and CLI.

The following nine sub-decisions make up the full design.

### 1. Handshake strategy: fail-fast on first request, issue in background

Three options exist when `certificate_callback` finds no cert for a hostname:

**Option A — Block the handshake until issuance completes (Caddy-style).**
The callback awaits the full ACME flow (order create → challenge serve → LE
poll → finalize → download). HTTP-01 issuance takes 5-30 seconds. Pingora's
TLS callback runs inside the OS-thread Pingora runtime; a 30-second await
blocks the connection and starves other handshakes sharing the worker thread.

**Option B — Fail-fast on the first handshake; issue in background; retry
succeeds (recommended).** The callback returns immediately with `Ok(None)` (same
as today, no regression). Simultaneously it publishes an `IssueCert` job onto a
bounded in-process channel that is consumed by a background `OnDemandCertManager`
task. Subsequent TLS handshakes for the same hostname also fail until the cert is
ready, but because HTTP-01 completes in seconds and clients retry (browsers do
within 1-5 seconds on TLS errors), the second or third request typically succeeds.
This is structurally identical to how `OnDemandManager` handles scale-to-zero
today (`crates/temps-proxy/src/on_demand.rs`): the first request triggers a side-
effect and fast-fails while the side-effect completes.

**Option C — Serve a self-signed fallback immediately; replace it silently.**
Generates a throwaway RSA key and a self-signed cert on the fly, allowing the
handshake to complete. The client sees a cert error instead of a handshake abort.
Semantically worse for end users (cert-error pages are more alarming than
connection-refused); self-signed cert generation is non-trivial CPU work on every
miss; the cert would need to be replaced atomically mid-session.

**Decision: Option B.** It is non-blocking, requires no cert generation in the
hot path, and aligns with the existing on-demand manager pattern. The first
request to a new hostname fails with a TLS error; within 5-30 seconds the cert
is issued; subsequent requests succeed. This is acceptable for QuickStart mode
where the operator is the first user. The failure-fast behavior is documented
as expected in the console.

### 2. The allowlist gate (anti-DoS control)

Only hostnames that pass an allowlist check trigger issuance. The gate is the
primary defense against random-SNI floods and against provisioning certs for
arbitrary hostnames that happen to reach the proxy.

**Gate logic:** A hostname passes if and only if it is a direct subdomain of the
instance's on-demand zone. The on-demand zone is derived at startup from
`AppSettings.external_url` (stored in the `settings` table, managed by
`crates/temps-config/src/service.rs`):

- If `external_url` matches `*.<ip>.sslip.io` or `http://<ip>.sslip.io`, the
  zone is `<ip>.sslip.io`. Any `<app>.<ip>.sslip.io` passes; `deep.sub.<ip>.sslip.io`
  does not.
- If `external_url` is a custom domain (e.g. `https://paas.mycompany.com`), the
  zone can be extended to `*.mycompany.com` if the operator explicitly opts in via
  `on_demand_tls_zone` in the settings table (see §6).
- If no zone is configured, the gate rejects all SNI, effectively disabling
  on-demand TLS.

The gate is evaluated before any DB lookup, rate-limit check, or job publication.
It is a pure string suffix match: O(1), no I/O, safe to run in the
`certificate_callback` hot path.

The gate also applies a secondary check: the hostname must correspond to a route
in the `CachedPeerTable` (the proxy's in-memory route table). A hostname with a
valid zone suffix but no registered deployment cannot get a cert. This prevents
a random SNI flood from creating `domains` rows for hostnames that will never
serve traffic. The route-table check is also O(1) in-memory.

**Third check — STABLE hostnames only; never per-deployment (critical).**

Temps assigns hostnames at two cardinalities (verified in the codebase):

- **Stable, low-cardinality** — the console host (`console.<base>`) and the
  per-ENVIRONMENT alias (`<project>-<env>.<base>`, e.g. `myapp-production.<base>`),
  stored in `environment_domains` (`crates/temps-environments/src/services/environment_service.rs:234-242`).
  One per environment; lives for the life of the environment.
- **Ephemeral, high-cardinality** — the per-DEPLOYMENT calculated hostname
  (`<project>-<env>-<deployment_id>.<base>`, e.g. `myapp-production-42.<base>`),
  stored in `deployment_domains` with `is_calculated = true`
  (`crates/temps-deployments/src/services/services.rs:2601-2621`). A NEW hostname
  on every push/preview; obsolete on the next deploy.

On-demand TLS MUST issue certs ONLY for stable hostnames. Per-deployment
hostnames are explicitly excluded because:

- They are ephemeral — a cert for `myapp-production-42` is useless once deploy 43
  ships, so each is wasted issuance against the shared `sslip.io` community bucket.
- A redeploy/preview loop on the same hostname trips the LE per-hostname limits
  (5 duplicate/week, 5 failed-auth/hour) described in the Context section.
- On custom domains they never needed a cert: the wildcard cert (`*.<base>`,
  DNS-01) already covers them via the proxy's SNI wildcard fallback
  (`tls_cert_loader.rs:40-46`). The sslip.io case is the ONLY place this gap
  exists, and the fix is to cert the stable env alias, not every deployment.

**Gate implementation:** a hostname passes the third check iff it exists in
`environment_domains` (or is the console host) AND does NOT exist in
`deployment_domains` as a calculated hostname. Because the `certificate_callback`
hot path cannot do a DB lookup, the proxy's in-memory route table
(`CachedPeerTable`) must carry a per-route `is_ephemeral` / `cert_eligible`
flag populated when routes are loaded (from the `is_calculated` column). The gate
reads this flag O(1). Per-deployment routes are marked ineligible at route-load
time; the callback never enqueues an issuance job for them.

**What deployment URLs get instead:** per-deployment hostnames on sslip.io stay
HTTP (served on port 80) or 308-redirect to the stable environment URL
(`<project>-<env>.<base>`), which IS certed. The redirect target is a config
choice surfaced in §6 (`on_demand_tls_deployment_url_mode`:
`http` (default) | `redirect_to_env`).

### 3. Per-host state machine and in-flight deduplication

Each hostname subject to on-demand issuance progresses through defined states,
persisted in the `domains` table (`crates/temps-entities/src/domains.rs`):

```
none (no row)
  |
  v  [certificate_callback fires, gate passes, no row exists]
pending  (row created, status="on_demand_pending")
  |
  v  [OnDemandCertManager picks up job]
issuing  (status="on_demand_issuing")
  |
  +--[ACME HTTP-01 flow succeeds]--> active (status="active", cert+key populated)
  |
  +--[ACME flow fails]-------------> failed (status="on_demand_failed",
                                              last_error populated with full chain,
                                              last_error_type = error category)
                                              backoff_until = now + exponential_delay
```

Transitions:

- `none → pending`: written by `OnDemandCertManager` immediately when the job
  is dequeued, inside a DB transaction with a `WHERE NOT EXISTS` guard to prevent
  duplicate rows from concurrent callbacks.
- `pending → issuing`: written when the ACME order is created.
- `issuing → active`: written when the finalized cert is downloaded and stored.
- `issuing → failed`: written on any ACME error, including LE API errors,
  challenge-serve failures, and timeouts.
- `failed → pending`: written after `backoff_until` has elapsed and a new
  request arrives for the same hostname.
- `active → issuing` (renewal): driven by the existing
  `TlsService::start_certificate_renewal_scheduler` in the console process
  (`crates/temps-cli/src/commands/serve/console.rs:1322-1354`). See §8.

**In-flight deduplication:** The `certificate_callback` does not publish a new
job if the hostname already has a row in `on_demand_pending` or `on_demand_issuing`
state. This check is a read against a small in-process `DashMap<String, OnDemandCertState>`
(mirroring the pattern in `on_demand.rs`) that is kept warm by the
`OnDemandCertManager`. The DB row is the authoritative state; the in-process
map is a cache for the hot path. On proxy restart, the map is rebuilt from the
DB at startup for rows in `pending` / `issuing` / `failed` states within the last
24 hours.

### 4. Rate limiting, abuse safety, and the sslip.io shared ceiling

Four layers of rate control:

**Layer 1 — Concurrent issuance cap.** A bounded `Semaphore` limits the number
of ACME flows running simultaneously. Default: 3 concurrent issuances. This
caps the HTTP-01 challenge-serve load and prevents a burst of new deployments
from overwhelming the ACME client. Configurable via `on_demand_tls_max_concurrent`
in the `settings` table.

**Layer 2 — Per-host negative cache with exponential backoff.** A failed
issuance sets `domains.backoff_until` to `now + delay`. Delay sequence:
5 min → 15 min → 1 hr → 4 hr → 24 hr → 24 hr (capped). The
`certificate_callback` checks the in-process `DashMap` before publishing a job;
if the entry is in `failed` state and `backoff_until` has not elapsed, the
job is not published. This prevents a retry storm from a single misconfigured
domain from consuming LE authorization attempts.

**Layer 3 — Global on-demand issuance rate limiter.** A token bucket (or a
simple counter in the `settings` table refreshed hourly) caps total on-demand
issuances per hour across all hostnames. Default: 10 per hour. When the bucket
is empty, new jobs are rejected and the callback records a
`rate_limited_by_operator` reason in the issuance log. This is separate from the
LE rate limit and is the operator's self-imposed safety net. Configurable via
`on_demand_tls_hourly_cap` in the `settings` table.

**Layer 4 — LE `rateLimited` error handling.** When the ACME server returns
`urn:ietf:params:acme:error:rateLimited`, the hostname is set to `failed` with
`backoff_until = retryAfter` from the LE `Retry-After` header (if present) or
`now + 1 hour` (fallback). The full ACME error body (including `detail` field)
is stored in `domains.last_error`. A `last_error_type = "rate_limited"` field
enables the console to surface a specific message ("Let's Encrypt rate limit
reached — try again after <time>") rather than a generic failure.

**The sslip.io shared ceiling is an operator responsibility.** The design cannot
solve the global shared rate limit. Instead it makes the problem visible:
the console "Certificates" surface (§5) shows the `rate_limited` error with the
LE retry window, and `temps domain list` surfaces it in the CLI. The docs for
QuickStart mode must note the shared rate-limit risk and recommend operators
migrate to a custom domain for production use.

**Random-SNI flood mitigation.** The allowlist gate (§2) stops most floods
before any state is created. For residual load (legitimate zone-matching hostnames
with no route), the route-table check (§2 secondary) prevents row creation. An
explicit per-IP rate limit on `certificate_callback` job publication (max 5 novel
hostnames per source IP per minute, enforced in the in-process map) adds a final
layer.

### 5. Observability (first-class design pillar)

Every on-demand issuance attempt produces a structured audit record. This is not
optional; it is the design requirement stated in the Context section.

**New table: `on_demand_cert_attempts`**

A new entity in `crates/temps-entities/src/on_demand_cert_attempts.rs` with the
following shape (expressed as migration columns, not Rust code):

| Column | Type | Purpose |
|---|---|---|
| `id` | `SERIAL PRIMARY KEY` | |
| `hostname` | `TEXT NOT NULL` | SNI that triggered the attempt |
| `trigger` | `TEXT NOT NULL` | always `"tls_callback"` in this feature |
| `challenge_served` | `BOOL` | did the proxy serve the `/.well-known/acme-challenge/` request? |
| `acme_request_sent` | `BOOL` | did we reach the LE API? |
| `acme_response_status` | `TEXT` | HTTP status or ACME error type from LE |
| `outcome` | `TEXT NOT NULL` | `"issued"`, `"failed"`, `"skipped_duplicate"`, `"skipped_gate"`, `"skipped_rate_limit"`, `"skipped_no_route"` |
| `error_chain` | `TEXT` | full `Display` chain of the error (all `source()` levels) |
| `error_category` | `TEXT` | `"rate_limited"`, `"dns_failure"`, `"acme_order_expired"`, `"challenge_mismatch"`, `"timeout"`, `"internal"`, `null` |
| `duration_ms` | `INT` | end-to-end issuance duration (0 for skipped) |
| `created_at` | `TIMESTAMPTZ NOT NULL` | |

This table is append-only. It is not the authoritative state (that is `domains`);
it is the audit log. Multiple rows per hostname are expected (retries, renewals).

**Console UI: "Certificates" page**

A new route in the console at `/certificates` (or beneath a domain-management
section) shows:

- A table of all hostnames with on-demand certs (or recent attempts), with
  columns: hostname, status (`pending` / `issuing` / `active` / `failed`),
  last attempt time, and outcome.
- Clicking a row shows the full `error_chain` from the most recent
  `on_demand_cert_attempts` row, the ACME response status, whether the challenge
  was served, and the backoff-until timestamp if in failed state.
- Active certs show expiration date and days-to-renewal.
- Failed certs show the `error_category` as a human-readable label ("Rate limit
  from Let's Encrypt — retry after 2026-06-25T00:00:00Z", "Challenge not served —
  is port 80 open?", "DNS lookup failed for hostname") derived from
  `error_category` + `acme_response_status` + `backoff_until`.

**CLI visibility**

`temps domain list` gains an `--on-demand` flag that filters to hostnames in the
on-demand states and renders the `error_category` and `backoff_until` columns.

A new `temps domain cert-status -d <hostname>` command queries the latest
`on_demand_cert_attempts` row for the hostname and prints the full error chain.
This is the operator's first-line diagnostic tool.

**What the end user sees on a failed handshake**

The proxy cannot inject a meaningful HTTP response into a failed TLS handshake —
the handshake failure is below HTTP. The client sees a TLS error (browser: "Your
connection is not private", curl: `SSL_ERROR_HANDSHAKE`). This is unavoidable for
Option B.

To mitigate this:

- The HTTP (port 80) response for the hostname, while issuance is in progress,
  is upgraded to a 503 with a plain-text body: "TLS certificate provisioning in
  progress. Retry in a few seconds." This is served from `request_filter` in
  `proxy.rs` for non-ACME-challenge HTTP requests to hostnames in
  `on_demand_pending` / `on_demand_issuing` state.
- After a `failed` state is entered, the HTTP response changes to: "TLS
  certificate issuance failed. Contact your administrator." The operator can read
  the full error from `temps domain cert-status -d <hostname>`.

This gives the end user a human-readable signal on port 80 without compromising
the TLS handshake path.

**Structured logging**

Every state transition in `OnDemandCertManager` emits a `tracing::info!` or
`tracing::warn!` event with fields: `hostname`, `transition`, `outcome`,
`error_chain`, `duration_ms`. These appear in `journalctl -u temps-proxy` and in
any OTel exporter configured. The log event is emitted in addition to the DB
write, not instead of it.

### 6. Configuration and opt-in

On-demand TLS is **opt-in** with automatic enablement for QuickStart mode.

**Setting model:** A new group of settings columns on the `settings` table (not
environment variables — per CLAUDE.md: "Add new runtime configuration as a column
on the relevant entity row"):

| Column | Type | Default | Purpose |
|---|---|---|---|
| `on_demand_tls_enabled` | `BOOL` | `false` | Master switch |
| `on_demand_tls_zone` | `TEXT NULL` | `null` (auto-derived from `external_url`) | Zone suffix for the allowlist gate |
| `on_demand_tls_max_concurrent` | `INT` | `3` | Concurrent issuance semaphore |
| `on_demand_tls_hourly_cap` | `INT` | `10` | Global per-hour cap |
| `on_demand_tls_deployment_url_mode` | `TEXT` | `"http"` | How ephemeral per-deployment hostnames behave with no cert: `"http"` (serve plain HTTP on :80) or `"redirect_to_env"` (308 to the stable env URL). Per-deployment hostnames are NEVER certed (see §2). |

**Auto-enablement for QuickStart mode:** During `temps setup` (or
`scripts/deploy.sh --mode=quick`), if the derived `external_url` contains
`sslip.io`, `on_demand_tls_enabled` is set to `true` and `on_demand_tls_zone`
is set to the derived `<ip>.sslip.io` suffix. This is the only automatic
enablement; all other install modes require explicit operator opt-in.

**Interaction with `--mode=local` and `disable_https_redirect`:** Local mode
(`127.0.0.1.sslip.io`) sets `disable_https_redirect = true`
(`crates/temps-proxy/src/config.rs:9`). On-demand TLS for local-mode is
purposeless — Let's Encrypt cannot reach `127.0.0.1` for the HTTP-01 challenge.
The startup validation rejects `on_demand_tls_enabled = true` when
`external_url` resolves to a loopback address, emits a warning, and overrides the
setting to `false`.

**Interaction with `temps setup --auto`:** The `--auto` flag today forces
`skip-ssl`. When `--mode=quick` is passed with `--auto`, setup should set
`on_demand_tls_enabled = true` and remove the `skip-ssl` override, since the
point of on-demand TLS is to replace the manual SSL provisioning step. This is
a behavioral change to `scripts/deploy.sh` that the implementer must confirm with
the deploy-script maintainer.

**Operator opt-out:** Setting `on_demand_tls_enabled = false` via
`PATCH /api/settings` immediately stops the `OnDemandCertManager` from accepting
new jobs. In-flight issuances complete. No existing certs are revoked.

### 7. Crate boundaries and module layout

No source code is produced here; the following specifies crate ownership and
module placement for the implementer.

**`crates/temps-proxy`** — primary implementation site:

- New module: `crates/temps-proxy/src/on_demand_cert.rs` — `OnDemandCertManager`
  struct, the bounded issuance job channel, the `DashMap<String, OnDemandCertState>`
  in-process state cache, the gate function (incl. the stable-vs-ephemeral check
  from §2), the per-IP rate limiter, and the global hourly cap token bucket.
  Mirrors the structural pattern of `on_demand.rs`.
- Modified: the proxy route table (`CachedPeerTable` / route-load path) — each
  route gains a `cert_eligible: bool` flag, set `false` for per-deployment
  calculated hostnames (`deployment_domains.is_calculated = true`) and `true` for
  stable env/console hostnames. This is what lets the hot-path gate exclude
  ephemeral hostnames with no DB lookup (§2, third check). The route loader that
  populates the table must join/derive this from the deployment-vs-environment
  domain source when building routes.
- Modified: `crates/temps-proxy/src/server.rs` — `DynamicCertLoader` gains an
  `Option<Arc<OnDemandCertManager>>` field; `certificate_callback` calls
  `on_demand_cert_manager.try_enqueue(sni)` when `load_certificate` returns
  `Ok(None)`.
- Modified: `crates/temps-proxy/src/tls_cert_loader.rs` — no structural change;
  `load_certificate` remains a pure DB lookup. The `OnDemandCertManager` is not
  injected here — it stays in `DynamicCertLoader` to avoid coupling the loader
  to issuance logic.
- Modified: `crates/temps-proxy/src/proxy.rs` — `request_filter` gains the 503
  "provisioning in progress" / "issuance failed" HTTP response for non-ACME-
  challenge HTTP requests to hostnames in on-demand states.
- Modified: `crates/temps-proxy/src/config.rs` — `ProxyConfig` gains
  `on_demand_cert_manager: Option<Arc<OnDemandCertManager>>`.

**`crates/temps-domains`** — ACME orchestration:

- `OnDemandCertManager` calls `DomainService::provision_on_demand(hostname)` — a
  new method on `crates/temps-domains/src/domain_service.rs` that encapsulates
  the full two-step ACME flow (create order → await HTTP-01 challenge →
  finalize → store). This reuses the existing `CertificateProvider` trait
  (`tls/providers.rs`) and `CertificateRepository` trait (`tls/repository.rs`).
  It does not duplicate the ACME client — the same `LetsEncryptProvider` used by
  manual provisioning drives on-demand issuance.
- `DomainService::provision_on_demand` writes `on_demand_cert_attempts` rows
  throughout the flow (start, challenge-served, outcome).

The proxy crate therefore depends on `temps-domains` (which it already does
indirectly). The ACME logic stays in `temps-domains`; the proxy contributes only
the trigger, gate, and in-process state cache.

**`crates/temps-entities`** — new entity:

- `crates/temps-entities/src/on_demand_cert_attempts.rs` — new entity for the
  audit log table.
- `crates/temps-entities/src/domains.rs` — add columns: `on_demand_backoff_until`
  (`Option<DBDateTime>`) to support the negative cache. The existing `status`,
  `last_error`, and `last_error_type` fields accommodate on-demand states without
  schema change; the new `status` values (`on_demand_pending`, `on_demand_issuing`,
  `on_demand_failed`) are string-typed and backward-compatible.

**`crates/temps-migrations`** — two new migration files:

- `m20260618_000001_on_demand_cert_attempts.rs` — creates `on_demand_cert_attempts`.
- `m20260618_000002_domains_on_demand_columns.rs` — adds `on_demand_backoff_until`
  to `domains`; adds `on_demand_tls_enabled`, `on_demand_tls_zone`,
  `on_demand_tls_max_concurrent`, `on_demand_tls_hourly_cap` to `settings`.

**Console UI** — new route under `web/`:

- New page component for `/certificates`, consuming a new API endpoint
  `GET /api/domains/on-demand-certs` that returns paginated rows from
  `on_demand_cert_attempts` joined with current `domains.status`.
- This is console-only. In split topology (ADR-017) the console process owns
  this UI and API; the proxy does not serve it.

**`crates/temps-cli/src/commands/proxy.rs` and `serve/proxy.rs`** — wiring:

- `OnDemandCertManager` is constructed in both the standalone `temps proxy` startup
  path and in `serve/proxy.rs` (the monolith path), conditioned on
  `on_demand_tls_enabled` read from the `settings` table at startup. The manager
  is passed into `setup_proxy_server` alongside `OnDemandManager`.
- In split topology, `OnDemandCertManager` lives in the proxy process
  (it issues certs in response to handshakes). The cert renewal scheduler stays
  in the console process (as today per ADR-017 §4).

### 8. Renewal of on-demand certs

On-demand certs are standard Let's Encrypt DV certificates with 90-day lifetimes.
They are renewed by the existing `TlsService::start_certificate_renewal_scheduler`
(`crates/temps-domains/src/tls/service.rs:903`) running in the console process.
This scheduler already calls `renew_expiring_certificates` for all domains with
`status = "active"`.

No change is required to the renewal path. The only prerequisite is that
on-demand-issued certs are stored in the `domains` table with `status = "active"`
after successful issuance — which is already the storage contract defined by
`DomainService` (see `domain_service.rs:444`).

Renewal is HTTP-01 (the method used for original issuance). The proxy's existing
challenge-serving infrastructure (`proxy.rs:2992-3025`) handles the renewal
challenge. Since the domain's cert is already active at renewal time, the
`disable_https_redirect` check does not interfere.

For on-demand certs in the `failed` state at renewal time (e.g. a cert that was
issued but later entered a failed renewal cycle), the existing `last_error` /
`last_error_type` columns already capture the failure. The console "Certificates"
UI surfaces this alongside first-issuance failures.

### 9. Interaction with the ADR-017 split topology

In the split topology:

- `OnDemandCertManager` runs in the **proxy process** (`temps proxy`). It holds
  the `DomainService` reference needed to drive ACME issuance. The proxy already
  has a DB connection (`Arc<DbConnection>`) and the `EncryptionService` (for key
  storage) — the same dependencies `DomainService` needs.
- The **cert renewal scheduler** stays in the **console process**, as ADR-017 §4
  assigns it. There is no conflict: on-demand issuance is triggered by handshakes
  (proxy), renewal is triggered by a cron-style scheduler (console).
- Both processes write to the same `domains` table. The `on_demand_cert_attempts`
  table is written only by the proxy process. The console reads it for the UI.
  No cross-process locking is needed — rows are append-only.
- Schema-skew risk (ADR-017 §Consequences): migrations adding `on_demand_*`
  columns to `domains` and `settings`, and the new `on_demand_cert_attempts`
  table, must be additive-only (nullable columns, new table) and backward-
  compatible with the N-1 proxy binary. The proxy's `OnDemandCertManager` reads
  the `on_demand_tls_enabled` setting at startup; if the column does not exist
  (pre-migration proxy), it defaults to `false` (feature disabled). This is the
  standard pattern for the split topology migration window.

---

## Consequences

### Positive

- QuickStart (`*.sslip.io`) users get automatic per-hostname HTTPS on first
  request without any manual `temps domain add` / `temps domain provision` steps.
- Every issuance failure is a queryable record, not an opaque TLS error.
  Operators can diagnose cert failures without correlating proxy logs.
- The 503 on port 80 during provisioning is a human-readable signal for end users.
- Reuses all existing infrastructure: `LetsEncryptProvider`, `DomainService`,
  challenge-serving in `proxy.rs`, renewal scheduler in the console.
- Off by default — no behavioral change for operators who do not set
  `on_demand_tls_enabled = true`.

### Negative

- First request to a new hostname fails with a TLS handshake error. This is
  inherent to Option B and unavoidable without blocking the handshake.
- The proxy process now drives ACME issuance (calling `DomainService`), which
  adds a network-I/O dependency (outbound HTTPS to Let's Encrypt) to the proxy's
  runtime. An LE API outage or slow response does not block request serving
  (the issuance job is background), but it does mean uncertified hostnames remain
  uncertified until LE recovers.
- Adds `temps-domains` as a compile-time dependency of the proxy process.
  `temps-domains` already depends on Sea-ORM, the ACME client library, and
  `temps-entities`. This increases proxy binary size and compile time.
- `on_demand_cert_attempts` is a write-heavy append-only table. With many
  deployments being provisioned simultaneously, it can grow quickly. A TTL-based
  cleanup job (retain last 90 days) should be added to the console's background
  maintenance tasks.

### Risks

**Headline risk: the shared sslip.io Let's Encrypt bucket — mitigated by scope,
not eliminated.** Per the corrected Context section, the `sslip.io` registered-
domain bucket is shared globally but sits at ~250k certs/week, so a single
instance issuing a handful of stable-hostname certs is negligible. The risk is
NOT "exhaust 50 slots"; it is (a) being a bad neighbor by churning certs, and (b)
tripping the per-hostname limits (5 duplicate/week, 5 failed-auth/hour) on a
flapping hostname. The **stable-hostnames-only gate (§2)** is the primary control
for both: by never issuing for ephemeral per-deployment hostnames, the instance's
issuance count stays ~1 per environment. The hourly cap, per-host backoff, and
negative cache remain as defense-in-depth against a misconfigured stable host.
The `rate_limited` error category surfaces any LE limit clearly in the console.
The long-term mitigation for full app HTTPS remains: migrate to a custom domain
with a DNS-01 wildcard, which covers all subdomains with one cert.

**Port 80 dependency.** HTTP-01 requires that Let's Encrypt can reach the proxy
on port 80 from the public internet. Operators behind a NAT, firewall, or
corporate proxy may have port 80 blocked. The startup validation should check
`on_demand_tls_enabled && !port_80_bound` and warn, but cannot verify external
reachability. The console should surface "Last challenge: not served" from
`on_demand_cert_attempts.challenge_served = false` to help diagnose this.

**Issuance in the proxy process adds an operational risk.** If `DomainService`
or the ACME client panics (unlikely but possible), it runs in a tokio task
inside the proxy process. The `OnDemandCertManager` must catch panics from
issuance tasks (using `tokio::task::JoinHandle::await` which converts a panic
into a `JoinError`) and record them as `failed` outcomes. A panic in an issuance
task must not propagate to the Pingora worker threads.

**Stale in-process state cache.** If the proxy process restarts, the `DashMap`
is rebuilt from the DB. During the rebuild window (startup), a flood of
handshakes for hostnames in `on_demand_pending` / `on_demand_issuing` state
could enqueue duplicate jobs. The DB-level `WHERE NOT EXISTS` guard on row
creation prevents duplicate ACME orders, but the channel may be flooded with
no-op jobs. The `try_enqueue` method should DB-check as a fallback when the
cache is cold (the first few seconds after startup).

---

## Alternatives Considered

### A. Status quo: manual per-host provisioning only

Operators run `temps domain add -d myapp.1.2.3.4.sslip.io -c http-01` and
`temps domain provision -d myapp.1.2.3.4.sslip.io` before first traffic.

Pros: no complexity, no rate-limit risk, fully controllable.

Cons: QuickStart mode is the operator's first experience and it is HTTP-only,
which is an unacceptable first impression. Operators deploying 10 apps must run
20 commands. This does not scale.

### B. Console-only issuance: issue certs at deployment creation time

When a deployment is created, the console (not the proxy) provisions a cert for
its hostname via the existing `DomainService` pipeline.

Pros: no hot-path involvement; issuance happens well before the first request;
no TLS handshake failures.

Cons: adds latency to deployment creation (30+ seconds for cert issuance).
Deployment creation is already a long operation; adding a mandatory 30-second
ACME flow would hurt DX. Also, the hostname may not be DNS-resolvable at
deployment creation time (especially for preview environments using non-sslip.io
patterns). Finally, this consumes LE certificates for deployments that may
never receive traffic, worsening the rate-limit problem.

### C. Wildcard cert via a real DNS provider instead of sslip.io

Recommend operators use a custom domain with a DNS provider that supports ACME
DNS-01 (Cloudflare, Route 53, etc.). Issue a wildcard `*.myapp.example.com` cert
via DNS-01, covering all subdomain hostnames automatically.

Pros: no per-hostname cert issuance; wildcard covers all subdomains; no LE rate-
limit per hostname; works offline (DNS-01 does not require port 80).

Cons: requires the operator to own a custom domain and configure a DNS provider.
This is not QuickStart mode. DNS-01 support for the most common providers is
partially implemented in `crates/temps-domains/src/dns_provider.rs` but is not
yet a polished zero-config experience. This is the right long-term path for
production deployments, not for the QuickStart first-run experience.

### D. TLS-ALPN-01 instead of HTTP-01

ACME TLS-ALPN-01 (RFC 8737) uses the TLS handshake itself to serve the
challenge, eliminating the port 80 dependency. The proxy would serve a
special certificate with the ACME key authorization encoded in a certificate
extension during the challenge handshake.

Pros: no port 80 required; single-port TLS operation; no HTTP challenge serving.

Cons: requires implementing RFC 8737 TLS extension handling in the Pingora TLS
callback — a non-trivial implementation. The challenge cert must be served for
exactly one handshake on the challenge hostname, then replaced with the real cert.
Pingora's `TlsAccept` API does not expose the ALPN negotiation result during
`certificate_callback`, which would require patching or wrapping Pingora to
detect the `acme-tls/1` ALPN protocol. The existing HTTP-01 infrastructure in
`proxy.rs:2992-3025` is production-proven and simpler to extend. TLS-ALPN-01 is
a future option if port-80 reachability proves to be a blocking problem in
practice.

---

## Implementation Notes

**Affected crates:**
- `crates/temps-proxy` — new module `on_demand_cert.rs`; modified `server.rs`,
  `proxy.rs`, `config.rs`, `tls_cert_loader.rs`
- `crates/temps-domains` — new `DomainService::provision_on_demand` method
- `crates/temps-entities` — new `on_demand_cert_attempts.rs`; modified `domains.rs`
- `crates/temps-migrations` — two new migration files
- `crates/temps-cli/src/commands/proxy.rs` and `serve/proxy.rs` — wiring for
  `OnDemandCertManager` construction
- `crates/temps-cli/src/commands/domain.rs` — new `cert-status` subcommand
- Console `web/` — new Certificates page

**Migration needed:** Yes. Two additive migrations (new table, new nullable
columns). Backward-compatible with N-1 proxy binary.

**Breaking changes:** No. Feature is opt-in. All existing behavior is unchanged
when `on_demand_tls_enabled = false` (the default).

**Security review required:** Yes. On-demand TLS involves outbound HTTPS from the
proxy to Let's Encrypt, DB writes from the proxy hot path, and a new allowlist
gate whose bypass would allow cert issuance for arbitrary hostnames. The gate
logic, the DB write path, and the `DomainService::provision_on_demand` method
must be reviewed by `security-auditor` before shipping.

---

## References

- `crates/temps-proxy/src/proxy.rs:1080-1129` — `handle_acme_http_challenge()`
- `crates/temps-proxy/src/proxy.rs:2992-3025` — ACME challenge intercept before
  HTTPS redirect
- `crates/temps-proxy/src/server.rs:41-143` — `DynamicCertLoader` /
  `certificate_callback` / `handshake_complete_callback`
- `crates/temps-proxy/src/tls_cert_loader.rs:27-50` — `CertificateLoader::load_certificate`
  (the `Ok(None)` return that is the hook point for on-demand)
- `crates/temps-proxy/src/on_demand.rs` — structural precedent for lazy
  action triggered from a hot path (`OnDemandManager`, `DashMap` state cache,
  `Semaphore`, background task pattern)
- `crates/temps-proxy/src/config.rs:9` — `disable_https_redirect` field
- `crates/temps-domains/src/domain_service.rs` — `DomainService::create_domain`,
  `provision_domain`, and existing ACME order orchestration
- `crates/temps-domains/src/tls/service.rs:903` — `start_certificate_renewal_scheduler`
  (the renewal path on-demand certs integrate with)
- `crates/temps-domains/src/plugin.rs:99` — comment noting renewal scheduler is
  started in `console.rs`
- `crates/temps-entities/src/domains.rs` — `domains` table entity (status,
  last_error, last_error_type, http_challenge_token, http_challenge_key_authorization)
- `crates/temps-entities/src/acme_orders.rs` — ACME order entity
- `crates/temps-cli/src/commands/domain.rs:479-488` — wildcard+http-01 rejection
  (confirms per-hostname HTTP-01 is valid, wildcard is not)
- `crates/temps-config/src/service.rs:543-552` — `get_url_scheme`, `external_url`
  sslip.io detection (used to derive the on-demand zone)
- ADR-017 §4 — background worker assignment table (renewal scheduler = console;
  on-demand cert issuance = proxy)
