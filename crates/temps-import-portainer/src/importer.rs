// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Portainer importer — WorkloadImporter implementation
//!
//! A Portainer **stack** is the project boundary: its compose services become
//! the workloads, database containers become managed-service candidates, and
//! published ports become the app's addresses. Standalone containers (no
//! stack label) import as single-workload projects.

use crate::client::PortainerClient;
use crate::error::PortainerImportError;
use crate::model::{DockerContainer, DockerContainerDetail};
use crate::validation::PortainerValidationRules;
use async_trait::async_trait;
use std::collections::HashMap;
use temps_import_types::plan::{
    DeploymentConfiguration, EnvironmentConfiguration, PlanComplexity, PlanMetadata,
    ProjectConfiguration, ProjectType, Protocol,
};
use temps_import_types::{
    CredentialValidation, DataImplication, DataImplicationSeverity, DeploymentStrategy, DomainPlan,
    DomainSnapshot, EnvironmentVariable, ImportContext, ImportCredentials, ImportError,
    ImportOutcome, ImportPlan, ImportResult, ImportSelector, ImportServiceProvider, ImportSource,
    ImporterCapabilities, ManualAction, ManualActionTiming, MigrationStep, MigrationSummary,
    NetworkConfiguration, NetworkInfo, NetworkMode, PortMapping, ProjectSnapshot, ResourceCounts,
    ResourceInfo, ResourceLimits, RiskLevel, ServiceAction, ServicePlan, ServiceSnapshot,
    SnapshotServiceType, StepResourceType, StepResult, UnsupportedFeature, WorkloadDescriptor,
    WorkloadId, WorkloadImporter, WorkloadSnapshot, WorkloadStatus, WorkloadType,
};
use tracing::info;

/// Workload ids are `endpoint/kind/name` so a plan can be rebuilt without
/// re-discovering: `1/stack/lab` or `1/container/my-app`.
const KIND_STACK: &str = "stack";
const KIND_CONTAINER: &str = "container";

const ENDPOINT_ID_KEY: &str = "portainer_endpoint_id";

/// Portainer importer
pub struct PortainerImporter {
    version: String,
}

impl Default for PortainerImporter {
    fn default() -> Self {
        Self::new()
    }
}

impl PortainerImporter {
    pub fn new() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Mapping helpers
// ---------------------------------------------------------------------------

fn sanitize_slug(name: &str) -> String {
    name.to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && c != '-', "-")
        .trim_matches('-')
        .to_string()
}

/// Parse `endpoint/kind/name` back into its parts
fn parse_workload_id(id: &str) -> Option<(i64, String, String)> {
    let mut parts = id.splitn(3, '/');
    let endpoint = parts.next()?.parse::<i64>().ok()?;
    let kind = parts.next()?.to_string();
    let name = parts.next()?.to_string();
    Some((endpoint, kind, name))
}

fn map_state(state: Option<&str>) -> WorkloadStatus {
    match state.unwrap_or("") {
        "running" => WorkloadStatus::Running,
        "paused" => WorkloadStatus::Paused,
        "exited" | "dead" => WorkloadStatus::Exited,
        "created" => WorkloadStatus::Stopped,
        "restarting" => WorkloadStatus::Building,
        _ => WorkloadStatus::Unknown,
    }
}

/// Detect a database from an image reference
fn detect_database(image: &str) -> Option<SnapshotServiceType> {
    let name = image.split('@').next().unwrap_or(image);
    let name = name.split(':').next().unwrap_or(name);
    let name = name.rsplit('/').next().unwrap_or(name);
    match name {
        "postgres" | "postgresql" | "timescaledb" | "pgvector" => {
            Some(SnapshotServiceType::Postgres)
        }
        "mysql" | "mariadb" | "percona" => Some(SnapshotServiceType::Mysql),
        "mongo" | "mongodb" => Some(SnapshotServiceType::MongoDB),
        "redis" | "keydb" | "valkey" => Some(SnapshotServiceType::Redis),
        _ => None,
    }
}

/// Reconstruct an externally reachable connection URL for a database
/// container from its env vars and published host port.
fn database_connection_url(
    service_type: &SnapshotServiceType,
    detail: &DockerContainerDetail,
    instance_host: Option<&str>,
) -> Option<String> {
    let host = instance_host?;
    let host_port = detail
        .published_ports()
        .into_iter()
        .find_map(|(_, host_port)| host_port)?;
    let config = &detail.config;
    match service_type {
        SnapshotServiceType::Postgres => Some(format!(
            "postgres://{}:{}@{}:{}/{}",
            config.env("POSTGRES_USER").unwrap_or("postgres"),
            config.env("POSTGRES_PASSWORD").unwrap_or(""),
            host,
            host_port,
            config
                .env("POSTGRES_DB")
                .or_else(|| config.env("POSTGRES_USER"))
                .unwrap_or("postgres"),
        )),
        SnapshotServiceType::Mysql => Some(format!(
            "mysql://{}:{}@{}:{}/{}",
            config.env("MYSQL_USER").unwrap_or("root"),
            config
                .env("MYSQL_PASSWORD")
                .or_else(|| config.env("MYSQL_ROOT_PASSWORD"))
                .unwrap_or(""),
            host,
            host_port,
            config.env("MYSQL_DATABASE").unwrap_or(""),
        )),
        SnapshotServiceType::MongoDB => Some(format!(
            "mongodb://{}:{}@{}:{}",
            config.env("MONGO_INITDB_ROOT_USERNAME").unwrap_or("root"),
            config.env("MONGO_INITDB_ROOT_PASSWORD").unwrap_or(""),
            host,
            host_port,
        )),
        SnapshotServiceType::Redis => Some(format!("redis://{}:{}", host, host_port)),
        _ => None,
    }
}

