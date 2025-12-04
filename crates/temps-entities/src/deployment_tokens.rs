//! Deployment Tokens Entity
//!
//! Deployment tokens provide API access credentials that are automatically
//! injected into deployments as TEMPS_API_TOKEN environment variable.
//! This allows deployed applications to access Temps APIs for:
//! - Enriching visitor data
//! - Sending emails
//! - Other platform features

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "deployment_tokens")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub project_id: i32,
    /// Optional environment ID - if set, token is specific to that environment
    /// If null, token applies to all environments in the project
    pub environment_id: Option<i32>,
    pub name: String,
    pub token_hash: String,
    /// First 8 characters for identification in UI
    pub token_prefix: String,
    /// JSON array of permission strings (e.g., ["visitors:enrich", "emails:send"])
    pub permissions: Option<Json>,
    pub is_active: bool,
    pub expires_at: Option<DBDateTime>,
    pub last_used_at: Option<DBDateTime>,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    /// User who created this token
    pub created_by: Option<i32>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "crate::projects::Entity",
        from = "Column::ProjectId",
        to = "crate::projects::Column::Id"
    )]
    Project,
    #[sea_orm(
        belongs_to = "crate::environments::Entity",
        from = "Column::EnvironmentId",
        to = "crate::environments::Column::Id"
    )]
    Environment,
    #[sea_orm(
        belongs_to = "crate::users::Entity",
        from = "Column::CreatedBy",
        to = "crate::users::Column::Id"
    )]
    CreatedByUser,
}

impl Related<crate::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

impl Related<crate::environments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Environment.def()
    }
}

impl Related<crate::users::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::CreatedByUser.def()
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

/// Deployment token permissions that can be granted to deployed applications
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentTokenPermission {
    /// Enrich visitor data with additional context
    VisitorsEnrich,
    /// Send emails through the Temps email service
    EmailsSend,
    /// Read analytics data
    AnalyticsRead,
    /// Log custom events
    EventsWrite,
    /// Read error tracking data
    ErrorsRead,
    /// Full access (all permissions)
    FullAccess,
}

impl DeploymentTokenPermission {
    /// Convert permission to string representation
    pub fn as_str(&self) -> &'static str {
        match self {
            DeploymentTokenPermission::VisitorsEnrich => "visitors:enrich",
            DeploymentTokenPermission::EmailsSend => "emails:send",
            DeploymentTokenPermission::AnalyticsRead => "analytics:read",
            DeploymentTokenPermission::EventsWrite => "events:write",
            DeploymentTokenPermission::ErrorsRead => "errors:read",
            DeploymentTokenPermission::FullAccess => "*",
        }
    }

    /// Parse permission from string
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "visitors:enrich" => Some(DeploymentTokenPermission::VisitorsEnrich),
            "emails:send" => Some(DeploymentTokenPermission::EmailsSend),
            "analytics:read" => Some(DeploymentTokenPermission::AnalyticsRead),
            "events:write" => Some(DeploymentTokenPermission::EventsWrite),
            "errors:read" => Some(DeploymentTokenPermission::ErrorsRead),
            "*" | "full_access" => Some(DeploymentTokenPermission::FullAccess),
            _ => None,
        }
    }

    /// Get all available permissions
    pub fn all() -> Vec<Self> {
        vec![
            DeploymentTokenPermission::VisitorsEnrich,
            DeploymentTokenPermission::EmailsSend,
            DeploymentTokenPermission::AnalyticsRead,
            DeploymentTokenPermission::EventsWrite,
            DeploymentTokenPermission::ErrorsRead,
            DeploymentTokenPermission::FullAccess,
        ]
    }

    /// Check if this permission grants access to another permission
    pub fn grants(&self, other: &DeploymentTokenPermission) -> bool {
        match self {
            DeploymentTokenPermission::FullAccess => true,
            _ => self == other,
        }
    }
}
