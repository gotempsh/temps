---
name: temps-best-practices
description: |
  Best-practices reference for building apps on Temps — currently covers observability (error tracking, traces, metrics, logs, analytics), with room to grow into other pillars later. Use to: design how an app should emit telemetry to Temps, debug why errors/traces/metrics/logs/events aren't showing up, choose which pillar a signal belongs in, wire up multiple pillars at once, or check ingestion endpoints/auth tokens (tk_/dt_/si_)/rate limits/quotas for Temps' OTLP/Sentry/analytics APIs. Triggers: "temps best practices", "temps observability", "wire up telemetry", "why isn't this showing up in traces/metrics/logs", "otel best practices for temps", "instrument this app for temps". For single-pillar setup prefer add-error-tracking or add-react-analytics. For anything outside observability (deploy, services/databases, domains, CI automation), use the temps-cli skill instead.
---

# Temps Best Practices

Best practices for building on Temps. Today this covers the **observability** pillar end to end (error tracking, traces, metrics, logs, analytics) — everything else an app needs from the platform (deploy, provisioning databases/caches, domains, CI/CD automation, notifications) is CLI-driven and covered by the **temps-cli** skill, not duplicated here.

## Observability

Temps replaces Sentry + Datadog/Honeycomb + PostHog with one ingestion surface. This section is the map across all five pillars — use it to decide *which* pillar a signal belongs in, and to find the concrete endpoint/auth/gotcha details for each. It complements, not replaces, the narrower setup skills:

- **add-error-tracking** — step-by-step Sentry SDK init per language/framework
- **add-react-analytics** — step-by-step `@temps-sdk/react-analytics` hook usage

## Quickstart: wire up any app end to end

Goal: an app in any language gets error tracking and traces flowing with minimal setup. Both signals use the same shape — one env var pointing at Temps, and the language's own official SDK does the rest.

**If the app is deployed on Temps, most of this is already done.** Every deployment automatically gets these env vars injected — no manual DSN copy-paste or token minting required:

| Env var | What it's for |
|---|---|
| `SENTRY_DSN` | Server-side/generic error tracking DSN, always present |
| `NEXT_PUBLIC_SENTRY_DSN` / `NUXT_PUBLIC_SENTRY_DSN` / `VITE_SENTRY_DSN` / `PUBLIC_SENTRY_DSN` / `REACT_APP_SENTRY_DSN` | Framework-specific public DSN, added when the detected build preset needs a public-prefixed var to expose it client-side (Next.js/Nuxt/Vite,React,Vue,SolidStart,Remix/SvelteKit,Astro,Rsbuild/Docusaurus respectively) — not added for Angular or backend/generic presets, which just use `SENTRY_DSN` |
| `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_PROTOCOL` (always `http/protobuf`), `OTEL_EXPORTER_OTLP_HEADERS` (deployment token auth) | Traces, auto-configured |
| `OTEL_SERVICE_NAME` (project name), `OTEL_SERVICE_VERSION` (commit SHA when available) | Auto-populated span metadata |

Implemented in `temps-deployments` crate: `src/services/workflow_planner.rs` (`gather_environment_variables`). If any of these are missing on a running deployment, that's a platform bug, not something to work around by hand-rolling credentials.

For an app **not** deployed on Temps (running elsewhere, only sending telemetry to a self-hosted Temps instance), get credentials manually:
- Error tracking DSN: **Error Tracking → DSN & Setup** (`https://<public_key>@<temps-host>/<project_id>`)
- A deployment token (`dt_...`) or API key (`tk_...`) for OTLP: **Project Settings → API Keys**

Either way, the app-side steps are the same:

1. **Error tracking**: follow [add-error-tracking](../add-error-tracking/SKILL.md) for the app's language — install the official Sentry SDK, point it at `SENTRY_DSN` (or the framework's public variant). No Temps-specific package exists or is needed.
2. **Traces**: follow the per-language quickstart in [references/opentelemetry-traces.md](references/opentelemetry-traces.md) — install the official OpenTelemetry SDK/agent for the language; if the three `OTEL_EXPORTER_OTLP_*` env vars are already set, most SDKs pick them up with zero additional config.
3. **Verify** both landed using the checklist below before considering the app "instrumented."

