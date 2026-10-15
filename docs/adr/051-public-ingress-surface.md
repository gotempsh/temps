# ADR-051: Public Ingress Surface for Third-Party Callbacks

**Status:** Accepted (Phase 1 implemented)
**Date:** 2026-10-11
**Author:** David Viejo
**Builds on:** ADR-017 (split proxy/console processes), ADR-045 (console access through Cloud: a private console is a supported topology)

---

## Context

### One URL, two jobs

`AppSettings.external_url` (`crates/temps-core/src/app_settings.rs:16`) is
the only "public" URL Temps knows about. It does two jobs that pull in
opposite directions:

1. **Where operators reach the console.** Operators increasingly want this
   private: the admin gate (`TEMPS_ADMIN_ALLOWED_IPS` / the `admin_gate`
   settings key), the dual-listener mode (`TEMPS_CONSOLE_ADMIN_ADDRESS`), a
   VPN, an identity-aware proxy, or no inbound port at all (ADR-045).
2. **The base of every URL Temps hands to a third party that calls back.**
   These callers are on the open internet and cannot be allowlisted:

| Inbound flow | URL given to the caller | Built in |
|---|---|---|
| Email open pixel / click redirect | `{external_url}/api/emails/{id}/track/open\|click/{n}` | `crates/temps-email/src/services/tracking_service.rs` (`get_base_url`, `inject_tracking_pixel`) |
| SES delivery/bounce events (SNS) | `{external_url}/api/t/webhook/ses` | `crates/temps-email/src/handlers/providers.rs` (`tracking_webhook_url`) |
| GitLab / Gitea / Bitbucket / generic git webhooks | `{external_url}/api/webhook/git/{provider}/events[/{token}]` | `crates/temps-projects/src/services/project.rs` |
| GitHub App manifest (webhook + callback URLs) | browser `window.location.origin` | `web/src/components/git-providers/GitProviderFlow.tsx` |
| Revenue provider webhooks (Stripe, ...) | browser `window.location.origin` + `/api/webhooks/revenue/...` | `web/src/components/project/ProjectRevenue.tsx` |
| Sentry DSN, OTLP endpoint injected into deployments | `external_url` host | `crates/temps-error-tracking/src/sentry/dsn_service.rs`, `crates/temps-deployments/src/services/env_resolver.rs` |

When the console is restricted, every one of these silently stops working.
The symptom is quiet: emails show `0` opens and `0` clicks forever, pushes
stop triggering deployments, bounces are never recorded. No error reaches
the operator, because the failing request never reaches Temps.

### Even a correct URL would be refused

There are three independent reasons a third-party request is refused today.

1. **Proxy gate.** `should_consult_admin_gate`
   (`crates/temps-proxy/src/proxy.rs`) exempts exactly one prefix,
   `/api/_temps`. Tracking, `/api/t/webhook/ses`, `/api/webhook/git/*`,
   `/api/webhooks/revenue/*`, the Sentry `store`/`envelope` routes and the
   agent trigger are all on console fall-through hosts. They get the branded
   404 whenever the gate is active.
2. **Wrong router.** The console has two routers
   (`crates/temps-cli/src/commands/serve/console.rs`):
   - `public_app`: `configure_public_routes`, no auth middleware, no gate.
   - `admin_app`: `configure_routes`, auth middleware plus the axum
     `admin_gate` layer.

   Git webhooks (`crates/temps-git/src/plugin.rs`, `configure_routes` only),
   the agent webhook trigger (`crates/temps-agents`) and the proof-of-work
   CAPTCHA verification (`/api/_temps/captcha/verify`, `crates/temps-proxy`)
   are on **`admin_app`**. In dual-listener mode they are only reachable on
   the private admin listener, so no topology can receive them publicly.
3. **No structural boundary in single-listener mode.** `public_app` and
   `admin_app` are merged onto `console_address`, so the proxy cannot send a
   request to "only the public routes". The `/api/_temps` prefix exemption is
   a path convention, not a boundary.

### Existing pieces this builds on

- `AppSettings::console_hostname()` and the reserved-platform-host check
  already keep the console hostname out of project routing
  (`crates/temps-routes/src/route_table.rs`). A second platform hostname fits
  the same mechanism.
