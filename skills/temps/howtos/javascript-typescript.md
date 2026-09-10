# JavaScript and TypeScript observability

Use the repository's package manager and preserve its lockfile. Explain and get
approval before installing dependencies.

## Analytics

For React, Next.js, Remix, Vite, or CRA, use the reviewed
`@temps-sdk/react-analytics` integration documented in the sibling
[`add-react-analytics` skill](../../add-react-analytics/SKILL.md). Important
constraints:

- Apps deployed on Temps normally use the default same-origin `/api/_temps`
  base path.
- `ignoreLocalhost` defaults to `true`; explicitly disable it only during local
  verification.
- In Next.js App Router, import `TempsAnalyticsProvider` directly into the root
  layout; the package already carries its client boundary.
- Session recording is opt-in and requires consent, masking, exclusions, and a
  sampling decision.

For vanilla browser applications, read
[../references/observability/analytics.md](../references/observability/analytics.md)
and use `@temps-sdk/analytics-browser`.

## Error tracking

Temps accepts the Sentry protocol. Read the sibling
[`add-error-tracking` skill](../../add-error-tracking/SKILL.md) and use the
official Sentry SDK for the detected framework with a Temps DSN.

- Keep server DSNs in `SENTRY_DSN`.
- Use the documented public-prefixed DSN variable for browser frameworks.
- For browser SDKs, use the injected same-origin Temps tunnel when available.
- Do not enable Sentry replay or performance uploads merely because the SDK
  offers them; use Temps analytics/replay and OpenTelemetry tracing instead.

## Server OpenTelemetry

Read [../references/observability/tracing.md](../references/observability/tracing.md).
Configure the official OpenTelemetry Node SDK before application imports. Temps
deployments provide server-side `OTEL_EXPORTER_OTLP_ENDPOINT`,
`OTEL_EXPORTER_OTLP_PROTOCOL`, `OTEL_EXPORTER_OTLP_HEADERS`,
`OTEL_SERVICE_NAME`, and `OTEL_SERVICE_VERSION`.

Never expose `OTEL_EXPORTER_OTLP_HEADERS` through `NEXT_PUBLIC_`, `VITE_`,
`PUBLIC_`, or `REACT_APP_` variables. Browser OTLP must go through a trusted
backend or collector.

## Verification

- Run the repository's existing typecheck/build command.
- Inspect browser Network requests for `/api/_temps/event`.
- Capture one handled synthetic exception.
- Exercise one server request and locate its trace.
- Search built client assets for `dt_`, `tk_`, `TEMPS_API_TOKEN`, and OTLP
  authorization headers before reporting success.
