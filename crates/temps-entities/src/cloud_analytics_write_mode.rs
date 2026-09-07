// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where a project's non-span analytics signals are *written* (ADR-043 §1).
//!
//! The second write-mode switch, orthogonal to — but gated on the same
//! prerequisites as — [`super::cloud_telemetry_write_mode`]. Where that module
//! controls span writes, this one controls the group of signals that share a
//! single Cloud switch: `otel_metrics`, `service_metrics`, `analytics_events`,
//! `analytics_sessions`, and `proxy_logs`.
//!
//! Stored on the project row (`projects.cloud_analytics_write_mode`), not read
//! from an environment variable, so an operator can flip one project at a time
//! at runtime through the API/UI and every change is audit-logged.
//!
//! # Why the default matters more than the ergonomics
//!
//! [`CloudAnalyticsWriteMode::Local`] is today's behaviour, byte for byte:
//! analytics events, metrics and proxy logs are written to the local store and
//! optionally mirrored. Every existing project, every newly created project,
//! and every project whose mode cannot be resolved is `Local`. The opposite
//! failure — a project silently becoming Cloud-primary because a lookup errored
//! — would mean telemetry stored nowhere on this instance, which is
//! unrecoverable once the collection window has passed.
//!
//! # PII note
//!
//! This switch covers signals with a much larger PII surface than spans
//! (`analytics_events` carries URLs, visitor IDs, referrers). The
//! consent copy shown when setting this to `cloud` must enumerate the exact
//! fields that leave the instance, as stated in ADR-043 §1. The switch being
//! `local` by default is the correct safety posture.

use sea_orm::{DeriveActiveEnum, EnumIter};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Per-project destination for analytics and metrics writes (ADR-043 §1).
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    ToSchema,
    DeriveActiveEnum,
    EnumIter,
    Default,
)]
#[sea_orm(rs_type = "String", db_type = "Text")]
#[serde(rename_all = "snake_case")]
pub enum CloudAnalyticsWriteMode {
    /// The default, and exactly what every instance did before ADR-043:
    /// analytics events, metrics and proxy logs are written to local storage
    /// first, then offered to the optional Cloud mirror. Ordering, retry
    /// behaviour and consent scope are all unchanged.
    #[default]
    #[sea_orm(string_value = "local")]
    Local,

    /// Opt-in. Analytics events, metrics and proxy logs are enqueued to this
    /// instance's durable telemetry outbox and shipped to Temps Cloud by a
    /// background worker. **No local write happens for these signals.**
    ///
    /// Only reachable when the project is at
    /// [`super::cloud_telemetry_fidelity::CloudTelemetryFidelity::Queryable`],
    /// the instance is linked, and the Cloud telemetry feature switch is on —
    /// see ADR-043 §1. Enforced by the service layer gate and the database
    /// `CHECK` constraint added in
    /// `m20260903_000003_add_cloud_analytics_write_mode`.
    #[sea_orm(string_value = "cloud")]
    Cloud,
}

impl CloudAnalyticsWriteMode {
    /// Whether this project's analytics bypass local storage entirely.
    pub fn is_cloud_primary(&self) -> bool {
        matches!(self, CloudAnalyticsWriteMode::Cloud)
    }

    /// Whether this project still needs a local analytics store for *new*
    /// writes.
    ///
    /// Deliberately the inverse of [`Self::is_cloud_primary`] rather than a
    /// separate flag: two independent booleans describing one fact drift.
    pub fn requires_local_analytics_store(&self) -> bool {
        !self.is_cloud_primary()
    }
}

impl std::fmt::Display for CloudAnalyticsWriteMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CloudAnalyticsWriteMode::Local => write!(f, "local"),
            CloudAnalyticsWriteMode::Cloud => write!(f, "cloud"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_local() {
        // The entire safety model rests on this. If it ever flips, an upgrade
        // would silently stop storing analytics for every existing project.
        assert_eq!(
            CloudAnalyticsWriteMode::default(),
            CloudAnalyticsWriteMode::Local
        );
        assert!(!CloudAnalyticsWriteMode::default().is_cloud_primary());
        assert!(CloudAnalyticsWriteMode::default().requires_local_analytics_store());
    }

    #[test]
    fn only_the_opt_in_mode_bypasses_local_storage() {
        assert!(!CloudAnalyticsWriteMode::Local.is_cloud_primary());
        assert!(CloudAnalyticsWriteMode::Cloud.is_cloud_primary());
        assert!(!CloudAnalyticsWriteMode::Cloud.requires_local_analytics_store());
    }

    #[test]
    fn display_matches_the_stored_column_values() {
        // `Display` ends up in audit entries and error text; it must not drift
        // from the `string_value`s persisted in Postgres.
        assert_eq!(CloudAnalyticsWriteMode::Local.to_string(), "local");
        assert_eq!(CloudAnalyticsWriteMode::Cloud.to_string(), "cloud");
    }

    #[test]
    fn serde_uses_the_same_snake_case_names_as_the_column() {
        assert_eq!(
            serde_json::to_string(&CloudAnalyticsWriteMode::Local).expect("must serialize"),
            r#""local""#
        );
        assert_eq!(
            serde_json::from_str::<CloudAnalyticsWriteMode>(r#""cloud""#)
                .expect("must deserialize"),
            CloudAnalyticsWriteMode::Cloud
        );
    }
}
