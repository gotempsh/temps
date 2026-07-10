use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "saml_providers")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub template: String,
    pub sp_entity_id: String,
    pub idp_entity_id: String,
    pub idp_sso_url: String,
    /// X.509 PEM. NOT a secret -- this is the IdP's public signing
    /// certificate, returned unmasked in admin API responses so admins
    /// can verify which cert is configured. Contrast with
    /// `oidc_providers::Model::client_secret_encrypted`.
    pub idp_x509_cert: String,
    /// URL the metadata was fetched from, if any. NULL when the admin
    /// pasted the IdP fields manually instead of importing metadata XML.
    pub idp_metadata_url: Option<String>,
    pub group_attribute: String,
    pub role_attribute: String,
    pub default_role: String,
    /// SAML attribute carrying the user's email. When NULL, the
    /// resolver falls back to the NameID value if its Format is
    /// `emailAddress`.
    pub email_attribute: Option<String>,
    pub jit_provisioning: bool,
    pub enabled: bool,
    /// Defaults `true` (unlike OIDC's `trust_idp_email`, which defaults
    /// `false`). SAML has no `email_verified` equivalent -- the signed
    /// assertion itself is the trust anchor, and SAML IdPs are always
    /// admin-controlled enterprise systems, not self-service consumer
    /// IdPs. See ADR 0013 §3 for the full rationale and
    /// `temps_auth::saml_service::resolve_user` for the security
    /// consequence this flag gates.
    pub trust_idp_email: bool,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::saml_login_states::Entity")]
    SamlLoginStates,
    #[sea_orm(has_many = "super::users::Entity")]
    Users,
    #[sea_orm(has_many = "super::saml_role_mappings::Entity")]
    SamlRoleMappings,
}

impl Related<super::saml_login_states::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::SamlLoginStates.def()
    }
}

impl Related<super::users::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Users.def()
    }
}

impl Related<super::saml_role_mappings::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::SamlRoleMappings.def()
    }
}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        let now = chrono::Utc::now();
        if insert && self.created_at.is_not_set() {
            self.created_at = Set(now);
        }
        self.updated_at = Set(now);
        Ok(self)
    }
}