## The five pillars

| Pillar | What it's for | Reference |
|---|---|---|
| Error tracking | Uncaught exceptions, handled errors, stack traces, source maps | [references/error-tracking.md](references/error-tracking.md) |
| Traces (OTLP) | Distributed request spans, AI/gen_ai call chains, latency breakdowns | [references/opentelemetry-traces.md](references/opentelemetry-traces.md) |
| Metrics (OTLP) | Counters/gauges/histograms — request rates, queue depth, custom business metrics | [references/metrics.md](references/metrics.md) |
| Logs (OTLP) | Structured log records correlated to traces | [references/logs.md](references/logs.md) |
| Analytics | Page views, custom product events, session replay, Web Vitals | [references/analytics.md](references/analytics.md) |

Read the specific reference file(s) for the pillar(s) in play — don't load all five unless doing a full-stack instrumentation pass.

## Deciding which pillar a signal belongs in

- **"This threw/crashed"** → error tracking, not a log line. Logs are for structured record-keeping, not exception capture.
- **"How long did this request/DB call/LLM call take, and what did it call downstream?"** → traces. If it's a single number you want to alert on or chart over time (not per-request), that's a metric instead.
- **"I want to count/aggregate something over time"** (requests/sec, cache hit rate, custom business counter) → metrics, not traces. Don't create a span just to record a number.
- **"A user did X in the product"** → analytics event, not a log or a trace attribute.
- **"I need to debug what happened at a point in time, correlated to a trace"** → OTEL logs with `trace_id`/`span_id` attributes set, so they join with the trace in the dashboard.

## Shared ingestion facts across pillars

All Temps telemetry ingestion (OTLP traces/metrics/logs) shares one auth and rate-limit model — know this once instead of re-deriving it per pillar:

- **Token prefixes**: `tk_` (API key, needs `X-Temps-Project-Id` header), `dt_` (deployment token, project-scoped already), `si_` (webhook/integration token). Sentry-compatible error ingestion uses the DSN's public key instead, not these tokens.
- **Auth header**: `Authorization: Bearer <token>` or `X-Temps-Api-Key: <token>`. Some OTLP exporters URL-encode the header as `Bearer%20<token>` — Temps accepts that too.
- **Rate limit**: 1000 req/60s per token by default (`TEMPS_OTEL_RATE_LIMIT`, `TEMPS_OTEL_RATE_LIMIT_WINDOW_SECS` — server-side config, not something the app sets).
- **Storage quota**: off by default; a self-hosted instance can opt in via `TEMPS_OTEL_QUOTA_GB`. If ingestion suddenly starts 413'ing, that's the likely cause.
- **Endpoint shape**: path-based `POST /otel/v1/{project_id}/{environment_id}/{deployment_id}/{traces|metrics|logs}` or header-based `POST /otel/v1/{traces|metrics|logs}` with project/env/deployment resolved from the token. Prefer the standard OTLP exporter env vars (`OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_HEADERS`) over hand-rolling requests — any OTLP/HTTP protobuf exporter works unmodified against these endpoints.

## Verification checklist for a full instrumentation pass

1. Error tracking: trigger a deliberate error, confirm it appears in **Error Tracking → Error Groups**.
2. Traces: make a request through the instrumented path, confirm a span appears in **Observe → Traces** with a sane `duration_ms` (see the unit gotcha in [references/opentelemetry-traces.md](references/opentelemetry-traces.md)).
3. Metrics: confirm a custom metric name passes the `[a-zA-Z0-9_.:- ]` character set (see [references/metrics.md](references/metrics.md)) and shows up in **Observe → Metrics**.
4. Logs: confirm log records with `trace_id` set join the correct trace in the UI.
5. Analytics: confirm `/_temps/event` or `/projects/{id}/events/ingest` calls land in **Analytics** within a few seconds.

If a signal never appears, check auth token prefix/header first, then rate limit/quota, before assuming the SDK integration is broken — most "nothing shows up" cases are one of those two.

## Everything else

Deployment, service/database provisioning, environment variables, domains, monitoring config, backups, and CI/CD automation are all reached through the Temps CLI (`bunx @temps-sdk/cli`). Use the **temps-cli** skill for those — this skill only owns the observability pillars above.
