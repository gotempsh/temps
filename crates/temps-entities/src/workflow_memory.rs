// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

/// Per-workflow persistent memory.
///
/// Each row is a "fact" that the workflow has learned from past runs. Memory is
/// strictly scoped by `(project_id, agent_id)` — two workflows in the same project
/// don't share memory, and the same workflow slug in different projects doesn't
/// share memory either.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "workflow_memory")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Project this memory belongs to (security boundary)
    pub project_id: i32,
    /// Workflow (project_agents row) this memory belongs to
    pub agent_id: i32,
    /// The natural-language fact the AI wrote
    #[sea_orm(column_type = "Text")]
    pub fact: String,
    /// Tags for tag-based retrieval (e.g. ["error_group_id:42", "file:src/api.ts"])
    #[sea_orm(column_type = "JsonBinary")]
    pub tags: serde_json::Value,
    /// Confidence 0..1, increases as the fact is reinforced or used
    pub confidence: f32,
    /// How many times the AI has cited this fact in subsequent runs
    pub times_used: i32,
    /// Provenance — which agent_runs.id contributed to this fact
    #[sea_orm(column_type = "JsonBinary")]
    pub source_run_ids: serde_json::Value,
    /// If non-null, this fact was replaced by another fact (forms a history chain)
    pub superseded_by: Option<i64>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    pub last_used_at: Option<DBDateTime>,
    /// Raw little-endian f32 vector embedding of `fact`, if computed.
    /// `None` means the fact hasn't been embedded yet (fresh row, or the
    /// embedding provider was offline). Consumers should never try to
    /// interpret this as a pgvector — stock Postgres has no such type.
    /// Use `temps-embeddings` to read/write it.
    #[sea_orm(column_type = "VarBinary(StringLen::None)", nullable)]
    pub embedding: Option<Vec<u8>>,
    /// When set, the fact is eligible for compaction and may be removed
    /// by the periodic sweep. `None` means "keep indefinitely" (the
    /// default for facts written by the AI — expiry is only set
    /// explicitly when the caller knows the fact has a shelf life, e.g.
    /// "deploy failing because of ongoing incident X").
    pub expires_at: Option<DBDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id",
        on_delete = "Cascade"
    )]
    Project,
    #[sea_orm(
        belongs_to = "super::project_agents::Entity",
        from = "Column::AgentId",
        to = "super::project_agents::Column::Id",
        on_delete = "Cascade"
    )]
    Agent,
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

impl Related<super::project_agents::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Agent.def()
    }
}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        let now = chrono::Utc::now();
        if insert {
            if self.created_at.is_not_set() {
                self.created_at = Set(now);
            }
            if self.updated_at.is_not_set() {
                self.updated_at = Set(now);
            }
        } else {
            self.updated_at = Set(now);
        }
        Ok(self)
    }
}
