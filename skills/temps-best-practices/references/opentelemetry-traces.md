# OpenTelemetry Traces

Temps ingests standard OTLP/HTTP (protobuf) trace exports — any language's OpenTelemetry SDK works unmodified, there is no Temps-specific tracing SDK. Point the standard OTLP exporter env vars at Temps instead of hand-writing requests.

## Endpoints

- Header-based: `POST /otel/v1/traces` — project/environment/deployment resolved from the auth token
- Path-based: `POST /otel/v1/{project_id}/{environment_id}/{deployment_id}/traces` — explicit scoping in the URL

Both accept `application/x-protobuf` bodies (`ExportTraceServiceRequest`), with optional gzip or zstd `Content-Encoding`.

Implemented in `temps-otel` crate: `src/handlers/ingest_handler.rs`.

## Standard OTLP exporter config

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=https://<temps-host>/otel/v1
OTEL_EXPORTER_OTLP_HEADERS=Authorization=Bearer%20<dt_or_tk_token>
```

For `tk_` tokens (not project-scoped by default), also set:

```bash
OTEL_EXPORTER_OTLP_HEADERS=Authorization=Bearer%20<tk_token>,X-Temps-Project-Id=<project_id>
```

`dt_` (deployment token) and `si_` tokens are already project/environment/deployment-scoped and don't need the project-id header.

## Auth

- `Authorization: Bearer <token>` (percent-encoded `Bearer%20<token>` is also accepted — some OTLP exporters encode headers that way)
- `X-Temps-Api-Key: <token>` as an alternative to the `Authorization` header
- `X-Temps-Project-Id: <id>` — required alongside `tk_` tokens, optional/ignored for `dt_`/`si_`

## Rate limits and quota

- 1000 requests / 60s per token by default (`TEMPS_OTEL_RATE_LIMIT`, `TEMPS_OTEL_RATE_LIMIT_WINDOW_SECS`), enforced before protobuf parsing — 429 on exceed.
- Storage quota is opt-in and off by default (`TEMPS_OTEL_QUOTA_GB`); when enabled, over-quota ingestion returns 413.
- Decompression bomb protection caps decompressed payload size; oversized payloads return 400.

## Storage

- Spans land in a hypertable (TimescaleDB) segmented by `trace_id`; ClickHouse is used for some analytics reads (see project history: `otel_spans compression fix PR #348`).
- Always query traces with a time bound — the hypertable lookups are unbounded by default if you don't pass one, which is expensive at scale.

## Critical gotcha: trace duration units

`duration_ms` on a `SpanRecord` is the **only** field guaranteed to be milliseconds. Everything else in a span's `attributes` map carries whatever unit the instrumenting library reported:

- `gen_ai.*` semantic-convention attributes (LLM call attributes) are commonly in **seconds**, not ms.
- Other attributes may be nanoseconds or microseconds depending on the source library.

This bit the AI trace-summarization feature (see `crates/temps-otel/src/handlers/query_handler.rs`, `crates/temps-otel/src/types.rs`) — it treated raw `gen_ai.*` attribute values as milliseconds and produced wildly inflated duration claims. When computing or displaying a duration from a span, always use `duration_ms` directly rather than reading a raw attribute unless you've confirmed that attribute's unit from the emitting SDK's semantic conventions.

## Gotchas

- **401s**: check token prefix matches what the endpoint expects and the header is well-formed (`Bearer <token>`, not just the raw token).
- **429s under load testing**: expected past 1000 req/60s per token — use multiple tokens or reduce export frequency (batch spans) rather than treating it as a bug.
- **Spans not appearing**: confirm `Content-Type: application/x-protobuf` — Temps does not accept OTLP/JSON on these endpoints.
