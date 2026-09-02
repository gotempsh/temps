<!-- SCOPE: Defines how a Traefik-discovered route obtains, imports, serves, and renews a TLS certificate without letting a container's own labels drive issuance. -->

# ADR-041: TLS certificate handling for Traefik-discovered routes

**Status:** Proposed
**Date:** 2026-08-31
**Author:** Temps Contributors

> Security-sensitive: this ADR introduces an endpoint that accepts private key
> material. Revision 2 incorporates the required changes from the
> `security-auditor` review ("approved with required changes"); the review's
> eight findings are addressed in §2, §3, §4, §5, §6, §7, and the Risks and
> Verification sections. Implementation may not start until this revision is
> re-confirmed.

## Context

Live Traefik-label discovery (shipped on this branch) lets Temps' Pingora proxy
route to containers Temps did not deploy. The reconciler in
`crates/temps-deployer/src/traefik_discovery.rs` watches a Docker network for
`traefik.enable=true` plus `traefik.http.routers.<n>.rule=Host(...)`, persists
matches into `traefik_discovered_routes`, and `load_routes()` merges them into
the live route table ("Section 6" of `crates/temps-routes/src/route_table.rs`).
The selling point is a one-line migration: stop the Traefik container, start
Temps, keep the labels.

### The hardening fix and its operational cost

Pre-merge security review found that mirroring a container's own
`traefik.http.routers.<n>.tls=true` label into `RouteInfo::cert_eligible` let
any container on the watched network drive ACME issuance for a hostname *it*
chose. `route_table.rs:1939` therefore hardcodes `cert_eligible: false` for
every discovered route. That fix is correct and this ADR does not revisit it.

It has a consequence nobody has addressed. The operator this feature is aimed
at — someone migrating a docker-compose / Coolify / Dokploy stack where Traefik
was terminating HTTPS — hands `:80`/`:443` to Temps and every HTTPS visitor
immediately gets a failed TLS handshake, because Temps holds no certificate for
that hostname and will never obtain one on its own. HTTP routing works; HTTPS
goes dark. For any stack with a real domain (i.e. most of them) the advertised
migration is a production outage, and the failure mode is the worst kind:
silent from the server's perspective, total from the visitor's.

*(INFO-1, recorded during security review: this premise is not universal.
`tls_cert_loader` falls back to a `*.parent` wildcard row (`wildcard_for`), and
`reserved_hosts()` does not consult the `domains` table at all — so an operator
who already holds `*.example.com` is serving HTTPS for a discovered
`app.example.com` today, with no `cert_eligible` and no import involved. That is
a pre-existing property of the base feature, not something this ADR introduces,
but it compounds finding 1 below and must be verified at code review.)*

### What already exists (verified in code, not assumed)

`crates/temps-domains/src/tls/` is a mature ACME service already used for
arbitrary operator-owned custom domains. The relevant, confirmed facts:

| Fact | Location |
| --- | --- |
| `Certificate` is persisted in the `domains` table; one row per hostname, `domain` UNIQUE | `tls/models.rs` `From<domains::Model>`, `tls/repository.rs:133` |
| `save_certificate` encrypts `private_key_pem` via `EncryptionService` before the upsert and decrypts on every read | `tls/repository.rs:133-177`, `:118` |
| That upsert's `update_columns` includes `VerificationMethod`, so a save can rewrite a row's renewal method | `tls/repository.rs:154-168` |
| The proxy serves a cert by exact `domains.domain` match (falling back to a `*.parent` row); status is deliberately **not** filtered, so any row holding cert+key serves TLS | `temps-proxy/src/tls_cert_loader.rs:366`, `:468` |
| Worker/edge cert sync *is* status-filtered, on `CERT_SERVING_STATUSES` (`active`, `active_renewal_failed`) | `temps-deployments/src/handlers/nodes.rs:1751` |
| `extract_cert_ders` = `rustls_pemfile::certs`, document order preserved, element 0 is the end-entity cert | `tls_cert_loader.rs:416` |
| `extract_key_der` accepts exactly three PEM key encodings: PKCS#1, PKCS#8, SEC1 | `tls_cert_loader.rs:433` |
| A daily scheduler (03:00 UTC, plus once at startup) calls `check_and_renew_certificates(30)` | `tls/service.rs:1334` |
| It renews everything with `expiration_time <= now + 30d`, **regardless of status** | `tls/repository.rs:350` |
| Renewal dispatch reads `verification_method` as a plain string and matches exactly two values: `"http-01"`, `"dns-01"`. Everything else hits `_ => warn!` and is silently never renewed | `tls/service.rs:490-505` |
| **`generate_certificate_from_order` stamps `verification_method: "acme"` on every certificate it returns** — and it is reached from the live `POST /domains/{domain}/provision` handler via `TlsService::provision_certificate`, as well as from `complete_http_challenge` | `tls/providers.rs:446-490`, `:816`, `:947`, `domain_handler.rs:671` |
| `DomainService::complete_challenge`, by contrast, updates the `domains` row directly and preserves the `http-01`/`dns-01` value `create_domain` set | `domain_service.rs:~630` |
| HTTP-01 renewal is order-based when a `DomainService` is wired; failures leave a recoverable order in the UI | `tls/service.rs:628` |
| DNS-01 renewal auto-publishes TXT only when a **verified, `auto_manage`** `dns_managed_domains` zone authoritatively covers the host; otherwise it degrades to a manual alarm | `tls/service.rs:699`, `:747` |
| DNS provider association is therefore **per-zone**, resolved by longest authoritative suffix | `temps-dns/src/services/provider_service.rs:920`, `:946` |
| Failure surfaces exist: `send_renewal_failure_notification` (`TlsRenewalFailed`, Critical) and `send_manual_renewal_notification` (`TlsCertExpiring`) | `tls/service.rs:1104`, `:1139` |
| The custom-domain flow decides HTTP-01 vs DNS-01 by **the operator declaring it**; `create_domain` accepts only `http-01`/`dns-01` | `domain_service.rs:158` |
| **`create_domain` is not idempotent** — an existing row returns `InvalidDomain("Domain {} already exists")` | `domain_service.rs:170` |
| `request_challenge` derives the challenge type from the **stored** `verification_method`, not from any argument | `domain_service.rs:296` |
| `TlsService::request_certificate_provisioning` / `request_certificate_renewal` are **no-op stubs** with their queue bodies commented out | `tls/service.rs:1189-1213` |
| `x509-parser 0.18` is already a direct dependency of `temps-domains` | `temps-domains/Cargo.toml:50`, `tls/providers.rs:493` |
| `DefaultBodyLimit::max(...)` as a route-level layer is the established body-cap pattern | `temps-error-tracking/src/sentry/handlers.rs:102`, `source_map_handlers.rs:73` |
| `Role::User` holds `DomainsCreate` but **not** `SettingsWrite`; `SettingsWrite` is Admin / PlatformAdmin only | `temps-auth/src/permissions.rs:769`, `:919`, `:1059` |

Two further facts shape the design:

- **`on_demand_cert.rs` is not the mechanism to reuse.** Its `try_enqueue`
  Check 1 restricts issuance to *direct subdomains of the configured on-demand
  zone* (ADR-018 §2), which is exactly what a migrated third-party domain is
  not. `DomainService::provision_on_demand` carries an explicit caller
  invariant (`domain_service.rs:988-999`) forbidding new callers that have not
  re-applied that gate.
- **Discovered rows do not survive container churn.** `handle_container_event`
  deletes every row for a container on `die`/`stop`/`destroy`
  (`traefik_discovery.rs:598`), and the reconciler deletes hosts that vanish
  from the network. The upsert deliberately excludes `enabled` from its
  `update_columns` (`:858`) so a *live* container's suppression survives a
  reconcile — but a `docker compose down && up` deletes and re-inserts the row
  with `enabled = true`. Any operator decision stored on that table is
  therefore only as durable as the container's uptime, which is unacceptable
  for a decision that authorizes minting a publicly-trusted certificate.

