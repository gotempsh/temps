// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Credentials a sandbox can use against a linked MariaDB service (ADR-051).
//!
//! The sandbox gets a database of its own (`_sandbox_<project>`, or
//! `_sandboxid_<sandbox>` for a sandbox linked directly; a namespace no
//! deployment can use; see [`crate::sandbox_engines::SANDBOX_DATABASE_PREFIX`])
//! and a user that can use that database and nothing else, with a password
//! derived from the administrator's so nothing new is stored. The variables
//! use the names a deployment gets (`MYSQL_*`, `MARIADB_*`, `DATABASE_URL`).
//!
//! MariaDB has no database owner, so the provenance of a sandbox database is
//! its sandbox user: it is created before the database, and only root can
//! create or drop it (the sandbox user holds no global privilege). An existing
//! database without that user was not created for a sandbox and is never
//! granted to one.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions};
use sqlx::ConnectOptions;
use temps_entities::external_services;
use tracing::info;

pub use crate::externalsvc::mariadb::sandbox_user_count_query;
use crate::externalsvc::mariadb::{mariadb_container_name, MariaDbService, USER_EXISTS_QUERY};
use crate::externalsvc::scoped_resource_name;
use crate::sandbox_access::sandbox_role_name;
use crate::sandbox_engines::{
    choose_sandbox_resource, derive_password, is_plain_identifier, require_local, text_parameter,
    ExistingResource, SandboxAccessEngine, SandboxAccessRequest, SandboxNamespace,
    SandboxNamespaceConflict, SandboxResourceChoice,
};
use crate::services::{ExternalServiceError, ExternalServiceManager};

const MARIADB_INTERNAL_PORT: &str = "3306";
const DATABASE_MAX: usize = 63;
const PASSWORD_TAG: &str = "temps-scoped-mariadb-v1";
/// Connections the user may hold at once, so a runaway loop in a sandbox
/// cannot exhaust the server's connection slots.
const USER_CONNECTION_LIMIT: u32 = 20;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const ENGINE: &str = "MariaDB";
/// Bound with the database name.
const DATABASE_EXISTS_QUERY: &str =
    "SELECT COUNT(*) FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = ?";

/// What the database `database` is to the sandbox user that goes with it.
pub fn classify_database(database_exists: bool, user_exists: bool, user: &str) -> ExistingResource {
    match (database_exists, user_exists) {
        (false, _) => ExistingResource::Missing,
        (true, true) => ExistingResource::Sandbox,
        (true, false) => ExistingResource::Foreign {
            evidence: format!("no sandbox user '{user}'@'%' exists for it"),
        },
    }
}

/// Gives each sandbox its own database and a user limited to it.
pub struct MariaDbSandboxAccess;

/// The database, user and statements that give one sandbox its access.
#[derive(Clone, PartialEq, Eq)]
pub struct MariaDbSandboxPlan {
    pub database: String,
    pub user: String,
    password: String,
}

impl std::fmt::Debug for MariaDbSandboxPlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MariaDbSandboxPlan")
            .field("database", &self.database)
            .field("user", &self.user)
            .field("password", &"<redacted>")
            .finish()
    }
}

impl MariaDbSandboxPlan {
    /// The plan for the sandbox namespace: see
    /// [`SandboxNamespace::database_name`].
    pub fn new(
        service_id: i32,
        namespace: &SandboxNamespace,
        root_password: &str,
    ) -> Result<Self, String> {
        Self::for_database(service_id, namespace.database_name(), root_password)
    }

    /// The plan for a project's pre-namespace name `<project>_sandbox`, used only
    /// for a database whose sandbox user exists.
    pub fn legacy(
        service_id: i32,
        project_slug: &str,
        root_password: &str,
    ) -> Result<Self, String> {
        Self::for_database(
            service_id,
            MariaDbService::normalize_database_name(&scoped_resource_name(project_slug, "sandbox")),
            root_password,
        )
    }

    fn for_database(
        service_id: i32,
        database: String,
        root_password: &str,
    ) -> Result<Self, String> {
        let user = sandbox_role_name(&database);
        for (kind, value) in [("database", &database), ("user", &user)] {
            if !is_plain_identifier(value, DATABASE_MAX) {
                return Err(format!(
                    "the {kind} name '{value}' is not a plain lowercase identifier"
                ));
            }
        }
        if root_password.is_empty() {
            return Err("the service has no root password to provision a user with".to_string());
        }
        Ok(Self {
            password: derive_password(root_password, PASSWORD_TAG, service_id, &user)?,
            database,
            user,
        })
    }

