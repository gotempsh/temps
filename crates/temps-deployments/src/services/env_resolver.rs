// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared resolver for a container's environment variables, keyed on the
//! *selected environment*.
//!
//! Every deploy path — the normal pipeline (via [`WorkflowPlanner`]), a
//! promotion, and a rollback — resolves env through this one place, so a
//! container is NEVER created without its environment's fully-resolved set:
//! user-defined vars, external-service runtime vars (DB/Redis/… connection
//! strings), `SENTRY_DSN` / `SENTRY_TUNNEL`, `TEMPS_API_URL` /
//! `TEMPS_API_TOKEN`, `CRON_SECRET`,
//! and the `OTEL_EXPORTER_OTLP_*` instrumentation vars.
//!
//! [`WorkflowPlanner`]: super::workflow_planner::WorkflowPlanner

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use sea_orm::{ColumnTrait, DatabaseConnection, DbErr, EntityTrait, QueryFilter};
use temps_core::{EncryptionService, SecretsManagerResolver};
use temps_entities::{
    deployments, env_var_environments, env_vars, environments, preset::Preset, projects,
};
use thiserror::Error;
use tracing::{debug, info};

use super::deployment_token_service::DeploymentTokenService;
use super::managed_environment_variables::{public_sentry_dsn_var, public_sentry_tunnel_var};
use super::workflow_planner::SecretsResolverSlot;

#[derive(Debug, Error)]
pub enum DeploymentEnvResolutionError {
    #[error(
        "Failed to query environment-variable bindings for project {project_id}, environment {environment_id}: {source}"
    )]
    EnvironmentVariableBindingsQuery {
        project_id: i32,
        environment_id: i32,
        #[source]
        source: DbErr,
    },

    #[error(
        "Failed to query environment variables for project {project_id}, environment {environment_id}: {source}"
    )]
    EnvironmentVariablesQuery {
        project_id: i32,
        environment_id: i32,
        #[source]
        source: DbErr,
    },

    #[error(
        "Failed to decrypt environment variable '{key}' (id {variable_id}) for project {project_id}, environment {environment_id}: {reason}"
    )]
    EnvironmentVariableDecryption {
        project_id: i32,
        environment_id: i32,
        variable_id: i32,
        key: String,
        reason: String,
    },

    #[error(
        "Failed to query linked services for project {project_id}, environment {environment_id}: {source}"
    )]
    LinkedServicesQuery {
        project_id: i32,
        environment_id: i32,
        #[source]
        source: DbErr,
    },

    #[error(
        "Failed to gather environment variables from linked services for project {project_id}, environment {environment_id}: {failures}"
    )]
    LinkedServiceVariables {
        project_id: i32,
        environment_id: i32,
        failures: String,
    },

    #[error(
        "Managed service binding '{target}' for template '{template_slug}' could not read '{source_variable}' from the linked {service} service in project {project_id}, environment {environment_id}"
    )]
    ManagedServiceBindingMissing {
        project_id: i32,
        environment_id: i32,
        template_slug: String,
        service: String,
        target: String,
        source_variable: String,
    },

    #[error(
        "Managed service bindings for template '{template_slug}' require a linked {service} service in project {project_id}, environment {environment_id}"
    )]
    ManagedServiceTypeMissing {
        project_id: i32,
        environment_id: i32,
        template_slug: String,
        service: String,
    },

    #[error(
        "Managed service bindings for template '{template_slug}' require exactly one linked {service} service in project {project_id}, environment {environment_id}; found {count}"
    )]
    ManagedServiceTypeAmbiguous {
        project_id: i32,
        environment_id: i32,
        template_slug: String,
        service: String,
        count: usize,
    },

    #[error(
        "Stored service template for project {project_id}, environment {environment_id} is invalid: {reason}"
    )]
    InvalidServiceTemplate {
        project_id: i32,
        environment_id: i32,
        reason: String,
    },

    #[error(
        "Secrets-manager resolution failed for project {project_id}, environment {environment_id}: {reason}. Verify provider connectivity and credentials"
    )]
    SecretsManager {
        project_id: i32,
        environment_id: i32,
        reason: String,
    },
}

async fn apply_secrets_manager_layer(
    resolved: &mut HashMap<String, String>,
    resolver: Option<Arc<dyn SecretsManagerResolver>>,
    project_id: i32,
    environment_id: i32,
) -> Result<(), DeploymentEnvResolutionError> {
    let Some(resolver) = resolver else {
        return Ok(());
    };

    let secret_bindings = resolver
        .resolve_secrets_for_deployment(project_id, environment_id)
        .await
        .map_err(|error| DeploymentEnvResolutionError::SecretsManager {
            project_id,
            environment_id,
            reason: error.to_string(),
        })?;

    info!(
        "Resolved {} secret(s) for project {} environment {}: [{}]",
        secret_bindings.len(),
        project_id,
        environment_id,
        secret_bindings
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    );
    for key in secret_bindings.keys() {
        if resolved.contains_key(key) {
            tracing::warn!(
                "Secrets binding overwrites existing env var '{}' for project {} environment {}",
                key,
                project_id,
                environment_id,
            );
        }
    }
    resolved.extend(secret_bindings);
    Ok(())
}

