# Analytics

For React apps, use the **add-react-analytics** skill for step-by-step provider/hook setup (`@temps-sdk/react-analytics`, `TempsAnalyticsProvider`, `useTrackEvent`, `useAnalytics`, session recording). That's currently the only official SDK — there is no Temps analytics SDK for other languages/frameworks. This file covers the underlying ingestion API so any language can send events directly over HTTP, the same way [error tracking](error-tracking.md) and [traces](opentelemetry-traces.md) are protocol-level rather than SDK-locked.

## Ingestion endpoints

Routes are registered as `/_temps/event` and `/projects/{project_id}/events/ingest` inside the crate, but the console server nests every plugin's routes under `/api` (`temps-cli/src/commands/serve/console.rs`), so the real externally-reachable paths are:

- `POST /api/_temps/event` — public, unauthenticated, reachable from any HTTP client (not just browsers). Body: `EventMetricsPayload` JSON.
- `POST /api/projects/{project_id}/events/ingest` — authenticated, for server-side/backend event submission. Body: `ConsoleEventPayload` JSON.

Both funnel through the same write path (`AnalyticsEventsService`) into an outbox table in Postgres; a background worker fans events out to ClickHouse if it's configured for the instance — a just-recorded event can take a few seconds to reach ClickHouse-backed dashboard views even though it's already durably written.

Implemented in `temps-analytics-events` crate: `src/handlers/events_handler.rs` (`record_event_metrics` for the public endpoint at line 646, `record_console_event` for the authenticated one at line 843, route table at line 1085), `src/types/requests.rs` (payload structs, lines 175–229).

### `EventMetricsPayload` (public endpoint)

```json
{
  "event_name": "page_view",       // required
  "event_data": {},                 // required, arbitrary JSON
  "request_path": "/products",      // required
  "request_query": "?sort=asc",     // required (empty string if none)
  "screen_width": 1920,             // optional
  "screen_height": 1080,            // optional
  "viewport_width": 1920,           // optional
  "viewport_height": 1080,          // optional
  "language": "en-US",              // optional
  "page_title": "Products",         // optional
  "referrer": "https://google.com", // optional, falls back to the Referer header
  "ttfb": 100.5,                    // optional, Web Vitals (ms)
  "lcp": 2500.0, "fid": 50.0, "fcp": 800.0, "inp": 150.0,  // optional, Web Vitals (ms)
  "cls": 0.1                        // optional, Web Vitals (score, unitless)
}
```

