// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// Record that a client-identified batch of rrweb events has already been
/// ingested for a session, so a resent batch can be discarded instead of
/// appended a second time.
///
/// `batch_id` comes from the browser and is only unique within a session --
/// the unique constraint is on `(session_id, batch_id)`.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "session_replay_ingest_batches")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub session_id: i32,
    pub batch_id: String,
    pub event_count: i32,
    pub received_at: DateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::session_replay_sessions::Entity",
        from = "Column::SessionId",
        to = "super::session_replay_sessions::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    SessionReplaySessions,
}

impl Related<super::session_replay_sessions::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::SessionReplaySessions.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