/// Build the OpenTelemetry SDK header-list value for a deployed project.
///
/// Header values in `OTEL_EXPORTER_OTLP_HEADERS` use URL encoding, so the
/// space in the Bearer scheme must be encoded. The diagnostic slug uses an
/// explicit hex transport header so Unicode and delimiter characters cannot
/// corrupt the header-list grammar. Authentication still relies only on the
/// token.
pub(super) fn otel_exporter_headers(
    token: Option<&str>,
    existing_headers: Option<&str>,
    project_slug: &str,
) -> String {
    let slug_header = format!(
        "X-Temps-Project-Slug-Hex={}",
        hex::encode(project_slug.as_bytes())
    );
    match token {
        Some(token) => format!(
            "Authorization=Bearer%20{},{slug_header}",
            urlencoding::encode(token),
        ),
        None => existing_headers
            .map(str::trim)
            .map(|headers| headers.trim_end_matches(',').trim_end())
            .filter(|headers| !headers.is_empty())
            .map(|headers| format!("{headers},{slug_header}"))
            .unwrap_or(slug_header),
    }
}

/// Applies environment-variable layers in increasing precedence order.
///
/// Linked services provide convenient defaults such as `POSTGRES_URL`, while
/// project environment variables are explicit user configuration and must win
/// when both layers define the same key. Platform-managed variables are added
/// by the caller after this merge and therefore remain reserved.
pub(super) fn merge_environment_variable_layers(
    resolved: &mut HashMap<String, String>,
    linked_service_vars: HashMap<String, String>,
    explicit_project_vars: HashMap<String, String>,
) {
    resolved.extend(linked_service_vars);
    resolved.extend(explicit_project_vars);
}

/// Apply declarative aliases from a reviewed bundled template to the variables
/// supplied by linked Temps-managed services. Explicit project variables are
/// merged afterwards and can override these defaults.
type LinkedServiceVariables = BTreeMap<String, Vec<HashMap<String, String>>>;

fn select_effective_environment_variables(
    variables: Vec<env_vars::Model>,
    links: &[env_var_environments::Model],
    environment_id: i32,
    is_preview_environment: bool,
) -> BTreeMap<String, env_vars::Model> {
    let linked_ids = links
        .iter()
        .map(|link| link.env_var_id)
        .collect::<std::collections::HashSet<_>>();
    let environment_linked_ids = links
        .iter()
        .filter(|link| link.environment_id == environment_id)
        .map(|link| link.env_var_id)
        .collect::<std::collections::HashSet<_>>();
    let is_environment_specific = |variable: &env_vars::Model| {
        variable.environment_id == Some(environment_id)
            || environment_linked_ids.contains(&variable.id)
    };
    let mut effective = BTreeMap::<String, env_vars::Model>::new();
    for variable in variables {
        let environment_specific = is_environment_specific(&variable);
        let global = variable.environment_id.is_none()
            && !linked_ids.contains(&variable.id)
            && (!is_preview_environment || variable.include_in_preview);
        if !environment_specific && !global {
            continue;
        }
        let replace = effective.get(&variable.key).is_none_or(|current| {
            (environment_specific, variable.id) > (is_environment_specific(current), current.id)
        });
        if replace {
            effective.insert(variable.key.clone(), variable);
        }
    }
    effective
}

fn apply_managed_service_bindings(
    linked_service_vars: &mut HashMap<String, String>,
    linked_service_vars_by_type: &LinkedServiceVariables,
    service_template: Option<&serde_json::Value>,
    project_id: i32,
    environment_id: i32,
) -> Result<(), DeploymentEnvResolutionError> {
    let Some(value) = service_template else {
        return Ok(());
    };
    let instance =
        serde_json::from_value::<temps_core::templates::ServiceTemplateInstance>(value.clone())
            .map_err(
                |error| DeploymentEnvResolutionError::InvalidServiceTemplate {
                    project_id,
                    environment_id,
                    reason: error.to_string(),
                },
            )?;
    instance.validate().map_err(
        |error| DeploymentEnvResolutionError::InvalidServiceTemplate {
            project_id,
            environment_id,
            reason: error.to_string(),
        },
    )?;
    let template = instance.template;
    let template_slug = template.slug.clone();

    for (service, bindings) in template.managed_service_bindings {
        let normalized_service = temps_core::templates::canonical_managed_service_type(&service);
        let service_candidates = linked_service_vars_by_type
            .get(&normalized_service)
            .ok_or_else(|| DeploymentEnvResolutionError::ManagedServiceTypeMissing {
                project_id,
                environment_id,
                template_slug: template_slug.clone(),
                service: service.clone(),
            })?;
        if service_candidates.len() != 1 {
            return Err(DeploymentEnvResolutionError::ManagedServiceTypeAmbiguous {
                project_id,
                environment_id,
                template_slug: template_slug.clone(),
                service,
                count: service_candidates.len(),
            });
        }
        let service_variables = &service_candidates[0];
        for (target, source) in bindings {
            let value = service_variables.get(&source).cloned().ok_or_else(|| {
                DeploymentEnvResolutionError::ManagedServiceBindingMissing {
                    project_id,
                    environment_id,
                    template_slug: template_slug.clone(),
                    service: service.clone(),
                    target: target.clone(),
                    source_variable: source.clone(),
                }
            })?;
            linked_service_vars.entry(target).or_insert(value);
        }
    }

    Ok(())
}

