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

use std::collections::HashMap;
use std::sync::Arc;

use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use temps_core::EncryptionService;
use temps_entities::{deployments, environments, preset::Preset, projects};
use tracing::{debug, info};

use super::deployment_token_service::DeploymentTokenService;
use super::managed_environment_variables::{public_sentry_dsn_var, public_sentry_tunnel_var};
use super::workflow_planner::SecretsResolverSlot;

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
    ) -> anyhow::Result<HashMap<String, String>> {
        use temps_entities::{env_var_environments, env_vars, project_services};

        let mut env_vars_map = HashMap::new();
        let mut explicit_project_vars = HashMap::new();

        // Add default HOST environment variable
        // This ensures containers bind to all network interfaces (0.0.0.0)
        // which is required for external access via port mapping
        // Can be overridden by user-defined environment variables
        env_vars_map.insert("HOST".to_string(), "0.0.0.0".to_string());

        // 1. Get environment variables for this project and environment
        // Query through the env_var_environments junction table to get all env vars
        // associated with this environment
        let env_var_ids: Vec<i32> = env_var_environments::Entity::find()
            .filter(env_var_environments::Column::EnvironmentId.eq(environment.id))
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(|eve| eve.env_var_id)
            .collect();

        if !env_var_ids.is_empty() {
            let env_vars_list = env_vars::Entity::find()
                .filter(env_vars::Column::Id.is_in(env_var_ids))
                .filter(env_vars::Column::ProjectId.eq(project.id))
                .all(self.db.as_ref())
                .await?;

            for env_var in env_vars_list {
                let value = if env_var.is_encrypted {
                    self.encryption_service
                        .decrypt_string(&env_var.value)
                        .map_err(|e| {
                            anyhow::anyhow!(
                                "Failed to decrypt environment variable '{}' (id={}): {}",
                                env_var.key,
                                env_var.id,
                                e
                            )
                        })?
                } else {
                    env_var.value
                };
                explicit_project_vars.insert(env_var.key, value);
            }
        }

        debug!(
            "📦 Loaded {} environment variables from env_vars table via env_var_environments",
            explicit_project_vars.len()
        );

        // 2. Get runtime environment variables from external services
        // First, get all services linked to this project
        let project_services_list = project_services::Entity::find()
            .filter(project_services::Column::ProjectId.eq(project.id))
            .all(self.db.as_ref())
            .await?;

        debug!(
            "🔌 Found {} external services linked to project {}",
            project_services_list.len(),
            project.id
        );

        // Track failed services to provide detailed error messages
        let mut failed_services: Vec<(i32, String)> = Vec::new();
        let mut linked_service_vars = HashMap::new();

        // Get runtime environment variables from each external service
        for project_service in project_services_list {
            debug!(
                "Fetching runtime env vars for service ID {} (project: {}, environment: {})",
                project_service.service_id, project.id, environment.id
            );

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
                    linked_service_vars.extend(service_env_vars);
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

            let error_message = format!(
                "Failed to gather environment variables from {} external service(s). \
                The deployment cannot proceed without all required external services configured:\n{}",
                failed_services.len(),
                failure_details
            );

            return Err(anyhow::anyhow!(error_message));
        }

        // Linked-service variables are defaults. Explicit project variables
        // express user intent and therefore take precedence on collisions.
        merge_environment_variable_layers(
            &mut env_vars_map,
            linked_service_vars,
            explicit_project_vars,
        );

        // Secrets-manager bindings are operator-controlled and therefore
        // override linked-service defaults and explicit project variables.
        // Platform-owned values are injected after this layer and remain
        // authoritative. Resolution is fail-closed: a deployment must never
        // continue with silently missing secrets.
        let maybe_secrets_resolver = {
            let guard = self.secrets_resolver.read().await;
            guard.as_ref().cloned()
        };
        if let Some(secrets_resolver) = maybe_secrets_resolver {
            let secret_bindings = secrets_resolver
                .resolve_secrets_for_deployment(project.id, environment.id)
                .await
                .map_err(|error| {
                    anyhow::anyhow!(
                        "Secrets-manager resolution failed for project {} environment {}: {}. \
                         The deployment cannot proceed. Verify provider connectivity and credentials.",
                        project.id,
                        environment.id,
                        error
                    )
                })?;

            info!(
                "Resolved {} secret(s) for project {} environment {}: [{}]",
                secret_bindings.len(),
                project.id,
                environment.id,
                secret_bindings
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            for key in secret_bindings.keys() {
                if env_vars_map.contains_key(key) {
                    tracing::warn!(
                        "Secrets binding overwrites existing env var '{}' for project {} environment {}",
                        key,
                        project.id,
                        environment.id,
                    );
                }
            }
            env_vars_map.extend(secret_bindings);
        }

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
    use std::collections::HashMap;

    use temps_entities::preset::Preset;

    use super::{
        apply_deployment_owned_variables, merge_environment_variable_layers, otel_exporter_headers,
    };

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
