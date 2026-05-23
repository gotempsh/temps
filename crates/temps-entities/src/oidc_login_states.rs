use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "oidc_login_states")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    #[sea_orm(unique)]
    pub state: String,
    pub nonce: String,
    pub pkce_verifier: String,
    pub provider_id: i32,
    pub return_to: Option<String>,
    pub expires_at: DBDateTime,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::oidc_providers::Entity",
        from = "Column::ProviderId",
        to = "super::oidc_providers::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    OidcProvider,
}

impl Related<super::oidc_providers::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::OidcProvider.def()
    }
}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        if insert && self.created_at.is_not_set() {
            self.created_at = Set(chrono::Utc::now());
        }
        Ok(self)
    }
}
