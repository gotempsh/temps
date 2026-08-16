//! Coolify importer — WorkloadImporter implementation
//!
//! The Coolify *environment* (inside a Coolify project) is the project
//! boundary for migration: importing an application pulls in its sibling
//! applications, the environment's standalone databases (as managed-service
//! candidates with data-migration guidance), and every application FQDN as a
//! domain.

use crate::client::CoolifyClient;
use crate::error::CoolifyImportError;
use crate::model::{CoolifyApplication, CoolifyDatabase, CoolifyEnvVar};
use crate::validation::CoolifyValidationRules;
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

/// Domain suffixes that are wildcard-DNS conveniences, not real customer
/// domains. These are planned as skipped — temps generates its own preview
/// domains.
const GENERATED_DOMAIN_SUFFIXES: &[&str] = &[".sslip.io", ".nip.io", ".traefik.me"];

/// Key inside `ProjectSnapshot.source_metadata` carrying the environment name
const ENVIRONMENT_NAME_KEY: &str = "environment_name";

/// Coolify platform importer
pub struct CoolifyImporter {
    version: String,
}

impl Default for CoolifyImporter {
    fn default() -> Self {
        Self::new()
    }
}

impl CoolifyImporter {
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

/// Map Coolify's "state:health" status string to a workload status
fn map_status(status: Option<&str>) -> WorkloadStatus {
    let state = status.unwrap_or("").split(':').next().unwrap_or("");
    match state {
        "running" => WorkloadStatus::Running,
        "exited" => WorkloadStatus::Exited,
        "stopped" => WorkloadStatus::Stopped,
        "paused" => WorkloadStatus::Paused,
        "starting" | "building" => WorkloadStatus::Building,
        "" => WorkloadStatus::Unknown,
        _ => WorkloadStatus::Unknown,
    }
}

/// Parse Coolify's `ports_exposes` ("3000" or "3000,9090") into ports
fn parse_ports(ports_exposes: Option<&str>) -> Vec<u16> {
    ports_exposes
        .unwrap_or("")
        .split(',')
        .filter_map(|p| p.trim().parse::<u16>().ok())
        .collect()
}

/// Extract hostnames from Coolify's comma-separated `fqdn` URL list
fn parse_fqdn_hosts(fqdn: Option<&str>) -> Vec<String> {
    fqdn.unwrap_or("")
        .split(',')
        .filter_map(|raw| {
            let raw = raw.trim();
            if raw.is_empty() {
                return None;
            }
            // Accept both full URLs and bare hosts
            if let Ok(url) = url::Url::parse(raw) {
                url.host_str().map(|h| h.to_string())
            } else {
                Some(raw.trim_matches('/').to_string())
            }
        })
        .collect()
}

/// Whether a hostname is a wildcard-DNS convenience domain
fn is_generated_domain(host: &str) -> bool {
    GENERATED_DOMAIN_SUFFIXES
        .iter()
        .any(|suffix| host.ends_with(suffix))
}

/// Parse a Coolify `git_repository` value into git info.
///
/// Coolify stores either a short `owner/repo` (GitHub implied), a full HTTPS
/// URL, or an SSH URL. Image-based apps hold a placeholder — callers must
/// check [`CoolifyApplication::is_image_based`] first.
fn parse_git_info(repository: &str, branch: Option<&str>) -> Option<GitInfo> {
    let branch = branch.unwrap_or("main").to_string();
    let repository = repository.trim();
    if repository.is_empty() {
        return None;
    }

    // SSH form: git@github.com:owner/repo.git
    if let Some(rest) = repository.strip_prefix("git@") {
        let (host, path) = rest.split_once(':')?;
        let path = path.trim_end_matches(".git");
        let (owner, repo) = path.split_once('/')?;
        return Some(GitInfo {
            provider: provider_from_host(host),
            owner: owner.to_string(),
            repo: repo.to_string(),
            default_branch: branch,
            clone_url: Some(format!("https://{}/{}/{}.git", host, owner, repo)),
        });
    }

    // Full URL form
    if repository.starts_with("http://") || repository.starts_with("https://") {
        let url = url::Url::parse(repository).ok()?;
        let host = url.host_str()?.to_string();
        let mut segments = url.path_segments()?;
        let owner = segments.next()?.to_string();
        let repo = segments.next()?.trim_end_matches(".git").to_string();
        if owner.is_empty() || repo.is_empty() {
            return None;
        }
        return Some(GitInfo {
            provider: provider_from_host(&host),
            owner: owner.clone(),
            repo: repo.clone(),
            default_branch: branch,
            clone_url: Some(format!("https://{}/{}/{}.git", host, owner, repo)),
        });
    }

    // Short form: owner/repo (GitHub implied — Coolify's default source)
    let (owner, repo) = repository.split_once('/')?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some(GitInfo {
        provider: "github".to_string(),
        owner: owner.to_string(),
        repo: repo.trim_end_matches(".git").to_string(),
        default_branch: branch,
        clone_url: Some(format!(
            "https://github.com/{}/{}.git",
            owner,
            repo.trim_end_matches(".git")
        )),
    })
}

fn provider_from_host(host: &str) -> String {
    if host.contains("github") {
        "github".to_string()
    } else if host.contains("gitlab") {
        "gitlab".to_string()
    } else if host.contains("bitbucket") {
        "bitbucket".to_string()
    } else {
        host.to_string()
    }
}

/// Map Coolify's `database_type` to snapshot + temps service identifiers
fn map_database_type(database_type: Option<&str>) -> (SnapshotServiceType, String) {
    match database_type.unwrap_or("") {
        "standalone-postgresql" => (SnapshotServiceType::Postgres, "postgres".to_string()),
        "standalone-mysql" => (SnapshotServiceType::Mysql, "mysql".to_string()),
        "standalone-mariadb" => (SnapshotServiceType::Mysql, "mariadb".to_string()),
        "standalone-redis" | "standalone-keydb" | "standalone-dragonfly" => {
            (SnapshotServiceType::Redis, "redis".to_string())
        }
        "standalone-mongodb" => (SnapshotServiceType::MongoDB, "mongodb".to_string()),
        other => (
            SnapshotServiceType::Other(other.to_string()),
            other.trim_start_matches("standalone-").to_string(),
        ),
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

/// Build a workload snapshot for one Coolify application
fn app_to_snapshot(app: &CoolifyApplication, envs: &[CoolifyEnvVar]) -> WorkloadSnapshot {
    let env: HashMap<String, String> = envs
        .iter()
        .filter(|e| !e.is_preview)
        .map(|e| (e.key.clone(), e.effective_value().to_string()))
        .collect();

    let ports: HashMap<u16, Option<u16>> = parse_ports(app.ports_exposes.as_deref())
        .into_iter()
        .map(|p| (p, None))
        .collect();

    let source_metadata = serde_json::json!({
        "build_pack": app.build_pack,
        "git_repository": app.git_repository,
        "git_branch": app.git_branch,
        "private_key_id": app.private_key_id,
        "base_directory": app.base_directory,
        "fqdn": app.fqdn,
        "coolify_status": app.status,
    });

    WorkloadSnapshot {
        id: WorkloadId::new(app.uuid.clone()),
        name: Some(app.name.clone()),
        workload_type: WorkloadType::Container,
        status: map_status(app.status.as_deref()),
        image: app.image_reference(),
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
        labels: HashMap::new(),
        health_check: None,
        restart_policy: None,
        created_at: chrono::Utc::now(),
        source_metadata,
    }
}

/// Build a service snapshot for one Coolify standalone database
fn db_to_service_snapshot(db: &CoolifyDatabase) -> ServiceSnapshot {
    let (service_type, _) = map_database_type(db.database_type.as_deref());
    ServiceSnapshot {
        id: db.uuid.clone(),
        name: db.name.clone(),
        service_type,
        version: db.version_from_image(),
        // Only ever the externally-reachable URL, never `internal_db_url`
        // (Coolify's own docker-network-internal address) -- that value is
        // attacker-controlled (comes verbatim from the source platform's
        // API) and must never reach the dump/restore container, which runs
        // on the host network. `reachable`/`source_url` below are already
        // gated on this being `Some`, but keeping a dangerous value out of
        // this field entirely (matching CapRover/Dokploy/Portainer, none of
        // which have this fallback) removes the risk for any future
        // consumer that doesn't know to re-check that gate.
        connection_url: db.reachable_url().map(|u| u.to_string()),
        env_vars: HashMap::new(),
        has_data: true,
        data_size_bytes: None,
        metadata: serde_json::json!({
            "database_type": db.database_type,
            "image": db.image,
            "is_public": db.is_public.unwrap_or(false),
            "public_port": db.public_port,
            "reachable": db.reachable_url().is_some(),
        }),
    }
}

/// Deployment configuration for one application snapshot
fn deployment_config(snapshot: &WorkloadSnapshot) -> DeploymentConfiguration {
    let git_repository = snapshot
        .source_metadata
        .get("git_repository")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let git_branch = snapshot
        .source_metadata
        .get("git_branch")
        .and_then(|v| v.as_str())
        .unwrap_or("main");
    let base_directory = snapshot
        .source_metadata
        .get("base_directory")
        .and_then(|v| v.as_str())
        .unwrap_or("/");

    let image = snapshot
        .image
        .clone()
        .unwrap_or_else(|| format!("nixpacks:{}#{}", git_repository, git_branch));

    let build = snapshot.image.is_none().then(|| BuildConfiguration {
        context: base_directory.to_string(),
        dockerfile: None,
        args: HashMap::new(),
        target: None,
    });

    // Git-built apps carry their repository into the plan so execution can
    // link the temps project to it and run the real deployment pipeline.
    let git = snapshot
        .image
        .is_none()
        .then(|| parse_git_info(git_repository, Some(git_branch)))
        .flatten()
        .map(|info| GitSourcePlan {
            owner: info.owner,
            repo: info.repo,
            branch: info.default_branch,
            clone_url: info.clone_url,
            is_public: snapshot
                .source_metadata
                .get("private_key_id")
                .map(|v| v.is_null())
                .unwrap_or(true),
        });

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

    let env_vars: Vec<EnvironmentVariable> = {
        let mut vars: Vec<_> = snapshot.env.iter().collect();
        vars.sort_by(|a, b| a.0.cmp(b.0));
        vars.into_iter()
            .map(|(key, value)| EnvironmentVariable {
                key: key.clone(),
                is_secret: temps_import_types::plan::looks_like_secret_env_key(key),
                value: value.clone(),
                source_description: Some("Coolify environment variable".to_string()),
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

/// Project-level inputs beyond the primary workload
#[derive(Debug, Default)]
struct PlanExtras {
    services: Vec<ServiceSnapshot>,
    domains: Vec<DomainSnapshot>,
    additional_workloads: Vec<WorkloadSnapshot>,
    environment_name: String,
}

impl CoolifyImporter {
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

        let has_git = snapshot
            .source_metadata
            .get("git_repository")
            .and_then(|v| v.as_str())
            .map(|r| !r.is_empty())
            .unwrap_or(false)
            && snapshot.image.is_none();

        let deployment = deployment_config(snapshot);
        let additional_deployments: Vec<DeploymentConfiguration> = extras
            .additional_workloads
            .iter()
            .map(deployment_config)
            .collect();

        // -- Services --------------------------------------------------------
        let mut service_plans = Vec::new();
        for service in &extras.services {
            let (_, temps_type) = (
                &service.service_type,
                match &service.service_type {
                    SnapshotServiceType::Postgres => "postgres".to_string(),
                    SnapshotServiceType::Mysql => "mysql".to_string(),
                    SnapshotServiceType::Redis => "redis".to_string(),
                    SnapshotServiceType::MongoDB => "mongodb".to_string(),
                    SnapshotServiceType::S3 => "s3".to_string(),
                    SnapshotServiceType::Kv => "kv".to_string(),
                    SnapshotServiceType::Blob => "blob".to_string(),
                    SnapshotServiceType::Other(name) => name.clone(),
                },
            );
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
                        "Database '{}' contains data that will NOT be copied automatically — it is reachable from outside, so a dump/restore works",
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
                        "Database '{}' is not publicly reachable — temps cannot access its data from outside the Coolify server",
                        service.name
                    ),
                    recommended_action: Some(
                        "Either enable 'public port' on the database in Coolify before migrating, or dump it on the Coolify server (docker exec) and restore into the temps-managed service".to_string(),
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
                    "Coolify-generated wildcard-DNS domain — temps assigns its own preview domain instead".to_string()
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
                    "Set the application's environment variables on the temps environment. Variables referencing Coolify-internal hostnames must be updated to the new temps service addresses."
                        .to_string(),
                resource_type: StepResourceType::EnvironmentVariable,
                risk: RiskLevel::Low,
                data_implications: vec![],
                pre_conditions: vec![],
                post_conditions: vec![
                    "Review copied values — rewrite any that point at Coolify-internal hosts".to_string(),
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
                    "Database '{}' data cannot be reached from outside the Coolify server — plan its dump/restore before switching traffic",
                    service_plan.name
                ));
                manual_actions.push(ManualAction {
                    timing: ManualActionTiming::BeforeMigration,
                    description: format!(
                        "Dump database '{}' (enable its public port in Coolify, or docker exec on the server)",
                        service_plan.name
                    ),
                    reason: "The database is not publicly reachable, so temps cannot copy its data".to_string(),
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
                "Migrate '{}' from Coolify: {} application(s), {} database(s), {} custom domain(s)",
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
                feature: "Coolify one-click services".to_string(),
                reason: "Compose-based one-click services (from Coolify's service catalog) are not scanned by this importer".to_string(),
                alternative: Some(
                    "Re-create them as temps services or deploy their compose file as a project".to_string(),
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
            source: ImportSource::Coolify.to_string(),
            source_id: source_id.to_string(),
            project: ProjectConfiguration {
                name: project_name.to_string(),
                slug,
                project_type: if has_git {
                    ProjectType::Git
                } else {
                    ProjectType::Docker
                },
                is_web_app: !extras.domains.is_empty() || !deployment.ports.is_empty(),
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

#[async_trait]
impl WorkloadImporter for CoolifyImporter {
    fn source(&self) -> ImportSource {
        ImportSource::Coolify
    }

    fn name(&self) -> &str {
        "Coolify"
    }

    fn version(&self) -> &str {
        &self.version
    }

    async fn health_check(&self) -> ImportResult<bool> {
        // Instance reachability depends entirely on per-request credentials.
        Ok(true)
    }

    async fn validate_credentials(
        &self,
        credentials: &ImportCredentials,
    ) -> ImportResult<CredentialValidation> {
        let client = match CoolifyClient::from_credentials(credentials).await {
            Ok(client) => client,
            Err(e) => {
                return Ok(CredentialValidation {
                    valid: false,
                    account_name: None,
                    message: Some(e.to_string()),
                })
            }
        };
        match client.servers().await {
            Ok(servers) => Ok(CredentialValidation {
                valid: true,
                account_name: servers.first().map(|s| s.name.clone()),
                message: Some(format!(
                    "Connected — instance manages {} server(s)",
                    servers.len()
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
        let client = CoolifyClient::from_credentials(credentials)
            .await
            .map_err(ImportError::from)?;
        let applications = client.applications().await.map_err(ImportError::from)?;
        let databases = client.databases().await.map_err(ImportError::from)?;

        let mut descriptors: Vec<WorkloadDescriptor> = applications
            .iter()
            .map(|app| WorkloadDescriptor {
                id: WorkloadId::new(app.uuid.clone()),
                name: Some(app.name.clone()),
                workload_type: WorkloadType::Container,
                status: map_status(app.status.as_deref()),
                image: app.image_reference(),
                created_at: None,
                labels: HashMap::new(),
            })
            .collect();

        descriptors.extend(databases.iter().map(|db| WorkloadDescriptor {
            id: WorkloadId::new(db.uuid.clone()),
            name: Some(db.name.clone()),
            workload_type: WorkloadType::Database,
            status: WorkloadStatus::Unknown,
            image: db.image.clone(),
            created_at: None,
            labels: HashMap::new(),
        }));

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

        info!("Discovered {} Coolify workloads", descriptors.len());
        Ok(descriptors)
    }

    async fn describe(
        &self,
        credentials: &ImportCredentials,
        workload_id: &WorkloadId,
    ) -> ImportResult<WorkloadSnapshot> {
        let client = CoolifyClient::from_credentials(credentials)
            .await
            .map_err(ImportError::from)?;
        let applications = client.applications().await.map_err(ImportError::from)?;
        let app = applications
            .iter()
            .find(|a| a.uuid == workload_id.as_str())
            .ok_or_else(|| {
                ImportError::from(CoolifyImportError::WorkloadNotFound {
                    uuid: workload_id.as_str().to_string(),
                })
            })?;
        let envs = client
            .application_envs(&app.uuid)
            .await
            .map_err(ImportError::from)?;
        Ok(app_to_snapshot(app, &envs))
    }

    async fn describe_project(
        &self,
        credentials: &ImportCredentials,
        workload_id: &WorkloadId,
    ) -> ImportResult<ProjectSnapshot> {
        let client = CoolifyClient::from_credentials(credentials)
            .await
            .map_err(ImportError::from)?;
        let applications = client.applications().await.map_err(ImportError::from)?;
        let databases = client.databases().await.map_err(ImportError::from)?;

        let anchor = applications
            .iter()
            .find(|a| a.uuid == workload_id.as_str())
            .ok_or_else(|| {
                if databases.iter().any(|d| d.uuid == workload_id.as_str()) {
                    ImportError::InvalidConfiguration(format!(
                        "'{}' is a standalone database — select one of the project's applications instead; its databases are imported alongside it",
                        workload_id
                    ))
                } else {
                    ImportError::from(CoolifyImportError::WorkloadNotFound {
                        uuid: workload_id.as_str().to_string(),
                    })
                }
            })?;

        // Resolve the Coolify project + environment name for the anchor
        let environment_id = anchor.environment_id;
        let mut project_name = anchor.name.clone();
        let mut environment_name = "production".to_string();
        if let Some(env_id) = environment_id {
            let projects = client.projects().await.map_err(ImportError::from)?;
            let mut resolved = false;
            for project in &projects {
                let detail = client
                    .project_detail(&project.uuid)
                    .await
                    .map_err(ImportError::from)?;
                if let Some(environment) = detail.environments.iter().find(|e| e.id == env_id) {
                    project_name = detail.name.clone();
                    environment_name = environment.name.clone();
                    resolved = true;
                    break;
                }
            }
            if !resolved {
                return Err(ImportError::from(CoolifyImportError::ProjectResolution {
                    environment_id: env_id,
                }));
            }
        }

        // Everything in the same Coolify environment belongs to the migration
        let in_environment = |candidate: Option<i64>| match (environment_id, candidate) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        };

        let mut additional_workloads = Vec::new();
        for app in applications
            .iter()
            .filter(|a| a.uuid != anchor.uuid && in_environment(a.environment_id))
        {
            let envs = client
                .application_envs(&app.uuid)
                .await
                .map_err(ImportError::from)?;
            additional_workloads.push(app_to_snapshot(app, &envs));
        }

        let services: Vec<ServiceSnapshot> = databases
            .iter()
            .filter(|db| in_environment(db.environment_id))
            .map(db_to_service_snapshot)
            .collect();

        let mut domains = Vec::new();
        for app in applications
            .iter()
            .filter(|a| a.uuid == anchor.uuid || in_environment(a.environment_id))
        {
            for host in parse_fqdn_hosts(app.fqdn.as_deref()) {
                domains.push(DomainSnapshot {
                    is_apex: host.chars().filter(|c| *c == '.').count() <= 1,
                    domain: host,
                    redirect_to: None,
                    redirect_status_code: None,
                    environment: Some(environment_name.clone()),
                    verified: true,
                });
            }
        }

        let anchor_envs = client
            .application_envs(&anchor.uuid)
            .await
            .map_err(ImportError::from)?;
        let primary_workload = app_to_snapshot(anchor, &anchor_envs);

        let git_info = (!anchor.is_image_based())
            .then(|| {
                anchor
                    .git_repository
                    .as_deref()
                    .and_then(|repo| parse_git_info(repo, anchor.git_branch.as_deref()))
            })
            .flatten();

        Ok(ProjectSnapshot {
            id: workload_id.clone(),
            name: project_name,
            primary_workload,
            additional_workloads,
            services,
            domains,
            git_info,
            detected_framework: anchor.build_pack.clone(),
            source_metadata: serde_json::json!({
                ENVIRONMENT_NAME_KEY: environment_name,
                "coolify_environment_id": environment_id,
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
        CoolifyValidationRules::all_rules()
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
            supports_build: true,  // git-based Coolify apps rebuild on temps
            supports_stacks: true, // a Coolify environment imports as one project
            supports_services: true,
            supports_domains: true,
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
        "Executing Coolify import for session: {} (dry_run: {})",
        context.session_id, context.dry_run
    );

    // Service + domain creation is reported honestly as manual until the
    // orchestrator wires the managed-service and domain services.
    let step_results: Vec<StepResult> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    if context.dry_run {
        info!(
            "Dry run — would create project '{}' with environment '{}' and image '{}'",
            context.project_name, plan.environment.name, plan.deployment.image
        );
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
    // real deployment pipeline can clone and build it. Private source repos
    // without a linked connection stay unlinked — the deploy step reports
    // that honestly instead of failing mid-clone.
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
        // is_secret vars carry a real value read from Coolify's API (unlike
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
    info!(
        "✓ Created project {} (id: {}) from Coolify import",
        project.name, project.id
    );

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
        builder: Some("coolify-import".to_string()),
        labels: vec!["imported".to_string(), "coolify".to_string()],
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
        commit_message: Set(Some(format!("Imported from Coolify ({})", plan.source_id))),
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
        "✅ Coolify import completed in {:.2}s — project {} (id: {}), environment {} (id: {}), deployment {} (id: {})",
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

    fn git_app() -> CoolifyApplication {
        serde_json::from_value(serde_json::json!({
            "uuid": "app-git-uuid",
            "name": "lab-shop",
            "environment_id": 2,
            "git_repository": "heroku/node-js-getting-started",
            "git_branch": "main",
            "build_pack": "nixpacks",
            "fqdn": "http://app-git-uuid.167.233.223.170.sslip.io",
            "ports_exposes": "5006",
            "status": "running:unknown"
        }))
        .unwrap()
    }

    fn image_app() -> CoolifyApplication {
        serde_json::from_value(serde_json::json!({
            "uuid": "app-image-uuid",
            "name": "lab-whoami",
            "environment_id": 2,
            "git_repository": "coollabsio/coolify",
            "git_branch": "main",
            "build_pack": "dockerimage",
            "fqdn": "https://whoami.example.com,http://app-image-uuid.1.2.3.4.sslip.io",
            "ports_exposes": "80",
            "docker_registry_image_name": "traefik/whoami",
            "docker_registry_image_tag": "latest",
            "status": "exited:unhealthy"
        }))
        .unwrap()
    }

    fn lab_db() -> CoolifyDatabase {
        serde_json::from_value(serde_json::json!({
            "uuid": "db-uuid",
            "name": "lab-db",
            "database_type": "standalone-postgresql",
            "image": "postgres:16-alpine",
            "is_public": true,
            "public_port": 5432,
            "external_db_url": "postgres://postgres:pw@1.2.3.4:5432/postgres",
            "internal_db_url": "postgres://postgres:pw@db-uuid:5432/postgres",
            "environment_id": 2
        }))
        .unwrap()
    }

    fn project_snapshot() -> ProjectSnapshot {
        let anchor = git_app();
        let envs = vec![
            CoolifyEnvVar {
                key: "PLAN".to_string(),
                value: Some("pro".to_string()),
                real_value: Some("pro".to_string()),
                is_preview: false,
                is_shown_once: false,
                is_coolify: false,
            },
            CoolifyEnvVar {
                key: "PREVIEW_ONLY".to_string(),
                value: Some("x".to_string()),
                real_value: Some("x".to_string()),
                is_preview: true,
                is_shown_once: false,
                is_coolify: false,
            },
        ];
        let primary = app_to_snapshot(&anchor, &envs);
        let additional = app_to_snapshot(&image_app(), &[]);
        let domains = vec![
            DomainSnapshot {
                domain: "whoami.example.com".to_string(),
                is_apex: false,
                redirect_to: None,
                redirect_status_code: None,
                environment: Some("production".to_string()),
                verified: true,
            },
            DomainSnapshot {
                domain: "app-git-uuid.167.233.223.170.sslip.io".to_string(),
                is_apex: false,
                redirect_to: None,
                redirect_status_code: None,
                environment: Some("production".to_string()),
                verified: true,
            },
        ];
        ProjectSnapshot {
            id: WorkloadId::new("app-git-uuid"),
            name: "My first project".to_string(),
            primary_workload: primary,
            additional_workloads: vec![additional],
            services: vec![db_to_service_snapshot(&lab_db())],
            domains,
            git_info: parse_git_info("heroku/node-js-getting-started", Some("main")),
            detected_framework: Some("nixpacks".to_string()),
            source_metadata: serde_json::json!({ ENVIRONMENT_NAME_KEY: "production" }),
        }
    }

    #[test]
    fn status_mapping_covers_coolify_states() {
        assert_eq!(map_status(Some("running:unknown")), WorkloadStatus::Running);
        assert_eq!(map_status(Some("exited:unhealthy")), WorkloadStatus::Exited);
        assert_eq!(map_status(Some("stopped")), WorkloadStatus::Stopped);
        assert_eq!(map_status(None), WorkloadStatus::Unknown);
    }

    #[test]
    fn parses_single_and_multiple_ports() {
        assert_eq!(parse_ports(Some("5006")), vec![5006]);
        assert_eq!(parse_ports(Some("3000, 9090")), vec![3000, 9090]);
        assert_eq!(parse_ports(Some("")), Vec::<u16>::new());
        assert_eq!(parse_ports(None), Vec::<u16>::new());
    }

    #[test]
    fn parses_fqdn_lists_into_hosts() {
        let hosts = parse_fqdn_hosts(Some("https://whoami.example.com,http://x.1.2.3.4.sslip.io"));
        assert_eq!(hosts, vec!["whoami.example.com", "x.1.2.3.4.sslip.io"]);
    }

    #[test]
    fn recognizes_generated_domains() {
        assert!(is_generated_domain("x.1.2.3.4.sslip.io"));
        assert!(!is_generated_domain("shop.example.com"));
    }

    #[test]
    fn parses_short_https_and_ssh_git_forms() {
        let short = parse_git_info("heroku/node-js-getting-started", Some("main")).unwrap();
        assert_eq!(short.provider, "github");
        assert_eq!(short.owner, "heroku");
        assert_eq!(
            short.clone_url.as_deref(),
            Some("https://github.com/heroku/node-js-getting-started.git")
        );

        let https = parse_git_info("https://gitlab.com/acme/shop.git", None).unwrap();
        assert_eq!(https.provider, "gitlab");
        assert_eq!(https.owner, "acme");
        assert_eq!(https.repo, "shop");

        let ssh = parse_git_info("git@github.com:acme/api.git", Some("develop")).unwrap();
        assert_eq!(ssh.provider, "github");
        assert_eq!(ssh.repo, "api");
        assert_eq!(ssh.default_branch, "develop");

        assert!(parse_git_info("", None).is_none());
        assert!(parse_git_info("not-a-repo", None).is_none());
    }

    #[test]
    fn snapshot_excludes_preview_env_vars() {
        let envs = vec![
            CoolifyEnvVar {
                key: "KEEP".to_string(),
                value: Some("1".to_string()),
                real_value: None,
                is_preview: false,
                is_shown_once: false,
                is_coolify: false,
            },
            CoolifyEnvVar {
                key: "DROP".to_string(),
                value: Some("2".to_string()),
                real_value: None,
                is_preview: true,
                is_shown_once: false,
                is_coolify: false,
            },
        ];
        let snapshot = app_to_snapshot(&git_app(), &envs);
        assert!(snapshot.env.contains_key("KEEP"));
        assert!(!snapshot.env.contains_key("DROP"));
        assert_eq!(snapshot.ports.len(), 1);
        assert!(snapshot.image.is_none()); // git app builds from source
    }

    #[test]
    fn image_app_snapshot_has_image_reference() {
        let snapshot = app_to_snapshot(&image_app(), &[]);
        assert_eq!(snapshot.image.as_deref(), Some("traefik/whoami:latest"));
        assert_eq!(snapshot.status, WorkloadStatus::Exited);
    }

    #[test]
    fn database_service_snapshot_reports_reachability() {
        let service = db_to_service_snapshot(&lab_db());
        assert_eq!(service.service_type, SnapshotServiceType::Postgres);
        assert_eq!(service.version.as_deref(), Some("16"));
        assert!(service.has_data);
        assert_eq!(
            service.metadata.get("reachable").and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn project_plan_covers_apps_databases_and_domains() {
        let importer = CoolifyImporter::new();
        let plan = importer.generate_project_plan(project_snapshot()).unwrap();

        assert_eq!(plan.source, "coolify");
        assert_eq!(plan.summary.resource_counts.deployments, 2);
        assert_eq!(plan.summary.resource_counts.services, 1);
        // Only the custom domain is imported; the sslip.io one is skipped
        assert_eq!(plan.summary.resource_counts.domains, 1);
        let skipped: Vec<_> = plan
            .domains
            .iter()
            .filter(|d| d.action == DomainAction::Skip)
            .collect();
        assert_eq!(skipped.len(), 1);
        assert!(skipped[0].domain.ends_with(".sslip.io"));

        // The reachable database gets an exact dump command
        let db_step = plan
            .steps
            .iter()
            .find(|s| s.id == "create-service-lab-db")
            .expect("database step present");
        assert!(db_step.data_implications.iter().any(|i| i
            .recommended_action
            .as_deref()
            .is_some_and(|a| a.contains("pg_dump"))));

        // Steps are contiguously ordered and end with the deploy step
        let orders: Vec<usize> = plan.steps.iter().map(|s| s.order).collect();
        assert_eq!(orders, (1..=plan.steps.len()).collect::<Vec<_>>());
        assert_eq!(plan.steps.last().unwrap().id, "deploy-application");

        assert_eq!(plan.summary.overall_risk, RiskLevel::High);
        assert_eq!(plan.project.project_type, ProjectType::Git);

        // The plan carries the git source so execution can link the project
        // and run the real deployment pipeline (short form implies GitHub,
        // no private key means public).
        let git = plan.deployment.git.as_ref().expect("git source in plan");
        assert_eq!(git.owner, "heroku");
        assert_eq!(git.repo, "node-js-getting-started");
        assert_eq!(git.branch, "main");
        assert!(git.is_public);
        assert_eq!(
            git.clone_url.as_deref(),
            Some("https://github.com/heroku/node-js-getting-started.git")
        );
    }

    #[test]
    fn unreachable_database_produces_data_not_migrated_warning() {
        let mut snapshot = project_snapshot();
        snapshot.services[0].metadata["reachable"] = serde_json::json!(false);
        snapshot.services[0].connection_url = None;

        let importer = CoolifyImporter::new();
        let plan = importer.generate_project_plan(snapshot).unwrap();

        let service = &plan.services[0];
        assert!(service
            .data_implications
            .iter()
            .any(|i| i.severity == DataImplicationSeverity::DataNotMigrated));
        assert!(!plan.summary.critical_warnings.is_empty());
        assert!(plan
            .summary
            .manual_actions_required
            .iter()
            .any(|a| a.timing == ManualActionTiming::BeforeMigration));
    }

    /// Live end-to-end against a real Coolify instance. Skips unless
    /// COOLIFY_TEST_URL and COOLIFY_TEST_TOKEN are set (e.g. a migration-lab
    /// box seeded with lab-shop / lab-whoami / lab-db).
    #[tokio::test]
    async fn live_discover_describe_and_plan() {
        let (Ok(url), Ok(token)) = (
            std::env::var("COOLIFY_TEST_URL"),
            std::env::var("COOLIFY_TEST_TOKEN"),
        ) else {
            println!("COOLIFY_TEST_URL/COOLIFY_TEST_TOKEN not set, skipping live test");
            return;
        };
        let credentials = ImportCredentials::with_token_and_url(token, url);
        let importer = CoolifyImporter::new();

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
        assert_eq!(plan.source, "coolify");
        assert!(!plan.steps.is_empty());
        println!(
            "live plan: {} — {} step(s), {} service(s), {} domain(s)",
            plan.summary.headline,
            plan.steps.len(),
            plan.services.len(),
            plan.domains.len()
        );
    }

    #[test]
    fn workload_plan_without_extras_is_low_complexity() {
        let importer = CoolifyImporter::new();
        let snapshot = app_to_snapshot(&image_app(), &[]);
        let plan = importer.generate_plan(snapshot).unwrap();
        assert_eq!(plan.summary.resource_counts.services, 0);
        assert_eq!(plan.metadata.complexity, PlanComplexity::Low);
        assert_eq!(plan.project.project_type, ProjectType::Docker);
    }
}
