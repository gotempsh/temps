// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Dokploy importer — WorkloadImporter implementation
//!
//! The Dokploy *environment* (inside a Dokploy project) is the project
//! boundary for migration: importing an application pulls in its sibling
//! applications, the environment's databases (as managed-service candidates
//! with data-migration guidance), and every application domain.

use crate::client::{DokployClient, DokployDbKind};
use crate::error::DokployImportError;
use crate::model::{DokployApplication, DokployDatabase, DokployProject};
use crate::validation::DokployValidationRules;
use async_trait::async_trait;
use std::collections::HashMap;
use temps_import_types::plan::{
    DeploymentConfiguration, EnvironmentConfiguration, GitSourcePlan, PlanComplexity, PlanMetadata,
    ProjectConfiguration, ProjectType, Protocol,
};
use temps_import_types::{
    BuildConfiguration, CredentialValidation, DataImplication, DataImplicationSeverity,
    DeploymentStrategy, DomainAction, DomainPlan, DomainSnapshot, EnvironmentVariable, GitInfo,
    ImportContext, ImportCredentials, ImportError, ImportOutcome, ImportPlan, ImportResult,
    ImportSelector, ImportServiceProvider, ImportSource, ImporterCapabilities, ManualAction,
    ManualActionTiming, MigrationStep, MigrationSummary, NetworkConfiguration, NetworkInfo,
    NetworkMode, PortMapping, ProjectSnapshot, ResourceCounts, ResourceInfo, ResourceLimits,
    RiskLevel, ServiceAction, ServicePlan, ServiceSnapshot, SnapshotServiceType, StepResourceType,
    StepResult, UnsupportedFeature, WorkloadDescriptor, WorkloadId, WorkloadImporter,
    WorkloadSnapshot, WorkloadStatus, WorkloadType,
};
use tracing::info;

/// Wildcard-DNS convenience domains — planned as skipped
const GENERATED_DOMAIN_SUFFIXES: &[&str] = &[".traefik.me", ".sslip.io", ".nip.io"];

const ENVIRONMENT_NAME_KEY: &str = "environment_name";

/// Dokploy platform importer
pub struct DokployImporter {
    version: String,
}

impl Default for DokployImporter {
    fn default() -> Self {
        Self::new()
    }
}

impl DokployImporter {
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

/// Map Dokploy's applicationStatus to a workload status
fn map_status(status: Option<&str>) -> WorkloadStatus {
    match status.unwrap_or("") {
        "running" => WorkloadStatus::Running,
        "idle" => WorkloadStatus::Stopped,
        "error" => WorkloadStatus::Failed,
        "done" => WorkloadStatus::Deployed,
        "" => WorkloadStatus::Unknown,
        _ => WorkloadStatus::Unknown,
    }
}

fn is_generated_domain(host: &str) -> bool {
    GENERATED_DOMAIN_SUFFIXES
        .iter()
        .any(|suffix| host.ends_with(suffix))
}

/// Git info from a Dokploy application (custom git URL or GitHub-app fields)
fn git_info_for(app: &DokployApplication) -> Option<GitInfo> {
    if app.is_image_based() {
        return None;
    }
    if let Some(url_str) = app.custom_git_url.as_deref().filter(|u| !u.is_empty()) {
        let url = url::Url::parse(url_str).ok()?;
        let host = url.host_str()?.to_string();
        let mut segments = url.path_segments()?;
        let owner = segments.next()?.to_string();
        let repo = segments.next()?.trim_end_matches(".git").to_string();
        if owner.is_empty() || repo.is_empty() {
            return None;
        }
        let provider = if host.contains("github") {
            "github".to_string()
        } else if host.contains("gitlab") {
            "gitlab".to_string()
        } else if host.contains("bitbucket") {
            "bitbucket".to_string()
        } else {
            host.clone()
        };
        return Some(GitInfo {
            provider,
            owner: owner.clone(),
            repo: repo.clone(),
            default_branch: app
                .custom_git_branch
                .clone()
                .unwrap_or_else(|| "main".to_string()),
            clone_url: Some(format!("https://{}/{}/{}.git", host, owner, repo)),
        });
    }
    // GitHub-app sourced applications
    if let (Some(owner), Some(repo)) = (app.owner.as_deref(), app.repository.as_deref()) {
        if !owner.is_empty() && !repo.is_empty() {
            return Some(GitInfo {
                provider: "github".to_string(),
                owner: owner.to_string(),
                repo: repo.to_string(),
                default_branch: app.branch.clone().unwrap_or_else(|| "main".to_string()),
                clone_url: Some(format!("https://github.com/{}/{}.git", owner, repo)),
            });
        }
    }
    None
}

/// Build a workload snapshot for one Dokploy application
fn app_to_snapshot(app: &DokployApplication) -> WorkloadSnapshot {
    let env: HashMap<String, String> = app.env_pairs().into_iter().collect();

    let source_metadata = serde_json::json!({
        "source_type": app.source_type,
        "build_type": app.build_type,
        "custom_git_url": app.custom_git_url,
        "custom_git_branch": app.custom_git_branch,
        "custom_git_build_path": app.custom_git_build_path,
        "app_name": app.app_name,
        "dokploy_status": app.application_status,
        "domains": app.domains.iter().filter_map(|d| d.host.clone()).collect::<Vec<_>>(),
    });

    WorkloadSnapshot {
        id: WorkloadId::new(app.application_id.clone()),
        name: Some(app.name.clone()),
        workload_type: WorkloadType::Container,
        status: map_status(app.application_status.as_deref()),
        image: app
            .is_image_based()
            .then(|| app.docker_image.clone())
            .flatten(),
        command: None,
        entrypoint: None,
        working_dir: None,
        env,
        ports: HashMap::new(), // Dokploy routes via domains, not exposed ports
        volumes: vec![],
        network: NetworkInfo {
            mode: NetworkMode::Bridge,
            networks: vec![],
            hostname: None,
            domain_name: None,
        },
        resources: ResourceInfo::default(),
        labels: HashMap::new(),
        health_check: None,
        restart_policy: None,
        created_at: chrono::Utc::now(),
        source_metadata,
    }
}

/// Connection URL scheme per database kind
fn scheme_for(kind: DokployDbKind) -> &'static str {
    match kind {
        DokployDbKind::Postgres => "postgres",
        DokployDbKind::Mysql | DokployDbKind::Mariadb => "mysql",
        DokployDbKind::Mongo => "mongodb",
        DokployDbKind::Redis => "redis",
    }
}