### The forces

1. A container must never be able to cause certificate issuance for a hostname
   it names. This is non-negotiable and is the reason `cert_eligible` is false.
2. The operator must be able to get HTTPS for a discovered host without hand-
   editing the database or re-deriving the domain through an unrelated screen.
3. HTTP-01 has a chicken-and-egg problem at cutover: it needs Temps to already
   be receiving `:80` traffic for the host, which only happens *after* the
   proxy swap that breaks HTTPS.
4. Traefik already holds a valid certificate and key for that exact hostname in
   its `acme.json`. Reusing it makes the cutover window zero.
5. Certificates Temps did not request must still renew, or the outage is merely
   deferred by up to 90 days — and a renewal that cannot proceed must say so
   loudly rather than expire quietly.
6. **Once a certificate exists for a discovered host, the identity of the
   container that currently owns that host stops being a routing detail and
   becomes an impersonation boundary.** See §2 and finding 1 in Risks.

## Decision

Temps gains **two operator-initiated paths to a certificate for a discovered
host**, both terminating in the *existing* `domains` table via the existing
`CertificateRepository`, and both gated on an explicit, durable operator
authorization that is stored independently of the container's lifetime.

No parallel certificate store, no second renewal loop, no new ACME client.

### Delivery boundary for the current implementation

In scope:

- Operator-triggered ACME issuance (HTTP-01 or DNS-01) for a discovered host.
- Import of an existing certificate + key from a Traefik v2/v3 `acme.json`
  document, for a discovered host.
- A durable per-host TLS authorization record — including the container
  identity it was granted against — and its surfacing in the discovery
  status/list API, the CLI, and the settings page.
- Eager DNS-01 zone validation at declaration time (§8).
- Correcting renewal dispatch for the `verification_method` values already
  present in production, then making a genuinely unknown value visible (§7).

Explicitly out of scope, stated here the way `traefik_labels.rs` states the
label-grammar boundary:

- **KV-backed Traefik ACME stores (Consul/etcd/ZooKeeper/boltdb).** Traefik v2
  and v3 Community Edition store ACME state in a JSON file
  (`certificatesResolvers.<n>.acme.storage`); distributed KV ACME storage was a
  Traefik v1 / Traefik Enterprise concern. Supporting it would mean shipping
  three KV clients to read data the operator can trivially export. If an
  operator has such a store, they extract the PEM material themselves and use
  the same import endpoint. **v1's flat `acme.json` layout is likewise not a
  supported input**; see §4 for how the parser degrades rather than guesses.
- **Wildcard certificates.** A discovered host can never be a wildcard —
  `normalize_host` rejects any `*` (`traefik_labels.rs:361`) — but Traefik may
  well hold `*.example.com` covering it. Importing that would write an
  `is_wildcard` `domains` row that serves **every** subdomain of the zone, a
  scope escalation from one authorized host to a whole namespace, and would
  force DNS-01 renewal with a provider Temps may not have. Wildcard entries are
  rejected at import with a message pointing at the existing `POST /domains` +
  DNS-01 flow, which already does this properly.
- **Automatic issuance of any kind.** Nothing in this ADR issues a certificate
  without a human request. In particular `cert_eligible` stays `false`.
- **Traefik middlewares, redirect-to-HTTPS routers, and TLS options.** Out of
  the label grammar already; out of scope here.

### 1. `cert_eligible` stays `false`; the container's `tls` label stays informational

`route_table.rs:1939` is not a temporary state to be undone. It is the
permanent answer to "can a container's label cause issuance?", and the answer
is no. The `TODO(security)` comment there is reworded to reference this ADR and
its `cert_eligible: false` becomes a documented invariant with a regression
test, not a placeholder.

This is safe precisely because `cert_eligible` and *serving* are independent:
the proxy loads a certificate by looking up the `domains` row for the SNI
(`tls_cert_loader.rs:366`) and never consults `RouteInfo::cert_eligible`. A
discovered host with an operator-authorized `domains` row serves HTTPS
correctly while remaining permanently ineligible for on-demand issuance.

*(INFO-4, recorded during security review: that independence cuts both ways.
`load_routes()` re-applies route precedence on every reload, so a discovered
row that should never have been adopted is caught a second time at load time.
There is no equivalent last line of defense for certificates: nothing between
`domains` and the TLS handshake re-checks whether the host is still legitimately
served by whoever authorized it. The authorization gate in §3/§5 is therefore
the only check, which is why finding 1's container-identity tracking is
mandatory rather than nice-to-have.)*

The `discovered.tls` column keeps its current role: a diagnostic that tells the
operator "Traefik was terminating TLS here, so this host probably needs a
certificate" — surfaced in the UI as a prompt, never as a trigger.

### 2. Authorization is a durable, host-keyed record, bound to a container identity

New table `traefik_route_certificates`, keyed by host, **not** columns on
`traefik_discovered_routes`:

```
traefik_route_certificates
  id                        SERIAL PRIMARY KEY
  host                      VARCHAR NOT NULL UNIQUE     -- normalized, lowercased
  cert_authorized           BOOLEAN NOT NULL DEFAULT FALSE
  authorized_at             TIMESTAMPTZ
  authorized_by_user_id     INTEGER REFERENCES users(id) ON DELETE SET NULL
  authorized_network        VARCHAR NOT NULL            -- discovery network at authorization time
  authorized_container_id   VARCHAR NOT NULL            -- full container ID at authorization time
  authorized_container_name VARCHAR NOT NULL            -- container name at authorization time
  container_drift_detected_at TIMESTAMPTZ               -- set when the serving container no longer matches
  renewal_method            VARCHAR NOT NULL            -- CHECK IN ('http-01','dns-01')
  source                    VARCHAR NOT NULL            -- CHECK IN ('acme','imported')
  certificate_id            INTEGER REFERENCES domains(id) ON DELETE SET NULL
  imported_at               TIMESTAMPTZ
  created_at                TIMESTAMPTZ NOT NULL DEFAULT NOW()
  updated_at                TIMESTAMPTZ NOT NULL DEFAULT NOW()
```

Rationale for each decision:

- **Separate table, not columns on `traefik_discovered_routes`.** That table's
  rows are deleted whenever the container stops. Authorization to hold a
  publicly-trusted certificate for a hostname must not be silently revoked —
  nor silently re-granted — by `docker compose restart`. Keying on `host` means
  the record survives container replacement and stays attached to the thing it
  is actually about.
- **`cert_authorized`, never named `enabled`.** `enabled` answers "route HTTP
  traffic here"; `cert_authorized` answers "the operator accepts responsibility
  for a certificate for this hostname". A route is routable over HTTP without
  being cert-authorized, and (transiently, while the container is down) a host
  can be cert-authorized with no discovered route.
- **`renewal_method` is CHECK-constrained to the two values the renewal
  dispatcher understands** (`tls/service.rs:490`). A third value would produce a
  certificate that is never renewed. The constraint makes that unrepresentable
  and is mirrored verbatim into `domains.verification_method`.
- **`certificate_id` FK to `domains(id)`** is the reference to the resulting
  `Certificate` row, so the UI can show expiry/status without a lookup by
  string.
- **No trigger.** Nothing in this table feeds the route table, so it must not
  fire `notify_route_table_change()`. Certificate changes reach the proxy
  through the cert-loader cache path, not a route reload.

#### 2a. Container identity is captured, and drift is a first-class visible state

