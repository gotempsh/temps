# ADR-089: Project Edge Rules — the Engine for the Request-Level Firewall

**Status:** Accepted — phases 1 and 2 implemented together (see [Implementation notes](#implementation-notes))
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

A row trigger stamps the project in `project_edge_rule_versions` with the next
value of a sequence. The proxy polls the sequence head every 5 s and, when it
moved, reloads **only the projects whose version changed**. This is
O(changes), as the Scalability rules require, and unlike `NOTIFY` it cannot
miss a change across a reconnect. A full reload every 15 minutes is the
backstop.

**Limits** (validated on write, enforced again at compile time):

- 100 rules per project.
- 32 conditions per rule.
- Expression nesting depth 4 (Railway's limit; deeper trees are unreadable in a
  builder anyway).
- 64 values per `in` list (CIDR lists: 1,000, compiled to a prefix trie).
- 256 bytes per condition value.
- 8 `*` wildcards per glob pattern.
- Regex source up to 256 bytes, compiled with the `regex` crate (linear time,
  no backtracking), with `size_limit` set (256 KiB compiled, and the same for
  its lazy DFA cache).
- Per project, 32 `regex` conditions and 256 *pattern* conditions: every
  condition that compiles to an automaton (`regex`, `glob`, and
  `starts_with`/`ends_with`/`contains` ignoring case). Those built from a
  literal compile to at most about 64 KiB, so one project's patterns stay
  under about 22 MiB in the worst case. The same budget applies to the
  drafts sent to `/evaluate`, before any of them is compiled.

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
| `host` | `ctx.host` (lowercase, port stripped) | `eq`, `in`, `ends_with`, `glob` |
| `path` | **normalized** path (see [Path matching](#path-matching)) | `eq`, `in`, `starts_with`, `ends_with`, `contains`, `glob`, `regex` |
| `raw_path` | path exactly as received, before normalization | `contains`, `glob`, `regex` |
| `method` | request header | `eq`, `in` |
| `query.<name>` | parsed lazily | `exists`, `not_exists`, `eq`, `contains`, `glob`, `regex` |
| `header.<name>` | borrowed from `RequestHeader` (see [Header matching](#header-matching)) | `exists`, `not_exists`, `eq`, `in`, `contains`, `glob`, `regex` |
| `cookie.<name>` | parsed lazily from `Cookie` | `exists`, `not_exists`, `eq` |
| `user_agent` | `ctx.user_agent` | `contains`, `glob`, `regex` |
| `ja4` | `ctx.tls_fingerprint` | `eq`, `in` |
| `bot_class` | `crawler_detector` / `ai_agent_detector`, computed lazily | `in` (`search_engine`, `ai_crawler`, `ai_agent`, `unknown_bot`, `human`) |
| `environment` | resolved route | `in` |

**Operators shared by every string field:**

- `glob` is the default pattern operator: `*` matches any run of characters,
  including `/`, and there are no other metacharacters. It is the right tool
  for almost every path rule (`/admin*`, `*.php`, `/api/*/internal*`), it
  cannot be catastrophically slow, and it reads the same to everyone. Railway
  offers only this.
- `regex` exists for the cases a glob cannot express. It uses the linear-time
  `regex` crate, is anchored only if the pattern anchors itself, and the UI
  marks it as advanced. Vercel offers regex on every field.
- `in` is an exact-match set. `neq` and `not_in` are not separate operators:
  negation is the `not` combinator, so missing-value behaviour stays
  consistent (below).

#### Path matching

`RequestPolicyGate` gives providers the raw path and leaves interpretation to
them. Edge rules are configured by operators, not plugin authors, so the engine
owns this:

1. **Normalized by default.** `path` is the canonical form: percent-decoded
   unreserved characters, collapsed `//`, resolved `.`/`..`, `\` treated as a
   literal, and the query excluded. This stops `/%61dmin`, `//admin` and
   `/x/../admin` from slipping past a block on `/admin`.
2. **Ambiguity fails closed.** If the raw path is **not** already canonical,
   any `allow` rule whose expression references `path` **does not match as a
   whole**. Ambiguity can cause an extra block, never an extra allow. The rule
   is skipped as a unit, not condition by condition. Making only the path
   condition false would be inverted by a `none`/`not` group: "allow everything
   except `/admin*`" would then allow `/%61dmin`. The upstream still receives
   the original path, unchanged from today.
3. **`raw_path` exists for blocking encoding tricks themselves.** A rule such as
   `raw_path contains "%2e%2e"` or `raw_path contains "%00"` blocks the probe
   instead of quietly normalizing it. Vercel exposes the same split
   (`path` / `raw_path`).
4. **Case: sensitive by default, with a per-condition `case_insensitive`
   flag.** URL paths are case-sensitive, and Railway matches that. But an
   upstream that treats paths case-insensitively turns a case-sensitive block
   on `/admin` into a bypass via `/ADMIN`. The flag lets the operator close
   that. On a `block`/`challenge` rule the UI suggests it whenever the pattern
   contains letters. On `allow` rules case-insensitive matching widens what is
   allowed, so the UI does not suggest it there.
5. **Trailing slash is significant.** `/admin` and `/admin/` are different
   paths. The examples and the UI's "starts with" preset use `/admin*`, which
   covers both, plus `/administrator`. The builder shows that consequence in the
   rule preview.

#### Header matching

1. **Names are case-insensitive and values case-sensitive** (HTTP semantics,
   same as Railway). Values accept the same `case_insensitive` flag as paths.
2. **A missing header never matches a value operator.** `eq`, `contains`,
   `glob` and `regex` on an absent header are false. Only `not_exists` matches
   absence.
   - `not(header.x-api-key eq "s3cret")` is **true** when the header is
     missing. This is the safe reading for a block rule ("block unless the
     correct key is present").
   - Railway documents the opposite for its `neq` operator: "a negative
     comparison against a missing value does not match". A rule like "block if
     `x-api-key` is not `s3cret`" is then bypassed by simply omitting the
     header. Dropping `neq` in favour of the `not` combinator removes that trap.
3. **Repeated headers.** For a header sent more than once, a positive operator
   matches if **any** value matches, so `not(...)` holds only if **no** value
   matches. Values are compared per header line. Comma-joined values are not
   split, because splitting is header-specific and would not match what the
   upstream sees.
4. **Spoofable headers are labelled.** `X-Forwarded-For`, `X-Real-IP`,
   `Forwarded` and `CF-Connecting-IP` are client-controlled unless the
   connection comes from a trusted proxy. The builder warns when a condition
   uses one of them and points to the `ip` field, which holds the client IP
   already resolved through Temps' trusted-proxy rules.
5. **Values are never logged.** A match records `edge_rule_id` only, never the
   header or cookie value that matched. These fields carry credentials
   (`Authorization`, session cookies).
6. **Size.** There is no per-value cutoff. Ignoring values above some length
   would let an attacker pad a header past it to evade a `contains` block.
   Cost is bounded instead by Pingora's total request-header limit, together
   with linear-time `glob`/`regex` matching.

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
- The challenge's own paths: `POST /api/_temps/captcha/verify` and the two
  solver files under `/api/__temps/`. The rest of `/api/_temps/` is evaluated:
  a project that serves its own Temps API receives it.
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
  compiled program, flushed to Postgres every 10s by a background task in one
  batched upsert. There
  are no per-request DB or network calls.
- **Logging.** The decision sets `ctx.routing_status` (`edge_rule_blocked`,
  `edge_rule_challenged`, `edge_rule_redirected`, `edge_rule_logged`) and a new
  `ctx.edge_rule_id`. Both travel in the existing batched `proxy_logs` path. One
  nullable `edge_rule_id` column is added to the Timescale table and to the
  ClickHouse schema (`crates/temps-proxy/migrations/clickhouse/`).
- **Memory bound.** 100 rules × bounded conditions × the number of projects.
  Rules without `regex` compile to roughly 2 KiB each, so 1,000 projects with
  full rule sets come to about 200 MiB. Automatons are bounded separately
  (§1): at most 32 regexes of up to 256 KiB and 256 patterns in total, about
  22 MiB per project in the worst case. A path condition like `/wp-admin`
  compiles to a few KiB; realistic rule sets are a few hundred KiB. Saturation is not a factor:
  evaluation is CPU-only and bounded by the limits in §1.

### 8. Multi-node: worker public ingress

Worker ingress receives route snapshots (`crates/temps-routes/src/route_sync.rs`)
and already forces traffic through the control plane when it cannot enforce a
policy (`policy_requires_control_plane`, `route_sync.rs:499-515`).

- **Phase 1:** an enabled rule in any live project, log-only included, adds
  "project edge rules are enabled" to `worker_route_policy`'s blocking
  policies, the way project IP access rules and rate limits already do, so
  workers serve no public, private or tailnet route and every request reaches
  the control-plane proxy. A log-only rule counts because a request a worker
  answers is never evaluated, so its counter would under-report. Every write
  that can change this (creating an enabled rule, enabling or disabling one,
  deleting an enabled one) sends `project_route_change` in its transaction, so
  workers pick it up on the next route generation instead of at the end of
  their current long poll. This fails closed, at the cost of the worker
  shortcut for the whole cluster while any rule is enabled.
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
- **Clients:** `web/src/api/client/` and `apps/temps-cli/` are both
  regenerated.

#### 9.1 CLI, MCP tools and skills

Edge rules ship with the same surfaces as the console, in the same release.
Operators script them, and agents are often asked to "block this path" or "why
was this request rejected?". Every surface calls the same service, so
validation, limits, permissions and audit logging have exactly one
implementation.

**CLI:** `temps edge-rules`.

| Command | Does |
|---|---|
| `list` | Ordered table: position, name, action, mode, enabled, match counts |
| `get <rule>` | One rule, with its expression in readable form and as JSON |
| `create` | From flags for simple rules (`--path /admin --action block --status 403`) or from `--file rule.json` for anything nested. Creates `log_only` unless `--enforce` |
| `update <rule>` | Same inputs as `create`; sends the revision it read, so a concurrent edit fails with 409 instead of being overwritten |
| `delete <rule>` | Asks for confirmation unless `--yes` |
| `move <rule> --to <n>` | Reorder |
| `enable`, `disable`, `enforce`, `log-only` `<rule>` | One-field changes |
| `test` | Runs `POST /evaluate` on a sample request (`--url`, `--method`, `--header`, `--ip`) and prints the trace: which rules matched and the final decision |
| `export`, `apply --file rules.json` | Rules as code (adjustment A4): `apply` shows the diff against the live list and applies it after confirmation. The writes are separate requests (deletes first), so the whole file is first validated through the dry run, and its environments checked, before anything is deleted |

**MCP tools:** category `edge-rules` in the `networking` group of the
ADR-039 server, next to `ip-access`.

| Tool | Kind |
|---|---|
| `list_edge_rules` | Read |
| `get_edge_rule` | Read |
| `evaluate_edge_rules` | Read: the dry run. It changes nothing, so it is available without `?write=1` and is the tool an agent uses to answer "why was this blocked?" |
| `create_edge_rule`, `update_edge_rule`, `delete_edge_rule`, `reorder_edge_rules`, `set_edge_rule_mode` | Write, propose-then-confirm |

- **Same schema as the API:** tool input schemas reuse the API's JSON
  expression AST (§2) and limits. The tool descriptions list the fields,
  operators and limits, so an agent can build a valid rule without trial and
  error.
- **Proposals describe the effect:** every write proposal includes the rule in
  plain words ("Block with 403 when path starts with /admin and client IP is
  not in 203.0.113.0/24") and the before/after for updates. The human
  confirming must be able to see that a rule could lock them out.
- **Agents never enforce in one step:** `create_edge_rule` always creates
  `log_only`. Enforcing is a separate `set_edge_rule_mode` proposal, so an
  agent-written rule is only ever enforced by a second explicit human
  confirmation.
- **Same access as the API:** tools check `EdgeRulesRead`/`EdgeRulesWrite`
  instance-wide **and on the project** (the caller's team role, like
  `project_permission_guard!`), plus deployment-token scope, both when
  proposing and when confirming. Audit entries record that the change came
  through MCP.
- **Proposals are bound to what was reviewed:** an update, mode change or
  deletion proposal stores the rule's revision at proposal time, and
  confirming refuses (revision conflict) if the rule changed while the
  proposal waited, so a human never enforces or deletes a rule they did not
  see.

**Skills:**
- `skills/temps-cli/SKILL.md` documents the `edge-rules` commands.
- `skills/temps/SKILL.md` and `skills/temps-best-practices/SKILL.md` gain
  short recipes: block scanner probes, restrict a path to an office network,
  and debug a blocked request with `test` / `evaluate_edge_rules`. Every
  recipe starts in log-only mode.

**Parity check:** a test lists the edge-rule API operations and asserts each
has a CLI command and an MCP tool, so a later endpoint cannot ship on one
surface only.

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

## Comparison with Railway edge rules and the Vercel WAF

Sources, read 2026-10-31:

- Railway edge rules: <https://docs.railway.com/networking/edge-rules> (linked
  from the issue).
- Vercel WAF:
  [custom rules](https://vercel.com/docs/vercel-firewall/vercel-waf/custom-rules),
  [rule configuration](https://vercel.com/docs/vercel-firewall/vercel-waf/rule-configuration),
  [rate limiting](https://vercel.com/docs/vercel-firewall/vercel-waf/rate-limiting),
  [firewall concepts](https://vercel.com/docs/vercel-firewall/firewall-concepts).
- Vercel's condition and operator enums are taken from its public OpenAPI spec
  (`PATCH /v1/security/firewall/config`).

### Scope and model

| | Railway | Vercel | This ADR |
|---|---|---|---|
| Scope | Per **service, per environment** | Per **project**; condition on `environment` | Per **project**; `environment_id` column + `environment` field |
| Structure | One JSON ruleset (`version: 1`, `rules[]` with `priority`, `if`, `then`) | Rules with `conditionGroup[]` and `action.mitigate`, plus separate IP rules and managed rulesets | Rows with `position`, typed `expression` and `action` |
| Change workflow | Whole ruleset validated atomically on save; live in seconds | Draft → **Review changes → Publish**; versioned configs that can be re-activated | Per-row CRUD with `revision` (see adjustment A1) |
| Config as code | JSON ruleset in the editor | `vercel.json` `routes[].mitigate` (deny/challenge only) | Not yet (see A4) |
| Limits | 64 KiB ruleset, 32 clauses/rule, depth 4, 64 per `in`, 256 chars/value, 8 wildcards | Rule count by plan (Hobby: 3 custom rules) | 100 rules, 32 clauses, depth 4, 64 per `in`, 256 B/value, 8 wildcards |

### Match fields

| Field | Railway | Vercel | This ADR |
|---|---|---|---|
| Client IP / CIDR | `ipv4.src`, **IPv4 only** (IPv6 never matches) | `ip_address` | `ip`, IPv4 and IPv6 |
| Host | `http.host` | `host` | `host` |
| Path (normalized) | `http.path`, normalized, case-sensitive | `path`, plus `target_path` and `route` (framework route pattern) | `path` |
| Raw path | — | `raw_path` | `raw_path` |
| Method | — | `method` | `method` |
| Header | `http.header` + `key` | `header` + `key` | `header.<name>` |
| Query | — | `query` + `key` | `query.<name>` (Phase 2) |
| Cookie | — | `cookie` + `key` | `cookie.<name>` |
| User agent | (via header) | `user_agent` | `user_agent` |
| Geo | — | continent, country, country region, city, AS number | country, continent, `is_eu`, ASN, `is_hosting_provider` |
| TLS fingerprint | — | `ja4_digest`, `ja3_digest` | `ja4` |
| Bots | — | `bot_name`, `bot_category` (managed bot detection) | `bot_class` (Temps' crawler/AI-agent detectors) |
| Platform-specific | — | `server_action`, `rate_limit_api_id`, `region`, `protocol`, `scheme` | `environment` |

### Operators and matching semantics

| | Railway | Vercel | This ADR |
|---|---|---|---|
| Operators | `eq`, `neq`, `in`, `not_in`, `contains`, `matches` (glob `*`) | `eq`, `neq`, `pre`, `suf`, `sub`, `inc`/`ninc`, `ex`/`nex`, `re` (regex), `gt`…`lte`, plus a `neg` flag | `eq`, `in`, `starts_with`, `ends_with`, `contains`, `glob`, `regex`, `exists`/`not_exists`; negation via `not` |
| Case | Path and header values case-sensitive, header names not | **All operators case-insensitive** | Sensitive by default, per-condition `case_insensitive` |
| Missing value + negative operator | **Does not match**, so omitting a header bypasses a "block if not X" rule | `nex` for absence; otherwise unspecified | Value operators false on missing; `not(...)` true. See [Header matching](#header-matching) |
| Combinators | `and` / `or` / `not`, nested | AND/OR chaining between conditions, condition groups | `all` / `any` / `not`, nested |

### Actions

| Action | Railway | Vercel | This ADR |
|---|---|---|---|
| Block | `block`, status 400–499, body ≤ 4 KiB | `deny`, fixed 403 | `block`, status 400–499, short body |
| Allow / skip rest | `allow`: skip later rules | `bypass`: skip later custom **and managed** rules, not system DDoS mitigation | `allow`: skip later edge rules only (open question 3) |
| Challenge | Browser verification; clearance continues evaluation | JS challenge; **1 h session bound to the browser**; then continues evaluation | PoW challenge on the §6 clearance |
| Redirect | `host` or `location`; 301/302/307/308; `preserve_path`, `preserve_query` | Redirect to a path or URL, 307 | `target` with `{path}`/`{query}`; 301/302/307/308 |
| Log only | — | `log`: a match is attributed only if no later rule blocks | `log` action + per-rule `mode: log_only` |
| Rate limit | — | `rate_limit`: fixed window (all plans) or token bucket; window 10 s–10 min (1 h Enterprise); keys IP and JA4 (UA and headers on Enterprise); follow-up 429/log/deny/challenge; **per-region counters** | Not in this draft (see A2) |
| Persistent block | — | Deny/challenge/rate-limit can ban the client IP for a chosen time; enforced before the WAF | Not in this draft (see A3) |
| Cache | `cache_override`: `ttl_seconds` (1 s–30 d) or `bypass` | — | `cache_override` (Phase 3, with the caching caveat) |
| Headers | — | — | `set_request_header`, `set_response_header` |

### What we take from each

These **adjustments** are already applied in this revision:

- **Glob as the default pattern** (Railway), with `regex` as an advanced
  operator (Vercel).
- **`raw_path`** (Vercel), so probes using encoding tricks can be blocked
  outright.
- **`cookie.<name>`** (Vercel).
- **Railway's tighter limits:** depth 4, 64 per `in` list, 256 B per value,
  8 wildcards.
- **No `neq`.** Negation goes through `not`, so a missing header cannot bypass
  a "block unless X" rule. This is the one point where Railway's documented
  semantics are unsafe.
- **Explicit case handling** in place of Vercel's always-insensitive matching.
  Insensitive matching widens `allow` rules silently.

These are **proposed and not yet applied**. Each changes scope or the data
model, so they need a decision:

- **A1. Versioned rulesets with publish and rollback.** Both competitors treat
  the ruleset as one unit: Railway validates the whole set atomically, and
  Vercel adds draft → review → publish with re-activatable versions. Rules can
  lock everyone out, so "roll back to the version from ten minutes ago" is
  the most valuable recovery tool. It also makes reorder atomic for free.
  - Proposal: replace per-row CRUD with
    `project_edge_rulesets(project_id, version, rules jsonb, status
    draft|published, published_by, published_at)`.
  - Each rule keeps a stable `rule_id` across versions for logs and counters.
  - The API becomes "get or put draft", "publish", "list versions", "activate
    version N".
- **A2. A `rate_limit` action.** Vercel's strongest feature beyond Railway, and
  the path from the previous discussion for folding today's rate limiting into
  the engine. Proposal: fixed window, keys `ip` | `ja4` | `header.<name>`,
  window 10 s–1 h, follow-up `block` (429) | `challenge` | `log`.
  - Counters live in the existing `rate_limiter.rs` store, which is per node.
    That is the same trade-off as Vercel's per-region counters, and should be
    documented the same way.
- **A3. Persistent block.** An option on `block`/`challenge`: "also ban this IP
  for N minutes". Implemented as a bounded, TTL'd in-memory set checked in
  `early_request_filter`, effectively a temporary `ip_access_control` entry
  that is never persisted. Banned requests then cost nothing beyond the
  connection, as in Vercel.
- **A4. Rules as code.** JSON export/import in the same shape as the API (and
  `temps edge-rules apply -f rules.json`), so a ruleset can be reviewed in a PR
  and copied between projects. Railway's editor is JSON-first, and Vercel
  accepts a subset in `vercel.json`.
- **A5. Log attribution rule.** Adopt Vercel's: a `log` match is reported under
  the log rule only if no later rule blocks the request. Otherwise the request
  is attributed to the blocking rule, so counters never double-count.

### Where we deliberately differ

- **IPv6 is first-class.** Railway's `ipv4.src` silently never matches IPv6
  clients, so an IPv4 allowlist plus a catch-all block denies every IPv6
  visitor.
- **No framework-route fields** (Vercel's `route`, `server_action`). Temps
  proxies arbitrary containers and does not know the application's routes.
- **Header actions** (`set_*_header`), which neither offers. They are cheap
  in our pipeline and replace one-off proxy settings.
- **The challenge is self-hosted proof-of-work**, not a third-party or
  browser-fingerprinting service. It is weaker against headless browsers than
  Vercel's, and the UI should say so.

---

## Phasing

| Phase | Scope | Ships |
|---|---|---|
| **0** | Fix the challenge forgery (§6) on today's attack mode | Security fix, independent of edge rules |
| **1 — request firewall** | Table and migration; evaluator crate; fields `ip`, `country`/`asn`/`is_hosting_provider`, `host`, `path`, `method`, `header`, `user_agent`, `ja4`, `bot_class`, `environment`; actions `block`, `allow`, `log`; `log_only` mode; counters; `proxy_logs.edge_rule_id`; API with `/evaluate`; UI, CLI, MCP tools and skill docs together (§9.1); workers fall back to the control plane | The roadmap request-level firewall |
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
5. **Which of adjustments A1–A5 from the comparison belong in Phase 1?** The
   recommendation is A1 (versioned publish and rollback) and A5 in Phase 1,
   because both shape the data model and are hard to retrofit. A2–A4 can
   follow in later phases.

---

## Implementation notes

The first release ships phases 1 and 2 together: every field, all five
actions (`block`, `allow`, `challenge`, `redirect`, `log`), both modes, the API
with `/evaluate`, the console, the CLI (`temps edge-rules`), the MCP tools
(networking group) and the skill docs. Where the code differs from the design
above:

- **Change feed.** A version table bumped by a row trigger and polled every
  5 s, rather than `pg_notify` (see §4). The cursor only advances to the newest
  version actually read, inside a `REPEATABLE READ` transaction.
- **Publishing is immediate.** A saved rule is live on the next poll. There is
  no draft state; `log_only` is how a rule is tried before it enforces.
- **Paths ignore case by default.** New conditions on `path` carry
  `case_insensitive: true`; the summary says "(ignoring case)".
- **Permissions.** Two new permissions, `edge_rules:read` and
  `edge_rules:write`. Admins and users get both, readers get read. On a
  team-gated project every team role reads them and the `admin` and `owner`
  roles write them, like the project's domains: they decide who reaches it.
- **Challenge before sign-in.** Edge rules run before the access decision, so
  on an environment that requires a Temps login the challenge comes first. The
  login redirect lets the challenge's solver files and verification POST
  through to Shield (`ingress::answered_before_login`); the visitor solves the
  challenge, then signs in.
- **Challenge** reuses Shield's proof-of-work page and clearance cookie for the
  environment. A visitor who already holds clearance passes; non-navigation
  requests get 429 instead of a page they could not render.
- **Logging.** Terminal decisions set `routing_status` to
  `edge_rule_blocked`, `edge_rule_redirected` or `edge_rule_challenged`, and
  `error_message` names the rule id, on the existing `proxy_logs` path.
  `log_only` matches are visible through the per-rule hit counters and
  `last_hit_at`. **Follow-up:** the dedicated, indexed `proxy_logs.edge_rule_id`
  column (Timescale, ClickHouse, the worker wire format) is not in this
  release.
- **Exempt paths.** `/.well-known/acme-challenge/`, the challenge's
  verification POST (`/api/_temps/captcha/verify`) and the two challenge solver
  files under `/api/__temps/` are never evaluated, so no rule can break
  certificate issuance or the challenge it serves. The rest of `/api/_temps/`
  and `/api/__temps/` is evaluated: a project that serves its own Temps API
  receives `/api/_temps/`, and on any other project it is still traffic for the
  project's hostname (analytics beacons, for instance), which a rule may block. `POST /_temps/password-verify` is exempt only when the
  environment has a password wall, which always handles it; rate limits still
  apply to it. Without a wall that path is the app's and is evaluated. The
  ACME exemption only applies
  to a plain path: one containing `%`, `\`, `;`, `//` or a `.`/`..` segment
  is evaluated like any other, so `/.well-known/acme-challenge/../admin`
  cannot use the prefix to skip a rule on `/admin`.
- **Rules default to log-only.** `POST /edge-rules` without `mode`, the CLI's
  `create`, and a rule in an `apply` file without `mode` all create
  `log_only`; enforcing is always explicit. MCP-created rules are log-only
  regardless of what the agent asks for.
- **AS numbers** are compared as numbers: `AS64496`, `as64496` and `64496`
  are the same value, and anything that is not a 32-bit number is rejected.
