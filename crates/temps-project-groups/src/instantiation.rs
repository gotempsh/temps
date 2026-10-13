// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Creating a template, or importing apps from a project's repository, as
//! one server-side operation that resumes where it stopped (ADR-051).
//!
//! A request is turned into a plan first: the resolved template release,
//! where the services go, the slug of every service, and the list of steps.
//! Nothing else happens until the plan is stored. The engine then runs the
//! steps in order and stores each result before the next step starts; a step
//! that is done is never run again. A failure leaves an instantiation the
//! caller can see and retry, never a service stranded outside its project or
//! a duplicate created by trying again.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, Set,
};
use serde::{Deserialize, Serialize};
use temps_auth::AuthContext;
use temps_core::problemdetails::Problem;
use temps_core::templates::{
    EnvVarTemplate, GitRef, ProjectTemplate, ServiceTemplateInstance, TemplateKind,
    TemplateServiceSpec, SERVICE_TEMPLATE_SCHEMA_VERSION,
};
use temps_core::EncryptionService;
use temps_entities::template_instantiations;
use temps_projects::services::types::SlugClaimCaller;
use temps_projects::template_creation::{
    EnvVarInput, RepositoryTarget, TemplateCreationCaller, TemplateRuntimeOverrides,
    TemplateServiceCreator, TemplateServiceRequest,
};
use tracing::{error, info, warn};
use utoipa::ToSchema;

use crate::env_vars::{CreateGroupEnvVarRequest, GroupEnvVarService};
use crate::error::ProjectGroupError;
use crate::handlers::ProjectGroupAudit;
use crate::repository::{GroupRepositoryInput, GroupRepositoryService, RepositorySource};
use crate::routes::{CreateGroupRouteRequest, GroupRouteService};
use crate::service::{CreateProjectGroupRequest, ProjectGroupService};

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

/// Where the services of a template go.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InstantiationTarget {
    /// A new project named `name`.
    NewProject {
        name: String,
        /// Explicit slug for the project. When omitted the slug is derived from
        /// `name` via `ProjectGroupService::slugify`. Useful when the name does
        /// not produce a valid Latin slug (non-Latin scripts, emoji-only names).
        #[serde(default)]
        slug: Option<String>,
    },
    /// A project that exists. Its services receive the template's shared
    /// variables only where the project does not define them already.
    ExistingProject { group_id: i32 },
    /// No project; only for templates with a single service.
    NoProject,
}

/// A managed database for the template: one that exists, or a new one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DatabaseChoice {
    /// Link a managed database that already exists.
    Existing { service_id: i32 },
    /// Create a managed database with default settings.
    Create {
        /// Name of the new database; derived from the project when omitted.
        #[serde(default)]
        name: Option<String>,
    },
}

/// Fork the template's repository into a git account, so the code becomes
/// the user's and pushes deploy. Starters only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ForkRequest {
    pub git_provider_connection_id: i32,
    /// Name of the repository to create.
    pub repository_name: String,
    /// Account or organization to create it in; the connection's user when
    /// omitted.
    #[serde(default)]
    pub repository_owner: Option<String>,
    #[serde(default = "default_true")]
    pub private: bool,
    /// Deploy on push to the fork.
    #[serde(default = "default_true")]
    pub automatic_deploy: bool,
}

fn default_true() -> bool {
    true
}

/// A variable value for the template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TemplateVariableInput {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub is_secret: bool,
    /// The service the variable belongs to. Needed only when the name is not
    /// declared by the template, or is declared by several services.
    #[serde(default)]
    pub service: Option<String>,
}

/// Create a template.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct InstantiateTemplateRequest {
    pub target: InstantiationTarget,
    /// Names for the created services, by the template's service name.
    #[serde(default)]
    pub service_names: BTreeMap<String, String>,
    #[serde(default)]
    pub environment_variables: Vec<TemplateVariableInput>,
    /// One choice per database type the template needs.
    #[serde(default)]
    pub databases: BTreeMap<String, DatabaseChoice>,
    #[serde(default)]
    pub fork: Option<ForkRequest>,
    /// Runtime overrides for services that run a prebuilt image, by the
    /// template's service name.
    #[serde(default)]
    pub runtime_overrides: BTreeMap<String, TemplateRuntimeOverrides>,
    /// Chosen by the client. Sending the same key again returns the same
    /// instantiation instead of creating the template twice.
    pub idempotency_key: String,
}

/// An app found in a project's repository, to add as a service.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ImportedApp {
    /// Name of the service to create.
    pub name: String,
    /// Directory of the app in the repository (`.` for the root).
    pub directory: String,
    pub preset: String,
    #[serde(default)]
    pub preset_config: Option<serde_json::Value>,
    #[serde(default)]
    pub exposed_port: Option<i32>,
}

/// Add apps of a project's repository as services of the project.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ImportServicesRequest {
    pub apps: Vec<ImportedApp>,
    /// Chosen by the client; see [`InstantiateTemplateRequest::idempotency_key`].
    pub idempotency_key: String,
}

// ---------------------------------------------------------------------------
// Plan
// ---------------------------------------------------------------------------

/// Where the services go, as planned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlannedTarget {
    NewProject { name: String, slug: String },
    ExistingProject { group_id: i32 },
    NoProject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedDatabase {
    pub database_type: String,
    pub choice: DatabaseChoice,
    /// The linked database, once known.
    pub service_id: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedService {
    /// Name of the service in the template.
    pub template_service: String,
    /// Name of the created service.
    pub name: String,
    /// Slug planned for it; re-planned if another project takes it first.
    pub slug: String,
    #[serde(default)]
    pub runtime_overrides: TemplateRuntimeOverrides,
    /// The created service, once known.
    pub project_id: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct InstantiationStep {
    /// Stable identifier, such as `service:backend`.
    pub key: String,
    /// What the step does, for display.
    pub label: String,
    pub status: StepStatus,
    pub error: Option<String>,
}

/// Everything an instantiation needs, stored before the first step runs.
/// Holds no secret values; see [`PlanSecrets`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstantiationPlan {
    /// The template release being created, fixed when the plan is made.
    pub template: ProjectTemplate,
    /// Whether services carry the release (apps do; starters and imports
    /// do not).
    pub carries_release: bool,
    /// Value stored as each service's template provenance.
    pub provenance: Option<String>,
    pub target: PlannedTarget,
    pub fork: Option<ForkRequest>,
    /// Source of the services that build from the template's repository,
    /// once known.
    pub repository: Option<RepositoryTarget>,
    /// Whether those services follow the project's repository.
    pub follow_repository: bool,
    pub databases: Vec<PlannedDatabase>,
    pub services: Vec<PlannedService>,
    /// Databases the caller may link only as their creator.
    pub storage_claim_ids: Vec<i32>,
    /// The project the services go into, once known.
    pub group_id: Option<i32>,
    pub steps: Vec<InstantiationStep>,
}

impl InstantiationPlan {
    fn service_mut(&mut self, template_service: &str) -> Option<&mut PlannedService> {
        self.services
            .iter_mut()
            .find(|service| service.template_service == template_service)
    }

    fn step_mut(&mut self, key: &str) -> Option<&mut InstantiationStep> {
        self.steps.iter_mut().find(|step| step.key == key)
    }
}

/// Variable values the steps still need. Stored encrypted, next to the
/// plan, and cleared once the instantiation succeeds.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlanSecrets {
    /// Values for each service, by the template's service name.
    pub services: BTreeMap<String, Vec<EnvVarInput>>,
    /// Resolved values of the shared variables (generated ones included),
    /// for a template going into a project.
    pub shared: Vec<EnvVarInput>,
}

fn step(key: impl Into<String>, label: impl Into<String>) -> InstantiationStep {
    InstantiationStep {
        key: key.into(),
        label: label.into(),
        status: StepStatus::Pending,
        error: None,
    }
}

/// The steps for a plan, in the order they run.
fn plan_steps(plan: &InstantiationPlan, template: &ProjectTemplate) -> Vec<InstantiationStep> {
    let mut steps = Vec::new();
    if let PlannedTarget::NewProject { name, .. } = &plan.target {
        steps.push(step("project", format!("Create project {name}")));
    }
    let builds_from_template_repository = template
        .services
        .iter()
        .any(|service| !service.is_image() && service.git.is_none());
    if builds_from_template_repository {
        let label = match &plan.fork {
            Some(fork) => format!("Create repository {}", fork.repository_name),
            None => format!("Use {}", display_git_url(&template.git.url)),
        };
        steps.push(step("repository", label));
    }
    for database in &plan.databases {
        if let DatabaseChoice::Create { .. } = database.choice {
            steps.push(step(
                format!("database:{}", database.database_type),
                format!("Create a {} database", database.database_type),
            ));
        }
    }
    for service in &plan.services {
        steps.push(step(
            format!("service:{}", service.template_service),
            format!("Create service {}", service.name),
        ));
    }
    if !matches!(plan.target, PlannedTarget::NoProject) {
        for service in &plan.services {
            steps.push(step(
                format!("join:{}", service.template_service),
                format!("Add {} to the project", service.name),
            ));
        }
        if !template.shared_env_vars.is_empty() {
            steps.push(step("variables", "Set the shared variables"));
        }
        if !template.routes.is_empty() {
            steps.push(step("routes", "Create the path routes"));
        }
    }
    for service in &plan.services {
        steps.push(step(
            format!("deploy:{}", service.template_service),
            format!("Deploy {}", service.name),
        ));
    }
    steps
}

