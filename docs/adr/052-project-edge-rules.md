# ADR-052: Project Edge Rules — the Engine for the Request-Level Firewall

**Status:** Proposed (design only; nothing implemented)
**Date:** 2026-10-31
**Author:** David Viejo
**Issue:** #1685 (Project level ingress & routing rules)
**Builds on:** #1007 (`RequestPolicyGate` extension hook), ADR-017 (split proxy/console processes), ADR-020 (multi-node hardening: worker public ingress)
**Related roadmap item:** request-level firewall

---

## Summary

Issue #1685 asks for one ordered list of rules per project, evaluated by the proxy
before a request reaches any container. Each rule is a match expression plus an
action:

- **Terminal actions:** `block`, `allow`, `challenge`, `redirect`.
- **Non-terminal actions:** for example "override cache".

The first matching terminal action decides the request. If no terminal action
matches, the request goes to the service.

The roadmap already includes a **request-level firewall**. A firewall is the same
thing restricted to access actions: block, allow, challenge, log. Building them as
two features would give operators two ordered lists, evaluated at two points in
the pipeline, with two sets of semantics to reason about when a request is
unexpectedly blocked.

**Decision (proposed):** build **one** engine. Edge rules are the general
mechanism. The request-level firewall is its first shipped slice: the access
actions plus a log-only mode. Routing actions (redirect, cache, headers) ship
later on the same rule list, data model, evaluator and UI.

---

## Context

### What exists today

None of these is a rules engine, but each is a piece the design should reuse.
Line numbers are as of `39d5ddb54`.

