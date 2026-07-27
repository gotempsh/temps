# Analytics

For React apps, use the **add-react-analytics** skill for step-by-step provider/hook setup (`@temps-sdk/react-analytics`, `TempsAnalyticsProvider`, `useTrackEvent`, `useAnalytics`, session recording). This file covers the underlying ingestion API and cross-framework facts.

## Ingestion endpoints

- `POST /_temps/event` — public, unauthenticated, CORS-enabled browser-side event ingestion. This is what the React SDK (and any hand-rolled browser tracker) posts to. Request body: `EventMetricsPayload` JSON (`event_name`, optional `properties`, optional `timestamp`); browser context (IP, User-Agent, session cookie) is extracted server-side from request headers, not sent by the client.
- `POST /projects/{project_id}/events/ingest` — authenticated (session or API key), for server-side/console event submission. Request body: `ConsoleEventPayload` JSON.

Both endpoints funnel through the same write path (`AnalyticsEventsService::record_event`) into an outbox table in Postgres; a background worker (`ChFanoutWorker`) fans events out to ClickHouse if it's configured for the instance.

Implemented in `temps-analytics-events` crate: `src/handlers/events_handler.rs`.

## Auth

- `/_temps/event` is intentionally public/unauthenticated — it's the browser-facing beacon endpoint, protected by project-scoped event validation rather than a bearer token. Don't try to add auth headers to it; that's not how it's gated.
- `/projects/{project_id}/events/ingest` requires a session cookie or API key bearer token — use this for server-side event submission (e.g., a backend job recording a business event), not for browser code.

## What belongs here vs. other pillars

- Page views, custom product events ("signed up", "clicked upgrade"), scroll/engagement/Web Vitals — all analytics.
- Session replay is bundled under analytics (`SessionRecordingProvider` in the React SDK), separate from error-tracking's replay-on-error integration — the two are independent capture paths even though both are called "replay."
- Don't route business events through OTEL logs or traces — analytics has its own dashboard, retention, and ClickHouse fan-out tuned for high-volume event data; OTEL logs/traces are not.

## Gotchas

- **Events not appearing**: check the Network tab for calls to `/_temps/event` first — if the request never fires, it's a client wiring issue (provider not mounted, `basePath` misconfigured), not a server-side ingestion problem.
- **Server-side events silently rejected**: `/projects/{project_id}/events/ingest` requires real auth — a missing/expired session or API key returns an auth error, unlike the public `/_temps/event` beacon.
- **ClickHouse-backed views lagging**: events land in the Postgres outbox immediately but ClickHouse fan-out is asynchronous via a background worker — a just-fired event may take a few seconds to appear in ClickHouse-backed dashboard views even though it's already durably recorded.