/// Add reviewed template aliases to linked-service values, then merge those
/// defaults beneath explicit project variables. Both normal deployments and
/// rollback/promotion paths use this entry point so their containers receive
/// the same managed-service contract.
pub(super) fn merge_managed_service_environment(
    resolved: &mut HashMap<String, String>,
    linked_service_vars_by_type: LinkedServiceVariables,
    explicit_project_vars: HashMap<String, String>,
    service_template: Option<&serde_json::Value>,
    project_id: i32,
    environment_id: i32,
) -> Result<(), DeploymentEnvResolutionError> {
    let mut linked_service_vars = HashMap::new();
    for service_candidates in linked_service_vars_by_type.values() {
        for service_variables in service_candidates {
            for (key, value) in service_variables {
                linked_service_vars
                    .entry(key.clone())
                    .or_insert_with(|| value.clone());
            }
        }
    }
    apply_managed_service_bindings(
        &mut linked_service_vars,
        &linked_service_vars_by_type,
        service_template,
        project_id,
        environment_id,
    )?;
    merge_environment_variable_layers(resolved, linked_service_vars, explicit_project_vars);
    Ok(())
}

/// Apply values owned by the deployment runtime after every tenant-controlled
/// layer has been resolved. These keys must behave identically for normal,
/// promoted, and rolled-back deployments.
pub(super) fn apply_deployment_owned_variables(
    resolved: &mut HashMap<String, String>,
    preset: Preset,
    deployment_slug: &str,
    exposed_port: Option<u32>,
) {
    let asset_prefix = format!("/_temps/assets/{deployment_slug}");
    if let Some(exposed_port) = exposed_port {
        resolved.insert("PORT".to_string(), exposed_port.to_string());
    }
    resolved.insert("TEMPS_ASSET_PREFIX".to_string(), asset_prefix.clone());
    if preset == Preset::NextJs {
        resolved.insert("NEXT_PUBLIC_TEMPS_ASSET_PREFIX".to_string(), asset_prefix);
    }
}

/// Resolves the full environment-variable map for a `(project, environment,
/// deployment)`. Holds the six services the resolution needs; cheap to clone
/// (every field is an `Arc`).
#[derive(Clone)]
pub struct DeploymentEnvResolver {
    pub db: Arc<DatabaseConnection>,
    pub encryption_service: Arc<EncryptionService>,
    pub config_service: Arc<temps_config::ConfigService>,
    pub external_service_manager: Arc<temps_providers::ExternalServiceManager>,
    pub dsn_service: Arc<temps_error_tracking::DSNService>,
    pub deployment_token_service: Arc<DeploymentTokenService>,
    pub secrets_resolver: SecretsResolverSlot,
}

impl DeploymentEnvResolver {
    /// Gather every environment variable a container for this deployment should
    /// receive. Returns an error only when a linked external service fails to
    /// provide its runtime vars (a missing DB connection string must fail the
    /// deploy, not silently boot a broken container); Sentry/token failures are
    /// logged and skipped (optional instrumentation).
    ///
    /// IMPORTANT: If any external service fails to provide env vars, the entire
    /// deployment fails with a meaningful error — this prevents silent failures
    /// where containers would be missing critical configuration.
    pub async fn resolve(
        &self,
        project: &projects::Model,
        environment: &environments::Model,
        deployment: &deployments::Model,
    ) -> Result<HashMap<String, String>, DeploymentEnvResolutionError> {
        use temps_entities::project_services;

        let mut env_vars_map = HashMap::new();
        let mut explicit_project_vars = HashMap::new();

        // Add default HOST environment variable
        // This ensures containers bind to all network interfaces (0.0.0.0)
        // which is required for external access via port mapping
        // Can be overridden by user-defined environment variables
        env_vars_map.insert("HOST".to_string(), "0.0.0.0".to_string());

        // 1. Resolve project variables with the same precedence used by the
        // service-template upgrade path: an environment-scoped row overrides
        // an unlinked global row, and the newest ID wins within a scope. Both
        // the current junction-table model and the legacy direct environment
        // column remain deployable.
        let env_vars_list = env_vars::Entity::find()
            .filter(env_vars::Column::ProjectId.eq(project.id))
            .all(self.db.as_ref())
            .await
            .map_err(
                |source| DeploymentEnvResolutionError::EnvironmentVariablesQuery {
                    project_id: project.id,
                    environment_id: environment.id,
                    source,
                },
            )?;
        let env_var_ids = env_vars_list
            .iter()
            .map(|variable| variable.id)
            .collect::<Vec<_>>();
        let variable_links = if env_var_ids.is_empty() {
            Vec::new()
        } else {
            env_var_environments::Entity::find()
                .filter(env_var_environments::Column::EnvVarId.is_in(env_var_ids))
                .all(self.db.as_ref())
                .await
                .map_err(|source| {
                    DeploymentEnvResolutionError::EnvironmentVariableBindingsQuery {
                        project_id: project.id,
                        environment_id: environment.id,
                        source,
                    }
                })?
        };
        let effective_variables = select_effective_environment_variables(
            env_vars_list,
            &variable_links,
            environment.id,
            environment.is_preview,
        );

        for env_var in effective_variables.into_values() {
            let value = if env_var.is_encrypted {
                self.encryption_service
                    .decrypt_string(&env_var.value)
                    .map_err(|error| {
                        DeploymentEnvResolutionError::EnvironmentVariableDecryption {
                            project_id: project.id,
                            environment_id: environment.id,
                            variable_id: env_var.id,
                            key: env_var.key.clone(),
                            reason: error.to_string(),
                        }
                    })?
            } else {
                env_var.value
            };
            explicit_project_vars.insert(env_var.key, value);
        }

        debug!(
            "📦 Loaded {} effective project environment variables",
            explicit_project_vars.len()
        );

        // 2. Get runtime environment variables from external services
        // First, get all services linked to this project
        let project_services_list = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.eq(project.id))
            .all(self.db.as_ref())
            .await
            .map_err(|source| DeploymentEnvResolutionError::LinkedServicesQuery {
                project_id: project.id,
                environment_id: environment.id,
                source,
            })?;

