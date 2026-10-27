// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Worker metrics (ADR 061): what this worker's edge proxy and host report to
//! the control plane.
//!
//! - The public ingress and tunnel-origin listeners count every request into
//!   one process-wide [`ProxyMetrics`], the same counters the control plane's
//!   proxy uses. Internal `*.temps.local` traffic is not counted: it is
//!   workload-to-workload, not edge traffic.
//! - `GET /agent/metrics` (node-token authenticated) serves the cumulative
//!   counters, a per-process `instance_id`, and a host sample taken at request
//!   time. The control plane pulls it on its own scrape interval, so the host
//!   is sampled at the plane's cadence, and stamps everything with its own
//!   receive time. Nothing here is timestamped.

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};
use std::time::Instant;

use axum::body::{Body, Bytes, HttpBody};
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;
use axum::{Extension, Json, Router};
use temps_auth::permission_guard;
use temps_core::problemdetails::Problem;
use temps_metrics::proxy::{ProxyMetrics, RequestDestination};
use temps_metrics::worker_report::WORKER_METRICS_VERSION;
use temps_metrics::{
    Collector, CollectorConfig, HostSample, NodeMetricsCollector, NodeMetricsScope, SourceKind,
    WorkerMetricsReport, HOST_METRIC_NAMES, WORKER_METRICS_PATH,
};
use utoipa::OpenApi;

use crate::auth::{require_agent_auth, AgentAuth, RequireAgentAuth};
use crate::AgentConfig;

/// This process's edge-proxy counters and the id that tells the control plane
/// they started from zero.
struct ProcessCounters {
    instance_id: String,
    proxy: Arc<ProxyMetrics>,
}

static COUNTERS: OnceLock<ProcessCounters> = OnceLock::new();

fn counters() -> &'static ProcessCounters {
    COUNTERS.get_or_init(|| ProcessCounters {
        instance_id: uuid::Uuid::new_v4().to_string(),
        proxy: Arc::new(ProxyMetrics::default()),
    })
}

/// The counters every public listener of this process records into.
pub(crate) fn proxy_metrics() -> Arc<ProxyMetrics> {
    counters().proxy.clone()
}

// ── Recording ─────────────────────────────────────────────────────────────────

const NO_UPSTREAM: u64 = u64::MAX;

/// One request in flight. Shared between the recording middleware, the
/// response body and (for an upgrade) the tunnel task; whichever lets go last
/// records it, so `elapsed` covers the whole body or the whole session.
/// A handful of relaxed atomics: no locks on the request path.
struct InFlight {
    metrics: Arc<ProxyMetrics>,
    started: Instant,
    /// 0 until the handler produced a response.
    status: AtomicU16,
    upstream_ms: AtomicU64,
    project: AtomicBool,
    streaming: AtomicBool,
}

impl Drop for InFlight {
    fn drop(&mut self) {
        let status = self.status.load(Ordering::Relaxed);
        // No response means the client went away before the handler answered;
        // nothing was served, so there is nothing to classify.
        if status == 0 {
            return;
        }
        let elapsed_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let upstream_ms = match self.upstream_ms.load(Ordering::Relaxed) {
            NO_UPSTREAM => None,
            ms => Some(ms),
        };
        let destination = if self.project.load(Ordering::Relaxed) {
            RequestDestination::Project
        } else {
            RequestDestination::Other
        };
        self.metrics.record(
            status,
            elapsed_ms,
            upstream_ms,
            destination,
            self.streaming.load(Ordering::Relaxed),
        );
    }
}

/// Request-extension handle the proxy uses to annotate the request it is
/// serving. Absent on the private `*.temps.local` listener.
#[derive(Clone)]
pub(crate) struct RequestRecorder(Arc<InFlight>);

impl RequestRecorder {
    fn start(metrics: Arc<ProxyMetrics>) -> Self {
        Self(Arc::new(InFlight {
            metrics,
            started: Instant::now(),
            status: AtomicU16::new(0),
            upstream_ms: AtomicU64::new(NO_UPSTREAM),
            project: AtomicBool::new(false),
            streaming: AtomicBool::new(false),
        }))
    }

    /// The request resolved a project route.
    pub(crate) fn mark_project(&self) {
        self.0.project.store(true, Ordering::Relaxed);
    }

    /// The upstream answered with headers; `since` is when it was dialled.
    pub(crate) fn upstream_answered(&self, since: Instant) {
        let ms = u64::try_from(since.elapsed().as_millis()).unwrap_or(NO_UPSTREAM - 1);
        self.0
            .upstream_ms
            .store(ms.min(NO_UPSTREAM - 1), Ordering::Relaxed);
    }

    fn respond(&self, status: StatusCode, streaming: bool) {
        self.0.status.store(status.as_u16(), Ordering::Relaxed);
        self.0.streaming.store(streaming, Ordering::Relaxed);
    }
}

fn is_event_stream(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("text/event-stream")
        })
}

/// Response body that releases its share of the request when it ends, so a
/// slow download is measured to its last byte. Size hint and end-of-stream
/// pass through untouched (no change to framing).
struct RecordedBody {
    inner: Body,
    recorder: Option<RequestRecorder>,
}

