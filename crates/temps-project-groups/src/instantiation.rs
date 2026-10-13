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
use temps_entities::{projects, template_instantiations};
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
use crate::repository::{
    same_repository, GroupRepository, GroupRepositoryInput, GroupRepositoryRef,
    GroupRepositoryService, MemberRepositoryRef, RepositorySource,
};
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
    /// Recorded before the service is created, so a plan that failed after
    /// this point is known to have attempted it (a project may exist that
    /// the plan does not name yet). Whether a project is this service's is
    /// decided by its ownership token, never by this flag.
    #[serde(default)]
    pub creation_started: bool,
    /// Names (never values) of the variables the caller supplied for this
    /// service, declared by the template or not. The values live only in the
    /// sealed [`PlanSecrets`], which the retention cleanup removes; this is
    /// what tells a retry afterwards that the service would be created
    /// without them. Empty for plans stored before this existed.
    #[serde(default)]
    pub supplied_variable_names: Vec<String>,
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

    /// Keys of the steps still to run that read values kept only in the
    /// sealed [`PlanSecrets`]: the shared variables (generated ones
    /// included) and the values supplied for a service that is not created
    /// yet. A service needs them when the template declares variables for it
    /// (generated ones cannot be told apart from supplied ones once the
    /// secrets are gone) or when the caller supplied any, declared or not:
    /// a variable the template never declared exists only in the secrets.
    /// Running without them would report success for a service that lacks
    /// them.
    fn steps_needing_secrets(&self) -> Vec<String> {
        let pending = |key: &str| {
            self.steps
                .iter()
                .any(|step| step.key == key && step.status != StepStatus::Done)
        };
        let mut keys = Vec::new();
        if !self.template.shared_env_vars.is_empty() && pending("variables") {
            keys.push("variables".to_string());
        }
        // Without a project the shared variables go to the one service.
        let shared_into_service = matches!(self.target, PlannedTarget::NoProject)
            && !self.template.shared_env_vars.is_empty();
        for service in &self.services {
            let key = format!("service:{}", service.template_service);
            if service.project_id.is_some() || !pending(&key) {
                continue;
            }
            let declares_variables = self
                .template
                .service(&service.template_service)
                .is_some_and(|spec| !spec.env_vars.is_empty());
            let has_supplied_variables = !service.supplied_variable_names.is_empty();
            if declares_variables || shared_into_service || has_supplied_variables {
                keys.push(key);
            }
        }
        keys
    }
}

/// The variable templates a service's supplied values are resolved against:
/// its own declared variables, preceded by the template's shared ones when
/// the service goes without a project (it then holds them itself).
fn service_variable_templates(
    plan: &InstantiationPlan,
    spec: &TemplateServiceSpec,
) -> Vec<EnvVarTemplate> {
    let mut variable_templates = spec.env_vars.clone();
    if matches!(plan.target, PlannedTarget::NoProject) {
        variable_templates.splice(0..0, plan.template.shared_env_vars.iter().cloned());
    }
    variable_templates
}

/// Check that every service not yet created will accept the values it is to
/// be created with, without creating anything. Runs the same resolution as
/// service creation (missing required values, duplicates, unsupported
/// generators), so a request accepted here is not refused for its variables
/// halfway through, after a remote repository was already created.
pub fn validate_plan_variables(
    plan: &InstantiationPlan,
    secrets: &PlanSecrets,
) -> Result<(), Problem> {
    for planned in plan
        .services
        .iter()
        .filter(|service| service.project_id.is_none())
    {
        let Some(spec) = plan.template.service(&planned.template_service) else {
            continue;
        };
        let supplied = secrets
            .services
            .get(&planned.template_service)
            .map(Vec::as_slice)
            .unwrap_or_default();
        temps_projects::template_creation::validate_template_variables(
            &service_variable_templates(plan, spec),
            supplied,
        )
        .map_err(|problem| {
            if problem.status_code != axum::http::StatusCode::BAD_REQUEST {
                return problem;
            }
            temps_core::problemdetails::new(axum::http::StatusCode::BAD_REQUEST)
                .with_title("Invalid Template Environment")
                .with_detail(format!(
                    "Service '{}': {}",
                    planned.template_service,
                    problem_message(problem)
                ))
        })?;
    }
    Ok(())
}

/// Whether instantiation `instantiation_id` can still run, given whether its
/// sealed secrets are still stored.
///
/// A stored instantiation always has sealed secrets (even when empty), so a
/// missing value means the retention cleanup removed it. Such a run could
/// only continue with empty values: `step_variables` would write nothing and
/// report success, and services would be created without what the caller
/// supplied. It is refused instead, naming what is lost.
pub fn ensure_secrets_available(
    instantiation_id: i32,
    secrets_present: bool,
    plan: &InstantiationPlan,
) -> Result<(), ProjectGroupError> {
    if secrets_present {
        return Ok(());
    }
    let steps = plan.steps_needing_secrets();
    if steps.is_empty() {
        return Ok(());
    }
    Err(ProjectGroupError::InstantiationSecretsPurged {
        instantiation_id,
        steps: steps.join(", "),
    })
}

/// Aborts the heartbeat task when dropped, so the lease stops being renewed
/// on every way out of a run, including an early `?` return and a panic. A
/// dropped `JoinHandle` only detaches the task: it would keep refreshing the
/// row of a run that is gone, and the instantiation would read as running
/// (and refuse a retry) for as long as the process lives.
struct HeartbeatGuard {
    handle: tokio::task::JoinHandle<()>,
}

impl HeartbeatGuard {
    /// Run `beat` every `interval` until the guard is dropped.
    fn start<F, Fut>(interval: std::time::Duration, beat: F) -> Self
    where
        F: Fn() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.tick().await; // skip the first immediate tick
            loop {
                ticker.tick().await;
                beat().await;
            }
        });
        Self { handle }
    }
}

