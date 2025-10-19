use sea_orm::entity::prelude::*;
use async_trait::async_trait;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "users")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub email: String,
    // Password authentication fields
    pub password_hash: Option<String>,
    pub email_verified: bool,
    pub email_verification_token: Option<String>,
    pub email_verification_expires: Option<DBDateTime>,
    pub password_reset_token: Option<String>,
    pub password_reset_expires: Option<DBDateTime>,
    // Common fields
    pub deleted_at: Option<DBDateTime>,
    pub mfa_secret: Option<String>,
    pub mfa_enabled: bool,
    pub mfa_recovery_codes: Option<String>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::sessions::Entity")]
    Sessions,
    #[sea_orm(has_many = "super::audit_logs::Entity")]
    AuditLogs,
    #[sea_orm(has_many = "super::user_roles::Entity")]
    UserRoles,
}



impl Related<super::sessions::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Sessions.def()
    }
}

impl Related<super::audit_logs::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::AuditLogs.def()
    }
}

impl Related<super::user_roles::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::UserRoles.def()
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