impl HttpBody for RecordedBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, axum::Error>>> {
        let this = &mut *self;
        let polled = Pin::new(&mut this.inner).poll_frame(cx);
        if matches!(polled, Poll::Ready(None) | Poll::Ready(Some(Err(_)))) {
            this.recorder = None;
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.inner.size_hint()
    }
}

async fn record(metrics: Arc<ProxyMetrics>, mut req: Request, next: Next) -> Response {
    let recorder = RequestRecorder::start(metrics);
    req.extensions_mut().insert(recorder.clone());
    let response = next.run(req).await;
    let streaming =
        response.status() == StatusCode::SWITCHING_PROTOCOLS || is_event_stream(response.headers());
    recorder.respond(response.status(), streaming);
    let (parts, body) = response.into_parts();
    Response::from_parts(
        parts,
        Body::new(RecordedBody {
            inner: body,
            recorder: Some(recorder),
        }),
    )
}

/// Count every request `router` serves into `metrics`.
pub(crate) fn recorded(router: Router, metrics: Arc<ProxyMetrics>) -> Router {
    router.layer(middleware::from_fn(move |req: Request, next: Next| {
        record(metrics.clone(), req, next)
    }))
}

// ── Report endpoint ───────────────────────────────────────────────────────────

struct ReportSource {
    instance_id: String,
    proxy: Arc<ProxyMetrics>,
    host: NodeMetricsCollector,
    data_dir: String,
}

impl ReportSource {
    async fn report(&self) -> WorkerMetricsReport {
        let config = CollectorConfig::new(0, SourceKind::Node, self.data_dir.clone());
        let host = match self.host.collect(&config).await {
            Ok(points) => points
                .into_iter()
                .filter(|point| HOST_METRIC_NAMES.contains(&point.name.as_str()))
                .map(|point| HostSample {
                    name: point.name,
                    value: point.value,
                })
                .collect(),
            Err(error) => {
                tracing::warn!(%error, "Host metrics unavailable for the worker metrics report");
                Vec::new()
            }
        };
        WorkerMetricsReport {
            version: WORKER_METRICS_VERSION,
            instance_id: self.instance_id.clone(),
            proxy: self.proxy.snapshot(),
            host,
        }
    }
}

/// The agent data directory (parent of `dns_data_dir`), whose filesystem the
/// disk metrics describe; `/` when it has none.
fn data_dir(config: &AgentConfig) -> String {
    config
        .dns_data_dir
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| parent.display().to_string())
        .unwrap_or_else(|| "/".to_string())
}

/// `GET /agent/metrics`, behind node-token authentication.
pub fn router(config: &AgentConfig, auth: Arc<AgentAuth>) -> Router {
    let counters = counters();
    routes(
        Arc::new(ReportSource {
            instance_id: counters.instance_id.clone(),
            proxy: counters.proxy.clone(),
            // Disk health goes out in the heartbeat (`server.rs`), not here.
            host: NodeMetricsCollector::with_scope(NodeMetricsScope::Host).without_disk_health(),
            data_dir: data_dir(config),
        }),
        auth,
    )
}

fn routes(source: Arc<ReportSource>, auth: Arc<AgentAuth>) -> Router {
    Router::new()
        .route(WORKER_METRICS_PATH, get(report))
        .layer(middleware::from_fn(require_agent_auth))
        .layer(Extension(auth))
        .with_state(source)
}

/// Cumulative edge-proxy counters and a host sample for the control plane.
#[utoipa::path(
    get,
    path = "/agent/metrics",
    tag = "Metrics",
    responses(
        (status = 200, description = "Worker metrics report (ADR 061)"),
        (status = 401, description = "Node authentication required")
    ),
    security(("bearer_auth" = []))
)]
async fn report(
    RequireAgentAuth(auth): RequireAgentAuth,
    State(source): State<Arc<ReportSource>>,
) -> Result<Json<WorkerMetricsReport>, Problem> {
    permission_guard!(auth, DeploymentsRead);
    Ok(Json(source.report().await))
}