- The admin gate config is read through `AdminGateHandle`
  (`Arc<ArcSwap<..>>`) with no I/O on the request path. That is the model for
  any new per-request decision in the proxy.
- `crates/temps-email-tracking` ships an HMAC-signed pixel/click rewriter.
  `EmailService::with_tracking_rewriter` has no callers, so it is dead code.

### Forces

- **Security.** Exposing callbacks must not expose anything else. A request
  that reaches the public surface must be structurally unable to reach an
  admin handler or the SPA, whatever its path, `Host` or headers.
- **Zero-config default.** A fresh install with a public console must keep
  working exactly as it does today, with no new setting.
- **Hot path.** The proxy decision is per request. It must stay lock-free,
  allocation-free and I/O-free (CLAUDE.md, Scalability & Efficiency).
- **No env-var configuration.** Which URL third parties call is runtime,
  per-instance configuration. It belongs in the settings row, editable and
  audited (CLAUDE.md, "Add new runtime configuration as environment
  variables" is forbidden).
- **Discoverability.** The operator must learn about the problem from the
  console, not from a blank Opens column.

---

## Decision

Temps gets a **public ingress surface**: a public-only router with its own
optional hostname. Every URL handed to a third party resolves against that
hostname. Every receiver lives on that router, and the proxy can reach that
router without being able to reach anything else.

### D1. A `public_ingress_url` setting

- Add `public_ingress_url: Option<String>` to `AppSettings`, next to
  `external_url`. Validation:
  - It must be an absolute `http(s)` URL with no path, query or fragment.
  - Its host must not be a project domain, the preview wildcard, or an
    unroutable host (loopback, `*.localho.st`, private ranges). This is the
    same check the Worker Nodes page applies to join URLs (ADR-048 D2a).
  - "The preview wildcard" means every name it covers, not only the apex.
    Each hostname Temps generates is a single label directly under the
    preview domain: an environment, a deployment, a Compose service, a
    workspace preview (`ws-<id>-<port>`). An ingress hostname there could
    take over an application's name, now or when one is generated later,
    and the proxy would handle a preview-shaped one as a sandbox preview
    before it reached the callback routes. So the whole namespace is refused
    rather than each generator being checked, and the same check runs when
    the preview domain is changed onto a stored ingress hostname. A deeper
    name (`hooks.cb.<preview>`) is not generated and stays allowed. As a
    second line, the proxy never treats the reserved ingress hostname as a
    workspace preview.
  - "A project domain" means any name the proxy serves for an application,
    not only a stored domain row: a name a wildcard route covers, a name
    generated from an environment's subdomain, and the names of stopped,
    sleeping or not-yet-deployed environments. The route table is the only
    place that knows all of them, so the check runs against a fresh load of
    it, and the setting is refused when that check cannot run.
- It is written through the settings API, audit-logged like every settings
  write, and editable from the console. It is **not** an environment
  variable. `temps setup` may accept it as a one-shot first-boot input,
  exactly like `external_url`.
- Resolution goes through one getter,
  `ConfigService::get_public_ingress_url()`: `public_ingress_url` if set,
  otherwise `external_url`. Unset therefore means today's behaviour.
- `AppSettings::public_ingress_hostname()` is the twin of
  `console_hostname()`. That host is a **reserved platform host**: it cannot
  be claimed by a project or by Traefik discovery, and it gets a certificate
  the same way the console hostname does.

### D2. Every callback URL is built from the ingress URL, and only those

Each URL Temps emits is classified by **who will request it**:

| Who requests it | Base URL | Examples |
|---|---|---|
| A third-party machine, or an end user's mail client/browser that is not an operator | `get_public_ingress_url()` | tracking pixel/click, SES/SNS, git webhooks, GitHub App manifest `hook_attributes.url`, revenue webhooks, agent webhook trigger, Sentry DSN and OTLP endpoint for browser SDKs |
| An operator's browser | `external_url` | links in notification emails ("view deployment"), OAuth/OIDC redirect URIs (browser-driven, so they work against a private console) |
| A workload container on the cluster | `internal_url` (unchanged) | server-side OTLP push, `TEMPS_API_URL` |

- Every row in the first group switches to the new getter.
- The console UI stops deriving callback URLs from `window.location.origin`.
  It reads them from the API (D6), so the GitHub App manifest and revenue
  setup show the ingress host even while the operator is browsing the
  private console. There is no fallback to the browser origin while that
  read is pending or after it fails: copy and submit controls stay disabled
  and the UI offers a retry, because a provider registered against the
  private console can never deliver and nothing would report it.
- A lint-style unit test enumerates the call sites of `get_external_url*`
  and fails when a new one appears in a crate that serves receivers. That
  forces the author to pick a column from the table above.

### D3. Every receiver lives on the public router

- These routes move from `configure_routes` to `configure_public_routes`
  (paths unchanged):
  - the git provider webhook receivers (`/webhook/git/{github,gitlab,gitea,bitbucket,generic}/...` and the legacy `/webhook/source/github/*`);
  - the agent webhook trigger (`/agents/webhook/{id}`);
  - the CAPTCHA endpoints (`/_temps/captcha/verify` and the WASM fallbacks).
    They sit under `/api/_temps`, which D4 routes to the public-only
    listener, so leaving them on the admin router would make them
    unreachable.
- The GitHub App OAuth `callback`/`install` routes are browser-driven and
  stay on the admin router. Only the server-to-server `events` receiver
  moves.
- **Rule for the public router:** a handler on it authenticates the
  *request*, never the *session*. The accepted methods are:
  - a provider signature (GitHub `X-Hub-Signature-256`, Gitea, GitLab
    `webhook-signature`, SNS RSA with a pinned certificate host, Stripe
    signing secret);
  - a per-resource unguessable capability in the path or a header (UUIDv4
    email id, Bitbucket/generic delivery token, agent `X-Webhook-Token`);
  - a scoped API token (OTLP, Sentry DSN key).

  Public handlers never read the session cookie.
- Each public route declares a request body limit appropriate to the
  provider (SNS and git payloads are bounded; nothing on this router
  accepts unbounded bodies).

### D4. The public router gets its own listener in every topology

The console binds the public-only router a second time on an **ingress
listener**, in every topology. The proxy targets that listener whenever it
must reach "public routes only". Nothing about the existing listeners
changes, so an external reverse proxy pointed at `console_address` keeps
working.

The ingress listener mounts the health probes and the public router under
`/api`, and nothing else. In particular it does not mount the MCP endpoint,
which `public_app` carries on `console_address`: its authenticated operations
accept a session cookie or an API key, and this surface must not.

That is why dual-listener mode does not reuse `console_address` as the
ingress listener, even though no admin route is mounted there. A request the
admin gate denied would land on a listener that serves authenticated MCP,
and a denied client holding valid credentials could run MCP operations: the
gate would stop restricting who can operate the instance. Startup refuses an
ingress address equal to another console listener for the same reason.

The proxy's console fall-through becomes a three-way decision. It is made in
`request_filter`/`resolve_peer` from the same `ArcSwap` snapshot that holds
the admin gate and console hostname, with no I/O:

```
host == public_ingress_hostname          -> ingress listener   (never gated, never SPA)
admin gate denies (console fall-through) -> ingress listener   (was: branded 404)
otherwise                                -> console listener   (unchanged)
```

- **Gate denial falls through to the public surface instead of 404.** A
  client the gate rejects can reach exactly the routes an anonymous internet
  client is supposed to reach, and nothing else, because the listener it
  lands on has nothing else mounted. Anything that is not a public route
  gets the public router's 404 (the proxy keeps rendering the branded 404
  page for it).
  - This alone fixes an instance whose console is restricted by the built-in
    gate, with no second hostname.
  - It replaces the `/api/_temps` path exemption with a structural one.
    `/api/_temps` and OTLP ingest (`/api/otel/v1/`), already routed to the
    console for any `Host`, are routed to the ingress listener instead. The
    rest of `/api/otel` is the console's authenticated query API and keeps
    going to the console listener.
