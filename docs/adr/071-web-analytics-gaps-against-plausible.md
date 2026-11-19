# ADR 071: What web analytics is missing to match Plausible

## Status

Proposed — 2026-11-19

Related: ADR 012 (ClickHouse analytics backend), ADR 040 (analytics ingest
keys), ADR 072 (product analytics gaps against PostHog; D2 here is the goal
metric its experiments use, and the user id of D4 is what its cohorts and
flag targeting key on).

This is one of four gap ADRs (071 Plausible, 072 PostHog, 073 Better Stack,
074 Sentry). Each one records what a comparable product offers that Temps does
not, ranked from most to least important. The target is 90% of what a typical
user of that product relies on, not feature-for-feature parity.

## Context

Plausible is the closest of the four. A site owner who moves from Plausible to
Temps today keeps most of the dashboard.

### What exists

| Area | Where | Today |
|---|---|---|
| Visitors, sessions, page views, bounce rate, live visitors | `crates/temps-analytics` | Yes |
| Breakdowns: pages, referrers, channels, UTM, country/region/city, browser, OS, device, language | `web/src/components/analytics/overview/` | Yes |
| Dashboard filters by dimension, period comparison | `filter_*` fields in `crates/temps-analytics/src/types/requests.rs` | Yes |
| Custom events with property breakdowns | `/projects/{project_id}/events/*` in `crates/temps-analytics-events` | Yes |
| Funnels | `crates/temps-analytics-funnels` | Yes |
| Engagement time (heartbeat, page leave) | `sdks/node/packages/analytics-core/src/EngagementTracker.ts` | Yes |
| Bot and AI-crawler filtering | `crates/temps-analytics-backend`, `web/src/pages/ProjectAiCrawlers.tsx` | Yes |
| Weekly email digest with visitors and top pages | `crates/temps-notifications/src/digest/` | Yes, weekly only |
| Revenue (Stripe MRR, customers) | `crates/temps-revenue` | Yes, not tied to a conversion |
| Visitor profiles with first-touch source, sessions and replays | `crates/temps-entities/src/visitor.rs`, `web/src/components/visitors/VisitorDetail.tsx` | Yes |
| Visitor enrichment | `PUT /analytics/visitors/{visitor_id}/enrich` | Server-side only |
| Journey of one visitor: sessions, pages, events | `/analytics/visitors/{visitor_id}/journey`, `web/src/components/visitors/VisitorJourney.tsx` | Yes, per browser |

### What the tracker sends by itself

The browser tracker emits three events without any code from the site owner:
`page_view`, `page_leave` and `heartbeat` (`Analytics.ts`,
`EngagementTracker.ts`). Everything else needs an explicit `track()` call.

### How a visitor is identified

Two mechanisms, both persistent:

- When Temps serves the page, the proxy sets `_temps_visitor_id` (365 days) and
  `_temps_sid` (30 minutes): `crates/temps-proxy/src/proxy.rs`,
  `visitor_max_age_days` and `session_max_age_minutes` in
  `crates/temps-proxy/src/traits.rs`.
- When Temps is only the analytics backend, the SDK stores `temps_visitor_id`
  and `temps_session_id` in `localStorage`
  (`sdks/node/packages/analytics-core/src/identity.ts`).

There is no mode that stores nothing on the visitor's device.

### How a visitor becomes a known user

Enrichment is a server-side `PUT` that merges a `custom_data` object into the
visitor row. To use it at signup, a developer has to do four things by hand:
get the visitor id from the browser to their backend (the sealed
`_temps_visitor_id` cookie when Temps proxies the app, otherwise the
`localStorage` id), hold a credential there (an API key, or the injected
deployment token with `visitors:enrich`), call the endpoint, and check
`success` in the answer.

The browser SDKs do not help:

- `identify(userId, traits)` exists in `analytics-core` and in the React
  provider and does nothing. Its body is a comment: "Placeholder for SDK
  parity; implement when identity endpoint is available"
  (`sdks/node/packages/analytics-core/src/Analytics.ts`).
- The analytics documentation shows `analytics.getVisitorId()` as the way to
  read the id in React. No SDK package defines it.

Nor does the data model. A visitor row has no user id: who the person is
lives in free-form `custom_data`. So the visitors list cannot be searched by
email or user id (its filters are country, region, city, channel and
referrer), and one person on a phone and a laptop is two unrelated visitors.

## Decision

The gaps, most important first. Each is a separate piece of work with its own
PR; this ADR fixes the order and the shape, not the implementation detail.

### D1. Outbound links, file downloads and 404s, tracked automatically

