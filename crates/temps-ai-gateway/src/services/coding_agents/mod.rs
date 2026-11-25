// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Coding-agent CLIs (Claude Code, Codex, OpenCode) kept current without a
//! Temps release (ADR 113).
//!
//! * The latest published release of each CLI is read from the npm registry,
//!   cached for six hours, and shown beside the version installed in the
//!   caller's persistent workspace (read by the existing harness-update
//!   endpoints).
//! * `ai_agent_cli_settings.auto_update` (off by default) lets a background
//!   pass install a newer release in workspaces used within the last two
//!   hours, at most [`MAX_UPDATES_PER_PASS`] per pass, each workspace at most
//!   once per six hours, never while a turn holds it. Every automatic update
//!   is audit-logged with the system as the actor.
//!
//! Control plane only: three registry requests per six hours, a map of at
//! most a few thousand entries, one pass every thirty minutes.

mod error;
pub(crate) mod registry;
#[cfg(test)]
mod tests;

pub use error::CodingAgentError;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ConnectionTrait, DatabaseConnection, EntityTrait,
};
use serde::{Deserialize, Serialize};
use temps_ai_agent_cli::harness_update::{harness_packages, HarnessPackage};
use temps_core::{AuditLogger, AuditOperation};
use temps_entities::ai_agent_cli_settings;
use tracing::{debug, info, warn};
use url::Url;
use utoipa::ToSchema;

use crate::providers::ProviderHttpClient;

/// How long a registry answer is reused.
pub const RELEASE_CACHE_TTL: Duration = Duration::from_secs(6 * 60 * 60);
/// A failed registry lookup is retried after this long.
pub const RELEASE_FAILURE_TTL: Duration = Duration::from_secs(15 * 60);
/// `refresh=true` re-reads the registry at most this often.
pub const MIN_REFRESH_AGE: Duration = Duration::from_secs(60);
/// Workspaces with a turn this recent are considered in use.
pub const RECENT_WORKSPACE_WINDOW: Duration = Duration::from_secs(2 * 60 * 60);
/// A workspace is updated automatically at most once per this interval.
pub const AUTO_UPDATE_COOLDOWN: Duration = Duration::from_secs(6 * 60 * 60);
/// Bounded fan-out: workspaces touched by one pass.
pub const MAX_UPDATES_PER_PASS: usize = 10;
/// Busy workspaces do not use up `MAX_UPDATES_PER_PASS` (they are retried,
/// and ten of them must not starve the idle ones behind), but every
/// workspace looked at counts against this bound.
pub const MAX_WORKSPACES_EXAMINED_PER_PASS: usize = 4 * MAX_UPDATES_PER_PASS;
const FIRST_PASS_DELAY: Duration = Duration::from_secs(5 * 60);
const PASS_INTERVAL: Duration = Duration::from_secs(30 * 60);

pub(crate) const CODING_AGENT_AUTO_UPDATED: &str = "AI_CODING_AGENT_AUTO_UPDATED";

/// Instance-wide coding-agent settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CodingAgentSettings {
    /// Install newer CLI releases in workspaces in use without asking.
    pub auto_update: bool,
    #[schema(value_type = Option<String>, format = DateTime)]
    pub updated_at: Option<DateTime<Utc>>,
}

/// A settings change; omitted fields are left as they are.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct CodingAgentSettingsUpdate {
    pub auto_update: Option<bool>,
}

/// The latest published release of one coding-agent CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct CodingAgentRelease {
    /// Temps provider id (`claude_cli`, `codex_cli`, `opencode`).
    pub provider_id: String,
    pub name: String,
    /// Executable name inside a workspace.
    pub binary: String,
    pub npm_package: String,
    /// Latest version on the npm registry, when it could be read.
    pub latest_version: Option<String>,
    /// When the registry was last asked.
    #[schema(value_type = Option<String>, format = DateTime)]
    pub checked_at: Option<DateTime<Utc>>,
    /// Why the latest version is unknown, when it is.
    pub error: Option<String>,
}

/// Every coding-agent CLI Temps manages, with its latest release, and the
/// auto-update setting.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CodingAgentsOverview {
    pub settings: CodingAgentSettings,
    pub agents: Vec<CodingAgentRelease>,
}

/// Outcome of one automatic-update pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AutoUpdatePass {
    pub checked: usize,
    pub updated: usize,
    pub failed: usize,
    pub busy: usize,
}

#[derive(Debug, Clone)]
struct CachedRelease {
    latest_version: Option<String>,
    error: Option<String>,
    checked_at: DateTime<Utc>,
}

impl CachedRelease {
    fn fresh(&self, now: DateTime<Utc>, refresh: bool) -> bool {
        let age = (now - self.checked_at).to_std().unwrap_or(Duration::ZERO);
        let ttl = if refresh {
            MIN_REFRESH_AGE
        } else if self.error.is_some() {
            RELEASE_FAILURE_TTL
        } else {
            RELEASE_CACHE_TTL
        };
        age < ttl
    }
}

