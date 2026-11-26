// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a PostgreSQL service holds after a restore, and which database each
//! linked project reads from it.
//!
//! A linked project reads the database named for its project and environment
//! on this server (or the custom name set on the link), and Temps creates it
//! when it is missing. A backup from another server carries that server's
//! databases, so when the names differ the app gets a new, empty database and
//! the restored data sits unused. The restore records what came back
//! ([`ExternalServiceManager::read_postgres_inventory`]) so the service page
//! can compare it with [`ExternalServiceManager::linked_databases`] and point
//! a link at the restored database
//! ([`ExternalServiceManager::set_link_database`]).

use crate::services::*;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use temps_entities::{environments, project_services, projects};

/// The most databases, and separately the most login roles, an inventory
/// keeps. A larger list is cut and marked `truncated`.
pub const MAX_INVENTORY_ENTRIES: usize = 1000;

/// How long reading the inventory may take, per attempt.
const INVENTORY_TIMEOUT: Duration = Duration::from_secs(30);

/// Longest psql error kept in an [`InventoryError`].
const MAX_ERROR_OUTPUT: usize = 400;

const DATABASE_MARKER: &str = "temps-database ";
const SUPERUSER_MARKER: &str = "temps-role super ";
const USER_MARKER: &str = "temps-role user ";

/// Every non-template database, then every role that can log in (without
/// PostgreSQL's own `pg_*` roles), one marked line each. Each half asks for
/// one more row than is kept, so a cut list is detected.
const INVENTORY_SQL: &str = "(SELECT 'temps-database ' || datname FROM pg_catalog.pg_database \
     WHERE NOT datistemplate ORDER BY datname LIMIT 1001) \
     UNION ALL \
     (SELECT 'temps-role ' || CASE WHEN rolsuper THEN 'super ' ELSE 'user ' END || rolname \
     FROM pg_catalog.pg_roles WHERE rolcanlogin AND rolname !~ '^pg_' \
     ORDER BY rolname LIMIT 1001)";

/// The databases and login roles a PostgreSQL service holds. Names only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostgresInventory {
    /// Non-template databases, sorted by name.
    pub databases: Vec<String>,
    /// Roles that can log in, sorted by name.
    pub roles: Vec<InventoryRole>,
    /// A list held more than [`MAX_INVENTORY_ENTRIES`] names and was cut.
    #[serde(default)]
    pub truncated: bool,
}

/// One role that can log in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryRole {
    pub name: String,
    pub superuser: bool,
}

/// Why the inventory of a service could not be read.
#[derive(Debug, thiserror::Error)]
pub enum InventoryError {
    #[error(transparent)]
    Service(#[from] ExternalServiceError),
    #[error(
        "Service {service_id} is a '{service_type}' service; only PostgreSQL services list \
         their databases and users"
    )]
    NotPostgres {
        service_id: i32,
        service_type: String,
    },
    #[error(
        "Service {service_id} is a PostgreSQL cluster; its databases and users are not listed \
         after a restore"
    )]
    Cluster { service_id: i32 },
    #[error("Service {service_id} has no configured user to list its databases with")]
    NoConfiguredUser { service_id: i32 },
    #[error(
        "Could not list the databases of service {service_id} in container '{container}': \
         {reason}"
    )]
    Exec {
        service_id: i32,
        container: String,
        reason: String,
    },
    #[error(
        "Listing the databases of service {service_id} in container '{container}' did not \
         finish within {seconds}s"
    )]
    TimedOut {
        service_id: i32,
        container: String,
        seconds: u64,
    },
}

/// The environment a link's database belongs to, when the link gives each
/// environment its own database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedEnvironment {
    pub id: i32,
    pub name: String,
    pub slug: String,
}

/// The database one linked project (or one of its environments) reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedDatabase {
    pub project_id: i32,
    pub project_name: String,
    pub project_slug: String,
    /// `None` when every environment of the project shares the database
    /// (`project` and `custom` modes).
    pub environment: Option<LinkedEnvironment>,
    pub mode: DatabaseProvisioningMode,
    /// The database name the apps receive.
    pub database: String,
}