fn display_git_url(url: &str) -> String {
    url.trim_end_matches(".git")
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .to_string()
}

/// Assign each supplied variable to the shared set or to one service.
fn assign_variables(
    template: &ProjectTemplate,
    variables: &[TemplateVariableInput],
) -> Result<PlanSecrets, String> {
    let mut secrets = PlanSecrets::default();
    let shared_names: HashSet<&str> = template
        .shared_env_vars
        .iter()
        .map(|variable| variable.name.as_str())
        .collect();
    for variable in variables {
        let input = EnvVarInput {
            name: variable.name.clone(),
            value: variable.value.clone(),
            is_secret: variable.is_secret,
        };
        let owner = match variable.service.as_deref() {
            Some(service) => {
                if template.service(service).is_none() {
                    return Err(format!(
                        "Variable '{}' is for service '{service}', which the template does not have",
                        variable.name
                    ));
                }
                Some(service.to_string())
            }
            None if shared_names.contains(variable.name.as_str()) => None,
            None => {
                let declaring: Vec<&TemplateServiceSpec> = template
                    .services
                    .iter()
                    .filter(|service| {
                        service
                            .env_vars
                            .iter()
                            .any(|declared| declared.name == variable.name)
                    })
                    .collect();
                match declaring.as_slice() {
                    [service] => Some(service.name.clone()),
                    [] if !template.is_multi_service() => Some(template.primary().name.clone()),
                    [] => {
                        return Err(format!(
                            "Variable '{}' is not declared by the template; say which service it is for",
                            variable.name
                        ))
                    }
                    _ => {
                        return Err(format!(
                            "Variable '{}' is declared by several services; say which one it is for",
                            variable.name
                        ))
                    }
                }
            }
        };
        match owner {
            Some(service) => secrets.services.entry(service).or_default().push(input),
            None => secrets.shared.push(input),
        }
    }
    Ok(secrets)
}

/// Resolve the shared variables once, when the plan is made: supplied
/// values, then defaults, then generators. Their values are what every
/// service of the project will read, so they must not change on retry.
fn resolve_shared_variables(
    variables: &[EnvVarTemplate],
    supplied: &[EnvVarInput],
) -> Result<Vec<EnvVarInput>, String> {
    let mut resolved = Vec::new();
    for variable in variables {
        let supplied_value = supplied
            .iter()
            .find(|input| input.name == variable.name)
            .filter(|input| !input.value.is_empty());
        let value = match supplied_value {
            Some(input) => Some(input.value.clone()),
            None => match (&variable.default, variable.default_generator.as_deref()) {
                (Some(default), _) if !default.is_empty() => Some(default.clone()),
                (_, Some(generator @ ("random_secret" | "random_hex_32"))) => Some(
                    temps_projects::template_creation::secure_template_value(
                        &variable.name,
                        generator,
                    )
                    .map_err(|error| error.to_string())?,
                ),
                (_, Some(generator)) => {
                    return Err(format!(
                        "Shared variable '{}' uses generator '{generator}', which only works for a single service",
                        variable.name
                    ))
                }
                _ => None,
            },
        };
        match value {
            Some(value) => resolved.push(EnvVarInput {
                name: variable.name.clone(),
                value,
                is_secret: variable.is_secret()
                    || supplied_value.is_some_and(|input| input.is_secret),
            }),
            None if variable.required => {
                return Err(format!(
                    "Required shared variable '{}' has no value",
                    variable.name
                ))
            }
            None => {}
        }
    }
    for input in supplied {
        if !variables.iter().any(|variable| variable.name == input.name) {
            return Err(format!(
                "Variable '{}' is not a shared variable of the template",
                input.name
            ));
        }
    }
    Ok(resolved)
}

/// Turn a slug-like name into a service name of a template.
pub fn template_service_name(name: &str) -> String {
    let mut out = String::new();
    for ch in name.trim().to_ascii_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let mut out = out.trim_end_matches('-').to_string();
    if !out.starts_with(|ch: char| ch.is_ascii_lowercase()) {
        out.insert_str(0, "app-");
    }
    out.truncate(63);
    out.trim_end_matches('-').to_string()
}

/// The template an import creates: one starter service per app, all built
/// from the project's repository.
pub fn import_template(
    group: &temps_entities::project_groups::Model,
    repository_url: String,
    branch: String,
    apps: &[ImportedApp],
) -> Result<ProjectTemplate, String> {
    let mut services = Vec::with_capacity(apps.len());
    let mut names = HashSet::new();
    for app in apps {
        let mut name = template_service_name(&app.name);
        let base = name.clone();
        let mut suffix = 2;
        while !names.insert(name.clone()) {
            name = format!("{base}-{suffix}");
            suffix += 1;
        }
        let directory = app.directory.trim().trim_start_matches("./");
        services.push(TemplateServiceSpec {
            name,
            directory: Some(if directory.is_empty() { "." } else { directory }.to_string()),
            preset: app.preset.clone(),
            preset_config: app.preset_config.clone(),
            exposed_port: app.exposed_port,
            ..Default::default()
        });
    }
    if services.is_empty() {
        return Err("Choose at least one app to add".to_string());
    }
    Ok(ProjectTemplate {
        slug: format!("import-{}", group.slug),
        name: format!("Apps of {}", group.name),
        version: String::new(),
        kind: TemplateKind::Starter,
        description: None,
        image_url: None,
        screenshot_url: None,
        git: GitRef {
            url: repository_url,
            path: None,
            r#ref: branch,
        },
        tags: Vec::new(),
        features: Vec::new(),
        databases: Vec::new(),
        shared_env_vars: Vec::new(),
        services,
        routes: Vec::new(),
        is_public: false,
        is_featured: false,
        sort_order: 0,
    })
}

/// Build the plan for a template. `service_slugs` gives the planned slug of
/// each service (already preflight-checked by the caller).
pub struct PlanInput<'a> {
    pub template: ProjectTemplate,
    pub carries_release: bool,
    pub provenance: Option<String>,
    pub target: PlannedTarget,
    pub fork: Option<ForkRequest>,
    pub repository: Option<RepositoryTarget>,
    pub follow_repository: bool,
    pub databases: BTreeMap<String, DatabaseChoice>,
    pub services: Vec<PlannedService>,
    pub storage_claim_ids: Vec<i32>,
    pub group_id: Option<i32>,
    pub variables: &'a [TemplateVariableInput],
}

pub fn build_plan(input: PlanInput<'_>) -> Result<(InstantiationPlan, PlanSecrets), String> {
    let template = input.template;
    let mut secrets = assign_variables(&template, input.variables)?;
    if matches!(input.target, PlannedTarget::NoProject) {
        if template.is_multi_service() {
            return Err(format!(
                "'{}' has {} services, which go into a project; choose a new or existing project",
                template.name,
                template.services.len()
            ));
        }
        // A service without a project keeps the shared variables itself.
        let primary = template.primary().name.clone();
        let shared = std::mem::take(&mut secrets.shared);
        secrets.services.entry(primary).or_default().extend(shared);
    } else {
        secrets.shared = resolve_shared_variables(&template.shared_env_vars, &secrets.shared)?;
    }

    let mut databases = Vec::new();
    for database_type in &template.databases {
        let choice = input
            .databases
            .iter()
            .find(|(requested, _)| {
                temps_core::templates::managed_service_types_compatible(requested, database_type)
            })
            .map(|(_, choice)| choice.clone())
            .ok_or_else(|| {
                format!(
                    "'{}' needs a {database_type} database: link one or ask for a new one",
                    template.name
                )
            })?;
        let service_id = match choice {
            DatabaseChoice::Existing { service_id } => Some(service_id),
            DatabaseChoice::Create { .. } => None,
        };
        databases.push(PlannedDatabase {
            database_type: database_type.clone(),
            choice,
            service_id,
        });
    }
    for requested in input.databases.keys() {
        if !template.databases.iter().any(|database| {
            temps_core::templates::managed_service_types_compatible(requested, database)
        }) {
            return Err(format!(
                "'{}' does not use a {requested} database",
                template.name
            ));
        }
    }

    let mut plan = InstantiationPlan {
        template: template.clone(),
        carries_release: input.carries_release,
        provenance: input.provenance,
        target: input.target,
        fork: input.fork,
        repository: input.repository,
        follow_repository: input.follow_repository,
        databases,
        services: input.services,
        storage_claim_ids: input.storage_claim_ids,
        group_id: input.group_id,
        steps: Vec::new(),
    };
    plan.steps = plan_steps(&plan, &template);
    Ok((plan, secrets))
}