pub struct CodingAgentService {
    db: Arc<DatabaseConnection>,
    ai_service: Arc<dyn temps_ai::AiService>,
    audit: Arc<dyn AuditLogger>,
    client: ProviderHttpClient,
    registry: Url,
    releases: tokio::sync::Mutex<HashMap<&'static str, CachedRelease>>,
    /// Last automatic attempt per (provider, principal). Pruned to the
    /// cooldown on every pass, so it holds at most the workspaces seen in it.
    attempts: std::sync::Mutex<HashMap<(&'static str, i32), DateTime<Utc>>>,
    pass: tokio::sync::Mutex<()>,
}

impl CodingAgentService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        ai_service: Arc<dyn temps_ai::AiService>,
        audit: Arc<dyn AuditLogger>,
    ) -> Self {
        #[allow(
            clippy::unwrap_used,
            reason = "NPM_REGISTRY is a constant, valid URL; covered by a test"
        )]
        let registry = Url::parse(registry::NPM_REGISTRY).unwrap();
        Self::with_registry(
            db,
            ai_service,
            audit,
            ProviderHttpClient::new(registry::REGISTRY_TIMEOUT),
            registry,
        )
    }

    fn with_registry(
        db: Arc<DatabaseConnection>,
        ai_service: Arc<dyn temps_ai::AiService>,
        audit: Arc<dyn AuditLogger>,
        client: ProviderHttpClient,
        registry: Url,
    ) -> Self {
        Self {
            db,
            ai_service,
            audit,
            client,
            registry,
            releases: tokio::sync::Mutex::new(HashMap::new()),
            attempts: std::sync::Mutex::new(HashMap::new()),
            pass: tokio::sync::Mutex::new(()),
        }
    }

    async fn settings_row<C: ConnectionTrait>(
        connection: &C,
    ) -> Result<Option<ai_agent_cli_settings::Model>, CodingAgentError> {
        Ok(
            ai_agent_cli_settings::Entity::find_by_id(ai_agent_cli_settings::SINGLETON_ID)
                .one(connection)
                .await?,
        )
    }

    pub async fn settings(&self) -> Result<CodingAgentSettings, CodingAgentError> {
        Ok(settings_view(
            Self::settings_row(self.db.as_ref()).await?.as_ref(),
        ))
    }

    /// Apply a settings change. Returns the new settings and the fields that
    /// changed, for the audit log.
    pub async fn update_settings(
        &self,
        update: CodingAgentSettingsUpdate,
    ) -> Result<(CodingAgentSettings, Vec<&'static str>), CodingAgentError> {
        ai_agent_cli_settings::Entity::insert(ai_agent_cli_settings::ActiveModel {
            id: Set(ai_agent_cli_settings::SINGLETON_ID),
            auto_update: Set(false),
            ..Default::default()
        })
        .on_conflict(
            OnConflict::column(ai_agent_cli_settings::Column::Id)
                .do_nothing()
                .to_owned(),
        )
        .exec_without_returning(self.db.as_ref())
        .await?;
        let row = Self::settings_row(self.db.as_ref()).await?.ok_or_else(|| {
            CodingAgentError::Database(sea_orm::DbErr::RecordNotFound(format!(
                "ai_agent_cli_settings row {} after insert",
                ai_agent_cli_settings::SINGLETON_ID
            )))
        })?;
        let mut changed = Vec::new();
        let mut active: ai_agent_cli_settings::ActiveModel = row.clone().into();
        if let Some(auto_update) = update.auto_update {
            if auto_update != row.auto_update {
                active.auto_update = Set(auto_update);
                changed.push("auto_update");
            }
        }
        if changed.is_empty() {
            return Ok((settings_view(Some(&row)), changed));
        }
        let saved = active.update(self.db.as_ref()).await?;
        info!(
            auto_update = saved.auto_update,
            "Coding-agent CLI settings updated"
        );
        Ok((settings_view(Some(&saved)), changed))
    }

    /// Every managed CLI with its latest release. `refresh` asks the
    /// registry again unless it answered within the last minute.
    pub async fn overview(&self, refresh: bool) -> Result<CodingAgentsOverview, CodingAgentError> {
        let settings = self.settings().await?;
        let mut agents = Vec::with_capacity(harness_packages().len());
        for package in harness_packages() {
            agents.push(self.release(*package, refresh).await);
        }
        Ok(CodingAgentsOverview { settings, agents })
    }

    /// Latest release of one CLI, from the cache when it is fresh.
    pub async fn release(&self, package: HarnessPackage, refresh: bool) -> CodingAgentRelease {
        let now = Utc::now();
        // Held across the lookup: concurrent callers wait for one request
        // instead of each asking the registry.
        let mut releases = self.releases.lock().await;
        let cached = match releases.get(package.provider_id) {
            Some(cached) if cached.fresh(now, refresh) => cached.clone(),
            _ => {
                let fetched =
                    match registry::fetch_latest(&self.client, &self.registry, package.npm_package)
                        .await
                    {
                        Ok(version) => CachedRelease {
                            latest_version: Some(version),
                            error: None,
                            checked_at: now,
                        },
                        Err(error) => {
                            warn!(
                                provider_id = package.provider_id,
                                package = package.npm_package,
                                error = %error,
                                "Coding-agent release lookup failed"
                            );
                            CachedRelease {
                                // Keep showing the last version known.
                                latest_version: releases
                                    .get(package.provider_id)
                                    .and_then(|previous| previous.latest_version.clone()),
                                error: Some(error.to_string()),
                                checked_at: now,
                            }
                        }
                    };
                releases.insert(package.provider_id, fetched.clone());
                fetched
            }
        };
        CodingAgentRelease {
            provider_id: package.provider_id.to_string(),
            name: package.name.to_string(),
            binary: package.binary.to_string(),
            npm_package: package.npm_package.to_string(),
            latest_version: cached.latest_version,
            checked_at: Some(cached.checked_at),
            error: cached.error,
        }
    }

    /// One automatic-update pass. Does nothing unless `auto_update` is on.
    pub async fn run_auto_update_pass(&self) -> Result<AutoUpdatePass, CodingAgentError> {
        let mut outcome = AutoUpdatePass::default();
        let Ok(_pass) = self.pass.try_lock() else {
            return Ok(outcome);
        };
        if !self.settings().await?.auto_update {
            return Ok(outcome);
        }
        let now = Utc::now();
        self.prune_attempts(now);
        'packages: for package in harness_packages() {
            let principals = self
                .ai_service
                .recent_workspace_principals(package.provider_id, RECENT_WORKSPACE_WINDOW)
                .await;
            for principal_id in principals {
                if outcome.checked - outcome.busy >= MAX_UPDATES_PER_PASS
                    || outcome.checked >= MAX_WORKSPACES_EXAMINED_PER_PASS
                {
                    break 'packages;
                }
                if self.attempted_recently(package.provider_id, principal_id, now) {
                    continue;
                }
                outcome.checked += 1;
                self.auto_update_workspace(*package, principal_id, now, &mut outcome)
                    .await;
            }
        }
        if outcome.checked > 0 {
            info!(
                checked = outcome.checked,
                updated = outcome.updated,
                failed = outcome.failed,
                busy = outcome.busy,
                "Coding-agent automatic update pass finished"
            );
        }
        Ok(outcome)
    }

    async fn auto_update_workspace(
        &self,
        package: HarnessPackage,
        principal_id: i32,
        now: DateTime<Utc>,
        outcome: &mut AutoUpdatePass,
    ) {
        let provider_id = package.provider_id;
        let status = match self
            .ai_service
            .workspace_harness_update_status(provider_id, principal_id, false)
            .await
        {
            Ok(status) => status,
            Err(error) => {
                self.record_attempt(provider_id, principal_id, now);
                outcome.failed += 1;
                warn!(provider_id, principal_id, error = %error, "Coding-agent version check failed");
                return;
            }
        };
        if status.updating {
            self.record_attempt(provider_id, principal_id, now);
            return;
        }
        // Without both versions there is nothing to compare: the release
        // lookup failed (registry timeout, `npm view` error) or the CLI in the
        // workspace could not report its version (a broken install). Either
        // is a failed check, not an up-to-date CLI.
        if status.latest_version.is_none() || status.installed_version.is_none() {
            self.record_attempt(provider_id, principal_id, now);
            outcome.failed += 1;
            warn!(
                provider_id,
                principal_id,
                installed = status.installed_version.as_deref().unwrap_or("unknown"),
                latest = status.latest_version.as_deref().unwrap_or("unknown"),
                detail = status
                    .detail
                    .as_deref()
                    .unwrap_or("a version was not reported"),
                "Coding-agent version check incomplete; automatic update skipped"
            );
            return;
        }
        if !status.update_available {
            self.record_attempt(provider_id, principal_id, now);
            return;
        }
        match self
            .ai_service
            .update_workspace_harness(provider_id, principal_id)
            .await
        {
            Ok(updated) if !update_took_effect(&status, &updated) => {
                // npm exited zero, but the CLI on PATH still reports an old
                // version or none (a second copy earlier on PATH, a broken
                // install). Not a success: no success audit.
                self.record_attempt(provider_id, principal_id, now);
                outcome.failed += 1;
                warn!(
                    provider_id,
                    principal_id,
                    expected = status.latest_version.as_deref().unwrap_or("unknown"),
                    installed = updated.installed_version.as_deref().unwrap_or("unknown"),
                    detail = updated.detail.as_deref().unwrap_or(""),
                    "Automatic coding-agent update finished but the installed version did not change to the latest"
                );
            }
            Ok(updated) => {
                self.record_attempt(provider_id, principal_id, now);
                outcome.updated += 1;
                self.ai_service
                    .invalidate_capabilities_for(Some(provider_id))
                    .await;
                info!(
                    provider_id,
                    principal_id,
                    from = status.installed_version.as_deref().unwrap_or("unknown"),
                    to = updated.installed_version.as_deref().unwrap_or("unknown"),
                    "Coding-agent CLI updated automatically"
                );
                let audit = AutoUpdateAudit {
                    provider_id: provider_id.to_string(),
                    principal_id,
                    from_version: status.installed_version,
                    to_version: updated.installed_version,
                };
                if let Err(error) = self.audit.create_audit_log(&audit).await {
                    warn!(
                        provider_id,
                        principal_id,
                        error = %error,
                        "Failed to create audit log for an automatic coding-agent update"
                    );
                }
            }
            Err(temps_ai::AiError::Provider { purpose, .. })
                if purpose == "provider.harness.update.busy" =>
            {
                // A turn holds the workspace: try again on the next pass.
                outcome.busy += 1;
                debug!(
                    provider_id,
                    principal_id, "Workspace busy; automatic update deferred"
                );
            }
            Err(error) => {
                self.record_attempt(provider_id, principal_id, now);
                outcome.failed += 1;
                warn!(provider_id, principal_id, error = %error, "Automatic coding-agent update failed");
            }
        }
    }

    fn attempted_recently(
        &self,
        provider_id: &'static str,
        principal_id: i32,
        now: DateTime<Utc>,
    ) -> bool {
        let attempts = self
            .attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        attempts
            .get(&(provider_id, principal_id))
            .is_some_and(|at| within(now, *at, AUTO_UPDATE_COOLDOWN))
    }

    fn record_attempt(&self, provider_id: &'static str, principal_id: i32, now: DateTime<Utc>) {
        self.attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert((provider_id, principal_id), now);
    }

    fn prune_attempts(&self, now: DateTime<Utc>) {
        self.attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|_, at| within(now, *at, AUTO_UPDATE_COOLDOWN));
    }

    /// Run automatic-update passes in the background for the life of the
    /// process.
    /// The returned handle is the only one; aborting it stops the task.
    pub fn spawn_background(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            tokio::time::sleep(FIRST_PASS_DELAY).await;
            let mut ticker = tokio::time::interval(PASS_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                if let Err(error) = self.run_auto_update_pass().await {
                    warn!(error = %error, "Coding-agent automatic update pass failed");
                }
            }
        })
    }
}