- **The ingress hostname never reaches the console listener**, even from an
  allowlisted IP. An operator who opens `https://hooks.example.com/` gets a
  404, not a login page. This keeps cookies, CSRF surface and the SPA off the
  public hostname.
- **External restrictions** (VPN, firewall, identity-aware proxy in front of
  the console hostname) are invisible to Temps. They are handled by D1: the
  operator exposes only the ingress hostname publicly and points D1 at it.

The ingress listener binds to loopback. Its address is bootstrap config,
like `console_address`. When the proxy runs in-process (`temps serve`), it
picks a random loopback port and is handed the address directly. When
`temps proxy` runs as a separate process (ADR-017), both processes take
`--console-ingress-address` (`TEMPS_CONSOLE_INGRESS_ADDRESS`), the same way
they already share `--console-address`. The flag is optional there: a split
proxy started without it has no ingress listener to target and keeps the
previous behaviour (a 404 for gate-denied requests, `console_address` for
public ingest paths), so an existing split deployment is unaffected until
the operator opts in.

### D5. Changing the ingress URL re-points registered callbacks

A URL change is only half-done if the providers still call the old host.
When `public_ingress_url` (or, while it is unset, `external_url`) changes,
a background job re-registers every callback Temps created itself. It
records the outcome per callback and is idempotent. Individual failures
are surfaced in D6; they never block the settings write.

