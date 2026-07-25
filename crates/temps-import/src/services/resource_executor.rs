//! Resource execution phase for platform imports.
//!
//! Importers describe *what* exists on the source platform; this module makes
//! it real on the temps side, source-agnostically, around the importer's own
//! `execute()`:
//!
//! 1. **Services** — every `ServicePlan` with `action = Create` becomes a
//!    temps-managed service via [`ExternalServiceManager`].
//! 2. **Data population** — services whose plan carries a reachable
//!    `source_url` get their data copied by a one-off dump/restore container
//!    (`pg_dump | psql`, `mysqldump | mysql`, `mongodump | mongorestore`)
//!    running on the host network so it can reach both the source machine and
//!    the newly created local service.
//! 3. **Domains** — every `DomainPlan` with `action = Import` becomes a
//!    custom domain on the imported environment (certificates are issued
//!    on-demand once the user cuts DNS over).
//! 4. **Env rewriting** — env var values referencing the source platform are
//!    rewritten before the project is created: source database URLs point at
//!    the new managed service, and source-generated domains (sslip.io,
//!    traefik.me, CapRover subdomains) point at the environment's preview
//!    hostname.
//!
//! Every step reports an honest [`StepResult`] — a failed population or a
//! duplicate domain never aborts the rest of the import.

use bollard::Docker;
use std::collections::HashMap;
use std::sync::Arc;
use temps_core::public_hostname::PublicHostnameStrategy;
use temps_import_types::{CreatedResource, DomainAction, ImportPlan, ServiceAction, StepResult};
use temps_projects::services::CustomDomainService;
use temps_providers::externalsvc::ServiceType;
use temps_providers::services::{
    CreateExternalServiceRequest, ExternalServiceInfo, ExternalServiceManager,
};
use tracing::{info, warn};

/// Key inside `ServicePlan.parameters` holding the reachable source
/// connection URL (set by the platform importers at plan time).
pub const SOURCE_URL_PARAM: &str = "source_url";

/// A managed service created during import, with everything needed to
/// populate it and rewrite env vars.
pub struct CreatedServiceRecord {
    /// Name from the plan (matches `ServicePlan.name`)
    pub plan_name: String,
    /// Normalized plan service type ("postgres", "mysql", ...)
    pub plan_type: String,
    /// The created temps service
    pub info: ExternalServiceInfo,
    /// Locally reachable connection URL of the new service
    pub local_url: Option<String>,
    /// Reachable connection URL on the source platform (for population)
    pub source_url: Option<String>,
}

/// Executes the platform-generic parts of an import plan.
pub struct ResourceExecutor {
    external_services: Arc<ExternalServiceManager>,
    custom_domains: Arc<CustomDomainService>,
    docker: Arc<Docker>,
}

impl ResourceExecutor {
    pub fn new(
        external_services: Arc<ExternalServiceManager>,
        custom_domains: Arc<CustomDomainService>,
        docker: Arc<Docker>,
    ) -> Self {
        Self {
            external_services,
            custom_domains,
            docker,
        }
    }

    /// Map a plan's service type string to a temps [`ServiceType`].
    /// Returns `None` for types temps has no managed equivalent for.
    pub fn map_service_type(plan_type: &str) -> Option<ServiceType> {
        match plan_type {
            "postgres" | "postgresql" => Some(ServiceType::Postgres),
            // temps' managed MySQL-compatible service is MariaDB
            "mysql" | "mariadb" => Some(ServiceType::Mariadb),
            "mongodb" | "mongo" => Some(ServiceType::Mongodb),
            "redis" | "keydb" | "dragonfly" => Some(ServiceType::Redis),
            _ => None,
        }
    }

