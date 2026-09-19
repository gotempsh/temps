# ADR-045: Console Access Through Cloud

**Status:** Proposed
**Date:** 2026-09-19
**Author:** David Viejo
**Builds on:** ADR-039 (management channel, capability negotiation, `StatusRequest`), ADR-040 (Cloud read source — same no-fallback, backend-sets-the-truth discipline), ADR-044 (`managed_by_cloud` row pattern)

---

## Context

### The channel that already exists

A linked instance opens one outbound, long-lived connection dedicated to
proving it is alive: `crates/temps-cloud-client/src/heartbeat.rs` dials
`GET {backend}/v1/management`, upgrades to a WebSocket, authenticates with
the linked bearer token, exchanges `Hello` frames to negotiate a
`Capability` set, then sends `Heartbeat` every 30s. When both sides
negotiate `Capability::InstanceStatusReporting` (ADR-039) the same
connection also carries a slower `StatusReport`, and Cloud may send a
`StatusRequest` back — a request, never a command (`messages.rs:1183–1200`).
Every frame is an `Envelope { kind: String, payload: Value }`
(`messages.rs:17–45`), so an unknown `kind` is dropped, never a fatal parse
error. Nothing on this channel carries end-user HTTP traffic today — the
crate's own doc calls this a *control* plane that "never carries end-user
application traffic" (`lib.rs:23–28`). This ADR is the first thing to cross
that boundary, deliberately, and scopes exactly what crosses it.

### The decision this ADR implements (not relitigated)

An operator who has linked an instance to Temps Cloud can open that
instance's console from Temps Cloud **without exposing any inbound port**.
Cloud terminates the browser's connection on a per-instance hostname it
allocates and proxies the HTTP/WebSocket traffic down over the instance's
existing outbound, authenticated hub connection. The operator authenticates
via **OIDC, with Temps Cloud as the identity provider, from day one** — no
interim password flow. This ADR designs the OSS side: wire frames,
in-process dispatch, managed OIDC provider, settings switch, audit, and the
security boundary.

### What the console actually needs to serve through this

Grepping `web/src` for every live-connection primitive finds nine call
sites, none of which a single buffered request/response can carry:

| File | Mechanism | Backs |
|---|---|---|
| `components/runtime-logs/log-viewer.tsx:1004` | `WebSocket` | Live log tail (initial connect) |
| `components/runtime-logs/log-viewer.tsx:1239` | `WebSocket` | Per-container log tail |
| `components/runtime-logs/log-viewer.tsx:1337` | `WebSocket` | Log tail reconnect |
| `components/deployments/DeploymentStages.tsx:87` | `WebSocket` | Deployment build/job log stream |
| `hooks/useLogStream.ts:231` | `WebSocket` | Shared log-stream hook |
| `components/ai/DebugChatPanel-components.tsx:1727` | `WebSocket` | AI chat streaming |
| `components/agents/AutopilotRunDetail.tsx:1026` | `EventSource` | Autofixer/agent run streaming |
| `components/containers/useContainerMetricsStream.ts:55` | `EventSource` | Live container metrics |
| `pages/SandboxDetail.tsx:1226` | `EventSource` | Sandbox terminal job log tail |

Every one is long-lived, unbounded in duration, and — for `WebSocket` — bidirectional after an HTTP Upgrade. Buffering a single response cannot serve any of them.

### The nearest precedent, and why it falls short

`crates/temps-external-plugins/src/host_api.rs` (`RouterHostApi`) already
solves an adjacent problem: a plugin process calling the platform's own API
without a second credential. It holds a clone of the assembled `Router`,
builds a synthetic request, injects an `AuthContext` from a short-lived
actor token, and drives the router with `.oneshot(request)`
(`host_api.rs:365–493`). The handle is installed into a shared slot the
moment the router exists — not at plugin-connect time — in
`crates/temps-cli/src/commands/serve/console.rs:4251–4290`:

```rust
let bridge = Arc::new(RouterHostApi::new(plugin_api_router, db.clone(), cookie_crypto.clone(), user_service));
service.set_host_api(bridge).await;
```

built from `Router::new().nest("/api", public_router.clone().merge(admin_router.clone()))`
(`console.rs:4171–4172`), deliberately **without** the admin IP-allowlist
gate, because "a channel call arrives in-process ... and is authorised by
an actor token plus the handler's own permission check" (`console.rs:4166–4170`),
not by network topology.

This "shared slot, in-process dispatch" shape is right; the transport
underneath it is not reusable as-is:

1. **It buffers.** `.oneshot()` then `.collect().await.to_bytes()`
   (`host_api.rs:441–451`), capped at `MAX_RESPONSE_BYTES = 8 MiB`
   (`host_api.rs:65`) / `MAX_CALL_BODY_BYTES = 32 MiB`
   (`crates/temps-core/src/external_plugin/channel.rs:401`). A log tail
   never completes; buffering it just hangs forever.
2. **No WebSocket-upgrade concept.** A `oneshot` call returns one
   `Response`; there is nothing for the raw duplex byte stream a `101`
   response hands off to.
3. **Wrong auth model.** `RouterHostApi` mints a synthetic identity because
   a plugin has no browser session. A console-proxied request *does* carry
   real browser cookies (§3) — minting an actor token would solve a
   problem this caller doesn't have.

§2–§3 keep the shared-slot idea and replace the transport with one that streams.

### Forces

