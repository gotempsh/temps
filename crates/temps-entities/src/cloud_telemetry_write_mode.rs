// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where a project's spans are *written* (ADR-041 §0/§1).
//!
//! Orthogonal to — but gated on — [`super::cloud_telemetry_fidelity`], which
//! decides how much of a span may leave the instance. This decides whether the
//! span is stored on the instance at all.
//!
//! Stored on the project row (`projects.cloud_telemetry_write_mode`), not read
//! from an environment variable, so an operator can flip one project at a time
//! at runtime through the API/UI and every change is audit-logged.
//!
//! # Why the default matters more than the ergonomics
//!
//! [`CloudTelemetryWriteMode::Local`] is today's behaviour, byte for byte:
//! spans are written to the local span store and optionally mirrored. Every
//! existing project, every newly created project, and every project whose mode
//! cannot be resolved is `Local`. The opposite failure — a project silently
//! becoming Cloud-primary because a lookup errored — would mean spans stored
//! nowhere on this instance, which is unrecoverable once the window has passed.
//!
//! # Why this is not called "disable ClickHouse"
//!
//! ClickHouse is one of two possible local span backends and is already
//! optional — the default install has none and stores spans in the TimescaleDB
//! `otel_spans` hypertable. The property this enum introduces is "this
//! project's spans are not stored on this instance at all", whichever backend
//! that store happens to be.

use sea_orm::{DeriveActiveEnum, EnumIter};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Per-project destination for span writes.
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
pub enum CloudTelemetryWriteMode {
    /// The default, and exactly what every instance did before ADR-041: the
    /// span is written to local storage first, then offered to the optional
    /// Cloud mirror. Ordering, retry behaviour and the `Metered` projection are
    /// all unchanged.
    #[default]
    #[sea_orm(string_value = "local")]
    Local,

    /// Opt-in. The span is enqueued to this instance's durable telemetry outbox
    /// and shipped to Temps Cloud by a background worker. **No local span write
    /// happens.**
    ///
    /// Only reachable when the project is at
    /// [`super::cloud_telemetry_fidelity::CloudTelemetryFidelity::Queryable`],
    /// the instance is linked, and the Cloud telemetry feature switch is on —
    /// see ADR-041 §1. A Cloud-primary project at `metered` fidelity would
    /// store nothing readable anywhere, so that combination is structurally
    /// unreachable rather than merely discouraged.
    #[sea_orm(string_value = "cloud")]
    Cloud,
}

impl CloudTelemetryWriteMode {
    /// Whether this project's spans bypass local storage entirely.
    pub fn is_cloud_primary(&self) -> bool {
        matches!(self, CloudTelemetryWriteMode::Cloud)
    }

    /// Whether this project still needs a local span store for *new* writes.
    ///
    /// Deliberately the inverse of [`Self::is_cloud_primary`] rather than a
    /// separate flag: two independent booleans describing one fact drift.
    pub fn requires_local_span_store(&self) -> bool {
        !self.is_cloud_primary()
    }
}

impl std::fmt::Display for CloudTelemetryWriteMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CloudTelemetryWriteMode::Local => write!(f, "local"),
            CloudTelemetryWriteMode::Cloud => write!(f, "cloud"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_local() {
        // The entire safety model rests on this. If it ever flips, an upgrade
        // would silently stop storing spans for every existing project.
        assert_eq!(
            CloudTelemetryWriteMode::default(),
            CloudTelemetryWriteMode::Local
        );
        assert!(!CloudTelemetryWriteMode::default().is_cloud_primary());
        assert!(CloudTelemetryWriteMode::default().requires_local_span_store());
    }

    #[test]
    fn only_the_opt_in_mode_bypasses_local_storage() {
        assert!(!CloudTelemetryWriteMode::Local.is_cloud_primary());
        assert!(CloudTelemetryWriteMode::Cloud.is_cloud_primary());
        assert!(!CloudTelemetryWriteMode::Cloud.requires_local_span_store());
    }

    #[test]
    fn display_matches_the_stored_column_values() {
        // `Display` ends up in audit entries and error text; it must not drift
        // from the `string_value`s persisted in Postgres.
        assert_eq!(CloudTelemetryWriteMode::Local.to_string(), "local");
        assert_eq!(CloudTelemetryWriteMode::Cloud.to_string(), "cloud");
    }

    #[test]
    fn serde_uses_the_same_snake_case_names_as_the_column() {
        assert_eq!(
            serde_json::to_string(&CloudTelemetryWriteMode::Local).expect("must serialize"),
            r#""local""#
        );
        assert_eq!(
            serde_json::from_str::<CloudTelemetryWriteMode>(r#""cloud""#)
                .expect("must deserialize"),
            CloudTelemetryWriteMode::Cloud
        );
    }
}
