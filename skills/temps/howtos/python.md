# Python observability

Detect the existing environment manager from `pyproject.toml`, lockfiles, or
`requirements.txt`. Preserve it and request approval before adding packages.

## Error tracking

Use the official `sentry-sdk` package against the Temps Sentry-compatible DSN:

```python
import os
import sentry_sdk

sentry_sdk.init(
    dsn=os.environ["SENTRY_DSN"],
    environment=os.environ.get("ENVIRONMENT", "production"),
)
```

Enable the detected Django, Flask, or FastAPI integration rather than manually
wrapping every handler. Keep the DSN in the deployment environment, not source.

## OpenTelemetry

Read [../references/observability/tracing.md](../references/observability/tracing.md)
and use the official OpenTelemetry Python SDK/exporter plus the instrumentation
package for the detected framework. Read OTLP endpoint and authorization from
the injected server environment. Use route templates for span names, exclude
the exact health endpoint, and flush providers during shutdown.

Browser analytics belongs in the frontend application; do not manufacture
browser page views from the Python server. Emit authoritative business events
from authenticated server paths only when the analytics API supports the
required event contract.

## Verification

- Run the project's existing formatter/typecheck/test commands.
- Capture a handled synthetic exception.
- Exercise a real framework route and locate the corresponding server span.
- Confirm no request body, cookie, authorization header, or high-cardinality
  user identifier was exported.
