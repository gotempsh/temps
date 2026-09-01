// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Plugin registration for the standalone sandbox API.
//!
//! Wires up the `SandboxService`, `StandaloneSandboxRegistry`, and
//! `JobTracker`, and exposes HTTP routes under `/v1/sandbox/*`.
//!
//! Dependencies:
//! - `temps-agents` plugin must run first to register the shared
//!   `Arc<dyn SandboxProvider>` we consume here.
//! - `temps-config` plugin for the `ConfigService` used to compute
//!   preview URL parts.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use temps_agents::sandbox::SandboxProvider;
use temps_config::ConfigService;
use temps_core::plugin::{
    PluginContext, PluginError, PluginRoutes, ServiceRegistrationContext, TempsPlugin,
};
use temps_git::GitProviderManager;
use tracing::{debug, info, warn};
use utoipa::OpenApi;

use crate::handlers::{configure_routes, SandboxApiDoc, SandboxAppState};
use crate::services::expiration_sweeper::SandboxExpirationSweeper;
use crate::services::job_tracker::JobTracker;
use crate::services::registry::StandaloneSandboxRegistry;
use crate::services::sandbox_service::SandboxService;
use crate::services::snapshot_service::SnapshotService;

pub struct SandboxPlugin;

impl SandboxPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SandboxPlugin {
    fn default() -> Self {
        Self::new()
    }
}

/// Root directory on the host where per-sandbox working directories live.
/// Mirrors the `agent-sessions` pattern used by `temps-agents::executor`.
fn data_root() -> PathBuf {
    let root = std::env::var("TEMPS_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            // Match the rest of the codebase: $HOME/.temps when TEMPS_DATA_DIR
            // is unset. Falls back to `./.temps` on the unusual path where
            // $HOME is also unset (e.g. some container runtimes).
            std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".temps")
        });
    root.join("standalone-sandboxes")
}

