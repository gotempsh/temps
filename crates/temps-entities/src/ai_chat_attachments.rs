// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A file the user attached to a Temps assistant chat.
//!
//! Assistant chats have no workspace to keep files in, so their attachments
//! live here. Each row is bounded by the service (size per file, files and
//! bytes per conversation) and goes away with its conversation. Rows that were
//! uploaded but never sent ([`Model::sent_at`] is `None`) are swept after a
//! day, so abandoned uploads cannot fill the conversation's quota.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "ai_chat_attachments")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Opaque id handed to the browser (`att_…`).
    pub public_id: String,
    pub conversation_id: i64,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: i64,
    #[serde(skip)]
    pub content: Vec<u8>,
    /// For a PDF: the text layer read at upload (empty for a scan). `None` for
    /// other kinds, and for a PDF that could not be read.
    #[serde(skip)]
    pub extracted_text: Option<String>,
    /// Set when a message that references the file is accepted.
    pub sent_at: Option<DBDateTime>,
    pub created_at: DBDateTime,
}

// The payload can be large and private; never print it.
impl std::fmt::Debug for Model {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Model")
            .field("id", &self.id)
            .field("public_id", &self.public_id)
            .field("conversation_id", &self.conversation_id)
            .field("name", &self.name)
            .field("mime_type", &self.mime_type)
            .field("size_bytes", &self.size_bytes)
            .finish_non_exhaustive()
    }
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
