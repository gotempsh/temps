// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "oidc_providers")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub issuer_url: String,
    pub client_id: String,
    #[serde(skip_serializing)]
    pub client_secret_encrypted: String,
    pub scopes: String,
    pub jit_provisioning: bool,
    pub enabled: bool,
    pub template: String,
    pub group_claim: String,
    pub role_claim: String,
    pub default_role: String,
    /// When true, `resolve_user` skips the `email_verified` claim gate
    /// before linking or JIT-provisioning users. Only safe for IdPs
    /// where an administrator controls user provisioning (corporate
    /// Okta, Azure AD). Defaults false; admins opt in per provider via
    /// the OIDC provider edit form. See
    /// `temps_auth::oidc_service::resolve_user` for the security
    /// rationale this flag bypasses.
    pub trust_idp_email: bool,
    /// ADR-045 §4: set only on the console-access provider Temps Cloud
    /// provisions. Governs two things: (1) `resolve_user`'s
    /// `admin_only_role_required` gate is meaningless on a provider an
    /// operator does not control end-to-end, so it is paired with this flag
    /// rather than exposed generally; (2) the `PUT`/`DELETE
    /// /admin/oidc/providers/{id}` handlers refuse to edit or delete this
    /// row manually — its credentials are rotated by Cloud's own
    /// provisioning path, mirroring `s3_sources.managed_by_cloud`.
    pub managed_by_cloud: bool,
    /// ADR-045 §4 role gate, layer 2: when true, `resolve_user` hard-rejects
    /// with `OidcError::InsufficientRole` unless the role resolved from
    /// claims (via `role_claim`/`oidc_role_mappings`, same mechanism every
    /// other provider uses) is `RoleType::Admin` — never falling through to
    /// `default_role` the way an ungated provider would. Set only alongside
    /// `managed_by_cloud`; a belt-and-suspenders check independent of
    /// whatever Cloud's own account-linking screen enforces.
    pub admin_only_role_required: bool,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::oidc_login_states::Entity")]
    OidcLoginStates,
    #[sea_orm(has_many = "super::users::Entity")]
    Users,
    #[sea_orm(has_many = "super::oidc_role_mappings::Entity")]
    OidcRoleMappings,
}

impl Related<super::oidc_login_states::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::OidcLoginStates.def()
    }
}

impl Related<super::users::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Users.def()
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