**The problem (security review finding 1, HIGH).** `upsert_candidate`'s
`OnConflict` is on `host` alone (`traefik_discovery.rs:858`), and the
reconciler's collision tie-break is
`ordered.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)))`
(`:453`) — lowest container name wins. So a *differently-named* container that
claims the same `Host()` rule takes the hostname over on the next reconcile
pass, within ~30 seconds. For HTTP-only routing this is pre-existing, accepted
behaviour, visible to the operator through the `contested_by` conflict list.

Once this ADR ships it stops being equivalent. `tls_cert_loader::find_certificate_raw`
looks a certificate up **by SNI alone** and never consults the route table or
which container currently owns the host. A takeover that previously moved
plaintext HTTP traffic now moves HTTPS traffic terminated with a real,
publicly-trusted certificate — full impersonation, with a green padlock, of a
hostname the operator authorized for a different workload.

**The decision.** Authorization is recorded against the container identity that
held the host at authorization time, and divergence is surfaced, but
`cert_authorized` does **not** auto-clear.

- On authorization (Path A or Path B), `authorized_container_id` and
  `authorized_container_name` are captured from the discovered row.
- A check runs on every discovery reconciliation pass and on every read of the
  discovery status/list API: for each host with `cert_authorized = true`,
  compare the current `traefik_discovered_routes` row's
  `target_container_id`/`target_container_name` against the authorized values.
- On mismatch: set `container_drift_detected_at`, fire a **Critical** alarm
  (reusing the existing alarm machinery, not a new one), and expose a distinct
  state in the API and console — *"certificate-authorized, but this host is now
  served by a different container"* — naming both the authorized and the
  current container, with the timestamp. The discovered-route response's
  `tls_certificate` block carries `container_drift: true` plus both identities.
- Once drift is detected, further reconciliation passes must not re-fire the
  alarm for the same (host, current container) pair; clearing drift requires an
  explicit operator action (re-authorize against the new container, or
  deauthorize).

**Why not auto-clear `cert_authorized` on drift.** Auto-clearing was considered
and rejected, and the reasoning must be recorded because it is the less
obviously-safe choice:

1. **It would not remove the certificate.** `cert_authorized` governs renewal
   and future issuance; the `domains` row and its key are what actually serve
   TLS, and `find_certificate_raw` ignores this table entirely. Clearing the
   flag would produce the *appearance* of a mitigation while the impersonating
   container keeps being served with the same certificate. A control that looks
   like it fixed the problem and did not is worse than a loud alarm.
2. **The common cause is benign and self-inflicted.** A `docker compose up
   --force-recreate`, an image rebuild, or a rename changes the container ID
   (and sometimes the name) for the operator's *own* service. Auto-clearing
   would silently stop renewing certificates across routine redeploys, and the
   operator would discover it 60 days later as an expiry.
3. **Auto-clearing is itself a denial-of-service primitive.** A hostile
   container on the watched network that wins the name tie-break could, by
   existing for one reconcile pass, permanently deauthorize a legitimate host's
   renewals.

The honest control is therefore: make the takeover impossible to miss
(Critical alarm plus a distinct, persistent UI state), and require a human to
decide whether it was a redeploy or an attack. Deleting the certificate, if it
was an attack, is done through the existing domain-deletion endpoint.

**Known limitation, deliberately not fixed here:** `enabled = false` has the
same durability problem — an operator-suppressed route reappears enabled after
its container restarts. Pre-existing defect in the shipped feature, not
introduced here; this table is its natural future home.

### 3. Path A — operator-triggered ACME issuance

`POST /traefik-discovery/routes/{host}/certificate`

```
{ "challenge_type": "http-01" | "dns-01",
  "acknowledge_manual_dns_renewal": false }
```

Handler, matching the existing `set_traefik_discovered_route_enabled` shape in
`crates/temps-deployments/src/handlers/traefik_discovery.rs`:

1. `RequireAuth` + `auth.require_user()`. **Note on what this does and does not
   exclude:** `require_user()` rejects deployment tokens, but a *user-bound API
   key* carrying both permissions passes. That is accepted and stated
   explicitly rather than tightened to session-only auth, because the API is
   the supported automation surface for this migration and because the
   permission pair is a real restriction: `Role::User` holds `DomainsCreate`
   but not `SettingsWrite` (`permissions.rs:1059`), and `SettingsWrite` is
   Admin/PlatformAdmin only. Any caller reaching this endpoint is already an
   administrator by role, whatever credential they present.
2. `permission_guard!(auth, SettingsWrite)` **and**
   `permission_guard!(auth, DomainsCreate)`. No new `Permission` variant.
3. **Discovered-route gate (necessary, never sufficient):** the host must exist
   in `traefik_discovered_routes` with `enabled = true` and `network =` the
   currently-configured discovery network.
4. **Ownership gate, re-evaluated at request time (§5 step 7 applies
   identically here).** The discovered row's existence is not proof the host is
   unowned — `reserved_hosts()` only runs at reconcile time, so there is a
   window in which an operator has created a custom route or environment domain
   for host H and a stale `enabled` discovered row for H still exists. The
   handler therefore re-evaluates the same reserved-host set `reserved_hosts()`
   builds (`traefik_discovery.rs:1003`: `environment_domains`,
   `project_custom_domains`, `custom_routes`, `environments.subdomain` and its
   preview form, and the console hostname) **at request time**, and rejects 409
   on any match. It additionally rejects 409 if **any** `domains` row exists for
   the host that is not the row already referenced by this host's own
   `traefik_route_certificates.certificate_id`.
5. Persist the authorization row (`cert_authorized = true`, `source = 'acme'`,
   `renewal_method = challenge_type`, container identity per §2a).
6. Delegate issuance through the injected trait (§8). **`create_domain` is not
   idempotent** (`domain_service.rs:170`), so the adapter must not be written as
   "create or reuse":
   - No `domains` row for the host → `create_domain(host, challenge_type)`.
   - A row exists whose `verification_method` **equals** the declared
     `challenge_type` → proceed to `request_challenge` on the existing row.
   - A row exists whose `verification_method` **differs** → **409, naming both
     values** and instructing the operator to either declare the stored method
     or remove the domain first. Proceeding silently either way is forbidden:
     `request_challenge` reads the challenge type from the stored column
     (`domain_service.rs:296`), so the row's method would win while
     `traefik_route_certificates.renewal_method` recorded something else —
     precisely the disagreement §2's CHECK constraint exists to prevent.
   - A row exists with `verification_method` of `"acme"`/`"http"`/`"manual"`
     (see §7) → treated as a differing value, i.e. 409, until §7's alias
     normalization has run and rewritten it.
   Then `request_challenge` and, for HTTP-01, `complete_challenge` after the
   standard wait — byte-for-byte the sequence
   `handle_http01_renewal_order_based` (`tls/service.rs:628`) and
   `try_dns01_renewal_with_provider` (`:747`) already use, so a failure leaves a
   recoverable ACME order in the existing certificate-management UI.
7. Audit log `TRAEFIK_DISCOVERED_ROUTE_CERT_REQUESTED` via the existing
   `impl_audit_operation!` macro in `handlers/audit.rs`.

**How the challenge type is decided: the operator declares it.** Not a new
rule; it mirrors `POST /domains`. The only difference is strictness:
`create_domain` warns and silently defaults an unrecognized `challenge_type` to
`http-01`, whereas this endpoint returns 400. Silently choosing a validation
method is acceptable in a form that shows the result immediately; it is not
acceptable in an endpoint whose choice becomes the row's permanent renewal
method.

**DNS-01 requires an eager zone check (§8).** If `challenge_type` is `dns-01`
and no verified, `auto_manage` `dns_managed_domains` zone authoritatively covers
the host, the request is rejected 400 unless
`acknowledge_manual_dns_renewal: true` is present.