/// Build a workload snapshot from a container + its inspect detail
fn container_to_snapshot(
    container: &DockerContainer,
    detail: &DockerContainerDetail,
    endpoint_id: i64,
    insecure_tls_skip_verify: bool,
) -> WorkloadSnapshot {
    let env: HashMap<String, String> = detail.config.env_pairs().into_iter().collect();
    let ports: HashMap<u16, Option<u16>> = detail.published_ports().into_iter().collect();

    let image = detail
        .config
        .image
        .clone()
        .or_else(|| container.image.clone());

    let source_metadata = serde_json::json!({
        ENDPOINT_ID_KEY: endpoint_id,
        "container_id": container.id,
        "container_name": container.name(),
        "compose_service": container.service(),
        "stack": container.stack(),
        "volumes": detail.mounts.iter().filter_map(|m| m.name.clone()).collect::<Vec<_>>(),
        "docker_state": container.state,
        "insecure_tls_skip_verify": insecure_tls_skip_verify,
    });

    WorkloadSnapshot {
        id: WorkloadId::new(format!(
            "{}/{}/{}",
            endpoint_id,
            KIND_CONTAINER,
            container.name()
        )),
        name: Some(container.service()),
        workload_type: image
            .as_deref()
            .and_then(detect_database)
            .map(|_| WorkloadType::Database)
            .unwrap_or(WorkloadType::Container),
        status: map_state(container.state.as_deref()),
        image,
        command: None,
        entrypoint: None,
        working_dir: None,
        env,
        ports,
        volumes: vec![],
        network: NetworkInfo {
            mode: NetworkMode::Bridge,
            networks: vec![],
            hostname: None,
            domain_name: None,
        },
        resources: ResourceInfo::default(),
        labels: container.labels.clone(),
        health_check: None,
        restart_policy: None,
        created_at: chrono::Utc::now(),
        source_metadata,
    }
}

/// Build a service snapshot for a database container
fn db_to_service_snapshot(
    container: &DockerContainer,
    detail: &DockerContainerDetail,
    service_type: SnapshotServiceType,
    instance_host: Option<&str>,
) -> ServiceSnapshot {
    let connection_url = database_connection_url(&service_type, detail, instance_host);
    let reachable = connection_url.is_some();
    let image = detail
        .config
        .image
        .clone()
        .or_else(|| container.image.clone());
    let version = image.as_deref().and_then(|image| {
        let tag = image.split(':').nth(1)?;
        let version: String = tag
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        (!version.is_empty()).then_some(version)
    });
    let has_data = detail
        .mounts
        .iter()
        .any(|m| m.mount_type.as_deref() == Some("volume") || m.name.is_some());

    ServiceSnapshot {
        id: container.id.clone(),
        name: container.service(),
        service_type,
        version,
        connection_url,
        env_vars: HashMap::new(),
        has_data,
        data_size_bytes: None,
        metadata: serde_json::json!({
            "image": image,
            "reachable": reachable,
            "container_name": container.name(),
            "volumes": detail.mounts.iter().filter_map(|m| m.name.clone()).collect::<Vec<_>>(),
        }),
    }
}

fn dump_command(service_type: &SnapshotServiceType, url: &str, name: &str) -> Option<String> {
    let slug = sanitize_slug(name);
    match service_type {
        SnapshotServiceType::Postgres => Some(format!(
            "pg_dump --no-owner --format=custom \"{}\" -f {}.dump  # then: pg_restore --no-owner -d \"<temps database url>\" {}.dump",
            url, slug, slug
        )),
        SnapshotServiceType::Mysql => Some(format!(
            "mysqldump --single-transaction \"{}\" > {}.sql  # then: mysql \"<temps database url>\" < {}.sql",
            url, slug, slug
        )),
        SnapshotServiceType::MongoDB => Some(format!(
            "mongodump --uri=\"{}\" --archive={}.archive  # then: mongorestore --uri=\"<temps database url>\" --archive={}.archive",
            url, slug, slug
        )),
        _ => None,
    }
}

fn deployment_config(snapshot: &WorkloadSnapshot) -> DeploymentConfiguration {
    let env_vars: Vec<EnvironmentVariable> = {
        let mut vars: Vec<_> = snapshot.env.iter().collect();
        vars.sort_by(|a, b| a.0.cmp(b.0));
        vars.into_iter()
            .map(|(key, value)| EnvironmentVariable {
                key: key.clone(),
                is_secret: temps_import_types::plan::looks_like_secret_env_key(key),
                value: value.clone(),
                source_description: Some("Portainer container environment".to_string()),
            })
            .collect()
    };

    let ports: Vec<PortMapping> = {
        let mut sorted: Vec<u16> = snapshot.ports.keys().copied().collect();
        sorted.sort_unstable();
        sorted
            .iter()
            .enumerate()
            .map(|(index, port)| PortMapping {
                container_port: *port,
                host_port: snapshot.ports.get(port).copied().flatten(),
                protocol: Protocol::Tcp,
                is_primary: index == 0,
            })
            .collect()
    };

    DeploymentConfiguration {
        image: snapshot
            .image
            .clone()
            .unwrap_or_else(|| "unknown:latest".to_string()),
        build: None, // Portainer runs prebuilt images
        strategy: DeploymentStrategy::Replace,
        env_vars,
        ports,
        volumes: vec![],
        network: NetworkConfiguration {
            mode: NetworkMode::Bridge,
            hostname: None,
            dns_servers: vec![],
        },
        resources: ResourceLimits {
            cpu_limit: None,
            memory_limit: None,
            cpu_request: None,
            memory_request: None,
        },
        command: None,
        entrypoint: None,
        working_dir: None,
        health_check: None,
        git: None,
    }
}

// ---------------------------------------------------------------------------
// Plan generation
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct PlanExtras {
    services: Vec<ServiceSnapshot>,
    additional_workloads: Vec<WorkloadSnapshot>,
}