        debug!(
            "🔌 Found {} external services linked to project {}",
            project_services_list.len(),
            project.id
        );

        // Track failed services to provide detailed error messages
        let mut failed_services: Vec<(i32, String)> = Vec::new();
        let mut linked_service_vars: LinkedServiceVariables = BTreeMap::new();

        // Get runtime environment variables from each external service
        for project_service in project_services_list {
            debug!(
                "Fetching runtime env vars for service ID {} (project: {}, environment: {})",
                project_service.service_id, project.id, environment.id
            );

            let service = match self
                .external_service_manager
                .get_service(project_service.service_id)
                .await
            {
                Ok(service) => service,
                Err(error) => {
                    failed_services.push((project_service.service_id, error.to_string()));
                    continue;
                }
            };
            let service_type =
                temps_core::templates::canonical_managed_service_type(&service.service_type);
            match self
                .external_service_manager
                .get_runtime_env_vars(project_service.service_id, project.id, environment.id)
                .await
            {
                Ok(service_env_vars) => {
                    debug!(
                        "Got {} env vars from service {}: {}",
                        service_env_vars.len(),
                        project_service.service_id,
                        service_env_vars
                            .keys()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    linked_service_vars
                        .entry(service_type)
                        .or_default()
                        .push(service_env_vars);
                }
                Err(e) => {
                    // Collect the error - we'll fail the entire deployment if any service fails
                    let error_msg = format!("{}", e);
                    failed_services.push((project_service.service_id, error_msg));
                    tracing::error!(
                        "Failed to get runtime env vars for service {}: {}",
                        project_service.service_id,
                        e
                    );
                }
            }
        }

        // CRITICAL: If any external service failed, fail the entire deployment
        // This prevents silent failures where containers would be missing critical environment variables
        if !failed_services.is_empty() {
            let failure_details = failed_services
                .iter()
                .map(|(service_id, error)| format!("  • Service ID {}: {}", service_id, error))
                .collect::<Vec<_>>()
                .join("\n");

            return Err(DeploymentEnvResolutionError::LinkedServiceVariables {
                project_id: project.id,
                environment_id: environment.id,
                failures: format!("{} service(s):\n{failure_details}", failed_services.len()),
            });
        }

        merge_managed_service_environment(
            &mut env_vars_map,
            linked_service_vars,
            explicit_project_vars,
            project.service_template.as_ref(),
            project.id,
            environment.id,
        )?;

        // Secrets-manager bindings are operator-controlled and therefore
        // override linked-service defaults and explicit project variables.
        // Platform-owned values are injected after this layer and remain
        // authoritative. Resolution is fail-closed: a deployment must never
        // continue with silently missing secrets.
        let maybe_secrets_resolver = {
            let guard = self.secrets_resolver.read().await;
            guard.as_ref().cloned()
        };
        apply_secrets_manager_layer(
            &mut env_vars_map,
            maybe_secrets_resolver,
            project.id,
            environment.id,
        )
        .await?;

        // 3. Get or create Sentry DSN for error tracking
        // Generate/fetch DSN for this project/environment combination
        // This ensures each environment has its own DSN for proper error isolation
        debug!(
            "🔑 Fetching or generating Sentry DSN for project {} environment {}",
            project.id, environment.id
        );

        // Get base URL from config service for DSN generation
        match self.config_service.get_external_url_or_default().await {
            Ok(base_url) => {
                match self
                    .dsn_service
                    .get_or_create_project_dsn(
                        project.id,
                        Some(environment.id),
                        None, // deployment_id is None - DSN is per environment, not per deployment
                        &base_url,
                    )
                    .await
                {
                    Ok(project_dsn) => {
                        debug!(
                            "Got DSN for project {} environment {}: {}",
                            project.id, environment.id, project_dsn.dsn
                        );
                        // Always add SENTRY_DSN for server-side usage
                        env_vars_map.insert("SENTRY_DSN".to_string(), project_dsn.dsn.clone());

                        let sentry_tunnel =
                            format!("/api{}", temps_error_tracking::SENTRY_TUNNEL_ROUTE_PATH);
                        env_vars_map.insert("SENTRY_TUNNEL".to_string(), sentry_tunnel.clone());

                        // Add framework-specific public DSN env var based on preset.
                        // Each client bundler only exposes vars matching its own prefix
                        // convention to the browser bundle, so we mirror that mapping.
                        if let Some(public_var) = public_sentry_dsn_var(project.preset) {
                            env_vars_map.insert(public_var.to_string(), project_dsn.dsn);
                        }

                        // Add the same-origin tunnel path browser SDKs should pass as
                        // `Sentry.init({ tunnel })`. The value is a constant (not
                        // project-specific), but injecting it as an env var — rather
                        // than hardcoding it in every framework's setup snippet — means
                        // the path can change in one place (here) without touching
                        // deployed apps' source or docs.
                        if let Some(tunnel_var) = public_sentry_tunnel_var(project.preset) {
                            env_vars_map.insert(tunnel_var.to_string(), sentry_tunnel);
                        }
                    }
                    Err(e) => {
                        // Warn about Sentry DSN failure but don't fail the deployment
                        // Sentry is optional for monitoring, not required for app functionality
                        tracing::error!(
                            "Failed to get or create DSN for project {} environment {}: {}. \
                            Sentry DSN environment variables will NOT be included.",
                            project.id,
                            environment.id,
                            e
                        );
                    }
                }
            }
            Err(e) => {
                // Warn about external URL failure but don't fail the deployment
                // Sentry is optional for monitoring, not required for app functionality
                tracing::error!(
                    "Failed to get external URL from config: {}. \
                    Sentry DSN environment variables will NOT be included.",
                    e
                );
            }
        }

        // 4. Get or create deployment token for API access
        // This provides TEMPS_API_URL and TEMPS_API_TOKEN environment variables
        // allowing deployed applications to access Temps APIs for:
        // - Enriching visitor data
        // - Sending emails
        // - Other platform features
        debug!(
            "🔑 Getting or creating deployment token for project {} environment {}",
            project.id, environment.id
        );

        match self.config_service.get_external_url_or_default().await {
            Ok(base_url) => {
                // Set the API URL - this is always available
                env_vars_map.insert("TEMPS_API_URL".to_string(), format!("{}/api", base_url));

                // Get or create the deployment token
                match self
                    .deployment_token_service
                    .get_or_create_deployment_token(
                        project.id,
                        Some(environment.id),
                        Some(deployment.id),
                    )
                    .await
                {
                    Ok(token) => {
                        debug!(
                            "Got deployment token for project {} environment {} (prefix: {}...)",
                            project.id,
                            environment.id,
                            &token[..8.min(token.len())]
                        );
                        env_vars_map.insert("TEMPS_API_TOKEN".to_string(), token.clone());

                        // 5. CRON_SECRET - same token so the cron scheduler can send
                        // Authorization: Bearer <CRON_SECRET> and the deployed app can verify it
                        env_vars_map.insert("CRON_SECRET".to_string(), token);
                    }
                    Err(e) => {
                        // Warn about deployment token failure but don't fail the deployment
                        // Deployment tokens are optional for API access
                        tracing::warn!(
                            "Failed to get or create deployment token for project {} environment {}: {}. \
                            TEMPS_API_TOKEN environment variable will NOT be included.",
                            project.id,
                            environment.id,
                            e
                        );
                    }
                }
            }
            Err(e) => {
                // Warn about external URL failure but don't fail the deployment
                tracing::warn!(
                    "Failed to get external URL from config: {}. \
                    TEMPS_API_URL and TEMPS_API_TOKEN environment variables will NOT be included.",
                    e
                );
            }
        }

        // 5. OpenTelemetry environment variables for automatic instrumentation
        // Standard OTel SDK env vars so deployed apps can send traces/metrics/logs
        // without any manual configuration. Uses the same deployment token for auth.
        // See: https://opentelemetry.io/docs/specs/otel/configuration/sdk-environment-variables/
        if let Some(api_url) = env_vars_map.get("TEMPS_API_URL").cloned() {
            // TEMPS_API_URL is "{base}/api", OTLP endpoint is "{base}/api/otel"
            env_vars_map.insert(
                "OTEL_EXPORTER_OTLP_ENDPOINT".to_string(),
                format!("{}/otel", api_url),
            );
            env_vars_map.insert(
                "OTEL_EXPORTER_OTLP_PROTOCOL".to_string(),
                "http/protobuf".to_string(),
            );

            // Always include the project slug so authentication failures retain
            // project context. Authorization is added when token provisioning
            // succeeded (the token is already in TEMPS_API_TOKEN).
            let token = env_vars_map.get("TEMPS_API_TOKEN").map(String::as_str);
            let existing_headers = env_vars_map
                .get("OTEL_EXPORTER_OTLP_HEADERS")
                .map(String::as_str);
            let otel_headers = otel_exporter_headers(token, existing_headers, &project.slug);
            env_vars_map.insert("OTEL_EXPORTER_OTLP_HEADERS".to_string(), otel_headers);

            env_vars_map.insert("OTEL_SERVICE_NAME".to_string(), project.name.clone());

            // Use commit SHA as service version when available
            if let Some(ref commit_sha) = deployment.commit_sha {
                env_vars_map
                    .entry("OTEL_SERVICE_VERSION".to_string())
                    .or_insert_with(|| commit_sha.clone());
            }

            debug!(
                "Set OTEL_EXPORTER_OTLP_ENDPOINT for project {} environment {}",
                project.id, environment.id
            );
        }

        // Release identity is a platform-provided default, but advanced users
        // may supply a semantic release name. Keep the explicit/secret value
        // when present, matching the normal Git and image planners.
        if let Some(ref commit_sha) = deployment.commit_sha {
            env_vars_map
                .entry("SENTRY_RELEASE".to_string())
                .or_insert_with(|| commit_sha.clone());
        }

        info!(
            "Gathered {} total environment variables for deployment: {}",
            env_vars_map.len(),
            env_vars_map.keys().cloned().collect::<Vec<_>>().join(", ")
        );
        Ok(env_vars_map)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};
    use std::sync::Arc;