`DELETE /traefik-discovery/routes/{host}/certificate` clears `cert_authorized`
so Temps stops attempting renewal. It does **not** delete the `domains` row or
the certificate — deleting live key material as a side effect of a
deauthorization is the kind of surprise this codebase avoids; the existing
domain-deletion endpoint remains the way to do that.

### 4. Path B — importing Traefik's existing certificate

Path A still costs an outage window: HTTP-01 cannot validate until Temps owns
`:80` for the host, which is after the cutover. Path B removes the window by
reusing the certificate Traefik already has.

`POST /traefik-discovery/tls/import`

```
{
  "acme_json": "<the raw contents of Traefik's acme.json>",
  "hosts": ["app.example.com", "api.example.com"],
  "renewal_method": "http-01" | "dns-01",
  "acknowledge_manual_dns_renewal": false,
  "dry_run": true | false
}
```

Same auth, permission, and user requirements as Path A, including the API-key
note in §3.1.

**The document is uploaded, never read from a path.** An endpoint that takes a
server-side file path is a file-disclosure primitive: an authenticated admin
could aim it at any file on the host and learn its parseability, and error
messages leak content. The CLI reads `acme.json` locally and posts its contents;
the console accepts a file picker or paste and does the same. Parsing stays in
Rust, in one place, server-side, so validation cannot be bypassed by crafting a
request by hand.

**`dry_run: true` is the propose step.** Identical parse and full validation,
identical per-host verdicts, writes nothing. There is no server-side staging of
key material between propose and confirm — a staging store holding private keys
is a worse risk than sending the document twice over an already-authenticated
TLS connection.

Expected input shape (Traefik v2/v3):

```json
{
  "<resolver-name>": {
    "Account": { "Email": "...", "PrivateKey": "<base64>", "Registration": {} },
    "Certificates": [
      {
        "domain": { "main": "app.example.com", "sans": ["www.example.com"] },
        "certificate": "<base64 PEM chain>",
        "key": "<base64 PEM private key>",
        "Store": "default"
      }
    ]
  }
}
```

Parser rules:

- The top level is a map of resolver name → resolver state; **all** resolvers
  are scanned, since the operator should not have to know which one issued which
  host.
- Key casing is inconsistent in Traefik's own Go struct tags (`Account` and
  `Certificates` capitalized, `domain`/`certificate`/`key` lowercase, `Store`
  capitalized). Deserialization is therefore case-insensitive on field names at
  every level.
- **Duplicate-key rule:** a document containing two keys at the same level that
  differ only in case (e.g. both `certificates` and `Certificates`, or two
  resolvers `LE` and `le`) is rejected outright rather than resolved by
  last-wins. Low security impact given every downstream check, but ambiguity in
  a security-relevant parse is not something to resolve by accident.
- The v2 and v3 layouts are, as far as we can determine, identical; v1's was
  flat (no resolver key). The parser accepts a flat document if one appears, but
  **v1 is not a supported configuration**.
- **None of this is load-bearing.** The JSON's claimed `domain.main`/`sans` are
  a *hint for grouping only*. Authorization is decided against the X.509 SANs
  Temps parses out of the certificate itself (§5). A parser disagreement can
  only cause "we did not find your host", never "we imported the wrong thing".
- The `Account` block — including Traefik's ACME account private key — is
  discarded immediately after parsing and never persisted, logged, or returned.
  Temps registers its own ACME account.

Resource bounds:

- The request body cap is **1 MiB, applied as a route-level
  `DefaultBodyLimit::max(...)` layer** on the import route, matching the pattern
  already used in `temps-error-tracking` (`sentry/handlers.rs:102`,
  `source_map_handlers.rs:73`) — not a length check in prose or in the handler
  body, which would run after the body was already buffered.
- At most 256 certificate entries are parsed; beyond that the request is
  rejected with 400 rather than allocating.
- **No compression layer may ever be added to this route.** The security review
  found no decompression-bomb or algorithmic-complexity vector *given* the 1 MiB
  cap and the absence of `Content-Encoding` decompression here. That conclusion
  is conditional on both remaining true; adding a `RequestDecompressionLayer`
  to this router would invalidate it.

### 5. Import validation — what must hold before key material is accepted

**Definition of "the leaf", stated once and binding for every check below.**
The leaf is the **first PEM `CERTIFICATE` block in the `certificate` field**,
which is exactly what `extract_cert_ders` (`rustls_pemfile::certs`,
`tls_cert_loader.rs:416`) treats as the end-entity certificate when the proxy
later serves it. Steps 3–6 run against **that certificate only**, never against
any other chain element. Validator and server must not be able to disagree about
which certificate was checked.

For each requested host, **all** of the following, in this order, before
anything is written:

1. **The host is a discovered route.** An `enabled = true` row exists in
   `traefik_discovered_routes` with `network =` the currently-configured
   discovery network. Without this the endpoint is a generic
   "install-this-private-key-for-this-domain" primitive with no relationship to
   the feature. **This is a necessary condition, never a sufficient one** — step
   7 is what establishes the host is unowned.
2. **The `certificate` field is well-formed.** Reject if it contains zero
   `CERTIFICATE` blocks; reject if it contains **any non-`CERTIFICATE` PEM
   block**; reject if the chain exceeds 10 certificates. The non-`CERTIFICATE`
   rule closes a real hole: `rustls_pemfile::certs` silently ignores a
   `PRIVATE KEY` block smuggled into this field, but the field is stored
   verbatim into `domains.certificate`, which is **not** encrypted at rest — so
   an unfiltered import would write plaintext key material into a plaintext
   column. Then parse the leaf with `x509_parser::pem::parse_x509_pem` +
   `parse_x509`, reusing the pattern at `tls/providers.rs:493`.
3. **The leaf's own SANs cover the host.** The dNSName entries of the leaf's
   parsed `SubjectAlternativeName` extension are the authority — the JSON's
   `domain.main`/`sans` are never trusted for this. Exact, case-insensitive
   match against the requested host.
4. **No wildcard SAN is accepted as the match.** A certificate whose only
   covering SAN is `*.example.com` is rejected with a message naming the
   existing wildcard flow.
5. **Leaf validity window.** `not_after > now` with the same 5-minute safety
   margin `DomainService::has_usable_certificate` uses (`domain_service.rs:107`),
   and `not_before <= now`.
