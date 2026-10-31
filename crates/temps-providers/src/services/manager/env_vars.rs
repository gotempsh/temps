// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Environment variables a service exposes to projects, previews and sandboxes.

use crate::externalsvc::{ServiceConfig, ServiceType};
use crate::services::*;
use crate::types::EnvironmentVariableInfo;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::collections::HashMap;
use temps_entities::{external_services, project_services, projects};
use tracing::{error, info, warn};

impl ExternalServiceManager {
    /// Build runtime environment variables for a cluster service.
    ///
    /// For cluster topology, the standard `ExternalService::get_runtime_env_vars()` returns
    /// empty because the cluster service doesn't have access to the database to look up
    /// member addresses. This method queries `service_members` and builds the multi-host
    /// connection string with `target_session_attrs=read-write` for automatic failover.
    ///
    /// Returns `None` if the service is not a cluster (caller should fall through to
    /// the standard `get_runtime_env_vars` path).
    async fn build_cluster_env_vars(
        &self,
        service: &external_services::Model,
        parameters: &HashMap<String, serde_json::Value>,
    ) -> Result<Option<HashMap<String, String>>, ExternalServiceError> {
        self.build_cluster_env_vars_for_resource(service, parameters, None)
            .await
    }

    /// Build cluster env vars, optionally provisioning a per-tenant
    /// database on the live primary first. When `resource_name` is
    /// `Some(name)`:
    ///   1. Connect to the live primary as the admin user.
    ///   2. `CREATE DATABASE "<name>" OWNER "<admin>"` if missing.
    ///   3. Emit env vars whose `POSTGRES_DB` and `POSTGRES_URL` point
    ///      at that DB (so each project/environment gets its own).
    ///
    /// When `resource_name` is `None`, fall back to the cluster's
    /// configured `database` parameter — kept for the legacy callers
    /// that want a generic cluster-level view.
    async fn build_cluster_env_vars_for_resource(
        &self,
        service: &external_services::Model,
        parameters: &HashMap<String, serde_json::Value>,
        resource_name: Option<&str>,
    ) -> Result<Option<HashMap<String, String>>, ExternalServiceError> {
        if service.topology != "cluster" {
            return Ok(None);
        }

        let members = self.get_service_members(service.id).await?;
        let params_str = Self::params_to_strings(parameters);

        // Extract credentials from parameters
        let username = params_str
            .get("username")
            .cloned()
            .unwrap_or_else(|| "postgres".to_string());
        let password = params_str.get("password").cloned().unwrap_or_default();
        let admin_database = params_str
            .get("database")
            .cloned()
            .unwrap_or_else(|| "postgres".to_string());

        // Per-tenant database: when the caller passes a resource name
        // we provision a dedicated DB on the primary so each app gets
        // its own. Falls back to the cluster's admin DB when no name
        // is given.
        let database = if let Some(name) = resource_name {
            self.ensure_cluster_app_database(service.id, &username, &password, name)
                .await?;
            name.to_string()
        } else {
            admin_database
        };

        // Build multi-host connection string from running data nodes (not monitor)
        let data_nodes: Vec<&ServiceMemberInfo> = members
            .iter()
            .filter(|m| !is_role_monitor(&m.role) && m.status == "running")
            .collect();

        let mut env_vars = HashMap::new();
        env_vars.insert("POSTGRES_USER".to_string(), username.clone());
        env_vars.insert("POSTGRES_PASSWORD".to_string(), password.clone());
        env_vars.insert("POSTGRES_DB".to_string(), database.clone());

        if data_nodes.is_empty() {
            // No running data nodes — still return credentials but no URL
            warn!(
                "Cluster service {} has no running data nodes, POSTGRES_URL will be empty",
                service.id
            );
            return Ok(Some(env_vars));
        }

        // Inline per-host ports (`host1:port1,host2:port2/db`) are the
        // standard PostgreSQL multi-host URI form (libpq connection-string
        // docs, "Specifying Multiple Hosts") and are what real libpq,
        // psycopg2/3, tokio-postgres/sqlx, node-postgres, and Go's
        // actively-maintained `jackc/pgx` all parse correctly -- verified
        // live against this exact cluster's real hosts/ports with pgx's
        // `stdlib` driver (`target_session_attrs=read-write` correctly
        // landed on the primary). Go's OTHER popular driver, `lib/pq`,
        // cannot parse this (or any multi-host DSN) at all in its latest
        // *released* version (v1.10.9) -- multi-host/`target_session_attrs`
        // support exists only on lib/pq's unreleased `master` branch, and
        // the project itself has been in maintenance mode since 2022,
        // pointing new users at `pgx` instead. That's a real gap for any
        // app still on lib/pq, but it's a limitation of that specific,
        // now-unmaintained driver, not a malformed connection string --
        // reformatting the URI to work around lib/pq's parser (e.g. moving
        // ports into a `?port=` query parameter) does not actually fix
        // lib/pq (verified live: it fails identically either way) and
        // would make the string non-standard for every driver that DOES
        // support this correctly today.
        let hosts: Vec<String> = data_nodes
            .iter()
            .map(|n| {
                let host = n
                    .hostname
                    .clone()
                    .unwrap_or_else(|| n.container_name.clone());
                let port = n.port.unwrap_or(5432);
                format!(
                    "{}:{}",
                    crate::externalsvc::postgres_cluster::uri_host(&host),
                    port
                )
            })
            .collect();

        let encoded_password = urlencoding::encode(&password);

        let postgres_url = format!(
            "postgresql://{}:{}@{}/{}?target_session_attrs=read-write",
            urlencoding::encode(&username),
            encoded_password,
            hosts.join(","),
            database,
        );

        let host_list = data_nodes
            .iter()
            .map(|n| {
                n.hostname
                    .clone()
                    .unwrap_or_else(|| n.container_name.clone())
            })
            .collect::<Vec<_>>()
            .join(",");

        let port = data_nodes
            .first()
            .and_then(|n| n.port)
            .unwrap_or(5432)
            .to_string();

        env_vars.insert("POSTGRES_URL".to_string(), postgres_url);
        env_vars.insert("POSTGRES_HOST".to_string(), host_list);
        env_vars.insert("POSTGRES_PORT".to_string(), port);

        Ok(Some(env_vars))
    }