fn snapshot_type_for(kind: DokployDbKind) -> SnapshotServiceType {
    match kind {
        DokployDbKind::Postgres => SnapshotServiceType::Postgres,
        DokployDbKind::Mysql | DokployDbKind::Mariadb => SnapshotServiceType::Mysql,
        DokployDbKind::Mongo => SnapshotServiceType::MongoDB,
        DokployDbKind::Redis => SnapshotServiceType::Redis,
    }
}

/// Build a service snapshot for one Dokploy database.
///
/// `instance_host` is the Dokploy server's host — databases with an
/// `externalPort` are reachable there for dump/restore.
fn db_to_service_snapshot(
    kind: DokployDbKind,
    db: &DokployDatabase,
    instance_host: Option<&str>,
) -> ServiceSnapshot {
    let reachable_url = match (db.external_port, instance_host) {
        (Some(port), Some(host)) => Some(format!(
            "{}://{}:{}@{}:{}/{}",
            scheme_for(kind),
            db.database_user.as_deref().unwrap_or("postgres"),
            db.database_password.as_deref().unwrap_or(""),
            host,
            port,
            db.database_name.as_deref().unwrap_or(""),
        )),
        _ => None,
    };
    let reachable = reachable_url.is_some();

    ServiceSnapshot {
        id: db.id.clone().unwrap_or_else(|| db.name.clone()),
        name: db.name.clone(),
        service_type: snapshot_type_for(kind),
        version: db.version_from_image(),
        connection_url: reachable_url,
        env_vars: HashMap::new(),
        has_data: true,
        data_size_bytes: None,
        metadata: serde_json::json!({
            "kind": kind.router(),
            "image": db.docker_image,
            "external_port": db.external_port,
            "reachable": reachable,
            "app_name": db.app_name,
        }),
    }
}

/// Exact data-migration command for a database, when one exists
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

