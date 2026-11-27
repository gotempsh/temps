// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Storing a node's batch of egress connection records.
//!
//! The node said which workload each record is about (from the source
//! address, inside its proxy). The control plane accepts that only for
//! workloads placed on that node now: a record naming any other workload
//! is dropped and counted ([`EgressIngestOutcome::not_placed`]), with a
//! warning, while the rest of the batch is stored. A workload that moved
//! or was deleted between the connection and the delivery is ordinary, and
//! refusing the whole batch for it would lose every other workload's
//! records with it. Then every workload's opt-in is checked again here,
//! against its row now, so a record taken while recording was on but
//! delivered after it was turned off is dropped, and ownership always comes
//! from the row, never from the node.
//!
//! One implementation for every kind: the kind's adapter only loads the
//! rows ([`crate::WorkloadEgressAdapter::load`]).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::Serialize;
use temps_workload::{WorkloadKind, WorkloadRef};
use temps_workload_netpol::egress::record::MAX_BATCH_RECORDS;
use temps_workload_netpol::EgressRecordBatch;
use utoipa::ToSchema;

use crate::adapter::RecordedWorkload;
use crate::error::EgressRecordingError;
use crate::records::{self, Attribution};
use crate::service::EgressRecordingService;

/// What happened to a batch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct EgressIngestOutcome {
    /// Records stored as log records.
    pub stored: u64,
    /// Records dropped because their workload no longer records (turned
    /// off, destroyed or deleted).
    pub not_recording: u64,
    /// Records dropped because their workload is not placed on the node
    /// that sent them (it moved, or the node is wrong about it).
    pub not_placed: u64,
    /// Records dropped because their project's telemetry storage quota is
    /// used up, as its own OTLP logs are refused then.
    pub over_quota: u64,
    /// Records dropped because their project's quota could not be checked:
    /// a failed check never stores past a quota that may be used up.
    pub quota_unchecked: u64,
}

/// A project's telemetry quota, as checked for one batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quota {
    Admits,
    Exceeded,
    /// The check failed: treated as exceeded, counted apart.
    Unchecked,
}

impl EgressRecordingService {
    /// A slot of the telemetry ingest limit for a batch from `node_id`,
    /// taken before its body is read. None when there is no limit (or no
    /// store: [`Self::ingest`] then says so).
    pub fn try_acquire_ingest_permit(
        &self,
        node_id: i32,
    ) -> Result<Option<tokio::sync::OwnedSemaphorePermit>, EgressRecordingError> {
        let Some(store) = self.store.as_ref() else {
            return Ok(None);
        };
        store
            .try_acquire_ingest_permit()
            .map_err(|error| match error {
                temps_otel::OtelError::IngestSaturated { limit } => {
                    EgressRecordingError::IngestSaturated { node_id, limit }
                }
                source => EgressRecordingError::Store {
                    node_id,
                    records: 0,
                    source,
                },
            })
    }

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
        let mut ids: BTreeMap<WorkloadKind, BTreeSet<i64>> = BTreeMap::new();
        for record in &batch.records {
            records::validate(record).map_err(|reason| EgressRecordingError::InvalidRecord {
                node_id,
                reason: format!("record for {}: {reason}", record.workload),
            })?;
            ids.entry(record.workload.kind)
                .or_default()
                .insert(record.workload.id);
        }
        if batch.dropped > 0 {
            tracing::warn!(
                node_id,
                dropped = batch.dropped,
                "node dropped egress records before delivering them (its recording channel was \
                 full or a delivery failed); the record of its workloads' connections is \
                 incomplete"
            );
        }
        if batch.records.is_empty() {
            return Ok(EgressIngestOutcome::default());
        }