impl PortainerImporter {
    fn build_plan(
        &self,
        snapshot: &WorkloadSnapshot,
        project_name: &str,
        source_id: &str,
        extras: PlanExtras,
    ) -> ImportResult<ImportPlan> {
        let slug = sanitize_slug(project_name);
        let environment_name = "production".to_string();
        let deployment = deployment_config(snapshot);
        let additional_deployments: Vec<DeploymentConfiguration> = extras
            .additional_workloads
            .iter()
            .map(deployment_config)
            .collect();

        let mut service_plans = Vec::new();
        for service in &extras.services {
            let temps_type = match &service.service_type {
                SnapshotServiceType::Postgres => "postgres".to_string(),
                SnapshotServiceType::Mysql => "mysql".to_string(),
                SnapshotServiceType::Redis => "redis".to_string(),
                SnapshotServiceType::MongoDB => "mongodb".to_string(),
                SnapshotServiceType::S3 => "s3".to_string(),
                SnapshotServiceType::Kv => "kv".to_string(),
                SnapshotServiceType::Blob => "blob".to_string(),
                SnapshotServiceType::Other(name) => name.clone(),
            };
            let reachable = service
                .metadata
                .get("reachable")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let mut implications = Vec::new();
            if reachable {
                let command = service
                    .connection_url
                    .as_deref()
                    .and_then(|url| dump_command(&service.service_type, url, &service.name));
                implications.push(DataImplication {
                    severity: DataImplicationSeverity::Warning,
                    message: format!(
                        "Database container '{}' holds data in a Docker volume that will NOT be copied automatically — its published port makes a dump/restore possible",
                        service.name
                    ),
                    recommended_action: command.or_else(|| {
                        Some("Dump the database from the source and restore it into the temps-managed service".to_string())
                    }),
                });
            } else {
                implications.push(DataImplication {
                    severity: DataImplicationSeverity::DataNotMigrated,
                    message: format!(
                        "Database container '{}' publishes no host port — temps cannot reach its data from outside the Portainer host",
                        service.name
                    ),
                    recommended_action: Some(
                        "Publish the database port in Portainer before migrating, or dump it on the host (docker exec) and restore into the temps-managed service".to_string(),
                    ),
                });
            }

            let mut parameters = HashMap::new();
            parameters.insert("reachable".to_string(), serde_json::json!(reachable));
            if let Some(version) = &service.version {
                parameters.insert("version".to_string(), serde_json::json!(version));
            }
            if reachable {
                if let Some(url) = &service.connection_url {
                    parameters.insert("source_url".to_string(), serde_json::json!(url));
                }
            }

            service_plans.push(ServicePlan {
                name: service.name.clone(),
                service_type: temps_type,
                version: service.version.clone(),
                parameters,
                env_var_mappings: HashMap::new(),
                action: ServiceAction::Create,
                action_description: format!(
                    "Create a temps-managed {} service named '{}', then migrate its data (see data implications)",
                    service.service_type, service.name
                ),
                data_implications: implications,
            });
        }

        // Portainer has no domain concept — say so instead of silently
        // producing an empty domain list.
        let domain_plans: Vec<DomainPlan> = vec![];

        let mut steps = Vec::new();
        let mut order = 1usize;
        steps.push(MigrationStep {
            order,
            id: "create-project".to_string(),
            title: format!("Create project '{}'", project_name),
            description: format!(
                "Create a temps project '{}' with environment '{}'",
                project_name, environment_name
            ),
            resource_type: StepResourceType::Project,
            risk: RiskLevel::None,
            data_implications: vec![],
            pre_conditions: vec![],
            post_conditions: vec![format!("Project '{}' visible in temps", project_name)],
            skippable: false,
            skipped: false,
            reversible: true,
            estimated_duration: Some("< 5 seconds".to_string()),
        });

        if !deployment.env_vars.is_empty() {
            order += 1;
            steps.push(MigrationStep {
                order,
                id: "configure-env-vars".to_string(),
                title: format!("Copy {} environment variable(s)", deployment.env_vars.len()),
                description:
                    "Set the container's environment variables on the temps environment. Values referencing other compose services by name must be updated to the new temps service addresses."
                        .to_string(),
                resource_type: StepResourceType::EnvironmentVariable,
                risk: RiskLevel::Low,
                data_implications: vec![],
                pre_conditions: vec![],
                post_conditions: vec![
                    "Review copied values — rewrite any that point at compose service names".to_string(),
                ],
                skippable: true,
                skipped: false,
                reversible: true,
                estimated_duration: Some("< 5 seconds".to_string()),
            });
        }

        for service_plan in &service_plans {
            order += 1;
            steps.push(MigrationStep {
                order,
                id: format!("create-service-{}", sanitize_slug(&service_plan.name)),
                title: format!(
                    "Create managed {} service '{}'",
                    service_plan.service_type, service_plan.name
                ),
                description: service_plan.action_description.clone(),
                resource_type: StepResourceType::Service,
                risk: RiskLevel::High,
                data_implications: service_plan.data_implications.clone(),
                pre_conditions: vec![],
                post_conditions: vec![format!(
                    "Data restored into '{}' and row counts verified against the source",
                    service_plan.name
                )],
                skippable: true,
                skipped: false,
                reversible: true,
                estimated_duration: Some("manual — depends on data size".to_string()),
            });
        }

        order += 1;
        steps.push(MigrationStep {
            order,
            id: "deploy-application".to_string(),
            title: format!(
                "Deploy '{}'",
                snapshot.name.clone().unwrap_or_else(|| slug.clone())
            ),
            description: format!("Deploy the container image '{}'", deployment.image),
            resource_type: StepResourceType::Deployment,
            risk: RiskLevel::Low,
            data_implications: vec![],
            pre_conditions: vec![],
            post_conditions: vec!["Application responds on its temps domain".to_string()],
            skippable: false,
            skipped: false,
            reversible: true,
            estimated_duration: Some("10-60 seconds".to_string()),
        });

        let mut critical_warnings = Vec::new();
        // Portainer's API exposes no repository metadata for a container, so
        // the imported project is never linked to a git repo and the first
        // deployment cannot start automatically.
        let mut manual_actions = vec![ManualAction {
            timing: ManualActionTiming::AfterMigration,
            description: "Link a git repository or trigger the first deployment manually"
                .to_string(),
            reason: "Portainer has no repository metadata for this container — the import records configuration only, so temps has nothing to build from yet".to_string(),
        }];
        for service_plan in &service_plans {
            if service_plan
                .data_implications
                .iter()
                .any(|i| i.severity == DataImplicationSeverity::DataNotMigrated)
            {
                critical_warnings.push(format!(
                    "Database '{}' data cannot be reached from outside the Portainer host — plan its dump/restore before switching traffic",
                    service_plan.name
                ));
                manual_actions.push(ManualAction {
                    timing: ManualActionTiming::BeforeMigration,
                    description: format!(
                        "Dump database '{}' (publish its port in Portainer, or docker exec on the host)",
                        service_plan.name
                    ),
                    reason: "The container publishes no host port, so temps cannot copy its data"
                        .to_string(),
                });
            } else {
                manual_actions.push(ManualAction {
                    timing: ManualActionTiming::AfterMigration,
                    description: format!(
                        "Restore database '{}' into the temps-managed service and verify row counts",
                        service_plan.name
                    ),
                    reason: "Data is copied via dump/restore, not automatically".to_string(),
                });
            }
        }
        manual_actions.push(ManualAction {
            timing: ManualActionTiming::AfterMigration,
            description:
                "Point your DNS at the temps server — Portainer served these apps by host:port, so there is no existing domain to carry over"
                    .to_string(),
            reason: "Portainer has no domain management; temps assigns a preview domain and can hold custom domains".to_string(),
        });

        let overall_risk = if !service_plans.is_empty() {
            RiskLevel::High
        } else {
            RiskLevel::Low
        };

        let workload_count = 1 + additional_deployments.len();
        let summary = MigrationSummary {
            headline: format!(
                "Migrate '{}' from Portainer: {} container(s), {} database(s)",
                project_name,
                workload_count,
                service_plans.len()
            ),
            overall_risk,
            resource_counts: ResourceCounts {
                projects: 1,
                environments: 1,
                deployments: workload_count,
                environment_variables: deployment.env_vars.len(),
                services: service_plans.len(),
                domains: 0,
            },
            critical_warnings,
            manual_actions_required: manual_actions,
            unsupported_features: vec![
                UnsupportedFeature {
                    feature: "Docker volumes".to_string(),
                    reason: "Volume contents are not copied by the importer; only databases have a dump/restore path".to_string(),
                    alternative: Some("Copy non-database volume data manually, or mount equivalent storage in temps".to_string()),
                },
                UnsupportedFeature {
                    feature: "Swarm-mode stacks".to_string(),
                    reason: "Swarm services (replicas, placement constraints) collapse to a single container per temps environment".to_string(),
                    alternative: Some("Use temps worker nodes for horizontal scale".to_string()),
                },
            ],
        };

        let complexity = if service_plans.is_empty() && additional_deployments.is_empty() {
            PlanComplexity::Low
        } else if service_plans.len() <= 1 {
            PlanComplexity::Medium
        } else {
            PlanComplexity::High
        };

        Ok(ImportPlan {
            version: "1.0".to_string(),
            source: ImportSource::Portainer.to_string(),
            source_id: source_id.to_string(),
            project: ProjectConfiguration {
                name: project_name.to_string(),
                slug,
                project_type: ProjectType::Docker,
                is_web_app: !deployment.ports.is_empty(),
            },
            environment: EnvironmentConfiguration {
                name: environment_name,
                subdomain: sanitize_slug(project_name),
                resources: ResourceLimits {
                    cpu_limit: None,
                    memory_limit: None,
                    cpu_request: None,
                    memory_request: None,
                },
            },
            deployment,
            services: service_plans,
            domains: domain_plans,
            additional_deployments,
            steps,
            summary,
            metadata: PlanMetadata {
                generated_at: chrono::Utc::now(),
                generator_version: self.version.clone(),
                complexity,
                warnings: {
                    let mut warnings = Vec::new();
                    let insecure_tls_skip_verify = snapshot
                        .source_metadata
                        .get("insecure_tls_skip_verify")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if insecure_tls_skip_verify {
                        warnings.push(
                            "Connecting to this Portainer instance skips TLS certificate verification (credentials.extra[\"verify_tls\"] = \"false\" was set) — the admin password travels over an unverified connection. Remove that override once this instance has a trusted certificate.".to_string(),
                        );
                    }
                    warnings
                },
            },
            cost_analysis: None,
        })
    }
}

