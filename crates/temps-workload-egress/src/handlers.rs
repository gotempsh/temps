// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What every kind's recording endpoints share, and the node endpoint.
//!
//! Each kind keeps its own URLs, where its users expect them
//! (`/v1/sandboxes/{id}/egress-recording`,
//! `/projects/{project_id}/environments/{environment_id}/egress-recording`),
//! and its own authorization. Those handlers are thin: they resolve the
//! caller's workload in the kind's terms, then call
//! [`EgressRecordingService`] and answer with the types here, so the
//! request body, the query, the responses, the audit entry and every error
//! are the same whatever the kind.
//!
//! `POST /internal/nodes/{node_id}/egress-records` is one endpoint for
//! every kind: a worker's batch, authenticated with the node token and
//! mounted outside the plugin router next to the network-policy report.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use temps_core::problemdetails::{self, Problem};
use temps_workload_netpol::EgressRecordBatch;
use utoipa::ToSchema;

use crate::activity::EgressOutcomeFilter;
use crate::error::EgressRecordingError;
use crate::ingest::EgressIngestOutcome;
use crate::service::{EgressRecordingService, EgressRecordingStatus};

/// The largest batch body a node may send: 512 bounded records fit well
/// inside it.
pub const MAX_BATCH_BODY: usize = 1024 * 1024;

impl From<EgressRecordingError> for Problem {
    fn from(error: EgressRecordingError) -> Self {
        let detail = error.to_string();
        match &error {
            EgressRecordingError::NotFound { noun, .. } => {
                problemdetails::new(StatusCode::NOT_FOUND)
                    .with_title(format!("{} Not Found", capitalized(noun)))
                    .with_detail(detail)
            }
            EgressRecordingError::Unavailable {
                noun,
                id,
                node_id,
                node_name,
                code,
                ..
            } => problemdetails::new(StatusCode::UNPROCESSABLE_ENTITY)
                .with_type("https://temps.sh/probs/egress-recording-unavailable")
                .with_title("Egress Recording Unavailable On This Node")
                .with_detail(detail)
                .with_value("workload", noun.to_string())
                .with_value("workload_id", id.clone())
                .with_value("node_id", *node_id)
                .with_value("node_name", node_name.clone())
                .with_value("reason_code", code.to_string()),
            EgressRecordingError::NotRecordable { noun, id, .. } => {
                problemdetails::new(StatusCode::UNPROCESSABLE_ENTITY)
                    .with_type("https://temps.sh/probs/egress-recording-unavailable")
                    .with_title("Egress Recording Unavailable For This Workload")
                    .with_detail(detail)
                    .with_value("workload", noun.to_string())
                    .with_value("workload_id", id.clone())
                    .with_value("reason_code", crate::service::NOT_RECORDABLE)
            }
            EgressRecordingError::OtelUnavailable { noun, id } => {
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_type("https://temps.sh/probs/egress-recording-unavailable")
                    .with_title("OpenTelemetry Storage Unavailable")
                    .with_detail(detail)
                    .with_value("workload", noun.to_string())
                    .with_value("workload_id", id.clone())
                    .with_value("reason_code", "otel_unavailable")
            }
            EgressRecordingError::KindNotRecorded { kind } => {
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_title("Egress Recording Not Available For This Workload Kind")
                    .with_detail(detail)
                    .with_value("workload_kind", kind.as_str())
            }
            EgressRecordingError::BatchTooLarge { .. } => {
                problemdetails::new(StatusCode::PAYLOAD_TOO_LARGE)
                    .with_title("Egress Record Batch Too Large")
                    .with_detail(detail)
            }
            EgressRecordingError::InvalidRecord { .. } => {
                problemdetails::new(StatusCode::BAD_REQUEST)
                    .with_title("Invalid Egress Record")
                    .with_detail(detail)
            }
            EgressRecordingError::NotPlacedOnNode { .. } => {
                problemdetails::new(StatusCode::FORBIDDEN)
                    .with_title("Workload Not Placed On This Node")
                    .with_detail(detail)
            }
            EgressRecordingError::UnknownWorkload { .. } => {
                problemdetails::new(StatusCode::FORBIDDEN)
                    .with_title("Unknown Workload")
                    .with_detail(detail)
            }
            EgressRecordingError::IngestUnavailable { .. } => {
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_title("OpenTelemetry Storage Unavailable")
                    .with_detail(detail)
            }
            EgressRecordingError::Store { .. } => {
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_title("Egress Records Not Stored")
                    .with_detail(detail)
            }
            EgressRecordingError::Query { noun, id, .. } => {
                problemdetails::new(StatusCode::SERVICE_UNAVAILABLE)
                    .with_title("Recorded Connections Unreadable")
                    .with_detail(detail)
                    .with_value("workload", noun.to_string())
                    .with_value("workload_id", id.clone())
            }
            EgressRecordingError::Database { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Database Error")
                    .with_detail(detail)
            }
        }
    }
}

