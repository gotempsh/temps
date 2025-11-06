use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "challenge_sessions")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub environment_id: i32,
    /// Identifier used for challenge verification (IP address or JA4 fingerprint)
    pub identifier: String,
    /// Type of identifier: "ip" or "ja4"
    pub identifier_type: String,
    /// Optional user agent for additional tracking
    pub user_agent: Option<String>,
    /// When the challenge was completed
    pub completed_at: DBDateTime,
    /// When this challenge session expires
    pub expires_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "crate::environments::Entity",
        from = "Column::EnvironmentId",
        to = "crate::environments::Column::Id"
    )]
    Environment,
}

impl Related<crate::environments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Environment.def()
    }
}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(self, _db: &C, _insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        // No automatic timestamp updates needed - all fields are set explicitly
        Ok(self)
    }
}
