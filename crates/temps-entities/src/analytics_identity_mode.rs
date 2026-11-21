// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! How a project tells its visitors apart (ADR 109 D3).
//!
//! Stored on the project row (`projects.analytics_identity_mode`) so the
//! proxy and the ingest path read it from the route table they already hold,
//! with no lookup per request, and so an operator switches one project at a
//! time at runtime with an audit entry.

use sea_orm::{DeriveActiveEnum, EnumIter};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Per-project visitor identity mode.
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
pub enum AnalyticsIdentityMode {
    /// The default, and what every project did before ADR 109: the proxy
    /// sets `_temps_visitor_id` (365 days) and `_temps_sid` (30 minutes), and
    /// the SDK keeps its ids in `localStorage`. Visitors are recognised across
    /// days, which visitor profiles, retention and session replay rely on.
    #[default]
    #[sea_orm(string_value = "persistent")]
    Persistent,

    /// Nothing is stored on the visitor's device. The visitor id is a hash of
    /// a salt that rotates every day, the project, the client address and the
    /// user agent, so a visitor is a new visitor the next day and cannot be
    /// followed across days or recovered once the salt is deleted.
    #[sea_orm(string_value = "cookieless")]
    Cookieless,
}

impl AnalyticsIdentityMode {
    pub fn is_cookieless(self) -> bool {
        matches!(self, AnalyticsIdentityMode::Cookieless)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AnalyticsIdentityMode::Persistent => "persistent",
            AnalyticsIdentityMode::Cookieless => "cookieless",
        }
    }
}

impl std::fmt::Display for AnalyticsIdentityMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_persistent() {
        // Existing projects keep their cookies, profiles and replays on upgrade.
        assert_eq!(
            AnalyticsIdentityMode::default(),
            AnalyticsIdentityMode::Persistent
        );
        assert!(!AnalyticsIdentityMode::default().is_cookieless());
    }

    #[test]
    fn display_and_serde_match_the_stored_values() {
        for (mode, stored) in [
            (AnalyticsIdentityMode::Persistent, "persistent"),
            (AnalyticsIdentityMode::Cookieless, "cookieless"),
        ] {
            assert_eq!(mode.to_string(), stored);
            assert_eq!(
                serde_json::to_string(&mode).expect("serialize"),
                format!("\"{stored}\"")
            );
        }
    }
}