    use async_trait::async_trait;
    use temps_core::SecretsManagerResolver;
    use temps_entities::preset::Preset;

    use super::{
        apply_deployment_owned_variables, apply_managed_service_bindings,
        apply_secrets_manager_layer, merge_environment_variable_layers,
        merge_managed_service_environment, otel_exporter_headers,
        select_effective_environment_variables, DeploymentEnvResolutionError,
    };

    fn keycloak_release_json() -> serde_json::Value {
        serde_json::to_value(temps_core::templates::ServiceTemplateInstance::new(
            temps_core::templates::SERVICE_TEMPLATE_SCHEMA_VERSION,
            temps_core::templates::bundled_template_by_slug("keycloak")
                .expect("Keycloak service template must be bundled"),
        ))
        .expect("service release should serialize")
    }

    struct TestSecretsResolver {
        result: Result<HashMap<String, String>, String>,
    }

    #[async_trait]
    impl SecretsManagerResolver for TestSecretsResolver {
        async fn resolve_secrets_for_deployment(
            &self,
            _project_id: i32,
            _environment_id: i32,
        ) -> Result<HashMap<String, String>, Box<dyn std::error::Error + Send + Sync>> {
            self.result
                .clone()
                .map_err(|reason| std::io::Error::other(reason).into())
        }
    }

