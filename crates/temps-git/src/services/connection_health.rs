// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Daily git-connection health checks.
//!
//! Pings each active connection's upstream (GitHub App installation, PAT, or
//! OAuth token) once per 24h, persists `health_status` + `health_message`, and
//! emits admin notifications on healthy↔unhealthy transitions. The
//! notifications crate handles throttling via `batch_key` + priority window,
//! so we don't re-implement rate limiting here.

use std::sync::{Arc, OnceLock};

use chrono::Utc;
use futures::stream::{self, StreamExt};
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use temps_entities::{git_provider_connections, git_providers};
use temps_monitoring::alarm_service::{AlarmService, AlarmSeverity, AlarmType, FireAlarmRequest};
use thiserror::Error;
use tracing::{debug, error, info, warn};

use super::git_provider::{AuthMethod, GitProviderError};
use super::git_provider_manager::{GitProviderManager, GitProviderManagerError};
use super::github::GithubAppService;

pub const HEALTH_STATUS_HEALTHY: &str = "healthy";
pub const HEALTH_STATUS_UNHEALTHY: &str = "unhealthy";
pub const HEALTH_STATUS_UNKNOWN: &str = "unknown";

/// Max connections probed concurrently during the daily sweep.
/// Most tenants run 1-2 git integrations; 8 is a safe upper bound that also
/// keeps rate-limit pressure low when someone has a dozen.
const HEALTH_CHECK_CONCURRENCY: usize = 8;

#[derive(Debug, Error)]
pub enum ConnectionHealthError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("Provider manager error: {0}")]
    ProviderManager(#[from] GitProviderManagerError),

    #[error("Connection {connection_id} not found")]
    NotFound { connection_id: i32 },
}

/// Outcome of probing a single connection.
#[derive(Debug, Clone)]
pub struct HealthCheckOutcome {
    pub connection_id: i32,
    pub status: String,
    pub message: Option<String>,
    /// True when the stored status flipped as a result of this check. Used by
    /// the caller to decide whether to send a notification.
    pub transitioned: bool,
}

pub struct ConnectionHealthService {
    db: Arc<DatabaseConnection>,
    git_provider_manager: Arc<GitProviderManager>,
    github_service: Arc<GithubAppService>,
    /// Set post-construction via `set_alarm_service` once all plugins have
    /// registered — `GitPlugin` registers before `MonitoringPlugin`, so
    /// `AlarmService` isn't available yet during `register_services`.
    alarm_service: OnceLock<Arc<AlarmService>>,
    /// Base URL used for deep-links inside notifications (e.g. admin console).
    console_base_url: String,
}

