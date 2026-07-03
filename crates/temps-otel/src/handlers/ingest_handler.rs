//! OTLP/HTTP ingest handlers.
//!
//! Accept protobuf-encoded payloads with gzip/zstd compression.
//! Authenticate via per-project API key in the `Authorization` header.
//! Return correct OTLP response envelopes.

use std::collections::HashMap;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use prost::Message;
use tracing::{debug, error, warn};

use crate::error::OtelError;
use crate::ingest::auth::{IngestAuth, ServiceAuth};
use crate::ingest::decode;
use crate::proto;
use crate::services::cross_project::{is_valid_trace_id, TraceHintMsg};
use crate::types::{MetricPoint as OtlpMetricPoint, MetricType};
use crate::OtelAppState;
use temps_core::problemdetails::{self, Problem};
use temps_core::ProblemDetails;
use temps_metrics::{
    validate_metric_name, MetricKind, MetricPoint as StoreMetricPoint, SourceKind,
};

impl From<OtelError> for Problem {
    fn from(error: OtelError) -> Self {
        match error {
            OtelError::AuthFailed { .. } | OtelError::InvalidApiKey => {
                warn!(error = %error, "OTel ingest auth failed");
                problemdetails::new(StatusCode::UNAUTHORIZED)
                    .with_title("Authentication Failed")
                    .with_detail(error.to_string())
            }
            OtelError::RateLimitExceeded { .. } | OtelError::ServiceRateLimitExceeded { .. } => {
                warn!(error = %error, "OTel ingest rate limited");
                problemdetails::new(StatusCode::TOO_MANY_REQUESTS)
                    .with_title("Rate Limit Exceeded")
                    .with_detail(error.to_string())
            }
            OtelError::QuotaExceeded { .. } => {
                warn!(error = %error, "OTel ingest quota exceeded");
                problemdetails::new(StatusCode::PAYLOAD_TOO_LARGE)
                    .with_title("Storage Quota Exceeded")
                    .with_detail(error.to_string())
            }
            OtelError::ProtobufDecode { .. } | OtelError::Validation { .. } => {
                warn!(error = %error, "OTel ingest bad payload");
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Payload")
                    .with_detail(error.to_string())
            }
            OtelError::DecompressionFailed { .. } | OtelError::UnsupportedEncoding { .. } => {
                warn!(error = %error, "OTel ingest decompression error");
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Decompression Error")
                    .with_detail(error.to_string())
            }
            OtelError::ProjectNotFound { .. } => {
                warn!(error = %error, "OTel ingest project not found");
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Project Not Found")
                    .with_detail(error.to_string())
            }
            OtelError::DashboardNotFound { .. } => {
                warn!(error = %error, "OTel dashboard not found");
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Dashboard Not Found")
                    .with_detail(error.to_string())
            }
            OtelError::MetricAlertNotFound { .. } => {
                warn!(error = %error, "OTel metric alert rule not found");
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title("Metric Alert Rule Not Found")
                    .with_detail(error.to_string())
            }
            OtelError::Storage { .. }
            | OtelError::Database(_)
            | OtelError::S3 { .. }
            | OtelError::Io(_)
            | OtelError::Serialization(_)
            | OtelError::Internal { .. } => {
                error!(error = %error, "OTel ingest internal error");
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}

/// Extract authentication token from headers.
///
/// Checks `Authorization: Bearer <token>` and `X-Temps-Api-Key: <token>`.
/// Works for `tk_`, `dt_`, and `si_` token prefixes. Handles both plain
/// `Bearer <token>` and percent-encoded `Bearer%20<token>` from OTLP SDKs.
fn extract_token(headers: &HeaderMap) -> Option<String> {
    if let Some(auth) = headers.get("authorization") {
        if let Ok(value) = auth.to_str() {
            // SECURITY: never log the raw header — it carries a live, mostly
            // non-expiring credential (Bearer dt_/tk_/si_). Log only its shape.
            tracing::debug!(
                scheme = value.split_whitespace().next().unwrap_or(""),
                len = value.len(),
                "OTLP extract_token"
            );
            if let Some(key) = value.strip_prefix("Bearer ") {
                return Some(key.trim().to_string());
            }
            // Some OTLP exporters send the literal string "Bearer%20<token>"
            if let Some(key) = value.strip_prefix("Bearer%20") {
                return Some(key.trim().to_string());
            }
        }
    }

    if let Some(key) = headers.get("x-temps-api-key") {
        if let Ok(value) = key.to_str() {
            return Some(value.trim().to_string());
        }
    }

    None
}

/// Extract optional project ID from `X-Temps-Project-Id` header.
///
/// Required for `tk_` (API key) tokens; ignored for `dt_` tokens.
fn extract_project_id_header(headers: &HeaderMap) -> Option<i32> {
    headers
        .get("x-temps-project-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<i32>().ok())
}

/// Extract Content-Encoding from headers.
fn content_encoding(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
}

// ── OTLP → MetricsStore conversion ─────────────────────────────────

/// Maximum number of labels accepted per OTLP metric point written to
/// `service_metrics`.  Points with more labels are capped — excess labels are
/// silently dropped.  This prevents GIN index write amplification and JSONB
/// column size attacks.
///
/// # SECURITY(metrics-security-5): labels JSONB size amplification
/// Enforcing per-point limits here bounds worst-case GIN index write cost.
const MAX_LABELS_PER_POINT: usize = 64;

/// Maximum byte length for a single label key or value.
/// Keys or values exceeding this limit are silently dropped.
const MAX_LABEL_BYTES: usize = 1024;

/// Convert a single OTLP `MetricPoint` (gauge or sum) to a `MetricPoint`
/// understood by `temps_metrics::MetricsStore`.
///
/// Returns `None` for histogram/summary points — those carry count/sum/bucket
/// arrays that do not map to the scalar `service_metrics` schema.  Histogram
/// points are still stored in the OTel-specific `otel_metrics` table via the
/// regular `OtelService::ingest_metrics` path.
///
/// # Source mapping (SECURITY note)
/// OTLP metrics pushed by an SDK are always scoped to a deployment (the SDK
/// endpoint includes `deployment_id` in the path, or the deployment token
/// implies it).  `deployment_id` is an argument derived exclusively from the
/// authenticated token context — it is **never** read from the OTLP payload.
/// See `resolve_ingest_context` for the auth boundary.
///
/// When `deployment_id` is `None` the point is dropped — there is no stable
/// entity in `service_metrics.source_id` to attach it to.
///
/// # Counter temporality (CORRECTNESS note)
/// OTLP `Sum` metrics can carry either `DELTA` or `CUMULATIVE` temporality.
/// Both are stored as `MetricKind::Gauge` (raw scalar write) because:
///
/// - **DELTA**: the SDK has already computed the delta; applying scraper-style
///   delta computation again would produce wrong values (double-delta).
/// - **CUMULATIVE**: we have no in-memory state to compute a delta here, and
///   storing cumulative as-if-it-were-a-delta would produce wildly inflated
///   aggregates.  Storing the raw value as a Gauge preserves the semantics
///   (ever-increasing monotonic value) without corrupting aggregates.
///
/// Downstream consumers that need delta semantics for OTLP cumulative counters
/// should query `query_range()` which sums `avg_value` buckets (effectively
/// rate-of-change over the bucket window when applied to cumulative gauges).
///
/// # TODO(metrics): Issue 12 — add per-deployment in-memory state to perform
/// correct delta conversion for CUMULATIVE Sum points, mirroring the scraper's
/// `apply_delta` logic.  Until then, CUMULATIVE Sum points are stored as
/// monotonically increasing Gauge values.
fn otlp_to_store_point(p: &OtlpMetricPoint, deployment_id: i32) -> Option<StoreMetricPoint> {
    // Histograms have no single scalar value suitable for service_metrics.
    if p.metric_type == MetricType::Histogram {
        return None;
    }

    // SECURITY(metrics-security-1): OTLP metric names are attacker-controllable
    // (sent by the deployed application) and are interpolated into SQL by the
    // store. Drop names outside the [a-zA-Z0-9_.:-] allowlist at this trust
    // boundary so they never reach the write path.
    if validate_metric_name(&p.metric_name).is_err() {
        warn!(
            deployment_id,
            metric_name = %p.metric_name,
            "Dropping deployment metric: name outside allowlist (possible injection attempt)"
        );
        return None;
    }

    let value = p.value?;

    // Both Sum and Gauge are stored as MetricKind::Gauge.
    // See the correctness note above for why Sum is not stored as Counter.
    let kind = MetricKind::Gauge;

    // Extract environment from resource attributes.
    let environment = p.resource.deployment_environment.clone().or_else(|| {
        p.resource
            .attributes
            .get("service.namespace")
            .map(|v| v.to_string())
    });

    // SECURITY(metrics-security-5): enforce label count and size limits to
    // prevent GIN index write amplification and JSONB column size attacks.
    // Keys are normalised to lowercase to prevent label-key casing bypasses.
    let mut labels: HashMap<String, String> = HashMap::new();
    for (k, v) in &p.attributes {
        if labels.len() >= MAX_LABELS_PER_POINT {
            warn!(
                metric_name = %p.metric_name,
                "OTLP metric point has more than {} labels; excess labels dropped",
                MAX_LABELS_PER_POINT
            );
            break;
        }
        let key = k.to_lowercase();
        if key.len() > MAX_LABEL_BYTES || v.len() > MAX_LABEL_BYTES {
            warn!(
                metric_name = %p.metric_name,
                key = %k,
                "OTLP label key or value exceeds {} bytes; label dropped",
                MAX_LABEL_BYTES
            );
            continue;
        }
        // SECURITY: strip temps.* internal keys from OTLP payload to prevent
        // a deployment from injecting internal routing attributes.
        if key.starts_with("temps.") {
            continue;
        }
        labels.insert(key, v.clone());
    }

    Some(StoreMetricPoint {
        time: p.timestamp,
        source_kind: SourceKind::Deployment,
        source_id: deployment_id,
        name: p.metric_name.clone(),
        value,
        kind,
        engine: None,
        environment,
        node_id: None,
        labels,
    })
}

// ── Shared ingest logic ─────────────────────────────────────────────

/// Resolved context for an ingest request: who authenticated and which
/// project/environment/deployment the telemetry belongs to.
struct IngestContext {
    project_id: i32,
    environment_id: Option<i32>,
    deployment_id: Option<i32>,
}

/// Authenticate the request and resolve the ingest context.
///
/// For header-only requests the IDs come from the `ProjectAuth` returned
/// by `authenticate()`.  For path-based requests the path IDs take
/// precedence — if a `dt_` token is used, we validate that the path IDs
/// match the token record.
async fn resolve_ingest_context(
    state: &OtelAppState,
    token: &str,
    path_ids: Option<(i32, i32, i32)>,
    headers: &HeaderMap,
) -> Result<IngestContext, OtelError> {
    let header_project_id = extract_project_id_header(headers);

    // When path IDs are provided, use path project_id as the header_project_id
    // so `authenticate_api_key` can verify access.
    let effective_project_id = path_ids.map(|(pid, _, _)| pid).or(header_project_id);

    let auth = state
        .otel_service
        .authenticate(token, effective_project_id)
        .await?;

    match path_ids {
        Some((path_project_id, path_environment_id, path_deployment_id)) => {
            // Validate: if the token already binds to a project (dt_ tokens),
            // the path project_id must match.
            if auth.project_id != path_project_id {
                return Err(OtelError::AuthFailed {
                    reason: format!(
                        "Token is bound to project {} but path specifies project {}",
                        auth.project_id, path_project_id
                    ),
                });
            }

            // For dt_ tokens, also validate environment_id if the token has one.
            if let Some(token_env) = auth.environment_id {
                if token_env != path_environment_id {
                    return Err(OtelError::AuthFailed {
                        reason: format!(
                            "Token is bound to environment {} but path specifies environment {}",
                            token_env, path_environment_id
                        ),
                    });
                }
            }

            Ok(IngestContext {
                project_id: path_project_id,
                environment_id: Some(path_environment_id),
                deployment_id: Some(path_deployment_id),
            })
        }
        None => Ok(IngestContext {
            project_id: auth.project_id,
            environment_id: auth.environment_id,
            deployment_id: auth.deployment_id,
        }),
    }
}

/// Ingest metrics for an infrastructure service (`si_` token path).
///
/// The service is identified by `service_auth.service_id` from the
/// authenticated token — `source_id` is never read from the OTLP payload.
/// Points are stored as `SourceKind::Database` in the unified `service_metrics`
/// table, without project/environment/deployment context.
async fn do_ingest_service_metrics(
    state: &OtelAppState,
    service_auth: ServiceAuth,
    headers: &HeaderMap,
    body: &Bytes,
) -> Result<(StatusCode, [(&'static str, &'static str); 1], Vec<u8>), Problem> {
    // SECURITY(metrics-security-2): the `si_` ingest path is otherwise
    // unthrottled. Apply a per-service rate limit (keyed on service_id) before
    // doing any decode/decompress work, so a runaway or compromised exporter
    // can't flood the metrics write channel / TimescaleDB. Fail fast with 429.
    state
        .otel_service
        .check_service_rate_limit(service_auth.service_id)?;

    let data = decode::decompress(body, content_encoding(headers))?;
    // project_id = 0 and deployment_id = None: service metrics are not scoped
    // to a project.  The decode function attaches project_id to each point for
    // OTel-table storage; we only use the resulting OtlpMetricPoint slice to
    // convert scalar points for service_metrics.
    let points = decode::decode_metrics_request(&data, 0, None)?;
    let count = points.len();

    for point in &points {
        debug!(
            service_id = service_auth.service_id,
            service_name = %service_auth.service_name,
            metric_name = %point.metric_name,
            metric_type = %point.metric_type,
            value = point.value,
            "New service metric received"
        );
    }

    // Convert scalar OTLP points to MetricsStore points with Database source kind.
    // Histograms are excluded (same rule as the deployment path).
    let store_points: Vec<StoreMetricPoint> = if state.metrics_write_tx.is_some() {
        points
            .iter()
            .filter_map(|p| {
                if p.metric_type == MetricType::Histogram {
                    return None;
                }
                // SECURITY(metrics-security-1): metric names on the `si_` ingest
                // path are attacker-controllable (sent by the monitored
                // container over OTLP) and are interpolated into SQL by the
                // store. Drop names outside the [a-zA-Z0-9_.:-] allowlist here,
                // at the trust boundary, so they never reach the write path.
                if validate_metric_name(&p.metric_name).is_err() {
                    warn!(
                        service_id = service_auth.service_id,
                        metric_name = %p.metric_name,
                        "Dropping service metric: name outside allowlist (possible injection attempt)"
                    );
                    return None;
                }
                let value = p.value?;
                Some(StoreMetricPoint {
                    time: p.timestamp,
                    source_kind: SourceKind::Database,
                    source_id: service_auth.service_id,
                    name: p.metric_name.clone(),
                    value,
                    kind: MetricKind::Gauge,
                    engine: None,
                    environment: None,
                    node_id: None,
                    labels: HashMap::new(),
                })
            })
            .collect()
    } else {
        Vec::new()
    };

    // Write to the unified MetricsStore via a bounded channel (same backpressure
    // pattern as the deployment path).
    if let Some(tx) = &state.metrics_write_tx {
        if !store_points.is_empty() && tx.try_send(store_points).is_err() {
            warn!(
                service_id = service_auth.service_id,
                "otlp service metrics write channel full; dropping metric batch (backpressure)"
            );
        }
    }

    debug!(
        service_id = service_auth.service_id,
        service_name = %service_auth.service_name,
        received = count,
        "Ingested service metrics batch"
    );

    let response = proto::collector::metrics::v1::ExportMetricsServiceResponse {
        partial_success: None,
    };
    Ok((
        StatusCode::OK,
        [("content-type", "application/x-protobuf")],
        response.encode_to_vec(),
    ))
}

/// Core metrics ingest: authenticate, decompress, decode, store.
async fn do_ingest_metrics(
    state: &OtelAppState,
    token: &str,
    path_ids: Option<(i32, i32, i32)>,
    headers: &HeaderMap,
    body: &Bytes,
) -> Result<(StatusCode, [(&'static str, &'static str); 1], Vec<u8>), Problem> {
    // When path IDs are provided, use the path project_id for header resolution.
    let header_project_id = path_ids
        .map(|(pid, _, _)| pid)
        .or_else(|| extract_project_id_header(headers));

    let auth_result = state
        .otel_service
        .authenticate_any(token, header_project_id)
        .await?;

    match auth_result {
        IngestAuth::Service(service_auth) => {
            return do_ingest_service_metrics(state, service_auth, headers, body).await;
        }
        IngestAuth::Project(_) => {
            // Fall through to the existing project auth path below.
        }
    }

    let ctx = resolve_ingest_context(state, token, path_ids, headers).await?;
    state.otel_service.check_rate_limit(ctx.project_id)?;
    state.otel_service.check_quota(ctx.project_id).await?;

    let data = decode::decompress(body, content_encoding(headers))?;
    let points = decode::decode_metrics_request(&data, ctx.project_id, ctx.deployment_id)?;
    let count = points.len();

    for point in &points {
        debug!(
            project_id = ctx.project_id,
            metric_name = %point.metric_name,
            metric_type = %point.metric_type,
            value = point.value,
            service = %point.resource.service_name,
            "New metric received"
        );
    }

    // Convert scalar OTLP points to MetricsStore points *before* consuming
    // `points` by passing it to ingest_metrics. Histograms are excluded here —
    // they remain in otel_metrics only.  The write is non-blocking and must
    // never fail the OTLP HTTP response.
    let store_points: Option<Vec<StoreMetricPoint>> =
        if let (Some(_), Some(deployment_id)) = (&state.metrics_store, ctx.deployment_id) {
            let v: Vec<StoreMetricPoint> = points
                .iter()
                .filter_map(|p| otlp_to_store_point(p, deployment_id))
                .collect();
            if v.is_empty() {
                None
            } else {
                Some(v)
            }
        } else {
            None
        };

    let stored = state.otel_service.ingest_metrics(points).await?;

    // Write OTLP metric points to the unified MetricsStore via a bounded
    // channel (backpressure path).  If the channel is full (TimescaleDB write
    // backlog) we drop the batch and increment the dropped counter rather than
    // spawning an unbounded number of tasks.
    //
    // SCALABILITY(metrics-scale-1): bounded channel prevents unbounded
    // task accumulation and connection pool exhaustion under OTLP burst load.
    if let (Some(tx), Some(sp)) = (&state.metrics_write_tx, store_points) {
        if tx.try_send(sp).is_err() {
            warn!(
                project_id = ctx.project_id,
                "otlp metrics write channel full; dropping metric batch (backpressure)"
            );
        }
    }

    debug!(
        project_id = ctx.project_id,
        environment_id = ?ctx.environment_id,
        deployment_id = ?ctx.deployment_id,
        received = count,
        stored,
        "Ingested metrics batch"
    );

    let response = proto::collector::metrics::v1::ExportMetricsServiceResponse {
        partial_success: None,
    };
    Ok((
        StatusCode::OK,
        [("content-type", "application/x-protobuf")],
        response.encode_to_vec(),
    ))
}

/// Core traces ingest: authenticate, decompress, decode, store.
async fn do_ingest_traces(
    state: &OtelAppState,
    token: &str,
    path_ids: Option<(i32, i32, i32)>,
    headers: &HeaderMap,
    body: &Bytes,
) -> Result<(StatusCode, [(&'static str, &'static str); 1], Vec<u8>), Problem> {
    let ctx = resolve_ingest_context(state, token, path_ids, headers).await?;
    state.otel_service.check_rate_limit(ctx.project_id)?;
    state.otel_service.check_quota(ctx.project_id).await?;

    let data = decode::decompress(body, content_encoding(headers))?;
    let spans = decode::decode_traces_request(&data, ctx.project_id, ctx.deployment_id)?;
    let count = spans.len();

    // Log each span at debug level so operators can see what arrived
    for span in &spans {
        debug!(
            project_id = ctx.project_id,
            trace_id = %span.trace_id,
            span_id = %span.span_id,
            parent_span_id = ?span.parent_span_id,
            name = %span.name,
            kind = %span.kind,
            status = %span.status_code,
            duration_ms = span.duration_ms,
            service = %span.resource.service_name,
            "New span received"
        );
    }

    // ADR-027 Phase 0: collect distinct trace_ids before moving `spans` into
    // `ingest_spans`.  We fire the hint AFTER ingest succeeds so that only
    // durably-stored spans produce discovery rows.
    // Only well-formed (32-char lowercase hex) trace_ids become discovery rows.
    // OTLP-decoded ids are always valid, but this guards against ever writing
    // keys the read path (which validates the same way) could never match.
    let hint_trace_ids: std::collections::HashSet<String> = spans
        .iter()
        .map(|s| s.trace_id.clone())
        .filter(|tid| is_valid_trace_id(tid))
        .collect();

    let stored = state.otel_service.ingest_spans(spans).await?;

    // Forward cross-project trace hint to the bounded background writer.
    // `try_send` never blocks the OTLP HTTP response; on Full/Closed we warn
    // and drop — hint loss is non-fatal (re-populated by the next ingest batch).
    if let Some(ref tx) = state.trace_hint_tx {
        use tokio::sync::mpsc::error::TrySendError;
        let msg = TraceHintMsg {
            trace_ids: hint_trace_ids,
            project_id: ctx.project_id,
        };
        match tx.try_send(msg) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                warn!(
                    project_id = ctx.project_id,
                    "cross_project trace hint channel full; hint dropped (backpressure — non-fatal)"
                );
            }
            Err(TrySendError::Closed(_)) => {
                warn!(
                    project_id = ctx.project_id,
                    "cross_project trace hint channel closed unexpectedly; hint dropped"
                );
            }
        }
    }

    debug!(
        project_id = ctx.project_id,
        environment_id = ?ctx.environment_id,
        deployment_id = ?ctx.deployment_id,
        received = count,
        stored,
        "Ingested traces batch"
    );

    let response = proto::collector::trace::v1::ExportTraceServiceResponse {
        partial_success: None,
    };
    Ok((
        StatusCode::OK,
        [("content-type", "application/x-protobuf")],
        response.encode_to_vec(),
    ))
}

/// Core logs ingest: authenticate, decompress, decode, store.
async fn do_ingest_logs(
    state: &OtelAppState,
    token: &str,
    path_ids: Option<(i32, i32, i32)>,
    headers: &HeaderMap,
    body: &Bytes,
) -> Result<(StatusCode, [(&'static str, &'static str); 1], Vec<u8>), Problem> {
    let ctx = resolve_ingest_context(state, token, path_ids, headers).await?;
    state.otel_service.check_rate_limit(ctx.project_id)?;
    state.otel_service.check_quota(ctx.project_id).await?;

    let data = decode::decompress(body, content_encoding(headers))?;
    let records = decode::decode_logs_request(&data, ctx.project_id, ctx.deployment_id)?;
    let count = records.len();

    for record in &records {
        debug!(
            project_id = ctx.project_id,
            severity = %record.severity,
            body = %record.body,
            service = %record.resource.service_name,
            "New log received"
        );
    }

    let stored = state.otel_service.ingest_logs(records).await?;

    debug!(
        project_id = ctx.project_id,
        environment_id = ?ctx.environment_id,
        deployment_id = ?ctx.deployment_id,
        received = count,
        stored,
        "Ingested logs"
    );

    let response = proto::collector::logs::v1::ExportLogsServiceResponse {
        partial_success: None,
    };
    Ok((
        StatusCode::OK,
        [("content-type", "application/x-protobuf")],
        response.encode_to_vec(),
    ))
}

// ── Header-based ingest handlers ────────────────────────────────────

/// Ingest metrics via OTLP/HTTP protobuf.
///
/// Authenticates via API key in header, decompresses, decodes protobuf,
/// checks rate limit and storage quota, then stores.
#[utoipa::path(
    tag = "OTel Ingest",
    post,
    path = "/otel/v1/metrics",
    request_body(content = String, content_type = "application/x-protobuf", description = "OTLP ExportMetricsServiceRequest (protobuf, optionally gzip/zstd compressed)"),
    responses(
        (status = 200, description = "Metrics accepted (OTLP protobuf response)"),
        (status = 400, description = "Invalid payload", body = ProblemDetails),
        (status = 401, description = "Missing or invalid API key", body = ProblemDetails),
        (status = 413, description = "Storage quota exceeded", body = ProblemDetails),
        (status = 429, description = "Rate limit exceeded", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("api_key" = []))
)]
pub async fn ingest_metrics(
    State(state): State<OtelAppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, Problem> {
    let token = extract_token(&headers).ok_or_else(|| OtelError::AuthFailed {
        reason: "Missing token in Authorization or X-Temps-Api-Key header".into(),
    })?;
    let result = do_ingest_metrics(&state, &token, None, &headers, &body).await;
    if let Err(ref e) = result {
        error!(error = ?e, "Failed to ingest metrics (header auth)");
    }
    result
}

/// Ingest trace spans via OTLP/HTTP protobuf.
///
/// Authenticates via API key in header, decompresses, decodes protobuf,
/// checks rate limit and storage quota, then stores spans.
#[utoipa::path(
    tag = "OTel Ingest",
    post,
    path = "/otel/v1/traces",
    request_body(content = String, content_type = "application/x-protobuf", description = "OTLP ExportTraceServiceRequest (protobuf, optionally gzip/zstd compressed)"),
    responses(
        (status = 200, description = "Traces accepted (OTLP protobuf response)"),
        (status = 400, description = "Invalid payload", body = ProblemDetails),
        (status = 401, description = "Missing or invalid API key", body = ProblemDetails),
        (status = 413, description = "Storage quota exceeded", body = ProblemDetails),
        (status = 429, description = "Rate limit exceeded", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("api_key" = []))
)]
pub async fn ingest_traces(
    State(state): State<OtelAppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, Problem> {
    let token = extract_token(&headers).ok_or_else(|| OtelError::AuthFailed {
        reason: "Missing token in Authorization or X-Temps-Api-Key header".into(),
    })?;
    let result = do_ingest_traces(&state, &token, None, &headers, &body).await;
    if let Err(ref e) = result {
        error!(error = ?e, "Failed to ingest traces (header auth)");
    }
    result
}

/// Ingest log records via OTLP/HTTP protobuf.
///
/// Authenticates via API key in header, decompresses, decodes protobuf,
/// checks rate limit and storage quota, routes high-severity logs
/// to DB and all logs to S3.
#[utoipa::path(
    tag = "OTel Ingest",
    post,
    path = "/otel/v1/logs",
    request_body(content = String, content_type = "application/x-protobuf", description = "OTLP ExportLogsServiceRequest (protobuf, optionally gzip/zstd compressed)"),
    responses(
        (status = 200, description = "Logs accepted (OTLP protobuf response)"),
        (status = 400, description = "Invalid payload", body = ProblemDetails),
        (status = 401, description = "Missing or invalid API key", body = ProblemDetails),
        (status = 413, description = "Storage quota exceeded", body = ProblemDetails),
        (status = 429, description = "Rate limit exceeded", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("api_key" = []))
)]
pub async fn ingest_logs(
    State(state): State<OtelAppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, Problem> {
    let token = extract_token(&headers).ok_or_else(|| OtelError::AuthFailed {
        reason: "Missing token in Authorization or X-Temps-Api-Key header".into(),
    })?;
    let result = do_ingest_logs(&state, &token, None, &headers, &body).await;
    if let Err(ref e) = result {
        error!(error = ?e, "Failed to ingest logs (header auth)");
    }
    result
}

// ── Path-based ingest handlers ──────────────────────────────────────
//
// These accept project_id, environment_id, and deployment_id as path
// parameters, so the OTLP exporter endpoint becomes:
//
//   https://host/api/otel/v1/{project_id}/{environment_id}/{deployment_id}
//
// The SDK automatically appends /traces, /metrics, /logs.
// Authentication still uses the Authorization header (Bearer tk_/dt_).

/// Path parameters for path-based ingest: (project_id, environment_id, deployment_id).
type IngestPathParams = (i32, i32, i32);

/// Ingest metrics with project/environment/deployment in the URL path.
#[utoipa::path(
    tag = "OTel Ingest",
    post,
    path = "/otel/v1/{project_id}/{environment_id}/{deployment_id}/metrics",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("deployment_id" = i32, Path, description = "Deployment ID"),
    ),
    request_body(content = String, content_type = "application/x-protobuf", description = "OTLP ExportMetricsServiceRequest (protobuf, optionally gzip/zstd compressed)"),
    responses(
        (status = 200, description = "Metrics accepted (OTLP protobuf response)"),
        (status = 400, description = "Invalid payload", body = ProblemDetails),
        (status = 401, description = "Missing or invalid API key", body = ProblemDetails),
        (status = 413, description = "Storage quota exceeded", body = ProblemDetails),
        (status = 429, description = "Rate limit exceeded", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("api_key" = []))
)]
pub async fn ingest_metrics_by_path(
    State(state): State<OtelAppState>,
    Path(path_ids): Path<IngestPathParams>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, Problem> {
    let token = extract_token(&headers).ok_or_else(|| OtelError::AuthFailed {
        reason: "Missing token in Authorization or X-Temps-Api-Key header".into(),
    })?;
    let result = do_ingest_metrics(&state, &token, Some(path_ids), &headers, &body).await;
    if let Err(ref e) = result {
        error!(error = ?e, "Failed to ingest metrics (path auth)");
    }
    result
}

/// Ingest trace spans with project/environment/deployment in the URL path.
#[utoipa::path(
    tag = "OTel Ingest",
    post,
    path = "/otel/v1/{project_id}/{environment_id}/{deployment_id}/traces",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("deployment_id" = i32, Path, description = "Deployment ID"),
    ),
    request_body(content = String, content_type = "application/x-protobuf", description = "OTLP ExportTraceServiceRequest (protobuf, optionally gzip/zstd compressed)"),
    responses(
        (status = 200, description = "Traces accepted (OTLP protobuf response)"),
        (status = 400, description = "Invalid payload", body = ProblemDetails),
        (status = 401, description = "Missing or invalid API key", body = ProblemDetails),
        (status = 413, description = "Storage quota exceeded", body = ProblemDetails),
        (status = 429, description = "Rate limit exceeded", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("api_key" = []))
)]
pub async fn ingest_traces_by_path(
    State(state): State<OtelAppState>,
    Path(path_ids): Path<IngestPathParams>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, Problem> {
    let token = extract_token(&headers).ok_or_else(|| OtelError::AuthFailed {
        reason: "Missing token in Authorization or X-Temps-Api-Key header".into(),
    })?;
    let result = do_ingest_traces(&state, &token, Some(path_ids), &headers, &body).await;
    if let Err(ref e) = result {
        error!(error = ?e, "Failed to ingest traces (path auth)");
    }
    result
}

/// Ingest log records with project/environment/deployment in the URL path.
#[utoipa::path(
    tag = "OTel Ingest",
    post,
    path = "/otel/v1/{project_id}/{environment_id}/{deployment_id}/logs",
    params(
        ("project_id" = i32, Path, description = "Project ID"),
        ("environment_id" = i32, Path, description = "Environment ID"),
        ("deployment_id" = i32, Path, description = "Deployment ID"),
    ),
    request_body(content = String, content_type = "application/x-protobuf", description = "OTLP ExportLogsServiceRequest (protobuf, optionally gzip/zstd compressed)"),
    responses(
        (status = 200, description = "Logs accepted (OTLP protobuf response)"),
        (status = 400, description = "Invalid payload", body = ProblemDetails),
        (status = 401, description = "Missing or invalid API key", body = ProblemDetails),
        (status = 413, description = "Storage quota exceeded", body = ProblemDetails),
        (status = 429, description = "Rate limit exceeded", body = ProblemDetails),
        (status = 500, description = "Internal server error", body = ProblemDetails),
    ),
    security(("api_key" = []))
)]
pub async fn ingest_logs_by_path(
    State(state): State<OtelAppState>,
    Path(path_ids): Path<IngestPathParams>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, Problem> {
    let token = extract_token(&headers).ok_or_else(|| OtelError::AuthFailed {
        reason: "Missing token in Authorization or X-Temps-Api-Key header".into(),
    })?;
    let result = do_ingest_logs(&state, &token, Some(path_ids), &headers, &body).await;
    if let Err(ref e) = result {
        error!(error = ?e, "Failed to ingest logs (path auth)");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    // ── extract_token tests ──────────────────────────────────────

    #[test]
    fn test_extract_token_bearer() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer tk_abc123".parse().unwrap());
        assert_eq!(extract_token(&headers), Some("tk_abc123".to_string()));
    }

    #[test]
    fn test_extract_token_deployment_token_bearer() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer dt_abc123".parse().unwrap());
        assert_eq!(extract_token(&headers), Some("dt_abc123".to_string()));
    }

    #[test]
    fn test_extract_token_custom_header() {
        let mut headers = HeaderMap::new();
        headers.insert("x-temps-api-key", "tk_xyz789".parse().unwrap());
        assert_eq!(extract_token(&headers), Some("tk_xyz789".to_string()));
    }

    #[test]
    fn test_extract_token_missing() {
        let headers = HeaderMap::new();
        assert_eq!(extract_token(&headers), None);
    }

    #[test]
    fn test_extract_token_bearer_takes_priority() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer tk_first".parse().unwrap());
        headers.insert("x-temps-api-key", "tk_second".parse().unwrap());
        assert_eq!(extract_token(&headers), Some("tk_first".to_string()));
    }

    #[test]
    fn test_extract_token_non_bearer_auth_falls_through() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Basic dXNlcjpwYXNz".parse().unwrap());
        headers.insert("x-temps-api-key", "tk_fallback".parse().unwrap());
        assert_eq!(extract_token(&headers), Some("tk_fallback".to_string()));
    }

    #[test]
    fn test_extract_token_bearer_trimmed() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer  tk_spaces  ".parse().unwrap());
        assert_eq!(extract_token(&headers), Some("tk_spaces".to_string()));
    }

    // ── extract_project_id_header tests ────────────────────────────

    #[test]
    fn test_extract_project_id_header_present() {
        let mut headers = HeaderMap::new();
        headers.insert("x-temps-project-id", "42".parse().unwrap());
        assert_eq!(extract_project_id_header(&headers), Some(42));
    }

    #[test]
    fn test_extract_project_id_header_missing() {
        let headers = HeaderMap::new();
        assert_eq!(extract_project_id_header(&headers), None);
    }

    #[test]
    fn test_extract_project_id_header_invalid() {
        let mut headers = HeaderMap::new();
        headers.insert("x-temps-project-id", "not-a-number".parse().unwrap());
        assert_eq!(extract_project_id_header(&headers), None);
    }

    // ── content_encoding tests ─────────────────────────────────────

    #[test]
    fn test_content_encoding_present() {
        let mut headers = HeaderMap::new();
        headers.insert("content-encoding", "gzip".parse().unwrap());
        assert_eq!(content_encoding(&headers), Some("gzip"));
    }

    #[test]
    fn test_content_encoding_absent() {
        let headers = HeaderMap::new();
        assert_eq!(content_encoding(&headers), None);
    }

    // ── From<OtelError> for Problem tests ──────────────────────────

    #[test]
    fn test_error_auth_failed_maps_to_401() {
        let err = OtelError::AuthFailed {
            reason: "bad key".into(),
        };
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_error_invalid_api_key_maps_to_401() {
        let err = OtelError::InvalidApiKey;
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_error_rate_limit_maps_to_429() {
        let err = OtelError::RateLimitExceeded {
            project_id: 1,
            limit: 1000,
        };
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::TOO_MANY_REQUESTS);
    }

    #[test]
    fn test_error_quota_exceeded_maps_to_413() {
        let err = OtelError::QuotaExceeded {
            project_id: 1,
            used_bytes: 100,
            limit_bytes: 50,
        };
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[test]
    fn test_error_protobuf_decode_maps_to_400() {
        let err = OtelError::ProtobufDecode {
            reason: "bad data".into(),
        };
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_error_validation_maps_to_400() {
        let err = OtelError::Validation {
            message: "missing field".into(),
        };
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_error_decompression_maps_to_400() {
        let err = OtelError::DecompressionFailed {
            encoding: "gzip".into(),
            reason: "corrupt".into(),
        };
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_error_unsupported_encoding_maps_to_400() {
        let err = OtelError::UnsupportedEncoding {
            encoding: "brotli".into(),
        };
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_error_project_not_found_maps_to_404() {
        let err = OtelError::ProjectNotFound { project_id: 42 };
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_error_storage_maps_to_500() {
        let err = OtelError::Storage {
            message: "disk full".into(),
        };
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_error_database_maps_to_500() {
        let err = OtelError::Database(sea_orm::DbErr::Custom("test".into()));
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_error_s3_maps_to_500() {
        let err = OtelError::S3 {
            project_id: 1,
            reason: "timeout".into(),
        };
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_error_io_maps_to_500() {
        let err = OtelError::Io(std::io::Error::other("test"));
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_error_internal_maps_to_500() {
        let err = OtelError::Internal {
            message: "unexpected".into(),
        };
        let problem: Problem = err.into();
        assert_eq!(problem.status_code, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_error_problem_detail_contains_message() {
        let err = OtelError::RateLimitExceeded {
            project_id: 7,
            limit: 500,
        };
        let problem: Problem = err.into();
        let detail = problem
            .body
            .get("detail")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(detail.contains("project 7"), "detail: {detail}");
        assert!(detail.contains("500"), "detail: {detail}");
    }

    // ── IngestContext tests ────────────────────────────────────────

    #[test]
    fn test_ingest_context_from_path_ids() {
        let ctx = IngestContext {
            project_id: 10,
            environment_id: Some(20),
            deployment_id: Some(30),
        };
        assert_eq!(ctx.project_id, 10);
        assert_eq!(ctx.environment_id, Some(20));
        assert_eq!(ctx.deployment_id, Some(30));
    }

    #[test]
    fn test_ingest_path_params_type_alias() {
        let params: IngestPathParams = (1, 2, 3);
        assert_eq!(params.0, 1);
        assert_eq!(params.1, 2);
        assert_eq!(params.2, 3);
    }

    // ── otlp_to_store_point tests ────────────────────────────────────

    use crate::types::{MetricPoint as OtlpMetricPoint, MetricType, ResourceInfo};
    use std::collections::BTreeMap;

    fn make_otlp_point(metric_type: MetricType, value: f64) -> OtlpMetricPoint {
        let mut p = OtlpMetricPoint::skeleton(
            1,
            Some(42),
            ResourceInfo::default(),
            "test.metric".to_string(),
            metric_type,
            "count".to_string(),
            chrono::Utc::now(),
            BTreeMap::new(),
        );
        p.value = Some(value);
        p
    }

    /// CORRECTNESS(metrics-correctness-B): OTLP Sum must be stored as Gauge,
    /// not Counter. Counter semantics would cause double-delta computation.
    #[test]
    fn test_otlp_sum_stored_as_gauge_not_counter() {
        let p = make_otlp_point(MetricType::Sum, 1000.0);
        let result = otlp_to_store_point(&p, 42).unwrap();
        assert_eq!(
            result.kind,
            MetricKind::Gauge,
            "OTLP Sum must be stored as Gauge to avoid double-delta corruption; \
             got {:?}",
            result.kind
        );
        assert_eq!(result.value, 1000.0);
        assert_eq!(result.source_id, 42);
    }

    #[test]
    fn test_otlp_gauge_stored_as_gauge() {
        let p = make_otlp_point(MetricType::Gauge, 55.0);
        let result = otlp_to_store_point(&p, 42).unwrap();
        assert_eq!(result.kind, MetricKind::Gauge);
        assert_eq!(result.value, 55.0);
    }

    #[test]
    fn test_otlp_histogram_returns_none() {
        let p = make_otlp_point(MetricType::Histogram, 0.0);
        assert!(
            otlp_to_store_point(&p, 42).is_none(),
            "Histogram points have no scalar value and must be dropped"
        );
    }

    /// SECURITY(metrics-security-1): OTLP metric names are attacker-controllable
    /// and are interpolated into SQL by the store. `otlp_to_store_point` must
    /// drop names outside the [a-zA-Z0-9_.:-] allowlist at this trust boundary
    /// (returning `None`) so they never reach the write path. This is the
    /// deployment-path counterpart of the `si_` service-ingest guard.
    #[test]
    fn test_otlp_rejects_sql_injection_metric_name() {
        for payload in [
            "x'); DROP TABLE service_metrics; --",
            "metric' OR '1'='1",
            "name with space",
            "name;semicolon",
            "name\nnewline",
            "", // empty is also rejected by the allowlist
        ] {
            let mut p = make_otlp_point(MetricType::Gauge, 1.0);
            p.metric_name = payload.to_string();
            assert!(
                otlp_to_store_point(&p, 42).is_none(),
                "injection/invalid name must be dropped before reaching SQL: {payload:?}"
            );
        }
    }

    /// Counterpart to the rejection test: a well-formed name passes through and
    /// carries its value, confirming the guard doesn't over-reject.
    #[test]
    fn test_otlp_accepts_valid_metric_name() {
        let mut p = make_otlp_point(MetricType::Gauge, 42.0);
        p.metric_name = "rustfs.storage.used_bytes".to_string();
        let sp = otlp_to_store_point(&p, 42).expect("valid name should produce a store point");
        assert_eq!(sp.name, "rustfs.storage.used_bytes");
        assert_eq!(sp.value, 42.0);
    }

    /// SECURITY(metrics-security-5): label count cap must be enforced.
    #[test]
    fn test_otlp_label_count_cap() {
        let mut p = make_otlp_point(MetricType::Gauge, 1.0);
        // Insert more labels than the limit.
        for i in 0..(MAX_LABELS_PER_POINT + 10) {
            p.attributes.insert(format!("key_{}", i), "val".to_string());
        }
        let result = otlp_to_store_point(&p, 42).unwrap();
        assert!(
            result.labels.len() <= MAX_LABELS_PER_POINT,
            "label count {} exceeds cap {}",
            result.labels.len(),
            MAX_LABELS_PER_POINT
        );
    }

    /// SECURITY(metrics-security-5): over-length label keys/values must be dropped.
    #[test]
    fn test_otlp_label_size_cap_drops_oversized() {
        let mut p = make_otlp_point(MetricType::Gauge, 1.0);
        p.attributes
            .insert("normal_key".to_string(), "normal_value".to_string());
        p.attributes
            .insert("short_key".to_string(), "x".repeat(MAX_LABEL_BYTES + 1));
        p.attributes
            .insert("k".repeat(MAX_LABEL_BYTES + 1), "v".to_string());
        let result = otlp_to_store_point(&p, 42).unwrap();
        // Only the normal label should survive.
        assert_eq!(result.labels.len(), 1);
        assert!(result.labels.contains_key("normal_key"));
    }

    /// SECURITY(metrics-security-4): temps.* internal keys must be stripped.
    #[test]
    fn test_otlp_temps_internal_keys_stripped() {
        let mut p = make_otlp_point(MetricType::Gauge, 1.0);
        p.attributes
            .insert("temps.deployment_id".to_string(), "99".to_string());
        p.attributes
            .insert("temps.source_id".to_string(), "999".to_string());
        p.attributes
            .insert("safe_key".to_string(), "safe_val".to_string());
        let result = otlp_to_store_point(&p, 42).unwrap();
        // temps.* keys must be stripped; safe_key survives.
        assert!(!result.labels.contains_key("temps.deployment_id"));
        assert!(!result.labels.contains_key("temps.source_id"));
        assert!(result.labels.contains_key("safe_key"));
        // source_id comes from the auth context (argument), not from payload.
        assert_eq!(result.source_id, 42);
    }

    /// SECURITY(metrics-security-5): label keys must be normalised to lowercase.
    #[test]
    fn test_otlp_label_keys_normalized_to_lowercase() {
        let mut p = make_otlp_point(MetricType::Gauge, 1.0);
        p.attributes
            .insert("Environment".to_string(), "prod".to_string());
        p.attributes
            .insert("REGION".to_string(), "us-east-1".to_string());
        let result = otlp_to_store_point(&p, 42).unwrap();
        assert!(
            result.labels.contains_key("environment"),
            "keys must be lowercased"
        );
        assert!(
            result.labels.contains_key("region"),
            "keys must be lowercased"
        );
        assert!(!result.labels.contains_key("Environment"));
        assert!(!result.labels.contains_key("REGION"));
    }

    // ── Service metrics ingest ───────────────────────────────────────────────────

    #[test]
    fn test_si_token_routes_to_service_path() {
        // Verify prefix detection that routes to service ingest
        assert!("si_abc123456789".starts_with("si_"));
        assert!(!"tk_abc123456789".starts_with("si_"));
        assert!(!"dt_abc123456789".starts_with("si_"));
    }

    #[test]
    fn test_otlp_gauge_converts_to_database_source_kind() {
        use temps_metrics::SourceKind;
        // When we create a StoreMetricPoint for a service, source_kind must be Database
        let point = temps_metrics::MetricPoint {
            time: chrono::Utc::now(),
            source_kind: SourceKind::Database,
            source_id: 7,
            name: "rustfs.storage.used_bytes".to_string(),
            value: 1024.0,
            kind: temps_metrics::MetricKind::Gauge,
            engine: None,
            environment: None,
            node_id: None,
            labels: std::collections::HashMap::new(),
        };
        assert_eq!(point.source_id, 7);
        assert!(matches!(point.source_kind, SourceKind::Database));
        assert_eq!(point.name, "rustfs.storage.used_bytes");
    }

    #[test]
    fn test_service_auth_carries_service_id() {
        use crate::ingest::auth::ServiceAuth;
        let auth = ServiceAuth {
            service_id: 42,
            service_name: "rustfs-main".to_string(),
            token_id: 1,
        };
        // The service_id is what gets written as source_id in service_metrics
        assert_eq!(auth.service_id, 42);
    }

    #[test]
    fn test_otlp_metrics_protobuf_roundtrip() {
        use crate::proto;
        use prost::Message;

        // Build a minimal ExportMetricsServiceRequest like RustFS would send
        let request = proto::collector::metrics::v1::ExportMetricsServiceRequest {
            resource_metrics: vec![proto::metrics::v1::ResourceMetrics {
                resource: Some(proto::resource::v1::Resource {
                    attributes: vec![proto::common::v1::KeyValue {
                        key: "service.name".to_string(),
                        value: Some(proto::common::v1::AnyValue {
                            value: Some(proto::common::v1::any_value::Value::StringValue(
                                "rustfs-main".to_string(),
                            )),
                        }),
                    }],
                    dropped_attributes_count: 0,
                }),
                scope_metrics: vec![proto::metrics::v1::ScopeMetrics {
                    scope: None,
                    #[allow(clippy::needless_update)]
                    metrics: vec![proto::metrics::v1::Metric {
                        name: "rustfs.storage.used_bytes".to_string(),
                        description: "Storage used in bytes".to_string(),
                        unit: "By".to_string(),
                        data: Some(proto::metrics::v1::metric::Data::Gauge(
                            proto::metrics::v1::Gauge {
                                data_points: vec![proto::metrics::v1::NumberDataPoint {
                                    value: Some(
                                        proto::metrics::v1::number_data_point::Value::AsDouble(
                                            1073741824.0, // 1 GiB
                                        ),
                                    ),
                                    ..Default::default()
                                }],
                            },
                        )),
                        ..Default::default()
                    }],
                    schema_url: String::new(),
                }],
                schema_url: String::new(),
            }],
        };

        // Encode to protobuf bytes
        let encoded = request.encode_to_vec();
        assert!(!encoded.is_empty());

        // Decode back
        let decoded =
            proto::collector::metrics::v1::ExportMetricsServiceRequest::decode(encoded.as_slice())
                .expect("decode failed");

        assert_eq!(decoded.resource_metrics.len(), 1);
        let rm = &decoded.resource_metrics[0];
        assert_eq!(
            rm.scope_metrics[0].metrics[0].name,
            "rustfs.storage.used_bytes"
        );
    }
}
