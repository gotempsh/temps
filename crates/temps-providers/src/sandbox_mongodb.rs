// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Credentials a sandbox can use against a linked MongoDB service (ADR-051).
//!
//! The sandbox gets a database of its own (`_sandbox_<project>`, or
//! `_sandboxid_<sandbox>` for a sandbox linked directly; a namespace no
//! deployment can use; see [`crate::sandbox_engines::SANDBOX_DATABASE_PREFIX`])
//! and a user, defined in that database, with `readWrite` on it and no other
//! role. The password is derived from the administrator's so nothing new is
//! stored. The variables use the names a deployment gets (`MONGODB_*`).
//!
//! The sandbox user is the database's provenance: only the administrator can
//! create or drop it (`readWrite` carries no user administration). An existing
//! database without that user was not created for a sandbox and is never
//! granted to one.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use mongodb::bson::{doc, Document};
use mongodb::error::{CommandError, ErrorKind};
use mongodb::options::ClientOptions;
use mongodb::Client;
use temps_entities::external_services;
use tracing::info;

pub use crate::externalsvc::mongodb::{users_info, users_info_has_user};

use crate::externalsvc::mongodb::mongodb_container_name;
use crate::sandbox_access::sandbox_role_name;
use crate::sandbox_engines::{
    choose_sandbox_resource, derive_password, is_plain_identifier, require_local, text_parameter,
    ExistingResource, SandboxAccessEngine, SandboxAccessRequest, SandboxNamespace,
    SandboxNamespaceConflict, SandboxResourceChoice,
};
use crate::services::{ExternalServiceError, ExternalServiceManager};

const MONGODB_INTERNAL_PORT: &str = "27017";
const DATABASE_MAX: usize = 63;
const PASSWORD_TAG: &str = "temps-scoped-mongodb-v1";
const OPERATION_TIMEOUT: Duration = Duration::from_secs(10);
/// `createUser` answers this when the user exists already.
const USER_ALREADY_EXISTS: i32 = 51003;
const ENGINE: &str = "MongoDB";

/// Gives each sandbox its own database and a user limited to it.
pub struct MongoDbSandboxAccess;

/// The database and user that give one sandbox its access.
#[derive(Clone, PartialEq, Eq)]
pub struct MongoDbSandboxPlan {
    pub database: String,
    pub user: String,
    password: String,
}

impl std::fmt::Debug for MongoDbSandboxPlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MongoDbSandboxPlan")
            .field("database", &self.database)
            .field("user", &self.user)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// The sandbox database's name before sandboxes had a namespace of their own.
fn legacy_database_name(namespace: &str) -> String {
    let mut name: String = format!("{namespace}_sandbox")
        .to_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect();
    if name.starts_with(|character: char| character.is_ascii_digit()) {
        name.insert_str(0, "db_");
    }
    name.truncate(DATABASE_MAX);
    name
}

/// What the database is to the sandbox user that goes with it: the user is
/// the provenance (a MongoDB database exists implicitly once it holds data, so
/// a sandbox database with no data yet does not show up at all).
pub fn classify_database(database_exists: bool, user_exists: bool, user: &str) -> ExistingResource {
    match (database_exists, user_exists) {
        (_, true) => ExistingResource::Sandbox,
        (false, false) => ExistingResource::Missing,
        (true, false) => ExistingResource::Foreign {
            evidence: format!("it holds data and no sandbox user '{user}' is defined in it"),
        },
    }
}

impl MongoDbSandboxPlan {
    /// The plan for the sandbox namespace: see
    /// [`SandboxNamespace::database_name`].
    pub fn new(
        service_id: i32,
        namespace: &SandboxNamespace,
        admin_password: &str,
    ) -> Result<Self, String> {
        Self::for_database(service_id, namespace.database_name(), admin_password)
    }

    /// The plan for a project's pre-namespace name `<project>_sandbox`, used only
    /// when its sandbox user exists.
    pub fn legacy(
        service_id: i32,
        project_slug: &str,
        admin_password: &str,
    ) -> Result<Self, String> {
        Self::for_database(
            service_id,
            legacy_database_name(project_slug),
            admin_password,
        )
    }

