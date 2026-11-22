// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Control-plane side of worker metrics (ADR 061).
//!
//! On every `monitoring.scrape_interval_secs` tick the control plane pulls
//! `GET /agent/metrics` from each active or draining worker over the same
//! authenticated agent channel deployments use, and stores the result under
//! the worker's node id. Everything is stamped with the control plane's
//! receive time; [`temps_metrics::accept_report`] applies the bounds.
//!
//! An agent that predates the endpoint answers 404. That is recorded as
//! [`WorkerReportState::AgentTooOld`] on the status board (which the console
//! shows) and logged once, never as an error per interval. A single-node
//! install has no worker rows, so this loop does nothing there.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::StreamExt;
use reqwest::StatusCode;
use sea_orm::{DatabaseConnection, EntityTrait};
use temps_config::ConfigService;
use temps_core::EncryptionService;
use temps_entities::nodes;
use temps_metrics::worker_report::MAX_REPORT_BYTES;
use temps_metrics::{
    accept_report, decode_report, MetricPoint, MetricsStore, WorkerBaseline, WorkerMetricsReport,
    WorkerMetricsStatusBoard, WorkerReportState, WORKER_METRICS_PATH,
};
use tracing::{debug, info, warn};

/// Floor and fallback for the pull interval, matching the plane's own
/// node and proxy samplers.
const MIN_INTERVAL_SECS: u64 = 15;
const DEFAULT_INTERVAL_SECS: u64 = 30;

/// One pull may take at most this long, connect included.
const PULL_TIMEOUT: Duration = Duration::from_secs(5);

/// Workers pulled at once.
const PULL_CONCURRENCY: usize = 8;

/// Why a pull produced nothing to store.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum PullError {
    #[error("The agent has no metrics endpoint (HTTP {status}); upgrade the agent on this node")]
    AgentTooOld { status: u16 },
    #[error("The agent refused the control plane's node token (HTTP {status})")]
    Rejected { status: u16 },
    #[error("The agent could not be reached: {reason}")]
    Unreachable { reason: String },
    #[error("The node is {status}; it is not pulled until it is active again")]
    NotPulled { status: String },
    #[error("The agent answered HTTP {status} to the metrics request")]
    Unexpected { status: u16 },
    #[error("{0}")]
    Report(#[from] temps_metrics::WorkerReportError),
    #[error("The node's agent token is unavailable: {reason}")]
    Token { reason: String },
}

impl PullError {
    fn state(&self) -> WorkerReportState {
        match self {
            PullError::AgentTooOld { .. } => WorkerReportState::AgentTooOld,
            PullError::Rejected { .. } | PullError::Token { .. } => WorkerReportState::Rejected,
            PullError::Unreachable { .. } | PullError::NotPulled { .. } => {
                WorkerReportState::Unreachable
            }
            PullError::Unexpected { status } if *status >= 500 => WorkerReportState::Unreachable,
            PullError::Unexpected { .. } | PullError::Report(_) => WorkerReportState::Invalid,
        }
    }
}

/// Map a non-success agent status to why nothing was stored.
fn status_error(status: StatusCode) -> PullError {
    match status {
        StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED => PullError::AgentTooOld {
            status: status.as_u16(),
        },
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => PullError::Rejected {
            status: status.as_u16(),
        },
        other => PullError::Unexpected {
            status: other.as_u16(),
        },
    }
}

/// Read at most [`MAX_REPORT_BYTES`] + 1 bytes: enough to tell a report that
/// is too large without buffering whatever a worker chooses to send.
async fn read_capped(node_id: i32, mut response: reqwest::Response) -> Result<Vec<u8>, PullError> {
    if let Some(length) = response.content_length() {
        let size = usize::try_from(length).unwrap_or(usize::MAX);
        if size > MAX_REPORT_BYTES {
            return Err(temps_metrics::WorkerReportError::TooLarge {
                node_id,
                size,
                max: MAX_REPORT_BYTES,
            }
            .into());
        }
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| PullError::Unreachable {
        reason: e.to_string(),
    })? {
        let room = (MAX_REPORT_BYTES + 1).saturating_sub(body.len());
        body.extend_from_slice(&chunk[..chunk.len().min(room)]);
        if body.len() > MAX_REPORT_BYTES {
            break;
        }
    }
    Ok(body)
}