/// Deployment configuration for one application snapshot
fn deployment_config(snapshot: &WorkloadSnapshot) -> DeploymentConfiguration {
    let git_url = snapshot
        .source_metadata
        .get("custom_git_url")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let branch = snapshot
        .source_metadata
        .get("custom_git_branch")
        .and_then(|v| v.as_str())
        .unwrap_or("main");
    let build_path = snapshot
        .source_metadata
        .get("custom_git_build_path")
        .and_then(|v| v.as_str())
        .unwrap_or("/");

    let image = snapshot
        .image
        .clone()
        .unwrap_or_else(|| format!("nixpacks:{}#{}", git_url, branch));
    let build = snapshot.image.is_none().then(|| BuildConfiguration {
        context: build_path.to_string(),
        dockerfile: None,
        args: HashMap::new(),
        target: None,
    });

    // Git-built apps carry their repository into the plan so execution can
    // link the temps project to it and run the real deployment pipeline.
    let git = (snapshot.image.is_none() && !git_url.is_empty())
        .then(|| url::Url::parse(git_url).ok())
        .flatten()
        .and_then(|parsed| {
            let host = parsed.host_str()?.to_string();
            let mut segments = parsed.path_segments()?;
            let owner = segments.next()?.to_string();
            let repo = segments.next()?.trim_end_matches(".git").to_string();
            if owner.is_empty() || repo.is_empty() {
                return None;
            }
            Some(GitSourcePlan {
                clone_url: Some(format!("https://{}/{}/{}.git", host, owner, repo)),
                // A plain HTTPS clone URL with no embedded credentials means
                // the repository is cloneable without authentication.
                is_public: parsed.username().is_empty() && parsed.password().is_none(),
                owner,
                repo,
                branch: branch.to_string(),
            })
        });

    let env_vars: Vec<EnvironmentVariable> = {
        let mut vars: Vec<_> = snapshot.env.iter().collect();
        vars.sort_by(|a, b| a.0.cmp(b.0));
        vars.into_iter()
            .map(|(key, value)| EnvironmentVariable {
                key: key.clone(),
                is_secret: temps_import_types::plan::looks_like_secret_env_key(key),
                value: value.clone(),
                source_description: Some("Dokploy environment variable".to_string()),
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
                host_port: None,
                protocol: Protocol::Tcp,
                is_primary: index == 0,
            })
            .collect()
    };

    DeploymentConfiguration {
        image,
        build,
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
        git,
    }
}

// ---------------------------------------------------------------------------
// Plan generation
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct PlanExtras {
    services: Vec<ServiceSnapshot>,
    domains: Vec<DomainSnapshot>,
    additional_workloads: Vec<WorkloadSnapshot>,
    environment_name: String,
}

