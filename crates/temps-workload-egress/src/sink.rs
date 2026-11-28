// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Node 0's record sink: the control plane's own egress proxy hands its
//! batches straight to the ingest service, with no HTTP hop and no token.
//! Workers deliver theirs over `POST /internal/nodes/{id}/egress-records`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use temps_workload_netpol::{Delivered, EgressRecordBatch, EgressRecordSink, RecordSinkError};

use crate::error::EgressRecordingError;
use crate::service::{EgressRecordingService, CONTROL_PLANE_NODE_ID};

/// How long a batch waits for a slot of the telemetry ingest limit before
/// it is given up (dropped and counted, as a refused worker batch is).
pub const PERMIT_WAIT: Duration = Duration::from_secs(2);

/// How often a waiting batch tries for a slot again.
const PERMIT_RETRY: Duration = Duration::from_millis(50);

const DESTINATION: &str = "the control plane's egress record ingest";

/// Delivers node 0's batches to [`EgressRecordingService::ingest`], each
/// holding a slot of the process-wide telemetry ingest limit for as long
/// as it is stored, as OTLP requests and worker batches do: the control
/// plane's own recording never adds a write past that limit.
pub struct LocalRecordSink {
    service: Arc<EgressRecordingService>,
    permit_wait: Duration,
}

impl LocalRecordSink {
    pub fn new(service: Arc<EgressRecordingService>) -> Self {
        Self::with_permit_wait(service, PERMIT_WAIT)
    }

    /// [`Self::new`], waiting at most `permit_wait` for an ingest slot.
    pub fn with_permit_wait(service: Arc<EgressRecordingService>, permit_wait: Duration) -> Self {
        Self {
            service,
            permit_wait,
        }
    }

    /// A slot of the telemetry ingest limit, waiting up to `permit_wait`
    /// while every slot is taken.
    async fn permit(
        &self,
        records: usize,
    ) -> Result<Option<tokio::sync::OwnedSemaphorePermit>, RecordSinkError> {
        let deadline = tokio::time::Instant::now() + self.permit_wait;
        loop {
            match self
                .service
                .try_acquire_ingest_permit(CONTROL_PLANE_NODE_ID)
            {
                Ok(permit) => return Ok(permit),
                Err(EgressRecordingError::IngestSaturated { .. })
                    if tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(PERMIT_RETRY).await;
                }
                Err(error) => {
                    return Err(RecordSinkError::Delivery {
                        destination: DESTINATION.to_string(),
                        records,
                        reason: error.to_string(),
                    })
                }
            }
        }
    }
}

#[async_trait]
impl EgressRecordSink for LocalRecordSink {
    async fn deliver(&self, batch: EgressRecordBatch) -> Result<Delivered, RecordSinkError> {
        let records = batch.records.len();
        let _permit = self.permit(records).await?;
        self.service
            .ingest(CONTROL_PLANE_NODE_ID, batch)
            .await
            // What ingest no longer stores (opted out, deleted, moved) is
            // discarded, not delivered.
            .map(|outcome| Delivered {
                discarded: outcome.not_recording
                    + outcome.not_placed
                    + outcome.over_quota
                    + outcome.quota_unchecked,
                ..Default::default()
            })
            .map_err(|error| RecordSinkError::Delivery {
                destination: DESTINATION.to_string(),
                records,
                reason: error.to_string(),
            })
    }
}