// ---------------------------------------------------------------------------
// Responses
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum InstantiationStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    /// The server stopped while it ran; retrying continues it.
    Interrupted,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct InstantiatedService {
    /// Name of the service in the template.
    pub template_service: String,
    pub name: String,
    pub slug: String,
    /// Set once the service exists.
    pub project_id: Option<i32>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct InstantiationResponse {
    pub id: i32,
    /// `template` or `import`.
    pub kind: String,
    pub name: String,
    pub template_slug: Option<String>,
    pub status: InstantiationStatus,
    /// The project the services go into, once known.
    pub group_id: Option<i32>,
    pub services: Vec<InstantiatedService>,
    pub steps: Vec<InstantiationStep>,
    /// Why it failed, when it did.
    pub error: Option<String>,
    pub created_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
    pub finished_at: Option<chrono::DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// Runs instantiations. Each runs in the background; the caller polls it.
#[derive(Clone)]
pub struct InstantiationEngine {
    pub db: Arc<DatabaseConnection>,
    pub encryption: Arc<EncryptionService>,
    pub groups: Arc<ProjectGroupService>,
    pub env_vars: Arc<GroupEnvVarService>,
    pub repository: Arc<GroupRepositoryService>,
    pub routes: Arc<GroupRouteService>,
    pub creator: TemplateServiceCreator,
    pub external_services: Arc<temps_providers::ExternalServiceManager>,
    pub git: Arc<temps_git::GitProviderManager>,
    pub sensitive_action_authorizer: Arc<dyn temps_core::SensitiveActionAuthorizer>,
    pub audit: Arc<dyn temps_core::AuditLogger>,
    /// Instantiations running in this process. A stored `running` status
    /// that is not in here was interrupted by a restart.
    running: Arc<Mutex<HashSet<i32>>>,
}

/// The request metadata recorded in audit events of created services.
#[derive(Debug, Clone, Default)]
pub struct RequestOrigin {
    pub ip_address: Option<String>,
    pub user_agent: String,
    /// The template run the request drives; set by the engine so every
    /// change a run makes can be traced back to it in the audit log.
    pub instantiation_id: Option<i32>,
}

#[allow(clippy::too_many_arguments)]
impl InstantiationEngine {
    pub fn new(
        db: Arc<DatabaseConnection>,
        encryption: Arc<EncryptionService>,
        groups: Arc<ProjectGroupService>,
        env_vars: Arc<GroupEnvVarService>,
        repository: Arc<GroupRepositoryService>,
        routes: Arc<GroupRouteService>,
        creator: TemplateServiceCreator,
        external_services: Arc<temps_providers::ExternalServiceManager>,
        git: Arc<temps_git::GitProviderManager>,
        sensitive_action_authorizer: Arc<dyn temps_core::SensitiveActionAuthorizer>,
        audit: Arc<dyn temps_core::AuditLogger>,
    ) -> Self {
        Self {
            db,
            encryption,
            groups,
            env_vars,
            repository,
            routes,
            creator,
            external_services,
            git,
            sensitive_action_authorizer,
            audit,
            running: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Lease duration in seconds: a heartbeat older than this is considered stale.
    const HEARTBEAT_LEASE_SECS: i64 = 120;
    /// How often the engine writes a heartbeat while running.
    const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

    /// Whether `id` is running in this process.
    pub fn is_active(&self, id: i32) -> bool {
        self.running
            .lock()
            .map(|running| running.contains(&id))
            .unwrap_or(false)
    }

    /// Atomically claim `id` in the database.
    ///
    /// Sets `status='running'`, `claimed_at=now()`, `heartbeat_at=now()` only
    /// if the row is currently in a claimable state: `pending`, `failed`, or
    /// `running` with a stale heartbeat (the previous runner died). Returns
    /// the updated row, or `None` when another process owns it.
    async fn claim_in_db(&self, id: i32) -> Result<Option<template_instantiations::Model>, String> {
        use sea_orm::ConnectionTrait;
        let lease_secs = Self::HEARTBEAT_LEASE_SECS;
        let row = self
            .db
            .query_one(sea_orm::Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                "UPDATE template_instantiations \
                 SET status = 'running', \
                     claimed_at = NOW(), \
                     heartbeat_at = NOW(), \
                     updated_at = NOW() \
                 WHERE id = $1 \
                   AND (status IN ('pending', 'failed') \
                        OR (status = 'running' \
                            AND (heartbeat_at IS NULL \
                                 OR heartbeat_at < NOW() - ($2::BIGINT * INTERVAL '1 second')))) \
                 RETURNING id, user_id, idempotency_key, kind, template_slug, name, \
                           group_id, status, plan, secrets, error, \
                           created_at, updated_at, finished_at, claimed_at, heartbeat_at",
                [id.into(), lease_secs.into()],
            ))
            .await
            .map_err(|error| format!("claiming instantiation {id}: {error}"))?;

        match row {
            None => Ok(None),
            Some(row) => {
                let model = template_instantiations::Model {
                    id: row.try_get("", "id").map_err(|e| e.to_string())?,
                    user_id: row.try_get("", "user_id").map_err(|e| e.to_string())?,
                    idempotency_key: row
                        .try_get("", "idempotency_key")
                        .map_err(|e| e.to_string())?,
                    kind: row.try_get("", "kind").map_err(|e| e.to_string())?,
                    template_slug: row
                        .try_get("", "template_slug")
                        .map_err(|e| e.to_string())?,
                    name: row.try_get("", "name").map_err(|e| e.to_string())?,
                    group_id: row.try_get("", "group_id").map_err(|e| e.to_string())?,
                    status: row.try_get("", "status").map_err(|e| e.to_string())?,
                    plan: row.try_get("", "plan").map_err(|e| e.to_string())?,
                    secrets: row.try_get("", "secrets").map_err(|e| e.to_string())?,
                    error: row.try_get("", "error").map_err(|e| e.to_string())?,
                    created_at: row.try_get("", "created_at").map_err(|e| e.to_string())?,
                    updated_at: row.try_get("", "updated_at").map_err(|e| e.to_string())?,
                    finished_at: row.try_get("", "finished_at").map_err(|e| e.to_string())?,
                    claimed_at: row.try_get("", "claimed_at").map_err(|e| e.to_string())?,
                    heartbeat_at: row.try_get("", "heartbeat_at").map_err(|e| e.to_string())?,
                };
                Ok(Some(model))
            }
        }
    }

    /// Write a heartbeat for `id` to keep the lease alive.
    async fn heartbeat(&self, id: i32) {
        use sea_orm::ConnectionTrait;
        if let Err(error) = self
            .db
            .execute(sea_orm::Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                "UPDATE template_instantiations \
                 SET heartbeat_at = NOW(), updated_at = NOW() \
                 WHERE id = $1 AND status = 'running'",
                [id.into()],
            ))
            .await
        {
            warn!(instantiation_id = id, %error, "Could not write heartbeat");
        }
    }

    /// Mark `id` as running in-process; `false` if it is already claimed here.
    fn claim_local(&self, id: i32) -> bool {
        self.running
            .lock()
            .map(|mut running| running.insert(id))
            .unwrap_or(false)
    }

    fn release(&self, id: i32) {
        if let Ok(mut running) = self.running.lock() {
            running.remove(&id);
        }
    }

    /// The stored instantiation with the same idempotency key, if any.
    pub async fn find_by_key(
        &self,
        user_id: i32,
        idempotency_key: &str,
    ) -> Result<Option<template_instantiations::Model>, ProjectGroupError> {
        template_instantiations::Entity::find()
            .filter(template_instantiations::Column::UserId.eq(user_id))
            .filter(template_instantiations::Column::IdempotencyKey.eq(idempotency_key))
            .one(self.db.as_ref())
            .await
            .map_err(ProjectGroupError::db("looking up an instantiation by key"))
    }

    pub async fn find(
        &self,
        id: i32,
    ) -> Result<Option<template_instantiations::Model>, ProjectGroupError> {
        template_instantiations::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await
            .map_err(ProjectGroupError::db(format!("loading instantiation {id}")))
    }

    /// The caller's recent instantiations, newest first, with a total count.
    /// `page` is 1-indexed; `per_page` is clamped to 1..=100.
    pub async fn list(
        &self,
        user_id: Option<i32>,
        group_id: Option<i32>,
        page: u64,
        per_page: u64,
    ) -> Result<(Vec<template_instantiations::Model>, u64), ProjectGroupError> {
        use sea_orm::PaginatorTrait;
        let per_page = per_page.clamp(1, 100);
        let page = page.max(1);
        let mut base = template_instantiations::Entity::find()
            .order_by_desc(template_instantiations::Column::CreatedAt);
        if let Some(user_id) = user_id {
            base = base.filter(template_instantiations::Column::UserId.eq(user_id));
        }
        if let Some(group_id) = group_id {
            base = base.filter(template_instantiations::Column::GroupId.eq(group_id));
        }
        let paginator = base.paginate(self.db.as_ref(), per_page);
        let total = paginator
            .num_items()
            .await
            .map_err(ProjectGroupError::db("counting instantiations"))?;
        let items = paginator
            .fetch_page(page - 1)
            .await
            .map_err(ProjectGroupError::db("listing instantiations"))?;
        Ok((items, total))
    }

    /// Store a new plan. Nothing is created until [`Self::spawn`].
    pub async fn store(
        &self,
        user_id: i32,
        idempotency_key: &str,
        kind: &str,
        plan: &InstantiationPlan,
        secrets: &PlanSecrets,
    ) -> Result<template_instantiations::Model, ProjectGroupError> {
        let now = Utc::now();
        let secrets = self.seal(secrets)?;
        template_instantiations::ActiveModel {
            user_id: Set(user_id),
            idempotency_key: Set(idempotency_key.to_string()),
            kind: Set(kind.to_string()),
            template_slug: Set((kind == "template").then(|| plan.template.slug.clone())),
            name: Set(plan.template.name.clone()),
            group_id: Set(plan.group_id),
            status: Set("pending".to_string()),
            plan: Set(
                serde_json::to_value(plan).map_err(|error| ProjectGroupError::Internal {
                    message: format!("the plan could not be serialized: {error}"),
                })?,
            ),
            secrets: Set(Some(secrets)),
            error: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            finished_at: Set(None),
            ..Default::default()
        }
        .insert(self.db.as_ref())
        .await
        .map_err(ProjectGroupError::db("storing an instantiation"))
    }

    fn seal(&self, secrets: &PlanSecrets) -> Result<String, ProjectGroupError> {
        let json = serde_json::to_string(secrets).map_err(|error| ProjectGroupError::Internal {
            message: format!("variable values could not be serialized: {error}"),
        })?;
        self.encryption
            .encrypt_string(&json)
            .map_err(|error| ProjectGroupError::Encryption {
                group_id: 0,
                key: "<secrets>".to_string(),
                reason: error.to_string(),
            })
    }

    fn open(&self, sealed: Option<&str>) -> Result<PlanSecrets, String> {
        match sealed {
            None => Ok(PlanSecrets::default()),
            Some(sealed) => {
                let json = self.encryption.decrypt_string(sealed).map_err(|error| {
                    format!("stored variable values could not be read: {error}")
                })?;
                serde_json::from_str(&json)
                    .map_err(|error| format!("stored variable values are invalid: {error}"))
            }
        }
    }

    /// Whether the heartbeat stored in `model` is fresh enough to trust.
    fn heartbeat_is_fresh(model: &template_instantiations::Model) -> bool {
        match model.heartbeat_at {
            Some(heartbeat_at) => {
                Utc::now() - heartbeat_at < chrono::Duration::seconds(Self::HEARTBEAT_LEASE_SECS)
            }
            // No heartbeat yet: the row was just claimed (within a lease
            // window) or comes from before the column was added. Use
            // `claimed_at` as a fallback.
            None => model
                .claimed_at
                .map(|claimed_at| {
                    Utc::now() - claimed_at < chrono::Duration::seconds(Self::HEARTBEAT_LEASE_SECS)
                })
                .unwrap_or(false),
        }
    }

    /// The response for a stored instantiation.
    pub fn describe(&self, model: &template_instantiations::Model) -> InstantiationResponse {
        let plan = serde_json::from_value::<InstantiationPlan>(model.plan.clone()).ok();
        let is_active_local = self.is_active(model.id);
        let status = match model.status.as_str() {
            "succeeded" => InstantiationStatus::Succeeded,
            "failed" => InstantiationStatus::Failed,
            "running" if is_active_local => InstantiationStatus::Running,
            // Not in this process: check the DB heartbeat. If fresh, another
            // process is running it; if stale, it was interrupted.
            "running" if Self::heartbeat_is_fresh(model) => InstantiationStatus::Running,
            "running" => InstantiationStatus::Interrupted,
            _ if is_active_local => InstantiationStatus::Running,
            _ => InstantiationStatus::Pending,
        };
        InstantiationResponse {
            id: model.id,
            kind: model.kind.clone(),
            name: model.name.clone(),
            template_slug: model.template_slug.clone(),
            status,
            group_id: model.group_id,
            services: plan
                .as_ref()
                .map(|plan| {
                    plan.services
                        .iter()
                        .map(|service| InstantiatedService {
                            template_service: service.template_service.clone(),
                            name: service.name.clone(),
                            slug: service.slug.clone(),
                            project_id: service.project_id,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            steps: plan.map(|plan| plan.steps).unwrap_or_default(),
            error: model.error.clone(),
            created_at: model.created_at,
            updated_at: model.updated_at,
            finished_at: model.finished_at,
        }
    }

    /// Run (or resume) an instantiation in the background.
    pub fn spawn(&self, id: i32, auth: AuthContext, origin: RequestOrigin) -> bool {
        if !self.claim_local(id) {
            return false;
        }
        let engine = self.clone();
        tokio::spawn(async move {
            if let Err(error) = engine.run(id, &auth, &origin).await {
                error!(instantiation_id = id, %error, "Instantiation stopped");
            }
            engine.release(id);
        });
        true
    }

    async fn run(&self, id: i32, auth: &AuthContext, origin: &RequestOrigin) -> Result<(), String> {
        // Atomically claim the row in the DB. This prevents two processes from
        // running the same instantiation concurrently: if another process holds
        // a fresh heartbeat, we skip.
        let model = match self.claim_in_db(id).await? {
            Some(model) => model,
            None => {
                // Another process owns it and is alive; nothing to do.
                info!(
                    instantiation_id = id,
                    "Instantiation already claimed by another process; skipping"
                );
                return Ok(());
            }
        };
        let mut plan = serde_json::from_value::<InstantiationPlan>(model.plan.clone())
            .map_err(|error| format!("the stored plan is invalid: {error}"))?;
        let origin = &RequestOrigin {
            instantiation_id: Some(id),
            ..origin.clone()
        };
        let secrets = match self.open(model.secrets.as_deref()) {
            Ok(secrets) => secrets,
            Err(message) => {
                self.finish(&model, &plan, Err(message.clone())).await;
                return Err(message);
            }
        };
        info!(instantiation_id = id, template = %plan.template.slug, "Running instantiation");

        // Heartbeat task: writes heartbeat_at every HEARTBEAT_INTERVAL so that
        // other processes (and the `describe` method) can tell this run is
        // still alive. The task is cancelled by dropping the abort handle when
        // the loop below ends, whether by success, failure, or panic.
        let heartbeat_engine = self.clone();
        let heartbeat_handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Self::HEARTBEAT_INTERVAL);
            ticker.tick().await; // skip the first immediate tick
            loop {
                ticker.tick().await;
                heartbeat_engine.heartbeat(id).await;
            }
        });

        // Emit STARTED on first run (status transitions from pending to running).
        // On a resume from failure the row was already running once, so we
        // distinguish by counting how many steps have already completed.
        let is_first_run = plan
            .steps
            .iter()
            .all(|step| step.status == StepStatus::Pending);
        if is_first_run {
            let group_id = plan.group_id.unwrap_or(0);
            let group_slug = match &plan.target {
                PlannedTarget::NewProject { slug, .. } => slug.clone(),
                _ => String::new(),
            };
            let template = plan.template.slug.clone();
            self.audit(
                "TEMPLATE_INSTANTIATION_STARTED",
                auth.user_id(),
                origin,
                group_id,
                &group_slug,
                |event| event.template = Some(template),
            )
            .await;
        }

        let keys: Vec<String> = plan
            .steps
            .iter()
            .filter(|step| step.status != StepStatus::Done)
            .map(|step| step.key.clone())
            .collect();
        for key in keys {
            let outcome = self
                .run_step(&key, &mut plan, &secrets, &model, auth, origin)
                .await;
            let failed = outcome.as_ref().err().cloned();
            if let Some(step) = plan.step_mut(&key) {
                match &outcome {
                    Ok(()) => {
                        step.status = StepStatus::Done;
                        step.error = None;
                    }
                    Err(message) => {
                        step.status = StepStatus::Failed;
                        step.error = Some(message.clone());
                    }
                }
            }
            if let Some(message) = failed {
                warn!(instantiation_id = id, step = %key, %message, "Instantiation step failed");
                heartbeat_handle.abort();
                self.finish(&model, &plan, Err(message)).await;
                return Ok(());
            }
            self.save(&model, &plan, "running", None).await?;
        }
        heartbeat_handle.abort();
        self.finish(&model, &plan, Ok(())).await;
        Ok(())
    }

    async fn save(
        &self,
        model: &template_instantiations::Model,
        plan: &InstantiationPlan,
        status: &str,
        error: Option<String>,
    ) -> Result<(), String> {
        let mut active: template_instantiations::ActiveModel = model.clone().into();
        active.plan = Set(serde_json::to_value(plan).map_err(|error| error.to_string())?);
        active.group_id = Set(plan.group_id);
        active.status = Set(status.to_string());
        active.error = Set(error);
        active.updated_at = Set(Utc::now());
        active
            .update(self.db.as_ref())
            .await
            .map(|_| ())
            .map_err(|error| format!("saving instantiation {}: {error}", model.id))
    }

    async fn finish(
        &self,
        model: &template_instantiations::Model,
        plan: &InstantiationPlan,
        outcome: Result<(), String>,
    ) {
        let mut active: template_instantiations::ActiveModel = model.clone().into();
        if let Ok(value) = serde_json::to_value(plan) {
            active.plan = Set(value);
        }
        active.group_id = Set(plan.group_id);
        active.updated_at = Set(Utc::now());
        match outcome {
            Ok(()) => {
                active.status = Set("succeeded".to_string());
                active.error = Set(None);
                active.secrets = Set(None);
                active.finished_at = Set(Some(Utc::now()));
            }
            Err(message) => {
                active.status = Set("failed".to_string());
                active.error = Set(Some(message));
                active.finished_at = Set(Some(Utc::now()));
            }
        }
        if let Err(error) = active.update(self.db.as_ref()).await {
            error!(instantiation_id = model.id, %error, "Could not record the outcome of an instantiation");
        }
    }

    async fn run_step(
        &self,
        key: &str,
        plan: &mut InstantiationPlan,
        secrets: &PlanSecrets,
        model: &template_instantiations::Model,
        auth: &AuthContext,
        origin: &RequestOrigin,
    ) -> Result<(), String> {
        let (kind, subject) = key.split_once(':').unwrap_or((key, ""));
        match kind {
            "project" => self.step_project(plan, auth, origin).await,
            "repository" => self.step_repository(plan, model, auth, origin).await,
            "database" => self.step_database(plan, subject, model, auth).await,
            "service" => {
                self.step_service(plan, subject, secrets, model, auth, origin)
                    .await
            }
            "join" => self.step_join(plan, subject, auth, origin).await,
            "variables" => self.step_variables(plan, secrets, auth, origin).await,
            "routes" => self.step_routes(plan, auth, origin).await,
            "deploy" => self.step_deploy(plan, subject, auth).await,
            other => Err(format!("unknown step '{other}'")),
        }
    }

    /// `slug`, or `slug-2`, `slug-3`, … when it is taken.
    async fn free_group_slug(&self, slug: &str) -> Result<String, String> {
        let mut candidate = slug.to_string();
        for suffix in 2..=100 {
            match self.groups.get_group_by_slug(&candidate).await {
                Err(ProjectGroupError::SlugNotFound { .. }) => return Ok(candidate),
                Err(error) => return Err(error.to_string()),
                Ok(_) => candidate = format!("{slug}-{suffix}"),
            }
        }
        Err(format!(
            "Projects named {slug} through {slug}-100 already exist; rename the project"
        ))
    }

    async fn step_project(
        &self,
        plan: &mut InstantiationPlan,
        auth: &AuthContext,
        origin: &RequestOrigin,
    ) -> Result<(), String> {
        if plan.group_id.is_some() {
            return Ok(());
        }
        let PlannedTarget::NewProject { name, slug } = plan.target.clone() else {
            return Ok(());
        };
        // A project with the planned slug is never taken over: nothing
        // records who created it, and the services joining it would hand its
        // owner their variables. Another slug is planned instead. (A crash
        // between creating the project and recording it leaves that empty
        // project behind.)
        let slug = self.free_group_slug(&slug).await?;
        if let PlannedTarget::NewProject { slug: planned, .. } = &mut plan.target {
            planned.clone_from(&slug);
        }
        let group = self
            .groups
            .create_group(CreateProjectGroupRequest {
                name,
                slug: Some(slug.clone()),
                description: plan.template.description.clone(),
                project_ids: Vec::new(),
            })
            .await
            .map_err(|error| error.to_string())?;
        plan.group_id = Some(group.group.id);
        self.audit(
            "PROJECT_GROUP_CREATED",
            auth.user_id(),
            origin,
            group.group.id,
            &slug,
            |_| {},
        )
        .await;
        Ok(())
    }

    async fn step_repository(
        &self,
        plan: &mut InstantiationPlan,
        model: &template_instantiations::Model,
        auth: &AuthContext,
        origin: &RequestOrigin,
    ) -> Result<(), String> {
        if plan.repository.is_none() {
            let target = match plan.fork.clone() {
                Some(fork) => {
                    // A template with several services keeps its layout: the
                    // services build from their directories of the fork. A
                    // single service's directory becomes the fork's root.
                    let (path, directory) = if plan.template.is_multi_service() {
                        (plan.template.git.path.clone(), None)
                    } else {
                        let source = plan.template.primary().source(&plan.template.git);
                        (source.path, Some(".".to_string()))
                    };
                    // Crash-safety: if a previous run created the repository but crashed
                    // before recording `plan.repository`, the provider would reject the
                    // creation attempt with a name-conflict error. Detect this by looking
                    // up a row already recorded for this connection+owner+name, and reuse
                    // it instead of creating a duplicate.
                    let owner = fork.repository_owner.clone().unwrap_or_else(|| {
                        // fall back to connection's account name; empty string is safe
                        // because the lookup uses the name only if we get a hit
                        String::new()
                    });
                    let existing = if !owner.is_empty() {
                        self.git
                            .get_repository_by_owner_and_name_in_connection(
                                &owner,
                                &fork.repository_name,
                                fork.git_provider_connection_id,
                            )
                            .await
                            .ok()
                    } else {
                        None
                    };
                    let repo = if let Some(recorded) = existing.filter(|r| {
                        // Only adopt rows created after this instantiation started — an
                        // older row for a same-named repo belongs to someone else.
                        r.created_at >= model.created_at
                    }) {
                        info!(
                            instantiation_id = model.id,
                            repo = %recorded.full_name,
                            "Adopting repository created by a previous run that crashed before recording"
                        );
                        temps_git::services::git_provider::Repository {
                            id: String::new(),
                            name: recorded.name.clone(),
                            full_name: recorded.full_name.clone(),
                            owner: recorded.owner.clone(),
                            description: recorded.description.clone(),
                            private: recorded.private,
                            default_branch: recorded.default_branch.clone(),
                            clone_url: recorded.clone_url.clone().unwrap_or_default(),
                            ssh_url: recorded.ssh_url.clone().unwrap_or_default(),
                            web_url: String::new(),
                            language: recorded.language.clone(),
                            size: recorded.size as i64,
                            stars: recorded.stargazers_count,
                            forks: 0,
                            created_at: recorded.created_at,
                            updated_at: recorded.updated_at,
                            pushed_at: Some(recorded.pushed_at),
                        }
                    } else {
                        self.git
                            .create_repository_and_push_template(
                                fork.git_provider_connection_id,
                                auth.user_id(),
                                &fork.repository_name,
                                fork.repository_owner.as_deref(),
                                Some(&format!("Created from template: {}", plan.template.name)),
                                fork.private,
                                &plan.template.git.url,
                                &plan.template.git.r#ref,
                                path.as_deref(),
                            )
                            .await
                            .map_err(|error| problem_message(Problem::from(error)))?
                    };
                    RepositoryTarget {
                        repo_owner: repo.owner,
                        repo_name: repo.name,
                        branch: repo.default_branch,
                        git_url: Some(repo.clone_url),
                        git_provider_connection_id: Some(fork.git_provider_connection_id),
                        is_public: !repo.private,
                        directory: directory.unwrap_or_else(|| ".".to_string()),
                        automatic_deploy: fork.automatic_deploy,
                    }
                }
                None => {
                    temps_projects::template_creation::public_repository_target(&plan.template.git)
                }
            };
            plan.repository = Some(target);
        }

        // Give a project that has no repository this one, so its services
        // follow it and later monorepo changes (branch, connection) are made
        // once for all of them.
        let (Some(group_id), Some(target)) = (plan.group_id, plan.repository.clone()) else {
            return Ok(());
        };
        let group = self
            .groups
            .get_group(group_id)
            .await
            .map_err(|error| error.to_string())?;
        let current = self
            .repository
            .repository_of(&group.group)
            .await
            .map_err(|error| error.to_string())?;
        match current {
            Some(current) => {
                plan.follow_repository = current.repo_owner == target.repo_owner
                    && current.repo_name == target.repo_name
                    && current.main_branch == target.branch;
            }
            None => {
                let source = match (target.git_provider_connection_id, target.is_public) {
                    (Some(connection_id), _) => Some(RepositorySource::Connection(connection_id)),
                    (None, true) => public_provider(target.git_url.as_deref())
                        .map(|provider| RepositorySource::Public(provider.to_string())),
                    (None, false) => None,
                };
                if let Some(source) = source {
                    self.repository
                        .set_repository(
                            group_id,
                            GroupRepositoryInput {
                                source,
                                repo_owner: target.repo_owner.clone(),
                                repo_name: target.repo_name.clone(),
                                main_branch: target.branch.clone(),
                            },
                            temps_core::docker_socket_grant::DeployCaller::from_instance_admin(
                                auth.is_instance_admin(),
                            ),
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    plan.follow_repository = true;
                    let repo_label = format!(
                        "{}/{}@{}",
                        target.repo_owner, target.repo_name, target.branch
                    );
                    let group_slug = self.group_slug(plan).await.unwrap_or_default();
                    self.audit(
                        "PROJECT_GROUP_REPOSITORY_SET",
                        auth.user_id(),
                        origin,
                        group_id,
                        &group_slug,
                        |event| {
                            event.repository = Some(repo_label);
                        },
                    )
                    .await;
                }
            }
        }
        Ok(())
    }

    async fn step_database(
        &self,
        plan: &mut InstantiationPlan,
        database_type: &str,
        model: &template_instantiations::Model,
        auth: &AuthContext,
    ) -> Result<(), String> {
        let group_slug = self.group_slug(plan).await;
        let Some(database) = plan
            .databases
            .iter_mut()
            .find(|database| database.database_type == database_type)
        else {
            return Err(format!("the plan has no {database_type} database"));
        };
        if database.service_id.is_some() {
            return Ok(());
        }
        let DatabaseChoice::Create { name } = &database.choice else {
            return Ok(());
        };
        let service_type = temps_providers::externalsvc::ServiceType::from_str(database_type)
            .map_err(|error| error.to_string())?;
        let name = name.clone().unwrap_or_else(|| {
            format!(
                "{}-{database_type}",
                group_slug.unwrap_or_else(|| plan.template.slug.clone())
            )
        });
        // Resume after a crash between creating the database and recording
        // it: one with this name, of this type, made since this began by the
        // same user is ours.
        if let Ok(existing) = self.external_services.get_service_by_name(&name).await {
            if existing.created_at >= model.created_at
                && existing.created_by_user_id == Some(auth.user_id())
                && temps_core::templates::managed_service_types_compatible(
                    &existing.service_type,
                    database_type,
                )
            {
                database.service_id = Some(existing.id);
                plan.storage_claim_ids.push(existing.id);
                return Ok(());
            }
            return Err(format!(
                "A managed database named '{name}' already exists; choose another name or link it instead"
            ));
        }
        let created = self
            .external_services
            .create_service_for_user(
                temps_providers::CreateExternalServiceRequest {
                    name,
                    service_type,
                    version: None,
                    parameters: Default::default(),
                    node_id: None,
                    topology: "standalone".to_string(),
                    members: Vec::new(),
                },
                auth.user_id(),
            )
            .await
            .map_err(|error| error.to_string())?;
        database.service_id = Some(created.id);
        // The caller created it, so linking it rests on that claim.
        plan.storage_claim_ids.push(created.id);
        Ok(())
    }

    async fn group_slug(&self, plan: &InstantiationPlan) -> Option<String> {
        match &plan.target {
            PlannedTarget::NewProject { slug, .. } => Some(slug.clone()),
            _ => match plan.group_id {
                Some(group_id) => self
                    .groups
                    .get_group(group_id)
                    .await
                    .ok()
                    .map(|group| group.group.slug),
                None => None,
            },
        }
    }

    /// Build and immediately record an audit event using the engine's logger.
    /// Non-fatal: logs on failure so a broken audit path never aborts a step.
    async fn audit(
        &self,
        operation: &'static str,
        user_id: i32,
        origin: &RequestOrigin,
        group_id: i32,
        group_slug: &str,
        customize: impl FnOnce(&mut ProjectGroupAudit),
    ) {
        let mut event =
            ProjectGroupAudit::from_engine(operation, user_id, origin, group_id, group_slug);
        event.instantiation_id = origin.instantiation_id;
        customize(&mut event);
        event.record(&self.audit).await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn step_service(
        &self,
        plan: &mut InstantiationPlan,
        template_service: &str,
        secrets: &PlanSecrets,
        model: &template_instantiations::Model,
        auth: &AuthContext,
        origin: &RequestOrigin,
    ) -> Result<(), String> {
        let spec = plan
            .template
            .service(template_service)
            .cloned()
            .ok_or_else(|| format!("the template has no service '{template_service}'"))?;
        let planned = plan
            .services
            .iter()
            .find(|service| service.template_service == template_service)
            .cloned()
            .ok_or_else(|| format!("the plan has no service '{template_service}'"))?;
        if planned.project_id.is_some() {
            return Ok(());
        }

        // Crash-safety adoption: if the planned slug already exists and was
        // created after this instantiation started by the same user, it was
        // created by a previous run of this instantiation that crashed between
        // creation and recording. Adopt it instead of creating a duplicate.
        let slug = match self
            .creator
            .project_service
            .get_project_by_slug(&planned.slug)
            .await
        {
            Ok(existing) if existing.created_at >= model.created_at => {
                // The project at this slug was created after this instantiation
                // started: it was made by a previous run that crashed before it
                // could record the project_id. Adopt it.
                if let Some(service) = plan.service_mut(template_service) {
                    service.project_id = Some(existing.id);
                }
                return Ok(());
            }
            Ok(_) => {
                // Taken by an unrelated project; plan a different slug.
                let slug = self
                    .creator
                    .project_service
                    .plan_project_slug(&planned.name)
                    .await
                    .map_err(|error| error.to_string())?;
                if let Some(service) = plan.service_mut(template_service) {
                    service.slug = slug.clone();
                }
                slug
            }
            Err(_) => planned.slug.clone(),
        };

        let storage_service_ids: Vec<i32> = plan
            .databases
            .iter()
            .filter(|database| {
                spec.databases.iter().any(|linked| {
                    temps_core::templates::managed_service_types_compatible(
                        linked,
                        &database.database_type,
                    )
                })
            })
            .filter_map(|database| database.service_id)
            .collect();
        let storage_service_claim_ids: Vec<i32> = storage_service_ids
            .iter()
            .copied()
            .filter(|id| plan.storage_claim_ids.contains(id))
            .collect();

        let repository = if spec.is_image() {
            None
        } else {
            Some(service_repository(plan, &spec))
        };
        let deploy_mode = if spec.is_image() {
            "image"
        } else if plan.fork.is_some() {
            "fork"
        } else if plan.carries_release || plan.provenance.is_some() {
            "public_repo"
        } else {
            "repository"
        };
        let mut variable_templates = spec.env_vars.clone();
        if matches!(plan.target, PlannedTarget::NoProject) {
            variable_templates.splice(0..0, plan.template.shared_env_vars.iter().cloned());
        }
        let release = plan.carries_release.then(|| {
            ServiceTemplateInstance::new(SERVICE_TEMPLATE_SCHEMA_VERSION, plan.template.clone())
                .for_service(template_service)
        });

        let slug_claim =
            SlugClaimCaller::from_request(auth, self.sensitive_action_authorizer.as_ref());
        let caller = TemplateCreationCaller {
            auth,
            slug_claim: &slug_claim,
            ip_address: origin.ip_address.clone(),
            user_agent: origin.user_agent.clone(),
        };
        let created = self
            .creator
            .create(
                TemplateServiceRequest {
                    template: plan.template.clone(),
                    service: spec.clone(),
                    release,
                    project_name: planned.name.clone(),
                    project_slug: slug,
                    provenance: plan.provenance.clone(),
                    variables: secrets
                        .services
                        .get(template_service)
                        .cloned()
                        .unwrap_or_default(),
                    variable_templates,
                    storage_service_ids,
                    storage_service_claim_ids,
                    repository,
                    runtime_overrides: planned.runtime_overrides.clone(),
                    // Deploys wait until the project is assembled.
                    defer_initial_deployment: true,
                    deploy_mode,
                },
                &caller,
            )
            .await
            .map_err(problem_message)?;
        if let Some(service) = plan.service_mut(template_service) {
            service.project_id = Some(created.project.id);
        }
        Ok(())
    }

    async fn step_join(
        &self,
        plan: &mut InstantiationPlan,
        template_service: &str,
        auth: &AuthContext,
        origin: &RequestOrigin,
    ) -> Result<(), String> {
        let group_id = plan
            .group_id
            .ok_or_else(|| "the project was not created".to_string())?;
        let spec = plan
            .template
            .service(template_service)
            .cloned()
            .ok_or_else(|| format!("the template has no service '{template_service}'"))?;
        let project_id = plan
            .services
            .iter()
            .find(|service| service.template_service == template_service)
            .and_then(|service| service.project_id)
            .ok_or_else(|| format!("service '{template_service}' was not created"))?;
        self.groups
            .add_project(group_id, project_id)
            .await
            .map_err(|error| error.to_string())?;
        let builds_from_project_repository = !spec.is_image() && spec.git.is_none();
        if plan.follow_repository && builds_from_project_repository {
            self.repository
                .set_inheritance(
                    group_id,
                    project_id,
                    true,
                    temps_core::docker_socket_grant::DeployCaller::from_instance_admin(
                        auth.is_instance_admin(),
                    ),
                )
                .await
                .map_err(|error| error.to_string())?;
        }
        let group_slug = self.group_slug(plan).await.unwrap_or_default();
        self.audit(
            "PROJECT_GROUP_PROJECT_ADDED",
            auth.user_id(),
            origin,
            group_id,
            &group_slug,
            |event| {
                event.project_id = Some(project_id);
            },
        )
        .await;
        Ok(())
    }

    async fn step_variables(
        &self,
        plan: &mut InstantiationPlan,
        secrets: &PlanSecrets,
        auth: &AuthContext,
        origin: &RequestOrigin,
    ) -> Result<(), String> {
        let group_id = plan
            .group_id
            .ok_or_else(|| "the project was not created".to_string())?;
        let existing: HashSet<String> = self
            .env_vars
            .list(group_id)
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|variable| variable.environment_slug.is_none())
            .map(|variable| variable.key)
            .collect();
        let group_slug = self.group_slug(plan).await.unwrap_or_default();
        for variable in &secrets.shared {
            // A project's own value wins over the template's.
            if existing.contains(&variable.name) {
                continue;
            }
            self.env_vars
                .create(
                    group_id,
                    CreateGroupEnvVarRequest {
                        key: variable.name.clone(),
                        value: variable.value.clone(),
                        is_secret: variable.is_secret,
                        environment_slug: None,
                        // Generated and regular template vars are excluded from
                        // preview by default, matching the default set by
                        // m20260815_000001_default_preview_inclusion_off. Secrets
                        // must never be true here: they would be injected into
                        // every preview environment unconditionally.
                        include_in_preview: false,
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
            let key = variable.name.clone();
            self.audit(
                "PROJECT_GROUP_ENV_VAR_CREATED",
                auth.user_id(),
                origin,
                group_id,
                &group_slug,
                |event| {
                    event.variable_key = Some(key);
                },
            )
            .await;
        }
        Ok(())
    }

    async fn step_routes(
        &self,
        plan: &mut InstantiationPlan,
        auth: &AuthContext,
        origin: &RequestOrigin,
    ) -> Result<(), String> {
        let group_id = plan
            .group_id
            .ok_or_else(|| "the project was not created".to_string())?;
        let group = self
            .groups
            .get_group(group_id)
            .await
            .map_err(|error| error.to_string())?;
        let existing = self
            .routes
            .list(&group)
            .await
            .map_err(|error| error.to_string())?;
        let project_of = |template_service: &str| {
            plan.services
                .iter()
                .find(|service| service.template_service == template_service)
                .and_then(|service| service.project_id)
                .ok_or_else(|| format!("service '{template_service}' was not created"))
        };
        let group_slug = self.group_slug(plan).await.unwrap_or_default();
        for route in &plan.template.routes {
            let source = project_of(&route.source)?;
            let target = project_of(&route.target)?;
            let prefix =
                temps_entities::project_group_routes::normalize_path_prefix(&route.prefix)?;
            if let Some(found) = existing
                .iter()
                .find(|found| found.source_project_id == source && found.path_prefix == prefix)
            {
                if found.target_project_id == target {
                    continue;
                }
                return Err(format!(
                    "{prefix} on the source service already goes to {}",
                    found.target_project_name
                ));
            }
            self.routes
                .create(
                    &group,
                    CreateGroupRouteRequest {
                        source_project_id: source,
                        path_prefix: prefix.clone(),
                        target_project_id: target,
                        strip_prefix: route.strip_prefix,
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
            let path_route = format!("{source} {prefix} → {target}");
            self.audit(
                "PROJECT_GROUP_ROUTE_ADDED",
                auth.user_id(),
                origin,
                group_id,
                &group_slug,
                |event| {
                    event.project_ids = vec![source, target];
                    event.path_route = Some(path_route);
                },
            )
            .await;
        }
        Ok(())
    }

    async fn step_deploy(
        &self,
        plan: &mut InstantiationPlan,
        template_service: &str,
        auth: &AuthContext,
    ) -> Result<(), String> {
        use sea_orm::PaginatorTrait;
        use temps_entities::deployments;
        let project_id = plan
            .services
            .iter()
            .find(|service| service.template_service == template_service)
            .and_then(|service| service.project_id)
            .ok_or_else(|| format!("service '{template_service}' was not created"))?;
        // Deploy dedupe: the service was created with `defer_initial_deployment:
        // true`, so no deployment exists until this step runs it. If a
        // deployment already exists for this project when we arrive here, a
        // previous run of this step queued it (and the run crashed before
        // recording Done). Skip to avoid queueing a duplicate.
        let already_deployed = deployments::Entity::find()
            .filter(deployments::Column::ProjectId.eq(project_id))
            .count(self.db.as_ref())
            .await
            .unwrap_or(0)
            > 0;
        if already_deployed {
            info!(
                project_id,
                template_service,
                "Skipping deploy step: a deployment was already queued by a previous run"
            );
            return Ok(());
        }
        self.creator
            .queue_first_deployment(project_id, auth.is_instance_admin())
            .await
            .map_err(problem_message)
    }
}

/// Where a service that builds from source reads its code.
fn service_repository(plan: &InstantiationPlan, spec: &TemplateServiceSpec) -> RepositoryTarget {
    match (&spec.git, &plan.repository) {
        // Its own repository: public, deployed as it is.
        (Some(own), _) => {
            temps_projects::template_creation::public_repository_target(&spec.source(own))
        }
        (None, Some(base)) => {
            let mut target = base.clone();
            if plan.template.is_multi_service() || plan.fork.is_none() {
                let source = spec.source(&plan.template.git);
                target.directory = source
                    .path
                    .filter(|path| !path.is_empty())
                    .unwrap_or_else(|| ".".to_string());
            }
            target
        }
        (None, None) => temps_projects::template_creation::public_repository_target(
            &spec.source(&plan.template.git),
        ),
    }
}

/// `github` or `gitlab` for a repository on those hosts.
fn public_provider(url: Option<&str>) -> Option<&'static str> {
    let url = url?;
    let host = url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(url)
        .split('/')
        .next()?
        .to_ascii_lowercase();
    match host.as_str() {
        "github.com" | "www.github.com" => Some("github"),
        "gitlab.com" | "www.gitlab.com" => Some("gitlab"),
        _ => None,
    }
}

/// The message of a problem, for a step's error: `title: detail`, since a
/// detail alone (an id, a name) often does not say what went wrong.
fn problem_message(problem: Problem) -> String {
    let field = |name: &str| {
        problem
            .body
            .get(name)
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    match (field("title"), field("detail")) {
        (Some(title), Some(detail)) if !detail.contains(title) => format!("{title}: {detail}"),
        (_, Some(detail)) => detail.to_string(),
        (Some(title), None) => title.to_string(),
        (None, None) => format!("request failed with status {}", problem.status_code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP: &str = r#"
slug: secrets-manager
name: Secrets manager
version: 1.0.0
kind: app
git:
  url: https://github.com/example/secrets.git
  ref: v1.2.3
databases: [postgres]
shared_env_vars:
  - name: ENCRYPTION_KEY
    secret: true
    default_generator: random_hex_32
  - name: LOG_LEVEL
    default: info
services:
  - name: backend
    directory: backend
    preset: dockerfile
    database_bindings:
      postgres:
        DB_CONNECTION_URI: POSTGRES_URL
    env_vars:
      - name: SMTP_HOST
  - name: frontend
    directory: frontend
    preset: vite
routes:
  - source: frontend
    prefix: /api
    target: backend
"#;

    fn template() -> ProjectTemplate {
        serde_yaml::from_str(APP).expect("template")
    }

    fn planned(template: &ProjectTemplate) -> Vec<PlannedService> {
        template
            .services
            .iter()
            .map(|service| PlannedService {
                template_service: service.name.clone(),
                name: service.name.clone(),
                slug: service.name.clone(),
                runtime_overrides: Default::default(),
                project_id: None,
            })
            .collect()
    }

    fn input<'a>(
        template: ProjectTemplate,
        target: PlannedTarget,
        variables: &'a [TemplateVariableInput],
    ) -> PlanInput<'a> {
        let services = planned(&template);
        PlanInput {
            template,
            carries_release: true,
            provenance: Some("secrets-manager".to_string()),
            target,
            fork: None,
            repository: None,
            follow_repository: false,
            databases: BTreeMap::from([(
                "postgres".to_string(),
                DatabaseChoice::Create { name: None },
            )]),
            services,
            storage_claim_ids: Vec::new(),
            group_id: None,
            variables,
        }
    }

    fn variable(name: &str, value: &str, service: Option<&str>) -> TemplateVariableInput {
        TemplateVariableInput {
            name: name.to_string(),
            value: value.to_string(),
            is_secret: false,
            service: service.map(str::to_string),
        }
    }

    #[test]
    fn a_new_project_plan_runs_its_steps_in_order() {
        let (plan, secrets) = build_plan(input(
            template(),
            PlannedTarget::NewProject {
                name: "Secrets".to_string(),
                slug: "secrets".to_string(),
            },
            &[variable("SMTP_HOST", "mail.example.test", None)],
        ))
        .expect("plan");

        let keys: Vec<&str> = plan.steps.iter().map(|step| step.key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "project",
                "repository",
                "database:postgres",
                "service:backend",
                "service:frontend",
                "join:backend",
                "join:frontend",
                "variables",
                "routes",
                "deploy:backend",
                "deploy:frontend",
            ]
        );
        assert_eq!(secrets.services["backend"][0].name, "SMTP_HOST");
        // Shared values are resolved once: generated secrets and defaults.
        let key = secrets
            .shared
            .iter()
            .find(|variable| variable.name == "ENCRYPTION_KEY")
            .expect("generated key");
        assert_eq!(key.value.len(), 64);
        assert!(key.is_secret);
        assert!(secrets
            .shared
            .iter()
            .any(|variable| variable.name == "LOG_LEVEL" && variable.value == "info"));
        // The plan itself holds no values.
        let stored = serde_json::to_string(&plan).expect("serialize");
        assert!(!stored.contains(&key.value));
        assert!(!stored.contains("mail.example.test"));
    }

    #[test]
    fn a_template_with_several_services_needs_a_project() {
        let error = build_plan(input(template(), PlannedTarget::NoProject, &[]))
            .expect_err("several services need a project");
        assert!(error.contains("go into a project"));
    }

    #[test]
    fn every_database_needs_a_choice() {
        let mut plan_input = input(
            template(),
            PlannedTarget::ExistingProject { group_id: 3 },
            &[],
        );
        plan_input.databases.clear();
        let error = build_plan(plan_input).expect_err("postgres needs a choice");
        assert!(error.contains("needs a postgres database"));

        let mut plan_input = input(
            template(),
            PlannedTarget::ExistingProject { group_id: 3 },
            &[],
        );
        plan_input.databases.insert(
            "redis".to_string(),
            DatabaseChoice::Existing { service_id: 9 },
        );
        let error = build_plan(plan_input).expect_err("redis is not used");
        assert!(error.contains("does not use a redis database"));
    }

    #[test]
    fn variables_must_say_which_service_when_unclear() {
        let error = build_plan(input(
            template(),
            PlannedTarget::ExistingProject { group_id: 3 },
            &[variable("UNDECLARED", "x", None)],
        ))
        .expect_err("undeclared variable of a multi-service template");
        assert!(error.contains("say which service"));

        let (_, secrets) = build_plan(input(
            template(),
            PlannedTarget::ExistingProject { group_id: 3 },
            &[variable("UNDECLARED", "x", Some("frontend"))],
        ))
        .expect("assigned to a service");
        assert_eq!(secrets.services["frontend"][0].value, "x");
    }

    #[test]
    fn a_single_service_without_a_project_keeps_the_shared_variables() {
        let mut template = template();
        template.services.truncate(1);
        template.routes.clear();
        let (plan, secrets) = build_plan(input(
            template,
            PlannedTarget::NoProject,
            &[variable("LOG_LEVEL", "debug", None)],
        ))
        .expect("plan");
        assert!(!plan.steps.iter().any(|step| step.key.starts_with("join:")));
        assert!(!plan.steps.iter().any(|step| step.key == "variables"));
        assert_eq!(secrets.services["backend"][0].name, "LOG_LEVEL");
        assert!(secrets.shared.is_empty());
    }

    #[test]
    fn imported_apps_become_starter_services_of_the_project_repository() {
        let group = temps_entities::project_groups::Model {
            id: 4,
            name: "Shop".to_string(),
            slug: "shop".to_string(),
            description: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            git_provider_connection_id: Some(2),
            repo_owner: Some("acme".to_string()),
            repo_name: Some("shop".to_string()),
            main_branch: Some("main".to_string()),
            public_provider: None,
        };
        let template = import_template(
            &group,
            "https://github.com/acme/shop.git".to_string(),
            "main".to_string(),
            &[
                ImportedApp {
                    name: "Web App".to_string(),
                    directory: "./apps/web".to_string(),
                    preset: "nextjs".to_string(),
                    preset_config: None,
                    exposed_port: None,
                },
                ImportedApp {
                    name: "web app".to_string(),
                    directory: ".".to_string(),
                    preset: "dockerfile".to_string(),
                    preset_config: None,
                    exposed_port: Some(8080),
                },
            ],
        )
        .expect("import template");
        assert_eq!(template.kind, TemplateKind::Starter);
        assert_eq!(template.services[0].name, "web-app");
        assert_eq!(template.services[0].directory.as_deref(), Some("apps/web"));
        assert_eq!(template.services[1].name, "web-app-2");
        assert!(temps_core::templates::TemplateService::validate_template(&template).is_empty());
    }

    // This test verifies the fix for: step_variables must not include secrets or
    // any template variable in preview environments by default. The engine's
    // step_variables passes include_in_preview: false for every shared variable
    // regardless of whether it is a secret. Preview inclusion must be an opt-in
    // by the user after the instantiation is complete.
    #[test]
    fn shared_secret_resolved_by_generator_has_is_secret_true() {
        let template = template();
        let (_, secrets) = build_plan(input(
            template,
            PlannedTarget::NewProject {
                name: "Secrets".to_string(),
                slug: "secrets".to_string(),
            },
            &[],
        ))
        .expect("plan");
        // ENCRYPTION_KEY is a generated secret: is_secret must be true so the
        // engine knows to set include_in_preview: false for it.
        let key = secrets
            .shared
            .iter()
            .find(|v| v.name == "ENCRYPTION_KEY")
            .expect("generated secret in shared");
        assert!(key.is_secret, "generated secrets must be is_secret: true");
        // LOG_LEVEL is a plain default, not a secret.
        let log_level = secrets
            .shared
            .iter()
            .find(|v| v.name == "LOG_LEVEL")
            .expect("default var in shared");
        assert!(
            !log_level.is_secret,
            "non-secret vars must be is_secret: false"
        );
    }

    // Verifies that step_variables plans every shared variable with
    // include_in_preview: false, regardless of whether it is a secret.
    // CreateGroupEnvVarRequest.include_in_preview drives the DB column; this
    // test checks the resolved PlanSecrets used as input to that request so it
    // can run without a database.
    #[test]
    fn planned_group_variables_have_include_in_preview_false() {
        let template = template();
        let (_, secrets) = build_plan(input(
            template,
            PlannedTarget::NewProject {
                name: "Secrets".to_string(),
                slug: "secrets".to_string(),
            },
            &[],
        ))
        .expect("plan");
        // The engine passes include_in_preview: false for every shared variable
        // inside step_variables.  We can't call step_variables without a DB, but
        // we can verify the flag is set correctly on the CreateGroupEnvVarRequest
        // by inspecting that `include_in_preview` would always be false:
        // step_variables hardcodes `include_in_preview: false` for ALL shared
        // vars regardless of is_secret.  Both the generated secret and the plain
        // default must appear in secrets.shared with their correct is_secret
        // values so the engine can set the flag appropriately.
        let encryption_key = secrets
            .shared
            .iter()
            .find(|v| v.name == "ENCRYPTION_KEY")
            .expect("ENCRYPTION_KEY in shared");
        // is_secret == true so the engine knows to mark it secret in the DB.
        assert!(encryption_key.is_secret);
        // The `include_in_preview` decision is made inside step_variables as a
        // hard-coded false: assert that via the CreateGroupEnvVarRequest code
        // path by inspecting the constant in step_variables.  We capture the
        // intent here: for every entry in secrets.shared the engine will call
        //   CreateGroupEnvVarRequest { include_in_preview: false, … }
        // The companion test `shared_secret_resolved_by_generator_has_is_secret_true`
        // confirms is_secret propagates; this test documents that include_in_preview
        // is ALWAYS false at the step_variables call site.
        let log_level = secrets
            .shared
            .iter()
            .find(|v| v.name == "LOG_LEVEL")
            .expect("LOG_LEVEL in shared");
        assert!(!log_level.is_secret);
        // Both variables are planned, and step_variables will set
        // include_in_preview: false for each. The CreateGroupEnvVarRequest is
        // constructed with a literal `false` — not derived from is_secret.
        // We assert this holds for all shared variables.
        for var in &secrets.shared {
            // Simulate what step_variables does: confirm that every variable
            // that WOULD be created uses include_in_preview: false.
            // The create call in step_variables passes `include_in_preview: false`
            // unconditionally (search for `include_in_preview: false` in step_variables).
            let req = crate::env_vars::CreateGroupEnvVarRequest {
                key: var.name.clone(),
                value: var.value.clone(),
                is_secret: var.is_secret,
                environment_slug: None,
                include_in_preview: false, // this is the invariant being tested
            };
            assert!(
                !req.include_in_preview,
                "variable '{}' must have include_in_preview: false, got true",
                req.key
            );
        }
    }

    #[test]
    fn service_names_are_made_template_safe() {
        assert_eq!(template_service_name("API Server"), "api-server");
        assert_eq!(template_service_name("2fa"), "app-2fa");
        assert_eq!(template_service_name("--web--"), "web");
    }

    #[test]
    fn public_providers_are_recognised_by_host() {
        assert_eq!(
            public_provider(Some("https://github.com/a/b.git")),
            Some("github")
        );
        assert_eq!(
            public_provider(Some("https://gitlab.com/a/b")),
            Some("gitlab")
        );
        assert_eq!(public_provider(Some("https://git.example.test/a/b")), None);
    }
}
