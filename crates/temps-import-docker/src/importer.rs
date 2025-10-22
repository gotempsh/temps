//! Docker workload importer implementation

use async_trait::async_trait;
use bollard::{
    models::ContainerInspectResponse, query_parameters::ListContainersOptions,
    secret::RestartPolicyNameEnum, Docker,
};
use std::{collections::HashMap, sync::Arc};
use temps_import_types::{
    CreatedResource, ImportContext, ImportOutcome, ImportPlan, ImportResult, ImportSelector,
    ImportServiceProvider, ImportSource, ImportValidationRule, ImporterCapabilities, NetworkInfo,
    NetworkMode, ResourceInfo, VolumeMount, WorkloadDescriptor, WorkloadId, WorkloadImporter,
    WorkloadSnapshot, WorkloadStatus, WorkloadType,
};
use tracing::{debug, info};

use crate::validation::DockerValidationRules;

/// Docker workload importer
pub struct DockerImporter {
    docker: Arc<Docker>,
    version: String,
}

impl DockerImporter {
    /// Create a new Docker importer
    pub fn new() -> ImportResult<Self> {
        let docker = Docker::connect_with_local_defaults()
            .map_err(|e| temps_import_types::ImportError::SourceNotAccessible(e.to_string()))?;

        Ok(Self {
            docker: Arc::new(docker),
            version: env!("CARGO_PKG_VERSION").to_string(),
        })
    }

