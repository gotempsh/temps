//! Entity for the `cli_login_sessions` table — the OAuth-2.0-style device
//! authorization flow used by the Temps CLI. See the migration
//! `m20260511_000001_create_cli_login_sessions.rs` for the flow overview.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "cli_login_sessions")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// Opaque high-entropy token the CLI polls with. Never shown to a human.
    #[serde(skip_serializing)]
    pub device_code: String,
    /// Short human-readable code displayed in the CLI and entered in the
    /// browser approval page. Format: `XXXX-XXXX` from an unambiguous alphabet.
    pub user_code: String,
    /// `pending` | `approved` | `denied` | `expired`.
    pub status: String,
    /// Populated only after `approved`.
    pub user_id: Option<i32>,
    /// API key minted on approval (FK).
    pub api_key_id: Option<i32>,
    /// Plaintext API key, returned to the CLI exactly once via the poll
    /// endpoint and then cleared. Stored briefly so the unauthenticated poll
    /// endpoint can deliver it without ever holding a user session.
    #[serde(skip_serializing)]
    pub api_key_plaintext: Option<String>,
    /// Hostname or other identifier supplied by the CLI; shown to the user
    /// during approval so they can verify what device they are authorizing.
    pub client_name: Option<String>,
    /// IP the device-start request came from. Surfaced in the approval UI.
    pub requested_ip: Option<String>,
    pub expires_at: DBDateTime,
    pub last_polled_at: Option<DBDateTime>,
    pub approved_at: Option<DBDateTime>,
    pub denied_at: Option<DBDateTime>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "crate::users::Entity",
        from = "Column::UserId",
        to = "crate::users::Column::Id"
    )]
    User,
    #[sea_orm(
        belongs_to = "crate::api_keys::Entity",
        from = "Column::ApiKeyId",
        to = "crate::api_keys::Column::Id"
    )]
    ApiKey,
}

impl Related<crate::users::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::User.def()
    }
}

impl Related<crate::api_keys::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ApiKey.def()
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