/// Pulls worker reports into the metrics store and keeps the status board.
pub struct WorkerMetricsCollector {
    db: Arc<DatabaseConnection>,
    store: Arc<dyn MetricsStore>,
    config_service: Arc<ConfigService>,
    encryption_service: Arc<EncryptionService>,
    board: Arc<WorkerMetricsStatusBoard>,
}

/// One node's pull: id, name, what came back, and the receive time.
type PullOutcome = (
    i32,
    String,
    Result<WorkerMetricsReport, PullError>,
    DateTime<Utc>,
);

/// Per-node state kept between pulls.
#[derive(Default)]
struct Pulls {
    baselines: HashMap<i32, Option<WorkerBaseline>>,
    /// Nodes already warned about refused host samples; cleared by a clean
    /// report, so a misbehaving agent is logged once, not every interval.
    warned_rejects: HashSet<i32>,
}

impl WorkerMetricsCollector {
    pub fn new(
        db: Arc<DatabaseConnection>,
        store: Arc<dyn MetricsStore>,
        config_service: Arc<ConfigService>,
        encryption_service: Arc<EncryptionService>,
        board: Arc<WorkerMetricsStatusBoard>,
    ) -> Self {
        Self {
            db,
            store,
            config_service,
            encryption_service,
            board,
        }
    }

    /// Pull forever. Spawn on a background task.
    pub async fn run(self) {
        let mut pulls = Pulls::default();
        loop {
            let interval = match self.config_service.get_settings().await {
                Ok(settings) => settings
                    .monitoring
                    .scrape_interval_secs
                    .max(MIN_INTERVAL_SECS),
                Err(e) => {
                    debug!(
                        "Worker metrics: cannot read settings ({e}); using the default interval"
                    );
                    DEFAULT_INTERVAL_SECS
                }
            };
            tokio::time::sleep(Duration::from_secs(interval)).await;
            self.pull_all(&mut pulls).await;
        }
    }

    async fn pull_all(&self, pulls: &mut Pulls) {
        let nodes = match nodes::Entity::find().all(self.db.as_ref()).await {
            Ok(nodes) => nodes,
            Err(e) => {
                debug!("Worker metrics: cannot list nodes ({e}); skipping this interval");
                return;
            }
        };
        // Node 0 is the control plane itself; pending nodes have no agent yet.
        let nodes: Vec<nodes::Model> = nodes
            .into_iter()
            .filter(|node| node.id > 0 && node.status != "pending")
            .collect();
        let current: HashSet<i32> = nodes.iter().map(|node| node.id).collect();

        let outcomes: Vec<PullOutcome> = futures::stream::iter(nodes)
            .map(|node| async move {
                let result = if node.status == "active" || node.status == "draining" {
                    self.pull(&node).await
                } else {
                    Err(PullError::NotPulled {
                        status: node.status.clone(),
                    })
                };
                // Receive time: taken after the body was read.
                (node.id, node.name, result, Utc::now())
            })
            .buffer_unordered(PULL_CONCURRENCY)
            .collect()
            .await;

        let mut points = Vec::new();
        for (node_id, name, result, received_at) in outcomes {
            points.extend(apply(
                &self.board,
                pulls,
                node_id,
                &name,
                result,
                received_at,
            ));
        }

        pulls.baselines.retain(|id, _| current.contains(id));
        pulls.warned_rejects.retain(|id| current.contains(id));
        self.board.retain(&current);

        if points.is_empty() {
            return;
        }
        if let Err(e) = self.store.write_batch(points).await {
            // Same contract as the plane's own samplers: the next interval
            // supersedes a lost write.
            warn!("Worker metrics: write_batch failed (non-fatal): {e}");
        }
    }

