// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Creating services from templates (ADR-051).
//!
//! `POST /projects/from-template` creates one service; the instantiation
//! engine of project groups creates every service of a template. Both go
//! through the rules here: runtime overrides of image services, variable
//! defaults and generators resolved on the server, the managed databases a
//! service needs, and who may link those databases.

use std::collections::{BTreeMap, BTreeSet};

use axum::http::StatusCode;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};
use temps_auth::permissions::{Permission, Role};
use temps_auth::{permission_guard, AuthContext};
use temps_core::problemdetails::{self, Problem};
use tracing::error;
use utoipa::ToSchema;

use crate::services::types::{CreateProjectEnvVar, CreateProjectRequest, Project, SlugClaimCaller};
use crate::ProjectService;
use std::sync::Arc;
use temps_core::templates::{
    EnvVarTemplate, ProjectTemplate, ServiceTemplateInstance, TemplateServiceSpec,
};
use temps_entities::source_type::SourceType;
use tracing::info;

/// A variable value supplied when creating from a template.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct EnvVarInput {
    /// Variable name
    pub name: String,
    /// Variable value
    pub value: String,
    /// Mark the variable as a secret. Secret values are encrypted at rest,
    /// masked in list responses, and revealable only through an audited,
    /// permission-checked endpoint. Defaults to `false`.
    #[serde(default)]
    pub is_secret: bool,
}

/// Runtime overrides for a service that runs a prebuilt image. Curated
/// template values are used for everything left unset.
#[derive(Debug, Clone, Default, Deserialize, Serialize, ToSchema)]
pub struct TemplateRuntimeOverrides {
    /// Image reference; must be pinned to a sha256 digest.
    #[serde(default)]
    pub image: Option<String>,
    /// Entrypoint arguments. An empty list uses the image's own command.
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// CPU request in microcores (1_000_000 = one CPU core).
    #[serde(default)]
    pub cpu_request: Option<i32>,
    /// CPU limit in microcores. Zero means uncapped.
    #[serde(default)]
    pub cpu_limit: Option<i32>,
    /// Memory request in MiB.
    #[serde(default)]
    pub memory_request: Option<i32>,
    /// Memory limit in MiB. Zero means uncapped.
    #[serde(default)]
    pub memory_limit: Option<i32>,
    /// Public container port.
    #[serde(default)]
    pub exposed_port: Option<i32>,
    /// Relative HTTP health-check path.
    #[serde(default)]
    pub health_check_path: Option<String>,
}

impl From<&crate::handlers::templates::CreateProjectFromTemplateRequest>
    for TemplateRuntimeOverrides
{
    fn from(request: &crate::handlers::templates::CreateProjectFromTemplateRequest) -> Self {
        Self {
            image: request.image.clone(),
            command: request.command.clone(),
            cpu_request: request.cpu_request,
            cpu_limit: request.cpu_limit,
            memory_request: request.memory_request,
            memory_limit: request.memory_limit,
            exposed_port: request.exposed_port,
            health_check_path: request.health_check_path.clone(),
        }
    }
}

/// Which of `service_ids` the caller may link to a new service. Returns the
/// services authorized only by their one-time creator claim.
pub async fn authorize_storage_services(
    external_service_manager: &temps_providers::ExternalServiceManager,
    checker: Option<&dyn temps_core::ProjectAccessChecker>,
    auth: &AuthContext,
    service_ids: &[i32],
) -> Result<Vec<i32>, Problem> {
    if service_ids.is_empty() {
        return Ok(Vec::new());
    }

    let scopes = external_service_manager
        .project_scopes_for_services(service_ids)
        .await
        .map_err(|error| match &error {
            temps_providers::ExternalServiceError::ServiceNotFound { .. } => {
                storage_service_access_denied()
            }
            error => {
                error!(error = %error, "failed to resolve selected database access scopes");
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Database Authorization Failed")
                    .with_detail("The selected databases could not be authorized")
            }
        })?;

    authorize_storage_service_scopes(auth, checker, &scopes).await
}

pub fn storage_service_access_denied() -> Problem {
    problemdetails::new(StatusCode::FORBIDDEN)
        .with_title("Insufficient Permissions")
        .with_detail("You do not have access to one or more selected databases")
}