    pub async fn get_service_environment_variables(
        &self,
        service_id_val: i32,
        _project_id_val: i32,
    ) -> Result<HashMap<String, String>, ExternalServiceError> {
        let service = self.get_service(service_id_val).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id_val,
                service_type: service.service_type.clone(),
            }
        })?;
        let parameters = self.get_service_parameters(service_id_val).await?;

        // Cluster services: use multi-host env vars from service_members
        if let Some(cluster_vars) = self.build_cluster_env_vars(&service, &parameters).await? {
            return Ok(cluster_vars);
        }

        let service_instance = self.create_service_instance_for_parameters(
            service.name.clone(),
            service_type,
            &parameters,
        )?;

        // Convert parameters to strings for the service
        let params_str = Self::params_to_strings(&parameters);

        // Get connection info from the service instance
        service_instance
            .get_environment_variables(&params_str)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to get environment variables: {}", e),
            })
    }

    pub async fn get_runtime_env_vars(
        &self,
        service_id_val: i32,
        project_id: i32,
        environment_id: i32,
    ) -> Result<HashMap<String, String>, ExternalServiceError> {
        // Get service
        let service = self.get_service(service_id_val).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id_val,
                service_type: service.service_type.clone(),
            }
        })?;

        // Verify service is linked to project
        let link = project_services::Entity::find()
            .filter(
                project_services::Column::ServiceId
                    .eq(service_id_val)
                    .and(project_services::Column::ProjectId.eq(project_id)),
            )
            .one(self.db.as_ref())
            .await?;

        let link = link.ok_or(ExternalServiceError::ServiceNotLinkedToProject {
            service_id: service_id_val,
            project_id,
        })?;
        let provisioning = DatabaseProvisioningConfig::from_link(&link)?;

        // Resolve the environment inside the authorized project before
        // decrypting service configuration or provisioning any tenant
        // resource. An environment ID is not globally sufficient proof of
        // project ownership, and soft-deleted environments are not targets.
        let environment = temps_entities::environments::Entity::find_by_id(environment_id)
            .filter(temps_entities::environments::Column::ProjectId.eq(project_id))
            .filter(temps_entities::environments::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::EnvironmentNotFound {
                environment_id,
                project_id,
            })?;

        let parameters = self.get_service_parameters(service_id_val).await?;

        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::ProjectNotFound { id: project_id })?;
        let (project_scope, environment_scope) = provisioning.runtime_scope(
            &project.slug,
            &environment.slug,
            service_id_val,
            project_id,
        )?;
        self.ensure_s3_scope_unambiguous(
            service_id_val,
            &service.service_type,
            &parameters,
            project_id,
            &project_scope,
            &environment_scope,
        )
        .await?;
        let resource_name = crate::externalsvc::postgres::PostgresService::normalize_database_name(
            &crate::externalsvc::scoped_resource_name(&project_scope, &environment_scope),
        );

        // Cluster services: build multi-host env vars from
        // service_members AND provision the per-tenant database on
        // the live primary so apps get isolation parity with the
        // standalone path.
        if service.topology == "cluster" && service.service_type == "postgres" {
            if let Some(cluster_vars) = self
                .build_cluster_env_vars_for_resource(&service, &parameters, Some(&resource_name))
                .await?
            {
                return Ok(cluster_vars);
            }
        }
        // Other cluster types (none today, but keep the door open)
        // get the legacy non-tenant view.
        if let Some(cluster_vars) = self.build_cluster_env_vars(&service, &parameters).await? {
            return Ok(cluster_vars);
        }

        let service_config = ServiceConfig {
            name: service.name.clone(),
            service_type,
            version: service.version,
            parameters: serde_json::to_value(&parameters).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!("Failed to serialize parameters: {}", e),
                }
            })?,
        };

        if let ServiceExecutionRoute::Remote(node_id) = service_execution_route(service.node_id) {
            info!(
                service_id = service_id_val,
                node_id, "Dispatching external-service runtime provisioning to owning node"
            );
            let client = self.get_remote_client(node_id).await?;
            return client
                .get_runtime_env_vars(crate::remote_service_client::RemoteRuntimeEnvRequest {
                    service_config,
                    project_slug: project_scope,
                    environment_slug: environment_scope,
                })
                .await
                .map(|response| response.environment)
                .map_err(|error| ExternalServiceError::InternalError {
                    reason: format!(
                        "Failed to provision runtime environment for service {} on node {}: {}",
                        service_id_val, node_id, error
                    ),
                });
        }

        // Local standalone service: preserve the existing in-process provider
        // path against the control plane's Docker daemon.
        let service_instance = self.create_service_instance_for_parameters(
            service.name.clone(),
            service_type,
            &parameters,
        )?;

        // Initialize the service to populate its internal config
        service_instance
            .init(service_config.clone())
            .await
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to initialize service: {}", e),
            })?;

        // Get runtime environment variables (this provisions resources like databases/buckets)
        // `project` and `environment` were fetched up top — reuse the slugs.
        service_instance
            .get_runtime_env_vars(service_config, &project_scope, &environment_scope)
            .await
            .map_err(|e| {
                // A sandbox's database refused to a deployment is a conflict
                // the operator resolves, not an internal failure.
                match e.downcast_ref::<crate::sandbox_engines::SandboxNamespaceConflict>() {
                    Some(conflict) => ExternalServiceError::SandboxNamespaceConflict {
                        service_id: service_id_val,
                        conflict: conflict.clone(),
                    },
                    None => ExternalServiceError::InternalError {
                        reason: format!(
                            "Failed to get runtime environment variables for service {} \
                             (project scope '{}', environment scope '{}'): {}",
                            service_id_val, project_scope, environment_scope, e
                        ),
                    },
                }
            })
    }

    /// Credentials for the sandbox in `scope` to use `service_id_val`: a
    /// namespace of its own on the service and a credential that reaches only
    /// that namespace (ADR-051). The deployment credentials are never
    /// returned.
    ///
    /// The engine for the service's type does the provisioning (see
    /// [`crate::sandbox_engines`]); this method owns the checks every engine
    /// shares. A type with no engine fails with
    /// [`ExternalServiceError::NoSandboxAccessEngine`], which callers read as
    /// "leave this service out", never as a reason to hand out the
    /// deployment's credentials.
    pub async fn issue_sandbox_runtime_env(
        &self,
        service_id_val: i32,
        scope: &temps_core::SandboxScope,
    ) -> Result<HashMap<String, String>, ExternalServiceError> {
        let service = self.get_service(service_id_val).await?;
        let engine =
            crate::sandbox_engines::engine_for(&service.service_type).ok_or_else(|| {
                ExternalServiceError::NoSandboxAccessEngine {
                    service_id: service_id_val,
                    service_type: service.service_type.clone(),
                }
            })?;
        // A link made through a project must still be a link of that project:
        // the project is what the user was allowed to reach the service by.
        let namespace = match scope.project_id {
            Some(project_id) => {
                let linked = project_services::Entity::find()
                    .filter(
                        project_services::Column::ServiceId
                            .eq(service_id_val)
                            .and(project_services::Column::ProjectId.eq(project_id)),
                    )
                    .one(self.db.as_ref())
                    .await?
                    .is_some();
                if !linked {
                    return Err(ExternalServiceError::ServiceNotLinkedToProject {
                        service_id: service_id_val,
                        project_id,
                    });
                }
                crate::sandbox_engines::SandboxNamespace::project(
                    projects::Entity::find_by_id(project_id)
                        .one(self.db.as_ref())
                        .await?
                        .ok_or(ExternalServiceError::ProjectNotFound { id: project_id })?
                        .slug,
                )
            }
            // A link made to the service directly: a namespace of the
            // sandbox's own, named so that no project's can ever match it.
            None => {
                crate::sandbox_engines::SandboxNamespace::sandbox(scope.sandbox_public_id.clone())
            }
        };
        let parameters = self.get_service_parameters(service_id_val).await?;
        engine
            .issue(
                self,
                crate::sandbox_engines::SandboxAccessRequest {
                    service: &service,
                    namespace: &namespace,
                    project_id: scope.project_id,
                    parameters: &parameters,
                },
            )
            .await
    }

    pub async fn get_service_docker_environment_variables(
        &self,
        service_id_val: i32,
        project_id_val: i32,
    ) -> Result<HashMap<String, String>, ExternalServiceError> {
        // Verify service exists
        let service = self.get_service(service_id_val).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id_val,
                service_type: service.service_type.clone(),
            }
        })?;

        // Verify service is linked to project
        let link_exists = project_services::Entity::find()
            .filter(
                project_services::Column::ServiceId
                    .eq(service_id_val)
                    .and(project_services::Column::ProjectId.eq(project_id_val)),
            )
            .one(self.db.as_ref())
            .await?;

        if link_exists.is_none() {
            return Err(ExternalServiceError::ServiceNotLinkedToProject {
                service_id: service_id_val,
                project_id: project_id_val,
            });
        }

        let parameters = self.get_service_parameters(service_id_val).await?;

        // Cluster services: use multi-host env vars from service_members
        if let Some(cluster_vars) = self.build_cluster_env_vars(&service, &parameters).await? {
            return Ok(cluster_vars);
        }

        let service_instance = self.create_service_instance_for_parameters(
            service.name.clone(),
            service_type,
            &parameters,
        )?;

        // Convert parameters to strings for the service
        let params_str = Self::params_to_strings(&parameters);

        service_instance
            .get_docker_environment_variables(&params_str)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to get docker environment variables: {}", e),
            })
    }

    pub async fn get_service_environment_variable(
        &self,
        service_id_val: i32,
        project_id_val: i32,
        var_name: &str,
    ) -> Result<EnvironmentVariableInfo, ExternalServiceError> {
        let service = self.get_service(service_id_val).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id_val,
                service_type: service.service_type.clone(),
            }
        })?;
        let parameters = self.get_service_parameters(service_id_val).await?;

        // Verify project link exists
        let link_exists = project_services::Entity::find()
            .filter(
                project_services::Column::ServiceId
                    .eq(service_id_val)
                    .and(project_services::Column::ProjectId.eq(project_id_val)),
            )
            .one(self.db.as_ref())
            .await?;

        if link_exists.is_none() {
            return Err(ExternalServiceError::ServiceNotLinkedToProject {
                service_id: service_id_val,
                project_id: project_id_val,
            });
        }

        let service_instance = self.create_service_instance_for_parameters(
            service.name.clone(),
            service_type,
            &parameters,
        )?;
        // Convert parameters to strings for the service
        let params_str = Self::params_to_strings(&parameters);

        let env_vars = service_instance
            .get_environment_variables(&params_str)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to get environment variables: {}", e),
            })?;

        // Check if the variable exists
        match env_vars.get(var_name) {
            Some(value) => {
                // All config is encrypted at rest, but we can return env vars
                // Mark common sensitive variable names as sensitive
                let sensitive_vars = ["password", "secret", "key", "token", "api_key"];
                let is_sensitive = sensitive_vars
                    .iter()
                    .any(|s| var_name.to_lowercase().contains(s));

                Ok(EnvironmentVariableInfo {
                    name: var_name.to_string(),
                    value: value.clone(),
                    sensitive: is_sensitive,
                })
            }
            None => Err(ExternalServiceError::EnvironmentVariableNotFound {
                service_id: service_id_val,
                var_name: var_name.to_string(),
            }),
        }
    }

    pub async fn get_project_service_environment_variables(
        &self,
        project_id_val: i32,
    ) -> Result<HashMap<i32, HashMap<String, String>>, ExternalServiceError> {
        // Verify project exists
        let _project = projects::Entity::find_by_id(project_id_val)
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::ProjectNotFound { id: project_id_val })?;

        // Get all services linked to this project
        let linked_services = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.eq(project_id_val))
            .all(self.db.as_ref())
            .await?;

        let mut result = HashMap::new();

        // For each linked service, get its environment variables
        for linked_service in linked_services {
            match self
                .get_service_environment_variables(linked_service.service_id, project_id_val)
                .await
            {
                Ok(env_vars) => {
                    result.insert(linked_service.service_id, env_vars);
                }
                Err(e) => {
                    error!(
                        "Failed to get environment variables for service {}: {}",
                        linked_service.service_id, e
                    );
                    // Skip this service and continue with others
                    continue;
                }
            }
        }

        Ok(result)
    }

    /// Preview the env vars a deployment in `environment_id` would receive
    /// from every service linked to `project_id`. Side-effect-free: skips
    /// `CREATE DATABASE` / bucket creation that the real runtime path
    /// performs. Used by the resolved env vars UI so users can switch
    /// between environments and see the actual database selected by the link.
    pub async fn preview_project_service_environment_variables(
        &self,
        project_id_val: i32,
        environment_id: i32,
    ) -> Result<HashMap<i32, HashMap<String, String>>, ExternalServiceError> {
        let project = projects::Entity::find_by_id(project_id_val)
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::ProjectNotFound { id: project_id_val })?;
        let environment = temps_entities::environments::Entity::find_by_id(environment_id)
            .filter(temps_entities::environments::Column::ProjectId.eq(project_id_val))
            .filter(temps_entities::environments::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await?
            .ok_or(ExternalServiceError::EnvironmentNotFound {
                environment_id,
                project_id: project_id_val,
            })?;

        let linked_services = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.eq(project_id_val))
            .all(self.db.as_ref())
            .await?;

        let mut result = HashMap::new();
        for linked in linked_services {
            match self
                .preview_service_environment_variables(
                    linked.service_id,
                    &linked,
                    &project.slug,
                    &environment.slug,
                )
                .await
            {
                Ok(env_vars) => {
                    result.insert(linked.service_id, env_vars);
                }
                Err(e) => {
                    error!(
                        "Failed to preview environment variables for service {}: {}",
                        linked.service_id, e
                    );
                    continue;
                }
            }
        }

        Ok(result)
    }

    /// Side-effect-free per-service env var preview. Mirrors
    /// `get_service_environment_variables` but calls
    /// `preview_runtime_env_vars` on the service instance so no databases
    /// or buckets get provisioned. Cluster services fall back to their
    /// regular env var path because `build_cluster_env_vars` reads from
    /// `service_members` and doesn't provision anything.
    async fn preview_service_environment_variables(
        &self,
        service_id_val: i32,
        link: &project_services::Model,
        project_slug: &str,
        environment_slug: &str,
    ) -> Result<HashMap<String, String>, ExternalServiceError> {
        let service = self.get_service(service_id_val).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id_val,
                service_type: service.service_type.clone(),
            }
        })?;
        let parameters = self.get_service_parameters(service_id_val).await?;
        let provisioning = DatabaseProvisioningConfig::from_link(link)?;
        let (project_scope, environment_scope) = provisioning.runtime_scope(
            project_slug,
            environment_slug,
            service_id_val,
            link.project_id,
        )?;

        let resource_name = crate::externalsvc::postgres::PostgresService::normalize_database_name(
            &crate::externalsvc::scoped_resource_name(&project_scope, &environment_scope),
        );

        if service.topology == "cluster" && service.service_type == "postgres" {
            if let Some(cluster_vars) = self
                .build_cluster_env_vars_for_resource(&service, &parameters, Some(&resource_name))
                .await?
            {
                return Ok(cluster_vars);
            }
        }
        if let Some(cluster_vars) = self.build_cluster_env_vars(&service, &parameters).await? {
            return Ok(cluster_vars);
        }

        let service_instance = self.create_service_instance_for_parameters(
            service.name.clone(),
            service_type,
            &parameters,
        )?;
        let service_config = ServiceConfig {
            name: service.name.clone(),
            service_type,
            version: service.version,
            parameters: serde_json::to_value(&parameters).map_err(|e| {
                ExternalServiceError::InternalError {
                    reason: format!("Failed to serialize parameters: {}", e),
                }
            })?,
        };

        service_instance
            .init(service_config.clone())
            .await
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to initialize service: {}", e),
            })?;

        service_instance
            .preview_runtime_env_vars(service_config, &project_scope, &environment_scope)
            .await
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to preview runtime environment variables: {}", e),
            })
    }

    /// Consolidated method for getting environment variables with flexible options
    ///
    /// This method replaces 7 separate environment variable methods:
    /// - get_service_environment_variables()
    /// - get_runtime_env_vars()
    /// - get_service_docker_environment_variables()
    /// - get_service_environment_variable()
    /// - get_project_service_environment_variables()
    /// - get_service_preview_environment_variable_names()
    /// - get_service_preview_environment_variables_masked()
    pub async fn get_environment_variables(
        &self,
        service_id: i32,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        options: EnvironmentVariableOptions,
    ) -> Result<EnvironmentVariablesResponse, ExternalServiceError> {
        let service = self.get_service(service_id).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id,
                service_type: service.service_type.clone(),
            }
        })?;

        let parameters = self.get_service_parameters(service_id).await?;
        let params_str = Self::params_to_strings(&parameters);
        let service_instance = self.create_service_instance_for_parameters(
            service.name.clone(),
            service_type,
            &parameters,
        )?;

        let mut all_vars = HashMap::new();

        // Cluster services: use multi-host env vars from service_members
        let is_cluster = service.topology == "cluster";
        if is_cluster {
            if let Some(cluster_vars) = self.build_cluster_env_vars(&service, &parameters).await? {
                all_vars.extend(cluster_vars);
            }
        }

        // Get basic environment variables (standalone only)
        if !is_cluster && !options.include_runtime {
            let basic_vars = service_instance
                .get_environment_variables(&params_str)
                .map_err(|e| ExternalServiceError::InternalError {
                    reason: format!("Failed to get environment variables: {}", e),
                })?;
            all_vars.extend(basic_vars);
        }

        // Get Docker-specific variables if requested (standalone only)
        if !is_cluster && options.include_docker {
            if let (Some(proj_id), Some(_env_id)) = (project_id, environment_id) {
                // Verify service is linked to project
                let link = project_services::Entity::find()
                    .filter(
                        project_services::Column::ServiceId
                            .eq(service_id)
                            .and(project_services::Column::ProjectId.eq(proj_id)),
                    )
                    .one(self.db.as_ref())
                    .await?;

                if link.is_none() {
                    return Err(ExternalServiceError::ServiceNotLinkedToProject {
                        service_id,
                        project_id: proj_id,
                    });
                }

                let docker_vars = service_instance
                    .get_docker_environment_variables(&params_str)
                    .map_err(|e| ExternalServiceError::InternalError {
                        reason: format!("Failed to get docker environment variables: {}", e),
                    })?;
                all_vars.extend(docker_vars);
            }
        }

        // Get runtime variables if requested (standalone only — clusters already populated above)
        if !is_cluster && options.include_runtime {
            if let (Some(proj_id), Some(env_id)) = (project_id, environment_id) {
                // Verify service is linked to project
                let link = project_services::Entity::find()
                    .filter(
                        project_services::Column::ServiceId
                            .eq(service_id)
                            .and(project_services::Column::ProjectId.eq(proj_id)),
                    )
                    .one(self.db.as_ref())
                    .await?;

                let link = link.ok_or(ExternalServiceError::ServiceNotLinkedToProject {
                    service_id,
                    project_id: proj_id,
                })?;

                let service_config = ServiceConfig {
                    name: service.name.clone(),
                    service_type,
                    version: service.version,
                    parameters: serde_json::to_value(&parameters).map_err(|e| {
                        ExternalServiceError::InternalError {
                            reason: format!("Failed to serialize parameters: {}", e),
                        }
                    })?,
                };

                // Initialize the service to populate its internal config
                service_instance
                    .init(service_config.clone())
                    .await
                    .map_err(|e| ExternalServiceError::InternalError {
                        reason: format!("Failed to initialize service: {}", e),
                    })?;

                // Get project and environment slugs
                let project = projects::Entity::find_by_id(proj_id)
                    .one(self.db.as_ref())
                    .await?
                    .ok_or(ExternalServiceError::ProjectNotFound { id: proj_id })?;

                let environment = temps_entities::environments::Entity::find_by_id(env_id)
                    .filter(temps_entities::environments::Column::ProjectId.eq(proj_id))
                    .filter(temps_entities::environments::Column::DeletedAt.is_null())
                    .one(self.db.as_ref())
                    .await?
                    .ok_or(ExternalServiceError::EnvironmentNotFound {
                        environment_id: env_id,
                        project_id: proj_id,
                    })?;
                let provisioning = DatabaseProvisioningConfig::from_link(&link)?;
                let (project_scope, environment_scope) = provisioning.runtime_scope(
                    &project.slug,
                    &environment.slug,
                    service_id,
                    proj_id,
                )?;

                self.ensure_s3_scope_unambiguous(
                    service_id,
                    &service.service_type,
                    &parameters,
                    proj_id,
                    &project_scope,
                    &environment_scope,
                )
                .await?;
                let runtime_vars = service_instance
                    .get_runtime_env_vars(service_config, &project_scope, &environment_scope)
                    .await
                    .map_err(|e| ExternalServiceError::InternalError {
                        reason: format!("Failed to get runtime environment variables: {}", e),
                    })?;

                all_vars.extend(runtime_vars);
            }
        }

        // Handle names_only option
        if options.names_only {
            let names_only: HashMap<String, String> = all_vars
                .keys()
                .map(|k| (k.clone(), String::new()))
                .collect();
            return Ok(EnvironmentVariablesResponse {
                variables: names_only,
                masked: false,
            });
        }

        // Handle mask_sensitive option
        let variables = if options.mask_sensitive {
            let mut masked = all_vars;
            Self::mask_environment_variable_values(&mut masked);
            masked
        } else {
            all_vars
        };

        Ok(EnvironmentVariablesResponse {
            variables,
            masked: options.mask_sensitive,
        })
    }

    /// Get environment variable names (safe preview - no sensitive values)
    pub async fn get_service_preview_environment_variable_names(
        &self,
        service_id_val: i32,
    ) -> Result<Vec<String>, ExternalServiceError> {
        let service = self.get_service(service_id_val).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id_val,
                service_type: service.service_type.clone(),
            }
        })?;
        let parameters = self.get_service_parameters(service_id_val).await?;

        // Cluster services: use multi-host env vars from service_members
        if let Some(cluster_vars) = self.build_cluster_env_vars(&service, &parameters).await? {
            return Ok(cluster_vars.keys().cloned().collect());
        }

        let service_instance = self.create_service_instance_for_parameters(
            service.name.clone(),
            service_type,
            &parameters,
        )?;

        // Convert parameters to strings for the service
        let params_str = Self::params_to_strings(&parameters);

        let env_vars = service_instance
            .get_environment_variables(&params_str)
            .map_err(|e| ExternalServiceError::InternalError {
                reason: format!("Failed to get environment variables: {}", e),
            })?;

        Ok(env_vars.keys().cloned().collect())
    }

    /// Get environment variables with masked sensitive values
    pub async fn get_service_preview_environment_variables_masked(
        &self,
        service_id_val: i32,
    ) -> Result<HashMap<String, String>, ExternalServiceError> {
        let service = self.get_service(service_id_val).await?;
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            ExternalServiceError::InvalidServiceType {
                id: service_id_val,
                service_type: service.service_type.clone(),
            }
        })?;
        let parameters = self.get_service_parameters(service_id_val).await?;

        // Cluster services: use multi-host env vars from service_members
        let env_vars =
            if let Some(cluster_vars) = self.build_cluster_env_vars(&service, &parameters).await? {
                cluster_vars
            } else {
                let service_instance = self.create_service_instance_for_parameters(
                    service.name.clone(),
                    service_type,
                    &parameters,
                )?;
                let params_str = Self::params_to_strings(&parameters);
                service_instance
                    .get_environment_variables(&params_str)
                    .map_err(|e| ExternalServiceError::InternalError {
                        reason: format!("Failed to get environment variables: {}", e),
                    })?
            };

        // Bulk previews never return plaintext. A value can contain embedded
        // credentials even when its variable name looks operational.
        let mut masked_vars = env_vars;
        Self::mask_environment_variable_values(&mut masked_vars);

        Ok(masked_vars)
    }

    pub(crate) fn mask_environment_variable_values(variables: &mut HashMap<String, String>) {
        for value in variables.values_mut() {
            *value = "***".to_string();
        }
    }

    pub(super) fn is_sensitive_parameter(param_name: &str) -> bool {
        let normalized = param_name.to_ascii_lowercase().replace('-', "_");
        let is_key = (normalized == "key" || normalized.ends_with("_key"))
            && !normalized.starts_with("public_");
        let is_url = normalized == "url"
            || normalized.ends_with("_url")
            || normalized == "uri"
            || normalized.ends_with("_uri");
        let is_connection_secret = normalized == "dsn"
            || normalized.ends_with("_dsn")
            || normalized == "connection_string"
            || normalized.ends_with("_connection_string");
        let has_secret_marker = [
            "password",
            "passwd",
            "passphrase",
            "secret",
            "token",
            "credential",
        ]
        .iter()
        .any(|marker| {
            normalized == *marker
                || normalized.starts_with(&format!("{marker}_"))
                || normalized.ends_with(&format!("_{marker}"))
        });

        is_key
            || is_url
            || is_connection_secret
            || has_secret_marker
            || normalized == "keyfile_content"
            || normalized.starts_with("private_")
    }

    pub(super) fn mask_sensitive_parameter_values(
        parameters: &mut HashMap<String, serde_json::Value>,
    ) -> Vec<String> {
        let mut sensitive_parameters = Vec::new();
        for (name, value) in parameters {
            if Self::is_sensitive_parameter(name) {
                sensitive_parameters.push(name.clone());
                if !value.is_null() {
                    *value = serde_json::Value::String("***".to_string());
                }
            }
        }
        sensitive_parameters.sort();
        sensitive_parameters
    }
}