    fn for_database(
        service_id: i32,
        database: String,
        admin_password: &str,
    ) -> Result<Self, String> {
        let user = sandbox_role_name(&database);
        for (kind, value) in [("database", &database), ("user", &user)] {
            if !is_plain_identifier(value, DATABASE_MAX) {
                return Err(format!(
                    "the {kind} name '{value}' is not a plain lowercase identifier"
                ));
            }
        }
        if admin_password.is_empty() {
            return Err("the service has no administrator password".to_string());
        }
        Ok(Self {
            password: derive_password(admin_password, PASSWORD_TAG, service_id, &user)?,
            database,
            user,
        })
    }

    /// `createUser` in the sandbox's own database: `readWrite` there and
    /// nothing else, so the user can neither see another database nor manage
    /// users.
    pub fn create_user(&self) -> Document {
        doc! {
            "createUser": &self.user,
            "pwd": &self.password,
            "roles": [{ "role": "readWrite", "db": &self.database }],
        }
    }

    /// The same, for a user that exists already: sets the password and roles
    /// back, repairing a user changed by hand.
    pub fn update_user(&self) -> Document {
        doc! {
            "updateUser": &self.user,
            "pwd": &self.password,
            "roles": [{ "role": "readWrite", "db": &self.database }],
        }
    }

    pub fn environment(&self, host: &str, direct_connection: bool) -> HashMap<String, String> {
        let mut url = format!(
            "mongodb://{}:{}@{}:{}/{}?authSource={}",
            urlencoding::encode(&self.user),
            urlencoding::encode(&self.password),
            host,
            MONGODB_INTERNAL_PORT,
            self.database,
            self.database
        );
        if direct_connection {
            url.push_str("&directConnection=true");
        }
        HashMap::from([
            ("MONGODB_URL".to_string(), url),
            ("MONGODB_HOST".to_string(), host.to_string()),
            (
                "MONGODB_PORT".to_string(),
                MONGODB_INTERNAL_PORT.to_string(),
            ),
            ("MONGODB_DATABASE".to_string(), self.database.clone()),
            ("MONGODB_USERNAME".to_string(), self.user.clone()),
            ("MONGODB_PASSWORD".to_string(), self.password.clone()),
        ])
    }
}

fn failure(service_id: i32, what: &str, reason: impl std::fmt::Display) -> ExternalServiceError {
    ExternalServiceError::InternalError {
        reason: format!("Failed to {what} on MongoDB service {service_id}: {reason}"),
    }
}

