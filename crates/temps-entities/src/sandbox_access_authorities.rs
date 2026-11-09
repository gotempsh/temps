// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Internal encrypted host authority, never a response or audit payload.
use sea_orm::entity::prelude::*;

#[derive(Clone, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "sandbox_access_authorities")]
pub struct Model {
    /// Authenticated worker boot UUID; immutable for this registration.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    /// None is the local control-plane worker.
    pub node_id: Option<i32>,
    pub sdk_node_id: Uuid,
    pub seal_public_key: String,
    pub snapshot_key_encrypted: String,
    pub session_key_encrypted: String,
    pub ca_certificate: String,
    pub ca_key_encrypted: String,
    /// Monotonic across control-plane restarts and concurrent dispatchers.
    pub snapshot_version: i64,
    pub closed_at: Option<temps_core::DBDateTime>,
    pub created_at: temps_core::DBDateTime,
    pub updated_at: temps_core::DBDateTime,
}
impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SandboxAccessAuthority")
            .field("id", &self.id)
            .field("node_id", &self.node_id)
            .field("snapshot_version", &self.snapshot_version)
            .field("closed_at", &self.closed_at)
            .finish_non_exhaustive()
    }
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::nodes::Entity",
        from = "Column::NodeId",
        to = "super::nodes::Column::Id",
        on_delete = "Restrict"
    )]
    Node,
}
impl Related<super::nodes::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Node.def()
    }
}
impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn debug_hides_every_encrypted_key() {
        let now = chrono::Utc::now();
        let row = Model {
            id: Uuid::new_v4(),
            node_id: None,
            sdk_node_id: Uuid::new_v4(),
            seal_public_key: "public".into(),
            snapshot_key_encrypted: "snapshot-sentinel".into(),
            session_key_encrypted: "session-sentinel".into(),
            ca_certificate: "public-ca".into(),
            ca_key_encrypted: "ca-sentinel".into(),
            snapshot_version: 0,
            closed_at: None,
            created_at: now,
            updated_at: now,
        };
        let debug = format!("{row:?}");
        for secret in [
            &row.snapshot_key_encrypted,
            &row.session_key_encrypted,
            &row.ca_key_encrypted,
        ] {
            assert!(!debug.contains(secret));
        }
        assert!(debug.contains(&row.id.to_string()));
    }
}
