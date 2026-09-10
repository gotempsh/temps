// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::services::service::SessionReplayService;
use std::sync::Arc;
use temps_analytics::ingest_keys::{AnalyticsIngestKeyService, AnalyticsIngestRateLimiter};
use temps_routes::CachedPeerTable;

#[derive(Clone)]
pub struct AppState {
    pub session_replay_service: Arc<SessionReplayService>,
    pub audit_service: Arc<dyn temps_core::AuditLogger>,
    pub route_table: Arc<CachedPeerTable>,
    pub telemetry: std::sync::Arc<dyn temps_core::telemetry::TelemetryReporter>,
    /// Optional checker for team-based project access (human sessions only).
    pub project_access_checker: Option<Arc<dyn temps_core::ProjectAccessChecker>>,
    /// ADR-040: resolves an `X-Temps-Analytics-Key` / `?temps_key=` credential
    /// to a project scope for apps Temps does not deploy, where the route table
    /// has no entry to match `Host` against.
    pub ingest_key_service: Arc<AnalyticsIngestKeyService>,
    /// Per-key sliding-window limiter for the keyed ingest path.
    pub ingest_rate_limiter: Arc<AnalyticsIngestRateLimiter>,
}
