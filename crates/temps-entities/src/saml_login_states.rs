use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "saml_login_states")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    #[sea_orm(unique)]
    pub relay_state: String,
    /// The `ID=` attribute of the AuthnRequest we sent -- validated
    /// against the SAMLResponse's `InResponseTo` on the ACS path.
    pub authn_request_id: String,
    pub provider_id: i32,
    pub return_to: Option<String>,
    pub expires_at: DBDateTime,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::saml_providers::Entity",
        from = "Column::ProviderId",
        to = "super::saml_providers::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    SamlProvider,
}

impl Related<super::saml_providers::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::SamlProvider.def()
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