    #[test]
    fn explicit_project_vars_override_linked_service_defaults() {
        let mut resolved = HashMap::from([("HOST".to_string(), "0.0.0.0".to_string())]);
        let linked_service_vars = HashMap::from([
            (
                "POSTGRES_URL".to_string(),
                "postgresql://linked-database/app".to_string(),
            ),
            ("POSTGRES_HOST".to_string(), "linked-database".to_string()),
        ]);
        let explicit_project_vars = HashMap::from([(
            "POSTGRES_URL".to_string(),
            "postgresql://user-selected-database/app".to_string(),
        )]);

        merge_environment_variable_layers(
            &mut resolved,
            linked_service_vars,
            explicit_project_vars,
        );

        assert_eq!(
            resolved.get("POSTGRES_URL").map(String::as_str),
            Some("postgresql://user-selected-database/app")
        );
        assert_eq!(
            resolved.get("POSTGRES_HOST").map(String::as_str),
            Some("linked-database")
        );
        assert_eq!(resolved.get("HOST").map(String::as_str), Some("0.0.0.0"));
    }

    #[test]
    fn environment_variable_selection_includes_globals_and_prefers_current_scope() {
        let now = chrono::Utc::now();
        let variable = |id: i32, key: &str, value: &str, environment_id: Option<i32>| {
            temps_entities::env_vars::Model {
                id,
                project_id: 7,
                environment_id,
                key: key.to_string(),
                value: value.to_string(),
                created_at: now,
                updated_at: now,
                include_in_preview: false,
                is_encrypted: false,
                is_secret: false,
            }
        };
        let variables = vec![
            variable(1, "GLOBAL_ONLY", "global", None),
            variable(2, "SHARED", "global-fallback", None),
            variable(3, "SHARED", "production", None),
            variable(4, "LEGACY_DIRECT", "legacy", Some(20)),
            variable(5, "PREVIEW_ONLY", "preview", None),
        ];
        let links = vec![
            temps_entities::env_var_environments::Model {
                id: 1,
                env_var_id: 3,
                environment_id: 20,
                created_at: now,
            },
            temps_entities::env_var_environments::Model {
                id: 2,
                env_var_id: 5,
                environment_id: 21,
                created_at: now,
            },
        ];

        let selected = select_effective_environment_variables(variables, &links, 20, false);

        assert_eq!(selected["GLOBAL_ONLY"].value, "global");
        assert_eq!(selected["SHARED"].value, "production");
        assert_eq!(selected["LEGACY_DIRECT"].value, "legacy");
        assert!(!selected.contains_key("PREVIEW_ONLY"));
    }

    #[test]
    fn preview_environment_excludes_global_values_without_explicit_opt_in() {
        let now = chrono::Utc::now();
        let variable =
            |id: i32, key: &str, include_in_preview: bool| temps_entities::env_vars::Model {
                id,
                project_id: 7,
                environment_id: None,
                key: key.to_string(),
                value: "value".to_string(),
                created_at: now,
                updated_at: now,
                include_in_preview,
                is_encrypted: false,
                is_secret: key == "PRODUCTION_SECRET",
            };

        let selected = select_effective_environment_variables(
            vec![
                variable(1, "PRODUCTION_SECRET", false),
                variable(2, "PREVIEW_ALLOWED", true),
            ],
            &[],
            21,
            true,
        );

        assert!(!selected.contains_key("PRODUCTION_SECRET"));
        assert_eq!(selected["PREVIEW_ALLOWED"].value, "value");
    }

