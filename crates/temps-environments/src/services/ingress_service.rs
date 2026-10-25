// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! An environment's ingress settings and `TempsLogin` grants (ADR-055 §11).
//!
//! - **Mode** (`environments.ingress_mode`): which evaluator decides the
//!   environment's requests at the proxy (`legacy`, `shadow`, `enforce`).
//! - **Require login**: a stored `TempsLogin` policy on the environment's
//!   endpoint. Everything else the endpoint enforces is derived from the
//!   environment's existing settings when the proxy builds its route
//!   snapshot, so it is not stored here.
//! - **Login grants**: one `ingress_login_grants` row per sign-in, so a grant
//!   can be revoked before it expires.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, DbErr, EntityTrait, QueryFilter, QueryOrder,
    Set, TransactionTrait,
};
use temps_entities::ingress_access_policies::StoredPolicy;
use temps_entities::ingress_mode::{IngressMode, SandboxPreviewAccess};
use temps_entities::{
    environments, ingress_access_policies, ingress_endpoints, ingress_login_grants, sandboxes,
};
use temps_workload_ingress::{AccessPolicy, ProductKind};
use thiserror::Error;

/// The permission a `TempsLogin` on an environment asks for: seeing the
/// project in the console is what opening its environments requires.
pub const ENVIRONMENT_LOGIN_PERMISSION: &str = "projects:read";

#[derive(Debug, Error)]
pub enum IngressError {
    #[error("Environment {environment_id} not found in project {project_id}")]
    EnvironmentNotFound {
        project_id: i32,
        environment_id: i32,
    },

    #[error("Environment {environment_id} not found")]
    EnvironmentIdNotFound { environment_id: i32 },

    #[error(
        "Environment {environment_id} has no ingress endpoint; it is created with the environment, so the migration that adds ingress endpoints has not run"
    )]
    EndpointMissing { environment_id: i32 },

    #[error("Environment {environment_id} does not require a Temps login")]
    NotLoginProtected { environment_id: i32 },

    #[error("Ingress {operation} for environment {environment_id} failed: {source}")]
    Database {
        environment_id: i32,
        operation: &'static str,
        #[source]
        source: DbErr,
    },

    #[error("Sandbox {sandbox_id} not found or destroyed")]
    SandboxNotFound { sandbox_id: i32 },

    #[error(
        "Sandbox {public_id} does not ask for a Temps login: its preview access is '{access}'"
    )]
    SandboxNotLoginProtected {
        public_id: String,
        access: &'static str,
    },

    #[error("Ingress {operation} for sandbox {sandbox_id} failed: {source}")]
    SandboxDatabase {
        sandbox_id: i32,
        operation: &'static str,
        #[source]
        source: DbErr,
    },

    #[error("Sandbox {public_id} not found or destroyed")]
    SandboxPublicIdNotFound { public_id: String },

    #[error("Ingress {operation} for sandbox {public_id} failed: {source}")]
    SandboxPublicIdDatabase {
        public_id: String,
        operation: &'static str,
        #[source]
        source: DbErr,
    },
}

/// What a Temps-login grant for a sandbox preview is checked against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxLoginTarget {
    pub sandbox_id: i32,
    pub public_id: String,
    pub owner_user_id: Option<i32>,
    pub project_id: Option<i32>,
}

impl SandboxLoginTarget {
    /// The first DNS label every preview host of this sandbox starts with:
    /// `ws-<hex>-` (followed by the port).
    pub fn preview_label_prefix(&self) -> String {
        let hex = self
            .public_id
            .strip_prefix("sbx_")
            .unwrap_or(&self.public_id);
        format!("ws-{hex}-")
    }