6. **The key matches the leaf — proven by signing, not by inspection.** The
   `key` field must contain **exactly one** private-key PEM block; zero or two
   or more is a rejection. The key is loaded through the configured `rustls`
   `CryptoProvider`'s `key_provider().load_private_key(PrivateKeyDer)`, which
   accepts the same three encodings `extract_key_der` already handles — PKCS#8,
   **PKCS#1 (`BEGIN RSA PRIVATE KEY`) and SEC1 (`BEGIN EC PRIVATE KEY`)**, both
   of which lego/Traefik do emit. A fixed test message is signed with the
   resulting signing key and the signature is verified against the leaf's
   `subject_public_key_info` using the same provider.
   *(An earlier revision named `rustls-pemfile` for this. It only base64-decodes
   PEM to DER and derives nothing, and an approach that reads an optional
   embedded public key field would silently skip the check on exactly the SEC1
   and PKCS#1 inputs Traefik writes.)*
   **There is no "unverifiable, accept anyway" branch.** A key whose algorithm
   the provider cannot load, or whose test signature does not verify against the
   leaf, is **rejected**. A cert/key mismatch that reaches the `domains` table
   breaks every TLS handshake for that host at load time, which is exactly the
   outage this ADR exists to prevent.
7. **The host is not owned by anything else.** `save_certificate` upserts on
   `domains.domain` and will overwrite **any** existing row, so this check
   cannot be narrowed to `project_custom_domains`. Reject 409 if:
   - **any** `domains` row exists for the host that is not the row already
     referenced by this host's own `traefik_route_certificates.certificate_id`
     — this covers rows reached via `project_custom_domains.certificate_id`,
     rows reached via `custom_routes.domain_id`, bare rows created by
     `POST /domains` with no project link, and rows created by the
     on-demand/preview path; **or**
   - the host matches the reserved-host set `reserved_hosts()` builds
     (`traefik_discovery.rs:1003`), **re-evaluated at request time** rather than
     trusted from the last reconcile: `environment_domains`,
     `project_custom_domains`, `custom_routes`, `environments.subdomain` and its
     preview form, and the console hostname.
8. **A chain is accepted, a chain is stored.** After step 2's filtering, the
   surviving `CERTIFICATE` blocks are stored verbatim in document order; the
   cert loader already handles chains. No re-ordering, no intermediate fetching.

**No trust-anchor verification is performed, and the ADR does not claim
otherwise.** Verifying the chain to a public root was considered and rejected:
self-hosted operators legitimately run internal CAs, and Temps has no basis to
decide whose root is acceptable on their host. The consequence is stated plainly
in Risks: a self-signed certificate for any hostname passes every check above.
The control is not proof of domain control — it is the two-permission human gate
plus the discovered-route and ownership gates.

**What gets written, and for which hosts.** Only hosts that are (a) present in
the request's `hosts[]`, (b) covered by the leaf's SANs, and (c) independently
passing all eight checks are written. A certificate listing several
non-wildcard SANs is usable for several hosts, and because the cert loader
matches on exact `domains.domain`, each written host gets its own `domains` row
carrying the same chain and key. The response also lists other hosts found in
the document that *would* be importable — **that listing is informational only
and never triggers a write.**

Each written `domains` row is set explicitly to:

- `status = 'active'` — required, not incidental: `CERT_SERVING_STATUSES` gates
  worker/edge cert sync (`nodes.rs:1751`) even though the local proxy loader
  ignores status, so leaving it unstated risks a control-plane/edge split-brain
  where the origin serves HTTPS and edge nodes do not.
- `is_wildcard = false` (step 4 guarantees this).
- `expiration_time` = the leaf's `not_after`.
- `last_renewed` = the import timestamp.
- `verification_method` = the declared `renewal_method` (§7).

Failure is per-host and reported per-host. One rejected entry never aborts the
others, and the response lists every requested host with an explicit verdict and
reason.

**Accepted TOCTOU window.** Between step 1's check and `save_certificate`, the
reconciler may delete the discovered row (the container stopped). This is
accepted and not defended against with a transaction or a lock: the row's
absence does not make the written certificate wrong, the operator explicitly
asked for this host, and no partial state is possible because the write is a
single upsert. The real consequence of the discovered row being volatile is
finding 1 — whatever container next wins that host inherits the certificate —
which is addressed by §2a's drift detection, not by tightening this window.

### 6. Key material handling — enforceable, not aspirational

`tls::models::Certificate` currently derives both `Debug` and `Serialize` with
`private_key_pem: String` as a plain field. Every "never logged" / "never
returned" intention is one `debug!("{:?}", cert)` or one `Json(cert)` away from
being false. This ADR therefore specifies mechanisms, not promises:

- **Hand-written `Debug` for `Certificate`**, replacing the derive, redacting
  both `private_key_pem` and `certificate_pem`. A struct that cannot be printed
  with its secret cannot leak it through a log statement added later.
- **`#[serde(skip_serializing)]` on `private_key_pem`** — or, preferably, remove
  the blanket `Serialize` from `Certificate` entirely and have
  `certificate_summary` (§8) return a dedicated non-secret DTO. A type that is
  not serializable cannot be handed to `Json(...)` by a future handler.
- **The import request DTO must not derive `Debug`** over its raw `acme_json`
  field; any tracing of the request must log the host list and nothing else.
- **The body-parse error path must never echo request-body fragments into a
  Problem Details `detail`.** Serde's own `unknown field` / `invalid type`
  messages can carry input, so the rejection handler emits a fixed message
  ("`acme_json` could not be parsed as a Traefik ACME document") and logs the
  serde error's *category* only.
- **At rest:** the import path writes through
  `CertificateRepository::save_certificate` and nothing else, which already
  encrypts `private_key_pem` with `EncryptionService` before the upsert
  (`tls/repository.rs:137`) and decrypts on read (`:118`). No new persistence
  code touches `domains.private_key`.
- **In responses:** no endpoint added by this ADR returns `private_key_pem` or
  the certificate PEM. Import and status responses carry only derived,
  non-secret metadata: subject CN, SANs, issuer, `not_before`/`not_after`, days
  remaining, serial, `source`, `renewal_method`.
- **In audit:** `TRAEFIK_DISCOVERED_ROUTE_CERT_IMPORTED` records who imported
  what, for which host, from which network, with which declared renewal method,
  and the leaf's SHA-256 fingerprint — never the material.
- **In-process:** the parsed key lives only in the request-handling stack frame
  until `save_certificate` returns; it is not cached, not stored in a
  `OnceLock`, not passed to the reconciler.

### 7. Renewal: correct dispatch first, then a declared method, then a loud unknown

Temps cannot infer how to renew a certificate it did not request, so **the
operator declares the method at import time** and it is written to
`domains.verification_method` (and mirrored in
`traefik_route_certificates.renewal_method`) as exactly `http-01` or `dns-01`.
Enrolment is then automatic: `find_expiring_certificates`
(`tls/repository.rs:350`) selects on `expiration_time` alone regardless of
status, so an imported row is picked up by the existing 03:00 UTC scheduler on
its next run with zero new machinery.

What each declaration means:

- **`http-01` — "I accept HTTP-01 renewal once Temps owns this domain's
  traffic."** Correct for the migration case: after cutover Temps serves `:80`
  for the host and the existing order flow works. On failure,
  `handle_http01_renewal_order_based` records a `RenewalFailure`, fires
  `send_renewal_failure_notification` (`TlsRenewalFailed`, Critical), and leaves
  the order pending for a UI retry. The existing certificate keeps serving in
  `active_renewal_failed`, which is in `CERT_SERVING_STATUSES` — degraded and
  visible, not dark.
- **`dns-01` — "I will have DNS-01 available before this expires."** With a
  verified `auto_manage` zone covering the host,
  `try_dns01_renewal_with_provider` publishes the TXT record unattended.
  Without one, it degrades to `ManualRenewalNeeded` plus
  `send_manual_renewal_notification` (`TlsCertExpiring`, escalating to Critical
  inside 7 days). §8 makes that condition checkable *before* the operator
  chooses, rather than 60 days later.

#### 7a. Three sequenced changes to renewal dispatch

An earlier revision of this ADR specified a single change: make the
`_ => warn!("Unknown verification method")` arm
(`tls/service.rs:499-505`) push a `RenewalFailure` and fire a Critical alarm.
**That was based on a wrong premise** — that `"acme"`, `"manual"`, `"http"` and
`"tls-alpn-01"` are rare legacy values. They are not.
`generate_certificate_from_order` (`tls/providers.rs:446-490`) hardcodes
`verification_method: "acme"` on every `Certificate` it returns, and it is
reached from the live `POST /domains/{domain}/provision` handler through
`TlsService::provision_certificate` (`domain_handler.rs:671`, `providers.rs:816`)
as well as from `complete_http_challenge` (`providers.rs:947`); because
`save_certificate`'s `OnConflict` includes `VerificationMethod`
(`tls/repository.rs:163`), such a save also **rewrites** rows that
`create_domain` had correctly set to `http-01`. Shipping the original change
would have fired a Critical alarm on every one of those rows, every day,
forever, on every existing instance — with no API to correct
`domains.verification_method` and therefore no way for the operator to make it
stop.

The change is therefore split into three, in this order:

- **(a) Map the known aliases to correct dispatch.** `"acme"` and `"http"` →
  the HTTP-01 renewal path; `"manual"` → the existing
  `send_manual_renewal_notification` path (an actionable "renew this yourself"
  notification, **not** `TlsRenewalFailed`). These certificates then actually
  renew, or actually produce an actionable notification, instead of silently
  expiring. This is the change that fixes real production behaviour and it
  ships first, on its own.
- **(b) Stop producing the misleading literal.** `generate_certificate_from_order`
  records the real challenge type it just completed instead of the `"acme"`
  constant, and `save_certificate` stops being able to downgrade a correct
  `http-01`/`dns-01` value to it. Existing rows need a **backfill migration**
  mapping `"acme"`/`"http"` → `"http-01"` and leaving `"manual"` alone; that
  backfill is part of this step, not assumed.
- **(c) Only then** does a genuinely unrecognized value become a
  `RenewalFailure` plus Critical alarm. After (a) and (b) the set of such values
  is empty on a healthy instance, so the alarm means what it says.

`tls-alpn-01` appears only in test fixtures and in `cert_host_cache`/
`tls_cert_loader` constructions that never reach `domains`; step (c) covers it
if a real row ever appears.

This is **not** a small prerequisite change. It alters renewal behaviour for
every certificate on every instance and must be scoped, reviewed, and released
as such.

### 8. Crate boundaries, service seam, and operator surface

**No new crate edge.** `temps-deployments` does not depend on `temps-domains`
and will not start to. Following the established anti-coupling pattern (the
`OnDemandCertProvisioner` trait declared in `temps-proxy` with its adapter in
`crates/temps-cli/src/commands/serve/on_demand_cert.rs`, and
`DnsAutomationGate` in `temps-core`), this ADR declares a narrow trait beside
`TraefikDiscoveryAdminService`:

```
trait DiscoveredHostTlsProvisioner: Send + Sync {
    async fn request_issuance(&self, host: &str, challenge_type: ChallengeType) -> ...;
    async fn import_certificate(&self, host: &str, chain_pem: &str, key_pem: &str,
                                renewal_method: &str) -> ...;
    async fn certificate_summary(&self, host: &str) -> ...;  // non-secret DTO only
    async fn dns01_zone_status(&self, host: &str) -> ...;    // eager zone check, below
}
```

The adapter lives in the serve wiring layer and drives `DomainService` /
`TlsService` / `CertificateRepository` / `DnsProviderService`.
`temps-deployments` keeps its own error enum (`TraefikDiscoveryAdminError` gains
`CertificateAuthorization`, `CertificateMaterial`, `HostOwned`, and `Upstream`
variants with their own `Problem` mappings); no error type crosses the domain
boundary.

**Eager DNS-01 zone validation is in scope, not a follow-up.** This section
already requires the UI to show, at declaration time, whether a DNS-01 zone
exists for the host — which needs the same server-side check regardless
(`DnsProviderService::find_provider_for_domain`, already used at
`tls/service.rs:747`). Deferring it would leave this section's own UI
requirement unbacked. Therefore:

- Both the issuance and import endpoints return, **per host**, whether a
  verified `auto_manage` zone authoritatively covers it, and which provider.
- Declaring `dns-01` for a host with no such zone is rejected 400 unless
  `acknowledge_manual_dns_renewal: true` is present in the request. Silently
  succeeding would sign the operator up for manual TXT publication every 60 days
  without telling them.

**API** (all under the existing `/traefik-discovery` prefix, `SettingsRead` for
reads, `SettingsWrite` + `DomainsCreate` for writes):

| Method | Path | Purpose |
| --- | --- | --- |
| `POST` | `/traefik-discovery/routes/{host}/certificate` | Authorize + issue via ACME |
| `DELETE` | `/traefik-discovery/routes/{host}/certificate` | Deauthorize (stop renewing) |
| `POST` | `/traefik-discovery/tls/import` | Validate and import from `acme.json` (`dry_run` supported), 1 MiB `DefaultBodyLimit` layer |

`TraefikDiscoveredRouteResponse` gains a nullable `tls_certificate` block:
`{ authorized, source, renewal_method, status, not_after, days_remaining,
serving, dns01_zone, container_drift, authorized_container_name,
current_container_name, container_drift_detected_at }`.
`TraefikDiscoveryStatusResponse` gains a `tls` capability block:
`{ acme_email_configured, dns_providers_configured, hosts_with_container_drift,
setup_path }`. `provision_certificate` hard-fails on an empty
`letsencrypt.email` (`tls/service.rs:216`), so an instance without one must be
told *before* the operator clicks, with a deep link to the settings page — the
`configured: false` + reason + `setup_path` contract this feature already
follows for discovery itself.

Every `utoipa::path` for a `{host}` route declares `params(...)`; a missing path
parameter generates an SDK function typed `never` and silently breaks the CLI,
which the existing test suite already asserts against.

**CLI parity** in `apps/temps-cli` (`@temps-sdk/cli`), never as Rust
subcommands, extending the existing `traefik-discovery` command group:

```
bunx @temps-sdk/cli traefik-discovery tls request <host> --challenge http-01|dns-01
bunx @temps-sdk/cli traefik-discovery tls import --acme-json ./acme.json \
      [--host <h>]... --renewal http-01|dns-01 [--dry-run]
bunx @temps-sdk/cli traefik-discovery tls revoke <host>
```

`--dry-run` prints the per-host verdict table and is the documented first step.
`routes list` gains a TLS column that renders container drift distinctly from a
healthy authorized certificate.

**Console:** `web/src/pages/settings/TraefikDiscoveryPage.tsx` gains a TLS
column and a per-row action. A row with `tls = true` and no certificate renders
a prominent "HTTPS will fail for this host" warning with both remedies. A row
with `container_drift: true` renders a persistent Critical-severity banner
naming both containers — this is the surface finding 1 depends on, so it cannot
be a subtle badge.

### 9. The cutover runbook this makes possible

1. With Traefik still serving, enable discovery and confirm the hosts appear
   (`traefik-discovery routes list`). HTTP routing is verifiable without
   touching `:443`.
2. `traefik-discovery tls import --acme-json ./letsencrypt/acme.json --dry-run`
   — see exactly which hosts are importable and why the others are not.
3. Re-run without `--dry-run`, declaring `--renewal http-01`.
4. Stop Traefik, start Temps on `:80`/`:443`. Certificates are already present;
   the first HTTPS request succeeds.
5. Renewal happens on the existing daily scheduler; a failure is a Critical
   alarm, not an expiry.

Operators who cannot get the certificate file take the lossy path: cut over,
then Path A, accepting an outage window of one ACME round trip.

## Migration plan

1. **Renewal dispatch, step (a): alias mapping.** Ships first and alone —
   `"acme"`/`"http"` → HTTP-01 renewal, `"manual"` → manual-renewal
   notification. This is a behaviour change affecting every certificate on
   every instance; scope, review, and release it as such, with its own
   verification that no new alarm class fires on an instance whose rows are all
   `"acme"`.
2. **Renewal dispatch, step (b):** stop `generate_certificate_from_order`
   emitting the `"acme"` literal, prevent `save_certificate` from downgrading a
   correct value, and backfill existing rows.
3. **Schema.** Add `traefik_route_certificates` with the CHECK constraints, FKs,
   and container-identity columns from §2. No trigger. No change to
   `traefik_discovered_routes`.
4. **Trait + adapter.** Declare `DiscoveredHostTlsProvisioner` in
   `temps-deployments`, implement the adapter in the serve wiring layer, wire it
   into `TraefikDiscoveryAppState`.
5. **Confidentiality hardening (§6).** Hand-written `Debug`, serialization
   changes, non-secret summary DTO. Land before any endpoint can hold key
   material.
6. **Path A.** Issuance endpoint, deauthorization endpoint, ownership and
   reserved-host gates, the `verification_method` mismatch 409, DNS-01 zone
   check, audit operations, response-DTO extensions.
7. **Container-drift detection (§2a).** Reconciliation-time and read-time
   comparison, alarm, API/console state.
8. **Path B.** `acme.json` parser, X.509 + signing-based validation module with
   the adversarial tests below, import endpoint with `dry_run` and the
   `DefaultBodyLimit` layer.
9. **Renewal dispatch, step (c):** unknown `verification_method` becomes a
   `RenewalFailure` + Critical alarm, now that the set is empty on a healthy
   instance.
10. **Clients.** Regenerate both OpenAPI clients (`bun run spec:update` for the
    CLI, `bun run openapi-ts` for `web/`), add the CLI commands and tests, extend
    the settings page.
11. **Docs.** The cutover runbook in §9, plus a rewrite of the
    `TEMPS_TRAEFIK_DISCOVERY_ENABLED` entry in `CLAUDE.md` — its current
    sentence "a container's `traefik...tls` label is recorded but never triggers
    certificate issuance" stays true but becomes incomplete once there is a
    supported way to get a certificate, and an operator reading only that line
    concludes HTTPS is impossible.

Migration needed: yes (one additive table, plus the step-(b) backfill).
Breaking changes: none to any existing endpoint or response shape. Renewal
*behaviour* changes for existing `"acme"`/`"http"`/`"manual"` rows — that is the
point of steps 1–2 and is an improvement, but it is a behaviour change and the
release notes must say so.

## Verification

Certificate import is the highest-risk surface, so its tests are adversarial by
construction and are unit tests over the pure validator, not integration tests:

| Case | Required outcome |
| --- | --- |
| Cert whose SANs do not include the requested host, with `domain.main` in the JSON claiming they do | Rejected — the JSON is never trusted |
| **Chain where element 0 covers an attacker host and element 1 covers the requested host** | **Rejected — the leaf is element 0 only** |
| **`certificate` field containing a `PRIVATE KEY` block** | **Rejected; never persisted into `domains.certificate`** |
| **`certificate` field with zero CERTIFICATE blocks, or more than 10** | **Rejected** |
| **`key` field containing two private-key blocks, or zero** | **Rejected** |
| **SEC1 (`BEGIN EC PRIVATE KEY`) key + matching cert** | **Accepted, and the sign/verify check demonstrably ran** |
| **PKCS#1 (`BEGIN RSA PRIVATE KEY`) key + matching cert** | **Accepted, and the sign/verify check demonstrably ran** |
| **Key of an algorithm the `CryptoProvider` cannot load** | **Rejected — never "accept as unverifiable"** |
| Key that does not match the leaf's public key | Rejected |
| Requested host has no `traefik_discovered_routes` row / `enabled = false` / different network | Rejected, nothing written |
| Expired cert / `not_before` in the future | Rejected |
| Wildcard-only SAN coverage | Rejected with the wildcard message |
| **Host has a `domains` row reached via `custom_routes.domain_id`** | **409** |
| **Host has a bare `domains` row with no project link** | **409** |
| Host has a `domains` row via `project_custom_domains` | 409 |
| **Host is an `environment_domains` / `environments.subdomain` / console value while a stale `enabled` discovered row still exists** | **Rejected — reserved-host set re-evaluated at request time** |
| **Path A where the existing `domains` row's `verification_method` differs from the declared `challenge_type`** | **409 naming both values — never silent reuse** |
| Multi-SAN cert, two authorized hosts | Two `domains` rows, same chain, both `status = 'active'`, both serving |
| Other importable hosts listed in the response but absent from `hosts[]` | Listed only; **no rows written** |
| Valid import | `domains.private_key` in the DB is ciphertext; `status='active'`, `is_wildcard=false`, `expiration_time`=leaf `not_after`, `last_renewed`=import time |
| Any rejection or success | Response and logs contain no key bytes |
| `dry_run: true` | Identical verdicts, zero writes |
| Malformed / truncated / non-JSON body | 400 with a fixed message; **no request-body fragment in `detail`** |
| Body over 1 MiB | Rejected by the route-level `DefaultBodyLimit` layer before handler entry |
| Document with two same-level keys differing only in case | Rejected |
| **Import for host H, then a differently-named container wins the next reconcile** | **Critical alarm + `container_drift` state visible in API and console; `cert_authorized` unchanged** |
| Drift persists across further reconcile passes | Alarm not re-fired for the same (host, container) pair |
| **Renewal dispatch for `verification_method = "acme"`** | **Renews via the HTTP-01 path — not a Critical alarm** |
| **Renewal dispatch for `verification_method = "manual"`** | **Manual-renewal notification — not `TlsRenewalFailed`** |
| Renewal dispatch for a genuinely unknown value, after steps (a) and (b) | `RenewalFailure` + Critical alarm |
| Import then advance the clock past `now + 30d` | The scheduler picks the row up and dispatches on the declared method |
| **`Certificate` formatted with `{:?}`, or serialized** | **Contains no key bytes** |
| Declaring `dns-01` with no verified `auto_manage` zone, no acknowledgement | 400 |
| Declaring `dns-01` with no zone, `acknowledge_manual_dns_renewal: true` | Accepted, and the response says renewal will be manual |
| Deployment token as caller | 403 on every new endpoint |
| `SettingsWrite` without `DomainsCreate` (and vice versa) | 403 |
| **User-bound API key holding both permissions as caller** | **Accepted — documented outcome asserted, not incidental** |

Plus a route-table regression test asserting `cert_eligible == false` for every
discovered route, so the invariant in §1 cannot be undone by accident.

## Consequences

### Positive

- The advertised migration stops being an outage. With Path B the HTTPS gap at
  cutover is zero.
- Certificates for discovered hosts live in the same table, are served by the
  same loader, sync to worker nodes through the same `CERT_SERVING_STATUSES`
  query, and renew on the same scheduler as every other certificate. There is
  exactly one certificate store.
- The security property that motivated `cert_eligible: false` is preserved and
  strengthened: issuance now requires an authenticated administrator, two
  permissions, an enabled route on the configured network, a host owned by
  nothing else, and an audited authorization record — where before it required
  a label.
- Authorization survives container churn, and a change in the container serving
  an authorized host becomes a Critical, visible event rather than an invisible
  one.
- A whole class of pre-existing certificate-renewal defects is fixed on the way
  past: `"acme"` rows (the common case on every existing instance) start
  actually renewing, and `"manual"` rows start producing an actionable
  notification instead of a log line.

### Negative

- Temps gains an endpoint that accepts private key material. That is a
  genuinely new class of surface for this codebase and depends on §5 being
  right.
- Two paths to a certificate is more surface to document and support than one,
  and the operator chooses between them when they are least informed.
- Imported certificates are opaque to Temps' issuance history — no ACME order,
  no `renewal_attempts` trail, and no way to verify the declared renewal method
  until the first renewal runs, up to 60 days later. Eager DNS-01 zone checking
  (§8) narrows this but does not close it.
- Multi-SAN certificates duplicate key material across `domains` rows.
- The `acme.json` parser is a compatibility surface for a format Temps does not
  control and cannot version-negotiate.
- Renewal-dispatch steps (a)–(c) touch every certificate on every instance, so
  the change cannot be shipped quietly.

### Risks

- **Container takeover becomes HTTPS impersonation (finding 1, HIGH).** The
  reconciler's name-based tie-break lets a differently-named container claim an
  authorized host within one reconcile pass, and the cert loader serves by SNI
  without consulting the route table — so the takeover comes with a real,
  trusted certificate. §2a's captured container identity, drift detection,
  Critical alarm, and persistent console state are the mitigation. They make the
  event unmissable; they do not prevent it. An operator who ignores the alarm is
  impersonated. This is the single most important thing to get right in
  implementation and the reason `cert_authorized` deliberately does not
  auto-clear (§2a).
- **This endpoint installs operator-chosen key material; it does not prove
  domain control.** §5 performs no trust-anchor verification — deliberately, so
  operators running internal CAs are not locked out — which means a self-signed
  certificate for any hostname passes every check. The controls are the
  two-permission human gate (Admin/PlatformAdmin by role), the discovered-route
  requirement, and the ownership gate. Stated plainly because findings 1 and 5
  both rest on it: the ADR does **not** claim that an importer must already
  control the domain.
- **Imported private keys replicate beyond the control plane.** A row written
  with `status = 'active'` is synced to every worker/edge node by the existing
  `CERT_SERVING_STATUSES` query (`nodes.rs:1751`). That is required for the
  feature to work in a multi-node install and is listed under Positive for that
  reason — but an operator importing a third party's key material must know it
  will land on every edge node in the fleet, not only on the control plane. The
  import UI and CLI must say so at the point of import when the install has more
  than one node.
- **A wrong `renewal_method` declaration is only fully proven at renewal time.**
  Narrowed by the eager DNS-01 zone check (§8), which now rejects the most
  common bad declaration up front. What remains — declaring `http-01` for a host
  whose traffic Temps will never own — surfaces as a Critical alarm with the old
  certificate still serving for ~30 days.
- **Key material at rest concentrates in `domains`.** Import adds volume to the
  same encrypted column that already holds every custom-domain key; it does not
  add a new exposure class, but it raises the value of that column.
- **The gate depends on the currently-configured discovery network.** Repointing
  `TEMPS_TRAEFIK_DISCOVERY_NETWORK` stops previously authorized hosts matching
  the gate for *new* operations while their certificates keep serving. Correct
  and conservative, but it will look inconsistent unless the status endpoint
  shows `authorized_network` per row — which §2 requires.
- **Let's Encrypt rate limits.** Path A is human-triggered and naturally
  throttled; no limiter is added, deliberately, because a human-gated action
  does not need one and an unnecessary one would block legitimate bulk
  migration. Existing failure notifications surface a limit if it is hit.
- **Wildcard shadowing (INFO-1).** An operator holding `*.example.com` already
  serves HTTPS for discovered subdomains through the cert loader's wildcard
  fallback, without any authorization record. Pre-existing, compounds finding 1
  (a takeover of such a host is served under the wildcard with no drift record
  because no authorization exists), and must be verified at code review.

## Alternatives considered

- **Flip `cert_eligible` to mirror `discovered.tls`.** Rejected: exactly the
  vulnerability the hardening pass closed. A container would choose the
  hostname, and on-demand issuance would mint a publicly-trusted certificate
  under the operator's zone for a name the operator never approved.
- **Reuse the on-demand TLS path with a widened gate.** Rejected: that mechanism
  is scoped to direct subdomains of the on-demand/preview zone for ephemeral
  hostnames (ADR-018 §2), and `provision_on_demand` explicitly forbids new
  ungated callers. Widening its zone check would delete the primary anti-abuse
  control for a subsystem triggered by unauthenticated TLS handshakes.
- **Auto-clear `cert_authorized` when the serving container changes.** Rejected
  with reasoning in §2a: it would not stop the impersonation (the certificate
  keeps serving), it would silently break renewals across routine redeploys, and
  it would hand a hostile container a one-pass denial-of-service against a
  legitimate host's renewals.
- **Verify the imported chain against a public trust anchor.** Rejected as a
  hard requirement: self-hosted operators legitimately run internal CAs and
  Temps cannot adjudicate whose root is acceptable on their host. Rather than
  ship an opt-out that most operators would flip blindly, the ADR performs no
  trust-anchor check and states the consequence honestly in Risks.
- **Tell operators to use the existing `POST /domains` screen.** Rejected as the
  *only* answer: it works, but the operator must know it works, retype hostnames
  the system already knows, gets no warning on the discovery page that HTTPS is
  about to break, and still faces the HTTP-01 chicken-and-egg problem. It
  remains fully supported and is what deauthorization leaves you with.
- **Have Temps read `acme.json` from a path on the host.** Rejected: a
  file-disclosure primitive for an authenticated admin, and it fails entirely
  when Temps runs in a container without the Traefik volume mounted.
- **Stage the parsed material server-side between propose and confirm.**
  Rejected: a store holding third-party private keys between two requests is a
  worse risk than re-sending the document over an authenticated connection.
- **Store discovered-route certificates in their own table.** Rejected: a second
  certificate store means a second renewal loop, a second cert loader lookup, a
  second encryption path, and a second place to forget
  `CERT_SERVING_STATUSES`.
- **Put `cert_authorized` on `traefik_discovered_routes`.** Rejected: those rows
  are deleted when the container stops, so authorization would evaporate on
  `docker compose down` and silently return as unauthorized on the way back up.
- **Verify the cert/key pair by comparing an embedded public key field.**
  Rejected: it does not run on SEC1 and PKCS#1 keys, which is what Traefik and
  lego actually write, so it would silently skip the check on the common case.
  Signing a test message through the configured `CryptoProvider` and verifying
  against the leaf's SPKI is the only formulation that covers all three
  encodings the proxy itself accepts.
- **Infer the renewal method instead of asking.** Rejected: the certificate
  carries no record of how it was validated, and guessing costs a failure 60
  days later. Asking costs one enum in a request body.
- **Make the unknown-`verification_method` arm loud immediately.** Rejected —
  and this was the original proposal. `"acme"` is produced by the live issuance
  path, so it would have alarm-stormed every existing instance daily with no
  operator remedy. Fixing dispatch for the known aliases first (§7a) is the
  prerequisite, not an enhancement.
- **Copy Traefik's ACME account key so renewals continue "as Traefik".**
  Rejected: Temps would operate under an account it does not own, with no way to
  reason about that account's rate limits or contact address.
- **Support Consul/etcd Traefik ACME stores in v1.** Rejected as scope: three
  client libraries for a configuration Traefik v2/v3 CE does not produce.

## Scope and related decisions

This ADR governs how a *discovered* route obtains and keeps a certificate. It
does not change the on-demand TLS subsystem, the custom-domain flow, the ACME
client, or the route table's precedence rules — except for §7a, which corrects
renewal dispatch for every certificate because the new rows cannot be made safe
without it.

- **ADR-017** splits the proxy and console processes; certificate changes reach
  the proxy through the existing cert-loader path, not the `route_table_changes`
  channel, which is why §2 adds no trigger.
- **ADR-018** defines on-demand HTTP-01 issuance and owns the `cert_eligible`
  concept. This ADR keeps discovered routes permanently outside that mechanism.
- **ADR-031** covers managed DNS and Cloudflare proxying; the per-zone DNS
  provider association §7/§8 rely on is defined there and in
  `dns_managed_domains`.
- The Traefik label grammar boundary in
  `crates/temps-deployer/src/traefik_labels.rs` remains authoritative for what a
  discovered route *is*; this ADR adds no label support and reads no new labels.

---

**Maintenance:** Review when changing the ACME challenge types Temps supports,
the `domains` table's certificate columns, the renewal scheduler's dispatch, the
cert loader's SNI resolution (including wildcard fallback), the Traefik ACME
storage format, or the discovery reconciler's row lifecycle and host tie-break.
Owner: Temps platform maintainers. Last reviewed: 2026-08-31 (revision 2,
post-security-review).