1. **No inbound listener, ever.**
2. **Local is primary; Cloud is a relay, never a dependency** — matches
   ADR-040 and `temps-cloud-client/src/lib.rs:6–21`.
3. **Control plane, not hot path — but still bounded.** A handful of
   operator tabs per instance, not `temps-proxy`'s 100k+ req/s regime.
   The hot-path "bounded channel + `try_send` + drop" rule is *wrong* here
   — dropping HTTP body bytes corrupts the response, unlike dropping a
   metrics sample — but bounded memory and explicit backpressure still apply.
4. **`Capability` negotiation is the only supported version-skew mechanism**
   (`lib.rs:57–129`).
5. **The instance is already an OIDC relying party** (`temps-auth::OidcService`)
   — add one more provider row, not a second SSO stack.
6. **No new runtime config as env vars** — a settings-row column, per the
   `cloud.telemetry_enabled`/`backups_enabled` precedent (`app_settings.rs:257–265`).
7. **One domain per crate; each crate owns its error enum.**
8. **A compromised Cloud is an accepted, *bounded* risk.** ADR-040 already
   accepts Cloud can enumerate telemetry pseudonyms; this feature must
   state its own version of that boundary explicitly (§Security), since the
   stakes here are materially higher — interactive admin access, not
   read-only telemetry.

---

## Decision

### 1. A second, dedicated outbound connection

Console-proxy traffic gets **its own WebSocket connection**,
`{backend}/v1/console-proxy`, authenticated like the management channel and
negotiated with a new `Capability::ConsoleProxy` in its own `Hello` — not
layered onto the heartbeat socket. This mirrors, not contradicts, existing
practice: heartbeat itself is separate *because* it must stay "independent
of whatever else it does or does not have to say" (`heartbeat.rs:8–13`). A
large log tail sharing bytes with the 30s heartbeat cadence would
reintroduce exactly the head-of-line-blocking problem that split
justified splitting heartbeat out in the first place. The connection opens
lazily, only while `cloud.console_access_enabled` is true (§5), with the
same bounded-backoff reconnect discipline as `heartbeat.rs`, and its
absence has zero effect on direct console access, telemetry, or backups.

**Graceful shutdown.** On `temps serve` shutdown, before the socket closes,
the instance sends `ConsoleStreamEnd { reason: GoingAway }` on every
currently open stream, so Cloud can distinguish "the instance is restarting,
retry shortly" from an unexplained hard cut and surface a retry to the
browser rather than a dead tab. This is the same shape as every other
bounded-wait shutdown step in this codebase (`join_cloud_enrollment_bootstrap`,
`console.rs:963–989`) — best-effort, time-boxed, and never itself a reason
to delay the shutdown it announces.

### 2. Wire protocol: a small hand-rolled frame family, not a generic mux

**Hand-rolled, in `temps_cloud_protocol`, not `yamux`/`mplex`.** A generic
mux crate multiplexes over a raw byte stream, not an already
message-framed WebSocket — adopting one means discarding
`tokio-tungstenite`'s message framing and re-inventing it. The actual shape
needed (a bounded number of request/response exchanges, some upgrading to
a raw pipe) doesn't need a mux library's general features, and the
protocol crate is deliberately "dependency-light" so an operator can read
exactly what leaves their instance (`messages.rs:4–9`).

**This frame family, and the limits table below, are the canonical wire
contract for console proxying** — `temps_cloud_protocol` is the single
source of truth, exactly as it already is for `SpanRecord`/`Capability`,
and the Cloud backend pins this crate rather than maintaining a parallel
definition. Accepted as final for Phase 1; the Cloud side is being aligned
to this shape rather than the other way around.

**Control frames** are ordinary `Envelope`s (JSON, `Message::Text`):

```rust
pub struct ConsoleStreamOpen {
    pub stream_id: Uuid,
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    pub headers: Vec<(String, String)>,   // bounded, see limits
    pub client_ip: Option<String>,        // the browser's real IP, as Cloud saw it
    pub upgrade_requested: bool,
}
pub struct ConsoleResponseHead { pub stream_id: Uuid, pub status: u16, pub headers: Vec<(String, String)>, pub upgraded: bool }
pub struct ConsoleStreamEnd { pub stream_id: Uuid, pub reason: ConsoleStreamEndReason }
pub struct ConsoleStreamCancel { pub stream_id: Uuid }
pub struct ConsoleStreamRefused { pub stream_id: Uuid, pub reason: ConsoleRefusalReason }
pub struct ConsoleWindowUpdate { pub stream_id: Uuid, pub additional_bytes: u32 }

/// Cloud → instance, sent once right after the console-proxy `Hello`
/// completes, and again on every reconnect — see §4.
pub struct ConsoleOidcConfig {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,   // encrypted at rest immediately on receipt
    pub jwks_uri: String,
    /// The hostname Cloud allocated for this instance's console. Pinned for
    /// the life of the connection; every `ConsoleStreamOpen.headers` `Host`
    /// must match it exactly, see §3.
    pub console_host: String,
}
/// Cloud → instance: console access was disabled or the link was
/// disconnected from Cloud's side. Idempotent with §5's own disable path.
pub struct ConsoleOidcRevoke;
```

`ConsoleRefusalReason`/`ConsoleStreamEndReason` are `#[non_exhaustive]` with
`#[serde(other)] Unknown`, like `Unavailable` (`lib.rs:136–156`).
`ConsoleStreamEndReason` includes a `GoingAway` variant, sent on every open
stream when the instance is shutting down (§1).