#[async_trait::async_trait]
impl temps_core::SandboxRuntimeCredentialsProvider for ExternalServiceManager {
    async fn issue(
        &self,
        service_id: i32,
        project_id: i32,
        environment_id: i32,
    ) -> Result<HashMap<String, String>, temps_core::SandboxRuntimeCredentialsError> {
        self.get_runtime_env_vars(service_id, project_id, environment_id)
            .await
            .map_err(|error| match error {
                ExternalServiceError::ServiceNotFound { id } => {
                    temps_core::SandboxRuntimeCredentialsError::ServiceNotFound { service_id: id }
                }
                ExternalServiceError::EnvironmentNotFound {
                    environment_id,
                    project_id,
                } => temps_core::SandboxRuntimeCredentialsError::EnvironmentNotFound {
                    environment_id,
                    project_id,
                },
                ExternalServiceError::ServiceNotLinkedToProject {
                    service_id,
                    project_id,
                } => temps_core::SandboxRuntimeCredentialsError::ServiceNotLinked {
                    service_id,
                    project_id,
                },
                other => temps_core::SandboxRuntimeCredentialsError::Provider {
                    service_id,
                    reason: other.to_string(),
                },
            })
    }

    async fn sandbox_access_containers(
        &self,
        service_id: i32,
    ) -> Result<Vec<String>, temps_core::SandboxRuntimeCredentialsError> {
        let service = self
            .get_service(service_id)
            .await
            .map_err(|error| match error {
                ExternalServiceError::ServiceNotFound { id } => {
                    temps_core::SandboxRuntimeCredentialsError::ServiceNotFound { service_id: id }
                }
                other => temps_core::SandboxRuntimeCredentialsError::Provider {
                    service_id,
                    reason: other.to_string(),
                },
            })?;
        let Some(engine) = crate::sandbox_engines::engine_for(&service.service_type) else {
            return Ok(Vec::new());
        };
        engine.containers(self, &service).await.map_err(|error| {
            temps_core::SandboxRuntimeCredentialsError::Provider {
                service_id,
                reason: error.to_string(),
            }
        })
    }