// ---------------------------------------------------------------------------
// WorkloadImporter implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl WorkloadImporter for PortainerImporter {
    fn source(&self) -> ImportSource {
        ImportSource::Portainer
    }

    fn name(&self) -> &str {
        "Portainer"
    }

    fn version(&self) -> &str {
        &self.version
    }

    async fn health_check(&self) -> ImportResult<bool> {
        Ok(true)
    }

    async fn validate_credentials(
        &self,
        credentials: &ImportCredentials,
    ) -> ImportResult<CredentialValidation> {
        let client = match PortainerClient::from_credentials(credentials).await {
            Ok(client) => client,
            Err(e) => {
                return Ok(CredentialValidation {
                    valid: false,
                    account_name: None,
                    message: Some(e.to_string()),
                })
            }
        };
        match client.login().await {
            Ok(jwt) => {
                let endpoints = client.endpoints(&jwt).await.unwrap_or_default();
                Ok(CredentialValidation {
                    valid: true,
                    account_name: endpoints.first().map(|e| e.name.clone()),
                    message: Some(format!(
                        "Connected — {} Docker environment(s) available",
                        endpoints.len()
                    )),
                })
            }
            Err(e) => Ok(CredentialValidation {
                valid: false,
                account_name: None,
                message: Some(e.to_string()),
            }),
        }
    }

    async fn discover(
        &self,
        credentials: &ImportCredentials,
        selector: ImportSelector,
    ) -> ImportResult<Vec<WorkloadDescriptor>> {
        let client = PortainerClient::from_credentials(credentials)
            .await
            .map_err(ImportError::from)?;
        let jwt = client.login().await.map_err(ImportError::from)?;
        let endpoints = client.endpoints(&jwt).await.map_err(ImportError::from)?;
        if endpoints.is_empty() {
            return Err(ImportError::from(PortainerImportError::NoEndpoint));
        }
        let stacks = client.stacks(&jwt).await.unwrap_or_default();

        let mut descriptors = Vec::new();
        for endpoint in endpoints.iter().filter(|e| e.is_up()) {
            // Stacks first — they are the natural project unit.
            for stack in stacks.iter().filter(|s| s.endpoint_id == endpoint.id) {
                descriptors.push(WorkloadDescriptor {
                    id: WorkloadId::new(format!("{}/{}/{}", endpoint.id, KIND_STACK, stack.name)),
                    name: Some(stack.name.clone()),
                    workload_type: WorkloadType::Container,
                    status: if stack.status == 1 {
                        WorkloadStatus::Running
                    } else {
                        WorkloadStatus::Stopped
                    },
                    image: None,
                    created_at: None,
                    labels: HashMap::from([("environment".to_string(), endpoint.name.clone())]),
                });
            }

            // Standalone containers (no compose stack) import individually.
            let containers = client
                .containers(&jwt, endpoint.id)
                .await
                .map_err(ImportError::from)?;
            for container in containers
                .iter()
                .filter(|c| !c.is_portainer_infra() && c.stack().is_none())
            {
                descriptors.push(WorkloadDescriptor {
                    id: WorkloadId::new(format!(
                        "{}/{}/{}",
                        endpoint.id,
                        KIND_CONTAINER,
                        container.name()
                    )),
                    name: Some(container.name()),
                    workload_type: container
                        .image
                        .as_deref()
                        .and_then(detect_database)
                        .map(|_| WorkloadType::Database)
                        .unwrap_or(WorkloadType::Container),
                    status: map_state(container.state.as_deref()),
                    image: container.image.clone(),
                    created_at: None,
                    labels: HashMap::from([("environment".to_string(), endpoint.name.clone())]),
                });
            }
        }

        if let Some(pattern) = &selector.name_pattern {
            let needle = pattern.to_lowercase();
            descriptors.retain(|d| {
                d.name
                    .as_deref()
                    .map(|n| n.to_lowercase().contains(&needle))
                    .unwrap_or(false)
            });
        }
        if let Some(limit) = selector.limit {
            descriptors.truncate(limit);
        }

        info!("Discovered {} Portainer workloads", descriptors.len());
        Ok(descriptors)
    }

    async fn describe(
        &self,
        credentials: &ImportCredentials,
        workload_id: &WorkloadId,
    ) -> ImportResult<WorkloadSnapshot> {
        let client = PortainerClient::from_credentials(credentials)
            .await
            .map_err(ImportError::from)?;
        let jwt = client.login().await.map_err(ImportError::from)?;
        let (endpoint_id, _kind, name) =
            parse_workload_id(workload_id.as_str()).ok_or_else(|| {
                ImportError::InvalidConfiguration(format!(
                    "'{}' is not a Portainer workload id (expected endpoint/kind/name)",
                    workload_id
                ))
            })?;

        let containers = client
            .containers(&jwt, endpoint_id)
            .await
            .map_err(ImportError::from)?;
        let container = containers
            .iter()
            .find(|c| c.name() == name || c.service() == name)
            .ok_or_else(|| {
                ImportError::from(PortainerImportError::WorkloadNotFound {
                    id: workload_id.as_str().to_string(),
                })
            })?;
        let detail = client
            .container_detail(&jwt, endpoint_id, &container.id)
            .await
            .map_err(ImportError::from)?;
        Ok(container_to_snapshot(
            container,
            &detail,
            endpoint_id,
            client.skip_tls_verify(),
        ))
    }

    async fn describe_project(
        &self,
        credentials: &ImportCredentials,
        workload_id: &WorkloadId,
    ) -> ImportResult<ProjectSnapshot> {
        let client = PortainerClient::from_credentials(credentials)
            .await
            .map_err(ImportError::from)?;
        let jwt = client.login().await.map_err(ImportError::from)?;
        let (endpoint_id, kind, name) =
            parse_workload_id(workload_id.as_str()).ok_or_else(|| {
                ImportError::InvalidConfiguration(format!(
                    "'{}' is not a Portainer workload id (expected endpoint/kind/name)",
                    workload_id
                ))
            })?;

        let containers = client
            .containers(&jwt, endpoint_id)
            .await
            .map_err(ImportError::from)?;
        let instance_host = client.host();

        // Members of this project: a stack's containers, or the single
        // standalone container.
        let members: Vec<&DockerContainer> = if kind == KIND_STACK {
            containers
                .iter()
                .filter(|c| !c.is_portainer_infra() && c.stack() == Some(name.as_str()))
                .collect()
        } else {
            containers
                .iter()
                .filter(|c| !c.is_portainer_infra() && c.name() == name)
                .collect()
        };
        if members.is_empty() {
            return Err(ImportError::from(PortainerImportError::WorkloadNotFound {
                id: workload_id.as_str().to_string(),
            }));
        }

        // Inspect each member once; split into apps and databases.
        let mut apps: Vec<(&DockerContainer, DockerContainerDetail)> = Vec::new();
        let mut databases: Vec<(&DockerContainer, DockerContainerDetail, SnapshotServiceType)> =
            Vec::new();
        for container in members {
            let detail = client
                .container_detail(&jwt, endpoint_id, &container.id)
                .await
                .map_err(ImportError::from)?;
            let image = detail
                .config
                .image
                .clone()
                .or_else(|| container.image.clone());
            match image.as_deref().and_then(detect_database) {
                Some(service_type) => databases.push((container, detail, service_type)),
                None => apps.push((container, detail)),
            }
        }

        // A stack of only databases is still importable — the first database
        // becomes the primary workload so nothing is silently dropped.
        let (primary_container, primary_detail) = if let Some((c, d)) = apps.first() {
            (*c, d.clone())
        } else {
            let (c, d, _) = &databases[0];
            (*c, d.clone())
        };

        let primary_workload = container_to_snapshot(
            primary_container,
            &primary_detail,
            endpoint_id,
            client.skip_tls_verify(),
        );
        let additional_workloads: Vec<WorkloadSnapshot> = apps
            .iter()
            .filter(|(c, _)| c.id != primary_container.id)
            .map(|(c, d)| container_to_snapshot(c, d, endpoint_id, client.skip_tls_verify()))
            .collect();

        let services: Vec<ServiceSnapshot> = databases
            .iter()
            .filter(|(c, _, _)| c.id != primary_container.id)
            .map(|(c, d, t)| db_to_service_snapshot(c, d, t.clone(), instance_host.as_deref()))
            .collect();

        Ok(ProjectSnapshot {
            id: workload_id.clone(),
            name: name.clone(),
            primary_workload,
            additional_workloads,
            services,
            // Portainer has no domain management.
            domains: Vec::<DomainSnapshot>::new(),
            git_info: None,
            detected_framework: None,
            source_metadata: serde_json::json!({
                ENDPOINT_ID_KEY: endpoint_id,
                "kind": kind,
            }),
        })
    }

    fn generate_plan(&self, snapshot: WorkloadSnapshot) -> ImportResult<ImportPlan> {
        let name = snapshot
            .name
            .clone()
            .unwrap_or_else(|| snapshot.id.as_str().replace('/', "-"));
        let source_id = snapshot.id.as_str().to_string();
        self.build_plan(&snapshot, &name, &source_id, PlanExtras::default())
    }

    fn generate_project_plan(&self, snapshot: ProjectSnapshot) -> ImportResult<ImportPlan> {
        let extras = PlanExtras {
            services: snapshot.services.clone(),
            additional_workloads: snapshot.additional_workloads.clone(),
        };
        let source_id = snapshot.id.as_str().to_string();
        self.build_plan(
            &snapshot.primary_workload,
            &snapshot.name,
            &source_id,
            extras,
        )
    }

    fn validation_rules(&self) -> Vec<Box<dyn temps_import_types::ImportValidationRule>> {
        PortainerValidationRules::all_rules()
    }

    async fn execute(
        &self,
        context: ImportContext,
        plan: ImportPlan,
        services: &dyn ImportServiceProvider,
    ) -> ImportResult<ImportOutcome> {
        execute_plan(context, plan, services).await
    }

    fn capabilities(&self) -> ImporterCapabilities {
        ImporterCapabilities {
            supports_volumes: false,
            supports_networks: false,
            supports_health_checks: false,
            supports_resource_limits: false,
            supports_build: false, // Portainer runs prebuilt images
            supports_stacks: true, // a compose stack imports as one project
            supports_services: true,
            supports_domains: false, // Portainer has no domain management
            supports_project_snapshot: true,
            supports_cost_analysis: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

async fn execute_plan(
    context: ImportContext,
    plan: ImportPlan,
    services: &dyn ImportServiceProvider,
) -> ImportResult<ImportOutcome> {
    use sea_orm::{
        ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter,
    };
    use std::time::Instant;
    use temps_entities::{
        deployment_containers, deployments, environments, prelude::DeploymentMetadata, projects,
    };

    let start_time = Instant::now();
    info!(
        "Executing Portainer import for session: {} (dry_run: {})",
        context.session_id, context.dry_run
    );

    let step_results: Vec<StepResult> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    if context.dry_run {
        return Ok(ImportOutcome {
            session_id: context.session_id.clone(),
            success: true,
            project_id: None,
            environment_id: None,
            deployment_id: None,
            warnings,
            errors: vec![],
            created_resources: vec![],
            step_results,
            duration_seconds: start_time.elapsed().as_secs_f64(),
        });
    }

    let mut created_resources = Vec::new();
    let db = services.db();
    let project_service = services
        .project_service()
        .downcast_ref::<temps_projects::ProjectService>()
        .ok_or_else(|| ImportError::Internal("Failed to get ProjectService".to_string()))?;

    use temps_presets::get_preset_by_slug;
    if get_preset_by_slug(&context.preset).is_none() {
        warnings.push(format!(
            "Preset '{}' not found, using default configuration",
            context.preset
        ));
    }

    let create_project_request = temps_projects::services::types::CreateProjectRequest {
        name: context.project_name.clone(),
        expected_slug: None,
        repo_name: context.repo_name.clone(),
        repo_owner: context.repo_owner.clone(),
        directory: context.directory.clone(),
        main_branch: context.main_branch.clone(),
        preset: context.preset.clone(),
        preset_config: None,
        // is_secret vars carry a real value read from Portainer's container
        // inspect API (unlike Kamal's empty placeholders) — they must reach
        // the deployment or the app boots without its real credentials.
        // temps encrypts every env var at rest regardless of this flag;
        // only the plan API response masks it (mask_plan_secrets in the
        // orchestrator).
        environment_variables: Some(
            plan.deployment
                .env_vars
                .iter()
                // The plan's `is_secret` is a heuristic over the key name, not
                // a flag the source platform asserts, so it is deliberately not
                // forwarded as temps' write-only `is_secret`: that would hide
                // imported values from the operator who still needs to verify
                // them. Every value is encrypted at rest either way.
                .map(|env| {
                    temps_projects::services::types::CreateProjectEnvVar::plain(
                        env.key.clone(),
                        env.value.clone(),
                    )
                })
                .collect(),
        ),
        automatic_deploy: true,
        storage_service_ids: vec![],
        storage_service_claim_ids: vec![],
        storage_service_claim_user_id: None,
        is_public_repo: None,
        git_url: None,
        git_provider_connection_id: context.git_provider_connection_id,
        exposed_port: None,
        cpu_request: None,
        cpu_limit: None,
        memory_request: None,
        memory_limit: None,
        // Portainer workloads are pure containers/stacks with no git repo —
        // DockerImage is the correct source_type (was incorrectly Git, which
        // both misrepresents the project and made determine_deployment_source_type
        // try to plan a git deploy on any future redeploy with no metadata override).
        source_type: temps_entities::source_type::SourceType::DockerImage,
        template_slug: None,
    };

    let project = project_service
        .create_project(create_project_request)
        .await
        .map_err(|e| ImportError::ExecutionFailed(format!("Failed to create project: {}", e)))?;

    created_resources.push(temps_import_types::CreatedResource {
        resource_type: "project".to_string(),
        resource_id: project.id,
        resource_name: project.name.clone(),
    });

    let environment = environments::Entity::find()
        .filter(environments::Column::ProjectId.eq(project.id))
        .filter(environments::Column::Name.eq(&plan.environment.name))
        .one(db)
        .await
        .map_err(|e| ImportError::ExecutionFailed(format!("Failed to find environment: {}", e)))?
        .ok_or_else(|| {
            ImportError::ExecutionFailed(format!(
                "Environment '{}' not found for project {}",
                plan.environment.name, project.id
            ))
        })?;

    created_resources.push(temps_import_types::CreatedResource {
        resource_type: "environment".to_string(),
        resource_id: environment.id,
        resource_name: environment.name.clone(),
    });

    let deployment_count = deployments::Entity::find()
        .filter(deployments::Column::ProjectId.eq(project.id))
        .paginate(db, 100)
        .num_items()
        .await
        .map_err(|e| ImportError::ExecutionFailed(format!("Failed to count deployments: {}", e)))?;
    let deployment_slug = format!("{}-{}", project.slug, deployment_count + 1);

    let deployment_metadata = DeploymentMetadata {
        builder: Some("portainer-import".to_string()),
        labels: vec!["imported".to_string(), "portainer".to_string()],
        ..Default::default()
    };

    let now = chrono::Utc::now();
    let deployment = deployments::ActiveModel {
        project_id: Set(project.id),
        environment_id: Set(environment.id),
        slug: Set(deployment_slug.clone()),
        state: Set("pending".to_string()),
        metadata: Set(Some(deployment_metadata)),
        image_name: Set(Some(plan.deployment.image.clone())),
        commit_message: Set(Some(format!(
            "Imported from Portainer ({})",
            plan.source_id
        ))),
        // Not deployed yet — the orchestrator triggers the real
        // pipeline and records the outcome after this returns.
        deploying_at: Set(None),
        ready_at: Set(None),
        started_at: Set(None),
        finished_at: Set(None),
        ..Default::default()
    };
    let deployment = deployment
        .insert(db)
        .await
        .map_err(|e| ImportError::ExecutionFailed(format!("Failed to create deployment: {}", e)))?;

    created_resources.push(temps_import_types::CreatedResource {
        resource_type: "deployment".to_string(),
        resource_id: deployment.id,
        resource_name: deployment.slug.clone(),
    });

    for (index, port_mapping) in plan.deployment.ports.iter().enumerate() {
        let container_name = if index == 0 {
            format!("{}-{}", project.name, deployment.slug)
        } else {
            format!("{}-{}-{}", project.name, deployment.slug, index)
        };
        let deployment_container = deployment_containers::ActiveModel {
            deployment_id: Set(deployment.id),
            container_id: Set(plan.source_id.clone()),
            container_name: Set(container_name),
            container_port: Set(port_mapping.container_port as i32),
            host_port: Set(port_mapping.host_port.map(|p| p as i32)),
            image_name: Set(Some(plan.deployment.image.clone())),
            status: Set(Some("pending".to_string())),
            deployed_at: Set(now),
            ready_at: Set(None),
            ..Default::default()
        };
        deployment_container.insert(db).await.map_err(|e| {
            ImportError::ExecutionFailed(format!(
                "Failed to create deployment container for port {}: {}",
                port_mapping.container_port, e
            ))
        })?;
    }

    let mut environment_active: environments::ActiveModel = environment.clone().into();
    environment_active.current_deployment_id = Set(Some(deployment.id));
    environment_active.last_deployment = Set(Some(now));
    environment_active.update(db).await.map_err(|e| {
        ImportError::ExecutionFailed(format!(
            "Failed to update environment with current deployment: {}",
            e
        ))
    })?;

    let project_entity = projects::Entity::find_by_id(project.id)
        .one(db)
        .await
        .map_err(|e| {
            ImportError::ExecutionFailed(format!("Failed to fetch project entity: {}", e))
        })?
        .ok_or_else(|| {
            ImportError::ExecutionFailed(format!("Project {} not found after creation", project.id))
        })?;
    let mut project_active: projects::ActiveModel = project_entity.into();
    project_active.last_deployment = Set(Some(now));
    project_active.update(db).await.map_err(|e| {
        ImportError::ExecutionFailed(format!("Failed to update project last deployment: {}", e))
    })?;

    let duration = start_time.elapsed().as_secs_f64();
    info!(
        "✅ Portainer import completed in {:.2}s — project {} (id: {})",
        duration, project.name, project.id
    );

    Ok(ImportOutcome {
        session_id: context.session_id.clone(),
        success: true,
        project_id: Some(project.id),
        environment_id: Some(environment.id),
        deployment_id: Some(deployment.id),
        warnings,
        errors: vec![],
        created_resources,
        step_results,
        duration_seconds: duration,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn whoami_container() -> DockerContainer {
        serde_json::from_value(serde_json::json!({
            "Id": "c1",
            "Names": ["/lab-lab-whoami-1"],
            "Image": "traefik/whoami:latest",
            "State": "running",
            "Labels": {
                "com.docker.compose.project": "lab",
                "com.docker.compose.service": "lab-whoami"
            }
        }))
        .unwrap()
    }

    fn whoami_detail() -> DockerContainerDetail {
        serde_json::from_value(serde_json::json!({
            "Name": "/lab-lab-whoami-1",
            "Config": {
                "Image": "traefik/whoami:latest",
                "Env": ["LAB_SECRET=s3cret", "PLAN=pro", "PATH=/usr/bin"],
                "Labels": {}
            },
            "NetworkSettings": {"Ports": {"80/tcp": [{"HostIp": "0.0.0.0", "HostPort": "8080"}]}},
            "Mounts": []
        }))
        .unwrap()
    }

    fn db_container() -> DockerContainer {
        serde_json::from_value(serde_json::json!({
            "Id": "c2",
            "Names": ["/lab-lab-db-1"],
            "Image": "postgres:16",
            "State": "running",
            "Labels": {
                "com.docker.compose.project": "lab",
                "com.docker.compose.service": "lab-db"
            }
        }))
        .unwrap()
    }

    fn db_detail(published: bool) -> DockerContainerDetail {
        let ports = if published {
            serde_json::json!({"5432/tcp": [{"HostIp": "0.0.0.0", "HostPort": "5432"}]})
        } else {
            serde_json::json!({ "5432/tcp": null })
        };
        serde_json::from_value(serde_json::json!({
            "Name": "/lab-lab-db-1",
            "Config": {
                "Image": "postgres:16",
                "Env": ["POSTGRES_USER=lab", "POSTGRES_PASSWORD=pw", "POSTGRES_DB=labdb"],
                "Labels": {}
            },
            "NetworkSettings": {"Ports": ports},
            "Mounts": [{"Type": "volume", "Name": "lab_lab-db-data", "Destination": "/var/lib/postgresql/data"}]
        }))
        .unwrap()
    }

    fn project_snapshot(published_db: bool) -> ProjectSnapshot {
        let primary = container_to_snapshot(&whoami_container(), &whoami_detail(), 1, false);
        let service = db_to_service_snapshot(
            &db_container(),
            &db_detail(published_db),
            SnapshotServiceType::Postgres,
            Some("1.2.3.4"),
        );
        ProjectSnapshot {
            id: WorkloadId::new("1/stack/lab"),
            name: "lab".to_string(),
            primary_workload: primary,
            additional_workloads: vec![],
            services: vec![service],
            domains: vec![],
            git_info: None,
            detected_framework: None,
            source_metadata: serde_json::json!({ ENDPOINT_ID_KEY: 1, "kind": "stack" }),
        }
    }

    #[test]
    fn parses_workload_ids() {
        assert_eq!(
            parse_workload_id("1/stack/lab"),
            Some((1, "stack".to_string(), "lab".to_string()))
        );
        assert_eq!(
            parse_workload_id("2/container/my-app"),
            Some((2, "container".to_string(), "my-app".to_string()))
        );
        assert_eq!(parse_workload_id("nonsense"), None);
    }

    #[test]
    fn detects_database_images_including_registry_prefixes() {
        assert_eq!(
            detect_database("postgres:16"),
            Some(SnapshotServiceType::Postgres)
        );
        assert_eq!(
            detect_database("docker.io/library/mariadb:11"),
            Some(SnapshotServiceType::Mysql)
        );
        assert_eq!(detect_database("traefik/whoami:latest"), None);
    }

    #[test]
    fn container_snapshot_drops_image_noise_and_keeps_ports() {
        let snapshot = container_to_snapshot(&whoami_container(), &whoami_detail(), 1, false);
        assert_eq!(snapshot.name.as_deref(), Some("lab-whoami"));
        assert!(snapshot.env.contains_key("LAB_SECRET"));
        assert!(!snapshot.env.contains_key("PATH"));
        assert_eq!(snapshot.ports.get(&80), Some(&Some(8080)));
        assert_eq!(snapshot.status, WorkloadStatus::Running);
    }

    /// Regression test: Portainer's client accepts its default self-signed
    /// certificate without verification, but that used to be a silent
    /// choice — the user importing from a real instance had no way of
    /// knowing their admin password crossed an unverified TLS connection.
    /// `PortainerClient::skip_tls_verify()` now flows into the snapshot's
    /// `source_metadata` and from there into `PlanMetadata.warnings`.
    #[test]
    fn test_insecure_tls_skip_verify_surfaces_as_plan_warning() {
        let importer = PortainerImporter::new();
        let snapshot = container_to_snapshot(&whoami_container(), &whoami_detail(), 1, true);
        let plan = importer.generate_plan(snapshot).unwrap();

        assert!(
            plan.metadata
                .warnings
                .iter()
                .any(|w| w.contains("skips TLS certificate verification")),
            "expected a TLS-skip warning, got: {:?}",
            plan.metadata.warnings
        );
    }

    #[test]
    fn test_verified_tls_snapshot_has_no_insecure_tls_warning() {
        let importer = PortainerImporter::new();
        let snapshot = container_to_snapshot(&whoami_container(), &whoami_detail(), 1, false);
        let plan = importer.generate_plan(snapshot).unwrap();

        assert!(
            !plan
                .metadata
                .warnings
                .iter()
                .any(|w| w.contains("skips TLS certificate verification")),
            "a verified connection must not carry the TLS-skip warning"
        );
    }

    #[test]
    fn published_database_gets_connection_url() {
        let service = db_to_service_snapshot(
            &db_container(),
            &db_detail(true),
            SnapshotServiceType::Postgres,
            Some("1.2.3.4"),
        );
        assert_eq!(
            service.connection_url.as_deref(),
            Some("postgres://lab:pw@1.2.3.4:5432/labdb")
        );
        assert!(service.has_data, "volume-backed database has data");
    }

    #[test]
    fn unpublished_database_has_no_connection_url() {
        let service = db_to_service_snapshot(
            &db_container(),
            &db_detail(false),
            SnapshotServiceType::Postgres,
            Some("1.2.3.4"),
        );
        assert_eq!(service.connection_url, None);
    }

    #[test]
    fn stack_plan_covers_app_and_database_with_source_url() {
        let importer = PortainerImporter::new();
        let plan = importer
            .generate_project_plan(project_snapshot(true))
            .unwrap();

        assert_eq!(plan.source, "portainer");
        assert_eq!(plan.summary.resource_counts.services, 1);
        assert_eq!(plan.summary.resource_counts.domains, 0);
        assert_eq!(plan.project.project_type, ProjectType::Docker);

        // the orchestrator's population phase needs source_url
        let service = &plan.services[0];
        assert!(service.parameters.contains_key("source_url"));
        let db_step = plan
            .steps
            .iter()
            .find(|s| s.id == "create-service-lab-db")
            .expect("database step present");
        assert!(db_step.data_implications.iter().any(|i| i
            .recommended_action
            .as_deref()
            .is_some_and(|a| a.contains("pg_dump"))));
    }

    #[test]
    fn unpublished_database_produces_data_not_migrated_warning() {
        let importer = PortainerImporter::new();
        let plan = importer
            .generate_project_plan(project_snapshot(false))
            .unwrap();
        assert!(plan.services[0]
            .data_implications
            .iter()
            .any(|i| i.severity == DataImplicationSeverity::DataNotMigrated));
        assert!(!plan.summary.critical_warnings.is_empty());
        assert!(!plan.services[0].parameters.contains_key("source_url"));
    }

    #[test]
    fn plan_states_portainer_has_no_domains() {
        let importer = PortainerImporter::new();
        let plan = importer
            .generate_project_plan(project_snapshot(true))
            .unwrap();
        assert!(plan.domains.is_empty());
        assert!(plan
            .summary
            .manual_actions_required
            .iter()
            .any(|a| a.description.contains("DNS")));
    }

    #[test]
    fn plan_warns_the_first_deployment_needs_a_manual_trigger() {
        let importer = PortainerImporter::new();
        let plan = importer
            .generate_project_plan(project_snapshot(true))
            .unwrap();
        assert!(
            plan.summary
                .manual_actions_required
                .iter()
                .any(|a| a.description.contains("trigger the first deployment")),
            "Portainer's API has no repository metadata, so the plan must tell the \
             user the first deployment won't start automatically"
        );
    }

    /// Live end-to-end against a real Portainer instance. Skips unless
    /// PORTAINER_TEST_URL and PORTAINER_TEST_PASSWORD are set.
    #[tokio::test]
    async fn live_discover_describe_and_plan() {
        let (Ok(url), Ok(password)) = (
            std::env::var("PORTAINER_TEST_URL"),
            std::env::var("PORTAINER_TEST_PASSWORD"),
        ) else {
            println!("PORTAINER_TEST_URL/PORTAINER_TEST_PASSWORD not set, skipping live test");
            return;
        };
        let credentials = ImportCredentials::with_token_and_url(password, url);
        let importer = PortainerImporter::new();

        let validation = importer.validate_credentials(&credentials).await.unwrap();
        assert!(
            validation.valid,
            "credentials invalid: {:?}",
            validation.message
        );

        let discovered = importer
            .discover(&credentials, ImportSelector::default())
            .await
            .unwrap();
        assert!(!discovered.is_empty(), "expected workloads on the instance");
        let anchor = &discovered[0];

        let snapshot = importer
            .describe_project(&credentials, &anchor.id)
            .await
            .unwrap();
        let plan = importer.generate_project_plan(snapshot).unwrap();
        assert_eq!(plan.source, "portainer");
        assert!(!plan.steps.is_empty());
        println!(
            "live plan: {} — {} step(s), {} service(s)",
            plan.summary.headline,
            plan.steps.len(),
            plan.services.len()
        );
    }
}