    #[test]
    fn keycloak_aliases_are_derived_from_managed_postgres() {
        let linked = HashMap::from([
            ("POSTGRES_HOST".to_string(), "postgres-12".to_string()),
            ("POSTGRES_PORT".to_string(), "5432".to_string()),
            ("POSTGRES_DB".to_string(), "keycloak".to_string()),
            ("POSTGRES_USER".to_string(), "temps".to_string()),
            ("POSTGRES_PASSWORD".to_string(), "secret".to_string()),
        ]);

        let mut resolved = HashMap::new();
        let service_template = keycloak_release_json();
        merge_managed_service_environment(
            &mut resolved,
            BTreeMap::from([
                ("postgres".to_string(), vec![linked]),
                (
                    "redis".to_string(),
                    vec![HashMap::from([(
                        "POSTGRES_HOST".to_string(),
                        "wrong-provider".to_string(),
                    )])],
                ),
            ]),
            HashMap::new(),
            Some(&service_template),
            4,
            8,
        )
        .expect("the reviewed Keycloak bindings should resolve");

        assert_eq!(
            resolved.get("KC_DB_URL_HOST"),
            Some(&"postgres-12".to_string())
        );
        assert_eq!(resolved.get("KC_DB_URL_PORT"), Some(&"5432".to_string()));
        assert_eq!(
            resolved.get("KC_DB_URL_DATABASE"),
            Some(&"keycloak".to_string())
        );
        assert_eq!(resolved.get("KC_DB_USERNAME"), Some(&"temps".to_string()));
        assert_eq!(resolved.get("KC_DB_PASSWORD"), Some(&"secret".to_string()));
    }

    #[test]
    fn managed_service_binding_type_lookup_is_case_insensitive() {
        let linked = HashMap::from([
            ("POSTGRES_HOST".to_string(), "postgres-12".to_string()),
            ("POSTGRES_PORT".to_string(), "5432".to_string()),
            ("POSTGRES_DB".to_string(), "keycloak".to_string()),
            ("POSTGRES_USER".to_string(), "temps".to_string()),
            ("POSTGRES_PASSWORD".to_string(), "secret".to_string()),
        ]);
        let mut instance =
            serde_json::from_value::<temps_core::templates::ServiceTemplateInstance>(
                keycloak_release_json(),
            )
            .expect("valid Keycloak service instance");
        let bindings = instance
            .template
            .managed_service_bindings
            .remove("postgres")
            .expect("Keycloak PostgreSQL bindings");
        instance
            .template
            .managed_service_bindings
            .insert("Postgres".to_string(), bindings);

        let mut resolved = HashMap::new();
        merge_managed_service_environment(
            &mut resolved,
            BTreeMap::from([("postgres".to_string(), vec![linked])]),
            HashMap::new(),
            Some(&serde_json::to_value(instance).expect("serialize service instance")),
            4,
            8,
        )
        .expect("service binding type matching must be case insensitive");

        assert_eq!(
            resolved.get("KC_DB_URL_HOST"),
            Some(&"postgres-12".to_string())
        );
    }

    #[test]
    fn missing_required_managed_binding_fails_with_context() {
        let mut linked = HashMap::new();
        let linked_by_type = BTreeMap::from([("postgres".to_string(), vec![linked.clone()])]);
        let service_template = keycloak_release_json();
        let error = apply_managed_service_bindings(
            &mut linked,
            &linked_by_type,
            Some(&service_template),
            4,
            8,
        )
        .expect_err("missing managed PostgreSQL values must fail closed");

        assert!(matches!(
            error,
            DeploymentEnvResolutionError::ManagedServiceBindingMissing {
                project_id: 4,
                environment_id: 8,
                ..
            }
        ));
    }

    #[test]
    fn duplicate_managed_service_type_is_only_ambiguous_for_template_binding() {
        let postgres_candidates = vec![
            HashMap::from([("POSTGRES_HOST".to_string(), "postgres-a".to_string())]),
            HashMap::from([("POSTGRES_HOST".to_string(), "postgres-b".to_string())]),
        ];
        let service_template = keycloak_release_json();
        let error = merge_managed_service_environment(
            &mut HashMap::new(),
            BTreeMap::from([("postgres".to_string(), postgres_candidates)]),
            HashMap::new(),
            Some(&service_template),
            4,
            8,
        )
        .expect_err("a template binding must identify exactly one provider");

        assert!(matches!(
            error,
            DeploymentEnvResolutionError::ManagedServiceTypeAmbiguous {
                project_id: 4,
                environment_id: 8,
                count: 2,
                ..
            }
        ));
    }

    #[test]
    fn ordinary_projects_can_still_receive_multiple_services_of_the_same_type() {
        let mut resolved = HashMap::new();
        merge_managed_service_environment(
            &mut resolved,
            BTreeMap::from([(
                "postgres".to_string(),
                vec![
                    HashMap::from([("DATABASE_A".to_string(), "postgres-a".to_string())]),
                    HashMap::from([("DATABASE_B".to_string(), "postgres-b".to_string())]),
                ],
            )]),
            HashMap::new(),
            None,
            4,
            8,
        )
        .expect("projects without template bindings are not ambiguous");

        assert_eq!(resolved.get("DATABASE_A"), Some(&"postgres-a".to_string()));
        assert_eq!(resolved.get("DATABASE_B"), Some(&"postgres-b".to_string()));
    }