    async fn issue_for_sandbox(
        &self,
        service_id: i32,
        scope: &temps_core::SandboxScope,
    ) -> Result<HashMap<String, String>, temps_core::SandboxRuntimeCredentialsError> {
        self.issue_sandbox_runtime_env(service_id, scope)
            .await
            .map_err(|error| match error {
                ExternalServiceError::ServiceNotFound { id } => {
                    temps_core::SandboxRuntimeCredentialsError::ServiceNotFound { service_id: id }
                }
                ExternalServiceError::ServiceNotLinkedToProject {
                    service_id,
                    project_id,
                } => temps_core::SandboxRuntimeCredentialsError::ServiceNotLinked {
                    service_id,
                    project_id,
                },
                // Only a type with no engine falls through to the caller's
                // choice. A service whose engine cannot reach it (another
                // node) is a failure, never a downgrade to the shared
                // credentials.
                ExternalServiceError::NoSandboxAccessEngine {
                    service_id,
                    service_type,
                } => temps_core::SandboxRuntimeCredentialsError::NoScopedCredentials {
                    service_id,
                    service_type,
                },
                other => temps_core::SandboxRuntimeCredentialsError::Provider {
                    service_id,
                    reason: other.to_string(),
                },
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "docker-tests")]
    use crate::externalsvc::ServiceType;
    use crate::services::test_support::*;
    use chrono::Utc;
    #[cfg(feature = "docker-tests")]
    use serde_json::Value as JsonValue;
    use std::collections::HashMap;
    use std::sync::Arc;
    use temps_entities::project_services;

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_service_environment_variables() {
        let (manager, _test_db) = setup_test_manager_or_skip!();
        let random_unused_port = get_unused_port();
        // Create a postgres service
        let mut params = HashMap::new();
        params.insert(
            "database".to_string(),
            JsonValue::String("envtest".to_string()),
        );
        params.insert(
            "username".to_string(),
            JsonValue::String("envuser".to_string()),
        );
        params.insert(
            "password".to_string(),
            JsonValue::String("envpass".to_string()),
        );
        params.insert(
            "port".to_string(),
            JsonValue::String(random_unused_port.to_string()),
        );
        params.insert(
            "host".to_string(),
            JsonValue::String("localhost".to_string()),
        );

        let request = CreateExternalServiceRequest {
            name: "env-test-service".to_string(),
            service_type: ServiceType::Postgres,
            version: Some("16".to_string()),
            parameters: params,
            node_id: None,
            topology: "standalone".to_string(),
            members: Vec::new(),
        };

        let service = manager.create_service(request).await.unwrap();
        let service_id = service.id;

        // Create a dummy project for testing
        let project_id = 1; // Assuming project with ID 1 exists or will be created

        // Get environment variables
        let env_vars_result = manager
            .get_service_environment_variables(service_id, project_id)
            .await;
        assert!(env_vars_result.is_ok());

        let env_vars = env_vars_result.unwrap();
        assert!(env_vars.contains_key("POSTGRES_DB"));
        assert!(env_vars.contains_key("POSTGRES_USER"));
        assert!(env_vars.contains_key("POSTGRES_PASSWORD"));
        assert_eq!(env_vars.get("POSTGRES_DB"), Some(&"envtest".to_string()));
        assert_eq!(env_vars.get("POSTGRES_USER"), Some(&"envuser".to_string()));

        // Cleanup
        let _ = manager.delete_service(service_id).await;
    }

    #[test]
    fn test_bulk_environment_variable_masking_is_content_agnostic() {
        let mut variables = HashMap::from([
            ("PORT".to_string(), "5432".to_string()),
            (
                "RUSTFS_OBS_ENDPOINT_METRICS_HEADERS".to_string(),
                "Authorization=Bearer%20ingest-secret".to_string(),
            ),
        ]);

        ExternalServiceManager::mask_environment_variable_values(&mut variables);

        assert_eq!(variables["PORT"], "***");
        assert_eq!(variables["RUSTFS_OBS_ENDPOINT_METRICS_HEADERS"], "***");
        assert!(!serde_json::to_string(&variables)
            .expect("masked environment variables should serialize")
            .contains("ingest-secret"));
    }

    #[test]
    fn test_service_parameter_policy_does_not_mask_operational_settings() {
        assert!(ExternalServiceManager::is_sensitive_parameter("password"));
        assert!(ExternalServiceManager::is_sensitive_parameter("api_token"));
        assert!(ExternalServiceManager::is_sensitive_parameter(
            "DATABASE_URL"
        ));
        assert!(ExternalServiceManager::is_sensitive_parameter(
            "connection_string"
        ));
        assert!(ExternalServiceManager::is_sensitive_parameter(
            "keyfile_content"
        ));

        assert!(!ExternalServiceManager::is_sensitive_parameter(
            "max_connections"
        ));
        assert!(!ExternalServiceManager::is_sensitive_parameter("ssl_mode"));
        assert!(!ExternalServiceManager::is_sensitive_parameter("tls_mode"));
        assert!(!ExternalServiceManager::is_sensitive_parameter(
            "accept_invalid_certs"
        ));
    }

    #[test]
    fn test_mask_sensitive_parameter_values_returns_authoritative_names() {
        let mut parameters = HashMap::from([
            ("password".to_string(), serde_json::json!("database-secret")),
            ("api_token".to_string(), serde_json::json!("token-secret")),
            (
                "keyfile_content".to_string(),
                serde_json::json!("mongodb-replica-key"),
            ),
            ("username".to_string(), serde_json::json!("temps")),
            ("port".to_string(), serde_json::json!(5432)),
            ("max_connections".to_string(), serde_json::json!(100)),
            ("ssl_mode".to_string(), serde_json::json!("prefer")),
        ]);

        let sensitive_parameters =
            ExternalServiceManager::mask_sensitive_parameter_values(&mut parameters);

        assert_eq!(
            sensitive_parameters,
            vec!["api_token", "keyfile_content", "password"]
        );
        assert_eq!(parameters["password"], serde_json::json!("***"));
        assert_eq!(parameters["api_token"], serde_json::json!("***"));
        assert_eq!(parameters["keyfile_content"], serde_json::json!("***"));
        assert_eq!(parameters["username"], serde_json::json!("temps"));
        assert_eq!(parameters["port"], serde_json::json!(5432));
        assert_eq!(parameters["max_connections"], serde_json::json!(100));
        assert_eq!(parameters["ssl_mode"], serde_json::json!("prefer"));
    }

    #[test]
    fn test_masked_sensitive_updates_are_ignored() {
        let mut parameters = HashMap::from([
            ("password".to_string(), serde_json::json!("***")),
            ("api_token".to_string(), serde_json::json!("replacement")),
            ("username".to_string(), serde_json::json!("temps")),
        ]);

        parameters.retain(|name, value| {
            !(ExternalServiceManager::is_sensitive_parameter(name)
                && value.as_str().is_some_and(|value| value == "***"))
        });

        assert!(!parameters.contains_key("password"));
        assert_eq!(parameters["api_token"], serde_json::json!("replacement"));
        assert_eq!(parameters["username"], serde_json::json!("temps"));
    }

    async fn assert_preview_rejects_unavailable_environment(environment_id: i32) {
        let project_id = 10;
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![environment_preview_project(project_id)]])
            // The scoped `id + project_id + deleted_at IS NULL` query returns
            // no row for both foreign-project and soft-deleted environments.
            .append_query_results([Vec::<temps_entities::environments::Model>::new()])
            .into_connection();
        let manager = mock_service_manager_with_db(Arc::new(db));