**Missing.** A Plausible user gets these three reports by ticking a box. In
Temps each needs hand-written `track()` calls, so most sites never have them.

**Shape.**

- *Outbound links and file downloads.* One delegated `click` listener in
  `analytics-core`, on by default, emitting `outbound_click` (property: target
  host and URL) and `file_download` (property: URL and extension). The
  extension list is configurable; the default covers documents, archives,
  media and installers. One listener for the whole document, no per-link
  handlers, `sendBeacon` as the existing events use.
- *404s, server side.* Temps is the proxy, so it already records the status
  code, path, referrer and bot flag of every request in `proxy_logs`. A
  "Not found" report (path, hits, top referrer) is a query over rows that
  exist today, with no change to the site and no new write on the request
  path.
- *404s, client side.* Single-page apps answer `200` for a missing route, and
  sites that only use Temps for analytics have no proxy logs. For those, a
  `trackNotFound()` call and a `data-temps-404` marker emit a `not_found`
  event.

These are ordinary events, so they appear in the existing event breakdowns and
can be used as goals (D2) and funnel steps with no extra work.

### D2. Conversion goals

**Missing.** There is no way to say "a signup is a conversion" and see a
conversion rate. Funnels are the only conversion view, and they need at least
two steps. The existing `/projects/{project_id}/analytics/activity/goals`
endpoint is unrelated: those are prompts for the AI activity report.

**Shape.**

- A goal is a named row per project: either an event (name plus an optional
  property filter) or a page (path pattern). It reuses the matching funnel
  steps already have (`funnel_steps.event_name`, `event_filter`).
- The overview gains a Goals section: unique conversions, total conversions,
  conversion rate against visitors in the selected period.
- A goal is a dashboard filter like any dimension: "show me sources for
  visitors who converted".
- A goal may name a numeric property and currency as its revenue value, giving
  revenue and average order value per source. This is separate from the Stripe
  MRR in `temps-revenue`, which is account-level and not attributed to a
  visit.
- Because the name collides with the AI report's "goals", the UI calls these
  **Conversions** and the entity is `analytics_goals`.

### D3. Cookieless tracking

**Missing.** Both identity mechanisms store an identifier on the device, so a
site in the EU needs a consent banner before it can count anyone. Plausible's
main selling point is that it does not.

**Shape.**

- A per-project identity mode, stored on the project's analytics settings row
  (not an environment variable): `persistent` (today's behaviour, the default
  for existing projects) or `cookieless`.
- In `cookieless` mode the visitor id is a hash of a daily salt, the project,
  the client address and the user agent. The salt rotates every 24 hours and
  the previous one is deleted, so a visitor cannot be followed across days or
  recovered from stored data. The proxy sets no analytics cookie and the SDK
  writes nothing to `localStorage`.
- The salt is one small value per instance, loaded into memory and swapped on
  rotation. Computing the id is one hash per event with no I/O, which keeps
  the ingest path within the hot-path rules. Every node that ingests must read
  the same salt, so it lives in the database, not in process memory alone.

**Consequences the UI must state.** In cookieless mode a returning visitor is
a new visitor the next day. Multi-day profiles of anonymous visitors,
"returning visitors" and the retention insight of ADR 072 do not work, and
unique-visitor counts over a period are sums of daily uniques. Visitors who
sign in and are identified (D4) are the exception: their user id joins their
days together. The settings page says so next to the switch; the affected
views show a short fact line rather than empty charts.

### D4. One call to identify a visitor at signup

**Missing.** See *How a visitor becomes a known user*. Linking a signup to the
visit that produced it is the most common thing a developer wants from
visitor-level analytics, and today it is a multi-step integration that the
browser SDK's own `identify()` silently ignores.

Plausible does not do this at all: it is aggregate-only by design. Temps
already keeps visitor profiles, so this is where it should be better than
Plausible rather than equal to it.

**Shape.**

- **Browser: make `identify()` real.** `identify(userId, traits)` in
  `analytics-core`, and therefore in the React, Vue and Svelte packages, sends
  the user id and traits through the existing ingest endpoint. The server
  applies them to the visitor the request already resolves to. A browser can
  therefore only label itself; it never names a visitor id.
- **Server: one helper.** `temps.analytics.identify(request, { userId,
  traits })` in the Node SDK reads the visitor from the incoming request (the
  sealed cookie, or the id the browser SDK forwards in a header) and calls
  enrichment with the injected deployment token. The developer passes the
  request and the user; nothing else. The same helper follows in other
  languages as analytics SDKs appear (ADR 072 D9).