There is no `project_id` field — the project is resolved server-side from the request's `Host` header via the deployment route table. This means calling `/api/_temps/event` **only works if the request's Host header matches a domain Temps already routes to a deployment** (the deployed app's own domain, or a Temps-assigned preview domain). A backend script running outside that domain can't just POST to the Temps console host and expect this to resolve — use the authenticated endpoint instead for out-of-band server-side event submission.

### `ConsoleEventPayload` (authenticated endpoint)

```json
{
  "event_name": "purchase",         // required
  "event_data": { "amount": 49.99 }, // optional, defaults to {}
  "environment_id": 42,              // required
  "deployment_id": 100,              // required
  "visitor_id": null,                // optional, encrypted _temps_visitor_id cookie value (pass through if your backend read it from the user's request)
  "session_id": null,                // optional, encrypted _temps_sid cookie value
  "request_path": "/checkout",       // optional, defaults to "/"
  "request_query": ""                // optional, defaults to ""
}
```

`project_id` goes in the URL path, `environment_id`/`deployment_id` go in the body — all three are required to attribute the event correctly.

## Auth

- `/api/_temps/event` is intentionally public/unauthenticated. Host-header project resolution attributes an event to a project; it does not prove the event came from a genuine browser/user.
- `/api/projects/{project_id}/events/ingest` requires a bearer token (session cookie, `tk_` API key, or `dt_` deployment token — all three work) with `AnalyticsWrite` permission scoped to that project. A token bound to a different project is rejected.

### Trust boundary

Treat public browser analytics as untrusted input:

- Never drive billing, authorization, entitlements, inventory, or authoritative revenue from `/api/_temps/event`.
- Send conversion-critical events from authenticated server code after the business transaction commits.
- Deduplicate in application business storage or an outbox before calling Temps when retries can create duplicates. Temps does not enforce idempotency from an identifier placed in `event_data`.
- Validate event names and allowlist bounded properties server-side for authoritative events.

## Quickstart: send analytics from any language

Since there's no per-language SDK, the pattern is a plain HTTP POST — the same shape regardless of language:

**From a backend already deployed on Temps** (has `TEMPS_API_URL` and `TEMPS_API_TOKEN` auto-injected — see the top-level [SKILL.md](../SKILL.md) quickstart), record a custom server-side event with any HTTP client:

```bash
curl -X POST "$TEMPS_API_URL/projects/$TEMPS_PROJECT_ID/events/ingest" \
  -H "Authorization: Bearer $TEMPS_API_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "event_name": "purchase",
    "event_data": {"amount": 49.99, "currency": "USD"},
    "environment_id": '"$TEMPS_ENVIRONMENT_ID"',
    "deployment_id": '"$TEMPS_DEPLOYMENT_ID"'
  }'
```

`TEMPS_API_TOKEN` is currently a non-expiring deployment token with broad permissions, not an analytics-only key. Keep it server-side, never log or forward it, and never reuse it in browser code. Prefer a separately managed, purpose-scoped credential when the endpoint and deployment workflow support one; otherwise limit this call site and rotate the deployment credential after suspected exposure.

The equivalent in any language is just an authenticated JSON POST — Python (`requests.post(...)`), Go (`net/http` + `encoding/json`), Ruby (`Net::HTTP` or `httparty`), PHP (`curl_init`/Guzzle), Java (`HttpClient`), .NET (`HttpClient`), Rust (`reqwest`): no client library is needed because this is a plain REST call, not a protocol requiring a purpose-built SDK.

`environment_id`/`deployment_id`/`project_id` are **not** currently among the auto-injected env vars (unlike `TEMPS_API_URL`/`TEMPS_API_TOKEN`) — read them from the Temps dashboard (**Project Settings**) or from whatever IDs your deployment automation already has on hand, and set them yourself (e.g. as regular env vars) if a backend needs to call this endpoint repeatedly.

For browser-side tracking in a non-React frontend (vanilla JS, Vue, Svelte, a mobile app's WebView), POST directly to `/api/_temps/event` with `fetch`/`XMLHttpRequest`/equivalent using the `EventMetricsPayload` shape above — no auth needed, but it only resolves a project when the request's `Host` matches a routed deployment domain, so this only works called from code actually running on/against the deployed app's own origin.

Do not place secrets, emails, raw identifiers, payment data, or sensitive URL query values in `event_data`, `request_query`, URLs, or referrers. The endpoint stores application-provided event data; see [telemetry-hygiene.md](telemetry-hygiene.md).

## What belongs here vs. other pillars

- Page views, custom product events ("signed up", "clicked upgrade"), scroll/engagement/Web Vitals — all analytics.
- Session replay is bundled under analytics (`SessionRecordingProvider` in the React SDK), separate from error-tracking's replay-on-error integration — the two are independent capture paths even though both are called "replay."
- Don't route business events through OTEL logs or traces — analytics has its own dashboard, retention, and ClickHouse fan-out tuned for high-volume event data; OTEL logs/traces are not.

## Session replay acceptance criteria

Do not enable replay until all of these hold:

- Recording is off until the required consent exists.
- `maskAllInputs` is enabled.
- Authentication, payment, medical, account, admin, and secrets-management routes/elements are blocked or masked.
- The recorder that is actually mounted responds to consent changes; a separate control-hook state is not sufficient evidence.
- A replay containing synthetic sensitive values has been inspected in Temps and the values are absent.

Do not treat the current standalone `useSessionRecordingControl` hook as the consent boundary: it owns state independently from the mounted recorder. Consent must mount/start and stop/unmount the actual recording provider until the SDK exposes one shared control path.

## Gotchas

- **Browser events not appearing**: check the Network tab for calls to `/api/_temps/event` first — if the request never fires, it's a client wiring issue (provider not mounted, `basePath` misconfigured), not a server-side ingestion problem. If it fires but 404s, the request's Host header doesn't match a routed deployment.
- **Server-side events silently rejected**: `/api/projects/{project_id}/events/ingest` requires real auth with `AnalyticsWrite` permission — a missing/expired token, or a token scoped to a different project, returns an auth error, unlike the public beacon endpoint.
- **ClickHouse-backed views lagging**: events land in the Postgres outbox immediately but ClickHouse fan-out is asynchronous via a background worker — a just-fired event may take a few seconds to appear in ClickHouse-backed dashboard views even though it's already durably recorded.
- **Public event totals disagree with revenue/orders**: expected if the browser endpoint was blocked, retried, or forged. Use authenticated backend events as the authoritative source.
- **Consent UI says replay is off but events still arrive**: verify the mounted recorder and the consent control share the same state; do not rely on UI state alone.
