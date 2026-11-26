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
/// keeps. A larger list is cut and marked truncated.
pub const MAX_INVENTORY_ENTRIES: usize = 1000;

/// How long reading the inventory may take, per attempt.
const INVENTORY_TIMEOUT: Duration = Duration::from_secs(30);

/// Longest psql error kept in an [`InventoryError`].
const MAX_ERROR_OUTPUT: usize = 400;

/// Every non-template database and every role that can log in (without
/// PostgreSQL's own `pg_*` roles), as one JSON object, so a name holding a
/// newline or any other character comes back intact. Each list asks for one
/// more row than is kept, so a cut list is detected.
const INVENTORY_SQL: &str = "SELECT jsonb_build_object(\
     'databases', COALESCE((SELECT jsonb_agg(datname ORDER BY datname) FROM \
     (SELECT datname FROM pg_catalog.pg_database WHERE NOT datistemplate \
     ORDER BY datname LIMIT 1001) d), '[]'::jsonb), \
     'roles', COALESCE((SELECT jsonb_agg(jsonb_build_object('name', rolname, \
     'superuser', rolsuper) ORDER BY rolname) FROM \
     (SELECT rolname, rolsuper FROM pg_catalog.pg_roles \
     WHERE rolcanlogin AND rolname !~ '^pg_' ORDER BY rolname LIMIT 1001) r), '[]'::jsonb))";

/// The databases and login roles a PostgreSQL service holds. Names only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostgresInventory {
    /// Non-template databases, sorted by name.
    pub databases: Vec<String>,
    /// Roles that can log in, sorted by name.
    pub roles: Vec<InventoryRole>,
    /// The server held more than [`MAX_INVENTORY_ENTRIES`] databases and
    /// the list was cut.
    #[serde(default)]
    pub databases_truncated: bool,
    /// The server held more than [`MAX_INVENTORY_ENTRIES`] login roles and
    /// the list was cut.
    #[serde(default)]
    pub roles_truncated: bool,
}

impl PostgresInventory {
    /// Either list was cut.
    pub fn truncated(&self) -> bool {
        self.databases_truncated || self.roles_truncated
    }
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

/// The databases the inventory query connects to, in order: `template1`,
/// then the service's configured database, then `postgres`. A physical backup
/// can bring back a `template1` that refuses connections; any database
/// reaches the same cluster-wide catalogs.
fn inventory_databases(configured_database: Option<&str>) -> Vec<&str> {
    let mut databases = vec!["template1"];
    for database in configured_database
        .into_iter()
        .filter(|name| !name.is_empty())
        .chain(["postgres"])
    {
        if !databases.contains(&database) {
            databases.push(database);
        }
    }
    databases
}

/// The psql command that prints the inventory from `database`, over the
/// local socket or, as a fallback, local TCP.
fn inventory_command(user: &str, database: &str, over_tcp: bool) -> Vec<String> {
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
        database.to_string(),
        "-c".to_string(),
        INVENTORY_SQL.to_string(),
    ]);
    command
}

/// What [`INVENTORY_SQL`] prints.
#[derive(Deserialize)]
struct RawInventory {
    databases: Vec<String>,
    roles: Vec<InventoryRole>,
}