    /// Whether `host` is one of this sandbox's preview hosts:
    /// `ws-<hex>-<port>.<preview domain>`.
    pub fn owns_preview_host(&self, host: &str) -> bool {
        let Some((label, rest)) = host.split_once('.') else {
            return false;
        };
        let prefix = self.preview_label_prefix();
        !rest.is_empty()
            && label
                .strip_prefix(&prefix)
                .is_some_and(|port| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
    }
}

/// An environment's ingress settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngressSettings {
    pub project_id: i32,
    pub environment_id: i32,
    pub endpoint_id: i64,
    pub mode: IngressMode,
    /// The permission a stored `TempsLogin` asks for; `None` when the
    /// environment does not require a Temps login.
    pub login_permission: Option<String>,
}

impl IngressSettings {
    /// Whether the proxy asks visitors to sign in right now: a stored
    /// login only takes effect once the compiled endpoint decides.
    pub fn login_enforced(&self) -> bool {
        self.mode == IngressMode::Enforce && self.login_permission.is_some()
    }
}

/// What an update changes; `None` leaves a setting as it is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IngressSettingsUpdate {
    pub mode: Option<IngressMode>,
    pub require_login: Option<bool>,
}

/// A recorded `TempsLogin` grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedLoginGrant {
    pub grant_id: i64,
    pub expires_at: DateTime<Utc>,
}

pub struct IngressService {
    db: Arc<DatabaseConnection>,
}

