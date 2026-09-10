# OTEL Logs

Temps ingests standard OTLP/HTTP (protobuf) log exports. Use a standard OpenTelemetry logs SDK/exporter (or a logging bridge that emits OTLP logs); there is no Temps-specific logging SDK.

## Endpoints

Routes are registered as `/otel/v1/logs` inside the crate, but the console server nests every plugin's routes under `/api`, so the real externally-reachable paths are:

- Header-based: `POST /api/otel/v1/logs`
- Path-based: `POST /api/otel/v1/{project_id}/{environment_id}/{deployment_id}/logs`

Same auth, rate limit, and opt-in quota model as traces/metrics — see the shared-facts section in the top-level SKILL.md.

Implemented in `temps-otel` crate: `src/handlers/ingest_handler.rs` (log export handling).

## Payload

Standard `ExportLogsServiceRequest` protobuf: `SeverityNumber`, `Body`, `Attributes`, `Timestamp`, and optionally `TraceId`/`SpanId`.

**Always set `trace_id`/`span_id` on log records emitted from within a traced request** — this is what lets the Temps dashboard join a log line to the trace it happened during. Without it, the log is stored but orphaned from any trace context.

Application stdout/stderr is captured as container logs. Use OTLP logs when structured severity, attributes, and trace correlation are required; do not write production logs only to container-local files.

## Sensitive data and cardinality

Temps stores log bodies and attributes supplied by the exporter. Apply [hygiene.md](hygiene.md):

- Never log authorization/cookie headers, passwords, tokens, private keys, connection strings, payment data, raw bodies, or unredacted GenAI prompts/completions.
- Prefer structured allowlisted fields over serializing entire request/user objects.
- Keep attribute keys stable and values bounded. Do not turn user/session/request IDs, UUIDs, emails, raw queries, or stack traces into indexed dimensions.
- Redact in the logger/OTel processor before export, then verify with synthetic canary secrets.

## Storage and retention

- WARN/ERROR/FATAL-severity logs are stored in the primary time-series store (TimescaleDB/ClickHouse depending on backend config) for querying in the dashboard.
- All logs (including INFO/DEBUG) are optionally archived to S3 if `TEMPS_OTEL_S3_BUCKET` and credentials are configured — this is opt-in, not default.

## Gotchas

- **Low-severity logs not visible in dashboard queries**: only WARN+ severity is guaranteed to be in the queryable store by default; INFO/DEBUG logs may only exist in S3 archive if configured, otherwise they may not be retained at all. Don't rely on DEBUG-level logs being queryable in the dashboard unless S3 archival is set up.
- **Logs not correlating with traces**: verify `trace_id`/`span_id` are actually populated on the log record — most OTel logging bridges only auto-populate these when the log call happens inside an active span context (e.g., inside a request handler wrapped by the tracing middleware), not from a background job with no active span.
- **Use logs for structured record-keeping, not exception capture** — an uncaught exception belongs in error tracking (see [error-tracking.md](error-tracking.md)), not as an ERROR-level log line; you lose stack-trace grouping, source maps, and error-group deduplication if you only log it.
- **Shutdown loses the last logs**: batch log processors buffer records. Flush/shut down the provider during `SIGTERM` within Temps' runtime deadline.