    /// Statements run as the administrator on every connect so a user or
    /// grant changed by hand is put back. Identifiers are plain `[a-z0-9_]`
    /// and the password is hex, so nothing needs quoting beyond the delimiters
    /// here. The database name's underscores are escaped in the grant, where
    /// `_` would otherwise match any character and reach other databases.
    ///
    /// With `create_database`, the user is created first (it is the
    /// database's provenance) and the database is created without `IF NOT
    /// EXISTS`, so one that appeared since it was inspected fails the run
    /// instead of being granted to the sandbox.
    pub fn statements(&self, create_database: bool) -> Vec<String> {
        let Self {
            database,
            user,
            password,
        } = self;
        let escaped = database.replace('_', "\\_");
        let mut statements = vec![format!(
            "CREATE USER IF NOT EXISTS '{user}'@'%' IDENTIFIED BY '{password}'"
        )];
        if create_database {
            statements.push(format!("CREATE DATABASE `{database}`"));
        }
        statements.extend([
            format!(
                "ALTER USER '{user}'@'%' IDENTIFIED BY '{password}' \
                 WITH MAX_USER_CONNECTIONS {USER_CONNECTION_LIMIT}"
            ),
            format!("REVOKE ALL PRIVILEGES, GRANT OPTION FROM '{user}'@'%'"),
            format!("GRANT ALL PRIVILEGES ON `{escaped}`.* TO '{user}'@'%'"),
        ]);
        statements
    }

    pub fn environment(&self, host: &str) -> Result<HashMap<String, String>, String> {
        MariaDbService::build_env_vars(
            host,
            MARIADB_INTERNAL_PORT,
            &self.database,
            &self.user,
            &self.password,
        )
        .map_err(|error| error.to_string())
    }
}

fn failure(service_id: i32, what: &str, reason: impl std::fmt::Display) -> ExternalServiceError {
    ExternalServiceError::InternalError {
        reason: format!("Failed to {what} on MariaDB service {service_id}: {reason}"),
    }
}

#[async_trait]
impl SandboxAccessEngine for MariaDbSandboxAccess {
    fn service_types(&self) -> &'static [&'static str] {
        &["mariadb"]
    }

    async fn containers(
        &self,
        manager: &ExternalServiceManager,
        service: &external_services::Model,
    ) -> Result<Vec<String>, ExternalServiceError> {
        if service.node_id.is_some() {
            return Ok(Vec::new());
        }
        let parameters = manager.get_service_parameters(service.id).await?;
        Ok(vec![text_parameter(&parameters, "container_name")
            .unwrap_or_else(|| mariadb_container_name(&service.name))])
    }

    async fn issue(
        &self,
        _manager: &ExternalServiceManager,
        request: SandboxAccessRequest<'_>,
    ) -> Result<HashMap<String, String>, ExternalServiceError> {
        let SandboxAccessRequest {
            service,
            namespace,
            project_id,
            parameters,
        } = request;
        require_local(service)?;
        let root_password = text_parameter(parameters, "root_password").unwrap_or_default();
        let unsupported = |reason: String| ExternalServiceError::SandboxAccessUnsupported {
            service_id: service.id,
            service_type: service.service_type.clone(),
            reason,
        };
        let reserved =
            MariaDbSandboxPlan::new(service.id, namespace, &root_password).map_err(unsupported)?;
        // Only a project's sandboxes had a legacy name, and only one naming a
        // single project; one that is not a plain identifier was never
        // provisioned either.
        let legacy = namespace
            .legacy_project_slug()
            .and_then(|slug| MariaDbSandboxPlan::legacy(service.id, slug, &root_password).ok());
        let container = text_parameter(parameters, "container_name")
            .unwrap_or_else(|| mariadb_container_name(&service.name));
        let host = text_parameter(parameters, "host").unwrap_or_else(|| "localhost".to_string());
        let port: u16 = text_parameter(parameters, "port")
            .and_then(|port| port.parse().ok())
            .unwrap_or(3306);

        let options = MySqlConnectOptions::new()
            .host(&host)
            .port(port)
            .username("root")
            .password(&root_password)
            .disable_statement_logging();
        let pool = MySqlPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(CONNECT_TIMEOUT)
            .connect_with(options)
            .await
            .map_err(|error| failure(service.id, &format!("connect to {host}:{port}"), error))?;

        // Read what exists before changing anything.
        let reserved_state = inspect(&pool, service.id, &reserved).await?;
        let legacy_state = match &legacy {
            Some(plan) => inspect(&pool, service.id, plan).await?,
            None => ExistingResource::Missing,
        };
        let (plan, create_database) = match (
            choose_sandbox_resource(&legacy_state, &reserved_state),
            &legacy,
        ) {
            (SandboxResourceChoice::Adopt, _) => (&reserved, false),
            (SandboxResourceChoice::AdoptLegacy, Some(plan)) => (plan, false),
            // `legacy_state` is only `Sandbox` when there is a legacy plan.
            (SandboxResourceChoice::AdoptLegacy, None) | (SandboxResourceChoice::Create, _) => {
                (&reserved, true)
            }
            (SandboxResourceChoice::Refuse { evidence }, _) => {
                pool.close().await;
                return Err(ExternalServiceError::SandboxNamespaceConflict {
                    service_id: service.id,
                    conflict: SandboxNamespaceConflict::NotASandboxDatabase {
                        engine: ENGINE,
                        database: reserved.database.clone(),
                        evidence,
                    },
                });
            }
        };
        for statement in plan.statements(create_database) {
            sqlx::raw_sql(&statement)
                .execute(&pool)
                .await
                .map_err(|error| {
                    failure(
                        service.id,
                        &format!("provision the sandbox database '{}'", plan.database),
                        error,
                    )
                })?;
        }
        pool.close().await;

        info!(
            service_id = service.id,
            project_id = ?project_id,
            database = %plan.database,
            user = %plan.user,
            "Provisioned a database and user for a sandbox"
        );
        plan.environment(&container)
            .map_err(|reason| failure(service.id, "build the sandbox variables", reason))
    }
}