    /// Create every `action = Create` service in the plan.
    pub async fn create_services(
        &self,
        plan: &ImportPlan,
    ) -> (
        Vec<StepResult>,
        Vec<CreatedServiceRecord>,
        Vec<CreatedResource>,
    ) {
        let mut steps = Vec::new();
        let mut created = Vec::new();
        let mut resources = Vec::new();

        for service_plan in plan
            .services
            .iter()
            .filter(|s| s.action == ServiceAction::Create)
        {
            let step_id = format!("create-service-{}", sanitize_slug(&service_plan.name));
            let started = std::time::Instant::now();

            let Some(service_type) = Self::map_service_type(&service_plan.service_type) else {
                steps.push(StepResult {
                    step_id,
                    step_title: format!("Create managed service '{}'", service_plan.name),
                    success: true,
                    skipped: true,
                    message: format!(
                        "temps has no managed equivalent for '{}' — create a replacement manually",
                        service_plan.service_type
                    ),
                    created_resources: vec![],
                    duration_seconds: started.elapsed().as_secs_f64(),
                });
                continue;
            };

            let source_url = service_plan
                .parameters
                .get(SOURCE_URL_PARAM)
                .and_then(|v| v.as_str());
            let request = CreateExternalServiceRequest {
                name: service_plan.name.clone(),
                service_type,
                version: service_plan.version.clone(),
                parameters: service_parameters(&service_type, &service_plan.name, source_url),
                node_id: None,
                topology: "standalone".to_string(),
                members: vec![],
            };

            match self.external_services.create_service(request).await {
                Ok(info) => {
                    let local_url = self.local_dsn(info.id, &service_plan.service_type).await;
                    if local_url.is_none() {
                        warn!(
                            "Created service {} but could not build its local connection URL — data population and env rewriting are skipped for it",
                            info.id
                        );
                    }

                    let source_url = source_url.map(|s| s.to_string());

                    resources.push(CreatedResource {
                        resource_type: "service".to_string(),
                        resource_id: info.id,
                        resource_name: info.name.clone(),
                    });
                    steps.push(StepResult {
                        step_id,
                        step_title: format!("Create managed service '{}'", service_plan.name),
                        success: true,
                        skipped: false,
                        message: format!(
                            "Created temps-managed {} service '{}' (id {})",
                            service_plan.service_type, info.name, info.id
                        ),
                        created_resources: vec![CreatedResource {
                            resource_type: "service".to_string(),
                            resource_id: info.id,
                            resource_name: info.name.clone(),
                        }],
                        duration_seconds: started.elapsed().as_secs_f64(),
                    });
                    created.push(CreatedServiceRecord {
                        plan_name: service_plan.name.clone(),
                        plan_type: service_plan.service_type.clone(),
                        info,
                        local_url,
                        source_url,
                    });
                }
                Err(e) => {
                    steps.push(StepResult {
                        step_id,
                        step_title: format!("Create managed service '{}'", service_plan.name),
                        success: false,
                        skipped: false,
                        message: format!(
                            "Failed to create managed {} service '{}': {}",
                            service_plan.service_type, service_plan.name, e
                        ),
                        created_resources: vec![],
                        duration_seconds: started.elapsed().as_secs_f64(),
                    });
                }
            }
        }

        (steps, created, resources)
    }

    /// Build a locally reachable, correctly encoded connection URL for a
    /// created service. `get_local_address` yields only `host:port`; the
    /// credentials come from the service's (decrypted) config parameters.
    async fn local_dsn(&self, service_id: i32, plan_type: &str) -> Option<String> {
        let model = self.external_services.get_service(service_id).await.ok()?;
        let config = self
            .external_services
            .get_service_config(service_id)
            .await
            .ok()?;
        let address = self.external_services.get_local_address(model).await.ok()?;
        let (host, port) = address.rsplit_once(':')?;

        let parameter = |key: &str| {
            config
                .parameters
                .get(key)
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        };
        let scheme = match plan_type {
            "postgres" | "postgresql" => "postgres",
            "mysql" | "mariadb" => "mysql",
            "mongodb" | "mongo" => "mongodb",
            "redis" | "keydb" | "dragonfly" => "redis",
            _ => return None,
        };

        // url::Url percent-encodes credentials correctly on serialization —
        // generated passwords may contain %, ^, + and other URL-hostile chars.
        let mut url = url::Url::parse(&format!("{}://{}:{}", scheme, host, port)).ok()?;
        if let Some(username) = parameter("username") {
            url.set_username(&username).ok()?;
        }
        if let Some(password) = parameter("password") {
            url.set_password(Some(&password)).ok()?;
        }
        if let Some(database) = parameter("database") {
            url.set_path(&database);
        }
        Some(url.to_string())
    }

