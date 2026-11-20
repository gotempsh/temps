# ADR 110: What product analytics is missing to match PostHog

## Status

Accepted — 2026-11-19. D3 implemented; D1 implemented 2026-11-20;
D2, D4–D9 proposed. See [Settled while implementing](#settled-while-implementing).

Related: ADR 034 (feature flags; its Phases 2 and 3 are D3 here, and its
non-goal R8 is revisited by D2), ADR 012 (ClickHouse analytics backend),
ADR 076 (analytics ingest keys), ADR 109 (web analytics gaps; its conversion
goals are the metric D2 measures).

One of four gap ADRs (109 Plausible, 110 PostHog, 108 Better Stack,
111 Sentry). The target is 90% of what a typical user relies on.

## Context

PostHog is the widest of the four products and the one Temps covers least.
Temps has the collection side (events, visitors, replay) and little of the
analysis side.

### What exists

| Area | Where | Today |
|---|---|---|
| Event capture, property breakdowns, timelines, unique counts | `/projects/{project_id}/events/*` in `crates/temps-analytics-events` | Yes |
| Funnels | `crates/temps-analytics-funnels` | Yes |
| Page flow, visitor journey, visitor profiles and enrichment | `crates/temps-analytics` | Yes; browser `identify()` is a no-op (ADR 109 D4) |
| Web session replay with masking and console capture | `crates/temps-analytics-session-replay`, `SessionRecorder.ts` | Yes |
| Web vitals | `crates/temps-analytics-performance` | Yes |
| Feature flags: typed values, per-environment overrides, kill switch | `crates/temps-flags` | Phase 1 of ADR 034 |
| Feedback inbox fed by Sentry SDK feedback widgets | `crates/temps-error-tracking/src/handlers/feedback_handler.rs`, `web/src/pages/ProjectFeedback.tsx` | Yes |
| Dashboards | `metric_dashboards`, `web/src/pages/DashboardBuilder.tsx` | OpenTelemetry metrics only |
| First-party SDKs | `sdks/node/packages/` | Browser, React, Vue, Svelte, Node |

### What flags can do

ADR 034 shipped the primitive and designed the rest. The `rules` column exists
and is always `[]`; `bucket()` and the per-flag `salt` exist and nothing calls
them; `EvalContext` is threaded through and ignored. There is no targeting, no
percentage rollout, no variant, no browser evaluation and no exposure event.
ADR 034 R8 lists experiment statistics as a non-goal for its first version.

## Decision

The gaps, most important first.

### D1. Surveys

**Missing.** There is no way to ask users a question inside the app. The
feedback inbox receives what a Sentry SDK widget sends, but Temps cannot define
a question, decide who sees it, or summarise the answers.

**Shape.**

- A survey is a per-project row: an ordered list of questions (open text,
  rating, NPS, single choice, multiple choice) and display conditions (URL
  pattern, a triggering event, a sampling percentage, once per visitor, and,
  once D3 and D5 exist, a flag or a cohort).
- Delivery is the analytics browser SDK: it fetches the active surveys for the
  page and renders a small widget. An API mode returns the definition so an
  app can render its own UI.
- Answers are ordinary events (`survey_shown`, `survey_dismissed`,
  `survey_sent`, with the survey and question ids as properties). They need no
  new store, appear in existing breakdowns, and can be goals and funnel steps.
- A results view per survey: response rate, distribution per question, NPS
  score, and the open-text answers with a link to the visitor and their
  replay.
- Survey definitions reach the browser through the same public, read-only
  endpoint that D3 adds for client-visible flags. Whichever of D1 and D3 ships
  first builds it.

### D2. Experiments

**Missing.** Temps cannot run an A/B test: no variants, no exposure record, no
comparison, no statistics.

**Shape.**

- An experiment is a per-project row linking one multivariate flag (D3), one
  primary metric (a conversion goal from ADR 109 D2, or a funnel) and optional
  secondary metrics, with a start and end.
- Exposure events (ADR 034 R6.1) record which variant a subject saw. They are
  high volume, so they stay sampled and opt-in per flag, and switch on
  automatically only for a flag that belongs to a running experiment.
- Results: conversion rate per variant with an interval, the probability each
  variant is best, and a plain statement of whether there is enough data yet.
  One method, done correctly, rather than a choice of statistical engines.
- This overturns ADR 034 R8 for experiments only. Scheduled changes and
  approval workflows stay non-goals.

**Depends on D3.** Experiments are ranked above flag targeting because they
are the outcome users ask for, but they cannot be built first. See *Build
order*.

### D3. Flag targeting, rollouts and variants

**Missing.** Everything in ADR 034 Phase 2 and the exposure half of Phase 3.

**Shape.** As ADR 034 already decides: the rule loop at resolution step 3,
percentage rollout wired to `bucket()`, rule CRUD in the console and CLI, a
same-origin endpoint serving only `client_visible` flags to browsers, OFREP
for OpenFeature SDKs. This ADR adds two things:

- **Variants.** A flag may declare named variants with weights, so a rule can
  serve "50% control, 50% test". ADR 034 left `variant` as `None`.
- **Priority.** Phase 2 moves ahead of the remaining Phase 3 items, because
  D1's targeting and all of D2 wait on it.

### D4. Insight types

**Missing.** Trends and funnels exist. These do not:

1. **Retention**: of the visitors who first did X in a given week, how many
   came back and did Y in each following week.
2. **Stickiness**: how many days per period a visitor did X.
3. **Lifecycle**: new, returning, resurrecting and dormant visitors per
   period.
4. **Formulas** over two or more series (a ratio of two events).
5. **Saved insights and product dashboards**: a query cannot be named, saved
   or pinned. `metric_dashboards` holds OpenTelemetry metric panels only.

**Shape.** Retention first, since it is the question the other three cannot
answer. All four are queries over the existing event store with a bounded
period and a bounded number of cohorts; none needs a new ingest path. Saved
insights extend the existing dashboard builder with an analytics panel type
instead of adding a second dashboard system.

Retention, stickiness and lifecycle need a visitor identity that lasts longer
than a day, so they are unavailable for projects in the cookieless mode of
ADR 109 D3. The view says so.

### D5. Cohorts and group analytics

**Missing.**

- **Cohorts.** A set of visitors cannot be saved and reused. Visitor facets
  and segment views filter a list (`/analytics/visitor-facets`,
  `web/src/components/analytics/SegmentVisitors.ts`) but the result has no
  name and cannot be applied elsewhere.
- **Group analytics.** Events belong to a visitor only. There is no company,
  workspace or account to aggregate by, which is the unit a B2B product
  measures.

**Shape.**

- A cohort is a named, per-project definition: property conditions ("plan is
  pro", over the traits ADR 109 D4 lets an app set) and behavioural ones ("did
  `checkout` in the last 30 days"), evaluated at query time. It is usable as a
  filter on every insight, as a flag targeting condition and as a survey
  display condition. Static cohorts (an uploaded list) come later.
- A group is an optional key on an event (type and id) plus a small profile
  per group, parallel to visitor enrichment. Insights gain "unique groups" as
  an aggregation. The number of group types per project is capped so the key
  stays a bounded dimension.

### D6. Autocapture and heatmaps

**Missing.** Every event beyond page views needs code. No click or scroll
heatmap, no toolbar for defining events by pointing at the page. ADR 109 D1
adds automatic outbound-link and download events; general autocapture of
clicks and form submissions is a much larger volume of events and needs its
own sampling and quota before it can be on by default.

### D7. Replay depth

**Missing.** Console output is captured; network requests are not. No
rage-click or dead-click detection, and no filtering of replays by event,
error or flag variant beyond what the list offers today.

### D8. SQL access, exports and pipelines

**Missing.** No query editor over the event store, no scheduled export to
object storage, no forwarding of events to another destination.

### D9. Mobile and server SDKs

**Missing.** First-party analytics SDKs are JavaScript only. The Python
package under `sdks/python` is the plugin SDK. No iOS, Android, React Native
or Flutter. OFREP (D3) covers flags in other languages; analytics capture
would still need at least Python and Go clients.

## Build order

Importance and dependency differ for D1 to D3:

1. D3 (targeting, variants, the public read endpoint).
2. D1 (surveys), which reuses that endpoint and can target by flag.
3. ADR 109 D2 (conversion goals), then D2 (experiments).
4. D4, then D5. Cohorts are more useful once there are insights to apply them
   to.

## Not doing

- **A data warehouse.** Joining external sources (billing, CRM) inside Temps
  is outside the 90% and outside a 2 GB host.
- **A second dashboard system.** Product insights go into the existing
  builder.
- **Statistical method choice in experiments.** One method.

## Settled while implementing

### D3

Built as ADR 034 Phase 2 plus variants. Decisions the ADRs left open, or got
wrong about the code:

- **Variants live on the flag, not the rule.** `feature_flags.variants` holds
  `{key, value, description}`; a rule serves a value, one variant, or a
  `split` of variants with integer weights summing to 100. A variant a rule
  still serves cannot be removed (400 naming the variant and environment).
- **Splits hash independently of rollouts.** The arm is chosen by
  `bucket("{key}#split", salt, subject)`, not by the rollout bucket, so a rule
  with a 20% rollout and a 50/50 split still splits its 20% evenly. `bucket()`
  itself is unchanged and its locked vectors still pass; the split has its own.
- **Missing information never matches.** An attribute the caller did not send
  matches only `not_exists`. A partial rollout or split with no subject key is
  skipped rather than assigned at random.
- **A malformed stored rule serves the default with reason `ERROR`** for the
  whole flag in that environment, instead of skipping the rule: skipping would
  silently change which later rule matches. A split's shape is checked before
  the subject, so a broken split is reported whether or not the caller sent
  a key. The API reports it as
  `rules_valid: false` so the console can say so.
- **Equality is loose only between numbers and numeric strings.** `5 == "5"`;
  `true != "true"`. Attributes typed from query strings and forms stay usable
  without making booleans ambiguous.
- **Rule edits are compare-and-set.** `PUT …/rules` and `PATCH` of the flag
  carry `expected_updated_at`; a mismatch is 409 with "reload and apply again"
  (REVIEW.md stale writes). Rules are replaced as a list, never patched by
  index, because indices shift under concurrent edits. Both writes lock the
  flag row (variant edits exclusively, rule saves shared), so a variant
  cannot be removed while a rule naming it is being saved.
- **Delivery is an in-memory cache, not NOTIFY.** The public endpoints read an
  environment's compiled flags from an `ArcSwap` map (TTL 5 s, at most 4
  concurrent loads, stale-while-revalidate, at most 4,096 environments), and
  every write through `FlagService` drops the project's entries. A load that
  read the database before such a write is served but not cached (an
  invalidation generation), so it cannot put the old flags back. A change is
  live on the next request on a single binary and within 5 s across split
  console/proxy processes. ADR 034 R3.6 asked for < 2 s via NOTIFY; that is
  deferred until a split topology needs it.
- **The browser endpoint is `POST /api/_temps/flags`, resolved by `Host`**
  through the route table, like analytics ingest, with no key and no CORS. It
  serves only `client_visible` flags and only evaluated results (value,
  variant, reason). Limits: 30,000 requests/minute per project and 600 per
  client address, lock-free fixed windows; over the limit is 429 with
  `Retry-After`. Bodies are capped at 16 KiB and 50 attributes. The
  per-key `POST /api/_temps/flags/{key}` in ADR 034 R3.2 was not built: OFREP's
  single-flag endpoint covers it.
- **Implicit attributes** on the browser endpoints: `environment`,
  `environment_id`, `deployment_id`, `host`, `visitor_id`, `session_id` (Temps
  cookies), `browser`, `os`, `device_type` (User-Agent), `country` (GeoIP,
  when configured). Explicit attributes override them. Server-side implicit
  context (ADR 034's correction to R2.4: proxy header injection) is not built.
- **OFREP** is served twice: on the site's origin
  (`/api/_temps/ofrep/v1/evaluate/flags[/{key}]`, same scope and limits as the
  browser endpoint) and on the API (`/api/ofrep/v1/…`) for server SDKs, which
  requires an environment-scoped deployment token. Bulk responses carry an
  `ETag` over the flag definitions and the context, and honour
  `If-None-Match`.
- **Exposure events are ordinary analytics events.** A flag has
  `exposure_sample_rate` (0–1, default 0 = off; ADR 034 R6.1's opt-in). The
  server marks an evaluation `record_exposure` when the subject's
  `bucket("{key}#exposure", salt, subject)` falls under the rate, so the same
  visitors are always sampled; the browser SDK then sends one `flag_exposure`
  event (`flag_key`, `variant`, `reason`) per flag and variant per page
  through the existing ingest path. No new event stream, which answers this
  ADR's second open question. Server-side SDKs do not emit exposures yet.
- **Browser SDK.** `FeatureFlags` in `@temps-sdk/analytics-core` (lazy: no
  request until the first read; synchronous reads; never throws; bootstrap
  values for SSR) and `useFlag`/`useFlagVariant`/`useFeatureFlags` in
  `@temps-sdk/react-analytics`. Flags work when analytics is disabled for
  localhost; only the provider's `disabled` turns them off.
- **The Node SDK evaluator mirrors the Rust one**, with the same locked
  vectors. One deliberate difference: JavaScript has no linear-time regex
  engine, so `matches` never matches an input longer than 1,024 characters
  there.

### D1

Built in a new `temps-surveys` crate. Decisions the shape above left open:

- **Its own endpoint, the flags mechanism.** `POST /api/_temps/surveys`,
  resolved by `Host` through the route table like `/api/_temps/flags`, with
  the same body cap (16 KiB) and limits (30,000 requests/minute per project,
  600 per client address, 429 with `Retry-After`). A separate endpoint rather
  than a field on the flags response, so an app that uses only one of them
  does not pay for the other, and each can fail without hiding the other.
- **The server decides sampling and the flag condition; the browser decides
  the rest.** `sample_rate` is `bucket("survey:{id}", "", subject)`, so it is
  sticky per visitor; without a subject a survey under 100% is not shown. A
  flag condition evaluates the flag in the request's environment with the
  same implicit attributes as the flags endpoint, and fails closed (missing,
  archived, unevaluable flag, or flags that could not be loaded). Neither the
  rate nor the condition is sent to the browser. Path pattern, triggering
  event and once-per-visitor are applied in the browser, because only it
  knows the client-side route, the event just tracked and what it has shown
  (`localStorage`, at most 200 ids).
- **Cohort targeting waits on D5**, as the shape said.
- **Own permissions.** `surveys:read` (readers and up) and `surveys:write`
  (users and up). `analytics:write` is admin-only, which would have kept
  survey authoring from the product people it is for. Deployment tokens get
  neither.
- **At most 50 active surveys per project**, so the delivery payload and the
  per-request selection stay bounded. Activating one more is a 400 naming the
  limit.
- **Delivery reuses D3's cache design:** active surveys per project in an
  `ArcSwap` map (TTL 5 s, at most 4 concurrent loads, at most 4,096 projects,
  an invalidation generation so a load that read before a write is not
  cached). Every write through `SurveyService` drops the project's entry.
- **Edits are compare-and-set** on `expected_updated_at`, under a row lock,
  like flags.
- **Results are SQL over `events`**, three `GROUP BY` queries over the
  survey's `survey_*` events in a window (default 30 days, at most 366),
  reading `COALESCE(props, event_data::jsonb)` so events from older SDKs
  count. A distribution is capped at 5,000 rows. Choice answers keep the
  declared order and zero-fill; answers that no longer match a choice are
  counted as `other`, not dropped.
- **Responses link to the visitor, not the replay.** `events.session_id` is
  the analytics session, which does not map to a replay recording; the
  visitor profile lists that visitor's replays. A direct replay link waits on
  that mapping.
- **Deleting a survey keeps its answers**: they are events, and events are
  not deleted by deleting a definition.
- **An API mode is the SDK's `autoShow: false`**: `analytics.surveys` returns
  the eligible definitions and validates and records answers, and the app
  renders them.

## Open questions

- Whether surveys and feedback should be one inbox. They share the "what users
  told us" surface but come from different sources.
- ~~Whether exposure events for experiments can be derived from existing
  events carrying a variant property, avoiding a separate event stream.~~
  Settled for D3: exposures are ordinary `flag_exposure` events.
