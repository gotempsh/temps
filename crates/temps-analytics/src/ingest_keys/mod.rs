// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Analytics ingest keys (ADR-040).
//!
//! A project-scoped, optionally environment-scoped, **non-secret** credential
//! that lets an application Temps does not deploy send analytics, performance
//! and session-replay data. Without it, all five public ingest endpoints
//! resolve their scope solely from the request `Host` against the proxy route
//! table — which has no entry for an app hosted elsewhere, so every event is
//! rejected.
//!
//! This module owns the credential itself: minting, listing, updating,
//! rotating, revoking, and the cached hot-path resolution the ingest handlers
//! consume. It deliberately contains no ingest-side wiring; the handlers in
//! `temps-analytics-events`, `temps-analytics-performance` and
//! `temps-analytics-session-replay` call [`AnalyticsIngestKeyService::resolve`]
//! and [`AnalyticsIngestRateLimiter::check`] themselves.

pub mod audit;
pub mod handlers;
pub mod rate_limiter;
pub mod request;
pub mod service;
pub mod types;

#[cfg(test)]
pub(crate) mod test_fixtures;

pub use audit::{
    AnalyticsIngestKeyCreatedAudit, AnalyticsIngestKeyRevokedAudit, AnalyticsIngestKeyRotatedAudit,
    AnalyticsIngestKeyUpdatedAudit,
};
pub use handlers::{
    configure_ingest_key_routes, AnalyticsIngestKeyApiDoc, AnalyticsIngestKeysAppState,
};
pub use rate_limiter::{
    AnalyticsIngestRateLimiter, DEFAULT_RATE_LIMIT_PER_MINUTE, UNRESOLVED_KEY_RATE_LIMIT_PER_MINUTE,
};
pub use request::{
    extract_analytics_key, ingest_rate_limited_problem, invalid_ingest_key_problem,
    is_origin_allowed, origin_not_allowed_problem, resolve_client_identity,
    resolve_keyed_ingest_scope, ANALYTICS_INGEST_KEY_HEADER, ANALYTICS_INGEST_KEY_QUERY_PARAM,
};
pub use service::AnalyticsIngestKeyService;
pub use types::{
    AnalyticsIngestKey, AnalyticsIngestKeyError, CreateAnalyticsIngestKeyRequest,
    ResolvedIngestScope, UpdateAnalyticsIngestKeyRequest, ANALYTICS_INGEST_KEY_PREFIX,
};