impl TempsPlugin for SandboxPlugin {
    fn name(&self) -> &'static str {
        "sandbox"
    }

    fn register_services<'a>(
        &'a self,
        context: &'a ServiceRegistrationContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Require the shared sandbox provider from the agents plugin.
            // If it's missing we register nothing — the plugin still loads,
            // but its HTTP routes will surface a 503 "Unavailable" so ops
            // can tell the difference between "agents plugin absent" and
            // "feature broken".
            let provider = match context.get_service::<dyn SandboxProvider>() {
                Some(p) => p,
                None => {
                    warn!(
                        "Sandbox plugin: no SandboxProvider registered — \
                         /v1/sandbox/* endpoints will return 503 until the \
                         agents plugin is loaded before this one"
                    );
                    return Ok(());
                }
            };

            let db = context.require_service::<sea_orm::DatabaseConnection>();
            let platform_config = context.require_service::<ConfigService>();
            let cookie_crypto = context.require_service::<temps_core::CookieCrypto>();
            let git_provider_manager = context.require_service::<GitProviderManager>();

            let registry = Arc::new(StandaloneSandboxRegistry::new(provider.clone()));
            context.register_service(registry.clone());

            let jobs = Arc::new(JobTracker::new());
            context.register_service(jobs.clone());

            let root = data_root();
            if let Err(e) = std::fs::create_dir_all(&root) {
                warn!(
                    "Sandbox plugin: failed to pre-create data root '{}': {} \
                     — creation will be attempted per sandbox",
                    root.display(),
                    e
                );
            }

            // SnapshotService (ADR-037): wired with the same provider and
            // registry the sandbox service uses. Created first so it can be
            // injected into SandboxService for the destroy→nullify path.
            let snapshot_service =
                Arc::new(SnapshotService::new(db.clone(), registry.clone(), provider));
            context.register_service(snapshot_service.clone());

            let service = Arc::new(
                SandboxService::new(
                    db.clone(),
                    registry.clone(),
                    jobs,
                    platform_config,
                    cookie_crypto,
                    git_provider_manager,
                    root,
                )
                .with_snapshot_service(snapshot_service),
            );
            context.register_service(service);

            debug!("Sandbox plugin services registered");
            Ok(())
        })
    }

    fn initialize_plugin_services<'a>(
        &'a self,
        context: &'a PluginContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(async move {
            // Recover live handles for any sandbox row still marked running.
            // Without this, a server restart leaves zombie DB rows whose
            // containers we can no longer reach until the user stops them,
            // at which point `destroy` would fail with "not found".
            use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
            use temps_entities::sandboxes;

            let registry = match context.get_service::<StandaloneSandboxRegistry>() {
                Some(r) => r,
                None => {
                    debug!("Sandbox plugin: registry absent, skipping recovery");
                    return Ok(());
                }
            };
            let db = context.require_service::<sea_orm::DatabaseConnection>();

            let sandbox_service = context.get_service::<SandboxService>();

            // Inject this plugin's service into the agents' sandbox registry
            // so agent runs (autofixer, workflow agents) create first-class
            // `sandboxes` rows instead of untracked containers.
            match (
                context.get_service::<temps_agents::services::sandbox_registry::SandboxRegistry>(),
                sandbox_service.clone(),
            ) {
                (Some(agent_registry), Some(service)) => {
                    agent_registry.set_managed(Arc::new(
                        crate::services::agent_run_bridge::AgentRunSandboxBridge::new(service),
                    ));
                    info!("Sandbox plugin: agent-run sandboxes now tracked in the sandbox API");
                }
                _ => {
                    debug!(
                        "Sandbox plugin: agents registry or sandbox service absent — \
                         agent runs keep using the raw provider"
                    );
                }
            }

            match sandboxes::Entity::find()
                .filter(sandboxes::Column::Status.eq("running"))
                .all(db.as_ref())
                .await
            {
                Ok(rows) => {
                    let entries: Vec<(i32, String)> = rows
                        .iter()
                        // Agent-run sandboxes use `temps-sandbox-<run_id>`
                        // container names and are recovered by the agents'
                        // own registry — skip them here.
                        .filter(|r| r.agent_run_id.is_none())
                        .map(|r| {
                            let label = r
                                .public_id
                                .strip_prefix("sbx_")
                                .unwrap_or(&r.public_id)
                                .to_string();
                            (r.id, label)
                        })
                        .collect();
                    if !entries.is_empty() {
                        let recovered = registry.recover_active(&entries).await;
                        info!(
                            "Sandbox plugin: recovered {}/{} standalone sandboxes on startup",
                            recovered,
                            entries.len()
                        );
                    }
                }
                Err(e) => {
                    warn!("Sandbox plugin: failed to list running sandboxes: {}", e);
                }
            }

            // Agent-run counterpart of the recovery above: runs that were
            // in flight when the server died were just marked failed by
            // `AgentRunService::recover_stuck_runs` (register phase), but
            // their `sandboxes` rows still say "running" — and both the
            // scan above and the expiration sweeper skip agent-run rows by
            // design. Release them (container destroy by run id + row flip)
            // so a restart can't leave zombie rows or leaked containers.
            match &sandbox_service {
                Some(service) => match service.release_orphaned_agent_run_sandboxes().await {
                    Ok(0) => {}
                    Ok(n) => info!(
                        "Sandbox plugin: released {} orphaned agent-run sandbox(es) on startup",
                        n
                    ),
                    Err(e) => warn!(
                        "Sandbox plugin: failed to release orphaned agent-run sandboxes: {}",
                        e
                    ),
                },
                None => {
                    warn!(
                        "Sandbox plugin: sandbox service absent — skipping orphaned \
                         agent-run sandbox cleanup"
                    );
                }
            }

            // Spawn the expiration sweeper *after* recovery so the first
            // tick has a fully-populated in-memory handle map to consult.
            // Without recovery first, the sweeper would see expired rows
            // but not find their handles in the registry — we'd still
            // flip the DB status (by design, so zombie rows don't linger)
            // but we'd skip the provider-side container stop.
            let sweeper = Arc::new(SandboxExpirationSweeper::new(db.clone(), registry.clone()));
            tokio::spawn(async move {
                sweeper.run().await;
            });

            Ok(())
        })
    }

    fn configure_routes(&self, context: &PluginContext) -> Option<PluginRoutes> {
        // If no service was registered (missing SandboxProvider), expose no
        // routes — callers will get 404 instead of a misleading 503. The
        // warn! in `register_services` is the operator-facing signal.
        let sandbox_service = context.get_service::<SandboxService>()?;

        // Optional: only present when a plugin registers a checker. Absent
        // means `project_access_guard!` is inert and the scope guard plus
        // per-sandbox ownership are the only gates — same posture as every
        // other crate that takes this dependency optionally.
        let project_access_checker = context.get_service::<dyn temps_core::ProjectAccessChecker>();

        // SnapshotService is optional at the router level: absent when the
        // sandbox provider doesn't support snapshots (Firecracker-only builds).
        // Handlers check `Option<Arc<SnapshotService>>` and return 501 when None.
        let snapshot_service = context.get_service::<SnapshotService>();

        // Audit service is optional — absent in OSS builds without the audit
        // plugin. Handlers log a warning on audit failure but never fail the
        // primary request because of it.
        let audit_service = context.get_service::<dyn temps_core::AuditLogger>();

        let app_state = Arc::new(SandboxAppState {
            sandbox_service,
            snapshot_service,
            project_access_checker,
            audit_service,
        });
        let router = configure_routes().with_state(app_state);
        Some(PluginRoutes::new(router))
    }

    fn openapi_schema(&self) -> Option<utoipa::openapi::OpenApi> {
        Some(SandboxApiDoc::openapi())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_name_is_sandbox() {
        assert_eq!(SandboxPlugin::new().name(), "sandbox");
    }

    #[test]
    fn data_root_uses_env_when_set() {
        // Not asserting exact path — just that the function doesn't panic
        // and returns something terminated by `standalone-sandboxes`.
        std::env::set_var("TEMPS_DATA_DIR", "/tmp/temps-test-xyz");
        let got = data_root();
        assert!(got.ends_with("standalone-sandboxes"));
        std::env::remove_var("TEMPS_DATA_DIR");
    }
}
