# ADR-034: Feature Flags

- **Status:** Phase 1 implemented (`feat/feature-flags`); Phases 2–3 proposed
- **Date:** 2026-08-02
- **Deciders:** TBD
- **Related:** ADR-017 (split proxy/console), ADR-027 (cross-project traces), ADR-028 (project-scoped RBAC)

## Context

Temps replaces six paid SaaS tools with one binary. Feature flags are the
conspicuous gap: today the only runtime knob a Temps user has is an environment
variable, and changing one triggers a redeploy. That is correct for secrets and
static config and wrong for a kill switch — by the time the container restarts,
the incident has been running for two minutes.

Two things changed the urgency:

1. **Railway shipped native feature flags on 2026-07-10** — typed flags per
   project, targeting rules with percentage rollout, dashboard + CLI + SDK +
   MCP. That removes "no PaaS has this" from our differentiation story.
2. **We already own the two hard parts.** Every competitor's flag product has to
   solve identity (who is this user?) and low-latency delivery (how does the app
   read a flag without a network hop?). Temps already has analytics identity
   (`visitors`, `sessions`, event properties) and already terminates every
   request to every deployed app in its own Pingora proxy. Nobody else in this
   category has both.

The strategic point is not "add flags because Railway has flags". It is that
flags sitting next to analytics, session replay, and error tracking in the same
binary is a product no competitor can assemble: **flip a flag, and the same
system tells you what it did to conversion, error rate, and p95** — without
wiring a flag vendor to an analytics vendor to an error vendor.

## How other platforms do it

| Platform | Model | Identity | Delivery | Notes |
|---|---|---|---|---|
| **Vercel** (Flags SDK + Edge Config) | Flags-as-code — flags are `flag()` declarations in the repo; Edge Config is just a KV store the `decide()` function reads | None of its own; you bring the context | Server-side only, by design — Edge Config read at the edge, ~0ms | Vercel deliberately does *not* own the flag data model. Third parties (LaunchDarkly, Statsig, Hypertune, Split) sync their config into Edge Config to bootstrap their SDKs |
| **Railway** | Typed config registry per project (bool/string/number/JSON) with targeting rules | None — you pass a flat context bag incl. a `key` to hash on | TS SDK holds flags in memory, refreshes in background; reads are synchronous | Dashboard → Settings → Feature Flags; `railway flag set checkout.v2 true --when 'plan == "enterprise"'`; MCP tools at mcp.railway.com |
| **PostHog** | Flags as a feature of the analytics product | **Owns it** — flags target person properties, cohorts, and group properties already in the warehouse | Two modes: remote (`/flags` per check, ~500ms) and local evaluation (SDK polls `/api/feature_flag/local_evaluation`, 10–20ms), plus bootstrap payload to kill the client-side flash | Flag definitions contain PII, so local evaluation is server-side only |
| **Unleash** (OSS) | Toggle config polled and evaluated locally server-side | Stores no user attributes — full context on every evaluation (a privacy *feature* for regulated orgs) | Frontend SDKs are thin clients with no targeting logic; production needs a separate Unleash Edge deployment | Polling by default; SSE streaming is Enterprise-only |
| **Flagsmith** (OSS) | Django API + Postgres, optional Edge Proxy container | Stores identities and traits server-side, so partial context still evaluates correctly | Edge Proxy caches config for sub-ms local evaluation; min viable stack ≈768MB RAM | Combines flags with remote config |
| **Render / Netlify / Coolify / Dokploy** | None | — | — | Netlify's deploy previews are the closest analog to progressive delivery |

Four lessons worth stealing:

1. **Server-side evaluation is the default, not an option.** Vercel forces it
   because client-side evaluation means choosing between a spinner and flashing
   the wrong variant. PostHog's answer is the same via a different route
   (bootstrap payload). Any design that only ships a browser SDK is broken.
2. **Reads must not be network calls.** Everyone converges on: push config to
   something near the app, evaluate in-process, refresh in the background.
3. **Percentage rollout must be a stable hash of a caller-supplied key**, not a
   coin flip, or a user flickers between variants across requests.
4. **PostHog's edge is identity, and it is the only durable one.** Unleash and
   Railway make you carry the whole context on every call. PostHog does not,
   because it already knows the person. Temps is in PostHog's position, not
   Railway's — and should build accordingly.

## What Temps already has (and what it changes)

This is the part that makes the design different from a from-scratch flag service.

**`/_temps/*` is an existing public ingest namespace on the app's own domain.**
The Pingora proxy intercepts these paths before static-file serving and before
the upstream (`crates/temps-proxy/src/proxy.rs:3838`, `:4277`). The analytics
SDK already posts to `/api/_temps/event` from the app's own origin with **no API
key** (`sdks/node/packages/react-analytics`), because the proxy resolves the
tenant from the `Host` header via the route table:

```rust
// crates/temps-analytics-events/src/handlers/events_handler.rs:670
let (project_id, environment_id, deployment_id) =
    match state.route_table.get_route(&host) { ... }
```

Consequences for flags, all of them free:

- **Zero-config client SDK.** No public key, no project ID, no API host. Same as
  analytics today.
- **Ad-blocker resistant.** First-party origin, not a third-party flag CDN.
- **Environment and deployment are known without being told.** A preview
  environment automatically gets preview flag values; the flag read is already
  attributable to a specific deployment.

**The route table already has a push-based invalidation channel.** Route changes
propagate in-process plus `NOTIFY route_table_changes` for other nodes
(`crates/temps-proxy/src/on_demand.rs:893`). Flag config can ride the same
mechanism — meaning sub-second propagation with **no polling**, which is the
thing Unleash charges Enterprise money for (SSE streaming) and Flagsmith needs a
separate Edge Proxy container for.

**Identity already exists**: `visitors`, `sessions`, event `custom_properties`
(`crates/temps-entities/src/{visitor,sessions,events}.rs`). Note the App Users
primitive (decided 2026-07-12) is *not* shipped — flags should target what
exists today and be designed to absorb App Users later, not block on it.

**Precedent for the crate shape**: `temps-kv` is a self-contained plugin
(`KvPlugin` implementing `TempsPlugin`, registering services + routes +
OpenAPI). `temps-flags` should be a sibling and nothing more exotic.

## Requirements

### R1 — Data model

- **R1.1** Flag key: `[a-z0-9][a-z0-9._-]*`, unique per project, immutable after
  create (rename = new flag; keys leak into user code and analytics dimensions).
- **R1.2** Typed values: `bool`, `string`, `number`, `json`. Type is fixed at
  create. A bool-only v1 is a trap — remote config (Flagsmith, Railway) is half
  the value and retrofitting types is a migration.
- **R1.3** Every flag has a **default value** that is served when evaluation
  cannot complete for any reason. Never `null`, never "unset".
- **R1.4** **Variants**: named value buckets (`control` / `treatment`), so
  rollout percentages and analytics breakdowns key off a stable variant name
  rather than a serialized value.
- **R1.5** Scoping: flags are defined at **project** level; **values and rules
  are overridable per environment** (`environments.id`). This matches how
  `env_vars` / `env_var_environments` already work and is what makes preview
  environments useful. Inheritance is explicit: environment override, else
  project value, else default.
- **R1.6** Lifecycle metadata: `description`, `owner`, `created_at`,
  `last_evaluated_at`, `archived_at`. `last_evaluated_at` is what makes stale-flag
  cleanup possible, and stale flags are the #1 complaint about every flag product.
  **Implemented as SDK-reported per-flag exposure** — see "R1.6, decided" below.
- **R1.7** Storage: Postgres via Sea-ORM, new entities in `temps-entities`,
  migration in `temps-migrations`. **Not** in ClickHouse — flag *definitions* are
  small, transactional, and FK-related; flag *exposure events* are a different
  story (see R6).

### R2 — Targeting and rollout

- **R2.1** An ordered rule list per flag per environment. Each rule is
  `when <condition> serve <variant>`; first match wins; fall through to default.
- **R2.2** Condition operators over context attributes: `==`, `!=`, `in`,
  `not in`, `contains`, `matches` (anchored regex, size-limited), numeric
  comparisons, `exists`. Attribute values are strings/numbers/bools.
- **R2.3** **Percentage rollout with stable bucketing**: hash
  `(flag_key, salt, subject_key)` → `[0,100)`, compare against the rule's
  percentage. Salt is per-flag and regenerable ("reshuffle the cohort"). Same
  subject must always land in the same bucket for the same flag — cross-request,
  cross-node, cross-restart. This has to be a pure function with a unit test
  asserting exact bucket assignments for fixed inputs, or it will silently drift.
- **R2.4** **Implicit context.** The proxy already knows environment, deployment,
  country/region (`temps-geo`), device/browser (already parsed for analytics),
  visitor ID, and session ID. These must be available as targeting attributes
  **without the app passing them**. This is the capability Railway and Unleash
  structurally cannot offer, and it should be the headline.
- **R2.5** Explicit context: the caller may supply a flat attribute bag plus a
  `key` (the bucketing subject). Explicit attributes override implicit ones.
- **R2.6** Evaluation must be **total** — it returns a value for every input.
  Unknown attribute, malformed rule, type mismatch: log, emit a `reason`, serve
  the default. Per CLAUDE.md, no failure path may leave the caller stuck.