    async fn pull(&self, node: &nodes::Model) -> Result<WorkerMetricsReport, PullError> {
        let token = self.token(node)?;
        let endpoint = crate::cluster_ca::node_agent_endpoint(node);
        let client = crate::cluster_ca::build_node_http_client(
            &endpoint,
            self.config_service.as_ref(),
            self.encryption_service.as_ref(),
            Some(PULL_TIMEOUT),
        )
        .await
        .map_err(|e| PullError::Unreachable {
            reason: e.to_string(),
        })?;
        let url = format!(
            "{}{}",
            endpoint.dial_url.trim_end_matches('/'),
            WORKER_METRICS_PATH
        );
        let fetch = async {
            let response = client
                .get(&url)
                .bearer_auth(&token)
                .send()
                .await
                .map_err(|e| PullError::Unreachable {
                    reason: e.to_string(),
                })?;
            if !response.status().is_success() {
                return Err(status_error(response.status()));
            }
            read_capped(node.id, response).await
        };
        let body = tokio::time::timeout(PULL_TIMEOUT, fetch)
            .await
            .map_err(|_| PullError::Unreachable {
                reason: format!("no answer within {}s", PULL_TIMEOUT.as_secs()),
            })??;
        Ok(decode_report(node.id, &body)?)
    }

    fn token(&self, node: &nodes::Model) -> Result<String, PullError> {
        let encrypted = node
            .token_encrypted
            .as_ref()
            .ok_or_else(|| PullError::Token {
                reason: "the node has no stored agent token".to_string(),
            })?;
        let bytes = self
            .encryption_service
            .decrypt(encrypted)
            .map_err(|e| PullError::Token {
                reason: format!("cannot decrypt it: {e}"),
            })?;
        String::from_utf8(bytes).map_err(|_| PullError::Token {
            reason: "it is not valid UTF-8".to_string(),
        })
    }
}

