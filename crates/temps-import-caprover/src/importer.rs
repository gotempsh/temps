//! CapRover importer — WorkloadImporter implementation
//!
//! CapRover has no project grouping, so the migration path is app-centric:
//! the selected app is the primary workload, database-image apps (one-click
//! databases) become managed-service candidates, and custom + generated
//! domains become the domain plan.

use crate::client::CaproverClient;
use crate::error::CaproverImportError;
use crate::model::CaproverApp;
use crate::validation::CaproverValidationRules;
use async_trait::async_trait;
use std::collections::HashMap;
use temps_import_types::plan::{
    DeploymentConfiguration, EnvironmentConfiguration, PlanComplexity, PlanMetadata,
    ProjectConfiguration, ProjectType, Protocol,
};
use temps_import_types::{
    BuildConfiguration, CredentialValidation, DataImplication, DataImplicationSeverity,
    DeploymentStrategy, DomainAction, DomainPlan, DomainSnapshot, EnvironmentVariable, GitInfo,
    GitSourcePlan, ImportContext, ImportCredentials, ImportError, ImportOutcome, ImportPlan,
    ImportResult, ImportSelector, ImportServiceProvider, ImportSource, ImporterCapabilities,
    ManualAction, ManualActionTiming, MigrationStep, MigrationSummary, NetworkConfiguration,
    NetworkInfo, NetworkMode, PortMapping, ProjectSnapshot, ResourceCounts, ResourceInfo,
    ResourceLimits, RiskLevel, ServiceAction, ServicePlan, ServiceSnapshot, SnapshotServiceType,
    StepResourceType, StepResult, UnsupportedFeature, WorkloadDescriptor, WorkloadId,
    WorkloadImporter, WorkloadSnapshot, WorkloadStatus, WorkloadType,
};
use tracing::info;

const GENERATED_DOMAIN_SUFFIXES: &[&str] = &[".sslip.io", ".nip.io", ".traefik.me"];

const ROOT_DOMAIN_KEY: &str = "caprover_root_domain";

/// CapRover platform importer
pub struct CaproverImporter {
    version: String,
}

impl Default for CaproverImporter {
    fn default() -> Self {
        Self::new()
    }
}