/// Which database every linked project reads from one service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceLinkedDatabases {
    pub service_id: i32,
    /// The database in the service's own configuration.
    pub configured_database: Option<String>,
    /// Ordered by project, then environment.
    pub links: Vec<LinkedDatabase>,
}

/// The psql command that prints the inventory, over the local socket or, as
/// a fallback, local TCP.
fn inventory_command(user: &str, over_tcp: bool) -> Vec<String> {
    let mut command: Vec<String> = ["psql", "-X", "-A", "-t", "-q", "-w"]
        .iter()
        .map(|arg| arg.to_string())
        .collect();
    if over_tcp {
        command.extend(["-h".to_string(), "127.0.0.1".to_string()]);
    }
    command.extend([
        "-v".to_string(),
        "ON_ERROR_STOP=1".to_string(),
        "-U".to_string(),
        user.to_string(),
        "-d".to_string(),
        "template1".to_string(),
        "-c".to_string(),
        INVENTORY_SQL.to_string(),
    ]);
    command
}

/// Read the marked lines psql printed. Unmarked lines (notices) are skipped.
pub(crate) fn parse_inventory(output: &str) -> PostgresInventory {
    let mut inventory = PostgresInventory::default();
    for line in output.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(name) = line.strip_prefix(DATABASE_MARKER) {
            if !name.is_empty() {
                inventory.databases.push(name.to_string());
            }
        } else if let Some(name) = line.strip_prefix(SUPERUSER_MARKER) {
            if !name.is_empty() {
                inventory.roles.push(InventoryRole {
                    name: name.to_string(),
                    superuser: true,
                });
            }
        } else if let Some(name) = line.strip_prefix(USER_MARKER) {
            if !name.is_empty() {
                inventory.roles.push(InventoryRole {
                    name: name.to_string(),
                    superuser: false,
                });
            }
        }
    }
    inventory.databases.sort();
    inventory.databases.dedup();
    inventory.roles.sort_by(|a, b| a.name.cmp(&b.name));
    inventory.roles.dedup_by(|a, b| a.name == b.name);
    if inventory.databases.len() > MAX_INVENTORY_ENTRIES {
        inventory.databases.truncate(MAX_INVENTORY_ENTRIES);
        inventory.truncated = true;
    }
    if inventory.roles.len() > MAX_INVENTORY_ENTRIES {
        inventory.roles.truncate(MAX_INVENTORY_ENTRIES);
        inventory.truncated = true;
    }
    inventory
}

fn short(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.len() <= MAX_ERROR_OUTPUT {
        return trimmed.to_string();
    }
    let mut end = MAX_ERROR_OUTPUT;
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &trimmed[..end])
}

/// Every database a link gives its project, in the order the apps see them.
pub(crate) fn link_databases(
    link: &project_services::Model,
    project: &projects::Model,
    environments: &[environments::Model],
) -> Result<Vec<LinkedDatabase>, ExternalServiceError> {
    let provisioning = DatabaseProvisioningConfig::from_link(link)?;
    let database_for = |environment_slug: &str| {
        provisioning
            .runtime_scope(
                &project.slug,
                environment_slug,
                link.service_id,
                link.project_id,
            )
            .map(|(project_scope, environment_scope)| {
                crate::externalsvc::postgres::PostgresService::normalize_database_name(
                    &crate::externalsvc::scoped_resource_name(&project_scope, &environment_scope),
                )
            })
    };
    if provisioning.mode != DatabaseProvisioningMode::ProjectEnvironment {
        return Ok(vec![LinkedDatabase {
            project_id: project.id,
            project_name: project.name.clone(),
            project_slug: project.slug.clone(),
            environment: None,
            mode: provisioning.mode,
            database: database_for("")?,
        }]);
    }
    environments
        .iter()
        .filter(|environment| environment.project_id == project.id)
        .map(|environment| {
            Ok(LinkedDatabase {
                project_id: project.id,
                project_name: project.name.clone(),
                project_slug: project.slug.clone(),
                environment: Some(LinkedEnvironment {
                    id: environment.id,
                    name: environment.name.clone(),
                    slug: environment.slug.clone(),
                }),
                mode: provisioning.mode,
                database: database_for(&environment.slug)?,
            })
        })
        .collect()
}