#[async_trait]
impl SandboxAccessEngine for MongoDbSandboxAccess {
    fn service_types(&self) -> &'static [&'static str] {
        &["mongodb"]
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
            .unwrap_or_else(|| mongodb_container_name(&service.name))])
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
        let admin_user = text_parameter(parameters, "username").unwrap_or_else(|| "root".into());
        let admin_password = text_parameter(parameters, "password").unwrap_or_default();
        let unsupported = |reason: String| ExternalServiceError::SandboxAccessUnsupported {
            service_id: service.id,
            service_type: service.service_type.clone(),
            reason,
        };
        let reserved =
            MongoDbSandboxPlan::new(service.id, namespace, &admin_password).map_err(unsupported)?;
        // Only a project's sandboxes had a legacy name, and only one naming a
        // single project; one that is not a plain identifier was never
        // provisioned either.
        let legacy = namespace
            .legacy_project_slug()
            .and_then(|slug| MongoDbSandboxPlan::legacy(service.id, slug, &admin_password).ok());
        let container = text_parameter(parameters, "container_name")
            .unwrap_or_else(|| mongodb_container_name(&service.name));
        let host = text_parameter(parameters, "host").unwrap_or_else(|| "localhost".to_string());
        let port =
            text_parameter(parameters, "port").unwrap_or_else(|| MONGODB_INTERNAL_PORT.to_string());
        let replica_set = text_parameter(parameters, "replica_set").is_some();

        // `directConnection` keeps the driver on exactly this host:port: a
        // replica-set member registers itself under an address that only
        // resolves inside its own container.
        let connection_string = format!(
            "mongodb://{}:{}@{}:{}/?authSource=admin&directConnection=true",
            urlencoding::encode(&admin_user),
            urlencoding::encode(&admin_password),
            host,
            port
        );
        let mut options = ClientOptions::parse(&connection_string)
            .await
            .map_err(|error| failure(service.id, "parse the connection string", error))?;
        options.server_selection_timeout = Some(OPERATION_TIMEOUT);
        options.connect_timeout = Some(OPERATION_TIMEOUT);
        let client = Client::with_options(options)
            .map_err(|error| failure(service.id, "open a client", error))?;

        // Read what exists before changing anything.
        let reserved_state = inspect(&client, service.id, &reserved).await?;
        let legacy_state = match &legacy {
            Some(plan) => inspect(&client, service.id, plan).await?,
            None => ExistingResource::Missing,
        };
        let plan = match (
            choose_sandbox_resource(&legacy_state, &reserved_state),
            &legacy,
        ) {
            (SandboxResourceChoice::AdoptLegacy, Some(plan)) => plan,
            // `legacy_state` is only `Sandbox` when there is a legacy plan.
            (SandboxResourceChoice::Adopt, _)
            | (SandboxResourceChoice::AdoptLegacy, None)
            | (SandboxResourceChoice::Create, _) => &reserved,
            (SandboxResourceChoice::Refuse { evidence }, _) => {
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
        let database = client.database(&plan.database);

        let created =
            tokio::time::timeout(OPERATION_TIMEOUT, database.run_command(plan.create_user()))
                .await
                .map_err(|_| {
                    failure(
                        service.id,
                        &format!("create the user '{}'", plan.user),
                        format!("timed out after {}s", OPERATION_TIMEOUT.as_secs()),
                    )
                })?;
        match created {
            Ok(_) => {}
            Err(error) if is_user_exists(&error) => {
                tokio::time::timeout(OPERATION_TIMEOUT, database.run_command(plan.update_user()))
                    .await
                    .map_err(|_| {
                        failure(
                            service.id,
                            &format!("update the user '{}'", plan.user),
                            format!("timed out after {}s", OPERATION_TIMEOUT.as_secs()),
                        )
                    })?
                    .map_err(|error| {
                        failure(
                            service.id,
                            &format!("update the user '{}'", plan.user),
                            error,
                        )
                    })?;
            }
            Err(error) => {
                return Err(failure(
                    service.id,
                    &format!("create the user '{}'", plan.user),
                    error,
                ))
            }
        }

        info!(
            service_id = service.id,
            project_id = ?project_id,
            database = %plan.database,
            user = %plan.user,
            "Provisioned a database user for a sandbox"
        );
        Ok(plan.environment(&container, replica_set))
    }
}

/// What the database of `plan` is, read without changing anything.
async fn inspect(
    client: &Client,
    service_id: i32,
    plan: &MongoDbSandboxPlan,
) -> Result<ExistingResource, ExternalServiceError> {
    let timed_out = |what: &str| {
        failure(
            service_id,
            what,
            format!("timed out after {}s", OPERATION_TIMEOUT.as_secs()),
        )
    };
    let listing = format!("check whether the database '{}' exists", plan.database);
    let names = tokio::time::timeout(
        OPERATION_TIMEOUT,
        client
            .list_database_names()
            .filter(doc! { "name": &plan.database }),
    )
    .await
    .map_err(|_| timed_out(&listing))?
    .map_err(|error| failure(service_id, &listing, error))?;
    let lookup = format!("check whether the user '{}' exists", plan.user);
    let answer = tokio::time::timeout(
        OPERATION_TIMEOUT,
        client
            .database(&plan.database)
            .run_command(users_info(&plan.user, &plan.database)),
    )
    .await
    .map_err(|_| timed_out(&lookup))?
    .map_err(|error| failure(service_id, &lookup, error))?;
    Ok(classify_database(
        names.contains(&plan.database),
        users_info_has_user(&answer),
        &plan.user,
    ))
}