/// Fold one pull outcome into the board and the per-node baseline, and
/// return the points to store. Logs state changes once.
fn apply(
    board: &WorkerMetricsStatusBoard,
    pulls: &mut Pulls,
    node_id: i32,
    name: &str,
    result: Result<WorkerMetricsReport, PullError>,
    received_at: DateTime<Utc>,
) -> Vec<MetricPoint> {
    match result {
        Ok(report) => {
            let baseline = pulls.baselines.entry(node_id).or_default();
            let accepted = accept_report(node_id, report, baseline, received_at);
            if accepted.rejected > 0 {
                if pulls.warned_rejects.insert(node_id) {
                    warn!(
                        node_id,
                        node = %name,
                        rejected = accepted.rejected,
                        examples = ?accepted.rejected_examples,
                        "Worker metrics: dropped host samples this control plane does not accept \
                         (unknown name, duplicate or out of range); the rest were stored"
                    );
                }
            } else {
                pulls.warned_rejects.remove(&node_id);
            }
            let previous = board.state_of(node_id);
            board.record_report(node_id, received_at);
            if previous.is_some_and(|state| state != WorkerReportState::Reporting) {
                info!(node_id, node = %name, "Worker metrics: node is reporting again");
            }
            accepted.points
        }
        Err(error) => {
            let state = error.state();
            let detail = error.to_string();
            let previous = board.record_failure(node_id, state, detail.clone(), received_at);
            if previous != Some(state) {
                match state {
                    WorkerReportState::AgentTooOld => info!(
                        node_id,
                        node = %name,
                        "Worker metrics: the agent on this node is too old to report metrics; \
                         upgrade it to see its proxy and host metrics"
                    ),
                    _ => warn!(node_id, node = %name, "Worker metrics: {detail}"),
                }
            }
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use temps_metrics::proxy::{ProxyMetrics, RequestDestination, METRIC_REQUESTS};
    use temps_metrics::worker_report::WORKER_METRICS_VERSION;
    use temps_metrics::HostSample;

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_800_000_000 + secs, 0).unwrap()
    }

    fn report(proxy: &ProxyMetrics) -> WorkerMetricsReport {
        WorkerMetricsReport {
            version: WORKER_METRICS_VERSION,
            instance_id: "agent-a".to_string(),
            proxy: proxy.snapshot(),
            host: vec![HostSample {
                name: "node.load_avg_1m".to_string(),
                value: 0.5,
            }],
        }
    }

    #[test]
    fn old_agents_are_too_old_not_errors() {
        assert_eq!(
            status_error(StatusCode::NOT_FOUND).state(),
            WorkerReportState::AgentTooOld
        );
        assert_eq!(
            status_error(StatusCode::METHOD_NOT_ALLOWED).state(),
            WorkerReportState::AgentTooOld
        );
        assert_eq!(
            status_error(StatusCode::UNAUTHORIZED).state(),
            WorkerReportState::Rejected
        );
        assert_eq!(
            status_error(StatusCode::BAD_GATEWAY).state(),
            WorkerReportState::Unreachable
        );
        assert_eq!(
            status_error(StatusCode::BAD_REQUEST).state(),
            WorkerReportState::Invalid
        );
    }

    #[test]
    fn a_failed_pull_keeps_the_baseline_and_stores_nothing() {
        let board = WorkerMetricsStatusBoard::new();
        let mut pulls = Pulls::default();
        let proxy = ProxyMetrics::default();

        let first = apply(&board, &mut pulls, 7, "w", Ok(report(&proxy)), at(0));
        assert!(
            first.iter().all(|p| p.name != METRIC_REQUESTS),
            "the first report only sets the baseline"
        );
        assert!(first.iter().all(|p| p.time == at(0) && p.source_id == 7));
        assert_eq!(board.state_of(7), Some(WorkerReportState::Reporting));

        proxy.record(200, 3, None, RequestDestination::Project, false);
        let lost = apply(
            &board,
            &mut pulls,
            7,
            "w",
            Err(PullError::Unreachable {
                reason: "timeout".into(),
            }),
            at(30),
        );
        assert!(lost.is_empty());
        assert_eq!(board.state_of(7), Some(WorkerReportState::Unreachable));

        proxy.record(200, 3, None, RequestDestination::Project, false);
        let next = apply(&board, &mut pulls, 7, "w", Ok(report(&proxy)), at(60));
        let requests = next
            .iter()
            .find(|p| p.name == METRIC_REQUESTS)
            .map(|p| (p.value, p.time));
        assert_eq!(
            requests,
            Some((2.0, at(60))),
            "the lost pull widened the next interval"
        );
        assert_eq!(board.state_of(7), Some(WorkerReportState::Reporting));
    }

    #[test]
    fn an_old_agent_shows_as_too_old_on_the_board() {
        let board = WorkerMetricsStatusBoard::new();
        let mut pulls = Pulls::default();
        let points = apply(
            &board,
            &mut pulls,
            3,
            "old",
            Err(status_error(StatusCode::NOT_FOUND)),
            at(0),
        );
        assert!(points.is_empty());
        let (_, status) = board.snapshot().into_iter().next().unwrap();
        assert_eq!(status.state, WorkerReportState::AgentTooOld);
        assert_eq!(status.last_report_at, None);
        assert!(status.detail.unwrap().contains("upgrade the agent"));
    }

    #[tokio::test]
    async fn oversized_bodies_are_refused_without_buffering_them() {
        // Streamed without a length: read up to one byte past the bound.
        let chunks: Vec<Result<Vec<u8>, std::io::Error>> =
            (0..64).map(|_| Ok(vec![b'x'; 1024])).collect();
        let streamed = reqwest::Body::wrap_stream(futures::stream::iter(chunks));
        let response = reqwest::Response::from(axum::http::Response::new(streamed));
        let body = read_capped(1, response).await.unwrap();
        assert_eq!(body.len(), MAX_REPORT_BYTES + 1);
        assert!(matches!(
            decode_report(1, &body),
            Err(temps_metrics::WorkerReportError::TooLarge { .. })
        ));

        let declared = axum::http::Response::builder()
            .header("content-length", (MAX_REPORT_BYTES + 1).to_string())
            .body(vec![b'x'; MAX_REPORT_BYTES + 1])
            .unwrap();
        assert!(matches!(
            read_capped(1, reqwest::Response::from(declared)).await,
            Err(PullError::Report(
                temps_metrics::WorkerReportError::TooLarge { .. }
            ))
        ));
    }
}