- **R2.7** Every evaluation returns a **reason** (`RULE_MATCH` + rule index,
  `PERCENTAGE_ROLLOUT`, `DEFAULT`, `FLAG_NOT_FOUND`, `ERROR`). Debuggability for
  self-hosters with nobody to ask.

### R3 — Evaluation surfaces

- **R3.1 Server-side, in-process (primary).** The Node/Python SDK holds the
  environment's flag set in memory and evaluates locally. Reads are
  **synchronous** and require no network I/O. Background refresh.
- **R3.2 Same-origin HTTP (`/_temps/flags`)** served by the proxy, tenant
  resolved from `Host` like `/_temps/event` is today. Two shapes:
  - `POST /api/_temps/flags` — bulk-evaluate all flags for a context (client SDK)
  - `POST /api/_temps/flags/{key}` — single flag (server-side dynamic context)
- **R3.3 Bootstrap / SSR payload.** A server-rendered app must be able to embed
  evaluated flags into the initial HTML so the browser never flashes the wrong
  variant. Non-negotiable — this is the single most-cited flag-product failure.
- **R3.4 Edge injection (differentiator, can be phase 2).** Because Temps
  terminates the request, the proxy can inject the evaluated flag set into the
  HTML response for *any* app — no SDK, no code change, works for a static site.
  Nothing else in this market can do that.
- **R3.5** Caching: `ETag` / `If-None-Match` on the flag-config fetch, per
  OFREP's `flagConfigEtag` convention.
- **R3.6** Propagation SLO: a flag change is live everywhere in **< 2 seconds**,
  via `route_table_changes`-style NOTIFY, not polling.

### R4 — Management surfaces

- **R4.1 Console UI** under the project: list, create, edit value, edit rules,
  archive. Compact list by default. Show `last_evaluated_at` and current rollout
  % inline so stale flags are visible without drilling in.
- **R4.2 CLI parity is mandatory** (CLAUDE.md): `apps/temps-cli/src/commands/flags/`
  in the `@temps-sdk/cli` TypeScript client — **not** a Rust subcommand.
  Minimum: `list`, `get`, `set`, `rules add|rm|list`, `archive`, all with
  `--environment` and `--json`.
- **R4.3 REST API** with OpenAPI, feeding the generated SDKs (`sdks/node`,
  `web/src/api`). Never hand-roll fetch in `web/`.
- **R4.4 Kill switch must be one action.** "Disable this flag everywhere,
  immediately" is a single click and a single CLI command, not a rule edit.
- **R4.5 RBAC**: flag read vs. flag write are distinct permissions
  (`FlagsRead` / `FlagsWrite` / `FlagsDelete`). **Per-environment gating is
  deliberately not in Phase 1** — see "R4.5, decided" below.
- **R4.6 Audit logging** on every mutation: who, when, old value, new value,
  environment. Reuse `temps-audit`. This is table stakes for the compliance
  buyers the SOC2 work targets.

### R5 — Security

- **R5.1** Flag *definitions* may encode business logic and PII-adjacent
  targeting (email domains, plan names, cohort definitions). **They must never be
  shipped to a browser.** Client surfaces receive *evaluated results only*. This
  is the specific reason PostHog restricts local evaluation to server-side, and
  it is the easiest thing to get wrong here.
- **R5.2** The unauthenticated `/_temps/flags` endpoint is a public attack
  surface on every customer domain. It requires: strict per-IP and per-project
  rate limiting, a hard cap on context payload size, a cap on attribute count,
  and regex evaluation guarded against catastrophic backtracking.
- **R5.3** Bulk evaluation must not become a flag-key enumeration oracle for
  flags scoped to internal/staging use. Flags need a `client_visible` boolean;
  default **false** (server-only). Opt in, not opt out.
- **R5.4** Percentage-rollout hashing must not leak the subject key. Salted hash,
  server-side.
- **R5.5** Cross-project isolation: a flag read on project A's domain can only
  ever see project A's flags. The route table already enforces this, but it needs
  an explicit test — this codebase has shipped tenant-scoping IDORs before.
- **R5.6** `security-auditor` sign-off required before merge (CLAUDE.md).

### R6 — Observability and the actual moat

This is the section that justifies building it in Temps rather than telling users
to run Unleash.

- **R6.1 Exposure events.** When a flag is evaluated for a subject, record
  `(flag_key, variant, subject, timestamp)`. High volume → ClickHouse, alongside
  analytics events. **Sampled and opt-in by default** — the `otel_spans` incident
  (160GB/day) is the precedent for why unbounded new telemetry ships off by
  default with a quota.
- **R6.2 Flag → analytics correlation.** Flag variant becomes a breakdown
  dimension on existing analytics: conversion, funnel completion, Web Vitals.
  "Variant B's LCP is 400ms worse" answered in the product that flipped the flag.