#[derive(OpenApi)]
#[openapi(paths(report))]
pub struct WorkerMetricsApiDoc;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::any;
    use tower::ServiceExt;

    fn source(proxy: Arc<ProxyMetrics>) -> Arc<ReportSource> {
        Arc::new(ReportSource {
            instance_id: "fixture-instance".to_string(),
            proxy,
            host: NodeMetricsCollector::with_scope(NodeMetricsScope::Host).without_disk_health(),
            data_dir: std::env::temp_dir().display().to_string(),
        })
    }

    async fn get(router: &Router, token: Option<&str>) -> Response {
        let mut request = axum::http::Request::builder().uri(WORKER_METRICS_PATH);
        if let Some(token) = token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        router
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn the_report_needs_this_nodes_token() {
        let router = routes(
            source(Arc::new(ProxyMetrics::default())),
            Arc::new(AgentAuth::new("fixture-node-token")),
        );
        assert_eq!(get(&router, None).await.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            get(&router, Some("another-node-token")).await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            get(&router, Some("fixture-node-token")).await.status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn the_report_carries_counters_and_only_host_names() {
        let proxy = Arc::new(ProxyMetrics::default());
        proxy.record(200, 12, Some(5), RequestDestination::Project, false);
        let router = routes(
            source(proxy.clone()),
            Arc::new(AgentAuth::new("fixture-node-token")),
        );
        let response = get(&router, Some("fixture-node-token")).await;
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let report = temps_metrics::decode_report(1, &body).unwrap();
        assert_eq!(report.version, WORKER_METRICS_VERSION);
        assert_eq!(report.instance_id, "fixture-instance");
        assert_eq!(report.proxy, proxy.snapshot());
        assert!(report
            .host
            .iter()
            .all(|sample| HOST_METRIC_NAMES.contains(&sample.name.as_str())));
        // Process-scope samples (this agent's own fds) never leave the worker.
        assert!(report
            .host
            .iter()
            .all(|sample| !sample.name.starts_with("process.")));
    }

    #[test]
    fn the_data_dir_is_the_parent_of_the_dns_dir() {
        let mut config: AgentConfig = serde_json::from_value(serde_json::json!({
            "listen_address": "127.0.0.1:0",
            "token": "t",
            "node_name": "worker",
            "control_plane_url": "https://cp.test",
            "node_id": 1
        }))
        .unwrap();
        config.dns_data_dir = "/var/lib/temps-agent/dns".into();
        assert_eq!(data_dir(&config), "/var/lib/temps-agent");
        config.dns_data_dir = "dns".into();
        assert_eq!(data_dir(&config), "/");
    }

    async fn serve(router: Router, request: Request) -> Response {
        let response = router.oneshot(request).await.unwrap();
        // Drain the body: the request is recorded when it ends.
        let (parts, body) = response.into_parts();
        let _ = axum::body::to_bytes(body, 1024 * 1024).await;
        Response::from_parts(parts, Body::empty())
    }

    fn requests(metrics: &ProxyMetrics) -> u64 {
        let snapshot = metrics.snapshot();
        snapshot.delta_since(&Default::default()).total_requests()
    }

    #[tokio::test]
    async fn requests_are_counted_with_status_destination_and_upstream() {
        let metrics = Arc::new(ProxyMetrics::default());
        let app = recorded(
            Router::new()
                .route(
                    "/project",
                    any(|req: Request| async move {
                        let recorder = req.extensions().get::<RequestRecorder>().cloned();
                        if let Some(recorder) = recorder {
                            recorder.mark_project();
                            recorder.upstream_answered(Instant::now());
                        }
                        (StatusCode::OK, "hello")
                    }),
                )
                .fallback(|| async { StatusCode::NOT_FOUND }),
            metrics.clone(),
        );
        serve(
            app.clone(),
            axum::http::Request::builder()
                .uri("/project")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        serve(
            app,
            axum::http::Request::builder()
                .uri("/nope")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        let delta = metrics.snapshot().delta_since(&Default::default());
        assert_eq!(delta.total_requests(), 2);
        let samples: std::collections::HashMap<_, _> = delta
            .samples()
            .into_iter()
            .map(|sample| (sample.name, sample.value))
            .collect();
        assert_eq!(samples[temps_metrics::proxy::METRIC_REQUESTS_2XX], 1.0);
        assert_eq!(samples[temps_metrics::proxy::METRIC_REQUESTS_4XX], 1.0);
        assert_eq!(samples[temps_metrics::proxy::METRIC_REQUESTS_PROJECT], 1.0);
        assert_eq!(samples[temps_metrics::proxy::METRIC_REQUESTS_OTHER], 1.0);
        assert_eq!(samples[temps_metrics::proxy::METRIC_REQUESTS_CONSOLE], 0.0);
        assert!(samples.contains_key(temps_metrics::proxy::METRIC_UPSTREAM_AVG));
    }

    #[tokio::test]
    async fn a_request_is_recorded_only_once_its_body_has_ended() {
        let metrics = Arc::new(ProxyMetrics::default());
        let app = recorded(
            Router::new().route("/", any(|| async { "body" })),
            metrics.clone(),
        );
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(requests(&metrics), 0, "still sending the body");
        let _ = axum::body::to_bytes(response.into_body(), 1024).await;
        assert_eq!(requests(&metrics), 1);
    }

    #[tokio::test]
    async fn event_streams_count_as_streaming_sessions() {
        let metrics = Arc::new(ProxyMetrics::default());
        let app = recorded(
            Router::new().route(
                "/events",
                any(|| async { ([(header::CONTENT_TYPE, "text/event-stream")], "data: x\n\n") }),
            ),
            metrics.clone(),
        );
        serve(
            app,
            axum::http::Request::builder()
                .uri("/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        let samples: std::collections::HashMap<_, _> = metrics
            .snapshot()
            .delta_since(&Default::default())
            .samples()
            .into_iter()
            .map(|sample| (sample.name, sample.value))
            .collect();
        assert_eq!(
            samples[temps_metrics::proxy::METRIC_STREAMING_SESSIONS],
            1.0
        );
    }
}
