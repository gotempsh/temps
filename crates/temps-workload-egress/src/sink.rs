// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Node 0's record sink: the control plane's own egress proxy hands its
//! batches straight to the ingest service, with no HTTP hop and no token.
//! Workers deliver theirs over `POST /internal/nodes/{id}/egress-records`.

use std::sync::Arc;

use async_trait::async_trait;
use temps_workload_netpol::{Delivered, EgressRecordBatch, EgressRecordSink, RecordSinkError};

use crate::service::{EgressRecordingService, CONTROL_PLANE_NODE_ID};

/// Delivers node 0's batches to [`EgressRecordingService::ingest`].
pub struct LocalRecordSink {
    service: Arc<EgressRecordingService>,
}

impl LocalRecordSink {
    pub fn new(service: Arc<EgressRecordingService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl EgressRecordSink for LocalRecordSink {
    async fn deliver(&self, batch: EgressRecordBatch) -> Result<Delivered, RecordSinkError> {
        let records = batch.records.len();
        self.service
            .ingest(CONTROL_PLANE_NODE_ID, batch)
            .await
            // What ingest no longer stores (opted out, deleted, moved) is
            // discarded, not delivered.
            .map(|outcome| Delivered {
                discarded: outcome.not_recording + outcome.not_placed + outcome.over_quota,
                ..Default::default()
            })
            .map_err(|error| RecordSinkError::Delivery {
                destination: "the control plane's egress record ingest".to_string(),
                records,
                reason: error.to_string(),
            })
    }
}