fn within(now: DateTime<Utc>, at: DateTime<Utc>, window: Duration) -> bool {
    (now - at).to_std().is_ok_and(|age| age < window) || at > now
}

fn settings_view(row: Option<&ai_agent_cli_settings::Model>) -> CodingAgentSettings {
    CodingAgentSettings {
        auto_update: row.is_some_and(|row| row.auto_update),
        updated_at: row.map(|row| row.updated_at),
    }
}

/// Audit record of an automatic update; the system is the actor.
#[derive(Debug, Clone, Serialize)]
struct AutoUpdateAudit {
    provider_id: String,
    /// Owner of the workspace that was updated.
    principal_id: i32,
    from_version: Option<String>,
    to_version: Option<String>,
}

impl AuditOperation for AutoUpdateAudit {
    fn operation_type(&self) -> String {
        CODING_AGENT_AUTO_UPDATED.to_string()
    }

    fn user_id(&self) -> Option<i32> {
        None
    }

    fn ip_address(&self) -> Option<String> {
        None
    }

    fn user_agent(&self) -> &str {
        "temps-coding-agent-auto-update"
    }

    fn serialize(&self) -> temps_core::AuditResult<String> {
        temps_core::serialize_audit_operation(self)
    }
}

/// Whether an update reported as successful actually installed a new CLI:
/// the binary now reports a version, no newer release is left, and it is not
/// older than the latest release the check saw.
fn update_took_effect(
    before: &temps_ai::HarnessUpdateStatus,
    after: &temps_ai::HarnessUpdateStatus,
) -> bool {
    use temps_ai_agent_cli::harness_update::is_newer;
    let Some(installed) = after.installed_version.as_deref() else {
        return false;
    };
    if after.update_available {
        return false;
    }
    match before.latest_version.as_deref() {
        Some(latest) => !is_newer(latest, installed),
        None => before.installed_version.as_deref() != Some(installed),
    }
}
