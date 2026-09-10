// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Kamal importer — WorkloadImporter implementation
//!
//! Reads `config/deploy.yml` (supplied per-request) and turns it into a
//! project snapshot + migration plan. There is no server to query: Kamal's
//! desired state is the file.

use crate::error::KamalImportError;
use crate::model::{KamalAccessory, KamalConfig, KamalRole};
use crate::validation::KamalValidationRules;
use async_trait::async_trait;
use std::collections::HashMap;
use temps_import_types::plan::{
    BuildConfiguration, DeploymentConfiguration, EnvironmentConfiguration, PlanComplexity,
    PlanMetadata, ProjectConfiguration, ProjectType, Protocol,
};
use temps_import_types::{
    CredentialValidation, DataImplication, DataImplicationSeverity, DeploymentStrategy,
    DomainAction, DomainPlan, DomainSnapshot, EnvironmentVariable, ImportContext,
    ImportCredentials, ImportError, ImportOutcome, ImportPlan, ImportResult, ImportSelector,
    ImportServiceProvider, ImportSource, ImporterCapabilities, ManualAction, ManualActionTiming,
    MigrationStep, MigrationSummary, NetworkConfiguration, NetworkInfo, NetworkMode, PortMapping,
    ProjectSnapshot, ResourceCounts, ResourceInfo, ResourceLimits, RiskLevel, ServiceAction,
    ServicePlan, ServiceSnapshot, SnapshotServiceType, StepResourceType, StepResult,
    UnsupportedFeature, WorkloadDescriptor, WorkloadId, WorkloadImporter, WorkloadSnapshot,
    WorkloadStatus, WorkloadType,
};
use tracing::info;

/// Credentials key carrying the deploy.yml contents
pub const DEPLOY_YML_KEY: &str = "deploy_yml";

/// Kamal importer
pub struct KamalImporter {
    version: String,
}

impl Default for KamalImporter {
    fn default() -> Self {
        Self::new()
    }
}

