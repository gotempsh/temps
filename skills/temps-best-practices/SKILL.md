---
name: temps-best-practices
description: |
  Best-practices reference for preparing and instrumenting applications on Temps. Covers the app runtime contract (`.temps.yaml` health, HOST/PORT, readiness, SIGTERM, replicas, migrations) and production observability (errors, traces, metrics, logs, analytics, privacy, sampling, cardinality, credential boundaries). Use whenever building or reviewing an app for Temps, configuring health checks, adding telemetry, diagnosing missing/noisy signals, or checking OTLP/Sentry/analytics ingestion. Triggers include "temps best practices", "prepare this app for Temps", ".temps.yaml health", "temps health check", "ignore health checks in otel", "temps observability", "wire up telemetry", and "instrument this app for temps". Prefer focused setup skills for a single SDK. Use temps-cli for executing deploy and resource-management commands.
---

# Temps Best Practices

Best practices for application code that runs on Temps. This skill owns the **runtime contract** and **production observability**; use `temps-cli` for platform operations.

## Required workflow

1. For any deployable application, read [references/runtime-contract.md](references/runtime-contract.md).
2. When any telemetry or replay is enabled, read [references/telemetry-hygiene.md](references/telemetry-hygiene.md).
3. Read only the reference for each observability pillar in scope.
4. Determine the deployment source. For repository builds, inspect and merge `.temps.yaml`; for image/static deployments, inspect the deployment health-path override because no repository config is available.
5. Run the runtime and telemetry verification checklists before considering the work complete.

## Runtime contract summary

Every web application should expose a dedicated health endpoint. Repository builds configure it in `.temps.yaml` under the project's effective Temps Root Directory / Docker build context:

```yaml
health:
  path: /healthz
```

Use `health.path`; do not rely on the currently parsed-but-unapplied `status`, `interval`, `timeout`, or `retries` fields. If OpenTelemetry server tracing is enabled, exclude the exact health path from incoming spans and routine access-log/request-metric noise. Keep the route, `.temps.yaml`, and filters synchronized.

Image and static deployments cannot read `.temps.yaml`; set the same route through their deployment `health_check_path` / CLI `--health-check-path` override instead.

Also require the app to read `PORT`, bind to `HOST`/`0.0.0.0`, align a custom image's `EXPOSE`, handle `SIGTERM`, flush telemetry, and exit inside Temps' 10-second shutdown window. See the runtime reference for readiness semantics, scale-to-zero caveats, replicas, migrations, stdout/stderr, and cron authentication.

## Observability

Temps replaces Sentry + Datadog/Honeycomb + PostHog with one ingestion surface. This section is the map across all five pillars — use it to decide *which* pillar a signal belongs in, and to find the concrete endpoint/auth/gotcha details for each. It complements, not replaces, the narrower setup skills:

- **add-error-tracking** — step-by-step Sentry SDK init per language/framework
- **add-react-analytics** — step-by-step `@temps-sdk/react-analytics` hook usage

## Quickstart: wire up any app end to end

Goal: an app in any language gets runtime health, error tracking, and traces working safely.

**Temps auto-injects exporter destinations and credentials; it does not install or initialize an SDK.** Apps deployed through Temps receive these variables at Docker build time and container runtime, but application instrumentation still has to be installed, initialized, filtered, and verified:

| Env var | What it's for | Client-safe? |
|---|---|---:|
| `SENTRY_DSN` | Server-side/generic error-tracking DSN | Server by default |
| `NEXT_PUBLIC_SENTRY_DSN` / `NUXT_PUBLIC_SENTRY_DSN` / `VITE_SENTRY_DSN` / `PUBLIC_SENTRY_DSN` / `REACT_APP_SENTRY_DSN` | Framework-specific public Sentry write key | Yes |
| `SENTRY_RELEASE` | Commit SHA; leave `release` unset in `Sentry.init()` so the SDK reads it | Server/build metadata |
| `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_PROTOCOL` | OTLP destination and `http/protobuf` protocol | Endpoint is non-secret |
| `OTEL_EXPORTER_OTLP_HEADERS` | Deployment-token authorization for OTLP | **No — server only** |
| `OTEL_SERVICE_NAME`, `OTEL_SERVICE_VERSION` | Stable service and release metadata | Non-secret |

Build-time availability does not make a server credential browser-safe. Never expose `OTEL_EXPORTER_OTLP_HEADERS`, `TEMPS_API_TOKEN`, `dt_`, or `tk_` through a public-prefixed variable or client bundle. Browser/mobile OTLP must use an authenticated backend or collector; see [references/telemetry-hygiene.md](references/telemetry-hygiene.md).

**This auto-injection is scoped to apps deployed *through* Temps.** It does not apply to: an app running elsewhere and only sending telemetry to a self-hosted Temps instance, or to Temps' own console/dashboard (which is built by its own CI, not through this deploy pipeline — self-referential, since Temps doesn't deploy itself as a project on itself). For those cases, get the real credential from the dashboard and wire it through whatever env var / build-arg / secrets mechanism that project's *own* build system already uses:
- Error tracking DSN: **Error Tracking → DSN & Setup** (`https://<public_key>@<temps-host>/<project_id>`)
- A server-side deployment token (`dt_...`) or API key (`tk_...`) for OTLP: **Project Settings → API Keys**