    /// Copy data into each created service that has a reachable source URL,
    /// using a one-off container on the host network (so both the external
    /// source machine and the local service port are reachable).
    pub async fn populate_services(&self, created: &[CreatedServiceRecord]) -> Vec<StepResult> {
        let mut steps = Vec::new();

        for record in created {
            let step_id = format!("populate-service-{}", sanitize_slug(&record.plan_name));
            let started = std::time::Instant::now();

            let (Some(source_url), Some(local_url)) =
                (record.source_url.as_deref(), record.local_url.as_deref())
            else {
                steps.push(StepResult {
                    step_id,
                    step_title: format!("Copy data into '{}'", record.plan_name),
                    success: true,
                    skipped: true,
                    message: format!(
                        "Data for '{}' was not copied automatically — the source database is not reachable from this server (see the plan's data implications for the manual dump/restore path)",
                        record.plan_name
                    ),
                    created_resources: vec![],
                    duration_seconds: started.elapsed().as_secs_f64(),
                });
                continue;
            };

            let Some((image, command)) = dump_restore_command(&record.plan_type) else {
                steps.push(StepResult {
                    step_id,
                    step_title: format!("Copy data into '{}'", record.plan_name),
                    success: true,
                    skipped: true,
                    message: format!(
                        "Automatic data copy is not supported for {} — copy the data manually",
                        record.plan_type
                    ),
                    created_resources: vec![],
                    duration_seconds: started.elapsed().as_secs_f64(),
                });
                continue;
            };

            match self
                .run_transfer_container(&image, &command, source_url, local_url)
                .await
            {
                Ok(()) => {
                    info!(
                        "Populated imported service '{}' from source platform",
                        record.plan_name
                    );
                    steps.push(StepResult {
                        step_id,
                        step_title: format!("Copy data into '{}'", record.plan_name),
                        success: true,
                        skipped: false,
                        message: format!(
                            "Copied data from the source platform into managed service '{}'",
                            record.info.name
                        ),
                        created_resources: vec![],
                        duration_seconds: started.elapsed().as_secs_f64(),
                    });
                }
                Err(e) => {
                    steps.push(StepResult {
                        step_id,
                        step_title: format!("Copy data into '{}'", record.plan_name),
                        success: false,
                        skipped: false,
                        message: format!(
                            "Data copy into '{}' failed: {} — the service was created empty; run the dump/restore from the plan manually",
                            record.info.name, e
                        ),
                        created_resources: vec![],
                        duration_seconds: started.elapsed().as_secs_f64(),
                    });
                }
            }
        }

        steps
    }

