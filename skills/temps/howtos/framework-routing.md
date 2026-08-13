# Framework routing

Temps error ingestion is Sentry-compatible and server telemetry uses standard
OpenTelemetry. Prefer official SDK documentation and installed first-party
skills for the detected platform; substitute the Temps DSN or OTLP destination
without inventing APIs.

| Platform evidence | Error SDK | Analytics/tracing direction |
|---|---|---|
| Vue / Nuxt | `@sentry/vue` | Temps browser analytics or framework wrapper; server OTEL for Nuxt server code |
| Svelte / SvelteKit | `@sentry/sveltekit` | Temps Svelte analytics; server OTEL for hooks/server routes |
| Angular | `@sentry/angular` | Temps browser analytics; do not expose OTLP headers |
| Go | `github.com/getsentry/sentry-go` | Official OpenTelemetry Go SDK |
| Ruby / Rails | `sentry-ruby`, `sentry-rails` | Official OpenTelemetry Ruby SDK |
| Java / Spring | Sentry Spring Boot starter | OpenTelemetry Java agent or SDK |
| PHP | `sentry/sentry` | Official OpenTelemetry PHP SDK where supported |
| .NET | `Sentry.AspNetCore` | Official OpenTelemetry .NET SDK |
| React Native / Flutter | Official Sentry mobile SDK | Do not reuse browser `/api/_temps` assumptions without mobile support evidence |

For error tracking, consult the sibling
[`add-error-tracking` skill](../../add-error-tracking/SKILL.md). For every
platform, configure secrets outside source, redact sensitive data, bound
cardinality, exclude health noise, and prove a synthetic signal reaches the
correct Temps project.

If Temps lacks a supported analytics SDK for the detected framework, say so
clearly and offer a minimal browser SDK integration only when the application
runs in a browser. Do not claim an integration exists based on analogy.