Never hardcode a token or endpoint into source, a Dockerfile, or CI config. Do not ask the user for a token so it can be committed. A Sentry DSN public key is designed for client writes, but should still come from the platform's supported configuration path.

Either way, the app-side steps are the same:

1. **Runtime**: apply [references/runtime-contract.md](references/runtime-contract.md).
2. **Hygiene**: establish credential boundaries, redaction, cardinality, batching, sampling, and shutdown using [references/telemetry-hygiene.md](references/telemetry-hygiene.md).
3. **Error tracking**: follow [add-error-tracking](../add-error-tracking/SKILL.md), leaving release metadata environment-driven.
4. **Traces**: install and initialize the official OpenTelemetry SDK/agent using [references/opentelemetry-traces.md](references/opentelemetry-traces.md). Auto-injected env vars only configure export.
5. **Verify** runtime and telemetry behavior before considering the app production-ready.

## The five pillars

| Pillar | What it's for | Reference |
|---|---|---|
| Error tracking | Uncaught exceptions, handled errors, stack traces, source maps | [references/error-tracking.md](references/error-tracking.md) |
| Traces (OTLP) | Distributed request spans, AI/gen_ai call chains, latency breakdowns | [references/opentelemetry-traces.md](references/opentelemetry-traces.md) |
| Metrics (OTLP) | Counters/gauges/histograms — request rates, queue depth, custom business metrics | [references/metrics.md](references/metrics.md) |
| Logs (OTLP) | Structured log records correlated to traces | [references/logs.md](references/logs.md) |
| Analytics | Page views, custom product events, session replay, Web Vitals | [references/analytics.md](references/analytics.md) |

Read the specific reference file(s) for the pillar(s) in play — don't load all five unless doing a full-stack instrumentation pass.

Cross-pillar production rules live in [references/telemetry-hygiene.md](references/telemetry-hygiene.md), not duplicated in every pillar.

## Deciding which pillar a signal belongs in

- **"This threw/crashed"** → error tracking, not a log line. Logs are for structured record-keeping, not exception capture.
- **"How long did this request/DB call/LLM call take, and what did it call downstream?"** → traces. If it's a single number you want to alert on or chart over time (not per-request), that's a metric instead.
- **"I want to count/aggregate something over time"** (requests/sec, cache hit rate, custom business counter) → metrics, not traces. Don't create a span just to record a number.
- **"A user did X in the product"** → analytics event, not a log or a trace attribute.
- **"I need to debug what happened at a point in time, correlated to a trace"** → OTEL logs with `trace_id`/`span_id` attributes set, so they join with the trace in the dashboard.

## Shared ingestion facts across pillars

Temps OTLP ingestion shares a rate-limit/quota model, but token support differs by signal:

- **`tk_` API key**: server-side; needs `X-Temps-Project-Id`.
- **`dt_` deployment token**: server-side and project/environment scoped. Prefer the header-based endpoint so Temps resolves attribution from the token. Do not expose it to a browser.
- **`si_` integration token**: supported for infrastructure metrics ingestion, not general traces/logs.
- **Sentry-compatible ingestion**: uses the DSN public key rather than these machine tokens.
- **Auth header**: `Authorization: Bearer <token>` or `X-Temps-Api-Key: <token>`. Some OTLP exporters URL-encode the header as `Bearer%20<token>` — Temps accepts that too.
- **Rate limit**: 1000 req/60s per token by default (`TEMPS_OTEL_RATE_LIMIT`, `TEMPS_OTEL_RATE_LIMIT_WINDOW_SECS` — server-side config, not something the app sets).
- **Storage quota**: off by default; a self-hosted instance can opt in via `TEMPS_OTEL_QUOTA_GB`. If ingestion suddenly starts 413'ing, that's the likely cause.
- **Endpoint shape**: prefer header-based `POST /api/otel/v1/{traces|metrics|logs}` with attribution resolved from the token. Path-based endpoints exist, but should not be used to override a deployment token's intended deployment attribution.

## Definition of done

1. Runtime: app listens on the injected port/interface; the health route is configured through `.temps.yaml` for repository builds or the deployment override for image/static deploys; `SIGTERM` drains and exits within 10 seconds.
2. Health noise: repeated health requests succeed without routine server spans, access logs, or request metrics; a normal route remains observable.
3. Error tracking: a deliberate test error appears in **Error Tracking → Error Groups** with the expected release and no sensitive data.
4. Traces: a normal request appears in **Observe → Traces**, uses a route-template name, propagates context downstream, and has a sane `duration_ms`.
5. Metrics: names and bounded labels pass validation; no user/session/request identifiers create cardinality explosions.
6. Logs: a structured log inside an active span joins the correct trace and contains no secrets.
7. Analytics: browser events are treated as untrusted; an authoritative test conversion comes from authenticated server code.
8. Replay: recording is consent-gated, masked, path-restricted, and inspected with synthetic sensitive values.
9. Client bundle: built assets contain no `dt_`, `tk_`, `TEMPS_API_TOKEN`, or OTLP authorization header.

If a signal never appears, check whether the SDK was actually initialized, then endpoint/protocol, token type/header, rate limit, and quota.

## Everything else

Beyond the runtime health contract and observability guidance above, deployment, service/database provisioning, environment variables, domains, monitoring config, backups, and CI/CD automation are all reached through the Temps CLI (`bunx @temps-sdk/cli`). Use the **temps-cli** skill for those operations.