- **Fix or remove `getVisitorId()`.** Either the SDK exports it or the
  documentation stops showing it. With the two calls above, few apps need it.
- **A user id on the visitor.** An indexed `user_id` column next to
  `custom_data`, set by `identify`. Traits stay in `custom_data`.
- **Trust.** Traits sent from a browser are claims by that browser. They are
  stored with their origin (`browser` or `server`) so the console can show
  which facts the app's backend vouched for, and a per-project setting can
  refuse browser-origin identity altogether for apps that need it verified.
  The limits deployment tokens already have apply to both paths: 32 keys,
  8 KB, a per-minute budget on visitor-changing writes.
- **Volume.** `identify` is called on sign-in, not per event. The SDK
  remembers what it last sent and skips an identical call, and the server
  writes the visitor row only when something changed, so a provider that
  calls `identify` on every render costs nothing.
- **Sign-out.** `reset()` clears the browser-side identity so the next person
  on a shared device starts as a new visitor.

**With cookieless mode (D3).** A user id is the one stable identity a
cookieless project has. An identified user's sessions can be joined across
days by `user_id`; the anonymous visits before sign-in can only be joined
within the same day.

### D5. Analysing a visitor's journey

**Missing.** The journey view shows one visitor's sessions, pages and events,
with the channel and campaign of each session. Three things stop it answering
"how did this customer get here?":

1. **Finding the person.** No search by email, user id or any trait.
2. **One person, several browsers.** Each device is a separate visitor with a
   separate journey.
3. **One signal.** Replays are a separate tab; errors, feedback and revenue
   for the same visitor are in other parts of the console. Error events
   already carry a `visitor_id`.

And nothing answers the same question for many visitors at once.

**Shape.**

- **Search** the visitors list by `user_id` and by a small set of indexed
  traits (email, name), using the column D4 adds.
- **A person view.** Visitors sharing a `user_id` are shown as one person: one
  merged journey, with each session labelled by device. The rows are joined
  at query time, not rewritten, so a wrong `identify` call can be undone by
  correcting it.
- **One timeline.** The journey places sessions, conversions (D2), errors,
  replays, feedback and revenue events in time order, each linking to its own
  detail page. The first touch (`first_referrer`, `first_channel`,
  `first_utm_*`, already stored on the visitor) and the first conversion are
  called out at the top with the time between them.
- **Journeys in aggregate.** For a chosen conversion: the most common
  sequences of pages and events before it, the number of sessions and days it
  took, and the first-touch and last-touch source. This is the existing page
  flow run backwards from a goal instead of forwards from an entry page, over
  a bounded period and a bounded number of paths.
- **Deletion.** A person can be deleted as a unit (all their visitors, events
  and replays), which is what a data-subject request asks for.

### D6. Shared and public dashboards

**Missing.** Analytics can only be seen by a signed-in Temps user. There is no
link to send a client and no embed.

**Shape.** Status pages already solve this safely: a 128-bit random link, a
page built from an allowlist rather than by hiding fields, an in-memory
snapshot so public traffic never runs a query, and per-client rate limits
(`crates/temps-status-page/src/services/public_pages/`). A shared dashboard
uses the same pattern for a fixed set of aggregate views. It never exposes
visitor-level data, session replay or event properties. Optional password;
revocable by rotating the link.

### D7. CSV export

**Missing.** No breakdown, event list or funnel can be exported. Streamed
export per view, bounded by the selected period, with the same permissions as
the view.

### D8. Traffic spike and drop alerts, and a monthly report

**Missing.** Nothing tells the owner when traffic doubles or stops. The metric
alert evaluator and its anomaly detectors (`crates/temps-otel/src/detectors/`)
and notification routes already exist; this exposes visitors per interval as
an alertable series. The weekly digest gains a monthly option and per-project
recipients.

### D9. Search Console keywords and a Google Analytics import

**Missing.** No search-query data and no way to bring history across. Both are
integrations with an external account and matter mostly at migration time, so
they come last.

## Not doing

- **A sub-kilobyte script as a goal in itself.** D1 adds a listener to the
  tracker. The script stays small by keeping session replay and web vitals in
  the separate `auto-full` build, not by leaving features out.
- **Removing persistent identity.** Visitor profiles and session replay depend
  on it. Cookieless is a choice per project, not a replacement.

## Open questions

- Whether browser-origin identity (D4) should be accepted by default. It is
  what makes `identify()` a one-line integration, and it lets a visitor claim
  any user id for their own profile.
- Whether new projects should default to `cookieless`. It is the safer legal
  default and the worse product default.
- Whether D6 should allow embedding in an iframe, which changes the framing
  headers the public route sends.