        // Every workload the batch names, from its own kind's tables. A
        // kind nothing records for is a record this control plane never
        // asked for.
        let mut rows: HashMap<WorkloadRef, (RecordedWorkload, &'static str)> = HashMap::new();
        for (kind, kind_ids) in &ids {
            let adapter =
                self.adapter(*kind)
                    .map_err(|error| EgressRecordingError::InvalidRecord {
                        node_id,
                        reason: error.to_string(),
                    })?;
            let wanted: Vec<i64> = kind_ids.iter().copied().collect();
            for row in adapter.load(&wanted).await? {
                rows.insert(row.workload, (row, adapter.id_attribute()));
            }
        }

        // Placement, for every workload: a record of one the node does not
        // host now is never stored, whatever else the batch holds.
        let mut not_placed: BTreeSet<WorkloadRef> = BTreeSet::new();
        for (workload, (row, _)) in &rows {
            if !row.nodes.contains(&node_id) {
                tracing::warn!(
                    node_id,
                    %workload,
                    id = %row.public_id,
                    placed_on = ?row.nodes,
                    "dropped egress records from a node for a workload it does not host"
                );
                not_placed.insert(*workload);
            }
        }

        let node_name = self
            .node_names(&[node_id])
            .await?
            .remove(&node_id)
            .unwrap_or_default();
        let mut outcome = EgressIngestOutcome::default();
        let mut logs = Vec::with_capacity(batch.records.len());
        for record in &batch.records {
            let Some((row, id_attribute)) = rows.get(&record.workload) else {
                // Deleted since the connection.
                outcome.not_recording += 1;
                continue;
            };
            if not_placed.contains(&record.workload) {
                outcome.not_placed += 1;
                continue;
            }
            if !(row.enabled && row.live) {
                outcome.not_recording += 1;
                continue;
            }
            logs.push(records::to_log_record(
                record,
                &Attribution {
                    project_id: row.project_id,
                    workload: row.workload,
                    public_id: row.public_id.clone(),
                    id_attribute,
                    node_id,
                    node_name: node_name.clone(),
                },
            ));
        }
        if outcome.not_recording > 0 {
            tracing::debug!(
                node_id,
                dropped = outcome.not_recording,
                "dropped egress records of workloads that do not record"
            );
        }
        if logs.is_empty() {
            return Ok(outcome);
        }
        let store = self
            .store
            .as_ref()
            .ok_or(EgressRecordingError::IngestUnavailable {
                node_id,
                records: logs.len(),
            })?;
        // A project whose telemetry quota is used up stores no more of
        // these, as it stores no more of its own logs. One (cached) check
        // per project in the batch; a workload without a project has none.
        let mut admitted: HashMap<i32, Quota> = HashMap::new();
        for project_id in logs.iter().filter_map(|log| log.project_id) {
            if admitted.contains_key(&project_id) {
                continue;
            }
            let quota = match store.admits(project_id).await {
                Ok(true) => Quota::Admits,
                Ok(false) => Quota::Exceeded,
                Err(error) => {
                    // Fail closed: storing could go past a quota that is
                    // used up, so the project's records are dropped and
                    // counted, as when it is over its quota.
                    tracing::warn!(
                        node_id,
                        project_id,
                        %error,
                        "egress recording: could not check the project's telemetry quota; \
                         its records in this batch are dropped"
                    );
                    Quota::Unchecked
                }
            };
            admitted.insert(project_id, quota);
        }
        logs.retain(|log| {
            let quota = log
                .project_id
                .and_then(|project_id| admitted.get(&project_id).copied())
                .unwrap_or(Quota::Admits);
            match quota {
                Quota::Admits => true,
                Quota::Exceeded => {
                    outcome.over_quota += 1;
                    false
                }
                Quota::Unchecked => {
                    outcome.quota_unchecked += 1;
                    false
                }
            }
        });
        if outcome.over_quota > 0 {
            tracing::warn!(
                node_id,
                dropped = outcome.over_quota,
                "dropped egress records of projects over their telemetry storage quota"
            );
        }
        if logs.is_empty() {
            return Ok(outcome);
        }
        let count = logs.len();
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