    /// Run a one-off transfer container: `<command>` with SRC/DST env vars.
    async fn run_transfer_container(
        &self,
        image: &str,
        command: &str,
        source_url: &str,
        local_url: &str,
    ) -> Result<(), String> {
        use bollard::models::{ContainerCreateBody, HostConfig};
        use bollard::query_parameters::{
            CreateContainerOptionsBuilder, CreateImageOptions, RemoveContainerOptions,
            StartContainerOptions, WaitContainerOptions,
        };
        use futures_util::StreamExt;

        // Ensure the image exists (no-op when already pulled)
        let mut pull = self.docker.create_image(
            Some(CreateImageOptions {
                from_image: Some(image.to_string()),
                ..Default::default()
            }),
            None,
            None,
        );
        while let Some(item) = pull.next().await {
            if let Err(e) = item {
                return Err(format!("failed to pull transfer image '{}': {}", image, e));
            }
        }

        let name = format!(
            "temps-import-transfer-{}",
            &uuid::Uuid::new_v4().to_string()[..8]
        );
        let container = self
            .docker
            .create_container(
                Some(CreateContainerOptionsBuilder::new().name(&name).build()),
                ContainerCreateBody {
                    image: Some(image.to_string()),
                    cmd: Some(vec![
                        "sh".to_string(),
                        "-c".to_string(),
                        command.to_string(),
                    ]),
                    env: Some(vec![
                        format!("SRC={}", source_url),
                        format!("DST={}", local_url),
                    ]),
                    host_config: Some(HostConfig {
                        network_mode: Some("host".to_string()),
                        auto_remove: Some(false),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| format!("failed to create transfer container: {}", e))?;

        self.docker
            .start_container(&container.id, None::<StartContainerOptions>)
            .await
            .map_err(|e| format!("failed to start transfer container: {}", e))?;

        // Wait for completion (bounded by the container's own runtime)
        let mut wait = self
            .docker
            .wait_container(&container.id, None::<WaitContainerOptions>);
        // bollard's wait errors on nonzero exit codes with a often-empty
        // message — treat it as a failed status and let the log tail explain.
        let status = match wait.next().await {
            Some(Ok(result)) => result.status_code,
            Some(Err(bollard::errors::Error::DockerContainerWaitError { code, .. })) => code,
            Some(Err(e)) => {
                let logs = self.container_log_tail(&container.id).await;
                let _ = self
                    .docker
                    .remove_container(
                        &container.id,
                        Some(RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await;
                return Err(format!(
                    "transfer container failed: {} — {}",
                    e,
                    logs.chars().take(400).collect::<String>()
                ));
            }
            None => -1,
        };

        // Capture the tail of the logs for the error message before removal
        let logs = self.container_log_tail(&container.id).await;
        let _ = self
            .docker
            .remove_container(
                &container.id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        if status == 0 {
            Ok(())
        } else {
            Err(format!(
                "transfer exited with status {}: {}",
                status,
                logs.chars().take(500).collect::<String>()
            ))
        }
    }

    async fn container_log_tail(&self, container_id: &str) -> String {
        use bollard::query_parameters::LogsOptionsBuilder;
        use futures_util::StreamExt;

        let mut stream = self.docker.logs(
            container_id,
            Some(
                LogsOptionsBuilder::new()
                    .stdout(true)
                    .stderr(true)
                    .tail("20")
                    .build(),
            ),
        );
        let mut output = String::new();
        while let Some(Ok(chunk)) = stream.next().await {
            output.push_str(&chunk.to_string());
        }
        output
    }

    /// Create every `action = Import` domain on the imported environment.
    pub async fn create_domains(
        &self,
        plan: &ImportPlan,
        project_id: i32,
        environment_id: i32,
    ) -> (Vec<StepResult>, Vec<CreatedResource>) {
        let mut steps = Vec::new();
        let mut resources = Vec::new();

        for domain_plan in plan
            .domains
            .iter()
            .filter(|d| d.action == DomainAction::Import)
        {
            let step_id = format!("import-domain-{}", sanitize_slug(&domain_plan.domain));
            let started = std::time::Instant::now();

            match self
                .custom_domains
                .create_custom_domain(
                    project_id,
                    environment_id,
                    domain_plan.domain.clone(),
                    domain_plan.redirect_to.clone(),
                    domain_plan.status_code,
                    None,
                    None,
                )
                .await
            {
                Ok(model) => {
                    resources.push(CreatedResource {
                        resource_type: "domain".to_string(),
                        resource_id: model.id,
                        resource_name: domain_plan.domain.clone(),
                    });
                    steps.push(StepResult {
                        step_id,
                        step_title: format!("Import domain '{}'", domain_plan.domain),
                        success: true,
                        skipped: false,
                        message: format!(
                            "Registered '{}' on the imported environment — point its DNS at this server to finish the cutover (the certificate is issued automatically on first request)",
                            domain_plan.domain
                        ),
                        created_resources: vec![CreatedResource {
                            resource_type: "domain".to_string(),
                            resource_id: model.id,
                            resource_name: domain_plan.domain.clone(),
                        }],
                        duration_seconds: started.elapsed().as_secs_f64(),
                    });
                }
                Err(e) => {
                    steps.push(StepResult {
                        step_id,
                        step_title: format!("Import domain '{}'", domain_plan.domain),
                        success: false,
                        skipped: false,
                        message: format!(
                            "Failed to register domain '{}': {} — add it manually on the environment",
                            domain_plan.domain, e
                        ),
                        created_resources: vec![],
                        duration_seconds: started.elapsed().as_secs_f64(),
                    });
                }
            }
        }

        (steps, resources)
    }
}

/// Required creation parameters per service type, derived from the source
/// connection URL when available so the imported service matches what the
/// application expects (database name, user). Passwords are always
/// auto-generated by the service manager.
fn service_parameters(
    service_type: &ServiceType,
    plan_name: &str,
    source_url: Option<&str>,
) -> HashMap<String, serde_json::Value> {
    let mut parameters = HashMap::new();
    if !matches!(
        service_type,
        ServiceType::Postgres | ServiceType::Mariadb | ServiceType::Mongodb
    ) {
        return parameters;
    }

    let parsed = source_url.and_then(|u| url::Url::parse(u).ok());
    let database = parsed
        .as_ref()
        .map(|u| u.path().trim_start_matches('/').to_string())
        .filter(|db| !db.is_empty())
        .unwrap_or_else(|| sanitize_db_identifier(plan_name));
    let username = parsed
        .as_ref()
        .map(|u| u.username().to_string())
        .filter(|user| !user.is_empty())
        .unwrap_or_else(|| "app".to_string());

    parameters.insert("database".to_string(), serde_json::json!(database));
    parameters.insert("username".to_string(), serde_json::json!(username));
    parameters
}

/// A safe database identifier from an arbitrary service name
fn sanitize_db_identifier(name: &str) -> String {
    let cleaned: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let trimmed = cleaned.trim_matches('_');
    if trimmed.is_empty() {
        "app".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Image + shell command per service type for the one-off data transfer.
/// The command reads `$SRC` (source platform URL) and `$DST` (new local URL).
fn dump_restore_command(plan_type: &str) -> Option<(String, String)> {
    match plan_type {
        "postgres" | "postgresql" => Some((
            "postgres:16-alpine".to_string(),
            "pg_dump --no-owner --no-privileges \"$SRC\" | psql \"$DST\"".to_string(),
        )),
        "mysql" | "mariadb" => Some((
            "mariadb:11".to_string(),
            "mariadb-dump --skip-ssl \"--uri=$SRC\" | mariadb \"--uri=$DST\"".to_string(),
        )),
        "mongodb" | "mongo" => Some((
            "mongo:7".to_string(),
            "mongodump --uri=\"$SRC\" --archive | mongorestore --uri=\"$DST\" --archive"
                .to_string(),
        )),
        _ => None,
    }
}

/// Compute the environment's preview hostname (`{env-subdomain}.{preview_domain}`).
pub fn preview_hostname(preview_domain: &str, environment_subdomain: &str) -> String {
    PublicHostnameStrategy::Standard.environment_hostname(preview_domain, environment_subdomain)
}

/// Build env-var value rewrites for an import:
/// - reachable source database URLs → the new managed service's local URL
/// - source-generated domains (the plan's skipped domains) → the preview hostname
pub fn build_env_rewrites(
    plan: &ImportPlan,
    created: &[CreatedServiceRecord],
    preview_host: &str,
) -> Vec<(String, String)> {
    let mut rewrites = Vec::new();

    for record in created {
        if let (Some(source_url), Some(local_url)) = (&record.source_url, &record.local_url) {
            rewrites.push((source_url.clone(), local_url.clone()));
        }
    }

    for domain_plan in plan
        .domains
        .iter()
        .filter(|d| d.action == DomainAction::Skip)
    {
        if domain_plan.domain != preview_host {
            rewrites.push((domain_plan.domain.clone(), preview_host.to_string()));
        }
    }

    rewrites
}

/// Apply rewrites to the plan's env vars (substring replacement, longest
/// patterns first so full URLs win over their embedded hostnames).
/// Returns the number of variables whose value changed.
pub fn apply_env_rewrites(plan: &mut ImportPlan, rewrites: &[(String, String)]) -> usize {
    let mut ordered: Vec<&(String, String)> = rewrites.iter().collect();
    ordered.sort_by_key(|(from, _)| std::cmp::Reverse(from.len()));

    let mut changed = 0;
    let deployments =
        std::iter::once(&mut plan.deployment).chain(plan.additional_deployments.iter_mut());
    for deployment in deployments {
        for env_var in &mut deployment.env_vars {
            let original = env_var.value.clone();
            for (from, to) in &ordered {
                if env_var.value.contains(from.as_str()) {
                    env_var.value = env_var.value.replace(from.as_str(), to);
                }
            }
            if env_var.value != original {
                env_var.source_description = Some(format!(
                    "{} (rewritten for temps during import)",
                    env_var.source_description.as_deref().unwrap_or("imported")
                ));
                changed += 1;
            }
        }
    }
    changed
}

fn sanitize_slug(name: &str) -> String {
    name.to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && c != '-', "-")
        .trim_matches('-')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_import_types::plan::{
        DeploymentConfiguration, DeploymentStrategy, EnvironmentConfiguration, EnvironmentVariable,
        NetworkConfiguration, NetworkMode, PlanComplexity, PlanMetadata, ProjectConfiguration,
        ProjectType, ResourceLimits,
    };
    use temps_import_types::{DomainPlan, MigrationSummary, ResourceCounts, RiskLevel};

    fn plan_with(env: Vec<(&str, &str)>, skipped_domain: &str) -> ImportPlan {
        ImportPlan {
            version: "1.0".to_string(),
            source: "coolify".to_string(),
            source_id: "x".to_string(),
            project: ProjectConfiguration {
                name: "lab".to_string(),
                slug: "lab".to_string(),
                project_type: ProjectType::Git,
                is_web_app: true,
            },
            environment: EnvironmentConfiguration {
                name: "production".to_string(),
                subdomain: "lab".to_string(),
                resources: ResourceLimits {
                    cpu_limit: None,
                    memory_limit: None,
                    cpu_request: None,
                    memory_request: None,
                },
            },
            deployment: DeploymentConfiguration {
                image: "x".to_string(),
                build: None,
                strategy: DeploymentStrategy::Replace,
                env_vars: env
                    .into_iter()
                    .map(|(k, v)| EnvironmentVariable {
                        key: k.to_string(),
                        value: v.to_string(),
                        is_secret: false,
                        source_description: None,
                    })
                    .collect(),
                ports: vec![],
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
            },
            services: vec![],
            domains: vec![DomainPlan {
                domain: skipped_domain.to_string(),
                environment: "production".to_string(),
                redirect_to: None,
                status_code: None,
                action: DomainAction::Skip,
                action_description: String::new(),
            }],
            additional_deployments: vec![],
            steps: vec![],
            summary: MigrationSummary {
                headline: String::new(),
                overall_risk: RiskLevel::Low,
                resource_counts: ResourceCounts::default(),
                critical_warnings: vec![],
                manual_actions_required: vec![],
                unsupported_features: vec![],
            },
            metadata: PlanMetadata {
                generated_at: chrono::Utc::now(),
                generator_version: "test".to_string(),
                complexity: PlanComplexity::Low,
                warnings: vec![],
            },
            cost_analysis: None,
        }
    }

    fn record(source: &str, local: &str) -> CreatedServiceRecord {
        CreatedServiceRecord {
            plan_name: "lab-db".to_string(),
            plan_type: "postgres".to_string(),
            info: ExternalServiceInfo {
                id: 1,
                name: "lab-db".to_string(),
                service_type: ServiceType::Postgres,
                version: Some("16".to_string()),
                status: "running".to_string(),
                connection_info: None,
                created_at: String::new(),
                updated_at: String::new(),
                node_id: None,
                topology: "standalone".to_string(),
                members: vec![],
                error_message: None,
                metrics_enabled: false,
            },
            local_url: Some(local.to_string()),
            source_url: Some(source.to_string()),
        }
    }

    #[test]
    fn maps_plan_types_to_temps_service_types() {
        assert_eq!(
            ResourceExecutor::map_service_type("postgres"),
            Some(ServiceType::Postgres)
        );
        assert_eq!(
            ResourceExecutor::map_service_type("mysql"),
            Some(ServiceType::Mariadb)
        );
        assert_eq!(
            ResourceExecutor::map_service_type("redis"),
            Some(ServiceType::Redis)
        );
        assert_eq!(ResourceExecutor::map_service_type("clickhouse"), None);
    }

    #[test]
    fn dump_commands_exist_for_data_bearing_types() {
        assert!(dump_restore_command("postgres").is_some());
        assert!(dump_restore_command("mariadb").is_some());
        assert!(dump_restore_command("mongodb").is_some());
        assert!(dump_restore_command("redis").is_none());
    }

    #[test]
    fn rewrites_source_dsn_and_generated_domain() {
        let source_dsn = "postgres://postgres:pw@1.2.3.4:5432/postgres";
        let mut plan = plan_with(
            vec![
                ("DATABASE_URL", source_dsn),
                ("APP_URL", "http://lab-shop.1.2.3.4.sslip.io/checkout"),
                ("UNTOUCHED", "value"),
            ],
            "lab-shop.1.2.3.4.sslip.io",
        );
        let created = vec![record(source_dsn, "postgres://lab:new@localhost:15001/lab")];
        let rewrites = build_env_rewrites(&plan, &created, "lab.preview.temps.dev");

        let changed = apply_env_rewrites(&mut plan, &rewrites);
        assert_eq!(changed, 2);
        assert_eq!(
            plan.deployment.env_vars[0].value,
            "postgres://lab:new@localhost:15001/lab"
        );
        assert_eq!(
            plan.deployment.env_vars[1].value,
            "http://lab.preview.temps.dev/checkout"
        );
        assert_eq!(plan.deployment.env_vars[2].value, "value");
        assert!(plan.deployment.env_vars[0]
            .source_description
            .as_deref()
            .unwrap()
            .contains("rewritten"));
    }

    #[test]
    fn longest_rewrite_wins_over_embedded_hostname() {
        // The DSN contains the host 1.2.3.4 — the full-URL rewrite must be
        // applied before any shorter hostname rewrite could corrupt it.
        let source_dsn = "postgres://postgres:pw@db.1.2.3.4.sslip.io:5432/app";
        let mut plan = plan_with(vec![("DATABASE_URL", source_dsn)], "db.1.2.3.4.sslip.io");
        let created = vec![record(source_dsn, "postgres://new@localhost:15001/app")];
        let rewrites = build_env_rewrites(&plan, &created, "preview.temps.dev");
        apply_env_rewrites(&mut plan, &rewrites);
        assert_eq!(
            plan.deployment.env_vars[0].value,
            "postgres://new@localhost:15001/app"
        );
    }

    #[test]
    fn preview_hostname_composes_subdomain_and_domain() {
        let host = preview_hostname("preview.example.com", "lab");
        assert!(host.contains("lab"));
        assert!(host.ends_with("preview.example.com"));
    }
}

#[cfg(test)]
mod parameter_tests {
    use super::*;

    #[test]
    fn derives_database_and_username_from_source_url() {
        let params = service_parameters(
            &ServiceType::Postgres,
            "lab-db",
            Some("postgres://labuser:pw@1.2.3.4:5432/labdb"),
        );
        assert_eq!(params.get("database").unwrap(), "labdb");
        assert_eq!(params.get("username").unwrap(), "labuser");
    }

    #[test]
    fn falls_back_to_sanitized_name_without_source_url() {
        let params = service_parameters(&ServiceType::Postgres, "lab-db", None);
        assert_eq!(params.get("database").unwrap(), "lab_db");
        assert_eq!(params.get("username").unwrap(), "app");
    }

    #[test]
    fn redis_needs_no_parameters() {
        assert!(service_parameters(&ServiceType::Redis, "cache", None).is_empty());
    }
}
