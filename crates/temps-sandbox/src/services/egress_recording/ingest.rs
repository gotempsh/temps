// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Storing a node's batch of egress connection records.
//!
//! The node said which workload each record is about (from the source
//! address, inside its proxy). The control plane accepts that only for
//! sandboxes placed on that node: a batch naming any other sandbox is
//! refused whole, since a node that does that is broken or compromised.
//! Then every sandbox's opt-in is checked again here, against its row now,
//! so a record taken while recording was on but delivered after it was
//! turned off is dropped, and project attribution always comes from the
//! row, never from the node.

use std::collections::{HashMap, HashSet};

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};
use serde::Serialize;
use temps_entities::sandboxes;
use temps_workload::WorkloadKind;
use temps_workload_netpol::egress::record::MAX_BATCH_RECORDS;
use temps_workload_netpol::EgressRecordBatch;
use utoipa::ToSchema;

use super::records::{self, Attribution};
use super::{EgressRecordingError, EgressRecordingService, CONTROL_PLANE_NODE_ID};

/// What happened to a batch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct EgressIngestOutcome {
    /// Records stored as log records.
    pub stored: u64,
    /// Records dropped because their sandbox no longer records (turned off,
    /// destroyed, or without a project).
    pub not_recording: u64,
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct SandboxAttribution {
    id: i32,
    public_id: String,
    node_id: Option<i32>,
    project_id: Option<i32>,
    egress_recording: bool,
    status: String,
}

impl EgressRecordingService {
    /// Checks `headers` carry `node_id`'s token and the node is active, the
    /// same check every node endpoint makes. The error is the status and
    /// detail to answer with.
    pub async fn authenticate_node(
        &self,
        headers: &axum::http::HeaderMap,
        node_id: i32,
    ) -> Result<(), (axum::http::StatusCode, String)> {
        temps_routes::route_sync::authenticate_node(self.db.as_ref(), headers, node_id)
            .await
            .map(|_| ())
    }

    /// Stores `batch` from `node_id`, which the caller has authenticated
    /// (node token, or node 0 itself).
    pub async fn ingest(
        &self,
        node_id: i32,
        batch: EgressRecordBatch,
    ) -> Result<EgressIngestOutcome, EgressRecordingError> {
        if batch.records.len() > MAX_BATCH_RECORDS {
            return Err(EgressRecordingError::BatchTooLarge {
                node_id,
                records: batch.records.len(),
                max: MAX_BATCH_RECORDS,
            });
        }
        let mut ids = HashSet::new();
        for record in &batch.records {
            if record.workload.kind != WorkloadKind::Sandbox {
                return Err(EgressRecordingError::InvalidRecord {
                    node_id,
                    reason: format!(
                        "workload {} is not a sandbox; only sandboxes record egress",
                        record.workload
                    ),
                });
            }
            records::validate(record).map_err(|reason| EgressRecordingError::InvalidRecord {
                node_id,
                reason: format!("record for {}: {reason}", record.workload),
            })?;
            ids.insert(record.workload.id);
        }
        if batch.dropped > 0 {
            tracing::warn!(
                node_id,
                dropped = batch.dropped,
                "node dropped egress records before delivering them (its recording channel was \
                 full or a delivery failed); the record of its sandboxes' connections is \
                 incomplete"
            );
        }
        if batch.records.is_empty() {
            return Ok(EgressIngestOutcome::default());
        }

        let internal_ids: Vec<i32> = ids
            .iter()
            .filter_map(|id| i32::try_from(*id).ok())
            .collect();
        if let Some(out_of_range) = ids.iter().find(|id| i32::try_from(**id).is_err()) {
            return Err(EgressRecordingError::UnknownSandbox {
                node_id,
                sandbox: *out_of_range,
            });
        }
        let rows: HashMap<i64, SandboxAttribution> = sandboxes::Entity::find()
            .select_only()
            .columns([
                sandboxes::Column::Id,
                sandboxes::Column::PublicId,
                sandboxes::Column::NodeId,
                sandboxes::Column::ProjectId,
                sandboxes::Column::EgressRecording,
                sandboxes::Column::Status,
            ])
            .filter(sandboxes::Column::Id.is_in(internal_ids))
            .into_model::<SandboxAttribution>()
            .all(self.db.as_ref())
            .await
            .map_err(EgressRecordingError::db(
                "loading the sandboxes named by a batch of egress records",
            ))?
            .into_iter()
            .map(|row| (i64::from(row.id), row))
            .collect();

        // Placement first, for every sandbox, before anything is stored.
        for id in &ids {
            let row = rows.get(id).ok_or(EgressRecordingError::UnknownSandbox {
                node_id,
                sandbox: *id,
            })?;
            if row.node_id.unwrap_or(CONTROL_PLANE_NODE_ID) != node_id {
                tracing::warn!(
                    node_id,
                    sandbox = row.public_id,
                    placed_on = row.node_id.unwrap_or(CONTROL_PLANE_NODE_ID),
                    "refused egress records from a node for a sandbox it does not host"
                );
                return Err(EgressRecordingError::NotPlacedOnNode {
                    node_id,
                    sandbox: *id,
                });
            }
        }

        let node_name = self.node_name(node_id).await?;
        let mut outcome = EgressIngestOutcome::default();
        let mut logs = Vec::with_capacity(batch.records.len());
        for record in &batch.records {
            let Some(row) = rows.get(&record.workload.id) else {
                continue;
            };
            let project_id = match row.project_id {
                Some(project_id) if row.egress_recording && row.status != "destroyed" => project_id,
                _ => {
                    outcome.not_recording += 1;
                    continue;
                }
            };
            logs.push(records::to_log_record(
                record,
                &Attribution {
                    project_id,
                    sandbox_public_id: row.public_id.clone(),
                    node_id,
                    node_name: node_name.clone(),
                },
            ));
        }
        if outcome.not_recording > 0 {
            tracing::debug!(
                node_id,
                dropped = outcome.not_recording,
                "dropped egress records of sandboxes that do not record"
            );
        }
        if logs.is_empty() {
            return Ok(outcome);
        }
        let count = logs.len();
        let store = self
            .store
            .as_ref()
            .ok_or(EgressRecordingError::IngestUnavailable {
                node_id,
                records: count,
            })?;
        outcome.stored = store
            .store(logs)
            .await
            .map_err(|source| EgressRecordingError::Store {
                node_id,
                records: count,
                source,
            })?;
        Ok(outcome)
    }
}