| Callback | How it is re-pointed |
|---|---|
| GitLab, Gitea, Bitbucket project webhooks | update the hook through the provider API with the stored hook id |
| GitHub App webhook | `PATCH /app/hook/config` with the App JWT (Temps holds the App private key) |
| SES/SNS subscription | subscribe the new endpoint, confirm it, then unsubscribe the old one |
| Sentry DSN, OTLP endpoint (env vars) | take effect on the next deployment; D6 lists the environments still running the old value |
| Revenue provider webhooks | created by the operator in the provider dashboard; D6 shows the new URL to paste |

### D6. The console tells the operator when callbacks cannot arrive

Add a typed capability endpoint, `GET /settings/public-ingress`. It is
guarded by `projects:read` rather than `settings:read`: it is read by
everyone who wires a callback, and the URL it returns is printed in every
tracked email.

```rust
pub struct PublicIngressStatusResponse {
    /// URL third parties are given (explicit setting or external_url fallback)
    pub effective_url: String,
    pub source: PublicIngressSource,          // Explicit | ExternalUrlFallback
    /// Whether Temps can tell the effective host is unreachable for them
    pub configured: bool,
    /// Why callbacks will not arrive, when `configured` is false
    pub reason: Option<String>,
    /// Console path that fixes it
    pub setup_path: Option<String>,
    /// Per-callback registration state from D5
    pub callbacks: Vec<CallbackRegistrationStatus>,
}
```

`configured` is `false` when Temps can prove the problem from its own state:

- the ingress URL is the console host and that host is gated in a way the
  D4 fallback cannot cover;
- the ingress URL is unroutable (D1 rules);
- a D5 re-registration failed.

External restrictions cannot be detected locally. D6 therefore also offers
an explicit **"Test public reachability"** action, and the open questions
cover how to run it. Surfaces:

- **Email.** A banner on the email list and the domain/tracking settings:
  "Opens and clicks are tracked through `https://console.example.com`,
  which only allowlisted networks can reach. They will not be recorded."
  It links to the ingress setting.
- **Git providers and revenue setup.** The URLs come from this endpoint, and
  a stale or failed registration is shown inline.
- **Settings → Public ingress.** The URL, its source, the per-callback table
  and the test action.

---

## Invariants

1. A request routed to the ingress listener cannot execute an admin-router
   handler or receive the SPA, regardless of path, `Host`, `X-Forwarded-*`
   or cookies. This is enforced by what is mounted, not by a path match.
2. With `public_ingress_url` unset and the admin gate a no-op, the URLs Temps
   emits and the routes it serves are byte-for-byte what they are today.
3. Every handler on the public router authenticates the request by
   signature, capability token or scoped API token, and never by session.
4. The proxy's per-request work for this feature is one hostname comparison
   against an `ArcSwap` snapshot: no locks, no allocation, no I/O.
5. No new environment variable carries runtime configuration. The ingress
   listener address is bootstrap config, like `console_address`.

## Security Model

- **What becomes reachable.** In a gated topology, routes that already
  existed and were already unauthenticated-by-session become reachable from
  the internet, which is the point. The audit for this ADR is the D3 list:
  each receiver's verification method was checked when it was written. This
  ADR adds body limits and forbids session auth on that router.