impl CaproverImporter {
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

fn is_generated_domain(host: &str, root_domain: Option<&str>) -> bool {
    if let Some(root) = root_domain.filter(|r| !r.is_empty()) {
        if host.ends_with(root) {
            return true;
        }
    }
    GENERATED_DOMAIN_SUFFIXES
        .iter()
        .any(|suffix| host.ends_with(suffix))
}

/// Detect a database from a deployed image (CapRover one-click databases run
/// stock images).
fn detect_database(image: &str) -> Option<SnapshotServiceType> {
    let name = image.split(':').next().unwrap_or(image);
    let name = name.rsplit('/').next().unwrap_or(name);
    match name {
        "postgres" | "postgresql" | "timescaledb" => Some(SnapshotServiceType::Postgres),
        "mysql" | "mariadb" | "percona" => Some(SnapshotServiceType::Mysql),
        "mongo" | "mongodb" => Some(SnapshotServiceType::MongoDB),
        "redis" | "keydb" | "valkey" => Some(SnapshotServiceType::Redis),
        _ => None,
    }
}

/// Reconstruct a connection URL for a database app from its env vars and a
/// published host port. Returns `None` when no host port is published.
fn database_connection_url(
    service_type: &SnapshotServiceType,
    app: &CaproverApp,
    instance_host: Option<&str>,
) -> Option<String> {
    let host = instance_host?;
    let host_port = app.ports.iter().find_map(|p| p.host_port)?;
    match service_type {
        SnapshotServiceType::Postgres => Some(format!(
            "postgres://{}:{}@{}:{}/{}",
            app.env("POSTGRES_USER").unwrap_or("postgres"),
            app.env("POSTGRES_PASSWORD").unwrap_or(""),
            host,
            host_port,
            app.env("POSTGRES_DB")
                .or_else(|| app.env("POSTGRES_USER"))
                .unwrap_or("postgres"),
        )),
        SnapshotServiceType::Mysql => Some(format!(
            "mysql://{}:{}@{}:{}/{}",
            app.env("MYSQL_USER").unwrap_or("root"),
            app.env("MYSQL_PASSWORD")
                .or_else(|| app.env("MYSQL_ROOT_PASSWORD"))
                .unwrap_or(""),
            host,
            host_port,
            app.env("MYSQL_DATABASE").unwrap_or(""),
        )),
        SnapshotServiceType::MongoDB => Some(format!(
            "mongodb://{}:{}@{}:{}",
            app.env("MONGO_INITDB_ROOT_USERNAME").unwrap_or("root"),
            app.env("MONGO_INITDB_ROOT_PASSWORD").unwrap_or(""),
            host,
            host_port,
        )),
        SnapshotServiceType::Redis => Some(format!("redis://{}:{}", host, host_port)),
        _ => None,
    }
}

/// Git info from CapRover's repoInfo (`github.com/owner/repo` or a full URL).
/// The repoInfo password is a secret and is never copied anywhere.
fn git_info_for(app: &CaproverApp) -> Option<GitInfo> {
    let repo_info = app.repo_info()?;
    let raw = repo_info.repo.trim().trim_end_matches(".git");
    let without_scheme = raw
        .strip_prefix("https://")
        .or_else(|| raw.strip_prefix("http://"))
        .unwrap_or(raw);
    let mut segments = without_scheme.split('/');
    let host = segments.next()?.to_string();
    let owner = segments.next()?.to_string();
    let repo = segments.next()?.to_string();
    if host.is_empty() || owner.is_empty() || repo.is_empty() {
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
    let branch = if repo_info.branch.is_empty() {
        "main".to_string()
    } else {
        repo_info.branch.clone()
    };
    Some(GitInfo {
        provider,
        owner: owner.clone(),
        repo: repo.clone(),
        default_branch: branch,
        clone_url: Some(format!("https://{}/{}/{}.git", host, owner, repo)),
    })
}

fn app_status(app: &CaproverApp) -> WorkloadStatus {
    if app.deployed_image().is_none() {
        WorkloadStatus::Stopped // registered but nothing deployed yet
    } else if app.instance_count > 0 {
        WorkloadStatus::Running
    } else {
        WorkloadStatus::Stopped
    }
}

/// Build a workload snapshot for one CapRover app
fn app_to_snapshot(app: &CaproverApp, root_domain: Option<&str>) -> WorkloadSnapshot {
    let env: HashMap<String, String> = app
        .env_vars
        .iter()
        .map(|e| (e.key.clone(), e.value.clone()))
        .collect();

    let mut ports: HashMap<u16, Option<u16>> = app
        .ports
        .iter()
        .filter_map(|p| p.container_port.map(|c| (c, p.host_port)))
        .collect();
    if let Some(http_port) = app.container_http_port {
        ports.entry(http_port).or_insert(None);
    }

    let domains: Vec<String> = {
        let mut list: Vec<String> = app
            .custom_domain
            .iter()
            .filter_map(|d| d.public_domain.clone())
            .collect();
        if let Some(root) = root_domain.filter(|r| !r.is_empty()) {
            if !app.not_expose_as_web_app {
                list.push(format!("{}.{}", app.app_name, root));
            }
        }
        list
    };

    // repoInfo password deliberately excluded — it is a secret. The parsed
    // owner/repo/clone_url (git_info_for already extracts these correctly
    // from repoInfo.repo, which is a bare "host/owner/repo" string, not a
    // URL) are carried through so deployment_config can build a
    // GitSourcePlan without re-parsing. CapRover's own convention for "no
    // real credentials configured" is repoInfo.user == password == "public"
    // — any other value means a real credential is set, so treat it as
    // private rather than guess.
    let git_info = git_info_for(app);
    let is_public_repo = app
        .repo_info()
        .map(|r| r.user == "public" && r.password == "public")
        .unwrap_or(false);
    let source_metadata = serde_json::json!({
        "has_persistent_data": app.has_persistent_data,
        "instance_count": app.instance_count,
        "repo": app.repo_info().map(|r| r.repo.clone()),
        "repo_branch": app.repo_info().map(|r| r.branch.clone()),
        "git_owner": git_info.as_ref().map(|g| g.owner.clone()),
        "git_repo_name": git_info.as_ref().map(|g| g.repo.clone()),
        "git_clone_url": git_info.as_ref().and_then(|g| g.clone_url.clone()),
        "git_is_public": is_public_repo,
        "domains": domains,
        "not_expose_as_web_app": app.not_expose_as_web_app,
    });

    WorkloadSnapshot {
        id: WorkloadId::new(app.app_name.clone()),
        name: Some(app.app_name.clone()),
        workload_type: app
            .deployed_image()
            .and_then(detect_database)
            .map(|_| WorkloadType::Database)
            .unwrap_or(WorkloadType::Container),
        status: app_status(app),
        image: app.deployed_image().map(|i| i.to_string()),
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

/// Build a service snapshot for a database app
fn db_app_to_service_snapshot(
    app: &CaproverApp,
    service_type: SnapshotServiceType,
    instance_host: Option<&str>,
) -> ServiceSnapshot {
    let connection_url = database_connection_url(&service_type, app, instance_host);
    let reachable = connection_url.is_some();
    let version = app.deployed_image().and_then(|image| {
        let tag = image.split(':').nth(1)?;
        let version: String = tag
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        (!version.is_empty()).then_some(version)
    });

    ServiceSnapshot {
        id: app.app_name.clone(),
        name: app.app_name.clone(),
        service_type,
        version,
        connection_url,
        env_vars: HashMap::new(),
        has_data: app.has_persistent_data,
        data_size_bytes: None,
        metadata: serde_json::json!({
            "image": app.deployed_image(),
            "reachable": reachable,
            "host_ports": app.ports.iter().filter_map(|p| p.host_port).collect::<Vec<_>>(),
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

/// Deployment configuration for one app snapshot
fn deployment_config(snapshot: &WorkloadSnapshot) -> DeploymentConfiguration {
    let repo = snapshot
        .source_metadata
        .get("repo")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let branch = snapshot
        .source_metadata
        .get("repo_branch")
        .and_then(|v| v.as_str())
        .unwrap_or("main");

    let has_git = !repo.is_empty();
    let image = snapshot
        .image
        .clone()
        .unwrap_or_else(|| format!("build:{}#{}", repo, branch));
    // CapRover apps deploy an image even when git-configured — prefer the
    // git source for the temps project when repoInfo exists.
    let build = has_git.then(|| BuildConfiguration {
        context: "/".to_string(),
        dockerfile: None,
        args: HashMap::new(),
        target: None,
    });
    // Without this, execute_plan has no repo_owner/repo_name to create the
    // project from — the app and its services get created but the actual
    // deployment step fails with "repo_owner is missing", even though the
    // git source was right there in source_metadata the whole time.
    let git = has_git.then(|| {
        let owner = snapshot
            .source_metadata
            .get("git_owner")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let repo_name = snapshot
            .source_metadata
            .get("git_repo_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let clone_url = snapshot
            .source_metadata
            .get("git_clone_url")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let is_public = snapshot
            .source_metadata
            .get("git_is_public")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        GitSourcePlan {
            owner,
            repo: repo_name,
            branch: branch.to_string(),
            clone_url,
            is_public,
        }
    });

    let env_vars: Vec<EnvironmentVariable> = {
        let mut vars: Vec<_> = snapshot.env.iter().collect();
        vars.sort_by(|a, b| a.0.cmp(b.0));
        vars.into_iter()
            .map(|(key, value)| EnvironmentVariable {
                key: key.clone(),
                is_secret: temps_import_types::plan::looks_like_secret_env_key(key),
                value: value.clone(),
                source_description: Some("CapRover environment variable".to_string()),
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
    root_domain: Option<String>,
}

impl CaproverImporter {
    fn build_plan(
        &self,
        snapshot: &WorkloadSnapshot,
        project_name: &str,
        source_id: &str,
        extras: PlanExtras,
    ) -> ImportResult<ImportPlan> {
        let slug = sanitize_slug(project_name);
        let environment_name = "production".to_string();

        let has_git = snapshot
            .source_metadata
            .get("repo")
            .and_then(|v| v.as_str())
            .map(|r| !r.is_empty())
            .unwrap_or(false);
        let deployment = deployment_config(snapshot);

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
                        "Database '{}' contains data that will NOT be copied automatically — its published host port makes a dump/restore possible from outside",
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
                        "Database '{}' has no published host port — temps cannot access its data from outside the CapRover server",
                        service.name
                    ),
                    recommended_action: Some(
                        "Add a port mapping on the app in CapRover before migrating, or dump it on the CapRover server (docker exec) and restore into the temps-managed service".to_string(),
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
            let generated = is_generated_domain(&domain.domain, extras.root_domain.as_deref());
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
                    "CapRover-generated subdomain — temps assigns its own preview domain instead"
                        .to_string()
                } else {
                    "Register in temps, then point the domain's DNS at the temps server to cut over"
                        .to_string()
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
                    "Set the application's environment variables on the temps environment. Variables referencing CapRover-internal hostnames (srv-captain--*) must be updated to the new temps service addresses."
                        .to_string(),
                resource_type: StepResourceType::EnvironmentVariable,
                risk: RiskLevel::Low,
                data_implications: vec![],
                pre_conditions: vec![],
                post_conditions: vec![
                    "Review copied values — rewrite any that point at srv-captain--* hosts".to_string(),
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
                    "Database '{}' data cannot be reached from outside the CapRover server — plan its dump/restore before switching traffic",
                    service_plan.name
                ));
                manual_actions.push(ManualAction {
                    timing: ManualActionTiming::BeforeMigration,
                    description: format!(
                        "Dump database '{}' (publish a host port in CapRover, or docker exec on the server)",
                        service_plan.name
                    ),
                    reason: "The database has no published host port, so temps cannot copy its data".to_string(),
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

        let summary = MigrationSummary {
            headline: format!(
                "Migrate '{}' from CapRover: 1 application, {} database(s), {} custom domain(s)",
                project_name,
                service_plans.len(),
                custom_domain_count
            ),
            overall_risk,
            resource_counts: ResourceCounts {
                projects: 1,
                environments: 1,
                deployments: 1,
                environment_variables: deployment.env_vars.len(),
                services: service_plans.len(),
                domains: custom_domain_count,
            },
            critical_warnings,
            manual_actions_required: manual_actions,
            unsupported_features: vec![UnsupportedFeature {
                feature: "CapRover one-click apps (non-database)".to_string(),
                reason: "Only database one-click apps are recognized as services; other one-click apps must be imported individually".to_string(),
                alternative: Some("Import each app separately, or re-create it as a temps project".to_string()),
            }],
        };

        let complexity = if service_plans.is_empty() {
            PlanComplexity::Low
        } else if service_plans.len() <= 1 {
            PlanComplexity::Medium
        } else {
            PlanComplexity::High
        };

        Ok(ImportPlan {
            version: "1.0".to_string(),
            source: ImportSource::Caprover.to_string(),
            source_id: source_id.to_string(),
            project: ProjectConfiguration {
                name: project_name.to_string(),
                slug,
                project_type: if has_git {
                    ProjectType::Git
                } else {
                    ProjectType::Docker
                },
                is_web_app: !snapshot
                    .source_metadata
                    .get("not_expose_as_web_app")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
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
            additional_deployments: vec![],
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
impl WorkloadImporter for CaproverImporter {
    fn source(&self) -> ImportSource {
        ImportSource::Caprover
    }

    fn name(&self) -> &str {
        "CapRover"
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
        let client = match CaproverClient::from_credentials(credentials).await {
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
            Ok(token) => {
                let root = client
                    .system_info(&token)
                    .await
                    .ok()
                    .and_then(|i| i.root_domain);
                Ok(CredentialValidation {
                    valid: true,
                    account_name: root.clone(),
                    message: Some(match root {
                        Some(domain) if !domain.is_empty() => {
                            format!("Connected — root domain {}", domain)
                        }
                        _ => "Connected".to_string(),
                    }),
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
        let client = CaproverClient::from_credentials(credentials)
            .await
            .map_err(ImportError::from)?;
        let token = client.login().await.map_err(ImportError::from)?;
        let apps = client.apps(&token).await.map_err(ImportError::from)?;

        let mut descriptors: Vec<WorkloadDescriptor> = apps
            .app_definitions
            .iter()
            .map(|app| WorkloadDescriptor {
                id: WorkloadId::new(app.app_name.clone()),
                name: Some(app.app_name.clone()),
                workload_type: app
                    .deployed_image()
                    .and_then(detect_database)
                    .map(|_| WorkloadType::Database)
                    .unwrap_or(WorkloadType::Container),
                status: app_status(app),
                image: app.deployed_image().map(|i| i.to_string()),
                created_at: None,
                labels: HashMap::new(),
            })
            .collect();

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

        info!("Discovered {} CapRover apps", descriptors.len());
        Ok(descriptors)
    }

    async fn describe(
        &self,
        credentials: &ImportCredentials,
        workload_id: &WorkloadId,
    ) -> ImportResult<WorkloadSnapshot> {
        let client = CaproverClient::from_credentials(credentials)
            .await
            .map_err(ImportError::from)?;
        let token = client.login().await.map_err(ImportError::from)?;
        let root = client
            .system_info(&token)
            .await
            .map_err(ImportError::from)?
            .root_domain;
        let apps = client.apps(&token).await.map_err(ImportError::from)?;
        let app = apps
            .app_definitions
            .iter()
            .find(|a| a.app_name == workload_id.as_str())
            .ok_or_else(|| {
                ImportError::from(CaproverImportError::AppNotFound {
                    app_name: workload_id.as_str().to_string(),
                })
            })?;
        Ok(app_to_snapshot(app, root.as_deref()))
    }

    async fn describe_project(
        &self,
        credentials: &ImportCredentials,
        workload_id: &WorkloadId,
    ) -> ImportResult<ProjectSnapshot> {
        let client = CaproverClient::from_credentials(credentials)
            .await
            .map_err(ImportError::from)?;
        let token = client.login().await.map_err(ImportError::from)?;
        let root = client
            .system_info(&token)
            .await
            .map_err(ImportError::from)?
            .root_domain;
        let apps = client.apps(&token).await.map_err(ImportError::from)?;

        let anchor = apps
            .app_definitions
            .iter()
            .find(|a| a.app_name == workload_id.as_str())
            .ok_or_else(|| {
                ImportError::from(CaproverImportError::AppNotFound {
                    app_name: workload_id.as_str().to_string(),
                })
            })?;
        if anchor.deployed_image().and_then(detect_database).is_some() {
            return Err(ImportError::InvalidConfiguration(format!(
                "'{}' is a database app — select the application to migrate instead; databases are imported alongside it as managed services",
                workload_id
            )));
        }

        // CapRover has no grouping: database-image apps across the instance
        // become managed-service candidates for this migration.
        let instance_host = client.host();
        let services: Vec<ServiceSnapshot> = apps
            .app_definitions
            .iter()
            .filter_map(|app| {
                let service_type = app.deployed_image().and_then(detect_database)?;
                Some(db_app_to_service_snapshot(
                    app,
                    service_type,
                    instance_host.as_deref(),
                ))
            })
            .collect();

        let primary_workload = app_to_snapshot(anchor, root.as_deref());
        let domains: Vec<DomainSnapshot> = primary_workload
            .source_metadata
            .get("domains")
            .and_then(|v| v.as_array())
            .map(|hosts| {
                hosts
                    .iter()
                    .filter_map(|h| h.as_str())
                    .map(|host| DomainSnapshot {
                        is_apex: host.chars().filter(|c| *c == '.').count() <= 1,
                        domain: host.to_string(),
                        redirect_to: None,
                        redirect_status_code: None,
                        environment: Some("production".to_string()),
                        verified: true,
                    })
                    .collect()
            })
            .unwrap_or_default();

        let git_info = git_info_for(anchor);

        Ok(ProjectSnapshot {
            id: workload_id.clone(),
            name: anchor.app_name.clone(),
            primary_workload,
            additional_workloads: vec![],
            services,
            domains,
            git_info,
            detected_framework: None,
            source_metadata: serde_json::json!({
                ROOT_DOMAIN_KEY: root,
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
        let root_domain = snapshot
            .source_metadata
            .get(ROOT_DOMAIN_KEY)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let extras = PlanExtras {
            services: snapshot.services.clone(),
            domains: snapshot.domains.clone(),
            root_domain,
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
        CaproverValidationRules::all_rules()
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
            supports_stacks: false, // CapRover has no grouping — one app at a time
            supports_services: true,
            supports_domains: true,
            supports_project_snapshot: true,
            supports_cost_analysis: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Execution (same honest-manual pattern as the other platform importers)
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
        "Executing CapRover import for session: {} (dry_run: {})",
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
    // back to the public repository CapRover deploys from (repoInfo), so
    // the real deployment pipeline can clone and build it. Private source
    // repos without a linked connection stay unlinked — the deploy step
    // reports that honestly instead of failing mid-clone.
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
        // is_secret vars carry a real value read from CapRover's API
        // (unlike Kamal's empty placeholders) — they must reach the
        // deployment or the app boots without its real credentials. temps
        // encrypts every env var at rest regardless of this flag; only the
        // plan API response masks it (mask_plan_secrets in the orchestrator).
        environment_variables: Some(
            plan.deployment
                .env_vars
                .iter()
                .map(|env| (env.key.clone(), env.value.clone()))
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
        builder: Some("caprover-import".to_string()),
        labels: vec!["imported".to_string(), "caprover".to_string()],
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
        commit_message: Set(Some(format!("Imported from CapRover ({})", plan.source_id))),
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
        "✅ CapRover import completed in {:.2}s — project {} (id: {})",
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

    fn shop_app() -> CaproverApp {
        serde_json::from_value(serde_json::json!({
            "appName": "lab-shop",
            "hasPersistentData": false,
            "instanceCount": 1,
            "containerHttpPort": 3000,
            "notExposeAsWebApp": false,
            "envVars": [{"key": "PLAN", "value": "pro"}],
            "ports": [],
            "volumes": [],
            "customDomain": [{"publicDomain": "shop.example.com", "hasSsl": true}],
            "deployedVersion": 1,
            "versions": [{"version": 1, "deployedImageName": "node:22-alpine"}],
            "appPushWebhook": {"repoInfo": {"repo": "github.com/heroku/node-js-getting-started", "user": "public", "password": "secret", "branch": "main"}}
        }))
        .unwrap()
    }

    fn db_app(host_port: Option<u16>) -> CaproverApp {
        serde_json::from_value(serde_json::json!({
            "appName": "lab-db",
            "hasPersistentData": true,
            "instanceCount": 1,
            "notExposeAsWebApp": true,
            "envVars": [
                {"key": "POSTGRES_USER", "value": "lab"},
                {"key": "POSTGRES_PASSWORD", "value": "pw"},
                {"key": "POSTGRES_DB", "value": "labdb"}
            ],
            "ports": host_port.map(|p| vec![serde_json::json!({"containerPort": 5432, "hostPort": p})]).unwrap_or_default(),
            "deployedVersion": 1,
            "versions": [{"version": 1, "deployedImageName": "postgres:16-alpine"}]
        }))
        .unwrap()
    }

    fn project_snapshot(db_host_port: Option<u16>) -> ProjectSnapshot {
        let anchor = shop_app();
        let primary = app_to_snapshot(&anchor, Some("captain.1.2.3.4.sslip.io"));
        let service = db_app_to_service_snapshot(
            &db_app(db_host_port),
            SnapshotServiceType::Postgres,
            Some("1.2.3.4"),
        );
        let domains = primary
            .source_metadata
            .get("domains")
            .and_then(|v| v.as_array())
            .map(|hosts| {
                hosts
                    .iter()
                    .filter_map(|h| h.as_str())
                    .map(|host| DomainSnapshot {
                        is_apex: false,
                        domain: host.to_string(),
                        redirect_to: None,
                        redirect_status_code: None,
                        environment: Some("production".to_string()),
                        verified: true,
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        ProjectSnapshot {
            id: WorkloadId::new("lab-shop"),
            name: "lab-shop".to_string(),
            primary_workload: primary,
            additional_workloads: vec![],
            services: vec![service],
            domains,
            git_info: git_info_for(&anchor),
            detected_framework: None,
            source_metadata: serde_json::json!({ ROOT_DOMAIN_KEY: "captain.1.2.3.4.sslip.io" }),
        }
    }

    #[test]
    fn detects_database_images() {
        assert_eq!(
            detect_database("postgres:16-alpine"),
            Some(SnapshotServiceType::Postgres)
        );
        assert_eq!(
            detect_database("library/mysql:8"),
            Some(SnapshotServiceType::Mysql)
        );
        assert_eq!(detect_database("redis:7"), Some(SnapshotServiceType::Redis));
        assert_eq!(detect_database("node:22-alpine"), None);
    }

    #[test]
    fn builds_postgres_connection_url_from_env_and_host_port() {
        let url = database_connection_url(
            &SnapshotServiceType::Postgres,
            &db_app(Some(5432)),
            Some("1.2.3.4"),
        );
        assert_eq!(url.as_deref(), Some("postgres://lab:pw@1.2.3.4:5432/labdb"));
        assert_eq!(
            database_connection_url(
                &SnapshotServiceType::Postgres,
                &db_app(None),
                Some("1.2.3.4")
            ),
            None
        );
    }

    #[test]
    fn git_info_from_repo_info_excludes_password() {
        let git = git_info_for(&shop_app()).unwrap();
        assert_eq!(git.provider, "github");
        assert_eq!(git.owner, "heroku");
        assert_eq!(git.repo, "node-js-getting-started");
        // the snapshot must not contain the repo password
        let snapshot = app_to_snapshot(&shop_app(), None);
        assert!(!snapshot.source_metadata.to_string().contains("secret"));
    }

    /// Regression test: deployment_config() used to hardcode `git: None`
    /// regardless of repoInfo — execute_plan then had no repo_owner/
    /// repo_name to create the project from, so the deploy step failed
    /// with "repo_owner is missing" even though the git source was right
    /// there in source_metadata (proven live against a real CapRover
    /// instance during PR #441's e2e verification).
    #[test]
    fn deployment_git_source_uses_parsed_owner_repo_and_clone_url() {
        let snapshot = app_to_snapshot(&shop_app(), None);
        let deployment = deployment_config(&snapshot);
        let git = deployment
            .git
            .expect("a git-configured app must produce a GitSourcePlan");
        assert_eq!(git.owner, "heroku");
        assert_eq!(git.repo, "node-js-getting-started");
        assert_eq!(git.branch, "main");
        assert_eq!(
            git.clone_url.as_deref(),
            Some("https://github.com/heroku/node-js-getting-started.git")
        );
        // shop_app()'s repoInfo has a real (non-"public") password — must
        // not be misclassified as a credential-free public repository.
        assert!(!git.is_public);
    }

    #[test]
    fn deployment_git_source_is_public_when_caprovers_public_sentinel_is_set() {
        let app: CaproverApp = serde_json::from_value(serde_json::json!({
            "appName": "lab-shop",
            "hasPersistentData": false,
            "instanceCount": 1,
            "notExposeAsWebApp": false,
            "envVars": [],
            "ports": [],
            "volumes": [],
            "customDomain": [],
            "deployedVersion": 1,
            "versions": [{"version": 1, "deployedImageName": "node:22-alpine"}],
            "appPushWebhook": {"repoInfo": {"repo": "github.com/heroku/node-js-getting-started", "user": "public", "password": "public", "branch": "main"}}
        }))
        .unwrap();
        let snapshot = app_to_snapshot(&app, None);
        let deployment = deployment_config(&snapshot);
        let git = deployment
            .git
            .expect("a git-configured app must produce a GitSourcePlan");
        assert!(git.is_public);
    }

    #[test]
    fn deployment_has_no_git_source_without_repo_info() {
        let snapshot = app_to_snapshot(&db_app(Some(5432)), None);
        let deployment = deployment_config(&snapshot);
        assert!(deployment.git.is_none());
    }

    #[test]
    fn generated_and_custom_domains_are_classified() {
        assert!(is_generated_domain(
            "lab-shop.captain.1.2.3.4.sslip.io",
            Some("captain.1.2.3.4.sslip.io")
        ));
        assert!(is_generated_domain(
            "app.captain.example.com",
            Some("captain.example.com")
        ));
        assert!(!is_generated_domain(
            "shop.example.com",
            Some("captain.example.com")
        ));
    }

    #[test]
    fn project_plan_covers_app_database_and_domains() {
        let importer = CaproverImporter::new();
        let plan = importer
            .generate_project_plan(project_snapshot(Some(5432)))
            .unwrap();

        assert_eq!(plan.source, "caprover");
        assert_eq!(plan.summary.resource_counts.services, 1);
        // custom domain imported; generated lab-shop.captain.* skipped
        assert_eq!(plan.summary.resource_counts.domains, 1);
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
        let importer = CaproverImporter::new();
        let plan = importer
            .generate_project_plan(project_snapshot(None))
            .unwrap();
        assert!(plan.services[0]
            .data_implications
            .iter()
            .any(|i| i.severity == DataImplicationSeverity::DataNotMigrated));
        assert!(!plan.summary.critical_warnings.is_empty());
    }

    /// Live end-to-end against a real CapRover instance. Skips unless
    /// CAPROVER_TEST_URL and CAPROVER_TEST_PASSWORD are set.
    #[tokio::test]
    async fn live_discover_describe_and_plan() {
        let (Ok(url), Ok(password)) = (
            std::env::var("CAPROVER_TEST_URL"),
            std::env::var("CAPROVER_TEST_PASSWORD"),
        ) else {
            println!("CAPROVER_TEST_URL/CAPROVER_TEST_PASSWORD not set, skipping live test");
            return;
        };
        let credentials = ImportCredentials::with_token_and_url(password, url);
        let importer = CaproverImporter::new();

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
        assert!(!discovered.is_empty(), "expected apps on the instance");
        let anchor = discovered
            .iter()
            .find(|d| d.workload_type == WorkloadType::Container)
            .expect("at least one non-database app");

        let snapshot = importer
            .describe_project(&credentials, &anchor.id)
            .await
            .unwrap();
        let plan = importer.generate_project_plan(snapshot).unwrap();
        assert_eq!(plan.source, "caprover");
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