/// What the database of `plan` is, read without changing anything.
async fn inspect(
    pool: &sqlx::MySqlPool,
    service_id: i32,
    plan: &MariaDbSandboxPlan,
) -> Result<ExistingResource, ExternalServiceError> {
    let count = |query: &'static str, value: String| async move {
        sqlx::query_scalar::<_, i64>(query)
            .bind(value)
            .fetch_one(pool)
            .await
    };
    let database_exists = count(DATABASE_EXISTS_QUERY, plan.database.clone())
        .await
        .map_err(|error| {
            failure(
                service_id,
                &format!("check whether the database '{}' exists", plan.database),
                error,
            )
        })?
        > 0;
    let user_exists = count(USER_EXISTS_QUERY, plan.user.clone())
        .await
        .map_err(|error| {
            failure(
                service_id,
                &format!("check whether the user '{}' exists", plan.user),
                error,
            )
        })?
        > 0;
    Ok(classify_database(database_exists, user_exists, &plan.user))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> MariaDbSandboxPlan {
        MariaDbSandboxPlan::new(4, &SandboxNamespace::project("shop"), "rootpassword")
            .expect("plan")
    }

    #[test]
    fn the_database_and_user_are_named_for_the_namespace() {
        let plan = plan();
        assert_eq!(plan.database, "_sandbox_shop");
        assert_eq!(plan.user, "sbx__sandbox_shop");
    }

    #[test]
    fn the_grant_is_limited_to_one_database_with_underscores_escaped() {
        let statements = plan().statements(true);
        let grant = statements
            .iter()
            .find(|statement| statement.starts_with("GRANT"))
            .expect("a grant");
        assert_eq!(
            grant,
            "GRANT ALL PRIVILEGES ON `\\_sandbox\\_shop`.* TO 'sbx__sandbox_shop'@'%'"
        );
        assert!(
            !statements.iter().any(|s| s.contains("*.*")),
            "no global grant"
        );
    }

    #[test]
    fn privileges_are_revoked_before_they_are_granted_so_drift_is_repaired() {
        let statements = plan().statements(true);
        let position = |prefix: &str| {
            statements
                .iter()
                .position(|statement| statement.starts_with(prefix))
                .expect(prefix)
        };
        assert!(position("REVOKE") < position("GRANT"));
        assert!(position("CREATE USER") < position("ALTER USER"));
        assert!(statements[2].contains("MAX_USER_CONNECTIONS 20"));
    }

    #[test]
    fn an_existing_database_is_never_created_again_and_the_user_comes_first() {
        let fresh = plan().statements(true);
        let user = fresh
            .iter()
            .position(|statement| statement.starts_with("CREATE USER"))
            .expect("user");
        let database = fresh
            .iter()
            .position(|statement| statement.starts_with("CREATE DATABASE"))
            .expect("database");
        assert!(user < database, "the user is the provenance: {fresh:?}");
        // No IF NOT EXISTS: a database that appeared since it was inspected
        // fails the run rather than being granted to the sandbox.
        assert_eq!(fresh[database], "CREATE DATABASE `_sandbox_shop`");
        let adopted = plan().statements(false);
        assert!(
            !adopted.iter().any(|s| s.contains("CREATE DATABASE")),
            "{adopted:?}"
        );
    }

    #[test]
    fn a_database_is_the_sandboxs_only_when_its_user_exists() {
        assert_eq!(
            classify_database(false, false, "sbx__sandbox_shop"),
            ExistingResource::Missing
        );
        assert_eq!(
            classify_database(false, true, "sbx__sandbox_shop"),
            ExistingResource::Missing
        );
        assert_eq!(
            classify_database(true, true, "sbx__sandbox_shop"),
            ExistingResource::Sandbox
        );
        // The reported collision: an environment slugged `sandbox` made
        // `shop_sandbox` and no sandbox user exists for it.
        match classify_database(true, false, "sbx_shop_sandbox") {
            ExistingResource::Foreign { evidence } => {
                assert!(evidence.contains("sbx_shop_sandbox"), "{evidence}")
            }
            other => panic!("expected Foreign, got {other:?}"),
        }
    }

    #[test]
    fn the_legacy_plan_keeps_the_old_names_and_the_deployment_check_is_quoted() {
        let legacy = MariaDbSandboxPlan::legacy(4, "shop", "rootpassword").expect("plan");
        assert_eq!(legacy.database, "shop_sandbox");
        assert_eq!(legacy.user, "sbx_shop_sandbox");
        assert_eq!(
            sandbox_user_count_query("'sbx_shop_sandbox'"),
            "SELECT COUNT(*) FROM mysql.user WHERE User = 'sbx_shop_sandbox' AND Host = '%'"
        );
    }

    #[test]
    fn the_password_is_stable_and_follows_the_root_password() {
        assert_eq!(plan(), plan());
        let other = MariaDbSandboxPlan::new(4, &SandboxNamespace::project("shop"), "another-root")
            .expect("plan");
        assert_ne!(plan().password, other.password);
        let elsewhere =
            MariaDbSandboxPlan::new(5, &SandboxNamespace::project("shop"), "rootpassword")
                .expect("plan");
        assert_ne!(plan().password, elsewhere.password);
    }

    #[test]
    fn a_service_without_a_root_password_cannot_be_provisioned() {
        let error = MariaDbSandboxPlan::new(4, &SandboxNamespace::project("shop"), "")
            .expect_err("no password");
        assert!(error.contains("root password"), "{error}");
    }

    #[test]
    fn a_namespace_that_could_break_out_of_an_identifier_is_normalised_or_refused() {
        // Anything but [a-z0-9_] is replaced by the shared normaliser, so the
        // statement can never contain a quote or backtick from a name.
        let plan = MariaDbSandboxPlan::new(
            4,
            &SandboxNamespace::project("sh`op'; drop"),
            "rootpassword",
        )
        .expect("plan");
        assert!(
            is_plain_identifier(&plan.database, DATABASE_MAX),
            "{plan:?}"
        );
        assert!(is_plain_identifier(&plan.user, DATABASE_MAX), "{plan:?}");
        for statement in plan.statements(true) {
            assert!(!statement.contains(';'), "{statement}");
            assert_eq!(statement.matches('`').count() % 2, 0, "{statement}");
        }
    }

    #[test]
    fn the_environment_keeps_the_deployment_names() {
        let environment = plan().environment("maria-shop").expect("environment");
        for name in [
            "DATABASE_URL",
            "MYSQL_URL",
            "MYSQL_HOST",
            "MYSQL_PORT",
            "MYSQL_DATABASE",
            "MYSQL_USER",
            "MYSQL_PASSWORD",
            "MARIADB_URL",
        ] {
            assert!(environment.contains_key(name), "{name}");
        }
        assert_eq!(environment["MYSQL_HOST"], "maria-shop");
        assert_eq!(environment["MYSQL_PORT"], "3306");
        assert_eq!(environment["MYSQL_USER"], "sbx__sandbox_shop");
        assert!(!environment
            .values()
            .any(|value| value.contains("rootpassword")));
    }

    #[test]
    fn debug_output_hides_the_password() {
        let plan = plan();
        assert!(!format!("{plan:?}").contains(&plan.password));
    }

    // ---- against a real server -------------------------------------------

    use testcontainers::{
        core::{ContainerPort, Mount},
        runners::AsyncRunner,
        GenericImage, ImageExt,
    };

    async fn pool_as(
        port: u16,
        user: &str,
        password: &str,
    ) -> Result<sqlx::MySqlPool, sqlx::Error> {
        MySqlPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(
                MySqlConnectOptions::new()
                    .host("127.0.0.1")
                    .port(port)
                    .username(user)
                    .password(password),
            )
            .await
    }

    #[tokio::test]
    async fn an_environment_database_is_never_granted_to_a_sandbox() {
        let docker_up = match bollard::Docker::connect_with_defaults() {
            Ok(docker) => docker.ping().await.is_ok(),
            Err(_) => false,
        };
        if !docker_up {
            println!("Docker not available, skipping");
            return;
        }
        let container = GenericImage::new("mariadb", "11.4")
            .with_exposed_port(ContainerPort::Tcp(3306))
            // Data in memory: fast, and independent of the Docker disk.
            .with_mount(Mount::tmpfs_mount("/var/lib/mysql"))
            .with_env_var("MARIADB_ROOT_PASSWORD", "rootpassword")
            .start()
            .await
            .expect("the MariaDB test container starts");
        let port = container.get_host_port_ipv4(3306).await.expect("port");
        let mut root = None;
        for _ in 0..60 {
            if let Ok(pool) = pool_as(port, "root", "rootpassword").await {
                root = Some(pool);
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        let root = root.expect("root connects");

        // A live environment slugged `sandbox`, under the old sandbox name.
        sqlx::raw_sql(
            "CREATE DATABASE shop_sandbox; CREATE TABLE shop_sandbox.orders (id int); \
             INSERT INTO shop_sandbox.orders VALUES (1)",
        )
        .execute(&root)
        .await
        .expect("environment database");

        let reserved =
            MariaDbSandboxPlan::new(4, &SandboxNamespace::project("shop"), "rootpassword")
                .expect("plan");
        let legacy = MariaDbSandboxPlan::legacy(4, "shop", "rootpassword").expect("plan");
        let legacy_state = inspect(&root, 4, &legacy).await.expect("inspect legacy");
        assert!(matches!(legacy_state, ExistingResource::Foreign { .. }));
        let reserved_state = inspect(&root, 4, &reserved).await.expect("inspect");
        assert_eq!(reserved_state, ExistingResource::Missing);
        assert_eq!(
            choose_sandbox_resource(&legacy_state, &reserved_state),
            SandboxResourceChoice::Create
        );
        for statement in reserved.statements(true) {
            sqlx::raw_sql(&statement)
                .execute(&root)
                .await
                .unwrap_or_else(|error| panic!("{statement}: {error}"));
        }
        assert_eq!(
            inspect(&root, 4, &reserved).await.expect("inspect"),
            ExistingResource::Sandbox
        );
        // The deployment-side check, as the `mariadb` client would run it.
        let count: i64 =
            sqlx::query_scalar(&sandbox_user_count_query(&format!("'{}'", reserved.user)))
                .fetch_one(&root)
                .await
                .expect("count");
        assert_eq!(count, 1);

        let sandbox = pool_as(port, &reserved.user, &reserved.password)
            .await
            .expect("the sandbox user connects");
        sqlx::raw_sql("CREATE TABLE `_sandbox_shop`.notes (id int)")
            .execute(&sandbox)
            .await
            .expect("the sandbox uses its own database");
        assert!(
            sqlx::raw_sql("SELECT * FROM shop_sandbox.orders")
                .execute(&sandbox)
                .await
                .is_err(),
            "the sandbox must not read the environment's database"
        );

        // Something made by hand under the reserved name is not the sandbox's.
        sqlx::raw_sql("CREATE DATABASE `_sandbox_other`")
            .execute(&root)
            .await
            .expect("hand-made database");
        let other = MariaDbSandboxPlan::new(4, &SandboxNamespace::project("other"), "rootpassword")
            .expect("plan");
        assert!(matches!(
            inspect(&root, 4, &other).await.expect("inspect"),
            ExistingResource::Foreign { .. }
        ));
    }
}