- **What does not become reachable.** Admin APIs, the SPA, health details
  beyond `/healthz` and `/readyz`, the MCP endpoint's authenticated
  operations, and anything a future plugin mounts with `configure_routes`.
- **Gate fall-through is not a gate bypass.** Today a denied client gets a
  404. After this change it gets the public router: the same routes it could
  already reach on any project domain under `/api/_temps`, plus the
  receivers. A plugin author who puts a sensitive route on the public router
  has made it public in every topology. This ADR does not change that, but
  it makes it explicit (D3 rule) and covers it in review.
- **Separate hostname, separate cookie jar.** Session cookies are host-only,
  so the browser never sends them to the ingress hostname. If cookie scoping
  is ever widened to a parent domain, the ingress hostname must stay outside
  it. A test asserts the `Set-Cookie` attributes.
- **Abuse.** Public receivers are the cheapest thing on the instance to
  flood:
  - The tracking pixel and click redirect get a per-IP token-bucket limit
    with bounded memory (fixed-size sharded map, LRU eviction). Requests over
    the limit still return the pixel or redirect, but the event is not
    recorded, and drops are counted in a metric.
  - Signature-verified receivers reject before doing any DB work.
- **Tracking integrity.** Email ids are UUIDv4 (122 random bits), so the
  current open/click URLs are already unguessable capabilities. Forging an
  open requires the email itself. The unused HMAC rewriter in
  `temps-email-tracking` adds nothing for that threat model. It is removed,
  or folded into `TrackingService`, as a separate cleanup and is not on this
  ADR's critical path.

## Alternatives Considered

- **Extend the `/api/_temps` path exemption to more prefixes.** It is the
  smallest diff, but it repeats today's design flaw. The boundary becomes a
  list of strings that must stay in sync with plugin route registrations,
  and a mis-prefixed admin route becomes public. It also does nothing for
  receivers on the admin router or for externally restricted consoles.
- **Serve receivers on project/customer domains** (as `/api/_temps/*`
  already is). Webhook URLs would then depend on a particular project's
  domain staying alive and routed, which is wrong for instance-level
  callbacks like a GitHub App or SES. It is the right model for one case,
  custom tracking domains (future work below).
- **Relay everything through Temps Cloud.** This is the right answer for
  instances with *no* public ingress, but it should not be the only answer.
  It makes callbacks depend on Cloud and on a link, it costs per request
  (pixels are high-volume), and ADR-045 deliberately scoped that channel to
  the admin console. It is future work built on D4's ingress listener: the
  relay would dispatch into the ingress listener and nothing else.
- **A `TEMPS_PUBLIC_INGRESS_URL` environment variable.** Forbidden for
  runtime configuration. It would also not be audited or visible in the
  console.

## Consequences

### Positive
- Callbacks work for instances whose console is private, whether that is the
  built-in gate (D4, no new hostname needed) or an external restriction
  (D1 plus one public hostname).
- The public/admin boundary becomes structural in every topology, and the
  `/api/_temps` path special case goes away.
- Operators see broken callback delivery in the console instead of
  inferring it from missing data.

### Negative
- One more loopback listener in every topology, and one more bootstrap flag
  when the proxy runs as a separate process.
- Existing installs whose providers were registered against the console
  host keep calling it. That still works if the console host is public, or
  gated and covered by D4. Otherwise it needs D5 to run once after the
  setting is changed.

### Risks
- Moving git receivers to the public router changes which middleware runs
  on them. Any logic that relied on admin-router middleware (request
  metadata, audit context) must be re-provided explicitly. This is covered
  by handler tests in Phase 1.
- D5 calls provider APIs with stored credentials. A partially failed
  re-registration leaves some providers on the old URL. D6 makes this
  visible, and the job is idempotent and retryable.

## Implementation Plan

### Phase 1: the fix (implemented)

Two things in the list below are narrower than the decision text:

- **Certificates.** The ingress hostname sits outside the preview domain
  (D1), and on-demand TLS only issues inside that zone, so it does not cover
  it. The operator provisions the certificate under Domains, as for a
  console on a custom domain.
