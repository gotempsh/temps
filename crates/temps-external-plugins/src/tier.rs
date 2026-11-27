// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The tier a plugin belongs to, which bounds what it may ever ask for.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Who stands behind a plugin, and so how much it may ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PluginTier {
    /// A plugin anyone published. It runs as a Temps service in its own
    /// container, is never given the control-plane database or the instance
    /// data directory, and keeps its data in a database or volume of its
    /// own. It reaches Temps only with the permissions an administrator
    /// granted.
    #[default]
    Community,
    /// A plugin from the signed registry that was reviewed. Reserved: the
    /// registry does not yet name anything Temps can deploy as a service, so
    /// no plugin can be given this tier.
    Verified,
}

impl PluginTier {
    /// The value stored on the plugin's row.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Community => "community",
            Self::Verified => "verified",
        }
    }

    /// The tier a stored value names. A value this build does not know is
    /// read as the tier with the least access.
    pub fn from_stored(value: &str) -> Self {
        match value {
            "verified" => Self::Verified,
            _ => Self::Community,
        }
    }

    /// The tier of a plugin added from a service an administrator chose.
    /// Only a reviewed registry entry could make a plugin verified, and no
    /// such entry can be deployed yet.
    pub fn of_added_service() -> Self {
        Self::Community
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tier_round_trips_through_its_stored_value() {
        for tier in [PluginTier::Community, PluginTier::Verified] {
            assert_eq!(PluginTier::from_stored(tier.as_str()), tier);
        }
        assert_eq!(
            serde_json::to_value(PluginTier::Community).unwrap(),
            serde_json::json!("community")
        );
    }

    #[test]
    fn an_unknown_or_missing_tier_is_the_one_with_the_least_access() {
        for stored in ["", "trusted", "VERIFIED", "official"] {
            assert_eq!(PluginTier::from_stored(stored), PluginTier::Community);
        }
        assert_eq!(PluginTier::default(), PluginTier::Community);
    }

    #[test]
    fn a_service_an_administrator_adds_is_never_verified() {
        assert_eq!(PluginTier::of_added_service(), PluginTier::Community);
    }
}
