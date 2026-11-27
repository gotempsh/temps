// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Why recording, ingest or reading records failed.

use temps_otel::OtelError;
use temps_workload::WorkloadKind;

#[derive(Debug, thiserror::Error)]
pub enum EgressRecordingError {
    #[error("{noun} {id} not found")]
    NotFound { noun: &'static str, id: String },

    #[error(
        "Recording outbound connections is unavailable for {noun} {id} on node {node_id} \
         ('{node_name}'): {reason}"
    )]
    Unavailable {
        noun: &'static str,
        id: String,
        node_id: i32,
        node_name: String,
        code: &'static str,
        reason: String,
    },

    #[error("Recording outbound connections is unavailable for {noun} {id}: {reason}")]
    NotRecordable {
        noun: &'static str,
        id: String,
        reason: String,
    },

    #[error(
        "Recording outbound connections is unavailable for {noun} {id}: OpenTelemetry storage \
         is not enabled on this control plane"
    )]
    OtelUnavailable { noun: &'static str, id: String },

    #[error("outbound connections of {kind} workloads are not recorded by this control plane")]
    KindNotRecorded { kind: WorkloadKind },

    #[error(
        "node {node_id} sent {records} egress records in one batch; at most {max} are accepted"
    )]
    BatchTooLarge {
        node_id: i32,
        records: usize,
        max: usize,
    },

    #[error("node {node_id} sent an invalid egress record: {reason}")]
    InvalidRecord { node_id: i32, reason: String },

    #[error(
        "OpenTelemetry storage is not enabled on this control plane; {records} egress records \
         from node {node_id} cannot be stored"
    )]
    IngestUnavailable { node_id: i32, records: usize },

    #[error(
        "OpenTelemetry storage is not enabled on this control plane; the egress record batch \
         from node {node_id} was refused before its body was read"
    )]
    IngestDisabled { node_id: i32 },

    #[error(
        "egress records from node {node_id} not accepted: telemetry ingest is at its limit \
         of {limit} concurrent requests; the node retries with its next batch"
    )]
    IngestSaturated { node_id: i32, limit: usize },

    #[error("storing {records} egress records from node {node_id} failed: {source}")]
    Store {
        node_id: i32,
        records: usize,
        #[source]
        source: OtelError,
    },

    #[error("reading the recorded connections of {noun} {id} failed: {source}")]
    Query {
        noun: &'static str,
        id: String,
        #[source]
        source: OtelError,
    },

    #[error("database error while {operation}: {source}")]
    Database {
        operation: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },
}

impl EgressRecordingError {
    /// Maps a database error, naming the operation that failed.
    pub fn db(operation: &'static str) -> impl FnOnce(sea_orm::DbErr) -> Self {
        move |source| Self::Database { operation, source }
    }
}
