// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! How much of a span may leave this instance for Temps Cloud (ADR-040 §1).
//!
//! Stored on the project row (`projects.cloud_telemetry_fidelity`) rather than
//! read from an environment variable, so an operator can change it per project
//! at runtime through the API/UI and every change is audit-logged.
//!
//! This is the consent gate for a data-egress decision, so its default matters
//! more than its ergonomics: every existing and every newly created project is
//! [`CloudTelemetryFidelity::Metered`], and raising it is always a deliberate,
//! per-project act.

use sea_orm::{DeriveActiveEnum, EnumIter};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Per-project fidelity tier for the optional Temps Cloud telemetry mirror.
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
pub enum CloudTelemetryFidelity {
    /// The default, and exactly what every instance mirrored before ADR-040:
    /// HMAC-pseudonymised trace and span identifiers, the constant span name
    /// `"span"`, and no attributes at all.
    ///
    /// Answers "is my instance alive, and how much am I being billed for" and
    /// nothing else. Deliberately **not** readable back: a console page built
    /// on these rows would show identical rows named `span` carrying 64-hex
    /// identifiers that match nothing the user owns, which reads as data loss.
    #[default]
    #[sea_orm(string_value = "metered")]
    Metered,

    /// Opt-in. Mirrors the real span name, service name, span kind, status,
    /// parent span id and environment, plus real (not pseudonymised) trace and
    /// span identifiers, so the data can be read back and correlated with the
    /// user's own logs.
    ///
    /// Attributes remain **default-deny**: only keys listed in
    /// `projects.cloud_telemetry_attribute_allowlist` are shipped, and an empty
    /// allowlist — the default even here — ships none.
    ///
    /// Raising this is effectively one-way for data already sent: lowering it
    /// back to [`CloudTelemetryFidelity::Metered`] stops future egress but does
    /// not retract what already left.
    #[sea_orm(string_value = "queryable")]
    Queryable,
}

impl CloudTelemetryFidelity {
    /// Whether spans at this tier can be read back out of Cloud.
    ///
    /// A `Metered` project is not "broken" and not "empty" — it is
    /// unconfigured, and the read path must say so with a setup path rather
    /// than returning an empty success.
    pub fn is_queryable(&self) -> bool {
        matches!(self, CloudTelemetryFidelity::Queryable)
    }
}

impl std::fmt::Display for CloudTelemetryFidelity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CloudTelemetryFidelity::Metered => write!(f, "metered"),
            CloudTelemetryFidelity::Queryable => write!(f, "queryable"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_metered() {
        // The whole consent model rests on this. If it ever flips, an upgrade
        // would silently widen what leaves every existing instance.
        assert_eq!(
            CloudTelemetryFidelity::default(),
            CloudTelemetryFidelity::Metered
        );
        assert!(!CloudTelemetryFidelity::default().is_queryable());
    }

    #[test]
    fn is_queryable_only_for_the_opt_in_tier() {
        assert!(!CloudTelemetryFidelity::Metered.is_queryable());
        assert!(CloudTelemetryFidelity::Queryable.is_queryable());
    }

    #[test]
    fn display_matches_the_stored_column_values() {
        // `Display` is what ends up in audit entries and error text; it must
        // not drift from the `string_value`s persisted in Postgres.
        assert_eq!(CloudTelemetryFidelity::Metered.to_string(), "metered");
        assert_eq!(CloudTelemetryFidelity::Queryable.to_string(), "queryable");
    }

    #[test]
    fn serde_uses_the_same_snake_case_names_as_the_column() {
        assert_eq!(
            serde_json::to_string(&CloudTelemetryFidelity::Metered).expect("must serialize"),
            r#""metered""#
        );
        assert_eq!(
            serde_json::from_str::<CloudTelemetryFidelity>(r#""queryable""#)
                .expect("must deserialize"),
            CloudTelemetryFidelity::Queryable
        );
    }
}