impl DokployImporter {
    fn build_plan(
        &self,
        snapshot: &WorkloadSnapshot,
        project_name: &str,
        source_id: &str,
        extras: PlanExtras,
    ) -> ImportResult<ImportPlan> {
        let slug = sanitize_slug(project_name);
        let environment_name = if extras.environment_name.is_empty() {
            "production".to_string()
        } else {
            extras.environment_name.clone()
        };

        let has_git = snapshot.image.is_none();
        let deployment = deployment_config(snapshot);
        let additional_deployments: Vec<DeploymentConfiguration> = extras
            .additional_workloads
            .iter()
            .map(deployment_config)
            .collect();

        // -- Services --------------------------------------------------------
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
                        "Database '{}' contains data that will NOT be copied automatically — its external port makes a dump/restore possible from outside",
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
                        "Database '{}' has no external port — temps cannot access its data from outside the Dokploy server",
                        service.name
                    ),
                    recommended_action: Some(
                        "Set an external port on the database in Dokploy before migrating, or dump it on the Dokploy server (docker exec) and restore into the temps-managed service".to_string(),
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
                    // consumed by the orchestrator's data-population phase
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

        // -- Domains ---------------------------------------------------------
        let mut domain_plans = Vec::new();
        for domain in &extras.domains {
            let generated = is_generated_domain(&domain.domain);
            domain_plans.push(DomainPlan {
                domain: domain.domain.clone(),
                environment: environment_name.clone(),
                redirect_to: domain.redirect_to.clone(),
                status_code: domain.redirect_status_code,
                action: if generated {
                    DomainAction::Skip
                } else {
                    DomainAction::Import
                },
                action_description: if generated {
                    "Dokploy-generated wildcard-DNS domain — temps assigns its own preview domain instead".to_string()
                } else {
                    "Register in temps, then point the domain's DNS at the temps server to cut over".to_string()
                },
                replacement: None,
            });
        }
        let custom_domain_count = domain_plans
            .iter()
            .filter(|d| d.action == DomainAction::Import)
            .count();

        // -- Steps -----------------------------------------------------------
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
                    "Set the application's environment variables on the temps environment. Variables referencing Dokploy-internal hostnames must be updated to the new temps service addresses."
                        .to_string(),
                resource_type: StepResourceType::EnvironmentVariable,
                risk: RiskLevel::Low,
                data_implications: vec![],
                pre_conditions: vec![],
                post_conditions: vec![
                    "Review copied values — rewrite any that point at Dokploy-internal hosts".to_string(),
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

        for domain_plan in domain_plans
            .iter()
            .filter(|d| d.action == DomainAction::Import)
        {
            order += 1;
            steps.push(MigrationStep {
                order,
                id: format!("import-domain-{}", sanitize_slug(&domain_plan.domain)),
                title: format!("Import domain '{}'", domain_plan.domain),
                description: domain_plan.action_description.clone(),
                resource_type: StepResourceType::Domain,
                risk: RiskLevel::Medium,
                data_implications: vec![],
                pre_conditions: vec!["Access to the domain's DNS records".to_string()],
                post_conditions: vec![format!(
                    "DNS for '{}' points at the temps server and a certificate was issued",
                    domain_plan.domain
                )],
                skippable: true,
                skipped: false,
                reversible: true,
                estimated_duration: Some("DNS propagation: minutes to hours".to_string()),
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
            description: if has_git {
                "Deploy by building from the linked git repository".to_string()
            } else {
                format!("Deploy the container image '{}'", deployment.image)
            },
            resource_type: StepResourceType::Deployment,
            risk: RiskLevel::Low,
            data_implications: vec![],
            pre_conditions: vec![],
            post_conditions: vec!["Application responds on its temps domain".to_string()],
            skippable: false,
            skipped: false,
            reversible: true,
            estimated_duration: Some(if has_git {
                "build time: 1-10 minutes".to_string()
            } else {
                "10-60 seconds".to_string()
            }),
        });

        // -- Summary ---------------------------------------------------------
        let mut critical_warnings = Vec::new();
        let mut manual_actions = Vec::new();
        for service_plan in &service_plans {
            if service_plan
                .data_implications
                .iter()
                .any(|i| i.severity == DataImplicationSeverity::DataNotMigrated)
            {
                critical_warnings.push(format!(
                    "Database '{}' data cannot be reached from outside the Dokploy server — plan its dump/restore before switching traffic",
                    service_plan.name
                ));
                manual_actions.push(ManualAction {
                    timing: ManualActionTiming::BeforeMigration,
                    description: format!(
                        "Dump database '{}' (set an external port in Dokploy, or docker exec on the server)",
                        service_plan.name
                    ),
                    reason: "The database has no external port, so temps cannot copy its data".to_string(),
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
        if custom_domain_count > 0 {
            manual_actions.push(ManualAction {
                timing: ManualActionTiming::AfterMigration,
                description: format!(
                    "Update DNS for {} custom domain(s) to point at the temps server",
                    custom_domain_count
                ),
                reason: "DNS is controlled at the registrar and cannot be changed by temps"
                    .to_string(),
            });
        }

        let overall_risk = if !service_plans.is_empty() {
            RiskLevel::High
        } else if custom_domain_count > 0 {
            RiskLevel::Medium
        } else {
            RiskLevel::Low
        };

        let workload_count = 1 + additional_deployments.len();
        let summary = MigrationSummary {
            headline: format!(
                "Migrate '{}' from Dokploy: {} application(s), {} database(s), {} custom domain(s)",
                project_name,
                workload_count,
                service_plans.len(),
                custom_domain_count
            ),
            overall_risk,
            resource_counts: ResourceCounts {
                projects: 1,
                environments: 1,
                deployments: workload_count,
                environment_variables: deployment.env_vars.len(),
                services: service_plans.len(),
                domains: custom_domain_count,
            },
            critical_warnings,
            manual_actions_required: manual_actions,
            unsupported_features: vec![UnsupportedFeature {
                feature: "Dokploy compose services".to_string(),
                reason: "Compose-based services are not scanned by this importer yet".to_string(),
                alternative: Some(
                    "Re-create them as temps services or deploy their compose file as a project"
                        .to_string(),
                ),
            }],
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
            source: ImportSource::Dokploy.to_string(),
            source_id: source_id.to_string(),
            project: ProjectConfiguration {
                name: project_name.to_string(),
                slug,
                project_type: if has_git {
                    ProjectType::Git
                } else {
                    ProjectType::Docker
                },
                is_web_app: !extras.domains.is_empty(),
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
                warnings: vec![],
            },
            cost_analysis: None,
        })
    }
}

// ---------------------------------------------------------------------------
// WorkloadImporter implementation
// ---------------------------------------------------------------------------

/// Find the project + environment containing an application id
fn locate_application<'a>(
    projects: &'a [DokployProject],
    application_id: &str,
) -> Option<(&'a DokployProject, &'a crate::model::DokployEnvironment)> {
    for project in projects {
        for environment in &project.environments {
            if environment
                .applications
                .iter()
                .any(|a| a.application_id == application_id)
            {
                return Some((project, environment));
            }
        }
    }
    None
}

#[async_trait]
impl WorkloadImporter for DokployImporter {
    fn source(&self) -> ImportSource {
        ImportSource::Dokploy
    }

    fn name(&self) -> &str {
        "Dokploy"
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
        let client = match DokployClient::from_credentials(credentials).await {
            Ok(client) => client,
            Err(e) => {
                return Ok(CredentialValidation {
                    valid: false,
                    account_name: None,
                    message: Some(e.to_string()),
                })
            }
        };
        match client.projects().await {
            Ok(projects) => Ok(CredentialValidation {
                valid: true,
                account_name: None,
                message: Some(format!(
                    "Connected — instance has {} project(s)",
                    projects.len()
                )),
            }),
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
        let client = DokployClient::from_credentials(credentials)
            .await
            .map_err(ImportError::from)?;
        let projects = client.projects().await.map_err(ImportError::from)?;

        // Database stubs in project.all carry no name; Dokploy has no
        // batch-fetch endpoint, only one .one?id= call per database. Collect
        // every stub needing a name across all projects/environments first,
        // then resolve them all CONCURRENTLY — still N HTTP calls (the API
        // shape leaves no way around that), but N in parallel rather than N
        // sequential round-trips, which is what actually made discover()
        // slow on accounts with many databases.
        let mut unnamed: Vec<(DokployDbKind, String)> = Vec::new();
        for project in &projects {
            for environment in &project.environments {
                for (kind, stubs) in [
                    (DokployDbKind::Postgres, &environment.postgres),
                    (DokployDbKind::Mysql, &environment.mysql),
                    (DokployDbKind::Mariadb, &environment.mariadb),
                    (DokployDbKind::Mongo, &environment.mongo),
                    (DokployDbKind::Redis, &environment.redis),
                ] {
                    for stub in stubs {
                        if stub.name.is_none() {
                            if let Some(id) = &stub.id {
                                unnamed.push((kind, id.clone()));
                            }
                        }
                    }
                }
            }
        }
        let resolved_names: HashMap<(DokployDbKind, String), String> =
            futures_util::future::join_all(unnamed.into_iter().map(|(kind, id)| {
                let client = &client;
                async move {
                    let name = client.database(kind, &id).await.ok().map(|db| db.name);
                    (kind, id, name)
                }
            }))
            .await
            .into_iter()
            .filter_map(|(kind, id, name)| name.map(|n| ((kind, id), n)))
            .collect();

        let mut descriptors = Vec::new();
        for project in &projects {
            for environment in &project.environments {
                for app in &environment.applications {
                    descriptors.push(WorkloadDescriptor {
                        id: WorkloadId::new(app.application_id.clone()),
                        name: app.name.clone(),
                        workload_type: WorkloadType::Container,
                        status: map_status(app.application_status.as_deref()),
                        image: None,
                        created_at: None,
                        labels: HashMap::from([("project".to_string(), project.name.clone())]),
                    });
                }
                for (kind, stubs) in [
                    (DokployDbKind::Postgres, &environment.postgres),
                    (DokployDbKind::Mysql, &environment.mysql),
                    (DokployDbKind::Mariadb, &environment.mariadb),
                    (DokployDbKind::Mongo, &environment.mongo),
                    (DokployDbKind::Redis, &environment.redis),
                ] {
                    for stub in stubs {
                        let Some(id) = &stub.id else { continue };
                        let name = stub
                            .name
                            .clone()
                            .or_else(|| resolved_names.get(&(kind, id.clone())).cloned());
                        descriptors.push(WorkloadDescriptor {
                            id: WorkloadId::new(format!("{}/{}", kind.router(), id)),
                            name,
                            workload_type: WorkloadType::Database,
                            status: WorkloadStatus::Unknown,
                            image: None,
                            created_at: None,
                            labels: HashMap::from([("project".to_string(), project.name.clone())]),
                        });
                    }
                }
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

        info!("Discovered {} Dokploy workloads", descriptors.len());
        Ok(descriptors)
    }

    async fn describe(
        &self,
        credentials: &ImportCredentials,
        workload_id: &WorkloadId,
    ) -> ImportResult<WorkloadSnapshot> {
        let client = DokployClient::from_credentials(credentials)
            .await
            .map_err(ImportError::from)?;
        let app = client
            .application(workload_id.as_str())
            .await
            .map_err(ImportError::from)?;
        Ok(app_to_snapshot(&app))
    }

    async fn describe_project(
        &self,
        credentials: &ImportCredentials,
        workload_id: &WorkloadId,
    ) -> ImportResult<ProjectSnapshot> {
        let client = DokployClient::from_credentials(credentials)
            .await
            .map_err(ImportError::from)?;
        let projects = client.projects().await.map_err(ImportError::from)?;

        let (project, environment) = locate_application(&projects, workload_id.as_str())
            .ok_or_else(|| {
                ImportError::from(DokployImportError::WorkloadNotFound {
                    id: workload_id.as_str().to_string(),
                })
            })?;

        let anchor = client
            .application(workload_id.as_str())
            .await
            .map_err(ImportError::from)?;

        let mut additional_workloads = Vec::new();
        for stub in environment
            .applications
            .iter()
            .filter(|a| a.application_id != anchor.application_id)
        {
            let app = client
                .application(&stub.application_id)
                .await
                .map_err(ImportError::from)?;
            additional_workloads.push(app_to_snapshot(&app));
        }

        let instance_host = client.host();
        let mut services = Vec::new();
        for (kind, stubs) in [
            (DokployDbKind::Postgres, &environment.postgres),
            (DokployDbKind::Mysql, &environment.mysql),
            (DokployDbKind::Mariadb, &environment.mariadb),
            (DokployDbKind::Mongo, &environment.mongo),
            (DokployDbKind::Redis, &environment.redis),
        ] {
            for stub in stubs {
                let Some(id) = &stub.id else { continue };
                let db = client.database(kind, id).await.map_err(ImportError::from)?;
                services.push(db_to_service_snapshot(kind, &db, instance_host.as_deref()));
            }
        }

        let mut domains = Vec::new();
        let mut collect_domains = |app: &DokployApplication| {
            for domain in &app.domains {
                if let Some(host) = &domain.host {
                    domains.push(DomainSnapshot {
                        is_apex: host.chars().filter(|c| *c == '.').count() <= 1,
                        domain: host.clone(),
                        redirect_to: None,
                        redirect_status_code: None,
                        environment: Some(environment.name.clone()),
                        verified: true,
                    });
                }
            }
        };
        collect_domains(&anchor);
        for workload in &additional_workloads {
            if let Some(hosts) = workload
                .source_metadata
                .get("domains")
                .and_then(|v| v.as_array())
            {
                for host in hosts.iter().filter_map(|h| h.as_str()) {
                    domains.push(DomainSnapshot {
                        is_apex: host.chars().filter(|c| *c == '.').count() <= 1,
                        domain: host.to_string(),
                        redirect_to: None,
                        redirect_status_code: None,
                        environment: Some(environment.name.clone()),
                        verified: true,
                    });
                }
            }
        }

        let git_info = git_info_for(&anchor);
        let primary_workload = app_to_snapshot(&anchor);

        Ok(ProjectSnapshot {
            id: workload_id.clone(),
            name: project.name.clone(),
            primary_workload,
            additional_workloads,
            services,
            domains,
            git_info,
            detected_framework: anchor.build_type.clone(),
            source_metadata: serde_json::json!({
                ENVIRONMENT_NAME_KEY: environment.name,
                "dokploy_project_id": project.project_id,
                "dokploy_environment_id": environment.environment_id,
            }),
        })
    }

    fn generate_plan(&self, snapshot: WorkloadSnapshot) -> ImportResult<ImportPlan> {
        let name = snapshot
            .name
            .clone()
            .unwrap_or_else(|| snapshot.id.as_str().to_string());
        let source_id = snapshot.id.as_str().to_string();
        self.build_plan(&snapshot, &name, &source_id, PlanExtras::default())
    }

    fn generate_project_plan(&self, snapshot: ProjectSnapshot) -> ImportResult<ImportPlan> {
        let environment_name = snapshot
            .source_metadata
            .get(ENVIRONMENT_NAME_KEY)
            .and_then(|v| v.as_str())
            .unwrap_or("production")
            .to_string();
        let extras = PlanExtras {
            services: snapshot.services.clone(),
            domains: snapshot.domains.clone(),
            additional_workloads: snapshot.additional_workloads.clone(),
            environment_name,
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
        DokployValidationRules::all_rules()
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
            supports_build: true,
            supports_stacks: true, // a Dokploy environment imports as one project
            supports_services: true,
            supports_domains: true,
            supports_project_snapshot: true,
            supports_cost_analysis: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Execution (same honest-manual pattern as the Coolify/Kubernetes importers)
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
        "Executing Dokploy import for session: {} (dry_run: {})",
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

    // Prefer the repository the user linked in the wizard; otherwise fall
    // back to the public repository the source platform deploys from, so the
    // real deployment pipeline can clone and build it.
    let plan_git = plan
        .deployment
        .git
        .as_ref()
        .filter(|g| context.repo_owner.is_none() && g.is_public);
    let (repo_owner, repo_name, is_public_repo, git_url) = match plan_git {
        Some(git) => (
            Some(git.owner.clone()),
            Some(git.repo.clone()),
            Some(true),
            git.clone_url.clone(),
        ),
        None => (
            context.repo_owner.clone(),
            context.repo_name.clone(),
            None,
            None,
        ),
    };
    let main_branch = plan_git
        .map(|git| git.branch.clone())
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| context.main_branch.clone());

    let create_project_request = temps_projects::services::types::CreateProjectRequest {
        name: context.project_name.clone(),
        repo_name,
        repo_owner,
        directory: context.directory.clone(),
        main_branch,
        preset: context.preset.clone(),
        preset_config: None,
        // is_secret vars carry a real value read from Dokploy's API (unlike
        // Kamal's empty placeholders) — they must reach the deployment or
        // the app boots without its real credentials. temps encrypts every
        // env var at rest regardless of this flag; only the plan API
        // response masks it (mask_plan_secrets in the orchestrator).
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
        is_public_repo,
        git_url,
        git_provider_connection_id: context.git_provider_connection_id,
        exposed_port: None,
        source_type: temps_entities::source_type::SourceType::Git,
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
        builder: Some("dokploy-import".to_string()),
        labels: vec!["imported".to_string(), "dokploy".to_string()],
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
        commit_message: Set(Some(format!("Imported from Dokploy ({})", plan.source_id))),
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
        "✅ Dokploy import completed in {:.2}s — project {} (id: {})",
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

    fn git_app() -> DokployApplication {
        serde_json::from_value(serde_json::json!({
            "applicationId": "app-git",
            "name": "lab-shop",
            "sourceType": "git",
            "customGitUrl": "https://github.com/heroku/node-js-getting-started.git",
            "customGitBranch": "main",
            "customGitBuildPath": "/",
            "buildType": "nixpacks",
            "env": "LAB_SECRET=s3cret\nPLAN=pro",
            "applicationStatus": "running",
            "domains": [{"host": "shop.example.com"}]
        }))
        .unwrap()
    }

    fn docker_app() -> DokployApplication {
        serde_json::from_value(serde_json::json!({
            "applicationId": "app-docker",
            "name": "lab-whoami",
            "sourceType": "docker",
            "dockerImage": "traefik/whoami:latest",
            "applicationStatus": "idle",
            "domains": [{"host": "whoami.10.0.0.1.traefik.me"}]
        }))
        .unwrap()
    }

    fn lab_db(external_port: Option<u16>) -> DokployDatabase {
        serde_json::from_value(serde_json::json!({
            "postgresId": "pg-1",
            "name": "lab-db",
            "appName": "postgres-x",
            "databaseName": "labdb",
            "databaseUser": "lab",
            "databasePassword": "pw",
            "dockerImage": "postgres:16-alpine",
            "externalPort": external_port,
        }))
        .unwrap()
    }

    fn project_snapshot(db_external_port: Option<u16>) -> ProjectSnapshot {
        let anchor = git_app();
        let primary = app_to_snapshot(&anchor);
        let additional = app_to_snapshot(&docker_app());
        let service = db_to_service_snapshot(
            DokployDbKind::Postgres,
            &lab_db(db_external_port),
            Some("1.2.3.4"),
        );
        let domains = vec![
            DomainSnapshot {
                domain: "shop.example.com".to_string(),
                is_apex: false,
                redirect_to: None,
                redirect_status_code: None,
                environment: Some("production".to_string()),
                verified: true,
            },
            DomainSnapshot {
                domain: "whoami.10.0.0.1.traefik.me".to_string(),
                is_apex: false,
                redirect_to: None,
                redirect_status_code: None,
                environment: Some("production".to_string()),
                verified: true,
            },
        ];
        ProjectSnapshot {
            id: WorkloadId::new("app-git"),
            name: "lab".to_string(),
            primary_workload: primary,
            additional_workloads: vec![additional],
            services: vec![service],
            domains,
            git_info: git_info_for(&anchor),
            detected_framework: Some("nixpacks".to_string()),
            source_metadata: serde_json::json!({ ENVIRONMENT_NAME_KEY: "production" }),
        }
    }

    #[test]
    fn status_mapping_covers_dokploy_states() {
        assert_eq!(map_status(Some("running")), WorkloadStatus::Running);
        assert_eq!(map_status(Some("idle")), WorkloadStatus::Stopped);
        assert_eq!(map_status(Some("error")), WorkloadStatus::Failed);
        assert_eq!(map_status(Some("done")), WorkloadStatus::Deployed);
        assert_eq!(map_status(None), WorkloadStatus::Unknown);
    }

    #[test]
    fn git_info_from_custom_url_and_github_fields() {
        let git = git_info_for(&git_app()).unwrap();
        assert_eq!(git.provider, "github");
        assert_eq!(git.owner, "heroku");
        assert_eq!(git.repo, "node-js-getting-started");

        assert!(git_info_for(&docker_app()).is_none());

        let gh_app: DokployApplication = serde_json::from_value(serde_json::json!({
            "applicationId": "gh", "name": "gh-app", "sourceType": "github",
            "owner": "acme", "repository": "shop", "branch": "develop"
        }))
        .unwrap();
        let git = git_info_for(&gh_app).unwrap();
        assert_eq!(git.owner, "acme");
        assert_eq!(git.default_branch, "develop");
    }

    #[test]
    fn generated_domains_recognized() {
        assert!(is_generated_domain("whoami.10.0.0.1.traefik.me"));
        assert!(!is_generated_domain("shop.example.com"));
    }

    #[test]
    fn reachable_db_builds_connection_url() {
        let service = db_to_service_snapshot(
            DokployDbKind::Postgres,
            &lab_db(Some(5432)),
            Some("1.2.3.4"),
        );
        assert_eq!(
            service.connection_url.as_deref(),
            Some("postgres://lab:pw@1.2.3.4:5432/labdb")
        );
        assert_eq!(
            service.metadata.get("reachable").and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn unexposed_db_has_no_connection_url() {
        let service =
            db_to_service_snapshot(DokployDbKind::Postgres, &lab_db(None), Some("1.2.3.4"));
        assert_eq!(service.connection_url, None);
        assert_eq!(
            service.metadata.get("reachable").and_then(|v| v.as_bool()),
            Some(false)
        );
    }

    #[test]
    fn project_plan_covers_apps_databases_and_domains() {
        let importer = DokployImporter::new();
        let plan = importer
            .generate_project_plan(project_snapshot(Some(5432)))
            .unwrap();

        assert_eq!(plan.source, "dokploy");
        assert_eq!(plan.summary.resource_counts.deployments, 2);
        assert_eq!(plan.summary.resource_counts.services, 1);
        assert_eq!(plan.summary.resource_counts.domains, 1); // traefik.me skipped
        assert_eq!(plan.project.project_type, ProjectType::Git);

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
    fn unexposed_database_produces_data_not_migrated_warning() {
        let importer = DokployImporter::new();
        let plan = importer
            .generate_project_plan(project_snapshot(None))
            .unwrap();
        assert!(plan.services[0]
            .data_implications
            .iter()
            .any(|i| i.severity == DataImplicationSeverity::DataNotMigrated));
        assert!(!plan.summary.critical_warnings.is_empty());
    }

    /// Live end-to-end against a real Dokploy instance. Skips unless
    /// DOKPLOY_TEST_URL and DOKPLOY_TEST_TOKEN are set.
    #[tokio::test]
    async fn live_discover_describe_and_plan() {
        let (Ok(url), Ok(token)) = (
            std::env::var("DOKPLOY_TEST_URL"),
            std::env::var("DOKPLOY_TEST_TOKEN"),
        ) else {
            println!("DOKPLOY_TEST_URL/DOKPLOY_TEST_TOKEN not set, skipping live test");
            return;
        };
        let credentials = ImportCredentials::with_token_and_url(token, url);
        let importer = DokployImporter::new();

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
        let anchor = discovered
            .iter()
            .find(|d| d.workload_type == WorkloadType::Container)
            .expect("at least one application");

        let snapshot = importer
            .describe_project(&credentials, &anchor.id)
            .await
            .unwrap();
        let plan = importer.generate_project_plan(snapshot).unwrap();
        assert_eq!(plan.source, "dokploy");
        assert!(!plan.steps.is_empty());
        println!(
            "live plan: {} — {} step(s), {} service(s), {} domain(s)",
            plan.summary.headline,
            plan.steps.len(),
            plan.services.len(),
            plan.domains.len()
        );
    }
}