- **R6.3 Flag → error-tracking correlation.** Tag error events with active flag
  variants. "This exception only occurs for `checkout.v2 = true`" is the single
  highest-value sentence a flag product can print.
- **R6.4 Flag → session replay.** Filter replays by variant. Nobody else can do
  this without a three-vendor integration.
- **R6.5 Flag change → deploy/incident timeline.** Flag changes appear on the
  same timeline as deployments and alarms, because a flag flip is a production
  change and the on-call person needs to see it there.
- **R6.6 Automatic kill switch (later).** Error rate for a variant crosses a
  threshold → roll back to control, notify. We already have the alarm
  infrastructure.

### R7 — Compatibility

- **R7.1 Implement OFREP** (`POST /ofrep/v1/evaluate/flags[/{key}]`, OpenAPI
  0.3.0). This is cheap — it is a thin adapter over R3.2 — and it makes every
  OpenFeature SDK in every language work against Temps on day one, including
  languages we will never write an SDK for. It is also the honest answer to the
  lock-in objection: your flag calls are portable.
- **R7.2 Importers.** We already have five competitor importers
  (`temps-import-*`). An Unleash/Flagsmith/PostHog flag importer is the same
  pattern and the same acquisition motion.
- **R7.3** Flags are **not** environment variables and must not be presented as
  such. Env var change ⇒ redeploy; flag change ⇒ seconds. Documenting that
  distinction is what stops users reaching for the wrong tool.

### R8 — Non-goals (v1)

- Full experimentation/statistics (significance testing, sequential analysis).
  Record the exposure data so this stays possible; do not build the stats engine.