    #[test]
    fn platform_managed_vars_still_override_explicit_project_vars() {
        let mut resolved = HashMap::new();
        let explicit_project_vars = HashMap::from([(
            "SENTRY_DSN".to_string(),
            "https://user-defined.example/1".to_string(),
        )]);

        merge_environment_variable_layers(&mut resolved, HashMap::new(), explicit_project_vars);
        resolved.insert(
            "SENTRY_DSN".to_string(),
            "https://temps-managed.example/2".to_string(),
        );

        assert_eq!(
            resolved.get("SENTRY_DSN").map(String::as_str),
            Some("https://temps-managed.example/2")
        );
    }

    #[tokio::test]
    async fn secrets_manager_values_override_tenant_layers() {
        let mut resolved = HashMap::from([("DATABASE_URL".to_string(), "tenant".to_string())]);
        let resolver = Arc::new(TestSecretsResolver {
            result: Ok(HashMap::from([
                ("DATABASE_URL".to_string(), "secret".to_string()),
                ("API_KEY".to_string(), "managed".to_string()),
            ])),
        });

        apply_secrets_manager_layer(&mut resolved, Some(resolver), 41, 52)
            .await
            .expect("the test secrets resolver should succeed");

        assert_eq!(
            resolved.get("DATABASE_URL").map(String::as_str),
            Some("secret")
        );
        assert_eq!(resolved.get("API_KEY").map(String::as_str), Some("managed"));
    }

    #[tokio::test]
    async fn secrets_manager_failure_is_typed_and_contextual() {
        let resolver = Arc::new(TestSecretsResolver {
            result: Err("provider unavailable".to_string()),
        });

        let error = apply_secrets_manager_layer(&mut HashMap::new(), Some(resolver), 41, 52)
            .await
            .expect_err("secret resolution must fail closed");

        assert!(matches!(
            error,
            DeploymentEnvResolutionError::SecretsManager {
                project_id: 41,
                environment_id: 52,
                ..
            }
        ));
        assert!(error.to_string().contains("provider unavailable"));
    }

    #[tokio::test]
    async fn missing_secrets_manager_is_a_no_op() {
        let mut resolved = HashMap::from([("DATABASE_URL".to_string(), "tenant".to_string())]);

        apply_secrets_manager_layer(&mut resolved, None, 41, 52)
            .await
            .expect("an unconfigured optional secrets manager should be a no-op");

        assert_eq!(
            resolved.get("DATABASE_URL").map(String::as_str),
            Some("tenant")
        );
    }

    #[test]
    fn deployment_owned_vars_override_tenant_layers() {
        let mut resolved = HashMap::from([
            ("PORT".to_string(), "9999".to_string()),
            (
                "TEMPS_ASSET_PREFIX".to_string(),
                "/tenant-controlled".to_string(),
            ),
            (
                "NEXT_PUBLIC_TEMPS_ASSET_PREFIX".to_string(),
                "/tenant-controlled".to_string(),
            ),
        ]);

        apply_deployment_owned_variables(&mut resolved, Preset::NextJs, "deploy-abc", Some(3000));

        assert_eq!(resolved.get("PORT").map(String::as_str), Some("3000"));
        assert_eq!(
            resolved.get("TEMPS_ASSET_PREFIX").map(String::as_str),
            Some("/_temps/assets/deploy-abc")
        );
        assert_eq!(
            resolved
                .get("NEXT_PUBLIC_TEMPS_ASSET_PREFIX")
                .map(String::as_str),
            Some("/_temps/assets/deploy-abc")
        );
    }

    #[test]
    fn compose_deployments_do_not_receive_a_global_port() {
        let mut resolved = HashMap::from([("PORT".to_string(), "service-owned".to_string())]);

        apply_deployment_owned_variables(
            &mut resolved,
            Preset::DockerCompose,
            "deploy-compose",
            None,
        );

        assert_eq!(
            resolved.get("PORT").map(String::as_str),
            Some("service-owned")
        );
        assert_eq!(
            resolved.get("TEMPS_ASSET_PREFIX").map(String::as_str),
            Some("/_temps/assets/deploy-compose")
        );
    }

    #[test]
    fn otel_headers_include_encoded_token_and_project_slug() {
        assert_eq!(
            otel_exporter_headers(Some("dt_example"), None, "example-project"),
            "Authorization=Bearer%20dt_example,X-Temps-Project-Slug-Hex=6578616d706c652d70726f6a656374"
        );
    }

    #[test]
    fn otel_headers_preserve_project_slug_without_token() {
        assert_eq!(
            otel_exporter_headers(None, None, "example-project"),
            "X-Temps-Project-Slug-Hex=6578616d706c652d70726f6a656374"
        );
    }

    #[test]
    fn otel_headers_hex_encode_unicode_and_delimiters_in_project_slug() {
        assert_eq!(
            otel_exporter_headers(None, None, "café,slug=value"),
            "X-Temps-Project-Slug-Hex=636166c3a92c736c75673d76616c7565"
        );
    }

    #[test]
    fn otel_headers_preserve_manual_auth_when_platform_token_is_missing() {
        assert_eq!(
            otel_exporter_headers(
                None,
                Some("Authorization=Bearer%20manual-token"),
                "example-project"
            ),
            "Authorization=Bearer%20manual-token,X-Temps-Project-Slug-Hex=6578616d706c652d70726f6a656374"
        );
    }
}