impl KamalImporter {
    pub fn new() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

fn config_from(credentials: &ImportCredentials) -> Result<KamalConfig, KamalImportError> {
    let yaml = credentials
        .extra
        .get(DEPLOY_YML_KEY)
        .filter(|y| !y.trim().is_empty())
        .ok_or(KamalImportError::MissingConfig)?;
    KamalConfig::parse(yaml)
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

/// Detect the service type of an accessory from its image
fn detect_service_type(image: &str) -> Option<SnapshotServiceType> {
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

/// Build a workload snapshot for one Kamal role
fn role_to_snapshot(config: &KamalConfig, role_name: &str, role: &KamalRole) -> WorkloadSnapshot {
    let mut env: HashMap<String, String> = config
        .env
        .as_ref()
        .map(|e| e.clear_pairs().into_iter().collect())
        .unwrap_or_default();
    // Role-level env overrides the top-level one (Kamal's precedence).
    if let Some(role_env) = role.env() {
        env.extend(role_env.clear_pairs());
    }

    let mut ports = HashMap::new();
    if role.is_web_facing(role_name) {
        ports.insert(config.app_port(), None);
    }

    let secret_names: Vec<String> = config
        .env
        .as_ref()
        .map(|e| e.secret.clone())
        .unwrap_or_default();

    let source_metadata = serde_json::json!({
        "role": role_name,
        "hosts": role.hosts(),
        "cmd": role.cmd(),
        "web_facing": role.is_web_facing(role_name),
        "secret_names": secret_names,
        "dockerfile": config.builder.as_ref().and_then(|b| b.dockerfile.clone()),
        "registry_server": config.registry.as_ref().and_then(|r| r.server.clone()),
        "volumes": config.volumes,
    });

    WorkloadSnapshot {
        id: WorkloadId::new(format!("{}/{}", config.service, role_name)),
        name: Some(if role_name == "web" {
            config.service.clone()
        } else {
            format!("{}-{}", config.service, role_name)
        }),
        workload_type: if role.cmd().is_some() && !role.is_web_facing(role_name) {
            WorkloadType::Worker
        } else {
            WorkloadType::Container
        },
        // Kamal configs describe intent, not live state — anything else
        // would be a guess about servers we never contacted.
        status: WorkloadStatus::Unknown,
        image: Some(config.image_reference()),
        command: role.cmd().map(|c| {
            c.split_whitespace()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        }),
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

/// Build a service snapshot for one accessory
fn accessory_to_service(
    name: &str,
    accessory: &KamalAccessory,
    service_type: SnapshotServiceType,
) -> ServiceSnapshot {
    let env = accessory.env.as_ref();
    let host = accessory.effective_host();
    let host_port = accessory.host_port();

    // Kamal keeps accessory passwords in .kamal/secrets, so a complete URL is
    // impossible — expose host:port and let the plan say the rest explicitly.
    let connection_url = match (host, host_port) {
        (Some(host), Some(port)) => {
            let user = env
                .and_then(|e| {
                    e.clear_pairs()
                        .into_iter()
                        .find(|(k, _)| {
                            k == "POSTGRES_USER"
                                || k == "MYSQL_USER"
                                || k == "MONGO_INITDB_ROOT_USERNAME"
                        })
                        .map(|(_, v)| v)
                })
                .unwrap_or_default();
            let database = env
                .and_then(|e| {
                    e.clear_pairs()
                        .into_iter()
                        .find(|(k, _)| k == "POSTGRES_DB" || k == "MYSQL_DATABASE")
                        .map(|(_, v)| v)
                })
                .unwrap_or_default();
            let scheme = match service_type {
                SnapshotServiceType::Postgres => "postgres",
                SnapshotServiceType::Mysql => "mysql",
                SnapshotServiceType::MongoDB => "mongodb",
                SnapshotServiceType::Redis => "redis",
                _ => "tcp",
            };
            if user.is_empty() {
                Some(format!("{}://{}:{}/{}", scheme, host, port, database))
            } else {
                // password intentionally omitted — it is a Kamal secret
                Some(format!(
                    "{}://{}@{}:{}/{}",
                    scheme, user, host, port, database
                ))
            }
        }
        _ => None,
    };

    let version = accessory.image.split(':').nth(1).and_then(|tag| {
        let v: String = tag
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        (!v.is_empty()).then_some(v)
    });

    ServiceSnapshot {
        id: name.to_string(),
        name: name.to_string(),
        service_type,
        version,
        connection_url,
        env_vars: env
            .map(|e| e.clear_pairs().into_iter().collect())
            .unwrap_or_default(),
        has_data: accessory.has_persistent_data(),
        data_size_bytes: None,
        metadata: serde_json::json!({
            "image": accessory.image,
            "host": host,
            "host_port": host_port,
            // never "reachable": Kamal secrets mean we cannot form a
            // usable DSN, so the orchestrator must not try to auto-copy.
            "reachable": false,
            "secret_names": env.map(|e| e.secret.clone()).unwrap_or_default(),
            "directories": accessory.directories,
        }),
    }
}

fn deployment_config(snapshot: &WorkloadSnapshot, config: &KamalConfig) -> DeploymentConfiguration {
    let dockerfile = snapshot
        .source_metadata
        .get("dockerfile")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Env comes from the SNAPSHOT, not the config: the project-plan path
    // rebuilds a config view without env, and reading it there silently
    // dropped every secret placeholder.
    let mut env_vars: Vec<EnvironmentVariable> = snapshot
        .env
        .iter()
        .map(|(key, value)| EnvironmentVariable {
            key: key.clone(),
            value: value.clone(),
            is_secret: false,
            source_description: Some("Kamal env.clear".to_string()),
        })
        .collect();
    // Secret *names* are carried in the snapshot metadata; their values live
    // in .kamal/secrets and are deliberately never present here.
    for name in snapshot
        .source_metadata
        .get("secret_names")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
        .unwrap_or_default()
    {
        if !env_vars.iter().any(|v| v.key == name) {
            env_vars.push(EnvironmentVariable {
                key: name.to_string(),
                value: String::new(),
                is_secret: true,
                source_description: Some(
                    "Kamal env.secret — value lives in .kamal/secrets and must be set manually"
                        .to_string(),
                ),
            });
        }
    }
    env_vars.sort_by(|a, b| a.key.cmp(&b.key));

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
        image: snapshot
            .image
            .clone()
            .unwrap_or_else(|| config.image_reference()),
        // Kamal builds from a Dockerfile in the repo, so temps can rebuild
        // the same way once the repository is linked.
        build: Some(BuildConfiguration {
            context: config
                .builder
                .as_ref()
                .and_then(|b| b.context.clone())
                .unwrap_or_else(|| ".".to_string()),
            dockerfile: dockerfile.or_else(|| Some("Dockerfile".to_string())),
            args: config
                .builder
                .as_ref()
                .map(|b| {
                    b.args
                        .iter()
                        .map(|(k, v)| (k.clone(), crate::model::scalar_to_string(v)))
                        .collect()
                })
                .unwrap_or_default(),
            target: config.builder.as_ref().and_then(|b| b.target.clone()),
        }),
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
        command: snapshot.command.clone(),
        entrypoint: None,
        working_dir: None,
        health_check: None,
        git: None,
    }
}

// ---------------------------------------------------------------------------
// Plan generation
// ---------------------------------------------------------------------------

impl KamalImporter {
    fn build_plan(
        &self,
        config: &KamalConfig,
        snapshot: &ProjectSnapshot,
    ) -> ImportResult<ImportPlan> {
        let project_name = config.service.clone();
        let slug = sanitize_slug(&project_name);
        let environment_name = "production".to_string();

        let deployment = deployment_config(&snapshot.primary_workload, config);
        let additional_deployments: Vec<DeploymentConfiguration> = snapshot
            .additional_workloads
            .iter()
            .map(|w| deployment_config(w, config))
            .collect();

        // -- Services --------------------------------------------------------
        let mut service_plans = Vec::new();
        for service in &snapshot.services {
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

            let host_hint = service
                .connection_url
                .clone()
                .unwrap_or_else(|| "the accessory host".to_string());
            let implications = vec![DataImplication {
                severity: if service.has_data {
                    DataImplicationSeverity::DataNotMigrated
                } else {
                    DataImplicationSeverity::Warning
                },
                message: format!(
                    "Accessory '{}' runs on your own server ({}). Its password is a Kamal secret, so temps cannot connect and copy the data automatically",
                    service.name, host_hint
                ),
                recommended_action: Some(format!(
                    "On the accessory host run: kamal accessory exec {} 'pg_dump …' (or the equivalent dump for {}), then restore into the temps-managed service",
                    service.name, service.service_type
                )),
            }];

            let mut parameters = HashMap::new();
            parameters.insert("reachable".to_string(), serde_json::json!(false));
            if let Some(version) = &service.version {
                parameters.insert("version".to_string(), serde_json::json!(version));
            }
            // Deliberately no source_url: without the secret we cannot build
            // a working DSN, and a broken one would fail the copy silently.

            service_plans.push(ServicePlan {
                name: service.name.clone(),
                service_type: temps_type,
                version: service.version.clone(),
                parameters,
                env_var_mappings: HashMap::new(),
                action: ServiceAction::Create,
                action_description: format!(
                    "Create a temps-managed {} service named '{}' to replace the Kamal accessory, then migrate its data",
                    service.service_type, service.name
                ),
                data_implications: implications,
            });
        }

        // -- Domains ---------------------------------------------------------
        // Kamal's proxy.host is a real customer domain, so these import.
        let domain_plans: Vec<DomainPlan> = snapshot
            .domains
            .iter()
            .map(|domain| DomainPlan {
                domain: domain.domain.clone(),
                environment: environment_name.clone(),
                redirect_to: None,
                status_code: None,
                action: DomainAction::Import,
                action_description:
                    "Register in temps, then point the domain's DNS at the temps server — temps issues the certificate automatically, replacing kamal-proxy's Let's Encrypt handling"
                        .to_string(),
                replacement: None,
            })
            .collect();

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

        let secret_count = deployment.env_vars.iter().filter(|v| v.is_secret).count();
        if !deployment.env_vars.is_empty() {
            order += 1;
            steps.push(MigrationStep {
                order,
                id: "configure-env-vars".to_string(),
                title: format!(
                    "Copy {} environment variable(s){}",
                    deployment.env_vars.len(),
                    if secret_count > 0 {
                        format!(" ({} secret placeholders)", secret_count)
                    } else {
                        String::new()
                    }
                ),
                description: if secret_count > 0 {
                    format!(
                        "Set env.clear values on the temps environment. {} secret(s) are created as empty placeholders — Kamal stores their values in .kamal/secrets, which is not part of deploy.yml, so you must paste them in.",
                        secret_count
                    )
                } else {
                    "Set the application's environment variables on the temps environment.".to_string()
                },
                resource_type: StepResourceType::EnvironmentVariable,
                risk: if secret_count > 0 {
                    RiskLevel::Medium
                } else {
                    RiskLevel::Low
                },
                data_implications: vec![],
                pre_conditions: vec![],
                post_conditions: if secret_count > 0 {
                    vec!["Fill in every secret placeholder before deploying — the app will not boot without them".to_string()]
                } else {
                    vec![]
                },
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
                    "Data restored into '{}' and row counts verified against the accessory",
                    service_plan.name
                )],
                skippable: true,
                skipped: false,
                reversible: true,
                estimated_duration: Some("manual — depends on data size".to_string()),
            });
        }

        for domain_plan in &domain_plans {
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
            title: format!("Deploy '{}'", project_name),
            description: format!(
                "Build from the repository's {} and deploy — the same Dockerfile Kamal builds",
                deployment
                    .build
                    .as_ref()
                    .and_then(|b| b.dockerfile.clone())
                    .unwrap_or_else(|| "Dockerfile".to_string())
            ),
            resource_type: StepResourceType::Deployment,
            risk: RiskLevel::Low,
            data_implications: vec![],
            pre_conditions: vec!["Repository linked in temps".to_string()],
            post_conditions: vec!["Application responds on its temps domain".to_string()],
            skippable: false,
            skipped: false,
            reversible: true,
            estimated_duration: Some("build time: 1-10 minutes".to_string()),
        });

        // -- Summary ---------------------------------------------------------
        let mut critical_warnings = Vec::new();
        // deploy.yml never carries a repository reference (Kamal builds from
        // the checkout it's run in), so the imported project is never linked
        // to a git repo and the first deployment cannot start automatically.
        let mut manual_actions = vec![ManualAction {
            timing: ManualActionTiming::AfterMigration,
            description: "Link a git repository or trigger the first deployment manually"
                .to_string(),
            reason: "deploy.yml has no repository field — the import records configuration only, so temps has nothing to build from yet".to_string(),
        }];

        if secret_count > 0 {
            critical_warnings.push(format!(
                "{} environment secret(s) are referenced by name only — Kamal keeps their values in .kamal/secrets, so temps imports empty placeholders you must fill in",
                secret_count
            ));
            manual_actions.push(ManualAction {
                timing: ManualActionTiming::AfterMigration,
                description: format!(
                    "Set the {} secret value(s) on the temps environment (from your .kamal/secrets source: password manager, ENV, or 1Password/Bitwarden)",
                    secret_count
                ),
                reason: "deploy.yml references secrets by name; the values are deliberately not in the file".to_string(),
            });
        }
        for service_plan in &service_plans {
            critical_warnings.push(format!(
                "Accessory '{}' holds data on your own server and cannot be copied automatically (its password is a Kamal secret)",
                service_plan.name
            ));
            manual_actions.push(ManualAction {
                timing: ManualActionTiming::BeforeMigration,
                description: format!(
                    "Dump accessory '{}' with 'kamal accessory exec {}' and restore it into the temps-managed service",
                    service_plan.name, service_plan.name
                ),
                reason: "temps has no credentials for the accessory — Kamal stores them outside deploy.yml".to_string(),
            });
        }
        if !domain_plans.is_empty() {
            manual_actions.push(ManualAction {
                timing: ManualActionTiming::AfterMigration,
                description: format!(
                    "Update DNS for {} domain(s) to point at the temps server",
                    domain_plans.len()
                ),
                reason: "DNS is controlled at the registrar and cannot be changed by temps"
                    .to_string(),
            });
        }

        let mut unsupported = vec![UnsupportedFeature {
            feature: "Kamal secrets (.kamal/secrets)".to_string(),
            reason: "Secret values are resolved at deploy time from your password manager/ENV and never appear in deploy.yml".to_string(),
            alternative: Some("Set them as temps environment variables (encrypted at rest)".to_string()),
        }];
        if !config.volumes.is_empty() {
            unsupported.push(UnsupportedFeature {
                feature: format!("Docker volumes ({})", config.volumes.join(", ")),
                reason: "Volume contents live on your servers and are not copied by the importer"
                    .to_string(),
                alternative: Some(
                    "Copy the data manually, or use a temps storage service".to_string(),
                ),
            });
        }
        if config.asset_path.is_some() {
            unsupported.push(UnsupportedFeature {
                feature: "asset_path bridging".to_string(),
                reason: "Kamal bridges fingerprinted assets between versions; temps handles this per-deployment".to_string(),
                alternative: None,
            });
        }

        let overall_risk = if !service_plans.is_empty() || secret_count > 0 {
            RiskLevel::High
        } else if !domain_plans.is_empty() {
            RiskLevel::Medium
        } else {
            RiskLevel::Low
        };

        let workload_count = 1 + additional_deployments.len();
        let summary = MigrationSummary {
            headline: format!(
                "Migrate '{}' from Kamal: {} role(s), {} accessory service(s), {} domain(s)",
                project_name,
                workload_count,
                service_plans.len(),
                domain_plans.len()
            ),
            overall_risk,
            resource_counts: ResourceCounts {
                projects: 1,
                environments: 1,
                deployments: workload_count,
                environment_variables: deployment.env_vars.len(),
                services: service_plans.len(),
                domains: domain_plans.len(),
            },
            critical_warnings,
            manual_actions_required: manual_actions,
            unsupported_features: unsupported,
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
            source: ImportSource::Kamal.to_string(),
            source_id: config.service.clone(),
            project: ProjectConfiguration {
                name: project_name.clone(),
                slug,
                // Kamal always builds from a Dockerfile in the repo
                project_type: ProjectType::Docker,
                is_web_app: !domain_plans.is_empty() || !deployment.ports.is_empty(),
            },
            environment: EnvironmentConfiguration {
                name: environment_name,
                subdomain: sanitize_slug(&project_name),
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

    /// Build the project snapshot from a parsed config
    fn snapshot_from_config(&self, config: &KamalConfig) -> ProjectSnapshot {
        let (primary_name, primary_role) = config
            .primary_role()
            .map(|(n, r)| (n.clone(), r.clone()))
            .unwrap_or_else(|| {
                // A config with no servers still describes an app.
                ("web".to_string(), KamalRole::Hosts(vec![]))
            });

        let primary_workload = role_to_snapshot(config, &primary_name, &primary_role);
        let additional_workloads: Vec<WorkloadSnapshot> = config
            .servers
            .iter()
            .filter(|(name, _)| **name != primary_name)
            .map(|(name, role)| role_to_snapshot(config, name, role))
            .collect();

        let services: Vec<ServiceSnapshot> = config
            .accessories
            .iter()
            .filter_map(|(name, accessory)| {
                detect_service_type(&accessory.image)
                    .map(|service_type| accessory_to_service(name, accessory, service_type))
            })
            .collect();

        let domains: Vec<DomainSnapshot> = config
            .proxy
            .as_ref()
            .map(|p| p.hosts())
            .unwrap_or_default()
            .into_iter()
            .map(|host| DomainSnapshot {
                is_apex: host.chars().filter(|c| *c == '.').count() <= 1,
                domain: host,
                redirect_to: None,
                redirect_status_code: None,
                environment: Some("production".to_string()),
                verified: true,
            })
            .collect();

        ProjectSnapshot {
            id: WorkloadId::new(config.service.clone()),
            name: config.service.clone(),
            primary_workload,
            additional_workloads,
            services,
            domains,
            // deploy.yml has no repository field — Kamal builds the repo it
            // lives in, so the user links it in the wizard.
            git_info: None,
            detected_framework: None,
            source_metadata: serde_json::json!({
                "accessories_skipped": config
                    .accessories
                    .iter()
                    .filter(|(_, a)| detect_service_type(&a.image).is_none())
                    .map(|(n, a)| serde_json::json!({ "name": n, "image": a.image }))
                    .collect::<Vec<_>>(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// WorkloadImporter implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl WorkloadImporter for KamalImporter {
    fn source(&self) -> ImportSource {
        ImportSource::Kamal
    }

    fn name(&self) -> &str {
        "Kamal"
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
        match config_from(credentials) {
            Ok(config) => Ok(CredentialValidation {
                valid: true,
                account_name: Some(config.service.clone()),
                message: Some(format!(
                    "Parsed deploy.yml for '{}': {} role(s), {} accessory(ies)",
                    config.service,
                    config.servers.len(),
                    config.accessories.len()
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
        let config = config_from(credentials).map_err(ImportError::from)?;

        // The whole config is one importable project; roles are shown so the
        // user can see what it contains before selecting.
        let mut descriptors = vec![WorkloadDescriptor {
            id: WorkloadId::new(config.service.clone()),
            name: Some(config.service.clone()),
            workload_type: WorkloadType::Container,
            status: WorkloadStatus::Unknown,
            image: Some(config.image_reference()),
            created_at: None,
            labels: HashMap::from([
                ("roles".to_string(), config.servers.len().to_string()),
                (
                    "accessories".to_string(),
                    config.accessories.len().to_string(),
                ),
            ]),
        }];

        if let Some(pattern) = &selector.name_pattern {
            let needle = pattern.to_lowercase();
            descriptors.retain(|d| {
                d.name
                    .as_deref()
                    .map(|n| n.to_lowercase().contains(&needle))
                    .unwrap_or(false)
            });
        }

        info!(
            "Parsed Kamal config for '{}' ({} role(s), {} accessory(ies))",
            config.service,
            config.servers.len(),
            config.accessories.len()
        );
        Ok(descriptors)
    }

    async fn describe(
        &self,
        credentials: &ImportCredentials,
        workload_id: &WorkloadId,
    ) -> ImportResult<WorkloadSnapshot> {
        let config = config_from(credentials).map_err(ImportError::from)?;
        // Accept either the service name or "service/role"
        let requested_role = workload_id
            .as_str()
            .strip_prefix(&format!("{}/", config.service))
            .map(|r| r.to_string());

        match requested_role {
            Some(role_name) => {
                let role = config.servers.get(&role_name).ok_or_else(|| {
                    ImportError::from(KamalImportError::WorkloadNotFound {
                        name: role_name.clone(),
                    })
                })?;
                Ok(role_to_snapshot(&config, &role_name, role))
            }
            None => {
                if workload_id.as_str() != config.service {
                    return Err(ImportError::from(KamalImportError::WorkloadNotFound {
                        name: workload_id.as_str().to_string(),
                    }));
                }
                Ok(self.snapshot_from_config(&config).primary_workload)
            }
        }
    }

    async fn describe_project(
        &self,
        credentials: &ImportCredentials,
        workload_id: &WorkloadId,
    ) -> ImportResult<ProjectSnapshot> {
        let config = config_from(credentials).map_err(ImportError::from)?;
        if workload_id.as_str() != config.service {
            return Err(ImportError::from(KamalImportError::WorkloadNotFound {
                name: workload_id.as_str().to_string(),
            }));
        }
        Ok(self.snapshot_from_config(&config))
    }

    fn generate_plan(&self, snapshot: WorkloadSnapshot) -> ImportResult<ImportPlan> {
        // Workload-level plans are only reachable for sources without project
        // snapshots; Kamal always has one, so rebuild a minimal config view.
        let project = ProjectSnapshot {
            id: snapshot.id.clone(),
            name: snapshot
                .name
                .clone()
                .unwrap_or_else(|| snapshot.id.as_str().to_string()),
            primary_workload: snapshot,
            additional_workloads: vec![],
            services: vec![],
            domains: vec![],
            git_info: None,
            detected_framework: None,
            source_metadata: serde_json::Value::Null,
        };
        let config = KamalConfig {
            service: project.name.clone(),
            image: project
                .primary_workload
                .image
                .clone()
                .unwrap_or_else(|| project.name.clone()),
            servers: Default::default(),
            proxy: None,
            registry: None,
            builder: None,
            env: None,
            volumes: vec![],
            accessories: Default::default(),
            asset_path: None,
            ssh: None,
        };
        self.build_plan(&config, &project)
    }

    fn generate_project_plan(&self, snapshot: ProjectSnapshot) -> ImportResult<ImportPlan> {
        // Rebuild the config view from the snapshot's own metadata so the plan
        // can be regenerated without the original YAML.
        let config = KamalConfig {
            service: snapshot.name.clone(),
            image: snapshot
                .primary_workload
                .image
                .clone()
                .unwrap_or_else(|| snapshot.name.clone()),
            servers: Default::default(),
            proxy: None,
            registry: None,
            builder: None,
            env: None,
            volumes: snapshot
                .primary_workload
                .source_metadata
                .get("volumes")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            accessories: Default::default(),
            asset_path: None,
            ssh: None,
        };
        self.build_plan(&config, &snapshot)
    }

    fn validation_rules(&self) -> Vec<Box<dyn temps_import_types::ImportValidationRule>> {
        KamalValidationRules::all_rules()
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
            supports_build: true,  // Kamal builds a Dockerfile; temps can too
            supports_stacks: true, // roles + accessories import as one project
            supports_services: true,
            supports_domains: true, // proxy.host is a real domain
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
    use temps_entities::{deployments, environments, prelude::DeploymentMetadata, projects};

    let start_time = Instant::now();
    info!(
        "Executing Kamal import for session: {} (dry_run: {})",
        context.session_id, context.dry_run
    );

    let step_results: Vec<StepResult> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // Secrets are imported as empty placeholders — say so in the outcome.
    let secret_count = plan
        .deployment
        .env_vars
        .iter()
        .filter(|v| v.is_secret)
        .count();
    if secret_count > 0 {
        warnings.push(format!(
            "{} secret environment variable(s) were created empty — set their values from your .kamal/secrets source before deploying",
            secret_count
        ));
    }

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
        environment_variables: Some(
            plan.deployment
                .env_vars
                .iter()
                .filter(|env| !env.is_secret)
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
        // Kamal deploys a pre-built image from deploy.yml, no git repo —
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
        builder: Some("kamal-import".to_string()),
        labels: vec!["imported".to_string(), "kamal".to_string()],
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
            "Imported from Kamal deploy.yml ({})",
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
        "✅ Kamal import completed in {:.2}s — project {} (id: {})",
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

    /// Same config the model tests use — validated with `kamal config` (2.12).
    const FULL_CONFIG: &str = r#"
service: lab-shop
image: acme/lab-shop
servers:
  web:
    - 192.168.0.1
  job:
    hosts:
      - 192.168.0.2
    cmd: bin/jobs
proxy:
  ssl: true
  host: shop.example.com
  app_port: 3000
registry:
  server: ghcr.io
  username: acme-bot
  password:
    - KAMAL_REGISTRY_PASSWORD
builder:
  arch: amd64
  dockerfile: Dockerfile
env:
  clear:
    PLAN: pro
    FEATURE_CHECKOUT: "true"
  secret:
    - RAILS_MASTER_KEY
    - LAB_SECRET
volumes:
  - "app_storage:/app/storage"
accessories:
  db:
    image: postgres:16
    host: 192.168.0.2
    port: 5432
    env:
      clear:
        POSTGRES_USER: lab
        POSTGRES_DB: labdb
      secret:
        - POSTGRES_PASSWORD
    directories:
      - data:/var/lib/postgresql/data
  redis:
    image: valkey/valkey:8
    host: 192.168.0.2
    port: 6379
    directories:
      - data:/data
"#;

    fn credentials(yaml: &str) -> ImportCredentials {
        let mut extra = HashMap::new();
        extra.insert(DEPLOY_YML_KEY.to_string(), yaml.to_string());
        ImportCredentials {
            extra,
            ..Default::default()
        }
    }

    fn snapshot() -> ProjectSnapshot {
        let config = KamalConfig::parse(FULL_CONFIG).unwrap();
        KamalImporter::new().snapshot_from_config(&config)
    }

    fn plan() -> ImportPlan {
        let config = KamalConfig::parse(FULL_CONFIG).unwrap();
        let importer = KamalImporter::new();
        let snapshot = importer.snapshot_from_config(&config);
        importer.build_plan(&config, &snapshot).unwrap()
    }

    #[tokio::test]
    async fn validates_a_real_config() {
        let importer = KamalImporter::new();
        let result = importer
            .validate_credentials(&credentials(FULL_CONFIG))
            .await
            .unwrap();
        assert!(result.valid);
        assert_eq!(result.account_name.as_deref(), Some("lab-shop"));
        assert!(result.message.unwrap().contains("2 accessory"));
    }

    #[tokio::test]
    async fn rejects_missing_config_with_guidance() {
        let importer = KamalImporter::new();
        let result = importer
            .validate_credentials(&ImportCredentials::default())
            .await
            .unwrap();
        assert!(!result.valid);
        assert!(result.message.unwrap().contains("deploy.yml"));
    }

    #[test]
    fn snapshot_maps_roles_accessories_and_domain() {
        let snapshot = snapshot();
        assert_eq!(snapshot.name, "lab-shop");
        // web is primary, job is additional
        assert_eq!(snapshot.primary_workload.name.as_deref(), Some("lab-shop"));
        assert_eq!(snapshot.additional_workloads.len(), 1);
        assert_eq!(
            snapshot.additional_workloads[0].name.as_deref(),
            Some("lab-shop-job")
        );
        assert_eq!(
            snapshot.additional_workloads[0].workload_type,
            WorkloadType::Worker
        );
        assert_eq!(
            snapshot.additional_workloads[0].command,
            Some(vec!["bin/jobs".to_string()])
        );
        // proxy.host is a REAL domain, unlike the IP-derived ones elsewhere
        assert_eq!(snapshot.domains.len(), 1);
        assert_eq!(snapshot.domains[0].domain, "shop.example.com");
        // both accessories are recognised (postgres + valkey)
        assert_eq!(snapshot.services.len(), 2);
        // only the web role is web-facing, so it carries the app port
        assert!(snapshot.primary_workload.ports.contains_key(&3000));
        assert!(snapshot.additional_workloads[0].ports.is_empty());
    }

    #[test]
    fn secrets_import_as_flagged_placeholders_without_values() {
        let plan = plan();
        let secrets: Vec<&EnvironmentVariable> = plan
            .deployment
            .env_vars
            .iter()
            .filter(|v| v.is_secret)
            .collect();
        assert_eq!(secrets.len(), 2);
        for secret in secrets {
            assert!(
                secret.value.is_empty(),
                "secret values are not in deploy.yml and must not be invented"
            );
            assert!(secret
                .source_description
                .as_deref()
                .unwrap()
                .contains(".kamal/secrets"));
        }
        assert!(plan
            .summary
            .critical_warnings
            .iter()
            .any(|w| w.contains("secret")));
    }

    #[test]
    fn accessories_never_claim_to_be_auto_copyable() {
        let plan = plan();
        for service in &plan.services {
            assert_eq!(
                service.parameters.get("reachable"),
                Some(&serde_json::json!(false))
            );
            assert!(
                !service.parameters.contains_key("source_url"),
                "without the Kamal secret there is no usable DSN — the executor must not try"
            );
            assert!(service.data_implications.iter().any(|i| i
                .recommended_action
                .as_deref()
                .is_some_and(|a| a.contains("kamal accessory exec"))));
        }
    }

    #[test]
    fn domain_imports_rather_than_being_skipped() {
        let plan = plan();
        assert_eq!(plan.summary.resource_counts.domains, 1);
        assert_eq!(plan.domains[0].action, DomainAction::Import);
        assert!(plan.domains[0].action_description.contains("DNS"));
    }

    #[test]
    fn plan_reports_dockerfile_build_and_volumes_as_unsupported() {
        let plan = plan();
        let build = plan
            .deployment
            .build
            .as_ref()
            .expect("kamal builds a Dockerfile");
        assert_eq!(build.dockerfile.as_deref(), Some("Dockerfile"));
        assert!(plan
            .summary
            .unsupported_features
            .iter()
            .any(|f| f.feature.contains("volumes")));
        assert!(plan
            .summary
            .unsupported_features
            .iter()
            .any(|f| f.feature.contains("secrets")));
        assert_eq!(plan.summary.overall_risk, RiskLevel::High);
    }

    #[test]
    fn plan_warns_the_first_deployment_needs_a_manual_trigger() {
        let plan = plan();
        assert!(
            plan.summary
                .manual_actions_required
                .iter()
                .any(|a| a.description.contains("trigger the first deployment")),
            "deploy.yml never carries a repository, so the plan must tell the user \
             the first deployment won't start automatically"
        );
    }

    /// Regression: the orchestrator calls generate_project_plan (not
    /// build_plan directly). That path rebuilds a config view, and reading env
    /// from it silently dropped every secret placeholder.
    #[test]
    fn project_plan_path_keeps_secret_placeholders_and_clear_values() {
        let importer = KamalImporter::new();
        let plan = importer.generate_project_plan(snapshot()).unwrap();

        let secrets: Vec<&str> = plan
            .deployment
            .env_vars
            .iter()
            .filter(|v| v.is_secret)
            .map(|v| v.key.as_str())
            .collect();
        assert_eq!(
            secrets,
            vec!["LAB_SECRET", "RAILS_MASTER_KEY"],
            "secret placeholders must survive the project-plan path"
        );
        assert!(plan
            .deployment
            .env_vars
            .iter()
            .any(|v| v.key == "PLAN" && v.value == "pro" && !v.is_secret));
        assert!(plan
            .summary
            .critical_warnings
            .iter()
            .any(|w| w.contains("secret")));
    }

    #[tokio::test]
    async fn discover_returns_the_service_with_role_counts() {
        let importer = KamalImporter::new();
        let found = importer
            .discover(&credentials(FULL_CONFIG), ImportSelector::default())
            .await
            .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name.as_deref(), Some("lab-shop"));
        assert_eq!(
            found[0].labels.get("accessories").map(|s| s.as_str()),
            Some("2")
        );
    }

    #[tokio::test]
    async fn describe_project_rejects_an_unknown_service_name() {
        let importer = KamalImporter::new();
        let err = importer
            .describe_project(&credentials(FULL_CONFIG), &WorkloadId::new("other-app"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("other-app"));
    }

    #[tokio::test]
    async fn describe_supports_addressing_a_single_role() {
        let importer = KamalImporter::new();
        let snapshot = importer
            .describe(&credentials(FULL_CONFIG), &WorkloadId::new("lab-shop/job"))
            .await
            .unwrap();
        assert_eq!(snapshot.name.as_deref(), Some("lab-shop-job"));
        assert_eq!(snapshot.command, Some(vec!["bin/jobs".to_string()]));
    }
}
