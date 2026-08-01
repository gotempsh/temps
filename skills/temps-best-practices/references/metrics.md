# Metrics

Temps ingests standard OTLP/HTTP (protobuf) metrics exports — counters, gauges, histograms, summaries — through the same ingestion pipeline as traces and logs. Use a standard OpenTelemetry metrics SDK/exporter; there is no Temps-specific metrics SDK.

## Endpoints

Routes are registered as `/otel/v1/metrics` inside the crate, but the console server nests every plugin's routes under `/api`, so the real externally-reachable paths are:

- Header-based: `POST /api/otel/v1/metrics`
- Path-based: `POST /api/otel/v1/{project_id}/{environment_id}/{deployment_id}/metrics`

Metrics use the same header forms (`Authorization: Bearer <token>` or `X-Temps-Api-Key`), rate limit (1000 req/60s/token), and opt-in quota as traces/logs. Token scope differs: `tk_` and `dt_` are server credentials used across application OTLP signals, while `si_` is accepted only for infrastructure metrics and must not be used for traces or logs. See the shared-facts section in the top-level SKILL.md.

Implemented in `temps-otel` crate: `src/handlers/ingest_handler.rs`; validation in `temps-metrics` crate: `src/store/timescale.rs`.

## Metric name validation

Metric names must match `[a-zA-Z0-9_.:- ]` — alphanumeric plus underscore, dot, colon, hyphen (spaces are in the character class but not meaningfully supported). Names that fail this check are logged with a warning and silently dropped at ingest, not stored anywhere.

This is **not** a curated allowlist like Prometheus recording rules — any name matching the pattern is accepted and stored. The "allowlist" language sometimes used for this internally refers to the validation gate, not a fixed list of permitted metric names.

## Label handling

- Max 64 labels per data point (`MAX_LABELS_PER_POINT`).
- Max 1024 bytes per label key or value.
- Any label whose key starts with `temps.` is silently stripped before storage — this namespace is reserved for Temps' own internal routing attributes, so don't rely on setting `temps.*` labels from app code; they will not persist.
- Use a small allowlist of bounded dimensions. Never use user IDs, session IDs, request IDs, UUIDs, emails, raw URLs/queries, or error messages as labels.
- For HTTP metrics, use route templates such as `/users/:id`, not raw request paths.

## Storage split

- Scalar points (counters, gauges) go into a `service_metrics` table.
- Histogram/summary points (multi-valued aggregations) are excluded from `service_metrics` but retained in `otel_metrics` — if a custom histogram metric doesn't appear in a scalar-only view, check the histogram-specific store/dashboard instead.

## Gotchas

- **Metric silently missing, no error**: check the name against the character-set regex first — invalid names fail closed with only a server-side warning log, no error surfaced to the exporter.
- **Custom label not showing up**: confirm it doesn't start with `temps.` — those are dropped by design, not a bug.
- **High-cardinality metric spiking storage**: cardinality isn't capped by a distinct-value allowlist, only by per-point label count/size — a metric with a label like `user_id` can still blow up cardinality. Prefer bucketing or dropping high-cardinality labels client-side before export rather than relying on server-side protection.

See [telemetry-hygiene.md](telemetry-hygiene.md) for cross-pillar naming, sampling, and sensitive-data rules.