        let error = manager
            .preview_project_service_environment_variables(project_id, environment_id)
            .await
            .expect_err("unavailable environment must not be used for a service preview");

        assert!(matches!(
            error,
            ExternalServiceError::EnvironmentNotFound {
                environment_id: actual_environment_id,
                project_id: 10,
            } if actual_environment_id == environment_id
        ));
    }

    #[tokio::test]
    async fn service_preview_rejects_cross_project_environment() {
        assert_preview_rejects_unavailable_environment(20).await;
    }

    #[tokio::test]
    async fn service_preview_rejects_soft_deleted_environment() {
        assert_preview_rejects_unavailable_environment(21).await;
    }

    #[tokio::test]
    async fn runtime_credentials_reject_cross_project_environment_before_provisioning() {
        let service = encrypted_service_model(
            71,
            serde_json::json!({
                "username": "app",
                "password": "secret",
                "database": "postgres"
            }),
        );
        let link = project_services::Model {
            id: 9,
            project_id: 10,
            service_id: 71,
            database_provisioning_mode: "project_environment".to_string(),
            custom_database_name: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_results([vec![service]])
            .append_query_results([vec![link]])
            // Environment 20 belongs to another project, so the combined
            // id + project_id + deleted_at query returns no row.
            .append_query_results([Vec::<temps_entities::environments::Model>::new()])
            .into_connection();
        let manager = mock_service_manager_with_db(Arc::new(db));

        let error = manager
            .get_runtime_env_vars(71, 10, 20)
            .await
            .expect_err("cross-project environment must be rejected");

        assert!(matches!(
            error,
            ExternalServiceError::EnvironmentNotFound {
                environment_id: 20,
                project_id: 10
            }
        ));
    }

    #[cfg(feature = "docker-tests")]
    #[tokio::test]
    async fn test_masked_environment_variables() {
        let (manager, _test_db) = setup_test_manager_or_skip!();
        // Find a random unused port on the system

        let random_unused_port = get_unused_port();

        // Create a service with sensitive parameters
        let mut params = HashMap::new();
        params.insert(
            "database".to_string(),
            JsonValue::String("testdb".to_string()),
        );
        params.insert(
            "username".to_string(),
            JsonValue::String("user".to_string()),
        );
        params.insert(
            "password".to_string(),
            JsonValue::String("secret123".to_string()),
        );
        params.insert(
            "port".to_string(),
            JsonValue::String(random_unused_port.to_string()),
        );

        let request = CreateExternalServiceRequest {
            name: "masked-test".to_string(),
            service_type: ServiceType::Postgres,
            version: None,
            parameters: params,
            node_id: None,
            topology: "standalone".to_string(),
            members: Vec::new(),
        };

        let service = manager.create_service(request).await.unwrap();
        let service_id = service.id;

        // Get masked environment variables
        let masked_vars = manager
            .get_service_preview_environment_variables_masked(service_id)
            .await;

        assert!(masked_vars.is_ok());
        let vars = masked_vars.unwrap();

        // Bulk responses mask every value; credential-bearing content can
        // appear under otherwise operational-looking keys.
        assert_eq!(vars.get("POSTGRES_PASSWORD"), Some(&"***".to_string()));
        assert_eq!(vars.get("POSTGRES_DB"), Some(&"***".to_string()));
        assert_eq!(vars.get("POSTGRES_USER"), Some(&"***".to_string()));

        // Cleanup
        let _ = manager.delete_service(service_id).await;
    }
}