/// Read the JSON object psql printed: sorted, without duplicates or empty
/// names, each list cut at [`MAX_INVENTORY_ENTRIES`].
pub(crate) fn parse_inventory(output: &str) -> Result<PostgresInventory, String> {
    let raw: RawInventory = serde_json::from_str(output.trim()).map_err(|error| {
        format!(
            "psql printed an unreadable list ({error}): {}",
            short(output)
        )
    })?;
    let mut databases: Vec<String> = raw
        .databases
        .into_iter()
        .filter(|name| !name.is_empty())
        .collect();
    databases.sort();
    databases.dedup();
    let mut roles: Vec<InventoryRole> = raw
        .roles
        .into_iter()
        .filter(|role| !role.name.is_empty())
        .collect();
    roles.sort_by(|a, b| a.name.cmp(&b.name));
    roles.dedup_by(|a, b| a.name == b.name);
    let databases_truncated = databases.len() > MAX_INVENTORY_ENTRIES;
    databases.truncate(MAX_INVENTORY_ENTRIES);
    let roles_truncated = roles.len() > MAX_INVENTORY_ENTRIES;
    roles.truncate(MAX_INVENTORY_ENTRIES);
    Ok(PostgresInventory {
        databases,
        roles,
        databases_truncated,
        roles_truncated,
    })
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
    /// first, then over local TCP with the stored password, connected to
    /// `template1`, else the configured database, else `postgres`.
    /// Read-only.
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
        let configured_database = text("database");
        let container = self
            .service_metadata(service_id, &service.name, service_type, &parameters)?
            .container_name();

        self.inventory_in_container(
            service_id,
            service.node_id,
            &container,
            &user,
            &password,
            configured_database.as_deref(),
        )
        .await
    }

    /// Run the inventory query in `container` (on worker `node_id`, or on
    /// this host) as `user`, trying each database of
    /// [`inventory_databases`] over the socket and then TCP until one
    /// connection answers.
    async fn inventory_in_container(
        &self,
        service_id: i32,
        node_id: Option<i32>,
        container: &str,
        user: &str,
        password: &str,
        configured_database: Option<&str>,
    ) -> Result<PostgresInventory, InventoryError> {
        let mut failures = Vec::new();
        for database in inventory_databases(configured_database) {
            for over_tcp in [false, true] {
                let command = inventory_command(user, database, over_tcp);
                let run =
                    self.exec_for_inventory(node_id, container, command, password, service_id);
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
                    return parse_inventory(&stdout).map_err(|reason| InventoryError::Exec {
                        service_id,
                        container: container.to_string(),
                        reason,
                    });
                }
                failures.push(format!(
                    "database '{database}' over {} exited with {exit_code}: {}",
                    if over_tcp {
                        "local TCP"
                    } else {
                        "the local socket"
                    },
                    short(&stderr)
                ));
            }
        }
        Err(InventoryError::Exec {
            service_id,
            container: container.to_string(),
            reason: format!("psql could not connect: {}", failures.join("; ")),
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
        let environments: Vec<environments::Model> = if project_ids.is_empty() {
            Vec::new()
        } else {
            environments::Entity::find()
                .filter(environments::Column::ProjectId.is_in(project_ids))
                .filter(environments::Column::DeletedAt.is_null())
                .order_by_asc(environments::Column::Id)
                .all(self.db.as_ref())
                .await?
        };

        let mut by_project: HashMap<i32, Vec<environments::Model>> = HashMap::new();
        for environment in environments {
            by_project
                .entry(environment.project_id)
                .or_default()
                .push(environment);
        }

        let mut links = Vec::new();
        for (link, project) in &linked {
            let Some(project) = project.as_ref().filter(|p| p.deleted_at.is_none()) else {
                continue;
            };
            let project_environments = by_project.get(&project.id).map(Vec::as_slice);
            links.extend(link_databases(
                link,
                project,
                project_environments.unwrap_or(&[]),
            )?);
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
            env_aliases: serde_json::json!({}),
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
    fn inventory_reads_the_json_list_sorted_and_keeps_unusual_names_intact() {
        let output = serde_json::json!({
            "databases": ["shop_prod", "postgres", "line\nbreak", "", "shop_prod"],
            "roles": [
                {"name": "reader", "superuser": false},
                {"name": "origin-admin", "superuser": true},
                {"name": "", "superuser": false},
            ],
        })
        .to_string();
        let inventory = parse_inventory(&format!("{output}\n")).expect("inventory");
        assert_eq!(
            inventory.databases,
            vec!["line\nbreak", "postgres", "shop_prod"]
        );
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
        assert!(!inventory.truncated());
    }

    #[test]
    fn unreadable_psql_output_is_an_error_quoting_it() {
        let error = parse_inventory("WARNING: something else").expect_err("not json");
        assert!(error.contains("WARNING: something else"), "{error}");
    }

    #[test]
    fn each_list_says_on_its_own_when_it_was_cut() {
        let databases: Vec<String> = (0..=MAX_INVENTORY_ENTRIES)
            .map(|i| format!("db_{i:05}"))
            .collect();
        let output = serde_json::json!({ "databases": databases, "roles": [] }).to_string();
        let inventory = parse_inventory(&output).expect("inventory");
        assert_eq!(inventory.databases.len(), MAX_INVENTORY_ENTRIES);
        assert!(inventory.databases_truncated);
        assert!(!inventory.roles_truncated);

        let roles: Vec<_> = (0..=MAX_INVENTORY_ENTRIES)
            .map(|i| serde_json::json!({ "name": format!("role_{i:05}"), "superuser": false }))
            .collect();
        let output = serde_json::json!({ "databases": ["app"], "roles": roles }).to_string();
        let inventory = parse_inventory(&output).expect("inventory");
        assert_eq!(inventory.roles.len(), MAX_INVENTORY_ENTRIES);
        assert!(!inventory.databases_truncated);
        assert!(inventory.roles_truncated);
        assert!(inventory.truncated());
    }

    #[test]
    fn an_inventory_recorded_without_the_cut_flags_reads_as_complete() {
        let inventory: PostgresInventory =
            serde_json::from_value(serde_json::json!({ "databases": ["app"], "roles": [] }))
                .expect("inventory");
        assert!(!inventory.truncated());
    }

    #[test]
    fn inventory_command_is_read_only_and_never_prompts() {
        let socket = inventory_command("app", "template1", false);
        assert!(socket.contains(&"-w".to_string()), "never prompts");
        assert!(!socket.contains(&"-h".to_string()), "socket first");
        assert!(socket.windows(2).any(|w| w == ["-d", "template1"]));
        let tcp = inventory_command("app", "shop", true);
        assert!(tcp.windows(2).any(|w| w == ["-h", "127.0.0.1"]));
        assert!(tcp.windows(2).any(|w| w == ["-d", "shop"]));
        let sql = tcp.last().expect("sql");
        assert!(sql.starts_with("SELECT jsonb_build_object("), "{sql}");
        for word in ["INSERT", "UPDATE", "DELETE", "ALTER", "CREATE", "DROP"] {
            assert!(!sql.to_uppercase().contains(word), "{word} in {sql}");
        }
    }

    #[test]
    fn the_inventory_tries_template1_then_the_configured_database_then_postgres() {
        assert_eq!(
            inventory_databases(Some("shop")),
            vec!["template1", "shop", "postgres"]
        );
        assert_eq!(inventory_databases(None), vec!["template1", "postgres"]);
        assert_eq!(inventory_databases(Some("")), vec!["template1", "postgres"]);
        assert_eq!(
            inventory_databases(Some("postgres")),
            vec!["template1", "postgres"]
        );
        assert_eq!(
            inventory_databases(Some("template1")),
            vec!["template1", "postgres"]
        );
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
    /// PostgreSQL's own roles do not, a name holding a newline comes back
    /// intact, and a `template1` that refuses connections falls back to the
    /// configured database.
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
                "CREATE DATABASE \"line\nbreak\"",
                "ALTER DATABASE template1 ALLOW_CONNECTIONS false",
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
                .inventory_in_container(
                    1,
                    None,
                    &container,
                    "origin-admin",
                    "origin-secret",
                    Some("shop_prod"),
                )
                .await
                .map_err(|error| error.to_string())
        }
        .await;

        let _ = docker
            .remove_container(
                &container,
                Some(
                    RemoveContainerOptionsBuilder::new()
                        .force(true)
                        .v(true)
                        .build(),
                ),
            )
            .await;

        let inventory = outcome.expect("inventory");
        assert_eq!(
            inventory.databases,
            vec!["blog", "line\nbreak", "postgres", "shop_prod"]
        );
        let roles: Vec<_> = inventory
            .roles
            .iter()
            .map(|role| (role.name.as_str(), role.superuser))
            .collect();
        assert_eq!(roles, vec![("origin-admin", true), ("reader", false)]);
        assert!(!inventory.truncated());
    }
}