impl IngressService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    fn db_error(
        environment_id: i32,
        operation: &'static str,
    ) -> impl FnOnce(DbErr) -> IngressError {
        move |source| IngressError::Database {
            environment_id,
            operation,
            source,
        }
    }

    pub async fn get_settings(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<IngressSettings, IngressError> {
        let environment = environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await
            .map_err(Self::db_error(environment_id, "settings lookup"))?
            .ok_or(IngressError::EnvironmentNotFound {
                project_id,
                environment_id,
            })?;
        self.settings_for(self.db.as_ref(), &environment).await
    }

    /// The settings of a live environment found by id alone, for a login
    /// request that names only the environment.
    pub async fn get_settings_by_environment(
        &self,
        environment_id: i32,
    ) -> Result<IngressSettings, IngressError> {
        let environment = environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await
            .map_err(Self::db_error(environment_id, "settings lookup"))?
            .ok_or(IngressError::EnvironmentIdNotFound { environment_id })?;
        self.settings_for(self.db.as_ref(), &environment).await
    }

    async fn settings_for<C: sea_orm::ConnectionTrait>(
        &self,
        db: &C,
        environment: &environments::Model,
    ) -> Result<IngressSettings, IngressError> {
        let environment_id = environment.id;
        let endpoint = Self::endpoint(db, environment_id).await?;
        let policies = ingress_access_policies::Entity::find()
            .filter(ingress_access_policies::Column::EndpointId.eq(endpoint.id))
            .order_by_asc(ingress_access_policies::Column::Position)
            .all(db)
            .await
            .map_err(Self::db_error(environment_id, "policy lookup"))?;
        let login_permission = policies.into_iter().find_map(|row| match row.policy.0 {
            AccessPolicy::TempsLogin { permission } => Some(permission),
            _ => None,
        });
        Ok(IngressSettings {
            project_id: environment.project_id,
            environment_id,
            endpoint_id: endpoint.id,
            mode: environment.ingress_mode,
            login_permission,
        })
    }

    async fn endpoint<C: sea_orm::ConnectionTrait>(
        db: &C,
        environment_id: i32,
    ) -> Result<ingress_endpoints::Model, IngressError> {
        ingress_endpoints::Entity::find()
            .filter(ingress_endpoints::Column::EnvironmentId.eq(environment_id))
            .filter(ingress_endpoints::Column::PathPrefix.is_null())
            .one(db)
            .await
            .map_err(Self::db_error(environment_id, "endpoint lookup"))?
            .ok_or(IngressError::EndpointMissing { environment_id })
    }

    /// Applies `update` in one transaction, so the proxy's next snapshot
    /// sees the mode and the login together.
    pub async fn update_settings(
        &self,
        project_id: i32,
        environment_id: i32,
        update: IngressSettingsUpdate,
        user_id: Option<i32>,
    ) -> Result<IngressSettings, IngressError> {
        let txn = self
            .db
            .begin()
            .await
            .map_err(Self::db_error(environment_id, "settings transaction"))?;
        let environment = environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::DeletedAt.is_null())
            .one(&txn)
            .await
            .map_err(Self::db_error(environment_id, "settings lookup"))?
            .ok_or(IngressError::EnvironmentNotFound {
                project_id,
                environment_id,
            })?;

        let environment = match update.mode {
            Some(mode) if mode != environment.ingress_mode => {
                let mut active: environments::ActiveModel = environment.into();
                active.ingress_mode = Set(mode);
                active.updated_at = Set(Utc::now());
                active
                    .update(&txn)
                    .await
                    .map_err(Self::db_error(environment_id, "mode update"))?
            }
            _ => environment,
        };

        if let Some(require_login) = update.require_login {
            let endpoint = Self::endpoint(&txn, environment_id).await?;
            let policies = ingress_access_policies::Entity::find()
                .filter(ingress_access_policies::Column::EndpointId.eq(endpoint.id))
                .order_by_asc(ingress_access_policies::Column::Position)
                .all(&txn)
                .await
                .map_err(Self::db_error(environment_id, "policy lookup"))?;
            let has_login = policies
                .iter()
                .any(|row| matches!(row.policy.0, AccessPolicy::TempsLogin { .. }));
            if require_login && !has_login {
                let position = policies
                    .iter()
                    .map(|row| row.position + 1)
                    .max()
                    .unwrap_or(0);
                ingress_access_policies::ActiveModel {
                    endpoint_id: Set(endpoint.id),
                    position: Set(position),
                    policy: Set(StoredPolicy(AccessPolicy::TempsLogin {
                        permission: ENVIRONMENT_LOGIN_PERMISSION.to_string(),
                    })),
                    created_by: Set(user_id),
                    ..Default::default()
                }
                .insert(&txn)
                .await
                .map_err(Self::db_error(environment_id, "login policy insert"))?;
            } else if !require_login && has_login {
                let ids: Vec<i64> = policies
                    .iter()
                    .filter(|row| matches!(row.policy.0, AccessPolicy::TempsLogin { .. }))
                    .map(|row| row.id)
                    .collect();
                ingress_access_policies::Entity::delete_many()
                    .filter(ingress_access_policies::Column::Id.is_in(ids))
                    .exec(&txn)
                    .await
                    .map_err(Self::db_error(environment_id, "login policy delete"))?;
            }
        }

        let settings = self.settings_for(&txn, &environment).await?;
        txn.commit()
            .await
            .map_err(Self::db_error(environment_id, "settings commit"))?;
        Ok(settings)
    }

    /// Records a grant for `user_id` to open `host` of the environment.
    /// `expires_at` is when the last session cookie it can buy expires.
    pub async fn record_login_grant(
        &self,
        user_id: i32,
        environment_id: i32,
        host: &str,
        permission: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<IssuedLoginGrant, IngressError> {
        self.insert_login_grant(
            user_id,
            ProductKind::Environment,
            i64::from(environment_id),
            host,
            permission,
            expires_at,
        )
        .await
        .map_err(Self::db_error(environment_id, "login grant insert"))
    }

    /// A live sandbox whose preview asks for a Temps login, for a login
    /// request that names it. Destroyed sandboxes are not found; a sandbox
    /// in another access mode is [`IngressError::SandboxNotLoginProtected`].
    pub async fn sandbox_login_target(
        &self,
        sandbox_id: i32,
    ) -> Result<SandboxLoginTarget, IngressError> {
        let sandbox = sandboxes::Entity::find_by_id(sandbox_id)
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .one(self.db.as_ref())
            .await
            .map_err(|source| IngressError::SandboxDatabase {
                sandbox_id,
                operation: "sandbox lookup",
                source,
            })?
            .ok_or(IngressError::SandboxNotFound { sandbox_id })?;
        Self::login_target_of(sandbox)
    }

    /// [`Self::sandbox_login_target`] by the sandbox's public id
    /// (`sbx_<hex>`), for the console, which only knows that one.
    pub async fn sandbox_login_target_by_public_id(
        &self,
        public_id: &str,
    ) -> Result<SandboxLoginTarget, IngressError> {
        let sandbox = sandboxes::Entity::find()
            .filter(sandboxes::Column::PublicId.eq(public_id))
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .one(self.db.as_ref())
            .await
            .map_err(|source| IngressError::SandboxPublicIdDatabase {
                public_id: public_id.to_string(),
                operation: "sandbox lookup",
                source,
            })?
            .ok_or_else(|| IngressError::SandboxPublicIdNotFound {
                public_id: public_id.to_string(),
            })?;
        Self::login_target_of(sandbox)
    }

    fn login_target_of(sandbox: sandboxes::Model) -> Result<SandboxLoginTarget, IngressError> {
        if sandbox.preview_access != SandboxPreviewAccess::TempsLogin {
            return Err(IngressError::SandboxNotLoginProtected {
                public_id: sandbox.public_id,
                access: sandbox.preview_access.as_str(),
            });
        }
        Ok(SandboxLoginTarget {
            sandbox_id: sandbox.id,
            public_id: sandbox.public_id,
            owner_user_id: sandbox.user_id,
            project_id: sandbox.project_id,
        })
    }

    /// Records a grant for `user_id` to open `host`, a preview host of the
    /// sandbox.
    pub async fn record_sandbox_login_grant(
        &self,
        user_id: i32,
        sandbox_id: i32,
        host: &str,
        permission: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<IssuedLoginGrant, IngressError> {
        self.insert_login_grant(
            user_id,
            ProductKind::Sandbox,
            i64::from(sandbox_id),
            host,
            permission,
            expires_at,
        )
        .await
        .map_err(|source| IngressError::SandboxDatabase {
            sandbox_id,
            operation: "login grant insert",
            source,
        })
    }

    async fn insert_login_grant(
        &self,
        user_id: i32,
        owner_kind: ProductKind,
        owner_id: i64,
        host: &str,
        permission: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<IssuedLoginGrant, DbErr> {
        let row = ingress_login_grants::ActiveModel {
            user_id: Set(user_id),
            owner_kind: Set(owner_kind.as_str().to_string()),
            owner_id: Set(owner_id),
            host: Set(host.to_ascii_lowercase()),
            permission: Set(permission.to_string()),
            expires_at: Set(expires_at),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await?;
        Ok(IssuedLoginGrant {
            grant_id: row.id,
            expires_at: row.expires_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_database::test_utils::{is_container_runtime_unavailable, TestDatabase};
    use temps_entities::{preset::Preset, projects, upstream_config::UpstreamList, users};

    #[tokio::test]
    async fn a_missing_environment_is_reported_with_its_ids() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<environments::Model>::new()])
            .into_connection();
        let service = IngressService::new(Arc::new(db));
        let error = service.get_settings(3, 9).await.unwrap_err();
        assert!(matches!(
            error,
            IngressError::EnvironmentNotFound {
                project_id: 3,
                environment_id: 9
            }
        ));
        assert_eq!(error.to_string(), "Environment 9 not found in project 3");
    }

    #[tokio::test]
    async fn a_database_failure_names_the_environment_and_operation() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([DbErr::Custom("connection reset".into())])
            .into_connection();
        let service = IngressService::new(Arc::new(db));
        let error = service.get_settings_by_environment(4).await.unwrap_err();
        assert!(matches!(
            error,
            IngressError::Database {
                environment_id: 4,
                operation: "settings lookup",
                ..
            }
        ));
    }

    fn sandbox_row(access: SandboxPreviewAccess) -> sandboxes::Model {
        let now = Utc::now();
        sandboxes::Model {
            id: 11,
            public_id: "sbx_0123456789abcdef".into(),
            user_id: Some(4),
            agent_run_id: None,
            name: "sbx-11".into(),
            status: "running".into(),
            lifecycle: "ephemeral".into(),
            project_id: Some(3),
            source_repo_url: None,
            image: None,
            work_dir: "/workspace".into(),
            timeout_secs: 600,
            metadata: None,
            backend: None,
            node_id: None,
            created_at: now,
            last_activity_at: now,
            expires_at: now,
            preview_password_hash: None,
            preview_password_hint: None,
            preview_access: access,
            preview_allowed_cidrs: Vec::new(),
            egress_policy: Default::default(),
        }
    }

    #[tokio::test]
    async fn a_temps_login_sandbox_is_a_login_target() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![sandbox_row(SandboxPreviewAccess::TempsLogin)]])
            .into_connection();
        let service = IngressService::new(Arc::new(db));
        let target = service.sandbox_login_target(11).await.unwrap();
        assert_eq!(
            target,
            SandboxLoginTarget {
                sandbox_id: 11,
                public_id: "sbx_0123456789abcdef".into(),
                owner_user_id: Some(4),
                project_id: Some(3),
            }
        );
    }

    #[tokio::test]
    async fn sandboxes_in_other_modes_or_missing_get_no_login_grant() {
        for access in [SandboxPreviewAccess::Public, SandboxPreviewAccess::Password] {
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![sandbox_row(access)]])
                .into_connection();
            let error = IngressService::new(Arc::new(db))
                .sandbox_login_target(11)
                .await
                .unwrap_err();
            assert_eq!(
                error.to_string(),
                format!(
                    "Sandbox sbx_0123456789abcdef does not ask for a Temps login: its preview access is '{}'",
                    access.as_str()
                )
            );
        }
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<sandboxes::Model>::new()])
            .into_connection();
        let error = IngressService::new(Arc::new(db))
            .sandbox_login_target(12)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            IngressError::SandboxNotFound { sandbox_id: 12 }
        ));
    }

    #[tokio::test]
    async fn a_login_target_is_found_by_public_id_with_the_same_checks() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![sandbox_row(SandboxPreviewAccess::TempsLogin)]])
            .append_query_results([vec![sandbox_row(SandboxPreviewAccess::Public)]])
            .append_query_results([Vec::<sandboxes::Model>::new()])
            .append_query_errors([DbErr::Custom("connection reset".into())])
            .into_connection();
        let service = IngressService::new(Arc::new(db));
        let target = service
            .sandbox_login_target_by_public_id("sbx_0123456789abcdef")
            .await
            .unwrap();
        assert_eq!(target.sandbox_id, 11);
        assert_eq!(target.owner_user_id, Some(4));
        assert!(matches!(
            service
                .sandbox_login_target_by_public_id("sbx_0123456789abcdef")
                .await
                .unwrap_err(),
            IngressError::SandboxNotLoginProtected {
                access: "public",
                ..
            }
        ));
        let missing = service
            .sandbox_login_target_by_public_id("sbx_ffffffffffffffff")
            .await
            .unwrap_err();
        assert_eq!(
            missing.to_string(),
            "Sandbox sbx_ffffffffffffffff not found or destroyed"
        );
        let failed = service
            .sandbox_login_target_by_public_id("sbx_ffffffffffffffff")
            .await
            .unwrap_err();
        assert!(matches!(
            &failed,
            IngressError::SandboxPublicIdDatabase {
                public_id,
                operation: "sandbox lookup",
                ..
            } if public_id == "sbx_ffffffffffffffff"
        ));
    }

    #[test]
    fn a_sandbox_owns_only_its_own_preview_hosts() {
        let target = SandboxLoginTarget {
            sandbox_id: 11,
            public_id: "sbx_0123456789abcdef".into(),
            owner_user_id: Some(4),
            project_id: None,
        };
        assert!(target.owns_preview_host("ws-0123456789abcdef-3000.preview.example.test"));
        for host in [
            "ws-0123456789abcdee-3000.preview.example.test",
            "ws-0123456789abcdef-.preview.example.test",
            "ws-0123456789abcdef-30x0.preview.example.test",
            "ws-0123456789abcdef-3000",
            "app.example.test",
        ] {
            assert!(!target.owns_preview_host(host), "{host}");
        }
    }

    async fn test_database() -> Option<TestDatabase> {
        match TestDatabase::with_migrations().await {
            Ok(database) => Some(database),
            Err(error) if is_container_runtime_unavailable(&error.to_string()) => {
                eprintln!("Docker unavailable, skipping ingress service test: {error:#}");
                None
            }
            Err(error) => panic!("ingress service test database setup failed: {error:#}"),
        }
    }

    #[tokio::test]
    async fn settings_round_trip_and_grants_are_recorded() {
        let Some(test_db) = test_database().await else {
            return;
        };
        let db = test_db.connection_arc();
        let project = projects::ActiveModel {
            name: Set("Shop".to_string()),
            repo_name: Set("shop".to_string()),
            repo_owner: Set("temps-tests".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            slug: Set("shop".to_string()),
            preset: Set(Preset::NextJs),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("project fixture");
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Production".to_string()),
            slug: Set("production".to_string()),
            host: Set("shop.example.test".to_string()),
            upstreams: Set(UpstreamList::default()),
            subdomain: Set("shop.example.test".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("environment fixture");
        let user = users::ActiveModel {
            name: Set("Operator".to_string()),
            email: Set("operator@example.test".to_string()),
            email_verified: Set(true),
            must_change_password: Set(false),
            mfa_enabled: Set(false),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("user fixture");

        let service = IngressService::new(db.clone());
        // The environment trigger created its endpoint; nothing is stored.
        let settings = service
            .get_settings(project.id, environment.id)
            .await
            .unwrap();
        assert_eq!(settings.mode, IngressMode::Legacy);
        assert_eq!(settings.login_permission, None);
        assert!(!settings.login_enforced());

        let settings = service
            .update_settings(
                project.id,
                environment.id,
                IngressSettingsUpdate {
                    mode: Some(IngressMode::Enforce),
                    require_login: Some(true),
                },
                Some(user.id),
            )
            .await
            .unwrap();
        assert_eq!(settings.mode, IngressMode::Enforce);
        assert_eq!(
            settings.login_permission.as_deref(),
            Some(ENVIRONMENT_LOGIN_PERMISSION)
        );
        assert!(settings.login_enforced());

        // Asking again does not add a second login.
        service
            .update_settings(
                project.id,
                environment.id,
                IngressSettingsUpdate {
                    mode: None,
                    require_login: Some(true),
                },
                Some(user.id),
            )
            .await
            .unwrap();
        let stored = ingress_access_policies::Entity::find()
            .filter(ingress_access_policies::Column::EndpointId.eq(settings.endpoint_id))
            .all(db.as_ref())
            .await
            .unwrap();
        assert_eq!(stored.len(), 1);

        let by_id = service
            .get_settings_by_environment(environment.id)
            .await
            .unwrap();
        assert_eq!(by_id, settings);

        let expires_at = Utc::now() + chrono::Duration::hours(12);
        let grant = service
            .record_login_grant(
                user.id,
                environment.id,
                "Shop.Example.Test",
                ENVIRONMENT_LOGIN_PERMISSION,
                expires_at,
            )
            .await
            .unwrap();
        let row = ingress_login_grants::Entity::find_by_id(grant.grant_id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("grant row");
        assert_eq!(row.host, "shop.example.test");
        assert_eq!(row.owner_id, i64::from(environment.id));
        assert_eq!(row.revoked_at, None);

        let settings = service
            .update_settings(
                project.id,
                environment.id,
                IngressSettingsUpdate {
                    mode: Some(IngressMode::Shadow),
                    require_login: Some(false),
                },
                Some(user.id),
            )
            .await
            .unwrap();
        assert_eq!(settings.mode, IngressMode::Shadow);
        assert_eq!(settings.login_permission, None);

        // Another project's id does not reach this environment.
        assert!(matches!(
            service
                .get_settings(project.id + 1000, environment.id)
                .await,
            Err(IngressError::EnvironmentNotFound { .. })
        ));
    }
}