impl ExternalServiceManager {
    /// List the databases and login roles inside PostgreSQL service
    /// `service_id`, on whichever node runs it. Runs psql inside the
    /// service's container as its configured user: over the local socket
    /// first, then over local TCP with the stored password. Read-only.
    pub async fn read_postgres_inventory(
        &self,
        service_id: i32,
    ) -> Result<PostgresInventory, InventoryError> {
        let service = self.get_service(service_id).await?;
        if service.service_type != "postgres" {
            return Err(InventoryError::NotPostgres {
                service_id,
                service_type: service.service_type,
            });
        }
        if service.topology == "cluster" {
            return Err(InventoryError::Cluster { service_id });
        }
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            }
        })?;
        let parameters = self.parameters_of(&service)?;
        let text = |key: &str| {
            parameters
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .filter(|value| !value.is_empty())
        };
        let user = text("username").ok_or(InventoryError::NoConfiguredUser { service_id })?;
        let password = text("password").unwrap_or_default();
        let container = self
            .service_metadata(service_id, &service.name, service_type, &parameters)?
            .container_name();

        self.inventory_in_container(service_id, service.node_id, &container, &user, &password)
            .await
    }

    /// Run the inventory query in `container` (on worker `node_id`, or on
    /// this host) as `user`.
    async fn inventory_in_container(
        &self,
        service_id: i32,
        node_id: Option<i32>,
        container: &str,
        user: &str,
        password: &str,
    ) -> Result<PostgresInventory, InventoryError> {
        let mut last_failure = String::new();
        for over_tcp in [false, true] {
            let command = inventory_command(user, over_tcp);
            let run = self.exec_for_inventory(node_id, container, command, password, service_id);
            let (exit_code, stdout, stderr) =
                match tokio::time::timeout(INVENTORY_TIMEOUT, run).await {
                    Ok(result) => result?,
                    Err(_) => {
                        return Err(InventoryError::TimedOut {
                            service_id,
                            container: container.to_string(),
                            seconds: INVENTORY_TIMEOUT.as_secs(),
                        })
                    }
                };
            if exit_code == 0 {
                return Ok(parse_inventory(&stdout));
            }
            last_failure = format!(
                "psql exited with {exit_code} ({}): {}",
                if over_tcp {
                    "local TCP"
                } else {
                    "local socket"
                },
                short(&stderr)
            );
        }
        Err(InventoryError::Exec {
            service_id,
            container: container.to_string(),
            reason: last_failure,
        })
    }

    async fn exec_for_inventory(
        &self,
        node_id: Option<i32>,
        container: &str,
        command: Vec<String>,
        password: &str,
        service_id: i32,
    ) -> Result<(i64, String, String), InventoryError> {
        let exec_failed = |reason: String| InventoryError::Exec {
            service_id,
            container: container.to_string(),
            reason,
        };
        match node_id {
            Some(node_id) => {
                let client = self.get_remote_client(node_id).await?;
                let mut environment = HashMap::new();
                if !password.is_empty() {
                    environment.insert("PGPASSWORD".to_string(), password.to_string());
                }
                // The agent's exec body is sealed, so the password travels
                // encrypted to the node.
                let result = client
                    .exec_in_service(crate::remote_service_client::RemoteExecParams {
                        container_name: container.to_string(),
                        command,
                        environment,
                        user: None,
                        detach: false,
                    })
                    .await
                    .map_err(|error| exec_failed(format!("on node {node_id}: {error}")))?;
                Ok((result.exit_code, result.stdout, result.stderr))
            }
            None => {
                let env = format!("PGPASSWORD={password}");
                let env = (!password.is_empty()).then(|| vec![env.as_str()]);
                self.exec_in_local_container_with_env(container, &command, None, env)
                    .await
                    .map_err(|error| exec_failed(error.to_string()))
            }
        }
    }

    /// The database each project linked to `service_id` reads: one per
    /// environment for a `project_environment` link, one for the whole
    /// project otherwise. Soft-deleted projects and environments are left
    /// out. Three queries whatever the number of links.
    pub async fn linked_databases(
        &self,
        service_id: i32,
    ) -> Result<ServiceLinkedDatabases, ExternalServiceError> {
        let service = self.get_service(service_id).await?;
        let parameters = self.parameters_of(&service)?;
        let configured_database = parameters
            .get("database")
            .and_then(serde_json::Value::as_str)
            .filter(|name| !name.is_empty())
            .map(str::to_string);

        let linked = project_services::Entity::find()
            .filter(project_services::Column::ServiceId.eq(service.id))
            .find_also_related(projects::Entity)
            .order_by_asc(project_services::Column::ProjectId)
            .all(self.db.as_ref())
            .await?;
        let project_ids: Vec<i32> = linked.iter().map(|(link, _)| link.project_id).collect();
        let environments = if project_ids.is_empty() {
            Vec::new()
        } else {
            environments::Entity::find()
                .filter(environments::Column::ProjectId.is_in(project_ids))
                .filter(environments::Column::DeletedAt.is_null())
                .order_by_asc(environments::Column::Id)
                .all(self.db.as_ref())
                .await?
        };

        let mut links = Vec::new();
        for (link, project) in &linked {
            let Some(project) = project.as_ref().filter(|p| p.deleted_at.is_none()) else {
                continue;
            };
            links.extend(link_databases(link, project, &environments)?);
        }
        Ok(ServiceLinkedDatabases {
            service_id,
            configured_database,
            links,
        })
    }

    /// Change which database the link between `service_id` and `project_id`
    /// gives the project's apps. Running apps keep the database they were
    /// deployed with until their next deployment.
    pub async fn set_link_database(
        &self,
        service_id: i32,
        project_id: i32,
        provisioning: DatabaseProvisioningConfig,
    ) -> Result<ProjectServiceInfo, ExternalServiceError> {
        let service_info = self.get_service_info(service_id).await?;
        provisioning.validate(
            service_id,
            project_id,
            &service_info.service_type.to_string(),
        )?;

        let (link, project) = project_services::Entity::find()
            .filter(project_services::Column::ServiceId.eq(service_id))
            .filter(project_services::Column::ProjectId.eq(project_id))
            .find_also_related(projects::Entity)
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::ServiceNotLinkedToProject {
                service_id,
                project_id,
            })?;
        let project = project.ok_or(ExternalServiceError::ProjectNotFound { id: project_id })?;

        let mut active: project_services::ActiveModel = link.into();
        active.database_provisioning_mode = Set(provisioning.mode.as_str().to_string());
        active.custom_database_name = Set(provisioning.custom_database_name.clone());
        let saved = active.update(self.db.as_ref()).await?;

        Ok(ProjectServiceInfo {
            id: saved.id,
            project: ProjectInfo {
                id: project.id,
                slug: project.slug,
                created_at: project.created_at.to_rfc3339(),
            },
            service: service_info,
            database_provisioning_mode: DatabaseProvisioningMode::from_persisted(
                &saved.database_provisioning_mode,
                saved.service_id,
                saved.project_id,
            )?,
            custom_database_name: saved.custom_database_name,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn link(mode: &str, custom: Option<&str>) -> project_services::Model {
        project_services::Model {
            id: 1,
            project_id: 10,
            service_id: 5,
            database_provisioning_mode: mode.to_string(),
            custom_database_name: custom.map(str::to_string),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn project() -> projects::Model {
        let mut project = crate::services::test_support::environment_preview_project(10);
        project.name = "Shop".to_string();
        project.slug = "shop".to_string();
        project
    }

    fn environment(id: i32, project_id: i32, slug: &str) -> environments::Model {
        crate::services::test_support::environment_model(id, project_id, slug)
    }

    #[test]
    fn inventory_reads_marked_lines_sorted_and_skips_notices() {
        let output = "NOTICE: something\n\
                      temps-database shop_prod\n\
                      temps-database postgres\n\
                      temps-role user reader\n\
                      temps-role super origin-admin\r\n\
                      temps-database \n\
                      unmarked line\n";
        let inventory = parse_inventory(output);
        assert_eq!(inventory.databases, vec!["postgres", "shop_prod"]);
        assert_eq!(
            inventory.roles,
            vec![
                InventoryRole {
                    name: "origin-admin".into(),
                    superuser: true
                },
                InventoryRole {
                    name: "reader".into(),
                    superuser: false
                },
            ]
        );
        assert!(!inventory.truncated);
    }

    #[test]
    fn inventory_cuts_long_lists_and_says_so() {
        let output: String = (0..=MAX_INVENTORY_ENTRIES)
            .map(|i| format!("temps-database db_{i:05}\n"))
            .collect();
        let inventory = parse_inventory(&output);
        assert_eq!(inventory.databases.len(), MAX_INVENTORY_ENTRIES);
        assert!(inventory.truncated);
    }

    #[test]
    fn inventory_command_is_read_only_and_never_prompts() {
        let socket = inventory_command("app", false);
        assert!(socket.contains(&"-w".to_string()), "never prompts");
        assert!(!socket.contains(&"-h".to_string()), "socket first");
        let tcp = inventory_command("app", true);
        assert!(tcp.windows(2).any(|w| w == ["-h", "127.0.0.1"]));
        let sql = tcp.last().expect("sql");
        assert!(sql.starts_with("(SELECT"), "{sql}");
        for word in ["INSERT", "UPDATE", "DELETE", "ALTER", "CREATE", "DROP"] {
            assert!(!sql.to_uppercase().contains(word), "{word} in {sql}");
        }
    }

    #[test]
    fn a_per_environment_link_gives_each_environment_its_database() {
        let environments = vec![
            environment(1, 10, "production"),
            environment(2, 10, "staging"),
            environment(3, 99, "other-project"),
        ];
        let databases = link_databases(
            &link("project_environment", None),
            &project(),
            &environments,
        )
        .expect("databases");
        let names: Vec<_> = databases.iter().map(|d| d.database.as_str()).collect();
        assert_eq!(names, vec!["shop_production", "shop_staging"]);
        assert_eq!(
            databases[1].environment.as_ref().map(|e| e.id),
            Some(2),
            "{databases:?}"
        );
    }

    #[test]
    fn project_and_custom_links_give_one_database_for_the_project() {
        let environments = vec![environment(1, 10, "production")];
        let project_wide =
            link_databases(&link("project", None), &project(), &environments).expect("project");
        assert_eq!(project_wide.len(), 1);
        assert_eq!(project_wide[0].database, "shop");
        assert_eq!(project_wide[0].environment, None);

        let custom = link_databases(
            &link("custom", Some("shop_prod")),
            &project(),
            &environments,
        )
        .expect("custom");
        assert_eq!(custom[0].database, "shop_prod");
        assert_eq!(custom[0].mode, DatabaseProvisioningMode::Custom);
    }

    #[test]
    fn hyphenated_slugs_become_the_names_apps_receive() {
        let mut project = project();
        project.slug = "my-shop".to_string();
        let databases = link_databases(
            &link("project_environment", None),
            &project,
            &[environment(1, 10, "pre-prod")],
        )
        .expect("databases");
        assert_eq!(databases[0].database, "my_shop_pre_prod");
    }

    #[test]
    fn an_unknown_persisted_mode_is_an_error_naming_the_link() {
        let error =
            link_databases(&link("sideways", None), &project(), &[]).expect_err("unknown mode");
        let message = error.to_string();
        assert!(message.contains("sideways"), "{message}");
    }

    /// The real query, run in a PostgreSQL container through the manager's
    /// local exec: databases and login roles come back, templates and
    /// PostgreSQL's own roles do not.
    #[tokio::test]
    async fn docker_lists_the_databases_and_login_roles_of_a_running_server() {
        use bollard::models::ContainerCreateBody;
        use bollard::query_parameters::{
            CreateContainerOptionsBuilder, RemoveContainerOptionsBuilder, StartContainerOptions,
        };
        use bollard::Docker;

        let Ok(docker) = Docker::connect_with_local_defaults() else {
            println!("Docker not available, skipping");
            return;
        };
        if docker.ping().await.is_err() {
            println!("Docker daemon not responding, skipping");
            return;
        }
        let image = "postgres:18-alpine";
        if docker.inspect_image(image).await.is_err() {
            println!("{image} is not available locally, skipping");
            return;
        }
        let container = format!("temps_inventory_test_{}", rand::random::<u32>());
        docker
            .create_container(
                Some(
                    CreateContainerOptionsBuilder::new()
                        .name(&container)
                        .build(),
                ),
                ContainerCreateBody {
                    image: Some(image.to_string()),
                    env: Some(vec![
                        "POSTGRES_USER=origin-admin".to_string(),
                        "POSTGRES_PASSWORD=origin-secret".to_string(),
                        "POSTGRES_DB=shop_prod".to_string(),
                    ]),
                    ..Default::default()
                },
            )
            .await
            .expect("create the test container");

        let manager = crate::services::test_support::mock_service_manager(vec![]);
        let outcome = async {
            docker
                .start_container(&container, None::<StartContainerOptions>)
                .await
                .map_err(|error| format!("start: {error}"))?;
            let sql = |statement: &str| {
                vec![
                    "psql".to_string(),
                    "-X".to_string(),
                    "-qAt".to_string(),
                    "-h".to_string(),
                    "127.0.0.1".to_string(),
                    "-U".to_string(),
                    "origin-admin".to_string(),
                    "-d".to_string(),
                    "shop_prod".to_string(),
                    "-c".to_string(),
                    statement.to_string(),
                ]
            };
            // Ready once the final server (not the socket-only init one)
            // accepts TCP connections.
            let mut ready = false;
            for _ in 0..90 {
                let probe = manager
                    .exec_in_local_container(&container, &sql("SELECT 1"), None)
                    .await;
                if matches!(probe, Ok((0, ref out, _)) if out.trim() == "1") {
                    ready = true;
                    break;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            if !ready {
                return Err("the test server never accepted connections".to_string());
            }
            for statement in [
                "CREATE ROLE reader LOGIN PASSWORD 'reader-secret'",
                "CREATE ROLE no_login NOLOGIN",
                "CREATE DATABASE \"blog\"",
            ] {
                let (code, _, err) = manager
                    .exec_in_local_container(&container, &sql(statement), None)
                    .await
                    .map_err(|error| error.to_string())?;
                if code != 0 {
                    return Err(format!("{statement}: {err}"));
                }
            }
            manager
                .inventory_in_container(1, None, &container, "origin-admin", "origin-secret")
                .await
                .map_err(|error| error.to_string())
        }
        .await;

        let _ = docker
            .remove_container(
                &container,
                Some(RemoveContainerOptionsBuilder::new().force(true).build()),
            )
            .await;

        let inventory = outcome.expect("inventory");
        assert_eq!(inventory.databases, vec!["blog", "postgres", "shop_prod"]);
        let roles: Vec<_> = inventory
            .roles
            .iter()
            .map(|role| (role.name.as_str(), role.superuser))
            .collect();
        assert_eq!(roles, vec![("origin-admin", true), ("reader", false)]);
        assert!(!inventory.truncated);
    }
}