impl ConnectionHealthService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        git_provider_manager: Arc<GitProviderManager>,
        github_service: Arc<GithubAppService>,
        console_base_url: String,
    ) -> Self {
        Self {
            db,
            git_provider_manager,
            github_service,
            alarm_service: OnceLock::new(),
            console_base_url,
        }
    }

    /// Wire the `AlarmService` after construction. Idempotent — only the
    /// first call takes effect. Called from `GitPlugin::initialize_plugin_services`.
    pub fn set_alarm_service(&self, alarm_service: Arc<AlarmService>) {
        let _ = self.alarm_service.set(alarm_service);
    }

    /// Probe every active connection. Runs with bounded concurrency so a slow
    /// provider doesn't block the whole sweep. Returns the per-connection
    /// outcomes so callers/tests can assert on them.
    pub async fn run_health_checks_for_all(
        &self,
    ) -> Result<Vec<HealthCheckOutcome>, ConnectionHealthError> {
        let connections = git_provider_connections::Entity::find()
            .filter(git_provider_connections::Column::IsActive.eq(true))
            .all(self.db.as_ref())
            .await?;

        info!(
            count = connections.len(),
            "Starting scheduled git connection health sweep"
        );

        let outcomes = stream::iter(connections.into_iter().map(|c| c.id))
            .map(|id| {
                let this = self;
                async move {
                    match this.check_connection_health(id).await {
                        Ok(outcome) => Some(outcome),
                        Err(e) => {
                            error!(
                                connection_id = id,
                                error = %e,
                                "Health check failed unexpectedly"
                            );
                            None
                        }
                    }
                }
            })
            .buffer_unordered(HEALTH_CHECK_CONCURRENCY)
            .filter_map(|x| async move { x })
            .collect::<Vec<_>>()
            .await;

        let unhealthy = outcomes
            .iter()
            .filter(|o| o.status == HEALTH_STATUS_UNHEALTHY)
            .count();
        info!(
            total = outcomes.len(),
            unhealthy, "Git connection health sweep complete"
        );

        Ok(outcomes)
    }

    /// Probe a single connection, persist the result, and fire a notification
    /// on status transitions. Never errors on upstream/network problems —
    /// those are recorded as `unhealthy` with a message.
    pub async fn check_connection_health(
        &self,
        connection_id: i32,
    ) -> Result<HealthCheckOutcome, ConnectionHealthError> {
        let connection = self
            .git_provider_manager
            .get_connection(connection_id)
            .await
            .map_err(|e| match e {
                GitProviderManagerError::ConnectionNotFound(_) => {
                    ConnectionHealthError::NotFound { connection_id }
                }
                other => ConnectionHealthError::ProviderManager(other),
            })?;

        let provider = self
            .git_provider_manager
            .get_provider(connection.provider_id)
            .await?;

        let (status, message) = self.probe(&connection, &provider).await;

        let previous_status = connection.health_status.clone();
        let previous_message = connection.health_message.clone();
        let previous_failures = connection.consecutive_health_failures;
        let transitioned = previous_status != status;

        let new_failures = if status == HEALTH_STATUS_UNHEALTHY {
            previous_failures.saturating_add(1)
        } else {
            0
        };

        let now = Utc::now();
        let mut active: git_provider_connections::ActiveModel = connection.clone().into();
        active.health_status = Set(status.clone());
        active.health_message = Set(message.clone());
        active.last_health_check_at = Set(Some(now));
        active.consecutive_health_failures = Set(new_failures);
        active.updated_at = Set(now);
        active.update(self.db.as_ref()).await?;

        debug!(
            connection_id,
            account = connection.account_name.as_str(),
            status = status.as_str(),
            previous = previous_status.as_str(),
            failures = new_failures,
            "Persisted git connection health result"
        );

        if transitioned {
            self.notify_transition(
                &connection,
                &provider,
                &previous_status,
                &status,
                message.as_deref(),
                previous_message.as_deref(),
            )
            .await;
        }

        Ok(HealthCheckOutcome {
            connection_id,
            status,
            message,
            transitioned,
        })
    }

    /// Run the actual upstream probe. Returns `(status, message)`.
    async fn probe(
        &self,
        connection: &git_provider_connections::Model,
        provider: &git_providers::Model,
    ) -> (String, Option<String>) {
        // Resolve auth method from provider config.
        let auth_config = match self
            .git_provider_manager
            .decrypt_sensitive_data(&provider.auth_config)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                return (
                    HEALTH_STATUS_UNHEALTHY.to_string(),
                    Some(format!("Failed to decrypt provider auth config: {}", e)),
                );
            }
        };
        let auth_method = match serde_json::from_value::<AuthMethod>(auth_config) {
            Ok(m) => m,
            Err(e) => {
                return (
                    HEALTH_STATUS_UNHEALTHY.to_string(),
                    Some(format!("Invalid provider auth configuration: {}", e)),
                );
            }
        };

        let is_github_app_installation = matches!(auth_method, AuthMethod::GitHubApp { .. })
            && connection.installation_id.is_some();

        if is_github_app_installation {
            return self.probe_github_app(connection).await;
        }

        self.probe_via_token(connection, provider).await
    }

    async fn probe_github_app(
        &self,
        connection: &git_provider_connections::Model,
    ) -> (String, Option<String>) {
        let Some(installation_str) = connection.installation_id.as_deref() else {
            return (
                HEALTH_STATUS_UNHEALTHY.to_string(),
                Some("GitHub App connection missing installation_id".to_string()),
            );
        };

        let Ok(installation_id) = installation_str.parse::<i32>() else {
            return (
                HEALTH_STATUS_UNHEALTHY.to_string(),
                Some(format!(
                    "Invalid installation_id format: {}",
                    installation_str
                )),
            );
        };

        match self
            .github_service
            .verify_installation(installation_id)
            .await
        {
            Ok(true) => (HEALTH_STATUS_HEALTHY.to_string(), None),
            Ok(false) => (
                HEALTH_STATUS_UNHEALTHY.to_string(),
                Some("GitHub App installation no longer exists or was suspended".to_string()),
            ),
            Err(e) => {
                warn!(
                    connection_id = connection.id,
                    error = %e,
                    "verify_installation failed; marking unhealthy"
                );
                (
                    HEALTH_STATUS_UNHEALTHY.to_string(),
                    Some(format!(
                        "GitHub API error while verifying installation: {}",
                        e
                    )),
                )
            }
        }
    }

    async fn probe_via_token(
        &self,
        connection: &git_provider_connections::Model,
        provider: &git_providers::Model,
    ) -> (String, Option<String>) {
        // Try to resolve a usable access token. OAuth tokens may auto-refresh
        // here; PATs return as-is.
        let token = match self
            .git_provider_manager
            .validate_and_refresh_connection_token(connection.id)
            .await
        {
            Ok(t) => t,
            Err(e) => {
                return (
                    HEALTH_STATUS_UNHEALTHY.to_string(),
                    Some(format!("Failed to obtain access token: {}", e)),
                );
            }
        };

        let provider_service = match self
            .git_provider_manager
            .get_provider_service(provider.id)
            .await
        {
            Ok(s) => s,
            Err(e) => {
                return (
                    HEALTH_STATUS_UNHEALTHY.to_string(),
                    Some(format!("Failed to load provider service: {}", e)),
                );
            }
        };

        match provider_service.validate_token(&token).await {
            Ok(true) => (HEALTH_STATUS_HEALTHY.to_string(), None),
            Ok(false) => (
                HEALTH_STATUS_UNHEALTHY.to_string(),
                Some("Access token rejected by provider (revoked or expired)".to_string()),
            ),
            Err(GitProviderError::AuthenticationFailed(msg)) => (
                HEALTH_STATUS_UNHEALTHY.to_string(),
                Some(format!("Authentication failed: {}", msg)),
            ),
            Err(e) => (
                HEALTH_STATUS_UNHEALTHY.to_string(),
                Some(format!("Provider error: {}", e)),
            ),
        }
    }

    async fn notify_transition(
        &self,
        connection: &git_provider_connections::Model,
        provider: &git_providers::Model,
        previous_status: &str,
        new_status: &str,
        new_message: Option<&str>,
        previous_message: Option<&str>,
    ) {
        let Some(alarm_service) = self.alarm_service.get() else {
            debug!(
                connection_id = connection.id,
                "AlarmService not wired yet; skipping transition alarm"
            );
            return;
        };

        let provider_detail_url = format!(
            "{}/git-providers/{}",
            self.console_base_url.trim_end_matches('/'),
            provider.id
        );

        if new_status == HEALTH_STATUS_UNHEALTHY {
            let reason = new_message.unwrap_or("Unknown failure");
            let request = FireAlarmRequest {
                project_id: None,
                environment_id: None,
                deployment_id: None,
                container_id: None,
                service_id: None,
                alarm_type: AlarmType::GitProviderConnectionDown,
                severity: AlarmSeverity::Critical,
                title: format!(
                    "Git connection unhealthy: {} ({})",
                    connection.account_name, provider.name
                ),
                message: format!(
                    "The git connection '{}' on provider '{}' failed its daily health check.\n\n\
                     Reason: {}\n\n\
                     Projects that rely on this connection may stop syncing or deploying until it's fixed.\n\n\
                     Manage it at: {}",
                    connection.account_name, provider.name, reason, provider_detail_url,
                ),
                metadata: Some(serde_json::json!({
                    "connection_id": connection.id,
                    "provider_id": provider.id,
                    "account_name": connection.account_name,
                    "failure_reason": reason,
                    "previous_status": previous_status,
                })),
            };

            if let Err(e) = alarm_service.fire_alarm(request).await {
                error!(
                    connection_id = connection.id,
                    error = %e,
                    "Failed to fire git connection unhealthy alarm"
                );
            }
        } else if new_status == HEALTH_STATUS_HEALTHY && previous_status == HEALTH_STATUS_UNHEALTHY
        {
            debug!(
                connection_id = connection.id,
                ?previous_message,
                "Git connection recovered; resolving alarm"
            );
            // NOTE: `alarms` has no scope column for an individual git
            // connection, so this resolves every unhealthy connection alarm
            // system-wide rather than just this one — same known limitation
            // as the disk-space monitor's shared cooldown bucket.
            if let Err(e) = alarm_service
                .resolve_alarms_by_scope(
                    None,
                    None,
                    None,
                    None,
                    None,
                    AlarmType::GitProviderConnectionDown,
                )
                .await
            {
                error!(
                    connection_id = connection.id,
                    error = %e,
                    "Failed to resolve git connection unhealthy alarm(s)"
                );
            }
        } else {
            // unknown -> healthy (first successful check): don't spam admins.
            debug!(
                connection_id = connection.id,
                previous_status, new_status, "Skipping alarm for benign transition"
            );
        }
    }
}