- Scheduled / time-based flag changes.
- Approval workflows on flag changes (Enterprise-tier candidate).
- Flags-as-code / repo-declared flags (Vercel's model). Compelling, but it
  presumes a build step we do not control for every runtime.

## R1.6, decided: `last_evaluated_at` is SDK-reported, per flag

The obvious implementation is wrong. The snapshot endpoint hands the SDK *every*
flag in the environment and evaluation then happens locally, so stamping on
snapshot fetch would mark every flag as freshly used the moment any instance
boots — including flags nothing references. The column would then actively
mislead: someone sees a recent timestamp on a dead flag and keeps it, or trusts
it on a live one and deletes it.

So the flag set is not the signal; the *reads* are. `POST /flags/exposure` takes
the keys an app actually evaluated, and the SDK accumulates them in memory and
flushes on the refresh interval — never per call, which would put a network
round trip back into the hot path this client exists to remove.

Three properties that make it safe:

- **Bounded.** The SDK only records keys present in the snapshot, so
  `get('user-' + id, …)` cannot grow the set; the server caps a batch at
  `MAX_EXPOSURE_KEYS` regardless.
- **Not an oracle.** Unknown keys are ignored silently and counted out of
  `recorded`, so the endpoint cannot be used to discover which flag keys exist.
- **Still read-only in the sense that matters.** It writes `last_evaluated_at`
  and nothing else, so "a deployment token cannot change what a flag serves"
  remains true even though this is a write path.

It lives on `feature_flags` rather than `feature_flag_environments`: the row
always exists (a flag with no override anywhere has no environment row to
stamp), and the question it answers — "is it safe to delete this flag?" — is
answered across all environments at once. Per-environment resolution arrives
with full exposure events, which carry the environment on the event itself.

The UI labels it **"Last evaluated by an app"**, not "Last evaluated", and
renders `Never` distinctly — the whole risk here is someone reading the field as
"last time anything touched this flag".

## R4.5, decided: per-environment gating is credential scoping, not resource elevation

Phase 1 ships with a known gap: **anyone holding `FlagsWrite` can flip a flag in
any environment of a project, production included.** That is accepted for now,
and this section records why, plus the shape the fix should take — so the next
person doesn't re-derive it.

The tempting fix is resource-side elevation: a column on `environments` plus a
stronger permission required when writing there. It was rejected. It invents a
second, flag-specific authorization axis that nothing else in the platform has,
and it puts the rule on the *resource* when the thing that actually varies is
the *caller*.

**The direction is capability scoping on the credential.** Deployment tokens
already demonstrate it: they carry `project_id` and `environment_id`, and
`AuthContext::is_scoped_to_project` (`crates/temps-auth/src/context.rs:333`)
already confines them. API keys carry neither — `api_keys::Model` has
`user_id`, `role_type`, `permissions` and `service_id` and nothing about scope —
so they fall into that function's `None => true` branch and are unconfined by
construction.

What that implies, concretely:

- **Project scoping is already wired for flags.** All six project-scoped flag
  handlers call `project_scope_guard!`. The moment `project_id()` learns about a
  scoped API key, flags are confined with *zero* changes in `temps-flags`.
- **Environment scoping needs one new guard.** There is no
  `environment_scope_guard!(auth, environment_id)` today. Flags need exactly one
  call site for it — `set_flag_environment` is the only handler taking an
  `environment_id`.
- **The data model change belongs to API keys, not to flags.** Scoping columns
  (or a join table, for keys spanning several projects) go on `api_keys`, and
  every guard call site across the platform benefits, not just this crate.

An alternative that exists today and was *not* taken: `project_permission_guard!`
(`crates/temps-auth/src/permission_guard.rs:276`) narrows a permission through
the ADR-028 seam's `effective_project_permissions`. Its coverage snapshot pins
`temps-deployments`, `temps-environments`, `temps-projects` — environment
*variables* are covered, and flags are their direct sibling. Adding `temps-flags`
would make flag writes respect team-based project roles when a plugin registers
a checker, and is inert in plain OSS. It is a one-line guard plus a snapshot
entry if that turns out to be wanted before credential scoping lands.

## Open questions

1. **Own crate or fold into an existing one?** `temps-flags` as a `TempsPlugin`
   sibling to `temps-kv` is the obvious read, but evaluation has to run inside
   the proxy hot path (R3.4), and the proxy deliberately minimizes DB
   dependencies. Likely split: `temps-flags` owns CRUD/API/UI, and the proxy gets
   a read-only in-memory flag snapshot fed by the same NOTIFY channel as the
   route table.
2. **OSS vs. plugin boundary.** Per the core-primitives philosophy and the
   PR #486 precedent (teams OSS, custom roles behind a plugin seam): flags +
   targeting + rollout are a core primitive and belong in OSS. Approval
   workflows, scheduled changes, and automatic kill switches are the plugin-side
   candidates. Needs an explicit call before implementation, not after.
3. **Bucketing subject when the app passes nothing.** Falling back to the
   analytics visitor ID is powerful and is the differentiator — but it silently
   ties flag assignment to a cookie the end user can clear, and to a subsystem
   with its own privacy posture. Needs a deliberate decision and clear docs.
4. **Exposure event volume.** R6.1 could plausibly out-volume analytics events
   by an order of magnitude. Sampling strategy and default quota must be decided
   before the first exposure event is written, not after the first 600GB table.

## Design

**Decision: ship the kill switch, design the rule engine.** Phase 1 delivers only
"set a value per environment, flip it without a redeploy" — no rules, no
targeting, no percentage rollout. But the shapes that are *expensive* to change
after users depend on them are fixed now.

The discipline is a single question per design element: **is this cheap or
expensive to add later?**

| Expensive later — fix in v1 | Why |
|---|---|
| Resolution order + `reason` enum | It's the wire contract. Reordering silently changes what every existing flag serves |
| SDK method signature (incl. `context`) | Adding a parameter later edits every call site in every user's codebase |
| `value_type` (typed values) | Retyping stored data *and* user code. Bool-only v1 is the trap |
| `client_visible` default `false` | Flipping a security default later exposes flags that were never meant to leave the server |
| `salt` column, populated at create | Bucketing must be stable from the first rollout. A salt introduced later reshuffles every user already in an experiment |
| `key` immutability + unique index | Keys leak into user code and (later) analytics dimensions |

| Cheap later — defer | Why |
|---|---|
| `variants` column | Additive nullable column |
| Rule *contents* | The `rules` column exists and stays `[]`; only the evaluator grows |
| SSE push propagation | ETag polling first; streaming is a transport swap behind the same endpoint |
| Exposure events | Separate table, separate pipeline, no coupling to evaluation |

### Schema

Two tables, mirroring the `env_vars` / `env_var_environments` precedent
(definition at project level, override per environment).

`feature_flags` — the definition:

| Column | Type | Notes |
|---|---|---|
| `id` | serial PK | |
| `project_id` | int NOT NULL | FK → `projects`, ON DELETE CASCADE |
| `key` | varchar(128) NOT NULL | `[a-z0-9][a-z0-9._-]*`, immutable after create |
| `value_type` | varchar(16) NOT NULL | `bool` \| `string` \| `number` \| `json` |
| `default_value` | jsonb NOT NULL | served whenever evaluation can't do better. Never null |
| `description` | varchar(512) NULL | |
| `salt` | varchar(32) NOT NULL | generated at create. **Unused in Phase 1** |
| `client_visible` | bool NOT NULL DEFAULT false | opt-in, per R5.3 |
| `archived_at` | timestamptz NULL | |
| `created_at` / `updated_at` | timestamptz NOT NULL | |

Unique index on `(project_id, key)`.

`feature_flag_environments` — the override:

| Column | Type | Notes |
|---|---|---|
| `id` | serial PK | |
| `flag_id` | int NOT NULL | FK → `feature_flags`, ON DELETE CASCADE |
| `environment_id` | int NOT NULL | FK → `environments`, ON DELETE CASCADE |
| `enabled` | bool NOT NULL DEFAULT true | `false` = kill switch (R4.4) |
| `value` | jsonb NULL | NULL = inherit `default_value` |
| `rules` | jsonb NOT NULL DEFAULT `'[]'` | **always `[]` in Phase 1** |
| `last_evaluated_at` | timestamptz NULL | debounced background write, never per-evaluation |
| `created_at` / `updated_at` | timestamptz NOT NULL | |

Unique index on `(flag_id, environment_id)`.

`rules` ships empty but present. That is the whole trick: Phase 2 writes data
into a column that already exists, and the resolution order below already has a
reserved slot for it, so no existing flag changes behaviour on upgrade.

`last_evaluated_at` is what makes stale-flag cleanup possible (R1.6), but it is a
write on a read path. Per the hot-path rules it must be an in-memory timestamp
flushed on an interval (once a minute is plenty), never a row update per
evaluation.

### Resolution order — fixed now, extended later

```
1. flag missing or archived   → caller's fallback   reason = FLAG_NOT_FOUND
2. enabled == false           → default_value        reason = DISABLED
3. ── reserved for rules ──                          reason = RULE_MATCH{i}
                                                            | PERCENTAGE_ROLLOUT{i}
4. value IS NOT NULL          → value                reason = ENVIRONMENT_VALUE
5. otherwise                  → default_value        reason = DEFAULT
```

Step 3 is a no-op in Phase 1 because `rules` is always `[]`. Two properties fall
out of pinning the order now:

- **Rules beat the environment value.** A rule is more specific than a blanket
  per-environment setting, so it must resolve first. Deciding this after users
  have flags in production would change what those flags serve.
- **The kill switch outranks everything.** `enabled = false` short-circuits
  before rules are even consulted — so in Phase 2 it still means what it means
  today: "ignore all targeting, everyone gets the default." One action, one
  meaning, forever (R4.4).

### The evaluation seam

Evaluation is a pure, synchronous, total function in `temps-flags/src/eval.rs`.
No DB, no I/O, no `async`. This is what lets the same code run in a handler, in
the SDK, and (Phase 3) inside the proxy hot path.

```rust
/// Total function: returns a value for every input. Never panics, never errors.
pub fn evaluate(flag: &FlagSnapshot, _ctx: &EvalContext) -> Evaluation {
    if !flag.enabled {
        return Evaluation::new(&flag.default_value, EvalReason::Disabled);
    }

    // Phase 2 inserts the rule loop HERE. Nothing above or below it changes.

    match &flag.environment_value {
        Some(v) => Evaluation::new(v, EvalReason::EnvironmentValue),
        None => Evaluation::new(&flag.default_value, EvalReason::Default),
    }
}
```

`EvalContext` is threaded through from day one and ignored in Phase 1 — that is
deliberate. The parameter is the expensive thing; the code that reads it is
cheap.

```rust
pub struct EvalContext {
    /// Bucketing subject. Unused in Phase 1; required for stable rollout later.
    pub key: Option<String>,
    /// Targeting attributes. Unused in Phase 1.
    pub attributes: HashMap<String, AttributeValue>,
}

pub struct Evaluation {
    pub value: FlagValue,
    pub variant: Option<String>,   // always None in Phase 1
    pub reason: EvalReason,
}

pub enum EvalReason {
    FlagNotFound,
    Disabled,
    RuleMatch { index: usize },          // Phase 2
    PercentageRollout { index: usize },  // Phase 2
    EnvironmentValue,
    Default,
    Error,                               // degraded, never thrown
}

pub enum FlagValue {
    Bool(bool),
    String(String),
    Number(f64),
    Json(serde_json::Value),
}
```

Note for review: `FlagValue::Json` holds a `serde_json::Value`, which normally
violates the "never return untyped JSON" rule. Here the arbitrary JSON *is* the
domain — the enum itself is the type, and the API response DTO wrapping it is
fully typed. Flagging it so it doesn't get bounced in review.

### Pin the bucketing algorithm in Phase 1

`bucket()` ships in Phase 1 with its tests, even though **nothing calls it**.
Once a single user is in a percentage rollout, this function can never change —
any drift silently re-randomizes live experiments. Landing it early, tested and
unused, means it is fixed before anything can depend on it.

```rust
/// Stable bucket in [0, 100). Pure; identical across nodes, restarts, versions.
pub fn bucket(flag_key: &str, salt: &str, subject: &str) -> f64 {
    let mut h = Sha256::new();
    h.update(flag_key.as_bytes());
    h.update(b":");
    h.update(salt.as_bytes());
    h.update(b":");
    h.update(subject.as_bytes());
    let d = h.finalize();
    let n = u32::from_be_bytes([d[0], d[1], d[2], d[3]]);
    f64::from(n) / f64::from(u32::MAX) * 100.0
}
```

Locked test vectors (verified against the reference implementation):

| flag_key | salt | subject | bucket |
|---|---|---|---|
| `checkout.v2` | `a1b2c3` | `user_1001` | `36.47` |
| `checkout.v2` | `a1b2c3` | `user_1002` | `21.03` |
| `checkout.v2` | `a1b2c3` | `user_1003` | `92.11` |
| `checkout.v2` | `z9y8x7` | `user_1003` | `8.39` |
| `new.search` | `a1b2c3` | `user_1001` | `0.33` |

These assert the three properties the design depends on: ramping a percentage
never re-rolls anyone already included; rotating the salt reshuffles the cohort;
different flags bucket independently so the same unlucky users aren't the guinea
pigs for every experiment.

### Bootstrap: already solved

A service deployed on Temps already receives `TEMPS_API_URL` and
`TEMPS_API_TOKEN` as auto-injected environment variables
(`crates/temps-deployments/src/services/env_resolver.rs:253,272`). The token is
environment-scoped and carries a `permissions` array
(`crates/temps-entities/src/deployment_tokens.rs`).

So the flags SDK needs **no configuration at all** — add a `flags:read`
permission to the injected token and `flags.init()` works with zero arguments,
already scoped to the right project and environment.

**The flags client lives inside the existing `@temps-sdk/node-sdk` package, not
a separate library.** A separate package would have to duplicate this same
deployment-token bootstrap, and it is a few hundred lines of caching, not a
product.

```js
import { flags } from '@temps-sdk/node-sdk'

await flags.init()   // reads TEMPS_API_URL + TEMPS_API_TOKEN from the environment

// synchronous, in-memory, no network I/O
if (flags.get('payments.stripe_enabled', {}, true)) { ... }
```

Two operational notes: the token is baked into the container at deploy time, so
rotating or revoking it 401s running containers until redeploy; and a failed
`init()` must serve fallbacks and log loudly rather than throw — a flag service
outage may never take down the app that depends on it.

The same `context` argument is present and ignored in Phase 1. When Phase 2
lands, users add attributes to a call they have already written.

### Delivery, Phase 1

`GET {TEMPS_API_URL}/flags/snapshot` — all flags for the token's environment,
already resolved through steps 1–5, with `ETag` / `If-None-Match` (R3.5). SDK
polls on an interval; a `304` is nearly free. Push propagation over the existing
`route_table_changes`-style NOTIFY channel is Phase 2 and swaps the transport
without changing the endpoint or payload.

`client_visible = false` flags are included here (this is the server-side,
authenticated path). The unauthenticated same-origin `/api/_temps/flags`
endpoint is Phase 2, and filters to `client_visible = true` only.

### Management surface, Phase 1

REST, under the existing project scoping, with `permission_guard!`, audit
logging on every mutation, and OpenAPI feeding the generated SDKs:

```
GET    /projects/{id}/flags
POST   /projects/{id}/flags
GET    /projects/{id}/flags/{key}
PATCH  /projects/{id}/flags/{key}                          # description, default, client_visible
DELETE /projects/{id}/flags/{key}                          # archive, not hard delete
PUT    /projects/{id}/flags/{key}/environments/{env_id}    # value + enabled
```

CLI parity in `apps/temps-cli/src/commands/flags/` — TypeScript, not a Rust
subcommand (CLAUDE.md):

```
temps flags list [--environment <e>] [--json]
temps flags get <key> [--environment <e>]
temps flags set <key> <value> [--environment <e>]
temps flags disable <key> --environment <e>     # the kill switch
temps flags enable  <key> --environment <e>
temps flags archive <key>
```

`temps flags rules ...` is Phase 2 and slots in as a subcommand group without
touching the above.

### Explicitly not in Phase 1

Rule CRUD and UI, percentage rollout wiring, the unauthenticated
`/api/_temps/flags` endpoint, browser SDK, implicit context, NOTIFY propagation,
OFREP, exposure events. Each is additive against the schema and resolution order
above.

## Implementation notes (Phase 1, as built)

Deviations from the design above, all deliberate:

- **`FlagValue` is `serde_json::Value`, not an enum.** Storage is `jsonb` and the
  declared type lives in its own column, so an enum would only add a lossy
  conversion. Validation is `FlagValueType::matches(&Value)`. The response DTOs
  are still typed structs; the one polymorphic field is polymorphic by design.
- **`rules` is not in the snapshot payload.** The column exists and the
  evaluator reserves its step, but shipping `"rules": []` on every flag is noise.
  Adding a field to a JSON response later is backward-compatible — the resolution
  order, which is not, is already fixed.
- **`#[schema(value_type = Object)]` was removed from every JSON-valued field.**
  utoipa renders `Object` as `{"type": "object"}`, which told every generated
  client that a bool flag's default is an object. Unannotated `serde_json::Value`
  renders as a free-form `{}`, and the TypeScript client now gets `unknown`.
- **The snapshot endpoint requires a deployment token.** Scope comes from the
  token, never a path parameter, so a compromised app cannot read another
  tenant's flags. Session/API-key callers get a 400 pointing them at
  `/projects/{id}/flags`.
- **Deployment tokens are read-only for flags.** `Permission::FlagsRead` bridges
  to `DeploymentTokenPermission::FlagsRead`; `FlagsWrite`/`FlagsDelete`
  deliberately do not. A credential baked into a container must not be able to
  flip a production flag.
- **The `@temps-sdk/node-sdk` generated client was not regenerated.** Its
  checked-in spec is 242 paths stale; regenerating it here would bury this
  change in unrelated churn. `FlagsClient` is hand-written anyway — ETag caching
  and background refresh are not things a generated CRUD wrapper does.
- **Two guards are required on every project-scoped handler, not one.**
  `project_access_guard!` deliberately *skips* deployment tokens (they carry no
  user identity for a team-membership check) and delegates their confinement to
  `project_scope_guard!`. Shipping only the former left a cross-project IDOR: a
  deployment token for project A could read project B's flags by changing the
  path id. Caught by the security audit, reproduced live, fixed on all six
  project-scoped handlers, and locked down by four regression tests that fail if
  the guard is removed.
- **`feature_flags.salt` is `#[serde(skip_serializing)]`.** Response DTOs omit
  it by hand, but that was convention rather than enforcement — and a leaked
  salt lets a client pre-compute its own bucket and self-select into a rollout.
- **The snapshot endpoint returns one error for both "no such environment" and
  "another project's environment."** Distinguishing them turned the endpoint
  into an environment-id existence oracle.

- **Console UI lives under the project, not settings.** A flag is scoped to a
  project *and* an environment, so it sits in the project sidebar directly after
  Environment Variables — the sibling concept it exists to contrast with (env
  var ⇒ redeploy, flag ⇒ seconds). Route: `/projects/{slug}/flags`.
- **The table is one environment at a time; the sheet is all of them.** The
  question people actually arrive with is "what is this flag doing in
  production?", so the page has an environment selector and shows the
  *effective* value per row. The detail sheet then shows every environment at
  once, which is where the project↔environment relationship becomes explicit.
- **`resolveEffectiveValue()` in `web/src/components/project/flags/flag-value.ts`
  is a third mirror of the evaluator**, alongside Rust and the SDK. That is a
  real duplication risk and is called out in the file: if it drifts, the console
  shows one value while the app receives another. Worth collapsing when the
  same-origin evaluation endpoint lands in Phase 2 and the UI can just ask.
- **Kill switch vs. value are kept visually distinct.** The row switch sets the
  *value*; the kill switch lives in the row menu and the sheet, and a
  kill-switched row renders its switch disabled with a `Disabled` badge. Letting
  someone toggle a value that the kill switch is overriding would be a lie.

## Phasing

- **Phase 1 — the primitive. Done.** The two tables, `evaluate()` with steps
  1/2/4/5, `bucket()` shipped and tested but uncalled, snapshot endpoint with
  ETag, server-side SDK, console UI, CLI, RBAC, audit logging. Ships as: *flags
  you can flip without a redeploy.*
- **Phase 2 — targeting.** Rule engine at step 3, rollout wired to `bucket()`,
  rule CRUD + UI + CLI, same-origin client endpoint, implicit context (requires
  new upstream header injection — see below), NOTIFY propagation, OFREP.
- **Phase 3 — the moat.** Exposure events, variant as a breakdown dimension on
  analytics/errors/replay, flag changes on the incident timeline, edge injection
  for SDK-less apps.

Phase 1 alone reaches parity with Railway. Phase 3 is the part no competitor can
copy without also owning analytics, errors, and replay in the same binary.

**Correction to R2.4:** implicit context is free for *browser* calls through the
proxy, but **not** for in-process server-side evaluation — the proxy is not in
that path and today injects only `X-Forwarded-For` / `X-Forwarded-Proto`
(`crates/temps-proxy/src/proxy.rs:4008`); geo is resolved asynchronously in the
logging path, not on the request. Server-side implicit context requires new
upstream header injection in Phase 2, and any such header must strip its inbound
counterpart first — exactly as the proxy already does for `X-Temps-Demo-Mode`
(`proxy.rs:2882`) — or targeting becomes client-spoofable.

---
**Maintenance:** update when the crate boundary (open question 1) and the
OSS/plugin split (open question 2) are decided.