fn is_user_exists(error: &mongodb::error::Error) -> bool {
    matches!(
        error.kind.as_ref(),
        ErrorKind::Command(CommandError { code, .. }) if *code == USER_ALREADY_EXISTS
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> MongoDbSandboxPlan {
        MongoDbSandboxPlan::new(5, &SandboxNamespace::project("shop"), "adminpassword")
            .expect("plan")
    }

    #[test]
    fn the_database_and_user_are_named_for_the_namespace() {
        let plan = plan();
        assert_eq!(plan.database, "_sandbox_shop");
        assert_eq!(plan.user, "sbx__sandbox_shop");
    }

    #[test]
    fn a_namespace_with_odd_characters_still_gives_a_plain_database_name() {
        assert_eq!(legacy_database_name("My-Shop.v2"), "my_shop_v2_sandbox");
        assert_eq!(legacy_database_name("2fast"), "db_2fast_sandbox");
        let long = legacy_database_name(&"a".repeat(100));
        assert!(long.len() <= DATABASE_MAX);
    }

    #[test]
    fn the_user_may_read_and_write_its_own_database_only() {
        let command = plan().create_user();
        assert_eq!(
            command.get_str("createUser").ok(),
            Some("sbx__sandbox_shop")
        );
        let roles = command.get_array("roles").expect("roles");
        assert_eq!(roles.len(), 1, "exactly one role");
        let role = roles[0].as_document().expect("role document");
        assert_eq!(role.get_str("role").ok(), Some("readWrite"));
        assert_eq!(role.get_str("db").ok(), Some("_sandbox_shop"));
    }

    #[test]
    fn a_database_is_the_sandboxs_only_when_its_user_is_defined_in_it() {
        assert_eq!(
            classify_database(false, false, "sbx__sandbox_shop"),
            ExistingResource::Missing
        );
        // A sandbox database with no data yet is not listed, but is the
        // sandbox's.
        assert_eq!(
            classify_database(false, true, "sbx__sandbox_shop"),
            ExistingResource::Sandbox
        );
        assert_eq!(
            classify_database(true, true, "sbx__sandbox_shop"),
            ExistingResource::Sandbox
        );
        assert!(matches!(
            classify_database(true, false, "sbx_shop_sandbox"),
            ExistingResource::Foreign { .. }
        ));
    }

    #[test]
    fn users_info_asks_for_one_user_of_one_database() {
        let command = users_info("sbx_shop_sandbox", "shop_sandbox");
        let target = command.get_document("usersInfo").expect("usersInfo");
        assert_eq!(target.get_str("user").ok(), Some("sbx_shop_sandbox"));
        assert_eq!(target.get_str("db").ok(), Some("shop_sandbox"));
        assert!(!users_info_has_user(&doc! { "users": [], "ok": 1 }));
        assert!(users_info_has_user(
            &doc! { "users": [{ "user": "sbx_shop_sandbox" }], "ok": 1 }
        ));
        assert!(!users_info_has_user(&doc! { "ok": 1 }));
    }

    #[test]
    fn the_legacy_plan_keeps_the_old_names() {
        let legacy = MongoDbSandboxPlan::legacy(5, "shop", "adminpassword").expect("plan");
        assert_eq!(legacy.database, "shop_sandbox");
        assert_eq!(legacy.user, "sbx_shop_sandbox");
        // An environment slugged `sandbox` gets `shop_sandbox` on MongoDB too;
        // the new sandbox name is not that.
        assert_ne!(
            plan().database,
            crate::externalsvc::scoped_resource_name("shop", "sandbox")
        );
    }

    #[test]
    fn updating_a_user_resets_its_password_and_roles() {
        let update = plan().update_user();
        let create = plan().create_user();
        assert_eq!(
            update.get_str("updateUser").ok(),
            create.get_str("createUser").ok()
        );
        assert_eq!(update.get_str("pwd").ok(), create.get_str("pwd").ok());
        assert_eq!(
            update.get_array("roles").ok(),
            create.get_array("roles").ok()
        );
    }

    #[test]
    fn the_password_is_stable_and_follows_the_administrator_password() {
        assert_eq!(plan(), plan());
        let other = MongoDbSandboxPlan::new(5, &SandboxNamespace::project("shop"), "another")
            .expect("plan");
        assert_ne!(plan().password, other.password);
    }

    #[test]
    fn a_service_without_an_administrator_password_is_refused() {
        assert!(MongoDbSandboxPlan::new(5, &SandboxNamespace::project("shop"), "").is_err());
    }

    #[test]
    fn the_environment_keeps_the_deployment_names_and_authenticates_against_its_database() {
        let environment = plan().environment("mongo-shop", false);
        for name in [
            "MONGODB_URL",
            "MONGODB_HOST",
            "MONGODB_PORT",
            "MONGODB_DATABASE",
            "MONGODB_USERNAME",
            "MONGODB_PASSWORD",
        ] {
            assert!(environment.contains_key(name), "{name}");
        }
        let url = &environment["MONGODB_URL"];
        assert!(
            url.contains("/_sandbox_shop?authSource=_sandbox_shop"),
            "{url}"
        );
        assert!(!url.contains("directConnection"));
        assert!(plan().environment("mongo-shop", true)["MONGODB_URL"]
            .ends_with("&directConnection=true"));
        assert!(!environment
            .values()
            .any(|value| value.contains("adminpassword")));
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

    #[tokio::test]
    async fn an_environment_database_is_never_given_to_a_sandbox() {
        let docker_up = match bollard::Docker::connect_with_defaults() {
            Ok(docker) => docker.ping().await.is_ok(),
            Err(_) => false,
        };
        if !docker_up {
            println!("Docker not available, skipping");
            return;
        }
        let container = GenericImage::new("mongo", "7.0")
            .with_exposed_port(ContainerPort::Tcp(27017))
            // Data in memory: fast, and independent of the Docker disk.
            .with_mount(Mount::tmpfs_mount("/data/db"))
            .with_env_var("MONGO_INITDB_ROOT_USERNAME", "root")
            .with_env_var("MONGO_INITDB_ROOT_PASSWORD", "adminpassword")
            .start()
            .await
            .expect("the MongoDB test container starts");
        let port = container.get_host_port_ipv4(27017).await.expect("port");
        let mut options = ClientOptions::parse(format!(
            "mongodb://root:adminpassword@127.0.0.1:{port}/?authSource=admin&directConnection=true"
        ))
        .await
        .expect("options");
        options.server_selection_timeout = Some(Duration::from_secs(5));
        let client = Client::with_options(options).expect("client");
        let mut ready = false;
        for _ in 0..60 {
            if client.list_database_names().await.is_ok() {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        assert!(ready, "MongoDB answers");

        // A live environment slugged `sandbox`, under the old sandbox name.
        client
            .database("shop_sandbox")
            .collection::<Document>("orders")
            .insert_one(doc! { "id": 1 })
            .await
            .expect("environment data");

        let reserved =
            MongoDbSandboxPlan::new(5, &SandboxNamespace::project("shop"), "adminpassword")
                .expect("plan");
        let legacy = MongoDbSandboxPlan::legacy(5, "shop", "adminpassword").expect("plan");
        let legacy_state = inspect(&client, 5, &legacy).await.expect("inspect legacy");
        assert!(matches!(legacy_state, ExistingResource::Foreign { .. }));
        let reserved_state = inspect(&client, 5, &reserved).await.expect("inspect");
        assert_eq!(reserved_state, ExistingResource::Missing);
        assert_eq!(
            choose_sandbox_resource(&legacy_state, &reserved_state),
            SandboxResourceChoice::Create
        );
        client
            .database(&reserved.database)
            .run_command(reserved.create_user())
            .await
            .expect("create the sandbox user");
        // No data yet, but the user makes it the sandbox's.
        assert_eq!(
            inspect(&client, 5, &reserved).await.expect("inspect"),
            ExistingResource::Sandbox
        );
        // The deployment-side check sees no sandbox user in the
        // environment's database.
        let answer = client
            .database("shop_sandbox")
            .run_command(users_info(&legacy.user, "shop_sandbox"))
            .await
            .expect("usersInfo");
        assert!(!users_info_has_user(&answer));
    }
}
