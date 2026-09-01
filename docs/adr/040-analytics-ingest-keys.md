<!--
SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0
-->

# ADR-040: Key-Based Analytics Ingest for Apps Not Deployed by Temps

## Status

Proposed — requires `security-auditor` sign-off before implementation (see
[Security Considerations](#security-considerations-for-security-auditor)).

Part of gotempsh/temps#848. Builds on the client-generated visitor/session-id
fallback landed in `2b990321c` (`feat(analytics): fall back to a
client-generated visitor/session id`), which is a **prerequisite**, not an
alternative — see [Relationship to the visitor-id fallback](#relationship-to-the-visitor-id-fallback).

## Context

### The problem

All four public analytics ingest endpoints resolve
`project_id` / `environment_id` / `deployment_id` **exclusively** by matching
the request `Host` header against the in-memory proxy route table:

| Endpoint | Handler | Resolution call |
| --- | --- | --- |
| `POST /api/_temps/event` | `crates/temps-analytics-events/src/handlers/events_handler.rs:728` | `state.route_table.get_route(&host)` (`:751`) |
| `POST /api/_temps/speed` | `crates/temps-analytics-performance/src/handlers/handler.rs:532` | `state.route_table.get_route(&host)` (`:556`) |
| `POST /api/_temps/speed/update` | `crates/temps-analytics-performance/src/handlers/handler.rs:689` | `state.route_table.get_route(&host)` (`:713`) |
| `POST /api/_temps/session-replay/init` | `crates/temps-analytics-session-replay/src/handlers/handler.rs:805` | `state.route_table.get_route(&metadata.host)` (`:832`) |
| `POST /api/_temps/session-replay/events` | `crates/temps-analytics-session-replay/src/handlers/handler.rs:930` | `state.route_table.get_route(&metadata.host)` (`:941`) |

The route table only contains an entry for an environment that has a live
Temps-managed deployment. `crates/temps-routes/src/route_table.rs:681` gates
the entire `environment_domains` load behind
`if let Some(deployment_id) = environment.current_deployment_id`; the
`project_custom_domains` section (~`:914-1010`) has the same shape.

Consequence: if a user runs Temps purely as a **self-hosted observability
backend** for an app that Vercel/Netlify/Fly/their own nginx serves, there is
no route-table entry to match, so every ingest request returns 404 (events,
speed) or a 404 `Problem` (session replay). No amount of cookie or visitor-id
work fixes this — the request is rejected before identity is ever considered.

This is a hard blocker on a first-class use case: "bring your own hosting,
use Temps for analytics + replay + errors". Error tracking already works in
that configuration (Sentry-compatible DSN, keyed auth); analytics does not.

### What already exists to copy from

**Error tracking / DSN (key-based, product-facing precedent).**
`project_dsns` (`crates/temps-entities/src/project_dsns.rs`) carries
`project_id`, nullable `environment_id`/`deployment_id`, `public_key`,
`secret_key` (deprecated, always empty), `is_active`,
`rate_limit_per_minute`, `allowed_origins: Json`, `event_count`,
`last_used_at`. `DSNService`
(`crates/temps-error-tracking/src/sentry/dsn_service.rs`) mints a key with
`generate_key(32)` — 32 random bytes hex-encoded to **64 characters**, not 32
— and validates by `project_id + public_key + is_active`
(`validate_dsn_auth`, `:224`; `validate_dsn`, `:249`). The key is extracted
from a `?sentry_key=` query param, an `X-Sentry-Auth` header, or
`Authorization: DSN ...` (`extract_dsn_key`,
`crates/temps-error-tracking/src/sentry/handlers.rs:624`). Admin CRUD lives at
`crates/temps-error-tracking/src/sentry/dsn_handlers.rs:48-66`, gated by
`ErrorTrackingCreate` / `ErrorTrackingRead` / `ErrorTrackingWrite`. Ingest is
rate-limited by a per-project sliding window
(`crates/temps-error-tracking/src/sentry/rate_limiter.rs`) and served under
`CorsLayer::new().allow_origin(Any)` (`handlers.rs:88`).

**OTel ingest (bearer-token precedent).**
`IngestAuth::authenticate_any` (`crates/temps-otel/src/ingest/auth.rs:212`)
reads a token from `Authorization: Bearer` or `X-Temps-Api-Key`
(`crates/temps-otel/src/handlers/ingest_handler.rs:120,157`) and dispatches by
prefix: `si_` service ingest token, `dt_` deployment token, else `tk_` project
API key. Resolution is cached with a 5s TTL (`AUTH_CACHE_TTL`, `:49`).

**Sentry tunnel (Host-based, no credential).**
`ingest_tunneled_envelope` (`handlers.rs:344`) shows the "Host-resolved,
Origin-checked" pattern including `origin_matches_host` (`:437`) and the use
of the wildcard-aware `get_route_by_host` rather than the narrower
`get_route`.

Analytics has **neither** a key path nor an Origin check today.

### Dependency graph (verified)

```
temps-analytics          deps: temps-auth, temps-ai, temps-core,
                               temps-database, temps-entities, temps-migrations
                               (+ moka, rand, sea-orm, axum, utoipa)

temps-analytics-events   deps: temps-core, temps-database, temps-entities,
                               temps-auth, temps-proxy, temps-geo,
                               temps-analytics, temps-analytics-backend, temps-config
temps-analytics-performance
                         deps: temps-core, temps-entities, temps-routes,
                               temps-geo, temps-auth
temps-analytics-session-replay
                         deps: temps-core, temps-database, temps-entities,
                               temps-auth, temps-routes

temps-error-tracking     deps: temps-auth, temps-core, temps-database,
                               temps-embeddings, temps-entities, temps-geo,
                               temps-monitoring, temps-notifications,
                               temps-projects, temps-config, temps-proxy,
                               temps-routes, temps-migrations
temps-proxy              deps: ... temps-analytics ...
```

Two facts drive the design:

1. `temps-analytics` (the umbrella crate) is **already** a dependency of
   `temps-analytics-events`, and depends on nothing that could form a cycle
   with `temps-analytics-performance` or `temps-analytics-session-replay`. It
   already pulls in `sea-orm`, `temps-entities`, `temps-database`, `rand` and
   `moka` — everything a key service needs. It is a plugin crate with its own
   admin `configure_routes()` (`crates/temps-analytics/src/handler.rs:200`).
2. `temps-error-tracking` transitively depends on `temps-analytics` (via
   `temps-proxy`). Making the three ingest crates depend on
   `temps-error-tracking` to reuse `DSNService` would not literally cycle, but
   it inverts the layering (analytics depending on error tracking) and drags
   `temps-embeddings`, `temps-notifications`, `temps-monitoring` and
   `temps-projects` into every analytics ingest crate.

## Decision

Add a **project-scoped, optionally environment-scoped, non-secret analytics
ingest key** as a second resolution path for all five public analytics ingest
endpoints. When a key is presented, it resolves the ingest scope outright; the
`Host` header is no longer consulted for resolution. When no key is presented,
today's Host/route-table behaviour is preserved byte-for-byte.

### 1. Entity and schema — a new dedicated table

**`analytics_ingest_keys`**, not a generalization of `project_dsns`.

Rationale:

- `project_dsns.public_key` has a **global unique index**
  (`idx_project_dsns_public_key`) and `validate_dsn_auth` matches on
  `project_id + public_key + is_active` only. Reusing the table means one
  credential value is simultaneously a valid Sentry DSN key and a valid
  analytics key — a cross-product confused deputy with no way to scope it down
  later. Two tables makes the negative ("a `pa_` value can never authenticate
  error ingest") structurally true rather than a code invariant.
- Revocation semantics conflate: revoking an error-tracking DSN in the Console
  would silently kill analytics ingest, and vice versa. Operators would have
  no way to tell which product they just broke.
- Permission naming conflates: DSN CRUD is gated by `ErrorTrackingWrite`.
  Analytics ingest keys must be gated by `AnalyticsWrite`.
- Dependency cost: see the graph above.

**Entity file:** `crates/temps-entities/src/analytics_ingest_keys.rs`
(register in `crates/temps-entities/src/lib.rs`).

| Column | Type | Null | Default | Notes |
| --- | --- | --- | --- | --- |
| `id` | `integer` PK, autoincrement | no | | |
| `project_id` | `integer` | no | | FK → `projects(id)` `ON DELETE CASCADE` |
| `environment_id` | `integer` | **yes** | `NULL` | FK → `environments(id)` **`ON DELETE CASCADE`** (see below) |
| `name` | `varchar(128)` | no | `'Default ingest key'` | operator-facing label |
| `public_key` | `varchar(80)` | no | | `pa_` + 64 hex chars = 67 chars. **UNIQUE** |
| `is_active` | `boolean` | no | `true` | |
| `revoked_at` | `timestamptz` | yes | `NULL` | set alongside `is_active = false` |
| `rate_limit_per_minute` | `integer` | yes | `600` | `NULL`/`<= 0` ⇒ unlimited, matching `IngestRateLimiter::check` |
| `allowed_origins` | `jsonb` | yes | `NULL` | `["https://app.example.com"]`. `NULL`/`[]` ⇒ any origin |
| `event_count` | `bigint` | no | `0` | |
| `last_used_at` | `timestamptz` | yes | `NULL` | |
| `created_by_user_id` | `integer` | yes | `NULL` | FK → `users(id)` `ON DELETE SET NULL` |
| `created_at` | `timestamptz` | no | `NOW()` | |
| `updated_at` | `timestamptz` | no | `NOW()` | maintained by `ActiveModelBehavior::before_save`, copy `project_dsns.rs:70-89` |

Indexes:
- `idx_analytics_ingest_keys_public_key` — **UNIQUE** on `public_key` (the hot
  path lookup).
- `idx_analytics_ingest_keys_project_active` on `(project_id, is_active)`.

**Deliberate omissions:**

- **No `deployment_id` column.** The entire point of this ADR is the
  no-deployment case, and a deployment-scoped key would have to be re-minted on
  every deploy — the opposite of "a stable value baked into someone else's
  build". `deployment_id` for a stored event is *derived* at resolution time
  from `environments.current_deployment_id` when the key is environment-scoped
  (see §3), which gives correct deployment attribution for free if the
  environment later becomes Temps-deployed.
- **No `secret_key` column.** `project_dsns.secret_key` is a dead,
  always-empty compatibility field. Do not carry it forward.

**`environment_id` FK is `ON DELETE CASCADE`, not `SET NULL`.** `SET NULL`
would silently *widen* a key's scope from "environment X" to "the whole
project" the moment someone deletes that environment — a privilege expansion
triggered by a delete. `CASCADE` fails closed: the key dies with its
environment. (This differs from `fk_project_dsns_environment`, which is
`SET NULL`. That is a latent bug in DSNs, out of scope here, but worth an
issue.)

### 2. Service location — `temps-analytics`

The service lives in the umbrella crate so all three ingest crates can reach
it with a downward-only dependency:

- **New:** `crates/temps-analytics/src/ingest_keys/mod.rs`
- **New:** `crates/temps-analytics/src/ingest_keys/service.rs` —
  `AnalyticsIngestKeyService`
- **New:** `crates/temps-analytics/src/ingest_keys/types.rs` —
  `AnalyticsIngestKey` (admin DTO), `ResolvedIngestScope`,
  `AnalyticsIngestKeyError` (this crate owns its error enum; `From<...> for
  Problem` in the handler module)
- **New:** `crates/temps-analytics/src/ingest_keys/rate_limiter.rs` —
  `AnalyticsIngestRateLimiter` (port of
  `crates/temps-error-tracking/src/sentry/rate_limiter.rs`, keyed by `key_id`)
- **New:** `crates/temps-analytics/src/ingest_keys/handlers.rs` — admin CRUD
  (§4)
- **Modified:** `crates/temps-analytics/src/lib.rs` — `pub mod ingest_keys;`
  and re-exports
- **Modified:** `crates/temps-analytics/src/plugin.rs` — register
  `AnalyticsIngestKeyService` in `register_services`; merge the admin router
  in `configure_routes`
- **Modified Cargo.toml:** add `temps-analytics = { path = "../temps-analytics" }`
  to `crates/temps-analytics-performance/Cargo.toml` and
  `crates/temps-analytics-session-replay/Cargo.toml`. `temps-analytics-events`
  already has it. Add `temps-config` to `crates/temps-analytics/Cargo.toml`
  (for `get_external_url_or_default()` when rendering the ingest URL). No
  cycles: `temps-config` deps are `temps-auth`, `temps-entities`,
  `temps-database`, `temps-core`.

Service surface:

```
AnalyticsIngestKeyService::new(db: Arc<DatabaseConnection>) -> Self

// ingest path (cached, 5s TTL, moka; keyed by the raw key string)
async fn resolve(&self, key: &str) -> Result<ResolvedIngestScope, AnalyticsIngestKeyError>

// admin path
async fn create(&self, project_id, environment_id, name, allowed_origins, rate_limit, created_by) -> ...
async fn list(&self, project_id) -> Vec<AnalyticsIngestKey>
async fn rotate(&self, project_id, key_id) -> AnalyticsIngestKey
async fn revoke(&self, project_id, key_id) -> ()
async fn update(&self, project_id, key_id, patch) -> AnalyticsIngestKey
```

```rust
pub struct ResolvedIngestScope {
    pub key_id: i32,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    /// Derived from `environments.current_deployment_id` when the key is
    /// environment-scoped and that environment currently has a Temps
    /// deployment. `None` in the no-deployment case this ADR exists for.
    pub deployment_id: Option<i32>,
    pub allowed_origins: Option<Vec<String>>,
    pub rate_limit_per_minute: Option<i32>,
}
```

`resolve` is a single query joining `analytics_ingest_keys` to `environments`
(left) filtered on `public_key = $1 AND is_active = true`, plus
`environments.deleted_at IS NULL` (per the project's soft-delete rule).
Cache TTL 5s, capacity 10_000, mirroring `crates/temps-otel/src/ingest/auth.rs:49-50`.
Negative results are cached too (as `Option::None`).

**Correction (post-implementation, security review).** An earlier draft of this
section claimed negative caching means "an invalid key can't be used as a
DB-load amplifier". That overclaims. The cache is keyed by the literal key
string, so it only absorbs a *repeated* invalid value — one typo'd key baked
into a deployed bundle costs one query rather than one per pageview. It does
nothing against *distinct* invalid values: each miss is a real Postgres index
lookup, and 10 000 distinct misses (the cache capacity) also evict every
legitimately cached positive resolution. The implemented mitigation is a
syntactic gate ahead of both the cache and the query — a candidate must be
exactly `pa_` plus 64 lowercase hex characters, the exact minted shape, rather
than the looser "starts with `pa_` and is at most 80 characters" originally
written. That raises the cost of producing a plausible-looking key but does not
rate-limit distinct forged keys, which a bot can still generate at will.
Per-client-IP rate limiting on unresolved-key attempts, applied *before* the
lookup, is the actual fix; it is a follow-up and is **not implemented**.

`last_used_at` / `event_count` are updated on a throttled path (at most once
per 60s per key), copying the `LastUsedTracker` idea from
`crates/temps-otel/src/ingest/auth.rs:130-170`. Never on the synchronous
request path.

### 3. Auth precedence and request contract

#### Key material

Format: **`pa_` + 64 lowercase hex chars** (32 random bytes via
`rand::rng()`, hex-encoded — identical entropy to `DSNService::generate_key(32)`).

`pa_` = "public analytics". It is deliberately distinct from `tk_` (API key),
`dt_` (deployment token) and `si_` (service ingest token), all of which are
**secrets**. `pa_` signals "public, non-secret, write-only, analytics" to a
human reader and to any credential scanner.

#### Transport

Primary: **`X-Temps-Analytics-Key: pa_<64hex>`**

Fallback: **`?temps_key=pa_<64hex>`** query parameter, accepted on all five
endpoints.

Explicitly **not** `Authorization: Bearer` and **not** `X-Temps-Api-Key`:

- `X-Temps-Api-Key` is the OTel ingest header and today carries `tk_` admin
  API keys (`crates/temps-otel/src/handlers/ingest_handler.rs:157`). Sharing
  the header name with a value that gets pasted into a public JS bundle
  actively invites an operator to paste a `tk_` key there instead — and it
  would *work*, granting far more than analytics-write.
- `Authorization` is already inspected by `AuthMiddleware`
  (`crates/temps-auth/src/temps_middleware.rs:118-160`). A `pa_` token falls
  into the `else => None` branch today, but relying on that accident is
  fragile, and it offers zero benefit: any custom header on a cross-origin
  POST triggers a CORS preflight regardless.

The query-param fallback is **not optional**. `navigator.sendBeacon` — used by
the browser SDK for `page_leave`/unload events and by the late-metrics
`/speed/update` call — cannot set custom headers. Without it, exactly the
unload-path events that matter most are silently lost for the users this ADR
serves. This mirrors Sentry's own `?sentry_key=` param
(`crates/temps-error-tracking/src/sentry/handlers.rs:630`).

Extraction precedence: header, then query param. Never the request body (a
credential in a JSON body cannot be handled before body buffering and cannot
be used by `sendBeacon`'s `Blob` form without content-type games).

#### Resolution precedence — identical for all five endpoints

```
1. key := header X-Temps-Analytics-Key
        ?? query param temps_key

2. if key is present:
   2a. scope := ingest_key_service.resolve(key)
       on miss / inactive / malformed  ->  401  (RFC 7807 Problem,
                                                 title "Invalid analytics
                                                 ingest key")
       *** never fall through to the Host path ***
   2b. if scope.allowed_origins is non-empty:
           require request Origin header, exact match (scheme + host + port,
           host compared case-insensitively) against one entry
           no Origin, or no match  ->  403
       if allowed_origins is NULL or []:  any origin permitted
   2c. rate limit on (scope.key_id) with scope.rate_limit_per_minute
           over limit  ->  429
   2d. (project_id, environment_id, deployment_id) := scope.*
       *** Host is NOT consulted for resolution ***

3. else (no key present):  unchanged behaviour
   3a. host := metadata.host  (empty -> 400, as today)
   3b. route := route_table.get_route(&host)
           None  ->  404, as today
   3c. route.project is None  ->  204 (events, speed) / 404 Problem
                                       (session replay), as today
   3d. (project_id, environment_id, deployment_id) := route.*
```

**Two rules that are load-bearing and must not be "helpfully" softened later:**

- **A valid key makes `Host` irrelevant for resolution.** Requiring the Host to
  *also* resolve would defeat the entire purpose — in the target scenario there
  is no route-table entry at all. The Host header is still read and stored as a
  data field (`events.hostname`, `performance_metrics.host`) so self-referral
  detection and channel attribution keep working
  (`events_service.rs:1516-1521`), but it can never contradict the key about
  who owns the data.
- **An invalid key is a 401, not a fallback to Host.** Falling back would turn
  a typo'd key into silently mis-attributed or silently dropped data. A
  self-hosted user debugging alone must get a loud, specific error.

`allowed_origins` is a CORS-shaped *convenience* control (block a copy-pasted
key from being used on some other site by a casual attacker) — it is **not**
authentication. `curl` ignores `Origin`. See
[Security Considerations](#security-considerations-for-security-auditor) #3.

#### CORS — new, and required

The five public analytics ingest routes currently carry **no `CorsLayer` at
all** (the only `CorsLayer` in the workspace outside `temps-core`'s helper is
`crates/temps-error-tracking/src/sentry/handlers.rs:89`). That is fine today
because every ingest request is same-origin through the Temps proxy. Under
this ADR the request is cross-origin by definition, so without CORS the
browser blocks it before it leaves the page — the feature does not work at all.

Add to each of `configure_public_routes()` in
`crates/temps-analytics-events/src/handlers/events_handler.rs:1256`,
`crates/temps-analytics-performance/src/handlers/handler.rs:243`, and
`crates/temps-analytics-session-replay/src/handlers/handler.rs:1019`:

```rust
CorsLayer::new()
    .allow_origin(Any)
    .allow_methods([Method::POST, Method::OPTIONS])
    .allow_headers([header::CONTENT_TYPE,
                    HeaderName::from_static("x-temps-analytics-key")])
    .max_age(Duration::from_secs(600))
// allow_credentials stays FALSE (and is incompatible with allow_origin(Any))
```

Per-key `allowed_origins` cannot be enforced at the layer (it is per-request
DB state), so it is enforced in the handler as a 403. This is exactly the
shape Sentry ingest already uses (`allow_origin(Any)` + in-handler checks).

#### Relationship to the visitor-id fallback

`allow_credentials: false` means the browser will not send the
`_temps_visitor_id` / `_temps_sid` cookies cross-origin, and Temps never
served the HTML so it never issued them anyway. The client-generated
visitor/session-id fallback in `2b990321c`
(`resolve_client_identity`, present in all three handlers) is therefore a hard
prerequisite: this ADR makes the request *resolve*, that commit makes the
resolved event *have an identity*. Neither is sufficient alone.

#### Field caps and rate limiting — decision record

Every field this ADR makes client-supplied (as opposed to Host-resolved or
cookie-issued) gets an explicit bound, because "trust it, it's just
analytics" is how an ingest endpoint that must stay public becomes a DoS or
storage-exhaustion vector. Recorded here so a future change to any of these
numbers is a deliberate revision of a decision, not an accidental drift:

| What | Bound | Why | Where |
| --- | --- | --- | --- |
| `visitorId` / `sessionId` (client-generated fallback) | 8–64 chars, `[A-Za-z0-9_-]` | Wide enough for `crypto.randomUUID()` (36) and the SDK's non-crypto fallback shape (~30); these become unauthenticated `GROUP BY`/join keys once stored, so HTML/oversized/arbitrary-byte junk is rejected rather than persisted | `is_valid_client_identity`, `ingest_keys/request.rs` |
| `domain` / `hostname` (event payload) | ≤253 chars, `[A-Za-z0-9.\-:\[\]]` | Not full DNS validation (`localhost`, single-label intranet hosts, and IP literals are all legitimate) — only rejects input that could not plausibly be `window.location.hostname`, since every dashboard/report reads this column back out | `is_plausible_hostname`, `temps-analytics-events/events_service.rs` |
| Per-key request rate | default 600/min, ceiling 100k/min, `NULL`/non-positive = unlimited (operator opt-in) | Bounded by minted-key count (`key_id`), not by IP/visitor, which are unbounded | `AnalyticsIngestRateLimiter::check`, `analytics_ingest_keys.rate_limit_per_minute` |
| Unresolved-key attempts (key doesn't resolve at all) | 300/min, **global**, not per-IP | `resolve()`'s cache only helps on an exact repeated string, so a bot cycling through fresh valid-shaped (`pa_` + 64 hex) garbage would otherwise cost one DB query per request with no limit at all. A single global bucket bounds this without an IP-trust decision (see the rate limiter's per-`(key_id, ip)` note below) | `AnalyticsIngestRateLimiter::unresolved_budget_exhausted` |
| New `visitor` rows created per project | 120/min | Before this ADR, `visitor_id` only ever came from a server-issued cookie, so row creation was bounded by real traffic. On the keyed path it's a client-supplied string, so a request *within* its key's own rate-limit budget can still mint one new row per request forever by varying it. This caps that growth independently of, and in addition to, the request-rate limit | `MAX_NEW_VISITORS_PER_PROJECT_PER_MINUTE`, `temps-analytics-events/events_service.rs` |
| `allowed_origins` | ≤50 entries, ≤253 chars each | A single row must not be growable into an unbounded JSON blob every hot-path resolve has to parse | `MAX_ALLOWED_ORIGINS`, `MAX_ALLOWED_ORIGIN_LEN`, `ingest_keys/types.rs` |

**Known residual gap, accepted rather than closed here:** the per-key rate
limiter is keyed by `key_id`, so one abusive client sharing a key with
legitimate visitors burns the whole key's budget for everyone. A
per-`(key_id, ip)` sub-limit is the natural follow-up; it needs its own
`Origin`/IP-trust decision for cross-origin deployments (self-reported
`X-Forwarded-For` is not authoritative), so it is deliberately not bundled
into this ADR.

**Known residual gap, out of scope for this ADR:** `request_sessions` has no
`project_id` column at all, and `session_id` is globally unique — a
pre-existing design that relies on the id's cryptographic randomness for
tenant isolation rather than a schema-enforced boundary. On the keyed path
`session_id` is client-supplied, which narrows that reliance from
"unauthenticated but unguessable" to "unauthenticated but unguessable, and
now also unauthenticated end to end" for whichever project's key presents a
colliding value first. Closing this needs a schema change
(`request_sessions.project_id` + a composite unique index), tracked as a
follow-up rather than fixed in this pass.

### 4. Deployment context in the no-deployment case

`deployment_id` becomes `None` whenever the key is project-scoped, or is
environment-scoped to an environment with no `current_deployment_id`. Audit of
whether each writer tolerates that:

| Table / writer | Status | Detail |
| --- | --- | --- |
| `events` | **OK, no change** | `crates/temps-entities/src/events.rs:17-18` — both `Option<i32>`. Schema nullable with `ON DELETE SET NULL` (`m20250101_000001_initial_schema.rs`). `record_event(..)` already takes `Option<i32>` for both (`events_service.rs:1461-1462`). The events handler already produces `None` (`events_handler.rs:766-767`). |
| `performance_metrics` | **BROKEN — migration required** | `crates/temps-entities/src/performance_metrics.rs:14-15` — `environment_id: i32`, `deployment_id: i32`. Schema `NOT NULL` (`m20250101_000001_initial_schema.rs:1116-1124`) with FK `CASCADE` to `environments`/`deployments` (`:1203-1226`). Both handlers already hard-drop with 204 when either is absent (`handler.rs:565-577`, `:722-737`). |
| `session_replay_sessions` | **BROKEN, and already latently buggy** | `crates/temps-entities/src/session_replay_sessions.rs:16-17` — `i32`/`i32`. Schema `NOT NULL` (`m20250101_000001_initial_schema.rs:827-836`) with FK `CASCADE` (`:903-919`). `initialize_session` already accepts `Option<i32>` and writes `Set(environment_id.unwrap_or(0))` / `Set(deployment_id.unwrap_or(0))` (`crates/temps-analytics-session-replay/src/services/service.rs:415-416`). **There is no `environments.id = 0` or `deployments.id = 0`, so those inserts FK-violate today** — any Host that resolves to a project without a deployment already 500s on `/api/_temps/session-replay/init`. The migration below fixes that pre-existing bug as a side effect. |
| `session_replay_events`, `session_replay_ingest_batches` | OK | No env/deployment columns. |
| `visitor` | OK, but see note | `crates/temps-entities/src/visitor.rs:14` — `environment_id: i32` `NOT NULL`, but with **no FK** (there is no `fk_visitor_*` anywhere in `temps-migrations`). That is why the equivalent `environment_id.unwrap_or(0)` sentinel at `events_service.rs:1617` does not blow up. |

Explicit anti-pattern warning for the implementer: **do not "fix" session
replay by dropping the FKs so the `0` sentinel works.** That trades a loud
error for permanently unattributable rows that no join will ever match. Make
the columns nullable instead and replace `unwrap_or(0)` with the real `Option`.

`visitor.environment_id` should eventually become nullable too, so "no
environment" stops being encoded as a magic `0`. That requires touching
`TrackingEvent.environment_id: i32` in `temps-proxy` and is **out of scope for
this ADR** — file it as a follow-up issue. To keep the sentinel rare in the
meantime, the CLI and Console should nudge operators toward
**environment-scoped** keys (see §5).

### 5. Admin CRUD surface

New routes in `crates/temps-analytics/src/ingest_keys/handlers.rs`, mounted on
the admin router (so served under `/api`):

| Method | Path | Permission | Notes |
| --- | --- | --- | --- |
| `POST` | `/api/projects/{project_id}/analytics/ingest-keys` | `AnalyticsWrite` | mint; 201 |
| `GET` | `/api/projects/{project_id}/analytics/ingest-keys` | `AnalyticsRead` | list (includes revoked) |
| `PATCH` | `/api/projects/{project_id}/analytics/ingest-keys/{key_id}` | `AnalyticsWrite` | name / `allowed_origins` / `rate_limit_per_minute` |
| `POST` | `/api/projects/{project_id}/analytics/ingest-keys/{key_id}/rotate` | `AnalyticsWrite` | new `public_key`, same row/scope |
| `POST` | `/api/projects/{project_id}/analytics/ingest-keys/{key_id}/revoke` | `AnalyticsWrite` | soft: `is_active=false`, `revoked_at=NOW()` |

Every handler pairs `permission_guard!(auth, ...)` with
`project_access_guard!(auth, project_id, state.project_access_checker)` —
exactly the shape at `dsn_handlers.rs:157-167`.

**No new `Permission` variant.** `Permission::AnalyticsRead` /
`Permission::AnalyticsWrite` already exist
(`crates/temps-auth/src/permissions.rs:35-36`, strings `analytics:read` /
`analytics:write` at `:276-277`) and this mirrors DSN CRUD's reuse of
`ErrorTrackingRead`/`ErrorTrackingWrite`. Noted for the security review:
`AnalyticsWrite` today gates data mutation (e.g. `enrich_visitor`), and
minting a long-lived ingest credential is heavier; the mitigation is mandatory
audit logging rather than a new permission (see below).

**No hard `DELETE`.** Revocation must not destroy the record of which key
ingested what.

**Audit logging is mandatory** on create / rotate / revoke / update:
`ANALYTICS_INGEST_KEY_CREATED`, `_ROTATED`, `_REVOKED`, `_UPDATED`, using the
`AuditOperation` + `state.audit_service.create_audit_log(&audit)` pattern
(e.g. `crates/temps-deployments/src/handlers/remote_deployments.rs:459-470`).
Note: `DSNAppState` holds an `audit_service`
(`dsn_handlers.rs:41-46`) but **never calls it** — DSN mint/rotate/revoke is
currently unaudited. Do not copy that.

**No capability endpoint.** Per the project's "unconfigured features must
onboard" rule a capability endpoint exists to distinguish "not built" from
"not set up" when a feature depends on *operator configuration*. This feature
has no such prerequisite — there is nothing to configure, you just mint a key.
`GET .../ingest-keys` returning `[]` is a sufficient and honest empty state.
Do not add a `configured: false` endpoint here.

#### CLI parity (required — one command per endpoint)

Per `CLAUDE.md`, every new backend endpoint needs parity in
`apps/temps-cli` (`@temps-sdk/cli`), never in the Rust binary. Add a `keys`
subgroup to the existing `analytics` command
(`apps/temps-cli/src/commands/analytics/index.ts`), in a new file
`apps/temps-cli/src/commands/analytics/keys.ts`:

`-p, --project <project>` (slug or ID, resolved via the CLI-wide
`requireProjectSlug`/`getProjectBySlug` chain — same flag every other
multi-project command uses) rather than a raw `--project-id`, so this
command reads a `.temps/config.json`/`TEMPS_PROJECT`/context default like
the rest of the CLI instead of forcing every invocation to pass a numeric id:

| CLI command | Endpoint |
| --- | --- |
| `temps analytics keys list [-p <project>] [--json]` | `GET .../ingest-keys` |
| `temps analytics keys create [-p <project>] [--name <n>] [--environment-id <id>] [--allowed-origins <origin...>] [--rate-limit <n>] [--json] [-y]` | `POST .../ingest-keys` |
| `temps analytics keys update [-p <project>] --key-id <id> [--name <n>] [--allowed-origins <o...>] [--clear-origins] [--rate-limit <n>] [--clear-rate-limit]` | `PATCH .../ingest-keys/{key_id}` |
| `temps analytics keys rotate [-p <project>] --key-id <id> [-f\|-y]` | `POST .../ingest-keys/{key_id}/rotate` |
| `temps analytics keys revoke [-p <project>] --key-id <id> [-f\|-y]` | `POST .../ingest-keys/{key_id}/revoke` |

Structure, flags, spinner/table/prompt helpers: copy
`apps/temps-cli/src/commands/dsn/index.ts` (`registerDsnCommands`, `:57-111`).
Generated SDK operations consumed from `apps/temps-cli/src/api/sdk.gen.ts`:
`listAnalyticsIngestKeys`, `createAnalyticsIngestKey`,
`updateAnalyticsIngestKey`, `rotateAnalyticsIngestKey`,
`revokeAnalyticsIngestKey`.

Two known codegen traps that apply directly here:
- Every `#[utoipa::path]` must declare `params(("project_id" = i32, Path, ...),
  ("key_id" = i32, Path, ...))` or the generated TS path type comes out as
  `never` and the CLI won't compile.
- `operationId`s must be globally unique across the merged OpenAPI doc —
  `create_key`/`list_keys` will collide with existing operations; use the
  fully-qualified names above.

#### Console surface

`web/src/components/project/ProjectAnalytics.tsx` — add an "Ingest key" panel
to the setup tab, immediately above the existing framework snippets
(`:1990+`), because that is where a user asking "how do I send events" already
is. Requirements:

- Show the key **in the clear** with a copy button. Do not mask it; masking
  implies secrecy and sends the operator hunting for a "reveal" that should not
  exist.
- Empty state renders a "Create ingest key" button and one sentence explaining
  when you need one ("your app is not deployed by Temps"). Never render
  nothing.
- The framework snippets gain an `ingestKey` / `apiHost` variant for the
  cross-origin case, alongside the existing same-origin
  `basePath="/api/_temps"` variant.
- The `skills/add-react-analytics` skill needs the same second variant.

### 6. Migration plan

**New file:**
`crates/temps-migrations/src/migration/m20260831_000001_create_analytics_ingest_keys.rs`

**Registered in** `crates/temps-migrations/src/migration/mod.rs`: add
`mod m20260831_000001_create_analytics_ingest_keys;` after the existing
`mod m20260829_000001_allow_duplicate_ready_snapshot_digests;` (`:210`) and
`Box::new(m20260831_000001_create_analytics_ingest_keys::Migration),` after
`:452`.

`up()` does three things — they ship as one migration because shipping either
half alone leaves the feature broken:

1. `CREATE TABLE analytics_ingest_keys` per §1, using the
   `Table::create()` + `#[derive(DeriveIden)] enum` style of
   `m20260819_000001_create_session_replay_ingest_batches.rs` (the most recent
   create-table migration), plus the two indexes.
2. Drop `NOT NULL` on four columns, via
   `manager.get_connection().execute_unprepared(...)` — sea-orm's
   `modify_column` does not reliably express nullability changes on Postgres.
   Copy the exact style of
   `m20260828_000001_alarms_nullable_project.rs`:
   ```sql
   ALTER TABLE performance_metrics      ALTER COLUMN environment_id DROP NOT NULL;
   ALTER TABLE performance_metrics      ALTER COLUMN deployment_id  DROP NOT NULL;
   ALTER TABLE session_replay_sessions  ALTER COLUMN environment_id DROP NOT NULL;
   ALTER TABLE session_replay_sessions  ALTER COLUMN deployment_id  DROP NOT NULL;
   ```
   The existing FKs (`fk_performance_metrics_environment_id`,
   `fk_performance_metrics_deployment_id`,
   `fk_session_replay_sessions_environment_id`,
   `fk_session_replay_sessions_deployment_id`) stay as-is — Postgres FKs
   already permit `NULL`, so no drop/recreate is needed.
3. Normalize any pre-existing `0` sentinels (defensive; the FKs mean such rows
   cannot exist today, but this documents intent and covers an instance where a
   constraint was ever dropped):
   ```sql
   UPDATE session_replay_sessions SET environment_id = NULL WHERE environment_id = 0;
   UPDATE session_replay_sessions SET deployment_id  = NULL WHERE deployment_id  = 0;
   ```

`down()`: `DROP TABLE IF EXISTS analytics_ingest_keys` and **leave the four
columns nullable**, with a comment explaining why — restoring `NOT NULL` would
fail against rows that legitimately hold `NULL` after this feature has been
used, and deleting those rows to satisfy the constraint would silently destroy
a user's analytics history. This is a deliberate, documented asymmetry.

**Entity changes** in the same PR:
- `crates/temps-entities/src/performance_metrics.rs:14-15` → `Option<i32>`
- `crates/temps-entities/src/session_replay_sessions.rs:16-17` → `Option<i32>`
- `crates/temps-entities/src/analytics_ingest_keys.rs` (new)

**Downstream code changes** forced by those entity changes:
- `RecordPerformanceMetricsConfig.environment_id` / `.deployment_id` →
  `Option<i32>` (`crates/temps-analytics-performance/src/services/service.rs`)
- Delete the `let Some(environment) = ... else { 204 }` and
  `let Some(deployment) = ... else { 204 }` early returns in
  `record_speed_metrics` (`handler.rs:565-577`) and `update_speed_metrics`
  (`handler.rs:722-737`). This is a bug fix in its own right: a Temps-deployed
  environment momentarily without a deployment currently drops web-vitals
  silently.
- Replace `Set(environment_id.unwrap_or(0))` / `Set(deployment_id.unwrap_or(0))`
  with `Set(environment_id)` / `Set(deployment_id)` at
  `crates/temps-analytics-session-replay/src/services/service.rs:415-416`.
- Read-side queries that `GROUP BY`/filter on `environment_id` in
  `performance_metrics` and `session_replay_sessions` must be re-checked for
  `NULL` handling (sea-orm will now decode into `Option<i32>`; a raw-SQL
  `try_get::<i32>` on those columns would start erroring).

## Consequences

### Positive

- Temps becomes usable as a pure self-hosted observability backend for
  analytics, replay, and performance — closing the last gap versus error
  tracking, and matching PostHog/Plausible's "drop in a project key" model.
- Zero behavioural change for existing Temps-deployed projects: the Host path
  is untouched and is still the default when no key is sent.
- Fixes a live FK-violation bug in `/api/_temps/session-replay/init` for
  project-without-deployment routes.
- Fixes silent web-vitals loss on `/api/_temps/speed` when a deployment is
  momentarily absent.
- Adds the missing CORS layer, which is a prerequisite for any cross-origin
  analytics story (including future first-party SDKs on other hosts).
- Deployment attribution still works automatically when the key's environment
  *does* have a deployment — operators don't lose fidelity by adopting keys.

### Negative

- A second resolution path in five handlers. Mitigated by extracting one shared
  helper in `temps-analytics` (`resolve_ingest_scope(headers, query, host,
  route_table, key_service)`) rather than five copies.
- Two more crates depend on `temps-analytics`.
- Two write tables gain nullable scope columns; every read query touching
  `performance_metrics.environment_id` or
  `session_replay_sessions.environment_id` must handle `NULL`.
- Analytics ingest becomes reachable from any origin on the internet with a
  key that is, by design, public.
- The migration's `down()` is not a true inverse.

### Risks

- **Data poisoning.** Anyone who can read the customer's page can read the key
  and forge events. This is the accepted trade of every public-key analytics
  product; the mitigation is rate limiting + the fact that analytics is not a
  trust boundary. It must be documented as such, not glossed over.
- **`visitor.environment_id = 0` sentinel spread.** Project-scoped keys will
  create more `0`-environment visitor rows. Mitigated by defaulting to
  environment-scoped keys in the CLI/Console; properly fixed by the follow-up
  issue.
- **Read-side `NULL` regressions.** The nullability change is the highest-risk
  part of the migration and needs deliberate test coverage on the analytics
  dashboards, not just on ingest.
- **CORS `allow_origin(Any)` on write endpoints** — see security §10.

## Alternatives Considered

### Option A: Generalize `project_dsns` and reuse `DSNService`

- Pros: no new table, no new service, DSN admin UI/CLI already exists, one
  credential for errors + analytics is arguably simpler for the user.
- Cons: `public_key` is globally unique across the table, so one value would
  authenticate both Sentry ingest and analytics ingest with no way to scope it
  down — a cross-product confused deputy. Revocation conflates two products.
  Permission naming conflates (`ErrorTrackingWrite` vs `AnalyticsWrite`).
  `DSNService` lives in `temps-error-tracking`, which would drag
  `temps-embeddings`/`temps-notifications`/`temps-monitoring`/`temps-projects`
  into all three analytics ingest crates and invert the layering.
  **Rejected.**

### Option B: Reuse the OTel `IngestAuth` path (`tk_`/`dt_`/`si_` bearer tokens)

- Pros: no new table at all; `IngestAuth::authenticate_any` already resolves
  project/environment/deployment from a token.
- Cons: all three token types are **secrets**. This credential ships in a
  public browser bundle. `dt_` tokens are also per-deployment, which is exactly
  the coupling this ADR exists to break. Would require either accepting a
  secret in client JS or inventing a fourth prefix inside `api_keys` — at which
  point it is a new table with extra steps and a much worse blast radius if the
  dispatch is ever wrong. **Rejected.**

### Option C: Register a "virtual" route-table entry per external domain

Let operators register `app.example.com` against a project, and inject it into
the route table without a deployment.

- Pros: no new credential; all five handlers unchanged; reuses
  `project_custom_domains`.
- Cons: `Host` is fully attacker-controlled on this endpoint, so registration
  by domain alone is not authentication — anyone can send `Host:
  app.example.com`. It also requires domain ownership proof to be safe (which
  Temps has, but which is a heavy prerequisite for "I just want analytics"),
  breaks for apps on shared/ephemeral hostnames (`*.vercel.app` previews), and
  bloats the proxy's hot-path route table with entries the proxy will never
  route. **Rejected**, though it remains a reasonable *additional* convenience
  later.

### Option D: Require the key AND a matching Host

- Pros: strictly more restrictive.
- Cons: there is no route-table entry to match against in the target scenario,
  so this is equivalent to not shipping the feature. Recorded explicitly so it
  is not re-proposed during review. **Rejected.**

### Option E: 401 on invalid key vs. silent fall-through to Host

Chose 401. A silent fall-through turns a typo'd key into either mis-attributed
data (if the Host happens to resolve) or a confusing 404, and a self-hosted
user has no one to ask. Recorded as a decision, not an accident.

## Security Considerations for `security-auditor`

This design **requires** an adversarial review before implementation. Specific
things to attack:

1. **The key is public by construction — is every affordance honest about
   that?** It ships in a client JS bundle and in URLs. Therefore: the column is
   `public_key`, never `secret_key`; it is **not** hashed at rest (it must be
   displayable); it is **not** masked in `list` output; there is no "reveal"
   affordance. Confirm no doc string, OpenAPI description, CLI label, or
   Console string creates a false expectation of confidentiality — and
   conversely, that `pa_` is visually distinct enough from `tk_`/`dt_`/`si_`
   (which *are* secrets and *are* hashed) that an operator can't confuse them.

2. **Verify the negative: a `pa_` value must match nothing else.** Confirm no
   code path lets a `pa_` value authenticate against `api_keys`,
   `deployment_tokens`, `project_dsns`, `AuthMiddleware`,
   `IngestAuth::authenticate_any`, or any read endpoint. The separate-table
   decision is what makes this structural rather than a code invariant —
   confirm the implementation didn't quietly reintroduce a shared lookup.

3. **The real threat is data poisoning and attribution abuse, not exfiltration.**
   Anyone who reads the page can read the key and POST arbitrary events from
   anywhere. `allowed_origins` is browser-enforced only; `curl` ignores
   `Origin`. This matches the accepted model for Sentry DSNs and PostHog
   project keys. Please confirm the blast radius stays inside analytics:
   ingest must not be able to create `users`, must not write anything consumed
   for authorization, and must not land unescaped strings in a dashboard.
   **Note `record_event` has real side effects beyond appending a row** — it
   upserts `visitor` (`events_service.rs:1607-1650`) and `request_sessions`
   (`:1660+`). Those are unbounded row-creation primitives reachable by anyone
   holding the key. Assess whether they need their own cap.

4. **Rate limiting with no deployment to hang quotas off.** The OTel
   per-deployment quota machinery does not apply. Proposal:
   `AnalyticsIngestRateLimiter` keyed by **`key_id`** with
   `rate_limit_per_minute` from the row (default 600/min), fail-open on
   `NULL`/`<=0`, bounded cardinality = number of active keys (safe for a
   `Mutex<HashMap>`, same argument as
   `crates/temps-error-tracking/src/sentry/rate_limiter.rs:5-11`). **Open
   question for the auditor:** key-id-only means one abusive client burns the
   whole project's budget for every legitimate visitor. Should there be a
   second per-`(key_id, IP)` sub-limit using `temps_auth::resolve_client_ip`?
   Note that `resolve_client_ip` only trusts XFF when the direct peer is
   loopback, which in a cross-origin deployment may or may not hold.

5. **Unbounded-cardinality GROUP BY dimensions are now reachable with no Host
   allowlist whatsoever.** `events.language` is already validated precisely
   because ingest is unauthenticated (`events_handler.rs:817-833`). With a key
   the attacker is *authenticated but still anonymous*, so nothing improves.
   **I did not verify** whether `utm_source`/`utm_medium`/`utm_campaign`/
   `utm_term`/`utm_content`, `page_title`, `pathname`, `channel`, `props` and
   `custom_properties` have equivalent length/charset caps. Please check —
   this is an OOM/disk-exhaustion vector on a 4 GB box.

6. **`?temps_key=` lands in logs.** `proxy_logs` stores query strings
   (`request_query`). Acceptable because the value is public by design, but
   confirm the key never appears in an error message echoed to a third party,
   and that nothing elsewhere treats "it's in the query string, so it's
   redacted" as an invariant.

7. **Cross-tenant via attacker-controlled `Host`.** Under the new precedence
   the key alone decides the project, so a forged Host cannot cross tenants.
   But `events.hostname` still comes from `Host` and feeds channel attribution
   — an attacker can make forged events look like self-referrals. Believed low
   severity; confirm.

8. **`environment_id` FK is `ON DELETE CASCADE`, not `SET NULL`.** Chosen so
   deleting an environment revokes its keys rather than silently promoting them
   to project scope (a privilege expansion on delete). Confirm this is the
   right fail-closed choice, and that "delete an environment to kill a
   project's analytics ingest" is not a meaningful DoS (it requires
   environment-delete permission, which is strictly higher).

9. **Revocation latency.** The 5s resolution cache means a revoked key keeps
   working for up to 5 seconds. Matches `AUTH_CACHE_TTL`
   (`crates/temps-otel/src/ingest/auth.rs:49`). Stated rather than left
   implicit; confirm acceptable.

10. **`CorsLayer::allow_origin(Any)` on write endpoints.** Required for the
    feature to function and precedented by Sentry ingest
    (`crates/temps-error-tracking/src/sentry/handlers.rs:89`), but it means any
    page on the internet can POST here. Combined with #3, confirm the only
    reachable effects are analytics row appends (plus the visitor/session
    upserts flagged in #3). Confirm `allow_credentials` is and stays `false`.

11. **`AnalyticsWrite` gates credential minting.** Today that permission gates
    data mutation (`enrich_visitor`). Minting a long-lived ingest credential is
    heavier. Chosen mitigation is mandatory audit logging rather than a new
    permission variant (matching DSN CRUD's reuse of `ErrorTrackingWrite`).
    Confirm that's the right call, or require an `AnalyticsKeysManage`
    permission / `SensitiveAction` step-up instead.

## Implementation Notes

**Affected crates**

| Crate | Change |
| --- | --- |
| `temps-entities` | new `analytics_ingest_keys.rs`; `performance_metrics.rs:14-15` and `session_replay_sessions.rs:16-17` → `Option<i32>` |
| `temps-migrations` | new `m20260831_000001_create_analytics_ingest_keys.rs` + `mod.rs` registration |
| `temps-analytics` | new `ingest_keys/` module (service, types, rate limiter, admin handlers); plugin + lib wiring; `+temps-config` dep |
| `temps-analytics-events` | key extraction + precedence in `record_event_metrics`; CORS on `configure_public_routes` |
| `temps-analytics-performance` | key extraction + precedence in `record_speed_metrics` and `update_speed_metrics`; drop the env/deployment early returns; `RecordPerformanceMetricsConfig` → `Option<i32>`; CORS; `+temps-analytics` dep |
| `temps-analytics-session-replay` | key extraction + precedence in `init_session_replay` and `add_session_replay_events`; drop `unwrap_or(0)` at `service.rs:415-416`; CORS; `+temps-analytics` dep |
| `apps/temps-cli` | new `commands/analytics/keys.ts`, 5 commands; SDK regen |
| `web` | `ProjectAnalytics.tsx` ingest-key panel + cross-origin snippet variant; SDK regen |
| `skills/add-react-analytics` | cross-origin variant |

**Migration needed:** yes — one migration, three parts (create table; drop
`NOT NULL` on 4 columns; normalize `0` sentinels). `down()` is intentionally
not a full inverse.

**Breaking changes:** no API breaking changes. Internal Rust signature changes
(`RecordPerformanceMetricsConfig`, two entity models) are workspace-internal.
Read-side queries against `performance_metrics.environment_id` and
`session_replay_sessions.environment_id` must be audited for `NULL` handling —
that is the main regression surface.

**Suggested sequencing:**
1. Migration + entity nullability + `unwrap_or(0)` removal + handler early-return
   removal. Ships standalone; fixes the session-replay FK bug and the
   web-vitals drop on its own.
2. `analytics_ingest_keys` table + `AnalyticsIngestKeyService` + admin CRUD +
   audit + CLI.
3. Ingest-path key precedence + CORS in the three ingest crates.
4. Console panel + skill + docs.