| Capability | Where | Scope | How the proxy sees it |
|---|---|---|---|
| Instance IP block/allow | `ip_access_control` table; `crates/temps-proxy/src/service/ip_access_control_service.rs` | Instance | In-memory snapshot, 30s refresh plus refresh-on-write. Checked in `early_request_filter` (`crates/temps-proxy/src/proxy.rs:4734`) |
| Request policy hook | `crates/temps-core/src/request_policy_gate.rs` (#1007) | Project/env (plugin-defined) | Write-once `ArcSwap` slot with a single provider. Sync, no I/O. Returns `Continue / Allow / Deny / Unavailable`. OSS ships allow-all |
| Legacy project IP gate | `crates/temps-core/src/project_ip_gate.rs` | Project/env | Same contract; consulted when the policy gate returns `Continue` |
| Rate limit + blacklist | `crates/temps-proxy/src/rate_limiter.rs`; `SecurityConfig.rate_limiting` | Instance / project / env | Per-IP DashMap counters, at `proxy.rs:5811` |
| Attack mode (PoW challenge) | `projects.attack_mode`, `environments.attack_mode`; `challenge_sessions` table; `crates/temps-proxy/src/handler/captcha.rs` | Project / env | All-or-nothing challenge at `proxy.rs:5885`. Clearance is a DB row keyed by JA4, read through a moka cache |
| Password wall | `SecurityConfig.password_protection`; `handler/password_wall.rs` | Env | HMAC-signed cookie, `proxy.rs:6008` |
| Domain redirect | `project_custom_domains.redirect_to/status_code` | Domain | Carried in `RouteInfo.redirect_to` (`crates/temps-routes/src/route_table.rs:521`) |
| Geo / bot data | `temps-geo`; `crawler_detector.rs`, `ai_agent_detector.rs` | — | Used only by the log batch writer, never on the decision path |
| Unused placeholders | `SecurityConfig.geo_restrictions`, `challenge_config.protected_paths`, `attack_mode: Option<String>` (`crates/temps-entities/src/deployment_config.rs:72-80`) | — | No code reads them |

Every one of these is a hard-coded special case with its own storage, its own
position in `request_filter`, its own UI page and its own log status. Each new
access feature (geo block, path block, bot block) has so far meant another
special case. Edge rules replace that pattern going forward.

### Why not just implement edge rules as a `RequestPolicyGate` provider

The hook from #1007 is the obvious seam, and it is the wrong one for this:

- **It is single-provider and write-once.** Shipping OSS edge rules through it
  would take the only slot a plugin could use. The slot exists so a plugin can
  own project access policy, and it should keep that role.
- **Its decision type is access-only.** `Continue / Allow / Deny / Unavailable`
  cannot express `challenge`, `redirect`, or "set a header and continue".
  Widening it would make every plugin handle actions it does not implement.
- **Its context is deliberately minimal.** It carries path, method, host, IDs
  and client IP. Edge rules need headers, query, country/ASN, JA4 and bot class.

The design therefore keeps `RequestPolicyGate` exactly as it is and runs edge
rules **immediately after it** (see [Placement](#placement-in-the-request-pipeline)).
A plugin policy stays authoritative: a plugin `Deny` is final, and edge rules
never see that request.

---

## Decision

### 1. Data model

New table `project_edge_rules`. Migration `m2026MMDD_NNNNNN_create_project_edge_rules`.

| Column | Type | Notes |
|---|---|---|
| `id` | `serial` PK | |
| `project_id` | `int` FK → `projects`, `ON DELETE CASCADE` | |
| `environment_id` | `int NULL` FK → `environments` | `NULL` means all environments. A shorthand for an `environment` condition that is also indexable |
| `position` | `int` | Evaluation order, unique per project (deferrable unique constraint so reorders are atomic) |
| `name` | `text` | Operator label, shown in logs and UI |
| `enabled` | `bool` | |
| `mode` | `text` (`enforce` \| `log_only`) | `log_only` evaluates and records the match but never acts. Required for safe firewall rollout |
| `expression` | `jsonb` | Typed match AST (see §2), validated on write |
| `action` | `jsonb` | Typed action (see §3), validated on write |
| `revision` | `bigint` | Bumped on every write. Used for optimistic concurrency and stamped into logs/clearance |
| `created_at`, `updated_at` | `timestamptz` | |

A row trigger calls `pg_notify('edge_rules_changed', project_id::text)`. The proxy
reloads **only that project's** compiled program. This is O(changes), as the
Scalability rules require. A 60s idle reconcile protects against missed
notifications, matching `project_change_listener.rs`.

**Limits** (validated on write, enforced again at compile time):

- 100 rules per project.
- 32 conditions per rule.
- Expression nesting depth 8.
- 32 values per `in` list (CIDR lists: 1,000, compiled to a prefix trie).
- Regex source up to 256 bytes, compiled with the `regex` crate (linear time,
  no backtracking), with `size_limit` set.

### 2. Match expressions

The expression is a **structured, typed AST stored as JSON**. It is not a
free-form expression string. The API and UI both build it directly, it validates
with serde, and invalid rules are rejected at write time with a precise error.
A text syntax can come later as an alternative serialization of the same AST
(see [Alternatives](#alternatives-considered)).

```jsonc
{ "all": [
    { "field": "path", "op": "starts_with", "value": "/wp-" },
    { "not": { "field": "country", "op": "in", "value": ["ES", "PT"] } }
] }
```

**Combinators:** `all`, `any`, `not`.

**Fields.** Each field has a fixed set of valid operators. All of them are
available in-process with no I/O:

| Field | Source on the request path | Operators |
|---|---|---|
| `ip` | `ctx.ip_address` (already parsed as `parsed_ip`) | `in_cidr`, `eq` |
| `country`, `continent`, `is_eu` | `GeoIpService` (in-memory MaxMind reader, looked up **only if the program references a geo field**) | `eq`, `in` |
| `asn`, `is_hosting_provider` | same | `eq`, `in` |
| `host` | `ctx.host` | `eq`, `in`, `ends_with`, `matches` |
| `path` | **normalized** path (see below) | `eq`, `starts_with`, `ends_with`, `contains`, `matches` |
| `method` | request header | `eq`, `in` |
| `query.<name>` | parsed lazily | `exists`, `eq`, `contains`, `matches` |
| `header.<name>` | borrowed from `RequestHeader` | `exists`, `eq`, `contains`, `matches` |
| `user_agent` | `ctx.user_agent` | `contains`, `matches` |
| `ja4` | `ctx.tls_fingerprint` | `eq`, `in` |
| `bot_class` | `crawler_detector` / `ai_agent_detector`, computed lazily | `in` (`search_engine`, `ai_crawler`, `ai_agent`, `unknown_bot`, `human`) |
| `environment` | resolved route | `in` |

**Path normalization is part of the contract.** `RequestPolicyGate` gives
providers the raw path and leaves interpretation to them. Edge rules are
configured by operators, not plugin authors, so the engine owns this
responsibility:

1. The engine matches against a canonical path: percent-decoded unreserved
   characters, collapsed `//`, resolved `.`/`..`, and `\` treated as a literal.
   This prevents `/%61dmin`, `//admin` and `/x/../admin` from bypassing a block
   on `/admin`.
2. If the raw path is **not** already canonical, path conditions in `allow`
   rules **do not match**. Ambiguity therefore fails closed: it can cause an
   extra block, never an extra allow. The upstream still receives the
   original path, unchanged from today.

### 3. Actions

| Action | Terminal | Parameters | Behaviour |
|---|---|---|---|
| `block` | yes | `status` (403 default; 400–499 allowed), optional short body | Generic response with `Cache-Control: no-store` and `X-Request-ID`, matching the existing policy-denied response shape (`proxy.rs:5787`) |
| `allow` | yes | — | Stops edge-rule evaluation and forwards. Does **not** bypass the password wall, rate limiting or attack mode (see [Open questions](#open-questions)) |
| `challenge` | yes, until cleared | `difficulty` (bounded), `clearance_ttl` (≤ 24h) | Serves the PoW page. With valid clearance, the rule is treated as not matching and evaluation continues. **Requires the clearance redesign in §6** |
| `redirect` | yes | `target` (absolute URL or path, `{path}`/`{query}` placeholders), `status` ∈ {301, 302, 307, 308} | Target validated on write. Placeholders are substituted from the normalized, re-encoded path, so the result is never an open redirect to a host the operator didn't type |
| `set_request_header` / `set_response_header` | no | name, value (static) | Hop-by-hop and `temps-*` internal headers are rejected on write |
| `cache_override` | no | `cache_control` value, or `bypass` | See [Caching](#caching-caveat): the main proxy has no response cache today |
| `log` | no | — | Records a match and continues. This is what `mode: log_only` turns any action into |

### 4. Evaluation semantics

These are the semantics proposed in the issue, made precise:

1. Rules for the resolved `project_id` are evaluated in `position` order. A rule
   is skipped if it is disabled or its `environment_id` does not match.
2. The first matching rule with a terminal action decides the outcome:
   - Under `log_only`, the match is recorded and evaluation continues.
   - A `challenge` whose clearance is valid for this request counts as not
     matching, and evaluation continues.
3. A matching non-terminal action is accumulated and evaluation continues.
   Header and cache overrides are applied in order; a later rule overwrites the
   same header.
4. If no terminal action matches, the request continues down the existing
   pipeline: rate limit, attack mode, password wall, and so on.
5. **Accumulated non-terminal actions are discarded if a later terminal action
   blocks, challenges or redirects.** They apply only to requests that are
   forwarded.

### 5. Placement in the request pipeline

```
early_request_filter
  ├─ client IP / UA / JA4
  └─ instance IP block (ip_access_control)                  ← instance firewall today
request_filter
  ├─ preview gateway, wake-on-request
  ├─ host → project / environment / deployment resolution   (proxy.rs:5628)
  ├─ connection cap
  ├─ RequestPolicyGate + legacy ProjectIpGate                (proxy.rs:5711)  ← plugin policy, final on Deny
  ├─ ► EDGE RULES ◄                                          (new, ≈ proxy.rs:5810)
  ├─ rate limit / blacklist                                  (proxy.rs:5811)
  ├─ attack mode                                             (proxy.rs:5885)
  ├─ password wall, ACME, HTTPS redirect, domain redirect, …
  └─ upstream
```

This slot was chosen because the project and environment are known, the
client IP is already parsed, and nothing has spent budget yet. A blocked
caller never consumes rate-limit tokens, the same reasoning #1288 used for the
IP gate.

**Mandatory exemptions.** These are evaluated before rules and cannot be
overridden by a rule, otherwise an operator could lock out their own TLS or
challenge flow:

- `/.well-known/acme-challenge/*`. ACME is handled *after* this slot
  (`proxy.rs:6205`), so a broad `block` rule would silently stop certificate
  renewal.
- `/api/_temps/captcha/*` and `/api/__temps/temps_captcha_wasm*`, which attack
  mode already exempts (`proxy.rs:5890`).
- `/_temps/password-verify`.

The UI states these exemptions next to the rule list.

### 6. Prerequisite: challenge clearance must be unforgeable

> **This is a security defect in the existing attack-mode challenge. It should
> be fixed in its own PR before any rule can use `challenge`.**

`POST /_temps/captcha/verify` (`crates/temps-proxy/src/handler/captcha.rs:74`)
trusts every field the client sends:

- **`challenge`:** the proxy generates a random challenge into the page
  (`proxy.rs:1767`) but never stores or signs it. Verify only checks that
  `sha256(challenge + nonce)` has 20 leading zero bits, so one solved pair can
  be replayed indefinitely.
- **`environment_id` and `identifier`:** both are taken from the request body.
  Any client can mark any JA4 as cleared, for any environment, for 24 hours.

Clearance is also keyed by **JA4**, which identifies a TLS client stack, not a
visitor. Every visitor on the same browser build shares one clearance, and so
does a bot impersonating that build.

**Required design**, mirroring the password wall's HMAC cookie
(`handler/password_wall.rs`):

1. **Issue:** the challenge page embeds a server-signed token:
   `HMAC(key, env_id ‖ rule_id ‖ rule_revision ‖ client_binding ‖ issued_at ‖ random)`.
   `client_binding` is a hash of IP prefix + JA4.
2. **Verify:** check the signature and the age (e.g. ≤ 5 minutes), recompute
   `client_binding` from the *connection*, not the body, then verify the PoW
   over the token.
3. **Clearance:** a signed, `HttpOnly`, `Secure`, `SameSite=Lax` cookie scoped
   to the environment, carrying `{env_id, rule_id, rule_revision,
   client_binding, expires_at}`. Checking it is a pure in-memory HMAC
   verification, so no DB row or moka lookup is needed on the hot path, and
   `challenge_sessions` can be retired.
4. **Revocation:** editing a challenge rule bumps `revision`, which invalidates
   its clearances.

### 7. Hot-path budget

Edge rules sit on every proxied request, so the
[Scalability](../../CLAUDE.md#scalability--efficiency) hot-path rules apply in
full:

- **Compiled program.** Rows are compiled at load time into an immutable
  `EdgeProgram` per project: regexes built, CIDRs in a prefix trie, `in` lists
  in small sorted arrays or hash sets, and flags recording which lazy inputs
  (geo, query, bot class) any rule needs. Programs live in an
  `ArcSwap<HashMap<i32, Arc<EdgeProgram>>>`. A request does one map lookup and
  one `Arc` clone, with no locks.
- **No allocation on the common path.** Header, host and UA values are borrowed.
  Path normalization writes into a stack buffer and allocates only for paths
  over 1 KiB. Projects with no rules — the common case — return immediately
  after the map lookup.
- **Lazy inputs.** GeoIP, query parsing and bot classification run at most once
  per request, and only when the project's program references them.
- **Counters, not writes.** Per-rule match counts are `AtomicU64`s in the
  compiled program, flushed to Postgres every 60s by a background task. There
  are no per-request DB or network calls.
- **Logging.** The decision sets `ctx.routing_status` (`edge_rule_blocked`,
  `edge_rule_challenged`, `edge_rule_redirected`, `edge_rule_logged`) and a new
  `ctx.edge_rule_id`. Both travel in the existing batched `proxy_logs` path. One
  nullable `edge_rule_id` column is added to the Timescale table and to the
  ClickHouse schema (`crates/temps-proxy/migrations/clickhouse/`).
- **Memory bound.** 100 rules × bounded conditions × the number of projects. At
  roughly 2 KiB per compiled rule (dominated by regex DFAs at the capped
  `size_limit`), 1,000 projects with full rule sets come to about 200 MiB as an
  upper bound. Realistically a few hundred KiB. Saturation is not a factor:
  evaluation is CPU-only and bounded by the limits in §1.

### 8. Multi-node: worker public ingress

Worker ingress receives route snapshots (`crates/temps-routes/src/route_sync.rs`)
and already forces traffic through the control plane when it cannot enforce a
policy (`policy_requires_control_plane`, `route_sync.rs:499-515`).

- **Phase 1:** any project with an enabled rule sets
  `policy_requires_control_plane`. This fails closed: rules are always enforced,
  at the cost of the worker shortcut for those projects.
- **Later:** ship compiled-ready rule rows in the snapshot, so workers enforce
  them locally with the same evaluator crate. The evaluator lives in its own
  dependency-light crate (`temps-edge-rules`) so the worker binary and
  `temps-edge` can link it without pulling in the proxy.

### 9. API, permissions, audit

All endpoints go under `/projects/{project_id}/edge-rules`, with typed DTOs and
RFC 7807 errors:

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/` | Ordered list, with per-rule match counts and last-matched time. The 100-rule cap bounds it, so it returns the full ordered list rather than pages; ordering is the point |
| `POST` | `/` | Create, at `position` or appended |
| `PATCH` | `/{rule_id}` | Update. Requires `revision`; a mismatch returns 409 |
| `DELETE` | `/{rule_id}` | |
| `PUT` | `/order` | Atomic reorder: the full ID list plus the expected revisions |
| `POST` | `/evaluate` | **Dry run.** Takes a sample request (method, URL, headers, IP) and returns which rules match and the final decision, without sending traffic. This is the main debugging and onboarding tool |

- **Permissions:** `EdgeRulesRead` and `EdgeRulesWrite`, project-scoped under
  ADR-028.
- **Audit:** every write (create, update, delete, reorder) logs the full
  before/after expression and action. Rules can lock users out, so the audit
  trail is the recovery path.
- **Clients:** `web/src/api/client/` and `apps/temps-cli/` (`temps edge-rules
  list|add|rm|move|test`) are both regenerated.

### 10. UI and discoverability

- **Location:** Project → Settings → Security → **Edge rules**, next to IP
  restrictions, rate limiting and attack mode, where operators already go to
  protect a project.
- **Empty state:** never hidden. Shows ready-made examples that create a
  `log_only` rule in one click:
  - "Block WordPress probes: path starts with `/wp-`."
  - "Challenge traffic from hosting-provider networks."
  - "Redirect `/docs/*` to `https://docs.example.com/{path}`."
- **Rule list:** drag-to-reorder, an enable toggle, an enforce/log-only toggle,
  and match counts with sparklines, which show whether a `log_only` rule is safe
  to enforce.
- **Rule builder:** a structured condition editor (field → operator → value)
  that maps one-to-one onto the AST in §2. There is no free-text expression in
  Phase 1.
- **Test panel:** backed by `POST /evaluate`.
- **Proxy log view:** gains an "Edge rule" column and filter, linking from a
  blocked request to the rule that blocked it.
- **GeoIP onboarding:** if the GeoIP database is not configured, country and ASN
  conditions remain selectable but show an onboarding state: "GeoIP database
  not installed — country conditions will never match", linking to the setting
  that installs it.

### 11. Relationship to the existing special cases

These are **not** migrated in Phase 1. Behaviour and data stay unchanged, and
each special case remains in its current pipeline position. The UI shows them as
read-only "built-in" rows at the top or bottom of the edge-rule list, so the
operator sees the full effective order in one place.

- **Attack mode** is a `challenge` rule with an always-true expression. Once §6
  lands, it can be re-expressed as one.
- **`SecurityConfig.geo_restrictions` and `challenge_config.protected_paths`**
  are dead placeholders. They are deleted when the country field and the
  `challenge` action ship, rather than given a second implementation.
- **Instance `ip_access_control`** stays as the instance-level firewall. An
  instance-wide rule list using this same engine and table shape
  (`instance_edge_rules`, evaluated in `early_request_filter`) is the natural
  follow-up if the roadmap firewall is instance-scoped (see Open questions).

---

## Phasing

| Phase | Scope | Ships |
|---|---|---|
| **0** | Fix the challenge forgery (§6) on today's attack mode | Security fix, independent of edge rules |
| **1 — request firewall** | Table and migration; evaluator crate; fields `ip`, `country`/`asn`/`is_hosting_provider`, `host`, `path`, `method`, `header`, `user_agent`, `ja4`, `bot_class`, `environment`; actions `block`, `allow`, `log`; `log_only` mode; counters; `proxy_logs.edge_rule_id`; API with `/evaluate`; UI; workers fall back to the control plane | The roadmap request-level firewall |
| **2** | `challenge` (on the §6 clearance) and `redirect`; `query.<name>` field | Covers the full action set the issue asks for, except cache |
| **3** | `set_*_header`, `cache_override` (see caveat), a rule-scoped rate-limit action, instance-level rules, worker-local enforcement | Unified ingress controller |

### Caching caveat

The issue's "override cache" action assumes the proxy caches responses. The
main `temps-proxy` does not: it only sets `Cache-Control` on static deployments
(`proxy.rs:2912`). Only the separate `temps-edge` CDN node has a cache. In Phase
3, `cache_override` will therefore mean:

- In `temps-proxy`: setting or overriding the response `Cache-Control`, which
  downstream CDNs and `temps-edge` honour.
- In `temps-edge`: a real bypass or TTL override.

This needs stating plainly in the UI so operators don't expect a cache that
doesn't exist.

---

## Alternatives considered

1. **Ship edge rules as a `RequestPolicyGate` provider.** Rejected; see
   [Context](#why-not-just-implement-edge-rules-as-a-requestpolicygate-provider).
   It would occupy the plugin slot and cannot express non-access actions.
2. **Separate "firewall" and "edge rules" features.** Rejected. Two ordered
   lists at two pipeline positions make "why was this request blocked?" a
   two-place investigation, and the evaluator, storage, UI and logging would be
   built twice.
3. **A free-form expression language** (CEL, or a wirefilter-style
   `http.request.uri.path contains "/wp-"`). Deferred, not rejected.
   - Costs: a parser on the write path, harder validation errors, and a UI that
     must round-trip text and structure.
   - Path forward: the structured AST in §2 is exactly what such a parser would
     compile into, so a text syntax can be added later as a second input format
     with no storage or evaluator change.
4. **Store rules inside `deployment_config.security` JSON.** Rejected. It allows
   no per-rule identity for logs and counters, no atomic reorder, and no
   per-row audit. `deployment_config` changes also trigger a full route-table
   reload rather than a single-project recompile.
5. **Per-environment rule lists only.** Rejected as the primary scope; the issue
   asks for one list per project. The nullable `environment_id` column plus the
   `environment` field cover per-environment needs.

---

## Open questions

1. **Scope of the roadmap firewall.** Is it project-level (this ADR, Phase 1),
   instance-level, or both? If both, instance rules run first in
   `early_request_filter` with the same engine, and an instance `block` is
   final.
2. **Plugin versus OSS ownership.** #1007 added `RequestPolicyGate` so a provider
   could own project access policy. If the planned request firewall is meant to
   be that provider, this ADR's Phase 1 overlaps with it. The recommendation is
   to keep the firewall engine in OSS and leave the gate for policies that need
   external state (identity-aware access, central policy). The decision
   affects the crate layout.
3. **What `allow` bypasses.** This draft says `allow` only ends rule evaluation.
   The common request "allow my office IP through attack mode and rate limits"
   needs an explicit `skip: ["rate_limit", "attack_mode"]` option on `allow`.
   Should that ship in Phase 1, or wait for those special cases to become rules?
4. **Response status for `block`.** Fixed 403, or operator-chosen (e.g. 404 to
   hide an admin path)? This draft allows 400–499.