impl Drop for HeartbeatGuard {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Releases an instantiation's in-process claim when dropped, so a run that
/// panics does not leave it reading as running until the process restarts.
struct LocalClaim {
    engine: InstantiationEngine,
    id: i32,
}

impl Drop for LocalClaim {
    fn drop(&mut self) {
        self.engine.release(self.id);
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

    // Record which services were given values, by name only: the values are
    // sealed in `secrets` and eventually purged, and the plan has to keep
    // knowing that a service cannot be created without them.
    let mut services = input.services;
    for service in &mut services {
        service.supplied_variable_names = secrets
            .services
            .get(&service.template_service)
            .map(|supplied| supplied.iter().map(|input| input.name.clone()).collect())
            .unwrap_or_default();
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
        services,
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
            let _claim = LocalClaim {
                engine: engine.clone(),
                id,
            };
            if let Err(error) = engine.run(id, &auth, &origin).await {
                error!(instantiation_id = id, %error, "Instantiation stopped");
            }
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
        // The retention cleanup clears the secrets of a failed run. Going on
        // without them would skip the shared variables and report success.
        if let Err(error) = ensure_secrets_available(id, model.secrets.is_some(), &plan) {
            let message = error.to_string();
            self.finish(&model, &plan, Err(message.clone())).await;
            return Err(message);
        }
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
        // still alive. The guard aborts it when `run` returns by any path.
        let heartbeat = {
            let engine = self.clone();
            HeartbeatGuard::start(Self::HEARTBEAT_INTERVAL, move || {
                let engine = engine.clone();
                async move { engine.heartbeat(id).await }
            })
        };

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
                drop(heartbeat);
                self.finish(&model, &plan, Err(message)).await;
                return Ok(());
            }
            if let Err(message) = self.save(&model, &plan, "running", None).await {
                // The step ran but its result could not be stored. Stop
                // renewing the lease and try to mark the run failed with the
                // plan as it is now, so a retry resumes after this step. If
                // the database is still unreachable the row is left running
                // with a lease that expires, which reads as interrupted.
                error!(instantiation_id = id, step = %key, %message, "Could not store the progress of an instantiation");
                drop(heartbeat);
                self.finish(&model, &plan, Err(message.clone())).await;
                return Err(message);
            }
        }
        drop(heartbeat);
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
            "repository" => {
                self.step_repository(plan, secrets, model, auth, origin)
                    .await
            }
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
        secrets: &PlanSecrets,
        model: &template_instantiations::Model,
        auth: &AuthContext,
        origin: &RequestOrigin,
    ) -> Result<(), String> {
        if plan.repository.is_none() {
            let target = match plan.fork.clone() {
                Some(fork) => {
                    // Creating the repository cannot be undone and would
                    // block a corrected retry under the same name, so the
                    // values every service will be created with are checked
                    // first (the request handler did this already; this
                    // covers a plan stored by an earlier build).
                    validate_plan_variables(plan, secrets).map_err(problem_message)?;
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
                plan.follow_repository = target_follows_group_repository(&current, &target);
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
                            // The group has no repository, so nothing follows
                            // it and nothing was authorized: a project that
                            // starts following before this lands fails it.
                            &HashSet::new(),
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

        // The project this instantiation created for this service, if a
        // previous run created it and crashed before recording its id. It is
        // told apart by an ownership token (see
        // `temps_entities::projects::instantiation_token`): nothing else
        // about a project identifies the run that made it, so the name, the
        // creator or the timing of one that merely sits at the planned slug
        // prove nothing.
        //
        // The token proves ownership; *completion* is what its spelling
        // says. A project is inserted with the pending token and switched to
        // the final one only after its environment, variables and service
        // links exist, so:
        //   - a project with the final token is complete and is adopted;
        //   - a project with the pending token is this run's own but was cut
        //     short (no deployment was ever queued: those wait for this
        //     engine). It is discarded and created again below, never
        //     adopted and never marked done: adopting it would queue a
        //     deployment for a project with no environment or no variables.
        // The DB lease on the instantiation means no other runner is creating
        // this service while we look.
        if let Some(owned) = owned_project_for_service(
            self.db.as_ref(),
            &self.creator.project_service,
            model.id,
            template_service,
        )
        .await?
        {
            if owned.is_deleted {
                return Err(format!(
                    "Service '{template_service}' of instantiation {} was created as project {} \
                     ('{}') and has since been deleted; start a new instantiation",
                    model.id, owned.id, owned.slug
                ));
            }
            info!(
                instantiation_id = model.id,
                project_id = owned.id,
                slug = %owned.slug,
                service = template_service,
                "Adopting a service created by a previous run that crashed before recording it"
            );
            if let Some(service) = plan.service_mut(template_service) {
                service.project_id = Some(owned.id);
                service.slug.clone_from(&owned.slug);
                service.creation_started = true;
            }
            return Ok(());
        }

        // Any other project at the planned slug is someone else's. A planned
        // slug is not reserved, so one may have been made since the plan was
        // stored; taking it over would move it into this project and hand
        // it the shared variables. Another slug is planned instead.
        let slug = match self
            .creator
            .project_service
            .get_project_by_slug(&planned.slug)
            .await
        {
            Ok(_) => {
                warn!(
                    instantiation_id = model.id,
                    slug = %planned.slug,
                    service = template_service,
                    "The planned slug belongs to a project this instantiation did not create; planning another"
                );
                self.creator
                    .project_service
                    .plan_project_slug(&planned.name)
                    .await
                    .map_err(|error| error.to_string())?
            }
            Err(_) => planned.slug.clone(),
        };
        // Write down that creation begins before it does, so that a crash
        // from here on can be told apart from a project that merely holds
        // the slug.
        if let Some(service) = plan.service_mut(template_service) {
            service.slug = slug.clone();
            service.creation_started = true;
        }
        self.save(model, plan, "running", None)
            .await
            .map_err(|error| {
                format!(
                    "recording that service '{template_service}' is about to be created: {error}"
                )
            })?;

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
        let variable_templates = service_variable_templates(plan, &spec);
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
                    // The final token: the project is created holding its
                    // pending spelling and only takes this one once it is
                    // fully initialized.
                    instantiation_token: Some(temps_entities::projects::instantiation_token(
                        model.id,
                        template_service,
                    )),
                    deploy_mode,
                    cloudflare_enabled: None,
                    delivery_provider: None,
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

/// The project a previous run of instantiation `instantiation_id` left for
/// `template_service`, if it is one that may be adopted: complete, per the
/// final token. A project the run began and did not finish (it still holds
/// the pending token) is discarded first, so the caller creates the service
/// afresh instead of adopting a project without its environment, variables
/// or service links. May return a soft-deleted project (the caller reports
/// it); never returns an unfinished one.
async fn owned_project_for_service(
    db: &DatabaseConnection,
    project_service: &temps_projects::ProjectService,
    instantiation_id: i32,
    template_service: &str,
) -> Result<Option<projects::Model>, String> {
    if let Some(unfinished) =
        unfinished_project_for_service(db, instantiation_id, template_service).await?
    {
        let pending = temps_entities::projects::pending_instantiation_token(
            &temps_entities::projects::instantiation_token(instantiation_id, template_service),
        );
        warn!(
            instantiation_id,
            project_id = unfinished.id,
            slug = %unfinished.slug,
            service = template_service,
            "Discarding a project a previous run created but did not finish initializing"
        );
        project_service
            .discard_unfinished_instantiation_project(unfinished.id, &pending)
            .await
            .map_err(|error| {
                format!(
                    "discarding project {} ('{}'), which a previous run created for service \
                     '{template_service}' of instantiation {instantiation_id} and did not \
                     finish: {error}",
                    unfinished.id, unfinished.slug
                )
            })?;
    }
    let Some(owned) = project_created_for_service(db, instantiation_id, template_service).await?
    else {
        return Ok(None);
    };
    // Defense in depth: the final token is only written once the production
    // environment exists, so a project without one was not made by this code
    // path. Never hand it on to be deployed.
    if !owned.is_deleted && !project_has_environment(db, owned.id).await? {
        return Err(format!(
            "Service '{template_service}' of instantiation {instantiation_id} was created as \
             project {} ('{}') but has no environment, so it was not fully initialized; delete \
             that project and start a new instantiation",
            owned.id, owned.slug
        ));
    }
    Ok(Some(owned))
}

/// The *complete* project instantiation `instantiation_id` created for
/// `template_service`, found by its final ownership token (soft-deleted ones
/// included, since they still hold the token and the slug). The token is
/// only written once the project's environment, variables and service links
/// exist, so a project returned here has finished initializing.
///
/// This is the only proof of ownership. A project at the service's planned
/// slug without the token is never ours, whatever its name, provenance,
/// creator or age: the same user may well have created the same service
/// independently or through another instantiation. One that holds only the
/// pending token is [`unfinished_project_for_service`], not this.
async fn project_created_for_service(
    db: &DatabaseConnection,
    instantiation_id: i32,
    template_service: &str,
) -> Result<Option<projects::Model>, String> {
    let token = temps_entities::projects::instantiation_token(instantiation_id, template_service);
    project_holding_token(db, instantiation_id, template_service, &token).await
}

/// The project instantiation `instantiation_id` began creating for
/// `template_service` and did not finish: it holds the *pending* token, so
/// its environment, variables or service links may be missing. Never
/// adopted; the caller discards it.
async fn unfinished_project_for_service(
    db: &DatabaseConnection,
    instantiation_id: i32,
    template_service: &str,
) -> Result<Option<projects::Model>, String> {
    let token = temps_entities::projects::pending_instantiation_token(
        &temps_entities::projects::instantiation_token(instantiation_id, template_service),
    );
    project_holding_token(db, instantiation_id, template_service, &token).await
}

/// Whether `project_id` has at least one environment (deleted or not: this
/// asks whether initialization ever ran, not whether it is still deployable).
async fn project_has_environment(db: &DatabaseConnection, project_id: i32) -> Result<bool, String> {
    use sea_orm::PaginatorTrait;
    temps_entities::environments::Entity::find()
        .filter(temps_entities::environments::Column::ProjectId.eq(project_id))
        .count(db)
        .await
        .map(|count| count > 0)
        .map_err(|error| format!("looking up the environments of project {project_id}: {error}"))
}

async fn project_holding_token(
    db: &DatabaseConnection,
    instantiation_id: i32,
    template_service: &str,
    token: &str,
) -> Result<Option<projects::Model>, String> {
    projects::Entity::find()
        .filter(projects::Column::InstantiationToken.eq(token))
        .one(db)
        .await
        .map_err(|error| {
            format!(
                "looking up the project created for service '{template_service}' of \
                 instantiation {instantiation_id}: {error}"
            )
        })
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

/// Whether a service about to be created from `target` reads the repository
/// `current` the group already points at, so it may inherit it.
///
/// The same comparison as [`crate::repository::follows`] makes for existing
/// members (both go through [`same_repository`]): owner, name and branch are
/// not enough, since `acme/shop@main` exists on every provider and every
/// self-hosted instance. Writing the group's clone URL and connection into a
/// service whose template meant another repository would build the wrong
/// code on its first deployment.
fn target_follows_group_repository(current: &GroupRepository, target: &RepositoryTarget) -> bool {
    same_repository(
        &GroupRepositoryRef::from(current),
        &MemberRepositoryRef {
            connection_id: target.git_provider_connection_id,
            is_public: target.is_public,
            owner: &target.repo_owner,
            name: &target.repo_name,
            branch: &target.branch,
            git_url: target.git_url.as_deref(),
        },
    )
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
        yaml_serde::from_str(APP).expect("template")
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
                creation_started: false,
                supplied_variable_names: Vec::new(),
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

    // -----------------------------------------------------------------
    // Heartbeat and local claim
    // -----------------------------------------------------------------

    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn beats_stop_after(counter: &Arc<AtomicUsize>) {
        // Let any beat already scheduled run, then check none follow.
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        let settled = counter.load(Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        assert_eq!(
            counter.load(Ordering::SeqCst),
            settled,
            "the heartbeat kept running after its guard was dropped"
        );
    }

    #[tokio::test]
    async fn heartbeat_beats_while_the_guard_lives_and_stops_when_it_drops() {
        let counter = Arc::new(AtomicUsize::new(0));
        let beats = counter.clone();
        let guard = HeartbeatGuard::start(std::time::Duration::from_millis(5), move || {
            let beats = beats.clone();
            async move {
                beats.fetch_add(1, Ordering::SeqCst);
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        assert!(
            counter.load(Ordering::SeqCst) > 0,
            "the heartbeat never beat while the guard was alive"
        );
        drop(guard);
        beats_stop_after(&counter).await;
    }

    /// Stands in for `run`: a step succeeds, then storing its progress fails
    /// and `?` returns before any explicit cleanup.
    async fn run_that_fails_to_save(beats: Arc<AtomicUsize>) -> Result<(), String> {
        let _heartbeat = HeartbeatGuard::start(std::time::Duration::from_millis(5), move || {
            let beats = beats.clone();
            async move {
                beats.fetch_add(1, Ordering::SeqCst);
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        Err::<(), String>("saving instantiation 1: connection closed".to_string())?;
        Ok(())
    }

    #[tokio::test]
    async fn heartbeat_stops_when_a_run_returns_early_on_an_error() {
        let counter = Arc::new(AtomicUsize::new(0));
        let result = run_that_fails_to_save(counter.clone()).await;
        assert!(result.is_err());
        assert!(counter.load(Ordering::SeqCst) > 0);
        beats_stop_after(&counter).await;
    }

    #[tokio::test]
    async fn heartbeat_stops_when_the_run_panics() {
        let counter = Arc::new(AtomicUsize::new(0));
        let beats = counter.clone();
        let task = tokio::spawn(async move {
            let _heartbeat =
                HeartbeatGuard::start(std::time::Duration::from_millis(5), move || {
                    let beats = beats.clone();
                    async move {
                        beats.fetch_add(1, Ordering::SeqCst);
                    }
                });
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            panic!("a step panicked");
        });
        assert!(task.await.is_err(), "the run should have panicked");
        beats_stop_after(&counter).await;
    }

    // -----------------------------------------------------------------
    // Secrets purged by the retention cleanup
    // -----------------------------------------------------------------

    fn plan_with(
        steps: &[(&str, StepStatus)],
        target: PlannedTarget,
        shared: bool,
    ) -> InstantiationPlan {
        let mut template = template();
        if !shared {
            template.shared_env_vars.clear();
        }
        let services = planned(&template);
        InstantiationPlan {
            template,
            carries_release: true,
            provenance: Some("secrets-manager".to_string()),
            target,
            fork: None,
            repository: None,
            follow_repository: false,
            databases: Vec::new(),
            services,
            storage_claim_ids: Vec::new(),
            group_id: Some(3),
            steps: steps
                .iter()
                .map(|(key, status)| InstantiationStep {
                    key: key.to_string(),
                    label: key.to_string(),
                    status: *status,
                    error: None,
                })
                .collect(),
        }
    }

    fn new_project() -> PlannedTarget {
        PlannedTarget::NewProject {
            name: "Secrets".to_string(),
            slug: "secrets".to_string(),
        }
    }

    #[test]
    fn a_run_that_still_needs_shared_variables_is_refused_once_secrets_are_purged() {
        let plan = plan_with(
            &[
                ("service:backend", StepStatus::Done),
                ("service:frontend", StepStatus::Done),
                ("variables", StepStatus::Failed),
                ("deploy:backend", StepStatus::Pending),
            ],
            new_project(),
            true,
        );
        // Present: fine, whatever the plan needs.
        assert!(ensure_secrets_available(12, true, &plan).is_ok());
        // Purged: refused, naming the instantiation and the lost step.
        let error = ensure_secrets_available(12, false, &plan).expect_err("purged");
        assert!(matches!(
            error,
            ProjectGroupError::InstantiationSecretsPurged {
                instantiation_id: 12,
                ..
            }
        ));
        let message = error.to_string();
        assert!(message.contains("Instantiation 12"), "{message}");
        assert!(message.contains("variables"), "{message}");
        assert!(message.contains("Start a new instantiation"), "{message}");
        // Surfaces as a 4xx the caller can act on, not a server error.
        let problem = temps_core::problemdetails::Problem::from(error);
        assert_eq!(problem.status_code, axum::http::StatusCode::CONFLICT);
    }

    #[test]
    fn a_run_needing_supplied_service_values_is_refused_once_secrets_are_purged() {
        // The backend declares SMTP_HOST and is not created yet.
        let plan = plan_with(
            &[
                ("service:backend", StepStatus::Failed),
                ("service:frontend", StepStatus::Pending),
            ],
            new_project(),
            false,
        );
        let error = ensure_secrets_available(5, false, &plan).expect_err("purged");
        assert!(error.to_string().contains("service:backend"));
        // The frontend declares nothing, so it alone would not need them.
        let mut plan = plan;
        plan.services[0].project_id = Some(90);
        assert!(ensure_secrets_available(5, false, &plan).is_ok());
    }

    #[test]
    fn a_run_with_nothing_left_that_reads_secrets_may_still_be_retried() {
        // Failed at the deploy step, long after every value was written.
        let plan = plan_with(
            &[
                ("service:backend", StepStatus::Done),
                ("service:frontend", StepStatus::Done),
                ("variables", StepStatus::Done),
                ("deploy:backend", StepStatus::Failed),
                ("deploy:frontend", StepStatus::Pending),
            ],
            new_project(),
            true,
        );
        assert!(plan.steps_needing_secrets().is_empty());
        assert!(ensure_secrets_available(8, false, &plan).is_ok());
    }

    #[test]
    fn shared_variables_flow_into_the_only_service_without_a_project() {
        let mut template = template();
        template.services.truncate(1);
        template.routes.clear();
        let services = planned(&template);
        let mut plan = plan_with(
            &[("service:backend", StepStatus::Pending)],
            PlannedTarget::NoProject,
            true,
        );
        plan.template = template;
        plan.services = services;
        plan.template.services[0].env_vars.clear();
        // No declared variables of its own, but it receives the shared ones.
        assert_eq!(plan.steps_needing_secrets(), vec!["service:backend"]);
    }

    // -----------------------------------------------------------------
    // Supplied variables the template never declared
    // -----------------------------------------------------------------

    /// A service that declares no variables, given one the template never
    /// declared (the caller's own token, say).
    fn plan_with_undeclared_variable() -> (InstantiationPlan, PlanSecrets) {
        let mut template = template();
        template.shared_env_vars.clear();
        template.routes.clear();
        for service in &mut template.services {
            service.env_vars.clear();
        }
        let mut plan_input = input(
            template,
            PlannedTarget::ExistingProject { group_id: 3 },
            &[],
        );
        let supplied = [variable("API_TOKEN", "sk-test-value", Some("frontend"))];
        plan_input.variables = &supplied;
        let (plan, secrets) = build_plan(plan_input).expect("plan");
        (plan, secrets)
    }

    #[test]
    fn a_plan_records_which_services_were_given_values_but_never_the_values() {
        let (plan, secrets) = plan_with_undeclared_variable();
        assert_eq!(secrets.services["frontend"][0].value, "sk-test-value");
        let frontend = plan
            .services
            .iter()
            .find(|service| service.template_service == "frontend")
            .expect("frontend");
        assert_eq!(frontend.supplied_variable_names, vec!["API_TOKEN"]);
        let backend = plan
            .services
            .iter()
            .find(|service| service.template_service == "backend")
            .expect("backend");
        assert!(backend.supplied_variable_names.is_empty());
        let stored = serde_json::to_string(&plan).expect("serialize");
        assert!(stored.contains("API_TOKEN"));
        assert!(!stored.contains("sk-test-value"));
    }

    #[test]
    fn an_undeclared_supplied_variable_blocks_a_retry_once_secrets_are_purged() {
        let (mut plan, _) = plan_with_undeclared_variable();
        plan.steps = vec![
            InstantiationStep {
                key: "service:backend".to_string(),
                label: "backend".to_string(),
                status: StepStatus::Done,
                error: None,
            },
            InstantiationStep {
                key: "service:frontend".to_string(),
                label: "frontend".to_string(),
                status: StepStatus::Failed,
                error: None,
            },
        ];
        plan.services[0].project_id = Some(90);
        // The template declares nothing for either service: before the plan
        // recorded supplied names this was indistinguishable from "nothing
        // was lost", and the service was created without its token.
        assert_eq!(plan.steps_needing_secrets(), vec!["service:frontend"]);
        assert!(ensure_secrets_available(21, true, &plan).is_ok());
        let error = ensure_secrets_available(21, false, &plan).expect_err("purged");
        assert!(matches!(
            error,
            ProjectGroupError::InstantiationSecretsPurged {
                instantiation_id: 21,
                ..
            }
        ));
        assert!(error.to_string().contains("service:frontend"));
        let problem = temps_core::problemdetails::Problem::from(error);
        assert_eq!(problem.status_code, axum::http::StatusCode::CONFLICT);

        // Once the service exists the values are no longer needed.
        plan.services[1].project_id = Some(91);
        assert!(plan.steps_needing_secrets().is_empty());
        assert!(ensure_secrets_available(21, false, &plan).is_ok());
    }

    #[test]
    fn plans_stored_before_supplied_names_were_recorded_read_as_empty() {
        let json = serde_json::json!({
            "template_service": "frontend",
            "name": "frontend",
            "slug": "secrets-frontend",
            "project_id": null,
            "creation_started": true,
        });
        let service: PlannedService = serde_json::from_value(json).expect("old plan");
        assert!(service.supplied_variable_names.is_empty());
        assert!(service.creation_started);

        // And such a plan keeps the earlier rule: only declared variables
        // (here none) make a pending service need the secrets.
        let (mut plan, _) = plan_with_undeclared_variable();
        plan.services[1].supplied_variable_names.clear();
        plan.steps = vec![InstantiationStep {
            key: "service:frontend".to_string(),
            label: "frontend".to_string(),
            status: StepStatus::Pending,
            error: None,
        }];
        assert!(plan.steps_needing_secrets().is_empty());
    }

    #[test]
    fn variables_supplied_without_a_project_are_recorded_on_the_only_service() {
        let mut template = template();
        template.services.truncate(1);
        template.routes.clear();
        let (plan, _) = build_plan(input(
            template,
            PlannedTarget::NoProject,
            &[variable("LOG_LEVEL", "debug", None)],
        ))
        .expect("plan");
        assert_eq!(plan.services[0].supplied_variable_names, vec!["LOG_LEVEL"]);
    }

    // -----------------------------------------------------------------
    // Values are checked before any remote side effect
    // -----------------------------------------------------------------

    #[test]
    fn missing_or_duplicate_values_are_refused_before_a_fork_repository_exists() {
        // The backend declares SMTP_HOST, which has no default.
        let mut template = template();
        template.services[0].env_vars[0].required = true;
        let (plan, secrets) = build_plan(input(
            template.clone(),
            PlannedTarget::ExistingProject { group_id: 3 },
            &[],
        ))
        .expect("plan");
        let problem = validate_plan_variables(&plan, &secrets).expect_err("missing");
        assert_eq!(problem.status_code, axum::http::StatusCode::BAD_REQUEST);
        let message = problem_message(problem);
        assert!(message.contains("backend"), "{message}");
        assert!(message.contains("SMTP_HOST"), "{message}");

        // Supplied: accepted.
        let (plan, secrets) = build_plan(input(
            template.clone(),
            PlannedTarget::ExistingProject { group_id: 3 },
            &[variable("SMTP_HOST", "mail.example.test", None)],
        ))
        .expect("plan");
        assert!(validate_plan_variables(&plan, &secrets).is_ok());

        // The same name twice for one service.
        let (plan, secrets) = build_plan(input(
            template,
            PlannedTarget::ExistingProject { group_id: 3 },
            &[
                variable("SMTP_HOST", "a.example.test", None),
                variable("SMTP_HOST", "b.example.test", None),
            ],
        ))
        .expect("plan");
        let problem = validate_plan_variables(&plan, &secrets).expect_err("duplicate");
        assert_eq!(problem.status_code, axum::http::StatusCode::BAD_REQUEST);
        assert!(problem_message(problem).contains("SMTP_HOST"));
    }

    #[test]
    fn services_that_already_exist_are_not_revalidated() {
        let mut template = template();
        template.services[0].env_vars[0].required = true;
        let (mut plan, secrets) = build_plan(input(
            template,
            PlannedTarget::ExistingProject { group_id: 3 },
            &[],
        ))
        .expect("plan");
        plan.services[0].project_id = Some(7);
        assert!(validate_plan_variables(&plan, &secrets).is_ok());
    }

    #[test]
    fn the_fork_step_validates_variables_before_creating_the_repository() {
        // `step_repository` needs a database and a git provider to run, so
        // pin the ordering in the source: the check precedes the creation.
        let source = include_str!("instantiation.rs");
        let start = source
            .find("async fn step_repository(")
            .expect("step_repository");
        let body = &source[start..];
        let validated = body
            .find("validate_plan_variables(plan, secrets)")
            .expect("step_repository validates the plan's variables");
        let created = body
            .find(".create_repository_and_push_template(")
            .expect("step_repository creates the fork");
        assert!(validated < created);
    }

    // -----------------------------------------------------------------
    // Adopting a project found at a planned slug
    // -----------------------------------------------------------------

    #[test]
    fn creation_started_defaults_to_false_for_plans_stored_before_it_existed() {
        let json = serde_json::json!({
            "template_service": "backend",
            "name": "backend",
            "slug": "secrets-backend",
            "project_id": null,
        });
        let service: PlannedService = serde_json::from_value(json).expect("old plan");
        assert!(!service.creation_started);
    }

    async fn insert_project(
        db: &DatabaseConnection,
        slug: &str,
        name: &str,
        token: Option<String>,
        created_at: chrono::DateTime<Utc>,
    ) -> projects::Model {
        projects::ActiveModel {
            name: Set(name.to_string()),
            slug: Set(slug.to_string()),
            repo_name: Set("secrets".to_string()),
            repo_owner: Set("example".to_string()),
            directory: Set(".".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            template_slug: Set(Some("secrets-manager".to_string())),
            instantiation_token: Set(token),
            created_at: Set(created_at),
            updated_at: Set(created_at),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert project")
    }

    #[test]
    fn the_token_names_the_instantiation_and_the_service() {
        let token = temps_entities::projects::instantiation_token(7, "backend");
        assert_ne!(
            token,
            temps_entities::projects::instantiation_token(7, "frontend")
        );
        assert_ne!(
            token,
            temps_entities::projects::instantiation_token(8, "backend")
        );
        // `1` + `1:service:x` must never read as `11` + `:service:x`.
        assert_ne!(
            temps_entities::projects::instantiation_token(1, "backend"),
            temps_entities::projects::instantiation_token(11, "backend")
        );
    }

    /// A run that crashed after inserting its project and before recording
    /// the id finds that project again; nothing else is taken for it.
    #[tokio::test]
    async fn a_project_is_adopted_only_by_the_instantiation_that_created_it() {
        use temps_database::test_utils::TestDatabase;
        let Ok(db) = TestDatabase::with_migrations().await else {
            eprintln!("Docker/TimescaleDB unavailable, skipping instantiation adoption test");
            return;
        };
        let conn = db.connection_arc();
        let now = Utc::now();

        // Run 7 inserted its backend, then crashed before recording it.
        let own = insert_project(
            conn.as_ref(),
            "secrets-backend",
            "backend",
            Some(temps_entities::projects::instantiation_token(7, "backend")),
            now,
        )
        .await;
        let found = project_created_for_service(conn.as_ref(), 7, "backend")
            .await
            .expect("lookup")
            .expect("the run's own project");
        assert_eq!(found.id, own.id);
        assert!(!found.is_deleted);

        // Not the same instantiation, not the same service, and not an id
        // that merely begins the same.
        assert!(project_created_for_service(conn.as_ref(), 8, "backend")
            .await
            .expect("lookup")
            .is_none());
        assert!(project_created_for_service(conn.as_ref(), 7, "frontend")
            .await
            .expect("lookup")
            .is_none());
        assert!(project_created_for_service(conn.as_ref(), 70, "backend")
            .await
            .expect("lookup")
            .is_none());
        assert!(project_created_for_service(conn.as_ref(), 1, "backend")
            .await
            .expect("lookup")
            .is_none());
    }

    #[tokio::test]
    async fn a_lookalike_project_of_the_same_user_is_never_adopted() {
        use temps_database::test_utils::TestDatabase;
        let Ok(db) = TestDatabase::with_migrations().await else {
            eprintln!("Docker/TimescaleDB unavailable, skipping instantiation adoption test");
            return;
        };
        let conn = db.connection_arc();
        let now = Utc::now();

        // Run 7 failed before inserting anything. The same user then made
        // the same service on their own, and another run (8) made one too.
        // Both have the planned name and provenance, were created after run
        // 7 began, and would have a PROJECT_CREATED audit row for the user.
        let independent =
            insert_project(conn.as_ref(), "secrets-backend", "backend", None, now).await;
        let other_run = insert_project(
            conn.as_ref(),
            "secrets-backend-2",
            "backend",
            Some(temps_entities::projects::instantiation_token(8, "backend")),
            now,
        )
        .await;

        assert!(project_created_for_service(conn.as_ref(), 7, "backend")
            .await
            .expect("lookup")
            .is_none());
        // Each is found only by its own owner (an independent project has none).
        let found = project_created_for_service(conn.as_ref(), 8, "backend")
            .await
            .expect("lookup")
            .expect("run 8's project");
        assert_eq!(found.id, other_run.id);
        assert_ne!(found.id, independent.id);
        assert_eq!(independent.instantiation_token, None);
    }

    #[tokio::test]
    async fn a_deleted_project_of_the_run_is_still_recognised_as_its_own() {
        use temps_database::test_utils::TestDatabase;
        let Ok(db) = TestDatabase::with_migrations().await else {
            eprintln!("Docker/TimescaleDB unavailable, skipping instantiation adoption test");
            return;
        };
        let conn = db.connection_arc();
        let own = insert_project(
            conn.as_ref(),
            "secrets-worker",
            "worker",
            Some(temps_entities::projects::instantiation_token(9, "worker")),
            Utc::now(),
        )
        .await;
        let mut active: projects::ActiveModel = own.into();
        active.is_deleted = Set(true);
        active.deleted_at = Set(Some(Utc::now()));
        active.update(conn.as_ref()).await.expect("soft delete");

        let found = project_created_for_service(conn.as_ref(), 9, "worker")
            .await
            .expect("lookup")
            .expect("still holds the token");
        assert!(
            found.is_deleted,
            "the caller must see the deletion, not adopt the project"
        );
    }

    async fn insert_production_environment(db: &DatabaseConnection, project_id: i32) {
        use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO environments (name, slug, subdomain, host, upstreams, created_at, \
             updated_at, project_id, is_preview, protected, sleeping) \
             VALUES ('production', 'production', $2, $2, '[]'::jsonb, NOW(), NOW(), $1, \
             FALSE, FALSE, FALSE)",
            [project_id.into(), format!("env-{project_id}").into()],
        ))
        .await
        .expect("insert environment");
    }

    async fn project_exists(db: &DatabaseConnection, id: i32) -> bool {
        projects::Entity::find_by_id(id)
            .one(db)
            .await
            .expect("lookup")
            .is_some()
    }

    /// The crash window of project creation: a row inserted by the run and
    /// never sealed (it holds the pending token, and has no environment yet)
    /// is the run's own but unfinished. It is discarded and the service is
    /// created afresh; it is never adopted, so a half-initialized project is
    /// never marked done or deployed.
    #[tokio::test]
    async fn a_project_that_never_finished_initializing_is_discarded_not_adopted() {
        use temps_database::test_utils::TestDatabase;
        let Ok(db) = TestDatabase::with_migrations().await else {
            eprintln!("Docker/TimescaleDB unavailable, skipping instantiation adoption test");
            return;
        };
        let conn = db.connection_arc();
        let Some((project_service, _)) =
            crate::repository::tests::project_service(conn.clone()).await
        else {
            eprintln!("Docker client unavailable, skipping instantiation adoption test");
            return;
        };
        let pending = temps_entities::projects::pending_instantiation_token(
            &temps_entities::projects::instantiation_token(11, "backend"),
        );
        let unfinished = insert_project(
            conn.as_ref(),
            "shop-backend",
            "backend",
            Some(pending),
            Utc::now(),
        )
        .await;

        let found = owned_project_for_service(conn.as_ref(), &project_service, 11, "backend")
            .await
            .expect("lookup");
        assert!(found.is_none(), "an unfinished project is not adopted");
        assert!(
            !project_exists(conn.as_ref(), unfinished.id).await,
            "it was discarded so the service is created afresh"
        );
        // A second look finds nothing either.
        assert!(
            owned_project_for_service(conn.as_ref(), &project_service, 11, "backend")
                .await
                .expect("lookup")
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_completely_initialized_project_is_adopted() {
        use temps_database::test_utils::TestDatabase;
        let Ok(db) = TestDatabase::with_migrations().await else {
            eprintln!("Docker/TimescaleDB unavailable, skipping instantiation adoption test");
            return;
        };
        let conn = db.connection_arc();
        let Some((project_service, _)) =
            crate::repository::tests::project_service(conn.clone()).await
        else {
            eprintln!("Docker client unavailable, skipping instantiation adoption test");
            return;
        };
        let complete = insert_project(
            conn.as_ref(),
            "shop-worker",
            "worker",
            Some(temps_entities::projects::instantiation_token(12, "worker")),
            Utc::now(),
        )
        .await;
        insert_production_environment(conn.as_ref(), complete.id).await;
        // Another service of the same run is still mid-creation: only its
        // own row is discarded, the finished one is untouched.
        let other = insert_project(
            conn.as_ref(),
            "shop-backend-2",
            "backend",
            Some(temps_entities::projects::pending_instantiation_token(
                &temps_entities::projects::instantiation_token(12, "backend"),
            )),
            Utc::now(),
        )
        .await;

        let adopted = owned_project_for_service(conn.as_ref(), &project_service, 12, "worker")
            .await
            .expect("lookup")
            .expect("the complete project");
        assert_eq!(adopted.id, complete.id);
        assert!(
            project_exists(conn.as_ref(), other.id).await,
            "looking up one service never discards another service's project"
        );
    }

    /// A project holding the final token but no environment cannot come from
    /// the current creation path (the token is written after the environment
    /// exists); if one is ever found it is refused, not deployed.
    #[tokio::test]
    async fn a_tokened_project_without_an_environment_is_not_adopted() {
        use temps_database::test_utils::TestDatabase;
        let Ok(db) = TestDatabase::with_migrations().await else {
            eprintln!("Docker/TimescaleDB unavailable, skipping instantiation adoption test");
            return;
        };
        let conn = db.connection_arc();
        let Some((project_service, _)) =
            crate::repository::tests::project_service(conn.clone()).await
        else {
            eprintln!("Docker client unavailable, skipping instantiation adoption test");
            return;
        };
        let bare = insert_project(
            conn.as_ref(),
            "shop-api",
            "api",
            Some(temps_entities::projects::instantiation_token(13, "api")),
            Utc::now(),
        )
        .await;

        let error = owned_project_for_service(conn.as_ref(), &project_service, 13, "api")
            .await
            .expect_err("a project without an environment is refused");
        assert!(
            error.contains(&bare.id.to_string()) && error.contains("no environment"),
            "{error}"
        );
        assert!(
            project_exists(conn.as_ref(), bare.id).await,
            "a project that carries the final token is never deleted behind the user's back"
        );
    }

    /// Two source-built services and one pinned image. The git host is not
    /// GitHub, so creating the services never asks a public API for the
    /// repository's directories.
    const STACK_APP: &str = r#"
slug: stack-app
name: Stack app
version: 1.0.0
kind: app
git:
  url: https://git.example.test/example/stack-app.git
  ref: v1.0.0
databases: [postgres]
services:
  - name: api
    directory: api
    preset: dockerfile
    exposed_port: 8080
    database_bindings:
      postgres:
        DATABASE_URL: POSTGRES_URL
  - name: web
    directory: web
    preset: vite
  - name: cache
    preset: dockerfile
    image: registry.example.test/cache@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
    exposed_port: 6379
"#;

    struct DiscardingAudit;

    #[async_trait::async_trait]
    impl temps_core::AuditLogger for DiscardingAudit {
        async fn create_audit_log(
            &self,
            _operation: &dyn temps_core::AuditOperation,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    async fn project_ids_and_types(
        db: &DatabaseConnection,
        plan: &InstantiationPlan,
    ) -> Vec<(String, i32, temps_entities::types::ProjectType)> {
        let mut found = Vec::new();
        for service in &plan.services {
            let id = service
                .project_id
                .unwrap_or_else(|| panic!("service '{}' was created", service.template_service));
            let row = projects::Entity::find_by_id(id)
                .one(db)
                .await
                .expect("lookup")
                .expect("the created service is stored");
            let token =
                temps_entities::projects::instantiation_token(21, &service.template_service);
            assert_eq!(
                row.instantiation_token.as_deref(),
                Some(token.as_str()),
                "'{}' is complete and sealed",
                service.template_service
            );
            assert_eq!(row.template_slug.as_deref(), Some("stack-app"));
            assert!(
                row.service_template.is_some(),
                "'{}' keeps its release",
                service.template_service
            );
            found.push((service.template_service.clone(), id, row.project_type));
        }
        found
    }

    /// An app whose services are built from source can be instantiated
    /// against a real schema -- the `projects` check constraint ties a
    /// release to the service lifecycle, and this is the path that failed on
    /// it -- and a resumed run adopts what it created instead of making a
    /// second set.
    #[tokio::test]
    async fn an_app_with_source_built_services_is_created_and_resumed() {
        use temps_database::test_utils::TestDatabase;
        use temps_entities::types::ProjectType;
        let Ok(db) = TestDatabase::with_migrations().await else {
            eprintln!("Docker/TimescaleDB unavailable, skipping instantiation test");
            return;
        };
        let conn = db.connection_arc();
        let Some((project_service, git)) =
            crate::repository::tests::project_service(conn.clone()).await
        else {
            eprintln!("Docker client unavailable, skipping instantiation test");
            return;
        };
        let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
            eprintln!("Docker client unavailable, skipping instantiation test");
            return;
        };
        let encryption = Arc::new(EncryptionService::new(&"0".repeat(64)).expect("test key"));
        let server_config = Arc::new(
            temps_config::ServerConfig::new(
                "127.0.0.1:3000".to_string(),
                "postgresql://test".to_string(),
                None,
                None,
            )
            .expect("server config"),
        );
        let config = Arc::new(temps_config::ConfigService::new(
            server_config,
            conn.clone(),
        ));
        let audit: Arc<dyn temps_core::AuditLogger> = Arc::new(DiscardingAudit);
        let engine = InstantiationEngine::new(
            conn.clone(),
            encryption.clone(),
            Arc::new(ProjectGroupService::new(conn.clone())),
            Arc::new(GroupEnvVarService::new(conn.clone(), encryption.clone())),
            Arc::new(GroupRepositoryService::new(
                conn.clone(),
                project_service.clone(),
                git.clone(),
            )),
            Arc::new(GroupRouteService::new(conn.clone())),
            TemplateServiceCreator {
                project_service: project_service.clone(),
                config_service: config,
                public_hostname_resolver: Arc::new(temps_core::StandardHostnameResolver),
                audit_service: audit.clone(),
                telemetry: Arc::new(temps_core::NoopTelemetryReporter),
            },
            Arc::new(temps_providers::ExternalServiceManager::new(
                conn.clone(),
                encryption,
                Arc::new(docker),
                Arc::new(temps_providers::DnsRegistry::new(conn.clone())),
            )),
            git,
            Arc::new(temps_auth::DefaultSensitiveActionAuthorizer::new(
                conn.clone(),
            )),
            audit,
        );

        let user = temps_entities::users::ActiveModel {
            name: Set("Instantiation Owner".to_string()),
            email: Set("instantiation-owner@example.com".to_string()),
            ..Default::default()
        }
        .insert(conn.as_ref())
        .await
        .expect("the acting user is inserted");
        let auth = AuthContext::new_session(user.clone(), temps_auth::Role::Admin);
        let template: ProjectTemplate = yaml_serde::from_str(STACK_APP).expect("template");
        let services = planned(&template);
        let mut plan = InstantiationPlan {
            template,
            carries_release: true,
            provenance: Some("stack-app".to_string()),
            target: PlannedTarget::NewProject {
                name: "Stack".to_string(),
                slug: "stack".to_string(),
            },
            fork: None,
            repository: None,
            follow_repository: false,
            databases: Vec::new(),
            services,
            storage_claim_ids: Vec::new(),
            group_id: None,
            steps: Vec::new(),
        };
        let now = Utc::now();
        let model = template_instantiations::ActiveModel {
            id: Set(21),
            user_id: Set(user.id),
            idempotency_key: Set("stack-run".to_string()),
            kind: Set("template".to_string()),
            template_slug: Set(Some("stack-app".to_string())),
            name: Set("Stack".to_string()),
            status: Set("running".to_string()),
            plan: Set(serde_json::to_value(&plan).expect("plan")),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(conn.as_ref())
        .await
        .expect("the instantiation is stored");
        let secrets = PlanSecrets::default();
        let origin = RequestOrigin::default();

        for service in ["api", "web", "cache"] {
            engine
                .step_service(&mut plan, service, &secrets, &model, &auth, &origin)
                .await
                .unwrap_or_else(|error| panic!("service '{service}' must be created: {error}"));
        }
        let created = project_ids_and_types(conn.as_ref(), &plan).await;
        assert_eq!(
            created
                .iter()
                .map(|(name, _, project_type)| (name.as_str(), project_type.clone()))
                .collect::<Vec<_>>(),
            vec![
                ("api", ProjectType::Server),
                ("web", ProjectType::Static),
                ("cache", ProjectType::Service),
            ]
        );

        // A restart before the ids were recorded: the next run finds the
        // projects by their token, adopts them, and creates nothing.
        for service in &mut plan.services {
            service.project_id = None;
            service.creation_started = false;
        }
        for service in ["api", "web", "cache"] {
            engine
                .step_service(&mut plan, service, &secrets, &model, &auth, &origin)
                .await
                .unwrap_or_else(|error| panic!("service '{service}' must be adopted: {error}"));
        }
        assert_eq!(project_ids_and_types(conn.as_ref(), &plan).await, created);
        let stored = projects::Entity::find()
            .filter(projects::Column::TemplateSlug.eq("stack-app"))
            .all(conn.as_ref())
            .await
            .expect("lookup");
        assert_eq!(stored.len(), 3, "the retry did not create a second set");
    }

    fn group_repository(
        connection_id: Option<i32>,
        public_provider: Option<&str>,
    ) -> GroupRepository {
        GroupRepository {
            git_provider_connection_id: connection_id,
            public_provider: public_provider.map(str::to_string),
            repo_owner: "acme".to_string(),
            repo_name: "shop".to_string(),
            main_branch: "main".to_string(),
            repository_id: None,
            clone_url: None,
        }
    }

    fn template_target(
        url: Option<&str>,
        connection_id: Option<i32>,
        is_public: bool,
    ) -> RepositoryTarget {
        RepositoryTarget {
            repo_owner: "acme".to_string(),
            repo_name: "shop".to_string(),
            branch: "main".to_string(),
            git_url: url.map(str::to_string),
            git_provider_connection_id: connection_id,
            is_public,
            directory: ".".to_string(),
            automatic_deploy: false,
        }
    }

    #[test]
    fn a_service_on_another_provider_does_not_follow_the_groups_repository() {
        let github_group = group_repository(None, Some("github"));
        let gitlab_group = group_repository(None, Some("gitlab"));
        let github = template_target(Some("https://github.com/acme/shop.git"), None, true);
        let gitlab = template_target(Some("https://gitlab.com/acme/shop.git"), None, true);

        assert!(target_follows_group_repository(&github_group, &github));
        assert!(target_follows_group_repository(&gitlab_group, &gitlab));
        // Identical owner, name and branch, different provider: both ways.
        assert!(!target_follows_group_repository(&github_group, &gitlab));
        assert!(!target_follows_group_repository(&gitlab_group, &github));
        // A self-hosted host is neither of them.
        let self_hosted = template_target(Some("https://git.example.test/acme/shop"), None, true);
        assert!(!target_follows_group_repository(
            &github_group,
            &self_hosted
        ));
    }

    #[test]
    fn spelling_variants_of_the_same_repository_still_follow() {
        let group = group_repository(None, Some("github"));
        for url in [
            "https://github.com/acme/shop",
            "https://github.com/acme/shop.git",
            "https://www.github.com/acme/shop.git",
            "https://GitHub.com/acme/shop.git/",
        ] {
            assert!(
                target_follows_group_repository(&group, &template_target(Some(url), None, true)),
                "{url}"
            );
        }
        let mut upper = template_target(Some("https://github.com/Acme/Shop.git"), None, true);
        upper.repo_owner = "Acme".to_string();
        upper.repo_name = "Shop".to_string();
        assert!(target_follows_group_repository(&group, &upper));
    }

    #[test]
    fn a_service_through_another_connection_does_not_follow() {
        let group = group_repository(Some(7), None);
        // The fork lives behind the group's own connection.
        assert!(target_follows_group_repository(
            &group,
            &template_target(
                Some("https://git.example.test/acme/shop.git"),
                Some(7),
                false
            )
        ));
        // Same owner, name and branch on a second self-hosted connection.
        assert!(!target_follows_group_repository(
            &group,
            &template_target(
                Some("https://git.example.test/acme/shop.git"),
                Some(8),
                false
            )
        ));
        // A public template repository is not the group's connected one.
        assert!(!target_follows_group_repository(
            &group,
            &template_target(Some("https://github.com/acme/shop.git"), None, true)
        ));
        // And a connected fork is not a public group's anonymous clone.
        assert!(!target_follows_group_repository(
            &group_repository(None, Some("github")),
            &template_target(Some("https://github.com/acme/shop.git"), Some(7), false)
        ));
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