**Data frames carry no JSON and no base64.** Request-body chunks,
response-body chunks, and post-upgrade relay bytes are `Message::Binary`
with a fixed 17-byte header then payload: byte 0 packs the format version
in the high nibble (`1`) and the frame kind in the low nibble
(`RequestBodyChunk = 0`, `ResponseBodyChunk = 1`, `WsRelay = 2`); bytes
1–16 are the `stream_id` as a big-endian `u128`. There is no sequence
number — one WebSocket connection already delivers a stream's frames in
order — and no length field, because the WebSocket message boundary is the
length. This avoids the ~33% base64 tax plus JSON-string escaping a byte
field on an `Envelope` would cost, which matters continuously streaming log
tails on a 3 vCPU/4 GB box. `temps_cloud_protocol::console_proxy` is the
normative encoder/decoder.

**Limits (instance-enforced regardless of what Cloud claims):**

| Limit | Value | Rationale |
|---|---|---|
| `MAX_CONCURRENT_CONSOLE_STREAMS` | 16/link | Operator traffic; 17th refused with `TooManyStreams`. |
| `MAX_CONSOLE_FRAME_BYTES` | 64 KiB | Bounds one allocation per frame. |
| `MAX_CONSOLE_HEADER_BYTES` | 16 KiB total | Ordinary HTTP header-size sanity. |
| `CONSOLE_STREAM_IDLE_TIMEOUT` | 120s | Closes abandoned streams; matches every other bounded wait in this crate family. |
| `CONSOLE_STREAM_INITIAL_WINDOW_BYTES` | 256 KiB/direction/stream | Credit-based flow control. |
| Total bytes/sec per link | operator setting, default unthrottled | Same shape as `telemetry_bulk_rate_limit_spans_per_sec` (`app_settings.rs:287–311`) — settings row, never an env var. |

**Flow control is credit-based, not drop-based.** The receiver advertises a
window per stream and replenishes with `ConsoleWindowUpdate`; the sender
must not exceed the last-advertised credit. This is deliberately not the
hot-path "bounded channel + drop" rule — dropping HTTP body bytes corrupts
the response, unlike dropping a metrics sample.

**Compatibility.** `Capability::ConsoleProxy` negotiates like any other
capability; an old Cloud or old instance on either end simply never opens
the connection, matching the `telemetry_query` independence precedent
(`lib.rs:225–237`).

### 3. In-process dispatch: a streaming router handle

Add a shared slot analogous to `RouterHostApi`'s, for a streaming caller.
At the point `admin_app` is assembled in `console.rs:4209–4224`
(`Router::new().nest("/api", admin_router).fallback(serve_static_file)`),
clone that router — before the `admin_gate` IP layer, for the same reason
`plugin_api_router` omits it: authorization here comes from having already
passed through Cloud's auth and the browser's own session cookie, not from
network topology — and hand it to a new `ConsoleRouterHandle` read from a
shared slot `CloudService` populates the same way `RouterHostApi`'s bridge
is installed post-construction (`console.rs:4263–4290`).

```rust
// crates/temps-cloud/src/console_proxy.rs (new)
pub struct ConsoleRouterHandle { router: Router }  // admin_router + fallback(serve_static_file), no admin_gate layer
```

**No actor token.** `RouterHostApi` mints an `AuthContext` because a plugin
has no browser session. A console-proxy request is the browser's own real
request, cookies included, tunneled rather than re-authored — Cloud is a
transparent L7 relay. The console's existing session-cookie middleware
authenticates it exactly as it would a direct browser, provided the
synthetic request is built correctly.

**Building the request.** Body is `axum::body::Body::from_stream(...)` fed
by a channel the dispatcher pushes into as `console_request_body_chunk`
frames arrive — streamed in, never buffered whole. Two headers are
synthesized, not merely forwarded:

- **`Host`**: the per-instance console hostname Cloud allocated, never the
  instance's own `external_url`. **Pinned, not trusted per-request**: the
  instance learns this hostname exactly once per connection, from
  `ConsoleOidcConfig.console_host` (§4), and a `ConsoleStreamOpen` whose
  declared `Host` differs is refused with `ConsoleStreamRefused{HostMismatch}`
  before a request object is even built — Cloud cannot redirect a stream to
  a different hostname mid-connection by forging this header. **The unset
  case is fail-closed, not fail-open**: a `ConsoleStreamOpen` that arrives
  on a connection where `ConsoleOidcConfig` has not yet been applied — a
  race at connect time, a Cloud bug, or a hostile peer skipping the
  handshake — has no pin to compare against and is refused with
  `ConsoleStreamRefused{NotConfigured}` rather than treated as
  automatically matching or dispatched with `Host` unset. Nothing is ever
  forwarded to the router while the pin is absent.
  `RequestMetadata::build_from_request` derives `host`/`base_url`/cookie
  scope purely from `Host` (`request_metadata.rs:115–132`), and the session
  cookie carries no `Domain` attribute (only `.same_site(...)`, e.g.
  `auth_service.rs:222`) — so it scopes correctly to the Cloud-issued
  hostname with zero code change.
