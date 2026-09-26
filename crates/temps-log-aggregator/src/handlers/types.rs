// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::Arc;

use sea_orm::DatabaseConnection;
use temps_core::AuditLogger;

use crate::index::LineIndex;
use crate::services::{
    ChunkWriterService, LogMetadataService, LogSearchService, RetentionService, TailService,
};
use crate::store::manifest::ManifestRepo;
use crate::store::LogLineStore;

/// Shared state for log aggregator HTTP handlers
pub struct LogAggregatorAppState {
    pub search_service: Arc<LogSearchService>,
    pub metadata_service: Arc<LogMetadataService>,
    pub tail_service: Arc<TailService>,
    pub retention_service: Arc<RetentionService>,
    pub audit_service: Arc<dyn AuditLogger>,
    /// The ADR-046 chunk-backed log line store, used directly by handlers
    /// that don't go through `LogSearchService` (e.g. the purge endpoint) and
    /// by `resolve_log_access_scope` callers that need the raw connection.
    pub store: Arc<dyn LogLineStore>,
    /// Control-plane database connection, used to resolve the caller's
    /// authorization allow-list (`resolve_log_access_scope`) directly in
    /// handlers that don't already have it via a service.
    pub db: Arc<DatabaseConnection>,
    /// Optional checker for team-based project access (human sessions only).
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    /// ADR-047 line index — the capabilities endpoint reads its
    /// `unavailable_reason`, and the global analytics/attribute-filtered
    /// search handlers read facets/histograms/aggregates/pointers through
    /// the [`crate::index::analytics::LogAnalytics`] half of this trait.
    pub line_index: Arc<dyn LineIndex>,
    /// Manifest repository, for index coverage figures.
    pub manifests: Arc<ManifestRepo>,
    /// Chunk writer, for the collection status the capabilities endpoint
    /// reports (WAL recovery state and deferred generations).
    pub chunk_writer: Arc<ChunkWriterService>,
}

#[allow(clippy::too_many_arguments)]
pub async fn create_log_aggregator_app_state(
    search_service: Arc<LogSearchService>,
    metadata_service: Arc<LogMetadataService>,
    tail_service: Arc<TailService>,
    retention_service: Arc<RetentionService>,
    audit_service: Arc<dyn AuditLogger>,
    store: Arc<dyn LogLineStore>,
    db: Arc<DatabaseConnection>,
    line_index: Arc<dyn LineIndex>,
    chunk_writer: Arc<ChunkWriterService>,
) -> Arc<LogAggregatorAppState> {
    let manifests = Arc::new(ManifestRepo::new(db.clone()));
    Arc::new(LogAggregatorAppState {
        search_service,
        metadata_service,
        tail_service,
        retention_service,
        audit_service,
        store,
        db,
        project_access_checker: None,
        line_index,
        manifests,
        chunk_writer,
    })
}