- **`configured: false`.** With D4 in place the built-in gate no longer
  blocks callbacks, and an external restriction is invisible, so the only
  condition Temps can prove in Phase 1 is an unroutable effective host.

1. `AppSettings.public_ingress_url`, validation, settings API, audit,
   `get_public_ingress_url()`, `public_ingress_hostname()`, reserved-host
   wiring and certificate provisioning.
2. Switch every third-party URL builder to the new getter (D2 table). Expose
   the computed callback URLs to the UI and drop `window.location.origin`
   from `GitProviderFlow.tsx` and `ProjectRevenue.tsx`.
3. Move the git and agent-trigger receivers to `configure_public_routes`
   (D3) and add body limits.
4. Ingress listener plus the proxy three-way decision (D4). Retire the
   `/api/_temps` gate exemption in favour of it.
5. `GET /settings/public-ingress` without `callbacks` yet, the email banner
   and the settings page section (D6).

### Phase 2: re-pointing
6. D5 re-registration job and per-callback status in D6.
7. Tracking-endpoint rate limiting with drop metrics.

### Future work (separate ADRs)
- **Custom tracking domains per email domain** (`track.<sending-domain>`
  CNAME to the ingress host, served by the ingress listener only). This
  improves deliverability and keeps the platform hostname out of customer
  email.
- **Cloud ingress relay** for instances with no public ingress at all,
  dispatching only into the ingress listener.
- **Pull-mode receivers** where providers support them (SES → SNS → SQS
  polled by Temps), which need no inbound path.

## Testing

- **Proxy unit tests** for the three-way decision:
  - ingress host → ingress listener, both with and without the gate;
  - gated denial → ingress listener;
  - allowlisted → console listener;
  - ingress host plus an admin path → 404, never the console listener;
  - project routes are never affected.
- **Router composition test.** Build the ingress app and assert that a
  representative admin route, the SPA fallback and an authenticated
  `/api/_temps` sibling all return 404 on it. The test fails if
  `configure_routes` output ever leaks into the public router.
- **URL builder tests:**
  - every D2 third-party builder uses `public_ingress_url` when set and
    `external_url` when unset;
  - operator-browser links keep using `external_url`.
- **Handler tests** for each moved receiver on the public router: a valid
  signature or token is accepted; a missing or invalid one is rejected
  without DB work; an oversized body gets 413.
- **Settings validation tests:** a path or query is rejected; loopback,
  `*.localho.st` and project domains are rejected; changes are audit-logged.
- **D5 tests** with mocked provider clients: success, partial failure
  reported per callback, and an idempotent re-run.

## Open Questions

1. **How "Test public reachability" probes from outside.** Options: a linked
   instance asks Cloud to fetch a one-time token URL, or the operator is
   shown a one-time URL to open from a phone or other network. Cloud is
   more automatic but only works for linked instances. Not built in Phase 1.
2. **`external_url` changes when `public_ingress_url` is unset.** Should D5
   also run then (current proposal: yes, because it is the effective ingress
   URL), or only on explicit ingress changes?

Resolved during Phase 1:

- **Ingress listener address for a separately-run `temps proxy`:** an
  optional flag, see D4.
- **404 on the public-only surface:** a browser gets the same branded page
  the proxy rendered for a gate-denied request, so gating a console looks
  the same as before; `/api/*` and non-HTML clients get an empty 404.

## References

- `crates/temps-core/src/app_settings.rs`: `external_url`, `console_hostname()`
- `crates/temps-config/src/service.rs`: `get_external_url_or_default`
- `crates/temps-proxy/src/proxy.rs`: `should_consult_admin_gate`
- `crates/temps-proxy/src/services.rs`: `/api/_temps`, `/api/otel` console routing
- `crates/temps-cli/src/commands/serve/console.rs`: `public_app` / `admin_app` assembly and listener modes
- `crates/temps-cli/src/commands/serve/admin_gate.rs`: axum gate layer
- `crates/temps-email/src/services/tracking_service.rs`, `crates/temps-email/src/handlers/tracking.rs`
- `crates/temps-git/src/plugin.rs`, `crates/temps-git/src/handlers/*`
- ADR-017, ADR-045, ADR-048