- **`X-Forwarded-Proto: https`**, **`X-Forwarded-For: <real browser IP>`**
  (from `ConsoleStreamOpen.client_ip`), **plus a synthetic
  `ConnectInfo(loopback)`** on the request extensions. This is the load-bearing
  detail: `resolve_client_ip` (`client_ip.rs:26–53`) trusts `X-Forwarded-For`
  only from a loopback peer. A synthetic request has no real TCP peer, so
  without this, every console-proxied request's audited IP and rate-limit
  key collapse to `"unknown"` — the same gap `RouterHostApi` already has
  today (no `ConnectInfo`, no XFF forwarded). This ADR does not need to fix
  that pre-existing gap (a plugin call has no real client IP to report) but
  must not repeat it, since a console-proxied request *does* have one.

**Streaming the response.** `router.clone().call(request)` returns as soon
as headers exist; the dispatcher sends `ConsoleResponseHead` immediately,
then polls the body as a stream, emitting `console_response_body_chunk`
frames within the peer's advertised window. No total-size cap — an
unbounded log tail is the normal case, not an edge case to reject, unlike
`RouterHostApi`'s 8 MiB ceiling.

**WebSocket upgrade passthrough.** axum's `WebSocketUpgrade` needs a
`hyper::upgrade::OnUpgrade` on the request, normally inserted by hyper's
own connection driver reading a real socket. For an in-process request:
open a `tokio::io::duplex(BUF)` pair, run
`hyper::server::conn::http1::Builder::new().serve_connection(server_half, tower_service).with_upgrades()`
against one end, and feed the synthesized request bytes into the other.
Hyper then drives the upgrade exactly as for a real connection — the same
path already serving every WebSocket handler in this codebase
(`container_exec.rs`, `sandbox/terminal.rs`) — handing back an `Upgraded`
stream backed by the duplex's client half. The dispatcher relays raw bytes
between that half and `console_ws_frame` data frames until either side
closes. More code than the non-upgrading path, but it reuses hyper's own
framing rather than reimplementing RFC 6455, and none of the six
`WebSocket` call sites in `web/src` need any awareness of the tunnel.

### 4. Authentication: Cloud as a managed, day-one OIDC provider

**No new SSO stack.** `OidcService` already does discovery, PKCE, and
claim resolution (`oidc_service.rs:130–207`, `:722–884`). This ADR adds one
`oidc_providers` row, marked Cloud-managed, and reuses everything else.

**Provisioning.** `oidc_providers` gains `managed_by_cloud BOOLEAN NOT NULL
DEFAULT FALSE`, mirroring `s3_sources.managed_by_cloud`
(`temps-entities/src/s3_sources.rs:50`, `m20260830_000001_...`). Credentials
travel over the console-proxy channel, not `Hello`/`EnrollResponse`: once
the console-proxy `Hello` completes, Cloud sends `ConsoleOidcConfig
{ issuer, client_id, client_secret, jwks_uri, console_host }` (§2), and
`CloudService` upserts the row via `OidcService::create_provider`/
`update_provider` — `issuer_url` = `issuer`, `client_secret` encrypted at
rest immediately like every other provider's secret,
`scopes = "openid email profile temps_cloud_instance_role"`,
`template = "temps_cloud"` (a UI hint), `jit_provisioning = true`,
`trust_idp_email = true`. **`ConsoleOidcConfig` is re-sent on every
reconnect, and the upsert is idempotent** (same shape as
`create_provider`/`update_provider` already support for an operator editing
a provider by hand) — an instance that was offline for a client-secret
rotation, or a revoke-then-reissue, converges to Cloud's current
configuration the moment it reconnects, rather than continuing to use a
stale secret until an operator notices. `ConsoleOidcRevoke` is the inverse:
Cloud sends it when console access is disabled or the link is disconnected
from *its* side, and the instance runs exactly the disconnect/revoke
sequence below — the same code path §5's local toggle uses, just triggered
remotely.

`trust_idp_email = true` is deliberate: the `email_verified` gate exists to
stop an attacker self-registering at an arbitrary IdP with a victim's email
(`oidc_service.rs:761–797`). Reaching this exchange at all already required
operator-level access to the Cloud account this instance is enrolled
under — the same carve-out `trust_idp_email`'s own doc comment describes
for admin-controlled corporate IdPs (`oidc_providers.rs:27–34`).

**Claim mapping.** `sub` = the Cloud account's stable id — the same id for
that person on every instance they administer, so `resolve_user`'s existing
`(provider_id, subject)` fast path (`oidc_service.rs:744–753`) gives each
Cloud account its own local user row and its own audit trail on this
instance, never a shared "cloud-admin" account. `email` = the Cloud
account's email, taken under `trust_idp_email` as described above. A custom
claim, `temps_cloud_instance_role`, carries this account's role on *this
specific instance* (Cloud-computed, since one account can be owner of one
linked instance and unrelated to another) — distinct from the generic
`role_claim`/`group_claim` convention because it must be instance-scoped.

**Role gate: two independent layers.** The requirement — "owner/admin
become instance admins; others rejected" — is a hard reject, not
`evaluate_role`'s existing fallback to `RoleType::User`
(`oidc_service.rs:1395–1428`, which never refuses on its own). (1)
*Primary*: Cloud's own account-linking screen never offers "open this
console" to a non-owner/admin member — not this ADR's code, but the
intended first line. (2) *Belt-and-suspenders, OSS-side*: a new
`oidc_providers.admin_only_role_required` boolean, set only on the
Cloud-managed row; `resolve_user` checks it after `evaluate_role` and
before linking/JIT-provisioning, returning a new `OidcError::InsufficientRole`
if the resolved role isn't `Admin` rather than falling through to `User`.
This is the same posture ADR-040 already accepts for Cloud — made
explicit rather than assumed, so even a bug on Cloud's screen can't hand
out more than admin through this provider. `oidc_role_mappings` rows stay
simple: `owner → admin`, `admin → admin`, `* →` a role the check rejects.

