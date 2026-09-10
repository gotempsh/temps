# Telemetry Hygiene and Production Guardrails

Read this file whenever adding or reviewing error tracking, traces, metrics, OTLP logs, analytics, or session replay. Temps stores what applications send; filtering sensitive or unbounded data is primarily the application's responsibility.

## Credential and trust boundaries

Classify telemetry credentials before wiring an exporter:

| Value | Browser-safe? | Rule |
|---|---:|---|
| Sentry-compatible DSN public key | Yes | A client-write identifier, not a control-plane credential; still configure it through the platform instead of hardcoding it |
| Public `/api/_temps/event` analytics endpoint | Yes, but untrusted | Host-based attribution is not proof that an event came from a genuine user |
| `dt_` deployment token | No | Permanent machine credential; server-side only |
| `tk_` API key | No | Server-side only |
| `OTEL_EXPORTER_OTLP_HEADERS` / `TEMPS_API_TOKEN` | No | Contains a live bearer token; never expose to client code |

Build-time availability does not make a server credential browser-safe. Never:

- Give `dt_`, `tk_`, `TEMPS_API_TOKEN`, or `OTEL_EXPORTER_OTLP_HEADERS` a `NEXT_PUBLIC_`, `VITE_`, `PUBLIC_`, or similar prefix.
- Read those values from a client component or bundle.
- Put them in browser/mobile OTLP exporter configuration.

Browser or mobile OTLP must go through an application backend or collector that adds authentication server-side. Until Temps offers a restricted public OTLP write key and browser CORS support, direct client OTLP is unsupported.

## Sensitive-data policy

Redact before export. Temps does not provide a universal server-side scrubber for arbitrary telemetry payloads.

Do not send:

- Authorization, cookie, API-key, or session headers.
- Passwords, access/refresh tokens, private keys, or connection strings.
- Raw request/response bodies or database parameter values by default.
- Payment, medical, authentication, or other regulated data.
- Email addresses, full IP addresses, or stable user identifiers unless the product has an explicit lawful need and retention policy.
- Raw GenAI prompts/completions unless users knowingly opted in and a redaction policy is enforced.
- Sensitive URL query parameters or fragments.

Prefer allowlisting safe attributes over maintaining an ever-growing denylist. Use SDK processors/hooks such as Sentry `beforeSend`, OTel span/log processors, and analytics payload builders. Keep legacy Sentry `sendDefaultPii` false/undefined or use the current granular `dataCollection` allow/deny settings.

Test redaction with synthetic canary values that look like tokens, emails, and card numbers, then inspect the actual stored event/span/log/replay.

## Session replay

Session replay is the highest-risk signal because the backend stores captured rrweb events. Treat it as a separate privacy feature:

- Keep recording disabled by default.
- Obtain the required consent before mounting or starting the recorder.
- Use `maskAllInputs: true`.
- Block or mask authentication, payment, medical, account, admin, and secrets-management UI.
- Exclude sensitive paths explicitly.
- Verify the active recorder actually responds to the consent state; do not claim compliance from a disconnected control hook.
- Inspect a replay made with synthetic sensitive values before production rollout.

Do not use the current standalone `useSessionRecordingControl` hook as proof that recording stopped: its state is independent from the mounted recorder. Consent must control the actual provider lifecycle until the SDK exposes one shared control path.

## Cardinality and naming

Unbounded dimensions make telemetry expensive and hard to query across every pillar:

- Name server spans with route templates (`GET /users/:id`), not raw paths (`GET /users/123`) or full URLs.
- Never use user IDs, session IDs, request IDs, UUIDs, emails, raw SQL, raw queries, or error messages as metric labels.
- Keep metric label sets small and bounded.
- Keep analytics event names stable (`purchase_completed`); put only bounded dimensions in properties.
- Allowlist span and log attributes, and drop high-cardinality auto-instrumentation fields before export.
- Keep `service.name` stable per service. Put version and environment in their dedicated resource attributes.

## Production volume controls

Use batching in production. Configure sampling deliberately rather than inheriting an SDK's development defaults:

```bash
OTEL_TRACES_SAMPLER=parentbased_traceidratio
OTEL_TRACES_SAMPLER_ARG=0.05
```

Choose the ratio from traffic volume and debugging needs; `0.05` is an example, not a universal default. Preserve parent sampling decisions across services.

Also:

- Exclude health checks, static assets, and other known low-value traffic at the source.
- Disable noisy middleware/socket instrumentations when they do not answer a real operational question.
- Prefer batch span/log processors with bounded queues and export timeouts.
- Call the SDK's shutdown/flush path during `SIGTERM`; otherwise the last buffered telemetry is lost.
- Treat exporter failure as observable but non-fatal to normal request handling.

## Trace propagation and baggage

Distributed traces require the application SDK to extract inbound and inject outbound W3C `traceparent`/`tracestate` headers. Temps configures export destinations; it does not add propagation to application HTTP clients.

Verify propagation across project/service boundaries. Logs emitted inside a request should carry the active `trace_id` and `span_id`. Background jobs should start or continue an explicit span rather than assuming request-local context survives queue boundaries.

Baggage crosses process boundaries and must not carry secrets or PII. Clear or allowlist baggage before calling an untrusted service.

## Analytics is not an authority

The public analytics endpoint is intentionally unauthenticated. Treat browser events as user-controlled input:

- Do not use them as the source of truth for billing, authorization, entitlements, inventory, or authoritative revenue.
- Send conversion-critical events from the authenticated backend endpoint after the server has committed the business action.
- Deduplicate in application business storage or an outbox before posting where retries are possible. Temps analytics ingest does not enforce idempotency from a business-event identifier in `event_data`.

## Hygiene verification

Before considering telemetry production-ready:

1. Search built client assets for `dt_`, `tk_`, `TEMPS_API_TOKEN`, and OTLP authorization headers.
2. Exercise a normal request and verify route-template span naming and trace propagation.
3. Send synthetic sensitive values through error, log, trace, analytics, and replay paths; verify they are absent or masked.
4. Load-test representative traffic and inspect span/metric/event cardinality.
5. Send `SIGTERM` with telemetry buffered and confirm shutdown flushes it within the runtime deadline.
6. Confirm authoritative business events originate from authenticated server code.