/// `sandbox` as a title word: `Sandbox`.
fn capitalized(noun: &str) -> String {
    let mut letters = noun.chars();
    match letters.next() {
        Some(first) => first.to_uppercase().chain(letters).collect(),
        None => String::new(),
    }
}

/// Turn recording of the workload's outbound connections on or off.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SetEgressRecordingBody {
    pub enabled: bool,
}

/// Which page of recorded connections, and which outcomes.
#[derive(Debug, Clone, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EgressActivityParams {
    /// Page number, from 1. Default 1.
    pub page: Option<u64>,
    /// Records per page. Default 20, at most 100.
    pub per_page: Option<u64>,
    /// Only connections that ended this way.
    pub outcome: Option<EgressOutcomeFilter>,
}

/// The audit entry of a recording change, the same for every kind: the
/// operation is `<KIND>_EGRESS_RECORDING_UPDATED`.
#[derive(Debug, Clone, Serialize)]
pub struct EgressRecordingUpdatedAudit {
    pub context: temps_core::AuditContext,
    pub workload_kind: String,
    pub workload_id: String,
    pub project_id: Option<i32>,
    pub nodes: Vec<i32>,
    pub from: bool,
    pub to: bool,
}

impl EgressRecordingUpdatedAudit {
    /// The entry for a change by the user in `context` that left the
    /// workload in `status`.
    pub fn new(
        context: temps_core::AuditContext,
        from: bool,
        status: &EgressRecordingStatus,
    ) -> Self {
        Self {
            context,
            workload_kind: status.workload_kind.clone(),
            workload_id: status.workload_id.clone(),
            project_id: status.project_id,
            nodes: status.nodes.iter().map(|node| node.node_id).collect(),
            from,
            to: status.enabled,
        }
    }
}

impl temps_core::AuditOperation for EgressRecordingUpdatedAudit {
    fn operation_type(&self) -> String {
        format!(
            "{}_EGRESS_RECORDING_UPDATED",
            self.workload_kind.to_ascii_uppercase()
        )
    }
    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }
    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }
    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }
    fn serialize(&self) -> temps_core::AuditResult<String> {
        serde_json::to_string(self).map_err(|source| {
            temps_core::AuditError::serialize(
                format!(
                    "EgressRecordingUpdatedAudit for {} {}",
                    self.workload_kind, self.workload_id
                ),
                source,
            )
        })
    }
}

pub struct EgressRecordingNodeState {
    pub service: Arc<EgressRecordingService>,
}

/// The worker endpoint: `POST /internal/nodes/{node_id}/egress-records`.
pub fn node_routes(state: Arc<EgressRecordingNodeState>) -> Router {
    Router::new()
        .route(
            "/internal/nodes/{node_id}/egress-records",
            post(ingest_node_records),
        )
        .layer(DefaultBodyLimit::max(MAX_BATCH_BODY))
        .with_state(state)
}

async fn ingest_node_records(
    State(state): State<Arc<EgressRecordingNodeState>>,
    Path(node_id): Path<i32>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<EgressIngestOutcome>, Problem> {
    // Authenticated before the body is parsed.
    state
        .service
        .authenticate_node(&headers, node_id)
        .await
        .map_err(|(status, detail)| {
            problemdetails::new(status)
                .with_title("Node Authentication Failed")
                .with_detail(detail)
        })?;
    let batch: EgressRecordBatch = serde_json::from_slice(&body).map_err(|e| {
        problemdetails::new(StatusCode::BAD_REQUEST)
            .with_title("Invalid Egress Record Batch")
            .with_detail(format!(
                "egress record batch from node {node_id} is not valid JSON: {e}"
            ))
    })?;
    let outcome = state.service.ingest(node_id, batch).await?;
    Ok(Json(outcome))
}
