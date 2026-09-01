// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use utoipa::ToSchema;

/// Role a user holds within a team, or that a team holds on a project.
///
/// Named `TeamRole` rather than `Role` to keep it distinct from
/// `temps_auth::permissions::Role`, which is the instance-wide role
/// (Admin/User/…) attached to a session. The two are orthogonal: the
/// instance-wide role decides whether you may touch a resource *kind* at
/// all, `TeamRole` decides what you may do *within a project* you have
/// team access to. See `temps_teams::fixed_role_permissions` for the
/// project-scoped permission set each variant maps to.
///
/// Stored as a `varchar(32)` rather than a Postgres enum so the role set
/// can evolve in pure migration code without a schema-level enum
/// alteration blocking a downgrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum TeamRole {
    Owner,
    Admin,
    Deployer,
    Viewer,
}

impl TeamRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            TeamRole::Owner => "owner",
            TeamRole::Admin => "admin",
            TeamRole::Deployer => "deployer",
            TeamRole::Viewer => "viewer",
        }
    }

    pub const ALL: &'static [TeamRole] = &[
        TeamRole::Owner,
        TeamRole::Admin,
        TeamRole::Deployer,
        TeamRole::Viewer,
    ];
}

impl fmt::Display for TeamRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Unknown team role '{0}' (expected owner|admin|deployer|viewer)")]
pub struct TeamRoleParseError(pub String);

impl FromStr for TeamRole {
    type Err = TeamRoleParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "owner" => Ok(TeamRole::Owner),
            "admin" => Ok(TeamRole::Admin),
            "deployer" => Ok(TeamRole::Deployer),
            "viewer" => Ok(TeamRole::Viewer),
            other => Err(TeamRoleParseError(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_roundtrips_through_string() {
        for role in TeamRole::ALL {
            let parsed: TeamRole = role.as_str().parse().unwrap();
            assert_eq!(parsed, *role);
        }
    }

    #[test]
    fn unknown_role_returns_error_with_input() {
        let err = "superuser".parse::<TeamRole>().unwrap_err();
        assert!(err.to_string().contains("superuser"));
    }

    #[test]
    fn role_serializes_as_lowercase_string() {
        let json = serde_json::to_string(&TeamRole::Deployer).unwrap();
        assert_eq!(json, "\"deployer\"");
    }
}