    /// Create with a custom Docker client
    pub fn with_docker(docker: Docker) -> Self {
        Self {
            docker: Arc::new(docker),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

#[async_trait]
impl WorkloadImporter for DockerImporter {
    fn source(&self) -> ImportSource {
        ImportSource::Docker
    }

    fn name(&self) -> &str {
        "Docker Container Importer"
    }

    fn version(&self) -> &str {
        &self.version
    }

    async fn health_check(&self) -> ImportResult<bool> {
        debug!("Performing Docker health check");

        match self.docker.ping().await {
            Ok(_) => {
                info!("Docker is accessible");
                Ok(true)
            }
            Err(e) => {
                debug!("Docker health check failed: {}", e);
                Ok(false)
            }
        }
    }

    async fn discover(&self, selector: ImportSelector) -> ImportResult<Vec<WorkloadDescriptor>> {
        debug!(
            "Discovering Docker containers with selector: {:?}",
            selector
        );

        let mut filters = HashMap::new();

        // Apply status filter (default to running containers)
        if let Some(status_filters) = &selector.status_filter {
            filters.insert("status", status_filters.clone());
        } else {
            filters.insert("status", vec!["running".to_string()]);
        }

        // Apply label filter
        if let Some(label_filter) = &selector.label_filter {
            let label_strings: Vec<String> = label_filter
                .iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect();
            filters.insert("label", label_strings);
        }

        // Convert filters to use owned Strings
        let owned_filters: HashMap<String, Vec<String>> = filters
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();

        let options = ListContainersOptions {
            all: true,
            filters: Some(owned_filters),
            ..Default::default()
        };

        let containers = self
            .docker
            .list_containers(Some(options))
            .await
            .map_err(|e| temps_import_types::ImportError::DiscoveryFailed(e.to_string()))?;

        let mut descriptors = Vec::new();

        for container in containers {
            let id = container.id.ok_or_else(|| {
                temps_import_types::ImportError::Internal("Container missing ID".to_string())
            })?;

            // Extract names once
            let names = container.names.unwrap_or_default();

            // Apply name pattern filter if specified
            if let Some(pattern) = &selector.name_pattern {
                let matches = names
                    .iter()
                    .any(|name| name.trim_start_matches('/').contains(pattern));
                if !matches {
                    continue;
                }
            }

            let name = names.first().map(|n| n.trim_start_matches('/').to_string());

            use bollard::models::ContainerSummaryStateEnum;
            let status = match &container.state {
                Some(ContainerSummaryStateEnum::RUNNING) => WorkloadStatus::Running,
                Some(ContainerSummaryStateEnum::PAUSED) => WorkloadStatus::Paused,
                Some(ContainerSummaryStateEnum::EXITED) => WorkloadStatus::Exited,
                Some(ContainerSummaryStateEnum::DEAD) => WorkloadStatus::Failed,
                _ => WorkloadStatus::Unknown,
            };

            let labels = container.labels.unwrap_or_default();

            descriptors.push(WorkloadDescriptor {
                id: WorkloadId::new(id),
                name,
                workload_type: WorkloadType::Container,
                status,
                image: container.image,
                created_at: container
                    .created
                    .map(|ts| chrono::DateTime::from_timestamp(ts, 0).unwrap_or_default()),
                labels,
            });

            // Apply limit if specified
            if let Some(limit) = selector.limit {
                if descriptors.len() >= limit {
                    break;
                }
            }
        }

        info!(
            "Docker discovery completed: {} containers found",
            descriptors.len()
        );
        Ok(descriptors)
    }

    async fn describe(&self, workload_id: &WorkloadId) -> ImportResult<WorkloadSnapshot> {
        debug!("Describing Docker container: {}", workload_id);

        let inspect = self
            .docker
            .inspect_container(
                workload_id.as_str(),
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .map_err(|e| {
                if e.to_string().contains("No such container") {
                    temps_import_types::ImportError::ContainerNotFound(workload_id.to_string())
                } else {
                    temps_import_types::ImportError::InspectionFailed(e.to_string())
                }
            })?;

        Ok(DockerImporter::convert_to_snapshot(
            workload_id.clone(),
            inspect,
        )?)
    }

    fn generate_plan(&self, snapshot: WorkloadSnapshot) -> ImportResult<ImportPlan> {
        debug!(
            "Generating import plan for Docker container: {}",
            snapshot.id
        );

        use temps_import_types::plan::{
            DeploymentConfiguration, DeploymentStrategy, EnvironmentConfiguration,
            EnvironmentVariable, HealthCheckConfiguration, NetworkConfiguration, PlanComplexity,
            PlanMetadata, PortMapping, ProjectConfiguration, ProjectType, Protocol, ResourceLimits,
            VolumeMount as PlanVolumeMount, VolumeType,
        };

        // Generate project configuration
        let container_name = snapshot.name.clone().unwrap_or_else(|| {
            let id_str = snapshot.id.as_str();
            let prefix_len = std::cmp::min(8, id_str.len());
            format!("imported-{}", &id_str[..prefix_len])
        });

        // Sanitize name to create a slug
        let slug = container_name
            .to_lowercase()
            .replace(|c: char| !c.is_alphanumeric() && c != '-', "-")
            .trim_matches('-')
            .to_string();

        let project = ProjectConfiguration {
            name: container_name.clone(),
            slug: slug.clone(),
            project_type: ProjectType::Docker,
            is_web_app: !snapshot.ports.is_empty(), // Consider it a web app if it has ports
        };

        // Generate environment configuration
        let environment = EnvironmentConfiguration {
            name: "production".to_string(),
            subdomain: slug.clone(),
            resources: ResourceLimits {
                cpu_limit: snapshot.resources.cpu_limit.map(|c| (c * 1000.0) as i32), // Convert to millicores
                memory_limit: snapshot
                    .resources
                    .memory_limit
                    .map(|m| (m / 1_048_576) as i32), // Convert bytes to MB
                cpu_request: snapshot
                    .resources
                    .cpu_shares
                    .map(|s| (s / 1024 * 1000) as i32), // Convert shares to millicores (approximate)
                memory_request: snapshot
                    .resources
                    .memory_reservation
                    .map(|m| (m / 1_048_576) as i32), // Convert bytes to MB
            },
        };

        // Convert environment variables
        let env_vars: Vec<EnvironmentVariable> = snapshot
            .env
            .iter()
            .map(|(k, v)| {
                // Heuristic: treat variables with "SECRET", "PASSWORD", "TOKEN", "KEY" as secrets
                let is_secret = k.to_uppercase().contains("SECRET")
                    || k.to_uppercase().contains("PASSWORD")
                    || k.to_uppercase().contains("TOKEN")
                    || k.to_uppercase().contains("KEY")
                    || k.to_uppercase().contains("API_KEY");

                EnvironmentVariable {
                    key: k.clone(),
                    value: if is_secret {
                        "***REDACTED***".to_string()
                    } else {
                        v.clone()
                    },
                    is_secret,
                }
            })
            .collect();

        // Convert port mappings
        let mut ports: Vec<PortMapping> = snapshot
            .ports
            .iter()
            .map(|(container_port, host_port)| PortMapping {
                container_port: *container_port,
                host_port: *host_port,
                protocol: Protocol::Tcp, // Default to TCP
                is_primary: false,       // Will set primary below
            })
            .collect();

        // Mark the first HTTP-like port as primary (80, 8080, 3000, etc.)
        if let Some(primary_port) = ports
            .iter_mut()
            .find(|p| matches!(p.container_port, 80 | 443 | 8080 | 8000 | 3000 | 5000))
        {
            primary_port.is_primary = true;
        } else if let Some(first_port) = ports.first_mut() {
            // If no common HTTP port, mark the first one as primary
            first_port.is_primary = true;
        }

        // Convert volume mounts
        let volumes: Vec<PlanVolumeMount> = snapshot
            .volumes
            .iter()
            .map(|v| {
                use temps_import_types::VolumeType as SnapshotVolumeType;
                let volume_type = match v.volume_type {
                    SnapshotVolumeType::Bind => VolumeType::Bind,
                    SnapshotVolumeType::Volume => VolumeType::Volume,
                    SnapshotVolumeType::Tmpfs => VolumeType::Tmpfs,
                };
                PlanVolumeMount {
                    source: v.source.clone(),
                    destination: v.destination.clone(),
                    read_only: v.read_only,
                    volume_type,
                }
            })
            .collect();

        // Network configuration
        let network = NetworkConfiguration {
            mode: snapshot.network.mode.clone(),
            hostname: snapshot.network.hostname.clone(),
            dns_servers: vec![], // Docker doesn't expose custom DNS easily
        };

        // Health check configuration
        let health_check = snapshot.health_check.as_ref().and_then(|hc| {
            // Try to extract health check info from Docker's health check
            // This is a best-effort conversion
            if let Some(obj) = hc.as_object() {
                let test = obj.get("Test").and_then(|t| t.as_array());
                let interval = obj
                    .get("Interval")
                    .and_then(|i| i.as_i64())
                    .map(|i| (i / 1_000_000_000) as u32); // nanoseconds to seconds
                let timeout = obj
                    .get("Timeout")
                    .and_then(|t| t.as_i64())
                    .map(|t| (t / 1_000_000_000) as u32);
                let retries = obj
                    .get("Retries")
                    .and_then(|r| r.as_u64())
                    .map(|r| r as u32);

                // Try to extract HTTP path from test command
                let http_path = test.and_then(|test_arr| {
                    test_arr.iter().find_map(|item| {
                        item.as_str().and_then(|s| {
                            if s.contains("curl") || s.contains("wget") {
                                // Extract path from curl/wget command
                                s.split_whitespace()
                                    .find(|part| {
                                        part.starts_with("http://") || part.starts_with('/')
                                    })
                                    .map(|url| {
                                        if let Some(path) = url.split("://").nth(1) {
                                            path.split_once(':')
                                                .map(|(_, p)| p.to_string())
                                                .unwrap_or_else(|| path.to_string())
                                        } else {
                                            url.to_string()
                                        }
                                    })
                            } else {
                                None
                            }
                        })
                    })
                });

                // Use first port as health check port
                ports
                    .iter()
                    .find(|p| p.is_primary)
                    .map(|p| HealthCheckConfiguration {
                        http_path: http_path.or_else(|| Some("/".to_string())),
                        port: p.container_port,
                        interval: interval.unwrap_or(30),
                        timeout: timeout.unwrap_or(5),
                        retries: retries.unwrap_or(3),
                    })
            } else {
                None
            }
        });

        // Build deployment configuration
        let deployment = DeploymentConfiguration {
            image: snapshot.image.clone().unwrap_or_default(),
            build: None, // Docker containers are pre-built
            strategy: DeploymentStrategy::Replace,
            env_vars,
            ports,
            volumes,
            network,
            resources: environment.resources.clone(),
            command: snapshot.command.clone(),
            entrypoint: snapshot.entrypoint.clone(),
            working_dir: snapshot.working_dir.clone(),
            health_check,
        };

        // Calculate plan complexity
        let mut warnings = Vec::new();
        let mut complexity_score = 0;

        if deployment.volumes.len() > 3 {
            complexity_score += 1;
            warnings.push(format!(
                "Container has {} volume mounts",
                deployment.volumes.len()
            ));
        }

        if deployment.ports.len() > 5 {
            complexity_score += 1;
            warnings.push(format!(
                "Container exposes {} ports",
                deployment.ports.len()
            ));
        }

        if deployment.env_vars.iter().any(|e| e.is_secret) {
            warnings
                .push("Container uses environment variables that appear to be secrets".to_string());
        }

        if matches!(snapshot.network.mode, NetworkMode::Host) {
            complexity_score += 2;
            warnings.push("Container uses host networking mode".to_string());
        }

        if deployment
            .volumes
            .iter()
            .any(|v| v.volume_type == VolumeType::Bind)
        {
            complexity_score += 1;
            warnings.push("Container has bind mounts that may not be available".to_string());
        }

        let complexity = match complexity_score {
            0..=1 => PlanComplexity::Low,
            2..=3 => PlanComplexity::Medium,
            _ => PlanComplexity::High,
        };

        // Build plan metadata
        let metadata = PlanMetadata {
            generated_at: chrono::Utc::now(),
            generator_version: self.version.clone(),
            complexity,
            warnings,
        };

        Ok(ImportPlan {
            version: "1.0".to_string(),
            source: "docker".to_string(),
            source_container_id: snapshot.id.to_string(),
            project,
            environment,
            deployment,
            metadata,
        })
    }

    fn validation_rules(&self) -> Vec<Box<dyn ImportValidationRule>> {
        DockerValidationRules::all_rules()
    }

    async fn execute(
        &self,
        context: ImportContext,
        plan: ImportPlan,
        services: &dyn ImportServiceProvider,
    ) -> ImportResult<ImportOutcome> {
        use sea_orm::{
            ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, PaginatorTrait,
            QueryFilter,
        };
        use std::time::Instant;
        use temps_entities::{deployment_containers, deployments, environments, projects};

        let start_time = Instant::now();
        info!(
            "Executing Docker import for session: {} (dry_run: {})",
            context.session_id, context.dry_run
        );

        if context.dry_run {
            info!(
                "Dry run mode - would create project '{}' with environment '{}' and deploy image '{}'",
                context.project_name, plan.environment.name, plan.deployment.image
            );

            return Ok(ImportOutcome {
                session_id: context.session_id.clone(),
                success: true,
                project_id: None,
                environment_id: None,
                deployment_id: None,
                warnings: vec![],
                errors: vec![],
                created_resources: vec![],
                duration_seconds: start_time.elapsed().as_secs_f64(),
            });
        }

        let mut created_resources = Vec::new();
        let mut warnings = Vec::new();

        // Get services from provider
        let db = services.db();
        let project_service = services
            .project_service()
            .downcast_ref::<temps_projects::ProjectService>()
            .ok_or_else(|| {
                temps_import_types::ImportError::Internal(
                    "Failed to get ProjectService".to_string(),
                )
            })?;

        // Create project from plan
        info!(
            "Creating project '{}' from Docker import with preset '{}'",
            context.project_name, context.preset
        );

        // Get project type from preset
        use temps_presets::get_preset_by_slug;
        let project_type = match get_preset_by_slug(&context.preset) {
            Some(preset_val) => preset_val.project_type().to_string(),
            None => {
                warnings.push(format!(
                    "Preset '{}' not found, defaulting to 'server' project type",
                    context.preset
                ));
                "server".to_string()
            }
        };

        let create_project_request = temps_projects::services::types::CreateProjectRequest {
            name: context.project_name.clone(),
            repo_name: context.repo_name.clone(),
            repo_owner: context.repo_owner.clone(),
            directory: context.directory.clone(),
            main_branch: context.main_branch.clone(),
            preset: context.preset.clone(),
            output_dir: None,
            build_command: None,
            install_command: None,
            environment_variables: Some(
                plan.deployment
                    .env_vars
                    .iter()
                    .filter(|ev| !ev.is_secret)
                    .map(|ev| (ev.key.clone(), ev.value.clone()))
                    .collect(),
            ),
            automatic_deploy: false,
            project_type: Some(project_type),
            is_web_app: plan.project.is_web_app,
            performance_metrics_enabled: false,
            storage_service_ids: vec![],
            use_default_wildcard: Some(true),
            custom_domain: None,
            is_public_repo: None,
            git_url: None,
            git_provider_connection_id: context.git_provider_connection_id,
            is_on_demand: Some(false),
        };

        let project = project_service
            .create_project(create_project_request)
            .await
            .map_err(|e| {
                temps_import_types::ImportError::ExecutionFailed(format!(
                    "Failed to create project: {}",
                    e
                ))
            })?;

        created_resources.push(CreatedResource {
            resource_type: "project".to_string(),
            resource_id: project.id,
            resource_name: project.name.clone(),
        });

        info!(
            "✓ Created project {} (id: {}) from Docker import",
            project.name, project.id
        );

        // Get the production environment (created automatically with project)
        let environment = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project.id))
            .filter(environments::Column::Name.eq(&plan.environment.name))
            .one(db)
            .await
            .map_err(|e| {
                temps_import_types::ImportError::ExecutionFailed(format!(
                    "Failed to find environment: {}",
                    e
                ))
            })?
            .ok_or_else(|| {
                temps_import_types::ImportError::ExecutionFailed(format!(
                    "Environment '{}' not found for project {}",
                    plan.environment.name, project.id
                ))
            })?;

        created_resources.push(CreatedResource {
            resource_type: "environment".to_string(),
            resource_id: environment.id,
            resource_name: environment.name.clone(),
        });

        info!(
            "✓ Found environment '{}' (id: {}) for project {}",
            environment.name, environment.id, project.id
        );

        // Count existing deployments to generate sequential deployment number
        let deployment_count = deployments::Entity::find()
            .filter(deployments::Column::ProjectId.eq(project.id))
            .paginate(db, 100)
            .num_items()
            .await
            .map_err(|e| {
                temps_import_types::ImportError::ExecutionFailed(format!(
                    "Failed to count deployments: {}",
                    e
                ))
            })?;

        let deployment_number = deployment_count + 1;
        let deployment_slug = format!("{}-{}", project.slug, deployment_number);
        let deployment_metadata = serde_json::json!({
            "import_source": "docker",
            "imported_at": chrono::Utc::now().to_rfc3339(),
            "image": plan.deployment.image.clone(),
            "ports": plan.deployment.ports.clone(),
            "volumes": plan.deployment.volumes.clone(),
            "env_vars_count": plan.deployment.env_vars.len(),
        });

        let now = chrono::Utc::now();
        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set(deployment_slug.clone()),
            state: Set("completed".to_string()),
            metadata: Set(deployment_metadata),
            image_name: Set(Some(plan.deployment.image.clone())),
            commit_message: Set(Some("Imported from Docker container".to_string())),
            deploying_at: Set(Some(now)),
            ready_at: Set(Some(now)),
            started_at: Set(Some(now)),
            finished_at: Set(Some(now)),
            ..Default::default()
        };

        let deployment = deployment.insert(db).await.map_err(|e| {
            temps_import_types::ImportError::ExecutionFailed(format!(
                "Failed to create deployment: {}",
                e
            ))
        })?;

        created_resources.push(CreatedResource {
            resource_type: "deployment".to_string(),
            resource_id: deployment.id,
            resource_name: deployment.slug.clone(),
        });

        info!(
            "✓ Created deployment {} (id: {}) for imported project {} from Docker image '{}'",
            deployment.slug, deployment.id, project.name, plan.deployment.image
        );

        // Create deployment_container records for each port mapping
        for (idx, port_mapping) in plan.deployment.ports.iter().enumerate() {
            let container_name = if idx == 0 {
                format!("{}-{}", project.name, deployment.slug)
            } else {
                format!("{}-{}-{}", project.name, deployment.slug, idx)
            };

            let deployment_container = deployment_containers::ActiveModel {
                deployment_id: Set(deployment.id),
                container_id: Set(plan.source_container_id.clone()),
                container_name: Set(container_name.clone()),
                container_port: Set(port_mapping.container_port as i32),
                host_port: Set(port_mapping.host_port.map(|p| p as i32)),
                image_name: Set(Some(plan.deployment.image.clone())),
                status: Set(Some("running".to_string())),
                deployed_at: Set(now),
                ready_at: Set(Some(now)),
                ..Default::default()
            };

            deployment_container.insert(db).await.map_err(|e| {
                temps_import_types::ImportError::ExecutionFailed(format!(
                    "Failed to create deployment container for port {}: {}",
                    port_mapping.container_port, e
                ))
            })?;

            info!(
                "✓ Created deployment_container '{}' for port {}",
                container_name, port_mapping.container_port
            );
        }

        // Update environment to set current deployment
        let mut environment_active: environments::ActiveModel = environment.clone().into();
        environment_active.current_deployment_id = Set(Some(deployment.id));
        environment_active.last_deployment = Set(Some(now));

        environment_active.update(db).await.map_err(|e| {
            temps_import_types::ImportError::ExecutionFailed(format!(
                "Failed to update environment with current deployment: {}",
                e
            ))
        })?;

        info!(
            "✓ Set deployment {} as current deployment for environment {}",
            deployment.id, environment.name
        );

        // Update project to set last deployment timestamp
        let project_entity = projects::Entity::find_by_id(project.id)
            .one(db)
            .await
            .map_err(|e| {
                temps_import_types::ImportError::ExecutionFailed(format!(
                    "Failed to fetch project entity: {}",
                    e
                ))
            })?
            .ok_or_else(|| {
                temps_import_types::ImportError::ExecutionFailed(format!(
                    "Project {} not found after creation",
                    project.id
                ))
            })?;

        let mut project_active: projects::ActiveModel = project_entity.into();
        project_active.last_deployment = Set(Some(now));

        project_active.update(db).await.map_err(|e| {
            temps_import_types::ImportError::ExecutionFailed(format!(
                "Failed to update project with last deployment: {}",
                e
            ))
        })?;

        info!(
            "✓ Set last_deployment timestamp on project {}",
            context.project_name
        );

        let duration = start_time.elapsed().as_secs_f64();
        info!(
            "✅ Docker import completed successfully in {:.2}s - created project {} (id: {}), environment {} (id: {}), deployment {} (id: {})",
            duration, project.name, project.id, environment.name, environment.id, deployment.slug, deployment.id
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
            duration_seconds: duration,
        })
    }