pub async fn authorize_storage_service_scopes(
    auth: &AuthContext,
    checker: Option<&dyn temps_core::ProjectAccessChecker>,
    scopes: &[temps_providers::ExternalServiceProjectScope],
) -> Result<Vec<i32>, Problem> {
    if auth.is_admin() || auth.has_role(&Role::PlatformAdmin) {
        return Ok(Vec::new());
    }
    let Some(checker) = checker else {
        // Plain OSS has no team boundary.
        return Ok(Vec::new());
    };

    let project_ids: Vec<i32> = scopes
        .iter()
        .flat_map(|scope| scope.project_ids.iter().copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let permissions = checker
        .effective_project_permissions_batch(auth.user_id(), &project_ids)
        .await
        .map_err(|checker_error| {
            error!(
                user_id = auth.user_id(),
                ?project_ids,
                error = %checker_error,
                "selected database permission resolution failed closed"
            );
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Database Authorization Failed")
                .with_detail("Project permissions for the selected databases could not be resolved")
        })?;
    if project_ids
        .iter()
        .any(|project_id| !permissions.contains_key(project_id))
    {
        error!(
            user_id = auth.user_id(),
            ?project_ids,
            "selected database permission result omitted a project"
        );
        return Err(problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("Database Authorization Failed")
            .with_detail("Project permissions for the selected databases were incomplete"));
    }

    let fallback_ids: Vec<i32> = project_ids
        .iter()
        .copied()
        .filter(|project_id| matches!(permissions.get(project_id), Some(None)))
        .collect();
    let coarse_access = checker
        .user_can_access_projects(auth.user_id(), &fallback_ids)
        .await
        .map_err(|checker_error| {
            error!(
                user_id = auth.user_id(),
                ?fallback_ids,
                error = %checker_error,
                "selected database membership resolution failed closed"
            );
            problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Database Authorization Failed")
                .with_detail("Project access for the selected databases could not be resolved")
        })?;
    if fallback_ids
        .iter()
        .any(|project_id| !coarse_access.contains_key(project_id))
    {
        return Err(problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_title("Database Authorization Failed")
            .with_detail("Project access for the selected databases was incomplete"));
    }

    let required_permission = Permission::ExternalServicesWrite.to_string();
    let can_access_project = |project_id: &i32| match permissions.get(project_id) {
        Some(Some(project_permissions)) => project_permissions.contains(&required_permission),
        Some(None) => coarse_access.get(project_id).copied().unwrap_or(false),
        None => false,
    };
    let mut creator_claims = Vec::new();
    for scope in scopes {
        if scope.project_ids.is_empty() && scope.created_by_user_id == Some(auth.user_id()) {
            creator_claims.push(scope.service_id);
        } else if scope.project_ids.is_empty() || !scope.project_ids.iter().any(can_access_project)
        {
            return Err(storage_service_access_denied());
        }
    }

    Ok(creator_claims)
}

/// Best-effort parse of `owner` and `repo` from a git URL for use as project
/// labels in the public-repo (one-click) deploy path.
///
/// These are NOT validated against any Git connection — the actual clone uses
/// the full `git_url`. They only need to be non-empty so the deploy pipeline
/// plans the download job and queues the initial deploy. Handles
/// `https://host/owner/repo(.git)` and `git@host:owner/repo(.git)` shapes;
/// falls back to `("template", "<repo-or-app>")` so both fields are always set.
pub fn parse_owner_repo_from_git_url(git_url: &str) -> (String, String) {
    // Normalize: strip scheme, an optional `git@host:` prefix, and `.git`.
    let trimmed = git_url.trim();
    let without_scheme = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    // For SCP-style `git@host:owner/repo`, drop everything up to and including ':'.
    let path_part = without_scheme
        .rsplit_once(':')
        .map(|(_, rest)| rest)
        .unwrap_or(without_scheme);
    let path_part = path_part.trim_end_matches('/');
    let path_part = path_part.strip_suffix(".git").unwrap_or(path_part);

    let mut segments = path_part.rsplit('/');
    let repo = segments.next().filter(|s| !s.is_empty());
    let owner = segments.next().filter(|s| !s.is_empty());

    match (owner, repo) {
        (Some(o), Some(r)) => (o.to_string(), r.to_string()),
        (None, Some(r)) => ("template".to_string(), r.to_string()),
        _ => ("template".to_string(), "app".to_string()),
    }
}

pub fn project_created_from_template_telemetry_event(
    template_slug: &str,
    deploy_mode: &'static str,
    service_count: usize,
) -> temps_core::telemetry::TelemetryEvent {
    let safe_slug = temps_core::templates::telemetry_safe_template_slug(template_slug);
    temps_core::telemetry::TelemetryEvent::new(
        temps_core::telemetry::TelemetryEventKind::ProjectCreatedFromTemplate,
    )
    .with(
        "template_source",
        if safe_slug.is_some() {
            "bundled"
        } else {
            "custom"
        },
    )
    .with_opt("template_slug", safe_slug.map(str::to_string))
    .with("deploy_mode", deploy_mode)
    .with("service_count", service_count as i64)
}

pub fn image_deployment_dispatch_feedback(queued: bool) -> (Option<bool>, Option<String>) {
    if queued {
        (Some(true), None)
    } else {
        (
            Some(false),
            Some(
                "The project was created, but its initial deployment could not be queued. Open the project and select Deploy to retry."
                    .to_string(),
            ),
        )
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum TemplateServiceSelectionError {
    Missing(Vec<String>),
    Ambiguous(Vec<String>),
}

#[derive(Debug, PartialEq, Eq)]
pub struct EffectiveImageTemplateRuntime {
    pub image_ref: String,
    pub command: Option<Vec<String>>,
    pub cpu_request: Option<i32>,
    pub cpu_limit: Option<i32>,
    pub memory_request: Option<i32>,
    pub memory_limit: Option<i32>,
    pub exposed_port: Option<i32>,
    pub health_check_path: Option<String>,
}

pub fn image_template_preset_config(
    service: &temps_core::templates::TemplateServiceSpec,
    runtime: &EffectiveImageTemplateRuntime,
) -> Result<serde_json::Value, TemplateRuntimeOverrideError> {
    let mut config = match service.preset_config.as_ref() {
        Some(value) => temps_entities::preset::PresetConfig::parse_for_preset(
            &temps_entities::preset::Preset::Dockerfile,
            value,
        )
        .map_err(|reason| TemplateRuntimeOverrideError::InvalidImage { reason })?,
        None => temps_entities::preset::PresetConfig::default_for_preset(
            temps_entities::preset::Preset::Dockerfile,
        ),
    };
    let temps_entities::preset::PresetConfig::Dockerfile(config) = &mut config else {
        return Err(TemplateRuntimeOverrideError::InvalidImage {
            reason: "image templates must use the dockerfile runtime preset".to_string(),
        });
    };
    config.image_runtime = Some(temps_entities::preset::ImageRuntimeConfig {
        image_ref: runtime.image_ref.clone(),
        command: runtime.command.clone(),
        health_check_path: runtime.health_check_path.clone(),
    });
    serde_json::to_value(config).map_err(|error| TemplateRuntimeOverrideError::InvalidImage {
        reason: format!("could not store the selected runtime: {error}"),
    })
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TemplateRuntimeOverrideError {
    #[error("Runtime overrides are available only for templates that deploy a prebuilt image")]
    NotAnImageTemplate,
    #[error("Invalid image reference: {reason}")]
    InvalidImage { reason: String },
    #[error("Invalid container command: {reason}")]
    InvalidCommand { reason: String },
    #[error("Invalid health-check path: {reason}")]
    InvalidHealthCheckPath { reason: String },
}

pub fn has_template_runtime_overrides(request: &TemplateRuntimeOverrides) -> bool {
    request.image.is_some()
        || request.command.is_some()
        || request.cpu_request.is_some()
        || request.cpu_limit.is_some()
        || request.memory_request.is_some()
        || request.memory_limit.is_some()
        || request.exposed_port.is_some()
        || request.health_check_path.is_some()
}

pub fn resolve_image_template_runtime(
    template: &temps_core::templates::TemplateServiceSpec,
    request: &TemplateRuntimeOverrides,
) -> Result<Option<EffectiveImageTemplateRuntime>, TemplateRuntimeOverrideError> {
    let Some(template_image) = template.image.as_deref().filter(|image| !image.is_empty()) else {
        if has_template_runtime_overrides(request) {
            return Err(TemplateRuntimeOverrideError::NotAnImageTemplate);
        }
        return Ok(None);
    };

    let image_ref = request.image.as_deref().unwrap_or(template_image).trim();
    if image_ref.is_empty() {
        return Err(TemplateRuntimeOverrideError::InvalidImage {
            reason: "the image reference cannot be empty".to_string(),
        });
    }
    if image_ref.len() > 512 {
        return Err(TemplateRuntimeOverrideError::InvalidImage {
            reason: "the image reference cannot exceed 512 bytes".to_string(),
        });
    }
    if image_ref
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(TemplateRuntimeOverrideError::InvalidImage {
            reason: "the image reference cannot contain whitespace or control characters"
                .to_string(),
        });
    }
    if !temps_core::templates::is_pinned_image_reference(image_ref) {
        return Err(TemplateRuntimeOverrideError::InvalidImage {
            reason: "use an immutable image reference ending in @sha256:<64 hex characters>"
                .to_string(),
        });
    }

    let command = match request.command.as_ref() {
        Some(parts) if parts.is_empty() => None,
        Some(parts) => {
            if parts.len() > 64 {
                return Err(TemplateRuntimeOverrideError::InvalidCommand {
                    reason: "at most 64 arguments are supported".to_string(),
                });
            }
            let normalized = parts
                .iter()
                .map(|part| part.trim().to_string())
                .collect::<Vec<_>>();
            if normalized.iter().any(|part| {
                part.is_empty() || part.len() > 1_024 || part.chars().any(char::is_control)
            }) {
                return Err(TemplateRuntimeOverrideError::InvalidCommand {
                    reason: "arguments must be non-empty, at most 1024 bytes, and contain no control characters"
                        .to_string(),
                });
            }
            Some(normalized)
        }
        None => template.command.clone(),
    };

    let health_check_path = request
        .health_check_path
        .as_deref()
        .or(template.health_check_path.as_deref())
        .map(str::trim)
        .map(str::to_string);
    if let Some(path) = health_check_path.as_deref() {
        if path.len() > 2_048
            || !path.starts_with('/')
            || path.contains('@')
            || path.contains("://")
            || path.chars().any(char::is_control)
        {
            return Err(TemplateRuntimeOverrideError::InvalidHealthCheckPath {
                reason: "use a safe relative HTTP path starting with '/'".to_string(),
            });
        }
    }

    Ok(Some(EffectiveImageTemplateRuntime {
        image_ref: image_ref.to_string(),
        command,
        cpu_request: request.cpu_request.or_else(|| {
            template
                .resources
                .as_ref()
                .and_then(|resources| resources.cpu_request)
        }),
        cpu_limit: request.cpu_limit.or_else(|| {
            template
                .resources
                .as_ref()
                .and_then(|resources| resources.cpu_limit)
        }),
        memory_request: request.memory_request.or_else(|| {
            template
                .resources
                .as_ref()
                .and_then(|resources| resources.memory_request)
        }),
        memory_limit: request.memory_limit.or_else(|| {
            template
                .resources
                .as_ref()
                .and_then(|resources| resources.memory_limit)
        }),
        exposed_port: request.exposed_port.or(template.exposed_port),
        health_check_path,
    }))
}

pub fn validate_template_service_selection(
    required_services: &[String],
    selected_service_types: &[String],
) -> Result<(), TemplateServiceSelectionError> {
    let missing = required_services
        .iter()
        .filter(|required| {
            !selected_service_types.iter().any(|selected| {
                temps_core::templates::managed_service_types_compatible(required, selected)
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(TemplateServiceSelectionError::Missing(missing));
    }

    let ambiguous = required_services
        .iter()
        .filter(|required| {
            selected_service_types
                .iter()
                .filter(|selected| {
                    temps_core::templates::managed_service_types_compatible(required, selected)
                })
                .count()
                > 1
        })
        .cloned()
        .collect::<Vec<_>>();
    if !ambiguous.is_empty() {
        return Err(TemplateServiceSelectionError::Ambiguous(ambiguous));
    }

    Ok(())
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TemplateEnvironmentError {
    #[error("Environment variable '{name}' is defined more than once")]
    Duplicate { name: String },
    #[error("Required environment variable '{name}' has no value")]
    MissingRequired { name: String },
    #[error("Environment variable '{name}' uses unsupported generator '{generator}'")]
    UnsupportedGenerator { name: String, generator: String },
    #[error("Could not securely generate environment variable '{name}': {reason}")]
    SecureGeneration { name: String, reason: String },
    #[error(
        "Environment variable '{name}' is already configured; edit it in project settings instead"
    )]
    AlreadyConfigured { name: String },
    #[error("Environment variable '{name}' is not declared by the target template")]
    Unknown { name: String },
}

pub fn require_template_creation_permissions(auth: &AuthContext) -> Result<(), Problem> {
    permission_guard!(auth, ProjectsCreate);
    permission_guard!(auth, DeploymentsCreate);
    Ok(())
}

pub fn template_environment_variable_is_secret(
    variable: &temps_core::templates::EnvVarTemplate,
) -> bool {
    variable.is_secret()
}

pub fn secure_template_value(
    variable_name: &str,
    generator: &str,
) -> Result<String, TemplateEnvironmentError> {
    let mut bytes = [0_u8; 32];
    rand::TryRng::try_fill_bytes(&mut rand::rngs::SysRng, &mut bytes).map_err(|error| {
        TemplateEnvironmentError::SecureGeneration {
            name: variable_name.to_string(),
            reason: error.to_string(),
        }
    })?;

    match generator {
        "random_secret" => Ok(URL_SAFE_NO_PAD.encode(bytes)),
        "random_hex_32" => Ok(hex::encode(bytes)),
        _ => Err(TemplateEnvironmentError::UnsupportedGenerator {
            name: variable_name.to_string(),
            generator: generator.to_string(),
        }),
    }
}

pub fn canonicalize_template_environment_variables(
    variables: &[temps_core::templates::EnvVarTemplate],
    requested: &[EnvVarInput],
    generated_app_url: Option<&str>,
) -> Result<Vec<CreateProjectEnvVar>, TemplateEnvironmentError> {
    let mut requested_by_name = BTreeMap::new();
    for variable in requested {
        if requested_by_name
            .insert(
                variable.name.clone(),
                (variable.value.clone(), variable.is_secret),
            )
            .is_some()
        {
            return Err(TemplateEnvironmentError::Duplicate {
                name: variable.name.clone(),
            });
        }
    }

    let mut resolved = Vec::with_capacity(variables.len() + requested_by_name.len());
    for variable in variables {
        let requested_value = requested_by_name.remove(&variable.name);
        let explicitly_supplied = requested_value.is_some();
        let requested_secret = requested_value
            .as_ref()
            .is_some_and(|(_, is_secret)| *is_secret);
        let mut value = requested_value.map(|(value, _)| value);

        if value.as_deref().is_none_or(str::is_empty) {
            value = variable.default.clone();
        }
        if value.as_deref().is_none_or(str::is_empty) {
            value = match variable.default_generator.as_deref() {
                Some("app_url") => generated_app_url.map(str::to_string),
                Some(generator @ ("random_secret" | "random_hex_32")) => {
                    Some(secure_template_value(&variable.name, generator)?)
                }
                Some(generator) => {
                    return Err(TemplateEnvironmentError::UnsupportedGenerator {
                        name: variable.name.clone(),
                        generator: generator.to_string(),
                    });
                }
                None => None,
            };
        }

        if variable.required && value.as_deref().is_none_or(str::is_empty) {
            return Err(TemplateEnvironmentError::MissingRequired {
                name: variable.name.clone(),
            });
        }
        if let Some(value) = value {
            if !value.is_empty() || explicitly_supplied {
                resolved.push(CreateProjectEnvVar {
                    key: variable.name.clone(),
                    value,
                    is_secret: requested_secret
                        || template_environment_variable_is_secret(variable),
                });
            }
        }
    }

    resolved.extend(
        requested_by_name
            .into_iter()
            .map(|(key, (value, is_secret))| CreateProjectEnvVar {
                key,
                value,
                is_secret,
            }),
    );
    Ok(resolved)
}

pub fn canonicalize_template_upgrade_environment_variables(
    variables: &[temps_core::templates::EnvVarTemplate],
    requested: &[EnvVarInput],
    configured_names: &BTreeSet<String>,
    generated_app_url: Option<&str>,
) -> Result<Vec<CreateProjectEnvVar>, TemplateEnvironmentError> {
    let mut requested_by_name = BTreeMap::new();
    for variable in requested {
        if configured_names.contains(&variable.name) {
            return Err(TemplateEnvironmentError::AlreadyConfigured {
                name: variable.name.clone(),
            });
        }
        if requested_by_name
            .insert(
                variable.name.clone(),
                (variable.value.clone(), variable.is_secret),
            )
            .is_some()
        {
            return Err(TemplateEnvironmentError::Duplicate {
                name: variable.name.clone(),
            });
        }
    }

    let mut resolved = Vec::new();
    for variable in variables {
        if configured_names.contains(&variable.name) {
            continue;
        }
        let requested_value = requested_by_name.remove(&variable.name);
        let explicitly_supplied = requested_value.is_some();
        let requested_secret = requested_value
            .as_ref()
            .is_some_and(|(_, is_secret)| *is_secret);
        let mut value = requested_value.map(|(value, _)| value);

        if value.as_deref().is_none_or(str::is_empty) {
            value = variable.default.clone();
        }
        if value.as_deref().is_none_or(str::is_empty) {
            value = match variable.default_generator.as_deref() {
                Some("app_url") => generated_app_url.map(str::to_string),
                Some(generator @ ("random_secret" | "random_hex_32")) => {
                    Some(secure_template_value(&variable.name, generator)?)
                }
                Some(generator) => {
                    return Err(TemplateEnvironmentError::UnsupportedGenerator {
                        name: variable.name.clone(),
                        generator: generator.to_string(),
                    });
                }
                None => None,
            };
        }

        if variable.required && value.as_deref().is_none_or(str::is_empty) {
            return Err(TemplateEnvironmentError::MissingRequired {
                name: variable.name.clone(),
            });
        }
        if let Some(value) = value {
            if !value.is_empty() || explicitly_supplied {
                resolved.push(CreateProjectEnvVar {
                    key: variable.name.clone(),
                    value,
                    is_secret: requested_secret || variable.is_secret(),
                });
            }
        }
    }

    if let Some((name, _)) = requested_by_name.into_iter().next() {
        return Err(TemplateEnvironmentError::Unknown { name });
    }
    Ok(resolved)
}

pub fn missing_required_template_configuration(
    variables: &[temps_core::templates::EnvVarTemplate],
    configured_names: &BTreeSet<String>,
) -> Vec<temps_core::templates::EnvVarTemplate> {
    variables
        .iter()
        .filter(|variable| {
            variable.required
                && !configured_names.contains(&variable.name)
                && variable.default.as_deref().is_none_or(str::is_empty)
                && variable.default_generator.is_none()
        })
        .cloned()
        .collect()
}

pub fn upgrade_needs_generated_app_url(
    variables: &[temps_core::templates::EnvVarTemplate],
    requested: &[EnvVarInput],
    configured_names: &BTreeSet<String>,
) -> bool {
    variables.iter().any(|variable| {
        !configured_names.contains(&variable.name)
            && variable.default_generator.as_deref() == Some("app_url")
            && requested
                .iter()
                .find(|requested| requested.name == variable.name)
                .is_none_or(|requested| requested.value.is_empty())
            && variable.default.as_deref().is_none_or(str::is_empty)
    })
}

pub fn template_needs_generated_app_url(
    variables: &[temps_core::templates::EnvVarTemplate],
    requested: &[EnvVarInput],
) -> bool {
    variables.iter().any(|variable| {
        variable.default_generator.as_deref() == Some("app_url")
            && requested
                .iter()
                .find(|requested| requested.name == variable.name)
                .is_none_or(|requested| requested.value.is_empty())
            && variable.default.as_deref().is_none_or(str::is_empty)
    })
}

pub async fn template_app_url(
    config_service: &temps_config::ConfigService,
    public_hostname_resolver: &dyn temps_core::PublicHostnameResolver,
    project_slug: &str,
) -> Result<String, Problem> {
    let settings = config_service.get_settings().await.map_err(|error| {
        error!(%error, "Failed to load hostname settings for native template");
        temps_core::error_builder::internal_server_error()
            .title("Template URL Generation Failed")
            .detail("Could not load the platform hostname settings")
            .build()
    })?;
    let preview_domain = if settings.preview_domain.trim().is_empty() {
        "localho.st"
    } else {
        settings.preview_domain.trim()
    };
    let strategy = public_hostname_resolver.strategy_for(preview_domain).await;
    let (scheme, port) = settings
        .external_url
        .as_deref()
        .and_then(|value| url::Url::parse(value).ok())
        .map(|url| (url.scheme().to_string(), url.port()))
        .unwrap_or_else(|| ("http".to_string(), Some(config_service.proxy_port())));
    let hostname =
        strategy.environment_hostname(preview_domain, &format!("{project_slug}-production"));
    let port_suffix = port
        .filter(|port| !((scheme == "http" && *port == 80) || (scheme == "https" && *port == 443)))
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    Ok(format!("{scheme}://{hostname}{port_suffix}"))
}

/// Where a service that builds from source reads its code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RepositoryTarget {
    pub repo_owner: String,
    pub repo_name: String,
    pub branch: String,
    /// Clone URL; set for public repositories and forks.
    pub git_url: Option<String>,
    /// Connection the repository is read through; `None` when public.
    pub git_provider_connection_id: Option<i32>,
    pub is_public: bool,
    /// Directory of the service inside the repository.
    pub directory: String,
    /// Deploy on push. Meaningless for public repositories, which cannot send
    /// push webhooks.
    pub automatic_deploy: bool,
}

/// One service of a template, ready to create.
#[derive(Debug, Clone)]
pub struct TemplateServiceRequest {
    pub template: ProjectTemplate,
    pub service: TemplateServiceSpec,
    /// The app release the service carries; `None` for starters.
    pub release: Option<ServiceTemplateInstance>,
    pub project_name: String,
    /// Slug planned (and preflight-checked) before any side effect.
    pub project_slug: String,
    /// Bounded provenance stored on the project (see
    /// [`temps_core::templates::template_provenance`]); `None` for services
    /// that did not come from a catalog template, such as imported apps.
    pub provenance: Option<String>,
    /// Values supplied by the caller.
    pub variables: Vec<EnvVarInput>,
    /// Declared variables whose defaults and generators are resolved here.
    pub variable_templates: Vec<EnvVarTemplate>,
    pub storage_service_ids: Vec<i32>,
    pub storage_service_claim_ids: Vec<i32>,
    /// Source for a service that builds from source; unused for image
    /// services.
    pub repository: Option<RepositoryTarget>,
    pub runtime_overrides: TemplateRuntimeOverrides,
    /// Hold the first deployment until [`TemplateServiceCreator::queue_first_deployment`].
    pub defer_initial_deployment: bool,
    /// Ownership marker of the instantiation creating this service, built
    /// with [`temps_entities::projects::instantiation_token`]. Stored in the
    /// same `INSERT` as the project, so a resumed instantiation can recognise
    /// the project as its own. `None` outside an instantiation.
    pub instantiation_token: Option<String>,
    /// Label for telemetry and messages: `image`, `fork`, `public_repo` or
    /// `repository`.
    pub deploy_mode: &'static str,
    /// Legacy Cloudflare opt-in/out for the created project; `None` defers
    /// to `delivery_provider` and the instance default.
    pub cloudflare_enabled: Option<bool>,
    /// CDN delivery provider (`none`, `cloudflare`, `bunny`) the project
    /// starts with; `None` inherits the instance default.
    pub delivery_provider: Option<String>,
}

/// A service created from a template.
pub struct CreatedTemplateService {
    pub project: Project,
    pub deploy_mode: &'static str,
    /// Set for image services whose first deployment was queued (or failed
    /// to queue) during creation.
    pub deployment_queued: Option<bool>,
    pub deployment_error: Option<String>,
}

/// Who is creating: the audit identity and what their role lets them claim.
pub struct TemplateCreationCaller<'a> {
    pub auth: &'a AuthContext,
    pub slug_claim: &'a SlugClaimCaller<'a>,
    pub ip_address: Option<String>,
    pub user_agent: String,
}

/// Creates services from templates; shared by the single-service endpoint
/// and the instantiation engine so both follow the same rules.
#[derive(Clone)]
pub struct TemplateServiceCreator {
    pub project_service: Arc<ProjectService>,
    pub config_service: Arc<temps_config::ConfigService>,
    pub public_hostname_resolver: Arc<dyn temps_core::PublicHostnameResolver>,
    pub audit_service: Arc<dyn temps_core::AuditLogger>,
    pub telemetry: Arc<dyn temps_core::telemetry::TelemetryReporter>,
}

impl TemplateServiceCreator {
    /// Resolve variables, create the project and, for an image service whose
    /// deployment is not deferred, queue its first deployment.
    pub async fn create(
        &self,
        request: TemplateServiceRequest,
        caller: &TemplateCreationCaller<'_>,
    ) -> Result<CreatedTemplateService, Problem> {
        let service = &request.service;
        let runtime = resolve_image_template_runtime(service, &request.runtime_overrides).map_err(
            |error| {
                temps_core::error_builder::bad_request()
                    .title("Invalid Template Runtime Configuration")
                    .detail(error.to_string())
                    .build()
            },
        )?;

        let generated_app_url =
            if template_needs_generated_app_url(&request.variable_templates, &request.variables) {
                Some(
                    template_app_url(
                        &self.config_service,
                        self.public_hostname_resolver.as_ref(),
                        &request.project_slug,
                    )
                    .await?,
                )
            } else {
                None
            };
        let env_vars = canonicalize_template_environment_variables(
            &request.variable_templates,
            &request.variables,
            generated_app_url.as_deref(),
        )
        .map_err(template_environment_problem)?;
        let env_vars = (!env_vars.is_empty()).then_some(env_vars);

        let create_request = match (&runtime, &request.repository) {
            (Some(runtime), _) => {
                let preset_config =
                    image_template_preset_config(service, runtime).map_err(|error| {
                        temps_core::error_builder::bad_request()
                            .title("Invalid Template Runtime Configuration")
                            .detail(error.to_string())
                            .build()
                    })?;
                CreateProjectRequest {
                    cloudflare_enabled: request.cloudflare_enabled,
                    delivery_provider: request.delivery_provider.clone(),
                    name: request.project_name.clone(),
                    expected_slug: Some(request.project_slug.clone()),
                    // No Git source — the image is pulled from its registry.
                    repo_name: None,
                    repo_owner: None,
                    directory: ".".to_string(),
                    main_branch: service.source(&request.template.git).r#ref,
                    preset: service.preset.clone(),
                    preset_config: Some(preset_config),
                    environment_variables: env_vars,
                    automatic_deploy: false,
                    storage_service_ids: request.storage_service_ids.clone(),
                    storage_service_claim_ids: request.storage_service_claim_ids.clone(),
                    storage_service_claim_user_id: Some(caller.auth.user_id()),
                    is_public_repo: None,
                    git_url: None,
                    git_provider_connection_id: None,
                    exposed_port: runtime.exposed_port,
                    cpu_request: runtime.cpu_request,
                    cpu_limit: runtime.cpu_limit,
                    memory_request: runtime.memory_request,
                    memory_limit: runtime.memory_limit,
                    // docker_image source skips the build pipeline entirely;
                    // the deploy is queued explicitly.
                    source_type: SourceType::DockerImage,
                    template_slug: request.provenance.clone(),
                    hosting: Default::default(),
                }
            }
            (None, Some(repository)) => {
                let resources = service.resources.clone().unwrap_or_default();
                CreateProjectRequest {
                    cloudflare_enabled: request.cloudflare_enabled,
                    delivery_provider: request.delivery_provider.clone(),
                    name: request.project_name.clone(),
                    expected_slug: Some(request.project_slug.clone()),
                    repo_name: Some(repository.repo_name.clone()),
                    repo_owner: Some(repository.repo_owner.clone()),
                    directory: repository.directory.clone(),
                    main_branch: repository.branch.clone(),
                    preset: service.preset.clone(),
                    preset_config: service.preset_config.clone(),
                    environment_variables: env_vars,
                    automatic_deploy: repository_automatic_deploy(repository),
                    storage_service_ids: request.storage_service_ids.clone(),
                    storage_service_claim_ids: request.storage_service_claim_ids.clone(),
                    storage_service_claim_user_id: Some(caller.auth.user_id()),
                    is_public_repo: Some(repository.is_public),
                    git_url: repository.git_url.clone(),
                    git_provider_connection_id: repository.git_provider_connection_id,
                    exposed_port: service.exposed_port,
                    cpu_request: resources.cpu_request,
                    cpu_limit: resources.cpu_limit,
                    memory_request: resources.memory_request,
                    memory_limit: resources.memory_limit,
                    source_type: SourceType::Git,
                    template_slug: request.provenance.clone(),
                    hosting: Default::default(),
                }
            }
            (None, None) => {
                return Err(temps_core::error_builder::bad_request()
                    .title("Repository Required")
                    .detail(format!(
                        "Service '{}' builds from source, so it needs a repository",
                        service.name
                    ))
                    .build());
            }
        };

        let project = self
            .project_service
            .create_template_service_as(
                create_request,
                request.release.clone(),
                caller.slug_claim,
                request.defer_initial_deployment,
                request.instantiation_token.clone(),
            )
            .await
            .inspect_err(|error| {
                crate::handlers::log_reserved_slug_refusal(error, caller.auth.user_id(), None)
            })
            .map_err(Problem::from)?;

        let (mut deployment_queued, mut deployment_error) = (None, None);
        if let Some(runtime) = runtime.filter(|_| !request.defer_initial_deployment) {
            let queued = self
                .queue_image_deployment(project.id, runtime, caller.auth.is_instance_admin())
                .await;
            (deployment_queued, deployment_error) = image_deployment_dispatch_feedback(queued);
        }

        self.record_creation(&project, &request, caller).await;
        Ok(CreatedTemplateService {
            project,
            deploy_mode: request.deploy_mode,
            deployment_queued,
            deployment_error,
        })
    }

    /// Queue the first deployment of a service created with
    /// `defer_initial_deployment`.
    pub async fn queue_first_deployment(
        &self,
        project_id: i32,
        docker_socket_authorized: bool,
    ) -> Result<(), Problem> {
        let project = self
            .project_service
            .get_project(project_id)
            .await
            .map_err(Problem::from)?;
        let image_runtime = project
            .preset_config
            .as_ref()
            .and_then(|value| {
                serde_json::from_value::<temps_entities::preset::PresetConfig>(value.clone()).ok()
            })
            .and_then(|config| match config {
                temps_entities::preset::PresetConfig::Dockerfile(config) => config.image_runtime,
                _ => None,
            });
        match image_runtime {
            Some(runtime) if project.source_type == SourceType::DockerImage => {
                let queued = self
                    .queue_image_deployment(
                        project.id,
                        EffectiveImageTemplateRuntime {
                            image_ref: runtime.image_ref,
                            command: runtime.command,
                            cpu_request: None,
                            cpu_limit: None,
                            memory_request: None,
                            memory_limit: None,
                            exposed_port: None,
                            health_check_path: runtime.health_check_path,
                        },
                        docker_socket_authorized,
                    )
                    .await;
                if queued {
                    Ok(())
                } else {
                    Err(temps_core::error_builder::internal_server_error()
                        .title("Deployment Not Queued")
                        .detail(format!(
                            "{} was created, but its first deployment could not be queued. Open it and select Deploy.",
                            project.name
                        ))
                        .build())
                }
            }
            _ => self
                .project_service
                .queue_initial_deployment(project.id)
                .await
                .map_err(Problem::from),
        }
    }

    async fn queue_image_deployment(
        &self,
        project_id: i32,
        runtime: EffectiveImageTemplateRuntime,
        docker_socket_authorized: bool,
    ) -> bool {
        let deploy_job =
            temps_core::Job::DeployImageRequested(temps_core::DeployImageRequestedJob {
                project_id,
                target_environment_id: None,
                image_ref: runtime.image_ref.clone(),
                health_check_path: runtime.health_check_path,
                command: runtime.command,
                recovery_of_deployment_id: None,
                // ADR 045: claiming a declared slug is already instance-admin-only
                // with step-up, so the first deploy of a project that admin just
                // created must not be refused by the planner.
                docker_socket_authorized,
            });
        match self.project_service.queue_service.send(deploy_job).await {
            Ok(()) => {
                info!(project_id, image = %runtime.image_ref, "Queued image deploy for template service");
                true
            }
            Err(error) => {
                error!(project_id, image = %runtime.image_ref, %error, "Failed to queue image deploy for template service");
                false
            }
        }
    }

    async fn record_creation(
        &self,
        project: &Project,
        request: &TemplateServiceRequest,
        caller: &TemplateCreationCaller<'_>,
    ) {
        let audit_event = crate::handlers::audit::ProjectCreatedAudit {
            context: crate::handlers::audit::AuditContext {
                user_id: caller.auth.user_id(),
                ip_address: caller.ip_address.clone(),
                user_agent: caller.user_agent.clone(),
            },
            project_id: project.id,
            project_name: project.name.clone(),
            project_slug: project.slug.clone(),
            repo_name: project.repo_name.clone(),
            repo_owner: project.repo_owner.clone(),
            directory: project.directory.clone(),
            main_branch: project.main_branch.clone(),
            preset: project.preset.clone(),
            automatic_deploy: project.automatic_deploy,
        };
        if let Err(error) = self.audit_service.create_audit_log(&audit_event).await {
            error!(
                project_id = project.id,
                "Failed to create audit log: {error:?}"
            );
        }

        // Anonymous telemetry: the generic project-created event, so the
        // template path counts like any other creation, and a template event
        // carrying only the public, non-identifying slug and deploy mode.
        self.telemetry.report(
            temps_core::telemetry::TelemetryEvent::new(
                temps_core::telemetry::TelemetryEventKind::ProjectCreated,
            )
            .with("source_type", project.source_type.to_string())
            .with_opt("preset", project.preset.clone()),
        );
        if let Some(provenance) = &request.provenance {
            self.telemetry
                .report(project_created_from_template_telemetry_event(
                    provenance,
                    request.deploy_mode,
                    request.storage_service_ids.len(),
                ));
        }
    }
}

/// Whether pushes to a template's repository deploy the project.
///
/// Only a repository the project reaches through a Git connection can send
/// push webhooks, so the unconnected public-upstream mode never deploys on
/// push. Visibility is irrelevant: a public repository created through an
/// authenticated connection (a fork or a new repository the caller owns)
/// keeps the caller's choice.
pub fn repository_automatic_deploy(repository: &RepositoryTarget) -> bool {
    repository.automatic_deploy && repository.git_provider_connection_id.is_some()
}

/// Check that `requested` is acceptable for `variables` without creating
/// anything, so callers can fail before an irreversible side effect (such as
/// creating a remote repository) instead of after it.
///
/// This is the same resolution [`TemplateServiceCreator::create`] performs,
/// so a request accepted here is not rejected for its variables later. The
/// resolved values are discarded: generated secrets are produced again, and
/// `app_url` is stood in for by a placeholder because the real address needs
/// the planned project slug.
pub fn validate_template_variables(
    variables: &[EnvVarTemplate],
    requested: &[EnvVarInput],
) -> Result<(), Problem> {
    canonicalize_template_environment_variables(
        variables,
        requested,
        Some("https://app-url-placeholder.invalid"),
    )
    .map(|_| ())
    .map_err(template_environment_problem)
}

fn template_environment_problem(error: TemplateEnvironmentError) -> Problem {
    if matches!(&error, TemplateEnvironmentError::SecureGeneration { .. }) {
        error!(%error, "Secure template environment generation failed");
        temps_core::error_builder::internal_server_error()
            .title("Template Secret Generation Failed")
            .detail("Could not securely generate the required template credentials")
            .build()
    } else {
        temps_core::error_builder::bad_request()
            .title("Invalid Template Environment")
            .detail(error.to_string())
            .build()
    }
}

/// The source of a service built from a template's public repository: the
/// whole repository is cloned and the service builds from its directory.
pub fn public_repository_target(source: &temps_core::templates::GitRef) -> RepositoryTarget {
    let (repo_owner, repo_name) = parse_owner_repo_from_git_url(&source.url);
    RepositoryTarget {
        repo_owner,
        repo_name,
        branch: source.r#ref.clone(),
        git_url: Some(source.url.clone()),
        git_provider_connection_id: None,
        is_public: true,
        directory: source
            .path
            .clone()
            .filter(|path| !path.is_empty())
            .unwrap_or_else(|| ".".to_string()),
        automatic_deploy: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variable(name: &str, required: bool) -> EnvVarTemplate {
        EnvVarTemplate {
            name: name.to_string(),
            example: None,
            default: None,
            description: None,
            required,
            secret: false,
            default_generator: None,
        }
    }

    fn input(name: &str, value: &str) -> EnvVarInput {
        EnvVarInput {
            name: name.to_string(),
            value: value.to_string(),
            is_secret: false,
        }
    }

    fn connected_repository(is_public: bool, automatic_deploy: bool) -> RepositoryTarget {
        RepositoryTarget {
            repo_owner: "owner".to_string(),
            repo_name: "repo".to_string(),
            branch: "main".to_string(),
            git_url: Some("https://github.com/owner/repo.git".to_string()),
            git_provider_connection_id: Some(3),
            is_public,
            directory: ".".to_string(),
            automatic_deploy,
        }
    }

    #[test]
    fn a_public_repository_created_through_a_connection_keeps_automatic_deploy() {
        for is_public in [true, false] {
            assert!(
                repository_automatic_deploy(&connected_repository(is_public, true)),
                "is_public={is_public}: the caller's choice must survive"
            );
            assert!(
                !repository_automatic_deploy(&connected_repository(is_public, false)),
                "is_public={is_public}: an explicit opt-out must survive"
            );
        }
    }

    #[test]
    fn the_unconnected_public_upstream_never_deploys_on_push() {
        let upstream = public_repository_target(&temps_core::templates::GitRef {
            url: "https://github.com/example/starter.git".to_string(),
            r#ref: "main".to_string(),
            path: Some("apps/web".to_string()),
        });
        assert!(!upstream.automatic_deploy);
        assert!(!repository_automatic_deploy(&upstream));

        // Even if a caller asks for it, a repository with no connection cannot
        // send push webhooks.
        let mut requested = upstream;
        requested.automatic_deploy = true;
        assert!(!repository_automatic_deploy(&requested));
    }

    #[test]
    fn variables_are_validated_without_creating_anything() {
        let variables = vec![
            variable("API_TOKEN", true),
            EnvVarTemplate {
                default: Some("info".to_string()),
                ..variable("LOG_LEVEL", true)
            },
            EnvVarTemplate {
                default_generator: Some("app_url".to_string()),
                ..variable("PUBLIC_URL", true)
            },
        ];

        assert!(validate_template_variables(&variables, &[input("API_TOKEN", "x")]).is_ok());

        let missing = validate_template_variables(&variables, &[])
            .expect_err("a required variable without a value is rejected");
        assert_eq!(missing.status_code, StatusCode::BAD_REQUEST);
        let detail = missing
            .body
            .get("detail")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        assert!(detail.contains("API_TOKEN"), "{detail}");

        let duplicate = validate_template_variables(
            &variables,
            &[input("API_TOKEN", "one"), input("API_TOKEN", "two")],
        )
        .expect_err("a variable supplied twice is rejected");
        assert_eq!(duplicate.status_code, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn the_handler_validates_variables_before_it_creates_a_repository() {
        let source = include_str!("handlers/handlers.rs");
        let start = source
            .find("pub async fn create_project_from_template(")
            .expect("handler exists");
        let body = &source[start..];
        let validate = body
            .find("validate_template_variables(")
            .expect("the handler validates variables");
        let create = body
            .find("create_repository_and_push_template(")
            .expect("the handler creates the fork repository");
        assert!(
            validate < create,
            "variables must be validated before the irreversible repository creation"
        );
    }
}
