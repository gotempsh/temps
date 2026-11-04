// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Node enrollment token entity (ADR-020 WS-1.1).
//!
//! Short-lived, single-use (or bounded-use), optionally node-scoped tokens that
//! authorize a worker to register. Only the SHA-256 hash is stored.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "node_enrollment_tokens")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// SHA-256 hex of the plaintext token (never store plaintext).
    #[serde(skip_serializing)]
    pub token_hash: String,
    /// Maximum number of registrations this token may authorize.
    pub max_uses: i32,
    /// How many registrations it has authorized so far.
    pub used_count: i32,
    pub expires_at: DBDateTime,
    /// Optional pin: token only valid to register this node name.
    pub bound_node_name: Option<String>,
    /// Optional pin: scheduling labels the joining node must carry.
    pub bound_labels: Option<serde_json::Value>,
    /// Admin user who minted it.
    pub created_by_user_id: Option<i32>,
    pub revoked_at: Option<DBDateTime>,
    /// SHA-256 fingerprint of the cluster CA at mint time (for out-of-band CP
    /// verification by the joining node — ADR-020 WS-2.2).
    pub ca_fingerprint: Option<String>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializing_a_token_leaves_out_its_hash() {
        let now = chrono::Utc::now();
        let token = Model {
            id: 1,
            token_hash: "stored-token-hash".to_string(),
            max_uses: 1,
            used_count: 0,
            expires_at: now,
            bound_node_name: Some("worker-1".to_string()),
            bound_labels: None,
            created_by_user_id: None,
            revoked_at: None,
            ca_fingerprint: None,
            created_at: now,
            updated_at: now,
        };

        let json = serde_json::to_string(&token).expect("serializes");

        assert!(!json.contains("stored-token-hash"), "{json}");
        assert!(json.contains("worker-1"), "{json}");
    }
}
