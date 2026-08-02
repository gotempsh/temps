use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

use super::deployment_config::{DeploymentConfig, SecurityConfig};
use super::upstream_config::UpstreamList;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "environments")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub slug: String,
    pub subdomain: String,
    pub last_deployment: Option<DBDateTime>,
    pub host: String,
    pub upstreams: UpstreamList,
    pub created_at: DBDateTime,
    pub updated_at: DBDateTime,
    pub project_id: i32,
    pub current_deployment_id: Option<i32>,
    pub branch: Option<String>,
    pub deleted_at: Option<DBDateTime>,
    /// Deployment configuration (CPU, memory, port, analytics, auto-deploy settings, security)
    /// These override project-level defaults for this specific environment
    /// Security settings are in deployment_config.security
    pub deployment_config: Option<DeploymentConfig>,
    /// Indicates if this is a preview environment (auto-created per branch)
    /// Use the 'branch' field to track which branch this preview is for
    pub is_preview: bool,
    /// When true, git pushes do NOT auto-deploy to this environment.
    /// Deployments must be promoted from another environment.
    pub protected: bool,
    /// When true, the environment's containers have been stopped due to inactivity
    /// (on-demand mode). They will be started on the next incoming request.
    pub sleeping: bool,
    /// Per-environment override for the CAPTCHA attack-mode challenge.
    /// `None` (NULL) means inherit the project-level `attack_mode` setting;
    /// `Some(true)`/`Some(false)` explicitly enable/disable the challenge for
    /// this environment, taking precedence over the project default.
    /// (A nullable boolean column maps to `Option<bool>` automatically.)
    pub attack_mode: Option<bool>,
    /// Per-environment override for the proxy's HTTP→HTTPS redirect.
    /// `None` (NULL) means inherit the proxy default, which redirects only when
    /// the requested host has an active TLS certificate. `Some(true)` always
    /// redirects plain HTTP for this environment (useful when TLS is terminated
    /// by an upstream CDN, so no local cert exists to trigger the default);
    /// `Some(false)` never redirects it (HTTP-only clients).
    /// Requests under `/.well-known/acme-challenge/` are exempt in every case so
    /// Let's Encrypt HTTP-01 issuance and renewal can always complete.
    pub force_https: Option<bool>,
    /// Last proxied request timestamp for on-demand environments.
    /// Persisted periodically by the idle sweep, not on every request.
    pub last_activity_at: Option<DBDateTime>,
}

impl Model {
    /// Get the effective deployment configuration by merging project and environment configs
    ///
    /// The project configuration serves as defaults, and the environment configuration
    /// overrides specific values. This allows setting project-wide defaults with
    /// environment-specific overrides.
    pub fn get_effective_deployment_config(
        &self,
        project_config: &DeploymentConfig,
    ) -> DeploymentConfig {
        let env_config = self.deployment_config.clone().unwrap_or_default();
        project_config.merge(&env_config)
    }

    /// Get the effective security configuration by merging global, project, and environment configs
    ///
    /// The inheritance chain is: Environment > Project > Global
    /// This allows:
    /// - Global defaults for all projects
    /// - Project-specific overrides (from project.deployment_config.security)
    /// - Environment-specific overrides (from environment.deployment_config.security)
    pub fn get_effective_security_config(
        &self,
        global_config: &SecurityConfig,
        project_deployment_config: &DeploymentConfig,
    ) -> SecurityConfig {
        // Extract security configs from deployment configs
        let project_security = project_deployment_config
            .security
            .as_ref()
            .cloned()
            .unwrap_or_default();

        let env_security = self
            .deployment_config
            .as_ref()
            .and_then(|dc| dc.security.clone())
            .unwrap_or_default();

        // Chain: global -> project -> environment
        global_config.merge(&project_security).merge(&env_security)
    }
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id"
    )]
    Project,
    #[sea_orm(has_many = "super::env_vars::Entity")]
    EnvVars,
    #[sea_orm(
        belongs_to = "super::deployments::Entity",
        from = "Column::CurrentDeploymentId",
        to = "super::deployments::Column::Id",
        on_update = "NoAction",
        on_delete = "SetNull"
    )]
    CurrentDeployment,
    #[sea_orm(has_many = "super::environment_domains::Entity")]
    EnvironmentDomains,
}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

impl Related<super::env_vars::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::EnvVars.def()
    }
}

impl Related<super::deployments::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::CurrentDeployment.def()
    }
}

impl Related<super::environment_domains::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::EnvironmentDomains.def()
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
