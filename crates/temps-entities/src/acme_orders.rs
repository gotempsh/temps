use sea_orm::entity::prelude::*;
use async_trait::async_trait;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "acme_orders")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub order_url: String,
    pub domain_id: i32,
    pub email: String,
    pub status: String,
    pub identifiers: Json,
    pub authorizations: Option<Json>,
    pub finalize_url: Option<String>,
    pub certificate_url: Option<String>,
    pub error: Option<String>,
    pub error_type: Option<String>,
    pub token: Option<String>,  // For fast HTTP-01 challenge lookups (indexed)
    pub key_authorization: Option<String>,  // For fast HTTP-01 challenge lookups
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    pub expires_at: Option<DBDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::domains::Entity",
        from = "Column::DomainId",
        to = "super::domains::Column::Id"
    )]
    Domain,
}

impl Related<super::domains::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Domain.def()
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