    fn capabilities(&self) -> ImporterCapabilities {
        ImporterCapabilities {
            supports_volumes: true,
            supports_networks: true,
            supports_health_checks: true,
            supports_resource_limits: true,
            supports_build: false,  // Docker containers are pre-built
            supports_stacks: false, // Phase 1: single containers only
        }
    }
}

impl DockerImporter {
    /// Convert Docker inspect response to WorkloadSnapshot
    fn convert_to_snapshot(
        id: WorkloadId,
        inspect: ContainerInspectResponse,
    ) -> ImportResult<WorkloadSnapshot> {
        let config = inspect.config.as_ref().ok_or_else(|| {
            temps_import_types::ImportError::Internal("Container missing config".to_string())
        })?;

        let state = inspect.state.as_ref().ok_or_else(|| {
            temps_import_types::ImportError::Internal("Container missing state".to_string())
        })?;

        // Extract container name (remove leading /)
        let name = inspect
            .name
            .as_ref()
            .map(|n| n.trim_start_matches('/').to_string());

        // Parse status
        let status = if state.running.unwrap_or(false) {
            WorkloadStatus::Running
        } else if state.paused.unwrap_or(false) {
            WorkloadStatus::Paused
        } else if state.dead.unwrap_or(false) {
            WorkloadStatus::Failed
        } else {
            WorkloadStatus::Stopped
        };

        // Extract environment variables
        let mut env = HashMap::new();
        if let Some(env_vec) = &config.env {
            for entry in env_vec {
                if let Some((key, value)) = entry.split_once('=') {
                    env.insert(key.to_string(), value.to_string());
                }
            }
        }

        // Extract port mappings
        let mut ports = HashMap::new();
        if let Some(exposed_ports) = &config.exposed_ports {
            for port_spec in exposed_ports.keys() {
                if let Some(port_str) = port_spec.split('/').next() {
                    if let Ok(port) = port_str.parse::<u16>() {
                        ports.insert(port, None); // No host port mapping yet
                    }
                }
            }
        }

        // Get host port mappings from HostConfig
        if let Some(host_config) = &inspect.host_config {
            if let Some(port_bindings) = &host_config.port_bindings {
                for (container_port, bindings) in port_bindings {
                    if let Some(port_str) = container_port.split('/').next() {
                        if let Ok(port) = port_str.parse::<u16>() {
                            let host_port = bindings
                                .as_ref()
                                .and_then(|b| b.first().cloned())
                                .and_then(|binding| binding.host_port.clone())
                                .and_then(|hp| hp.parse::<u16>().ok());
                            ports.insert(port, host_port);
                        }
                    }
                }
            }
        }

        // Extract volume mounts
        let volumes = inspect
            .mounts
            .as_ref()
            .map(|mounts| {
                mounts
                    .iter()
                    .map(|mount| {
                        use bollard::models::MountPointTypeEnum;
                        use temps_import_types::VolumeType;
                        let volume_type = match &mount.typ {
                            Some(MountPointTypeEnum::BIND) => VolumeType::Bind,
                            Some(MountPointTypeEnum::VOLUME) => VolumeType::Volume,
                            Some(MountPointTypeEnum::TMPFS) => VolumeType::Tmpfs,
                            _ => VolumeType::Volume, // Default to volume
                        };
                        VolumeMount {
                            source: mount.source.clone().unwrap_or_default(),
                            destination: mount.destination.clone().unwrap_or_default(),
                            read_only: mount.rw.map(|rw| !rw).unwrap_or(false),
                            volume_type,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Network info
        let network_mode = inspect
            .host_config
            .as_ref()
            .and_then(|hc| hc.network_mode.clone())
            .unwrap_or_else(|| "default".to_string());

        let network = NetworkInfo {
            mode: match network_mode.as_str() {
                "bridge" => NetworkMode::Bridge,
                "host" => NetworkMode::Host,
                "none" => NetworkMode::None,
                _ => NetworkMode::Custom(network_mode),
            },
            networks: inspect
                .network_settings
                .as_ref()
                .and_then(|ns| ns.networks.as_ref())
                .map(|networks| networks.keys().cloned().collect())
                .unwrap_or_default(),
            hostname: config.hostname.clone(),
            domain_name: config.domainname.clone(),
        };

        // Resource limits
        let resources = inspect
            .host_config
            .as_ref()
            .map(|hc| ResourceInfo {
                cpu_limit: hc.nano_cpus.map(|n| (n as f64) / 1_000_000_000.0),
                memory_limit: hc.memory,
                memory_reservation: hc.memory_reservation,
                cpu_shares: hc.cpu_shares,
            })
            .unwrap_or_default();

        // Extract restart policy from host config
        let restart_policy = inspect
            .host_config
            .as_ref()
            .and_then(|hc| hc.restart_policy.as_ref())
            .and_then(|rp| rp.name.as_ref())
            .and_then(|name| match name {
                RestartPolicyNameEnum::NO | RestartPolicyNameEnum::EMPTY => {
                    Some(temps_import_types::RestartPolicy::No)
                }
                RestartPolicyNameEnum::ALWAYS => Some(temps_import_types::RestartPolicy::Always),
                RestartPolicyNameEnum::ON_FAILURE => {
                    Some(temps_import_types::RestartPolicy::OnFailure)
                }
                RestartPolicyNameEnum::UNLESS_STOPPED => {
                    Some(temps_import_types::RestartPolicy::UnlessStopped)
                }
            });

        Ok(WorkloadSnapshot {
            id,
            name,
            workload_type: WorkloadType::Container,
            status,
            image: config.image.clone(),
            command: config.cmd.clone(),
            entrypoint: config.entrypoint.clone(),
            working_dir: config.working_dir.clone(),
            env,
            ports,
            volumes,
            network,
            resources,
            labels: config.labels.clone().unwrap_or_default(),
            health_check: config
                .healthcheck
                .as_ref()
                .map(|hc| serde_json::to_value(hc).unwrap_or_default()),
            restart_policy,
            created_at: inspect.created.unwrap_or_else(chrono::Utc::now),
            source_metadata: serde_json::to_value(&inspect).unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use temps_import_types::{NetworkMode, ResourceInfo, VolumeType, WorkloadStatus};

    fn create_test_snapshot() -> WorkloadSnapshot {
        let mut env = HashMap::new();
        env.insert("NODE_ENV".to_string(), "production".to_string());
        env.insert("API_KEY".to_string(), "secret123".to_string());
        env.insert(
            "DATABASE_URL".to_string(),
            "postgres://localhost".to_string(),
        );

        let mut ports = HashMap::new();
        ports.insert(3000, Some(3000));
        ports.insert(8080, None);

        let volumes = vec![
            temps_import_types::VolumeMount {
                source: "/host/data".to_string(),
                destination: "/app/data".to_string(),
                read_only: false,
                volume_type: VolumeType::Bind,
            },
            temps_import_types::VolumeMount {
                source: "app-cache".to_string(),
                destination: "/app/cache".to_string(),
                read_only: false,
                volume_type: VolumeType::Volume,
            },
        ];

        WorkloadSnapshot {
            id: WorkloadId::new("abc123".to_string()),
            name: Some("my-web-app".to_string()),
            workload_type: WorkloadType::Container,
            status: WorkloadStatus::Running,
            image: Some("nginx:latest".to_string()),
            command: Some(vec!["npm".to_string(), "start".to_string()]),
            entrypoint: Some(vec!["/bin/sh".to_string()]),
            working_dir: Some("/app".to_string()),
            env,
            ports,
            volumes,
            network: NetworkInfo {
                mode: NetworkMode::Bridge,
                networks: vec!["bridge".to_string()],
                hostname: Some("my-web-app".to_string()),
                domain_name: None,
            },
            resources: ResourceInfo {
                cpu_limit: Some(2.0),
                memory_limit: Some(2_147_483_648), // 2GB in bytes
                memory_reservation: Some(1_073_741_824), // 1GB in bytes
                cpu_shares: Some(1024),
            },
            labels: HashMap::new(),
            health_check: None,
            restart_policy: None,
            created_at: chrono::Utc::now(),
            source_metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn test_generate_plan_creates_valid_plan() {
        // Arrange
        let importer = DockerImporter {
            docker: Arc::new(Docker::connect_with_local_defaults().unwrap()),
            version: "1.0.0".to_string(),
        };
        let snapshot = create_test_snapshot();

        // Act
        let result = importer.generate_plan(snapshot.clone());

        // Assert
        assert!(result.is_ok(), "generate_plan should succeed");
        let plan = result.unwrap();

        // Verify plan structure
        assert_eq!(plan.version, "1.0");
        assert_eq!(plan.source, "docker");
        assert_eq!(plan.source_container_id, "abc123");

        // Verify project configuration
        assert_eq!(plan.project.name, "my-web-app");
        assert_eq!(plan.project.slug, "my-web-app");
        assert_eq!(
            plan.project.project_type,
            temps_import_types::plan::ProjectType::Docker
        );
        assert!(
            plan.project.is_web_app,
            "Should be detected as web app due to ports"
        );

        // Verify environment configuration
        assert_eq!(plan.environment.name, "production");
        assert_eq!(plan.environment.subdomain, "my-web-app");
        assert_eq!(plan.environment.resources.cpu_limit, Some(2000)); // 2.0 cores = 2000 millicores
        assert_eq!(plan.environment.resources.memory_limit, Some(2048)); // 2GB = 2048 MB

        // Verify deployment configuration
        assert_eq!(plan.deployment.image, "nginx:latest");
        assert_eq!(
            plan.deployment.command,
            Some(vec!["npm".to_string(), "start".to_string()])
        );
        assert_eq!(
            plan.deployment.entrypoint,
            Some(vec!["/bin/sh".to_string()])
        );
        assert_eq!(plan.deployment.working_dir, Some("/app".to_string()));

        // Verify environment variables
        assert_eq!(plan.deployment.env_vars.len(), 3);
        let api_key_var = plan
            .deployment
            .env_vars
            .iter()
            .find(|e| e.key == "API_KEY")
            .unwrap();
        assert!(
            api_key_var.is_secret,
            "API_KEY should be detected as secret"
        );
        assert_eq!(
            api_key_var.value, "***REDACTED***",
            "Secret should be redacted"
        );

        let node_env_var = plan
            .deployment
            .env_vars
            .iter()
            .find(|e| e.key == "NODE_ENV")
            .unwrap();
        assert!(!node_env_var.is_secret, "NODE_ENV should not be a secret");
        assert_eq!(node_env_var.value, "production");

        // Verify ports
        assert_eq!(plan.deployment.ports.len(), 2);
        let primary_port = plan.deployment.ports.iter().find(|p| p.is_primary).unwrap();
        // Should mark one of the common HTTP ports as primary
        assert!(
            primary_port.container_port == 3000 || primary_port.container_port == 8080,
            "Should mark a port as primary, got {}",
            primary_port.container_port
        );

        // Verify volumes
        assert_eq!(plan.deployment.volumes.len(), 2);
        let bind_mount = plan
            .deployment
            .volumes
            .iter()
            .find(|v| v.source == "/host/data")
            .unwrap();
        assert_eq!(
            bind_mount.volume_type,
            temps_import_types::plan::VolumeType::Bind
        );
        assert_eq!(bind_mount.destination, "/app/data");

        // Verify metadata
        assert_eq!(plan.metadata.generator_version, "1.0.0");
        assert!(
            plan.metadata.warnings.len() > 0,
            "Should have warnings for bind mount"
        );
    }

    #[test]
    fn test_generate_plan_detects_complexity() {
        // Arrange
        let importer = DockerImporter {
            docker: Arc::new(Docker::connect_with_local_defaults().unwrap()),
            version: "1.0.0".to_string(),
        };
        let mut snapshot = create_test_snapshot();

        // Add many volumes to increase complexity
        for i in 0..5 {
            snapshot.volumes.push(temps_import_types::VolumeMount {
                source: format!("vol-{}", i),
                destination: format!("/data/{}", i),
                read_only: false,
                volume_type: VolumeType::Volume,
            });
        }

        // Act
        let result = importer.generate_plan(snapshot);

        // Assert
        assert!(result.is_ok());
        let plan = result.unwrap();
        assert!(
            plan.metadata.complexity == temps_import_types::plan::PlanComplexity::Medium
                || plan.metadata.complexity == temps_import_types::plan::PlanComplexity::High,
            "Should detect medium or high complexity due to many volumes"
        );
    }

    #[test]
    fn test_generate_plan_sanitizes_container_name() {
        // Arrange
        let importer = DockerImporter {
            docker: Arc::new(Docker::connect_with_local_defaults().unwrap()),
            version: "1.0.0".to_string(),
        };
        let mut snapshot = create_test_snapshot();
        snapshot.name = Some("My_Web@App#123!".to_string());

        // Act
        let result = importer.generate_plan(snapshot);

        // Assert
        assert!(result.is_ok());
        let plan = result.unwrap();
        assert_eq!(
            plan.project.slug, "my-web-app-123",
            "Should sanitize special characters"
        );
    }

    #[test]
    fn test_generate_plan_handles_no_name() {
        // Arrange
        let importer = DockerImporter {
            docker: Arc::new(Docker::connect_with_local_defaults().unwrap()),
            version: "1.0.0".to_string(),
        };
        let mut snapshot = create_test_snapshot();
        snapshot.name = None;

        // Act
        let result = importer.generate_plan(snapshot);

        // Assert
        assert!(result.is_ok());
        let plan = result.unwrap();
        assert!(
            plan.project.name.starts_with("imported-"),
            "Should generate name for unnamed containers"
        );
        assert!(
            plan.project.slug.starts_with("imported-"),
            "Should generate slug for unnamed containers"
        );
    }

    #[tokio::test]
    async fn test_execute_dry_run_returns_success_without_creating_resources() {
        // Arrange
        let importer = create_test_importer();
        let plan = create_test_plan();
        let context = temps_import_types::ImportContext {
            session_id: "test-session".to_string(),
            user_id: 1,
            dry_run: true,
            project_name: "test-project".to_string(),
            preset: "nodejs".to_string(),
            directory: ".".to_string(),
            main_branch: "main".to_string(),
            git_provider_connection_id: None,
            repo_owner: None,
            repo_name: None,
            metadata: std::collections::HashMap::new(),
        };

        // Create mock service provider
        let mock_services = create_mock_service_provider();

        // Act
        let result = importer.execute(context, plan, &mock_services).await;

        // Assert
        assert!(result.is_ok(), "Dry run should succeed");
        let outcome = result.unwrap();
        assert!(outcome.success, "Dry run should be marked as success");
        assert!(
            outcome.project_id.is_none(),
            "Dry run should not create project"
        );
        assert!(
            outcome.environment_id.is_none(),
            "Dry run should not create environment"
        );
        assert!(
            outcome.deployment_id.is_none(),
            "Dry run should not create deployment"
        );
        assert_eq!(outcome.session_id, "test-session");
    }

    #[tokio::test]
    #[ignore] // Requires Docker - run with: cargo test -- --ignored
    async fn test_execute_with_valid_plan_creates_all_resources() {
        // Arrange
        let importer = create_test_importer();
        let mut plan = create_test_plan();
        plan.deployment.image = "nginx:latest".to_string();
        plan.deployment.ports = vec![temps_import_types::PortMapping {
            container_port: 80,
            host_port: Some(8080),
            protocol: temps_import_types::plan::Protocol::Tcp,
            is_primary: true,
        }];

        let context = temps_import_types::ImportContext {
            session_id: "test-session-2".to_string(),
            user_id: 1,
            dry_run: false,
            project_name: "nginx-import".to_string(),
            preset: "docker".to_string(),
            directory: ".".to_string(),
            main_branch: "main".to_string(),
            git_provider_connection_id: None,
            repo_owner: None,
            repo_name: None,
            metadata: std::collections::HashMap::new(),
        };

        // Create mock service provider with real database
        let db = create_test_database().await;
        let mock_services = MockServiceProvider { db };

        // Act
        let result = importer.execute(context, plan, &mock_services).await;

        // Assert
        assert!(result.is_ok(), "Execute should succeed with valid plan");
        let outcome = result.unwrap();
        assert!(outcome.success, "Execution should be marked as success");
        assert!(outcome.project_id.is_some(), "Should create project");
        assert!(
            outcome.environment_id.is_some(),
            "Should create environment"
        );
        assert!(outcome.deployment_id.is_some(), "Should create deployment");
        assert!(outcome.duration_seconds > 0.0, "Should track duration");
    }

    #[tokio::test]
    #[ignore] // Requires Docker - run with: cargo test -- --ignored
    async fn test_execute_tracks_created_resources() {
        // Arrange
        let importer = create_test_importer();
        let plan = create_test_plan();
        let context = temps_import_types::ImportContext {
            session_id: "test-session-3".to_string(),
            user_id: 1,
            dry_run: false,
            project_name: "tracked-project".to_string(),
            preset: "docker".to_string(),
            directory: ".".to_string(),
            main_branch: "main".to_string(),
            git_provider_connection_id: None,
            repo_owner: None,
            repo_name: None,
            metadata: std::collections::HashMap::new(),
        };

        let db = create_test_database().await;
        let mock_services = MockServiceProvider { db };

        // Act
        let result = importer.execute(context, plan, &mock_services).await;

        // Assert
        assert!(result.is_ok());
        let outcome = result.unwrap();
        assert!(
            !outcome.created_resources.is_empty(),
            "Should track created resources"
        );

        // Verify resource types
        let resource_types: Vec<String> = outcome
            .created_resources
            .iter()
            .map(|r| r.resource_type.clone())
            .collect();
        assert!(
            resource_types.contains(&"project".to_string()),
            "Should track project creation"
        );
        assert!(
            resource_types.contains(&"environment".to_string()),
            "Should track environment creation"
        );
        assert!(
            resource_types.contains(&"deployment".to_string()),
            "Should track deployment creation"
        );
    }

    #[tokio::test]
    #[ignore] // Requires Docker - run with: cargo test -- --ignored
    async fn test_execute_handles_multiple_port_mappings() {
        // Arrange
        let importer = create_test_importer();
        let mut plan = create_test_plan();
        plan.deployment.ports = vec![
            temps_import_types::PortMapping {
                container_port: 80,
                host_port: Some(8080),
                protocol: temps_import_types::plan::Protocol::Tcp,
                is_primary: true,
            },
            temps_import_types::PortMapping {
                container_port: 443,
                host_port: Some(8443),
                protocol: temps_import_types::plan::Protocol::Tcp,
                is_primary: false,
            },
        ];

        let context = temps_import_types::ImportContext {
            session_id: "test-session-4".to_string(),
            user_id: 1,
            dry_run: false,
            project_name: "multi-port-project".to_string(),
            preset: "docker".to_string(),
            directory: ".".to_string(),
            main_branch: "main".to_string(),
            git_provider_connection_id: None,
            repo_owner: None,
            repo_name: None,
            metadata: std::collections::HashMap::new(),
        };

        let db = create_test_database().await;
        let mock_services = MockServiceProvider { db };

        // Act
        let result = importer.execute(context, plan, &mock_services).await;

        // Assert
        assert!(result.is_ok(), "Should handle multiple port mappings");
        let outcome = result.unwrap();
        assert!(outcome.success);

        // Note: In a full integration test, we would verify that 2 deployment_container records were created
    }

    #[tokio::test]
    #[ignore] // Requires Docker - run with: cargo test -- --ignored
    async fn test_execute_includes_warnings_for_unknown_preset() {
        // Arrange
        let importer = create_test_importer();
        let plan = create_test_plan();
        let context = temps_import_types::ImportContext {
            session_id: "test-session-5".to_string(),
            user_id: 1,
            dry_run: false,
            project_name: "unknown-preset-project".to_string(),
            preset: "nonexistent-preset-xyz".to_string(), // Unknown preset
            directory: ".".to_string(),
            main_branch: "main".to_string(),
            git_provider_connection_id: None,
            repo_owner: None,
            repo_name: None,
            metadata: std::collections::HashMap::new(),
        };

        let db = create_test_database().await;
        let mock_services = MockServiceProvider { db };

        // Act
        let result = importer.execute(context, plan, &mock_services).await;

        // Assert
        assert!(result.is_ok(), "Should succeed even with unknown preset");
        let outcome = result.unwrap();
        assert!(outcome.success);
        assert!(
            !outcome.warnings.is_empty(),
            "Should include warning about unknown preset"
        );
        assert!(
            outcome.warnings.iter().any(|w| w.contains("not found")),
            "Warning should mention preset not found"
        );
    }

    // Helper functions for tests

    /// Create a test importer
    fn create_test_importer() -> DockerImporter {
        // For tests, we create an importer without actually connecting to Docker
        DockerImporter {
            docker: Arc::new(Docker::connect_with_local_defaults().unwrap()),
            version: "test".to_string(),
        }
    }

    /// Create a test import plan
    fn create_test_plan() -> ImportPlan {
        use temps_import_types::plan::*;

        ImportPlan {
            version: "1.0".to_string(),
            source: "docker".to_string(),
            source_container_id: "test-container-123".to_string(),
            project: ProjectConfiguration {
                name: "test-project".to_string(),
                slug: "test-project".to_string(),
                project_type: ProjectType::Docker,
                is_web_app: true,
            },
            environment: EnvironmentConfiguration {
                name: "production".to_string(),
                subdomain: "test-project".to_string(),
                resources: ResourceLimits {
                    cpu_limit: Some(1000),
                    memory_limit: Some(512),
                    cpu_request: Some(500),
                    memory_request: Some(256),
                },
            },
            deployment: DeploymentConfiguration {
                image: "nginx:latest".to_string(),
                build: None,
                strategy: DeploymentStrategy::Replace,
                env_vars: vec![],
                ports: vec![],
                volumes: vec![],
                network: NetworkConfiguration {
                    mode: NetworkMode::Bridge,
                    hostname: None,
                    dns_servers: vec![],
                },
                resources: ResourceLimits {
                    cpu_limit: Some(1000),
                    memory_limit: Some(512),
                    cpu_request: Some(500),
                    memory_request: Some(256),
                },
                command: None,
                entrypoint: None,
                working_dir: None,
                health_check: None,
            },
            metadata: PlanMetadata {
                generated_at: chrono::Utc::now(),
                generator_version: "1.0".to_string(),
                complexity: PlanComplexity::Low,
                warnings: vec![],
            },
        }
    }

    /// Create a mock service provider for dry-run tests (no database needed)
    fn create_mock_service_provider() -> MockServiceProvider {
        let db = Arc::new(sea_orm::DatabaseConnection::Disconnected);
        MockServiceProvider { db }
    }

    /// Create a test database connection with migrations
    async fn create_test_database() -> std::sync::Arc<sea_orm::DatabaseConnection> {
        // Use TestDatabase from temps_database for integration tests
        match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(test_db) => test_db.db.clone(),
            Err(_) => {
                // Fallback to disconnected database for environments without Docker
                std::sync::Arc::new(sea_orm::DatabaseConnection::Disconnected)
            }
        }
    }

    /// Mock service provider for testing
    struct MockServiceProvider {
        db: std::sync::Arc<sea_orm::DatabaseConnection>,
    }

    impl ImportServiceProvider for MockServiceProvider {
        fn db(&self) -> &sea_orm::DatabaseConnection {
            &self.db
        }

        fn project_service(&self) -> &dyn std::any::Any {
            // Return self as placeholder - integration tests with #[ignore] won't run without Docker
            self
        }

        fn deployment_service(&self) -> &dyn std::any::Any {
            // Return self as placeholder - integration tests with #[ignore] won't run without Docker
            self
        }

        fn git_provider_manager(&self) -> &dyn std::any::Any {
            // Return self as placeholder - integration tests with #[ignore] won't run without Docker
            self
        }
    }
}