**Disconnect/revoke.** Disabling console access, or disconnecting the
link, deletes the managed provider row and invalidates every session for a
user whose `users.oidc_provider_id` points at it (`resolve_user` already
sets this column, `oidc_service.rs:807,874`) — one indexed delete, ordered
before credential revocation like ADR-044 §5's disconnect sequencing.

**Login page.** `GET /auth/oidc/providers` returns `OidcProviderSummary { slug, name, template }`
(`oidc_service.rs:208–223`); the login page renders one button per
provider. `template = "temps_cloud"` picks distinct copy/icon ("Continue
with Temps Cloud"), and the row — and its button — exists only while
`cloud.console_access_enabled` is true, so the button never leads nowhere.

**Audit: reuse for success, new rows for denials.** Every OIDC login already
writes `LoginAudit { login_method: "oidc", .. }` (`oidc_handler.rs:461–473`);
provisioning already writes `OidcProviderCreatedAudit`/`OidcProviderDeletedAudit`.
No new audit *type* is needed for successful logins — but `login_method` is
currently a constant identical for every provider, so a Cloud login is
indistinguishable from any other SSO login in the trail. Phase 1 changes
that one line to `format!("oidc:{}", provider.template)`.

**Denials must be audited too, not only successes** — a security review
finding, not an afterthought: an `OidcError::InsufficientRole` hard-reject
(§ role gate above) and a WebSocket-upgrade Origin mismatch (§Security
Model) are both attempts that matter to an operator precisely because they
failed. Both get their own audit row (`ConsoleSsoLoginDeniedAudit { reason:
"insufficient_role" | "origin_mismatch", .. }`, following the existing
`AuditOperation` shape in `temps-auth/src/audit.rs`), written on the
rejection path, not folded into `LoginAudit{success: false}` — the existing
`success: false` case on `LoginAudit` means "credentials didn't resolve to
a user at all", a different failure mode from "resolved to a real Cloud
account that the instance-side gate then refused to admit".

### 5. Settings: `cloud.console_access_enabled`

One field on `CloudSettings` (`app_settings.rs:257–265`) and
`CloudFeatureSwitches` (`temps-cloud-client/src/lib.rs:58–63`):

```rust
pub struct CloudSettings { /* ... */ pub console_access_enabled: bool }
pub struct CloudFeatureSwitches { pub telemetry: bool, pub backups: bool, pub notifications: bool, pub console_access: bool }
```

**Default depends on how the link was established**, reusing
`CloudEnrollmentActor` (`temps-cloud/src/handler.rs:65–68`,
`console.rs:903–961`): `UnattendedBootstrap` (the
`TEMPS_CLOUD_ENROLLMENT_CODE` first-boot path — no operator present) →
**default on**. `Operator(_)` (a pasted code) → **default off**, matching
`telemetry_enabled`/`backups_enabled`'s "linking never enables export;
settings are applied explicitly" rule (`temps-cloud-client/src/lib.rs:53–56`).
Never an env var.

**Toggling on** negotiates `Capability::ConsoleProxy`, opens the
console-proxy connection, provisions the managed OIDC row. **Toggling off**
does the reverse — symmetric with disconnect.

**Surface.** `CloudSettingsPage.tsx` already renders three `Switch`
controls reading one status response (`:172–460`); a fourth,
`console_access`, joins them, **always rendered**: not linked → disabled,
links to enrollment; linked but capability not negotiated (old Cloud) →
disabled, "not supported by this Temps Cloud version yet" (distinct
wording from "unavailable", per `Capability::TelemetryQuery`'s own
precedent, `lib.rs:79–82`); available, off → enabled with a concrete
description; on → shows the allocated hostname and a sign-in preview link.

Phase 2 (deferred): per-user role mapping beyond the binary admin/rejected
gate, and session policy specific to Cloud-originated sessions.

---

## Invariants

- **Local-first, unconditionally.** Direct console access is unaffected by
  a Cloud outage — the console-proxy connection is a purely additive
  consumer of the same router, never a dependency of it.
- **The proxy path never blocks or slows local request handling.** Each
  stream runs on its own task with credit-based backpressure (§2); a
  stalled Cloud connection stalls only console-proxy streams, never the
  console's own listener or `temps-proxy`, which shares no process boundary.
- **The instance enforces every limit independently of Cloud** — §2's
  table applies regardless of what Cloud sends or claims.
- **Classification: control plane, not hot path.** Bounded by operator
  concurrency (16 streams), not request rate; held to CLAUDE.md's "normal
  service rules, bounded memory, no unbounded fan-out" bar, not the
  atomics/no-alloc hot-path bar.

---

## Security Model

**What a compromised Cloud can do, by design:** act as any Cloud account
holding owner/admin on this instance — an ordinary instance-admin session,
same as a password login would grant. This is ADR-040's "compromised Cloud"
trust boundary, extended from "read telemetry" to "administer the
instance", because that is what this feature is.

**What it cannot do:** bypass instance auth (every request still passes
through the same `RequireAuth`/session middleware; Cloud relays HTTP, it
does not forge an `AuthContext` — §3 inserts none), run code outside the
console's own API surface, or reach Docker/the host filesystem except
through APIs an authenticated admin could already reach directly.

**The precise mechanism a compromised Hub gains, named rather than left
implicit.** The Hub (Cloud's relay component terminating the per-instance
hostname) sees two things a browser normally never exposes to a third
party: the OIDC `client_secret` (delivered to the instance via
`ConsoleOidcConfig`, §4, but visible to whatever forwarded it), and every
tunneled `/oidc/callback` request in full — the authorization code and the
PKCE-verifier cookie together. Holding both, a compromised Hub does not
need to wait for a browser to complete a login; it can perform the
code→token exchange itself. This is a materially more specific claim than
"compromised Cloud = instance admin" above, and the mitigations are
correspondingly specific rather than generic:

- **Cloud-side**: authorization codes are single-use and expire in 60s,
  with reuse detection — a code the Hub replays after the legitimate
  exchange already happened is rejected, and a double-redemption is a
  detectable signal on Cloud's side.
- **OSS-side (this ADR's scope)**: (a) the callback is bound to the
  PKCE-verifier cookie with `SameSite=Strict` (already the cookie floor,
  `auth_service.rs`), so a code intercepted in transit cannot be redeemed
  from a context that doesn't also hold that cookie; (b) the RP rejects any
  callback whose `state` was not minted by this instance within the last
  60s, matching the code's own lifetime rather than the RP's normal,
  longer `LOGIN_STATE_TTL_MINUTES` (`oidc_service.rs:39`) — tightened
  specifically for the Cloud-managed provider, since its authorization
  codes are known to expire that fast; (c) every callback failure is
  audited (§4), so a pattern of rejected/expired/reused callbacks is
  visible rather than silent.
- **Token lifetimes the RP must tolerate**: Cloud issues ID/access tokens
  with a 120s lifetime and **no refresh tokens** — openidconnect 4.x's
  token exchange must not assume a longer-lived token or attempt a refresh
  that doesn't exist. This is a non-issue for session length: the
  instance's own session policy (its own cookie TTL, MFA, revocation)
  governs everything *after* the initial exchange, exactly as it does for
  a password login. The short OIDC token lifetime only bounds the login
  handshake itself, not how long the resulting session lasts.

**What tunneled application content this exposes, stated honestly rather
than only as a byte-volume concern.** Six of the nine call sites in Context
carry customer application output through the Hub, not just Temps'
own operational metadata: live log tails, AI chat conversation content, and
sandbox terminal sessions are administratively-accessed customer/application
data flowing through a third party's relay. §2's frame-size and rate limits
bound how much moves and how fast — they do not change *what kind* of
content moves. This is precisely why `console_access_enabled` is a real,
explicit switch rather than an automatic consequence of linking, and why
its default is off for operator-initiated enrollment (§5) — an operator
must affirmatively decide their application's log/chat/terminal content is
acceptable to relay through Cloud, not discover after the fact that linking
did this silently.

| Threat | Mitigation |
|---|---|
| Forged `Host` misdirecting cookies/CSRF | `Host` is set by the dispatcher from the authenticated stream's allocated hostname, never from anything Cloud forwards from the browser; unset/unpinned is refused, never defaulted (§3). |
| CSRF across the two hostnames | `SameSite=Strict` on every session cookie already (`auth_service.rs:222`) — a cookie set on the Cloud hostname is never sent to a request not addressing it. |
| Open redirect on OIDC callback | Reuses the existing `sanitize_return_to_rejects_open_redirect` validation unchanged; no new callback endpoint. |
| Cookie scope leaking across instances | No `Domain` attribute is ever set (confirmed: only `.same_site(...)` calls) — a host-only cookie can't leak to a different hostname. |
| WS Origin on upgrade | **Decided, not deferred**: for every tunneled Upgrade request the dispatcher validates `Origin == https://<console_host>` (the connection's pinned hostname, §3) *before* the request reaches the router; a mismatch is a 403 plus an audit row (§4), never a silent drop. `SameSite=Strict` remains as defense in depth underneath this, not as the sole control — the gap this closes is a request that never carries the session cookie's `SameSite` context at all (a raw non-browser WS client), which Origin-checking catches and cookie policy alone does not. |
| Compromised Hub completing an OIDC exchange itself | See the dedicated mechanism above: short-lived single-use codes (Cloud), PKCE-cookie binding + tightened `state` TTL + failure audit (OSS). |
| Audit/rate-limit IP spoofing | §3's `ConnectInfo(loopback)` + `X-Forwarded-For` reuses `resolve_client_ip`'s existing trust gate rather than a new mechanism; verified against `crates/temps-auth/src/rate_limit.rs:112–128`. |
| Resource exhaustion | §2's stream/frame/idle-timeout caps, instance-enforced. |
| OIDC provider over-granting | §4's two-layer role gate, not Cloud's screen alone; both the reject and an Origin mismatch are audited (§4), not just successful logins. |

**Required sign-off:** `security-auditor`, specifically on the role gate,
the `Host`/cookie-scope design, the Origin-validation implementation, and
the WebSocket-upgrade relay.

---

## Alternatives Considered

- **Path-prefix proxy** (`cloud/instances/{id}/*`). Simpler DNS/certs, but
  the console SPA assumes it owns its origin (absolute paths, cookie
  scope) — the same reason the embedded-UI override in `console.rs:106–117`
  uses a separate listener, not a prefix. **Rejected.**
- **Instance opens its own inbound port + cert; Cloud just points DNS.**
  Violates the entire premise for operators behind NAT/CGNAT/firewalls —
  exactly this product's audience. **Rejected** on the constraint alone.
- **Store the admin password in Cloud.** No new SSO stack needed, but a
  long-lived high-privilege secret held by a third party for a purpose
  OIDC already solves without one; duplicates rotation/MFA policy.
  **Rejected.**
- **Iframe-embed the console in the Cloud dashboard.** Doesn't solve
  reachability at all — the console still needs a URL Cloud can iframe —
  and adds clickjacking surface. **Rejected**, orthogonal to the constraint.
- **SSH access instead.** Different product entirely (no browser UI: logs,
  deployment status, project settings); still needs an inbound port or its
  own tunnel. **Rejected** as not meeting the requirement.

---

## Consequences

### Positive
- Full console access with zero inbound configuration for operators behind NAT/CGNAT/firewalls.
- No second credential for ordinary use — the browser's own OIDC session is the only identity, audited through existing `LoginAudit`/session infrastructure.
- The frame family and its limits are fully visible in the public protocol crate.
- Every existing console feature (log tails, AI chat, deployment streams, sandbox terminals) works over the tunnel unmodified, because the seam sits below the router.

### Negative
- A second always-open outbound connection per instance with console access enabled.
- The dispatcher (stream bookkeeping, flow control, the duplex-pipe upgrade relay) is genuinely new, non-trivial code in a security-sensitive path.
- "Compromised Cloud = instance admin" is a strictly larger blast radius than ADR-040's "compromised Cloud = read telemetry" — inherent to the feature, must be stated plainly to whoever approves it.

### Risks
- **Upgrade-relay correctness.** The duplex/hyper-upgrade technique is the least precedented piece of this design here. Mitigation: a dedicated integration test through a real WebSocket handler before shipping.
- **Role-gate bypass.** If Cloud's authorize-time check is ever misconfigured, the OSS-side reject (§4 layer 2) is the only remaining control — must ship in the same phase as the provider, never as a follow-up.
- **Flow-control deadlock.** A side that never replenishes credit can wedge a stream open. Mitigated by the idle timeout as a backstop independent of the flow-control logic.

---

## Implementation Notes

**Affected crates:** `temps-cloud-protocol` (frames, `Capability::ConsoleProxy`, limits), `temps-cloud-client` (second connection, dispatcher, upgrade relay), `temps-cloud` (`ConsoleRouterHandle`, OIDC provisioning, `CloudFeatureSwitches.console_access`), `temps-auth` (`oidc_providers.managed_by_cloud` + `admin_only_role_required`, `resolve_user` role gate, `login_method` audit fix, `OidcProviderSummary.template`), `temps-entities`/`temps-migrations` (both new columns), `temps-cli` (`console.rs` slot wiring), `web/` (settings switch, login button), `apps/temps-cli` (status/enable/disable parity — no Rust subcommand, per repo rule).

**Migration:** yes — two additive, defaulted `oidc_providers` columns; one additive `CloudSettings` field. No existing row's behaviour changes.

**Breaking changes:** no. `Capability::ConsoleProxy` is negotiated; every new field is `#[serde(default)]`.

### Phase 1 — end-to-end SSO console access (this ADR's deliverable)

1. `temps-cloud-protocol`: frame family, limits, `Capability::ConsoleProxy`, round-trip + negotiation tests.
2. `temps-cloud-client`: second connection with its own reconnect/backoff; dispatcher with stream table, credit-based flow control, idle-timeout reaper.
3. `temps-cli`/`temps-cloud`: `ConsoleRouterHandle` slot wiring; the duplex-pipe/hyper-upgrade relay.
4. `temps-auth`: new columns; `resolve_user` hard-reject check; `login_method` audit fix; `OidcProviderSummary.template`.
5. `temps-cloud`: managed-provider upsert/teardown on enable/disable and on disconnect; `console_access_enabled` with the bootstrap-actor-dependent default.
6. `web/`: settings switch with the four onboarding states; login-page button; regenerate `web/src/api/client/`.
7. `apps/temps-cli`: status/enable/disable parity.

**Security review required before merge**, on the role gate, `Host`/cookie-scope design, and the WebSocket-upgrade relay.

### Phase 2 — deferred
Per-user role mapping beyond the binary admin/rejected gate; session policy specific to Cloud-originated sessions (shorter TTL, step-up re-auth, concurrent-session limits beyond the existing observational `ConcurrentSessionDetectedAudit`).

---

## Testing

- **Protocol round-trip**: `Envelope::new`/`decode` for every new frame; `Hello::negotiate` with `Capability::ConsoleProxy` against a peer lacking it.
- **Fake-Cloud-socket drives a real router**: open a real console-proxy WS server standing in for Cloud, drive `ConsoleStreamOpen` against a real assembled `Router`, assert headers arrive before body completes (no buffering), a large body arrives across multiple frames bounded by `MAX_CONSOLE_FRAME_BYTES`, and a WS-upgrading route round-trips bytes through the duplex relay both ways.
- **Flow control**: a consumer that never sends `ConsoleWindowUpdate` stalls the sender rather than buffering unboundedly or dropping bytes.
- **Limits**: 17th concurrent stream refused with `TooManyStreams`; an idle stream past timeout closes with the correct reason; a `ConsoleStreamOpen` whose `Host` doesn't match the connection's pinned `ConsoleOidcConfig.console_host` is refused with `HostMismatch` and never reaches the router.
- **Fail-closed hostname pin**: a `ConsoleStreamOpen` sent on a fresh connection *before* `ConsoleOidcConfig` has been applied is refused with `NotConfigured` — asserted by a test that opens a stream in that exact race window and checks the router was never called.
- **WS Origin enforcement**: a tunneled Upgrade request with `Origin` equal to the pinned `console_host` reaches the router; one with any other `Origin` (including none) gets a 403 before the router sees it, and produces exactly one audit row.
- **Rate-limit keying regression**: a console-proxied request through the full dispatch path lands in `auth_rate_limit_middleware`'s bucket for the real browser IP, not for the synthetic loopback address — guards §3's `ConnectInfo`/`X-Forwarded-For` synthesis against silent removal.
- **Graceful shutdown**: a shutdown with open streams sends `ConsoleStreamEnd{GoingAway}` on each before the socket closes; none are left to time out or hard-cut.
- **OIDC RP against a stub issuer**: a Cloud-shaped provider with `admin_only_role_required = true`; an `owner`/`admin` claim logs in, any other claim is refused with `InsufficientRole` (and audited, not just rejected), never silently downgraded to `User`. A separate case exercises a `state` older than 60s against the tightened TTL and asserts rejection plus an audit row.
- **Audit**: a Cloud SSO login writes a `login_method` distinguishable from any other provider's; an `InsufficientRole` rejection and an Origin mismatch each write their own denial audit row, distinct from `LoginAudit{success:false}`.
- **Settings UI test**: the new switch renders all four onboarding states, never absent.
- **e2e**: a real (or realistic stub) linked instance with console access enabled — SPA loads, a log tail streams live, an AI chat WebSocket round-trips.

---

## Open Questions

None remain open for Phase 1 as of this revision — the five questions raised
in the initial draft were resolved during design review and are recorded
here, closed, so a later implementer doesn't re-litigate them:

1. **Credential delivery — resolved.** `client_id`/`client_secret` travel
   over the console-proxy channel via `ConsoleOidcConfig`, sent once per
   connection (including every reconnect) rather than once at link time —
   see §2/§4. This also gives rotation and revocation a natural delivery
   path (`ConsoleOidcConfig`/`ConsoleOidcRevoke`) instead of requiring a
   separate mechanism later.
2. **Hostname pinning — resolved.** `ConsoleOidcConfig.console_host` is the
   pinned value for the connection's lifetime; a `ConsoleStreamOpen` with a
   different `Host` is refused (`HostMismatch`), never dispatched — see §3.
3. **One user per Cloud account — resolved.** Confirmed by
   `resolve_user`'s existing `(provider_id, subject)`-keyed link path
   (`oidc_service.rs:744–753`): each Cloud account gets its own local user
   row and its own audit trail on this instance. No shared account. See §4.
4. **Rate-limit keying — verified, no change needed.** Checked
   `crates/temps-auth/src/rate_limit.rs:112–128`:
   `auth_rate_limit_middleware` already extracts `ConnectInfo` and calls
   `resolve_client_ip(request.headers(), Some(peer))`
   (`crates/temps-core/src/client_ip.rs:26`) — the *resolved* client IP, not
   the raw peer socket. Because §3 inserts a synthetic loopback
   `ConnectInfo` plus a correct `X-Forwarded-For` before dispatch, this
   middleware already keys correctly on the real browser IP for
   console-proxied requests with zero code change. Worth a regression test
   (§Testing) precisely because it depends on §3's synthesis being present
   on every dispatched request, not because the middleware itself needs to change.
5. **Graceful restart — resolved.** §1 now specifies `ConsoleStreamEnd
   { reason: GoingAway }` sent on every open stream before the console-proxy
   socket closes on shutdown, so Cloud surfaces a retry to the browser
   instead of a silent hard cut.

---

## References

- `crates/temps-cloud-protocol/src/lib.rs`, `messages.rs` — `Capability`, `Hello`, `Envelope`, `Heartbeat`, `StatusRequest`, `EnrollResponse`
- `crates/temps-cloud-client/src/heartbeat.rs` — the dedicated management connection this design's console-proxy connection is modeled on and kept separate from
- `crates/temps-external-plugins/src/host_api.rs` — `RouterHostApi`, the nearest in-process dispatch precedent
- `crates/temps-cli/src/commands/serve/console.rs:4119–4290,825–1104` — router assembly/shared-slot pattern; `CloudEnrollmentActor` bootstrap flow
- `crates/temps-core/src/request_metadata.rs`, `client_ip.rs` — `RequestMetadata` and the loopback-trust gate on `X-Forwarded-For`
- `crates/temps-auth/src/rate_limit.rs:112–128` — `auth_rate_limit_middleware`, verified to already key on `resolve_client_ip`, not the raw peer socket
- `crates/temps-auth/src/oidc_service.rs`, `oidc_handler.rs`, `audit.rs` — `OidcService`, `resolve_user`, `evaluate_role`, `trust_idp_email`, existing audit types
- `crates/temps-entities/src/s3_sources.rs`, `m20260830_000001_add_managed_by_cloud_to_s3_sources.rs` — the `managed_by_cloud` precedent
- `crates/temps-core/src/app_settings.rs:249–312` — `CloudSettings`; `web/src/pages/settings/CloudSettingsPage.tsx` — switches this ADR's joins
- ADR-039 (capability negotiation), ADR-040 (compromised-Cloud trust boundary this ADR extends), ADR-044 (provisioning/disconnect ordering this ADR's OIDC lifecycle follows)
