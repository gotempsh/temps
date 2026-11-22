// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Who identified a visitor (ADR 109 D4).

use sea_orm::{DeriveActiveEnum, EnumIter};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Where a visitor's `user_id`, or one of the traits `identify` wrote, came
/// from.
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
)]
#[sea_orm(rs_type = "String", db_type = "Text")]
#[serde(rename_all = "snake_case")]
pub enum VisitorIdentitySource {
    /// Sent by the page through the public ingest endpoint. A claim the
    /// browser makes about itself: anyone can send any user id this way.
    #[sea_orm(string_value = "browser")]
    Browser,

    /// Sent by the app's backend with a credential (the injected deployment
    /// token or an API key). The app vouches for it.
    #[sea_orm(string_value = "server")]
    Server,
}

impl VisitorIdentitySource {
    pub fn as_str(self) -> &'static str {
        match self {
            VisitorIdentitySource::Browser => "browser",
            VisitorIdentitySource::Server => "server",
        }
    }

    /// Parse a stored `trait_sources` value; anything else is unknown.
    pub fn from_stored(value: &str) -> Option<Self> {
        match value {
            "browser" => Some(VisitorIdentitySource::Browser),
            "server" => Some(VisitorIdentitySource::Server),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_values_round_trip() {
        for source in [
            VisitorIdentitySource::Browser,
            VisitorIdentitySource::Server,
        ] {
            assert_eq!(
                VisitorIdentitySource::from_stored(source.as_str()),
                Some(source)
            );
        }
        assert_eq!(VisitorIdentitySource::from_stored("admin"), None);
    }
}
