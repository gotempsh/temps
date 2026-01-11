use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "webhook_deliveries")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub webhook_id: i32,
    pub event_type: String,
    pub event_id: String,
    /// JSON payload that was sent
    #[sea_orm(column_type = "Text")]
    pub payload: String,
    pub success: bool,
    pub status_code: Option<i32>,
    // SECURITY: response_body field deprecated and should be removed in future migration
    // Kept for backward compatibility with existing databases but should NEVER be populated
    #[deprecated(note = "SECURITY: Do not use - removed to prevent SSRF data exfiltration")]
    #[sea_orm(column_type = "Text", nullable)]
    pub response_body: Option<String>,
    pub error_message: Option<String>,
    pub attempt_number: i32,
    pub created_at: DBDateTime,
    pub delivered_at: Option<DBDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::webhooks::Entity",
        from = "Column::WebhookId",
        to = "super::webhooks::Column::Id"
    )]
    Webhook,
}

impl Related<super::webhooks::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Webhook.def()
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
            if self.success.is_not_set() {
                self.success = Set(false);
            }
            if self.attempt_number.is_not_set() {
                self.attempt_number = Set(1);
            }
        }

        Ok(self)
    }
}
