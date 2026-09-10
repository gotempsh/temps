# Rust observability

Preserve the workspace's dependency conventions and feature flags. Explain
changes to `Cargo.toml` and obtain approval before adding crates.

## Error tracking

Use the official `sentry` crate with `SENTRY_DSN` from the environment. Install
the framework integration when one exists and keep release/environment metadata
environment-driven. Do not attach secrets, request bodies, or unrestricted
user-provided values to events.

## OpenTelemetry

Read [../references/observability/tracing.md](../references/observability/tracing.md).
Use `tracing`, `tracing-subscriber`, `tracing-opentelemetry`, and the official
OTLP exporter versions compatible with the workspace. Initialize the subscriber
before serving requests, use route-template span names, exclude the health
route, and shut the tracer provider down during graceful termination.

Read `OTEL_EXPORTER_OTLP_ENDPOINT`, protocol, headers, service name, and service
version from the server environment. Never compile authorization headers into
the binary.

## Verification

- Run `cargo check --lib` and the affected crate tests.
- Capture a handled synthetic error without panicking the service.
- Exercise one HTTP route and locate its trace and child operations.
- Confirm shutdown flush completes inside the application's termination budget.
