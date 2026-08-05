//! Core service for the standalone sandbox API.
//!
//! Responsibilities:
//! - Lifecycle (create/get/list/stop/extend_timeout) against the
//!   `sandboxes` DB table + the in-memory `StandaloneSandboxRegistry`.
//! - Ownership check (every operation validates `user_id` matches).
//! - Translating between the public opaque ID and the internal `i32`
//!   used by the underlying `SandboxProvider`.
//!
//! Exec/fs/domain methods live in sibling modules but are re-exported
//! here so handlers have a single service to call into.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect,
};

use temps_agents::sandbox::SandboxCreateConfig;
use temps_agents::services::run_service::TERMINAL_RUN_STATUSES;
use temps_config::ConfigService;
use temps_entities::{agent_runs, sandbox_events, sandboxes};
use temps_git::GitProviderManager;

use crate::error::{from_agent_error, SandboxError};
use crate::services::job_tracker::JobTracker;
use crate::services::preview_urls::{self, PreviewUrlParts};
use crate::services::public_id;
use crate::services::registry::StandaloneSandboxRegistry;

/// Optional initial content to seed into the sandbox after create.
/// Mirrors `@vercel/sandbox`'s `source: { type, url, revision?, username?,
/// password?, depth? }` option, plus a temps-native `git_connection_id`
/// that resolves a stored provider token server-side.
#[derive(Debug, Clone)]
pub enum SandboxSource {
    /// Clone a git repository into the sandbox work dir.
    Git {
        url: String,
        /// Branch, tag, or commit SHA. `None` → default branch.
        revision: Option<String>,
        /// Shallow-clone depth. `None` → full history.
        depth: Option<u32>,
        /// HTTP Basic username for private repos. For GitHub tokens the
        /// conventional value is `"x-access-token"`.
        username: Option<String>,
        /// HTTP Basic password / token. Paired with `username`. The token
        /// is injected via `GIT_ASKPASS` — never in the URL or argv.
        password: Option<String>,
        /// Reference to a stored git provider connection. When set, temps
        /// resolves the token server-side via `GitProviderManager` and
        /// injects it as `username="x-access-token" + password=<token>`.
        /// Mutually exclusive with `username`/`password`.
        git_connection_id: Option<i32>,
    },
    /// Download a tarball (tar, tar.gz, tgz) from `url` and extract it
    /// into the sandbox work dir. The file at `url` must be reachable
    /// from inside the container (public URL, or the container network
    /// can reach it).
    Tarball { url: String },
}

/// Input to `create_sandbox`. A subset of the `@vercel/sandbox` create
/// options — we accept what SDK clients send and ignore the rest.
#[derive(Debug, Clone, Default)]
pub struct CreateSandboxRequest {
    /// Optional Docker image override. `None` → platform default.
    pub image: Option<String>,
    /// Optional human-readable name. Defaults to the internal ID.
    pub name: Option<String>,
    /// Idle timeout in seconds. Clamped to `[60, 86400]`.
    pub timeout_secs: Option<u64>,
    /// Environment variables to bake into the container at startup.
    pub env: HashMap<String, String>,
    /// Resource limits (null → provider defaults).
    pub cpu_limit: Option<f64>,
    pub memory_limit_mb: Option<u64>,
    pub pids_limit: Option<i64>,
    /// Root disk size in MB. Only honored by the Firecracker backend
    /// (Docker ignores it). `None` uses the provider default (1 GiB).
    pub disk_size_mb: Option<u64>,
    /// Optional initial content to seed into the work dir.
    pub source: Option<SandboxSource>,
    /// Optional preview-URL password applied atomically at create. Same
    /// validation rules as `set_preview_password` (8–256 chars, argon2-
    /// hashed server-side). `None` leaves preview URLs open (public once
    /// the sandbox ID is known). The plaintext is never returned — only
    /// the last-4 hint round-trips in `SandboxSummary`.
    pub preview_password: Option<String>,
    /// Ports the sandbox is expected to listen on. Persisted to the
    /// `metadata` JSON column and surfaced as `routes[]` in the SDK-
    /// shaped responses.
    pub ports: Vec<u16>,
    /// Isolation backend: "docker" (default) or "firecracker". `None` →
    /// the host's configured default (docker unless the operator changed
    /// it). Unknown values are a validation error, and requesting a
    /// backend the host doesn't have fails create rather than downgrading.
    pub backend: Option<String>,
}

/// Output DTO — what the service returns to handlers and what handlers
/// serialize into the response JSON. Wraps the DB model to keep internal
/// columns out of the public surface.
#[derive(Debug, Clone)]
pub struct SandboxSummary {
    pub public_id: String,
    pub name: String,
    pub status: String,
    pub image: Option<String>,
    pub work_dir: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    /// Present iff a preview password is configured. The hint is the
    /// last 4 chars of the plaintext — safe to display in the UI so
    /// users can tell two passwords apart.
    pub preview_password_hint: Option<String>,
    /// Ports the sandbox advertises (read from the `metadata` JSON
    /// column). Empty when the sandbox was created without declaring
    /// any ports.
    pub ports: Vec<u16>,
    /// Isolation backend the sandbox runs on ("docker" | "firecracker").
    /// `None` on rows created before the column existed.
    pub backend: Option<String>,
    /// Configured root disk size in MB (from metadata). `None` = default.
    pub disk_size_mb: Option<u64>,
    /// Set when this sandbox executes an agent run (autofixer / workflow
    /// agent). `None` for sandboxes created via the standalone API.
    pub agent_run_id: Option<i32>,
}

impl From<&sandboxes::Model> for SandboxSummary {
    fn from(m: &sandboxes::Model) -> Self {
        Self {
            public_id: m.public_id.clone(),
            name: m.name.clone(),
            status: m.status.clone(),
            image: m.image.clone(),
            work_dir: m.work_dir.clone(),
            created_at: m.created_at,
            expires_at: m.expires_at,
            preview_password_hint: m.preview_password_hint.clone(),
            ports: ports_from_metadata(m.metadata.as_ref()),
            backend: m.backend.clone(),
            disk_size_mb: m
                .metadata
                .as_ref()
                .and_then(|v| v.get("disk_size_mb"))
                .and_then(|v| v.as_u64()),
            agent_run_id: m.agent_run_id,
        }
    }
}

fn source_kind(source: &SandboxSource) -> &'static str {
    match source {
        SandboxSource::Git { .. } => "git",
        SandboxSource::Tarball { .. } => "tarball",
    }
}

/// Extract the `ports` array from a sandbox's `metadata` JSON blob.
/// Tolerates missing/malformed data — a sandbox created before ports
/// were tracked simply returns `[]`, and a value we can't parse is
/// dropped silently rather than erroring out list reads.
fn ports_from_metadata(metadata: Option<&serde_json::Value>) -> Vec<u16> {
    metadata
        .and_then(|v| v.get("ports"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|p| p.as_u64())
                .filter_map(|p| u16::try_from(p).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Reject standalone lifecycle ops (`pause`/`resume`/`restart`/`resize`)
/// on rows attributed to an agent run. Those containers are named
/// `temps-sandbox-<run_id>` — not by the row's public id — so the
/// standalone registry's name-based recovery would miss them and the DB
/// row would drift from the real container state. The run owns the
/// lifecycle; the user must act on the run instead.
fn ensure_not_agent_run(row: &sandboxes::Model) -> Result<(), SandboxError> {
    if let Some(run_id) = row.agent_run_id {
        return Err(SandboxError::ManagedByAgentRun {
            sandbox_id: row.public_id.clone(),
            run_id,
        });
    }
    Ok(())
}

/// Is the owning agent run finished (or gone)? `None` means the run row
/// no longer exists — nothing owns the sandbox anymore, so treat it as
/// terminal and let cleanup proceed. Shared by `destroy_sandbox`'s
/// agent-run branch and `release_orphaned_agent_run_sandboxes`.
fn run_status_is_terminal(status: Option<&str>) -> bool {
    status
        .map(|s| TERMINAL_RUN_STATUSES.contains(&s))
        .unwrap_or(true)
}

/// The container-name suffix for a standalone sandbox: the `public_id` with
/// its `sbx_` prefix stripped.
///
/// The same label the preview hostname embeds, so the gateway can
/// DNS-resolve `temps-sandbox-<label>` straight from the URL — and, because
/// the Docker provider derives the home volume name from the container
/// name, the thing that decides which volume this sandbox owns. Extracted
/// so `sandbox_service_and_provider_agree_on_volume_naming` can pin the
/// cross-crate half of that agreement; the original leak was exactly two
/// places deriving one identity independently.
fn container_label_for(public_id_value: &str) -> &str {
    public_id_value
        .strip_prefix(public_id::PUBLIC_ID_PREFIX)
        .unwrap_or(public_id_value)
}

/// Which host directory (if any) `destroy_sandbox` may recursively delete
/// for a given public id.
///
/// Split out from `remove_work_dir` so the guard is unit-testable without
/// standing up a whole service. `data_root.join(s)` silently resolves
/// absolute paths and `..` segments, so a public id of `../../etc` would
/// aim a recursive delete outside the data dir entirely. Ids are generated
/// internally today, but "the caller is trusted" is not something a
/// `remove_dir_all` should rely on — accept only the exact `sbx_<16 hex>`
/// shape `public_id::generate` produces.
fn work_dir_to_remove(data_root: &Path, public_id_value: &str) -> Option<PathBuf> {
    if !public_id::is_valid(public_id_value) {
        return None;
    }
    Some(data_root.join(public_id_value))
}

/// Bounds on `timeout_secs` at the service layer. The upper bound
/// protects against "sandbox leaks" where a caller creates sandboxes
/// with absurd timeouts and relies on the server never cleaning up.
const MIN_TIMEOUT_SECS: u64 = 60;
const MAX_TIMEOUT_SECS: u64 = 24 * 60 * 60; // 24 hours
const DEFAULT_TIMEOUT_SECS: u64 = 60 * 60; // 1 hour

pub struct SandboxService {
    db: Arc<DatabaseConnection>,
    registry: Arc<StandaloneSandboxRegistry>,
    jobs: Arc<JobTracker>,
    platform_config: Arc<ConfigService>,
    cookie_crypto: Arc<temps_core::CookieCrypto>,
    /// Resolves stored provider connections to access tokens so callers
    /// can clone private repos by `git_connection_id` without handing us
    /// raw credentials. Required — the git plugin registers it, and the
    /// sandbox plugin fails to start if it's absent (`require_service`).
    git_provider_manager: Arc<GitProviderManager>,
    /// Root on the host where per-sandbox working directories are
    /// allocated. Each sandbox gets `{data_dir}/{public_id}/` bind-mounted
    /// to `/workspace` inside the container.
    data_root: PathBuf,
}

impl SandboxService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        registry: Arc<StandaloneSandboxRegistry>,
        jobs: Arc<JobTracker>,
        platform_config: Arc<ConfigService>,
        cookie_crypto: Arc<temps_core::CookieCrypto>,
        git_provider_manager: Arc<GitProviderManager>,
        data_root: PathBuf,
    ) -> Self {
        Self {
            db,
            registry,
            jobs,
            platform_config,
            cookie_crypto,
            git_provider_manager,
            data_root,
        }
    }

    pub fn registry(&self) -> &StandaloneSandboxRegistry {
        self.registry.as_ref()
    }

    pub fn jobs(&self) -> &JobTracker {
        self.jobs.as_ref()
    }

    /// Compute preview URL parts once per call. Platform settings can
    /// change while the server is live (admin edits), so we don't cache.
    pub async fn preview_parts(&self) -> PreviewUrlParts {
        preview_urls::load(&self.platform_config).await
    }

    // ── Lookups ──────────────────────────────────────────────────────────

    /// Load a sandbox row by public ID, enforcing ownership. The typical
    /// entrypoint for every op that takes an ID from the URL.
    pub async fn find_by_public_id(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<sandboxes::Model, SandboxError> {
        if !public_id::is_valid(public_id_value) {
            return Err(SandboxError::NotFound {
                sandbox_id: public_id_value.to_string(),
            });
        }
        let row = sandboxes::Entity::find()
            .filter(sandboxes::Column::PublicId.eq(public_id_value))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| SandboxError::NotFound {
                sandbox_id: public_id_value.to_string(),
            })?;
        if row.user_id != Some(user_id) {
            // Don't leak existence to non-owners.
            return Err(SandboxError::NotFound {
                sandbox_id: public_id_value.to_string(),
            });
        }
        if row.status == "destroyed" {
            return Err(SandboxError::NotFound {
                sandbox_id: public_id_value.to_string(),
            });
        }
        Ok(row)
    }

    /// List the caller's non-destroyed sandboxes, newest first.
    pub async fn list_for_user(
        &self,
        user_id: i32,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<SandboxSummary>, u64), SandboxError> {
        let page = page.unwrap_or(1).max(1);
        let page_size = page_size.unwrap_or(20).clamp(1, 100);
        let paginator = sandboxes::Entity::find()
            .filter(sandboxes::Column::UserId.eq(user_id))
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .order_by_desc(sandboxes::Column::CreatedAt)
            .paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let rows = paginator.fetch_page(page - 1).await?;
        let items = rows.iter().map(SandboxSummary::from).collect();
        Ok((items, total))
    }

    // ── Agent-run sandboxes ──────────────────────────────────────────────

    /// Release every non-destroyed sandbox row attributed to an agent run
    /// that is already terminal (or whose run row no longer exists).
    ///
    /// Called by the plugin on startup, *after* the agents plugin's
    /// `AgentRunService::recover_stuck_runs` (register phase) has failed
    /// every run that was in flight when the server died. Without this,
    /// those runs' `sandboxes` rows stay "running" forever: the standalone
    /// recovery scan and the expiration sweeper both skip agent-run rows
    /// by design, and the run itself will never call `release` again —
    /// zombie row, possibly a leaked container.
    ///
    /// Per-run failures are logged and skipped so one bad row can't block
    /// cleanup of the rest. Returns the number of runs whose sandboxes
    /// were released.
    pub async fn release_orphaned_agent_run_sandboxes(&self) -> Result<usize, SandboxError> {
        let rows = sandboxes::Entity::find()
            .filter(sandboxes::Column::AgentRunId.is_not_null())
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .all(self.db.as_ref())
            .await?;
        if rows.is_empty() {
            return Ok(0);
        }

        // One lookup for all runs (no per-row query), deduped: a run can
        // in principle have several stale rows, and `release_for_agent_run`
        // already sweeps every row for its run.
        let mut run_ids: Vec<i32> = rows.iter().filter_map(|r| r.agent_run_id).collect();
        run_ids.sort_unstable();
        run_ids.dedup();
        let runs = agent_runs::Entity::find()
            .filter(agent_runs::Column::Id.is_in(run_ids.iter().copied()))
            .all(self.db.as_ref())
            .await?;
        let status_by_run: HashMap<i32, String> =
            runs.into_iter().map(|r| (r.id, r.status)).collect();

        let mut released = 0usize;
        for run_id in run_ids {
            let terminal = run_status_is_terminal(status_by_run.get(&run_id).map(|s| s.as_str()));
            if !terminal {
                // Legitimately active run (e.g. re-triggered right after
                // restart) — its sandbox is alive and owned; leave it.
                continue;
            }
            match self.release_for_agent_run(run_id, None).await {
                Ok(()) => released += 1,
                Err(e) => {
                    tracing::warn!(
                        "orphaned agent-run sandbox cleanup: failed to release sandbox(es) \
                         for terminal agent run {}: {}",
                        run_id,
                        e
                    );
                }
            }
        }
        if released > 0 {
            tracing::info!(
                "Released sandboxes for {} terminal agent run(s) on startup",
                released
            );
        }
        Ok(released)
    }

    /// Create a sandbox for an agent run (autofixer / workflow agent) as a
    /// first-class `sandboxes` row, then create the container through the
    /// shared provider. Called by the agents' `SandboxRegistry` via the
    /// `RunSandboxService` seam (see `temps_agents::sandbox::managed`).
    ///
    /// Unlike standalone sandboxes, the container keeps the historical
    /// `temps-sandbox-<run_id>` naming (no `container_name_override`) so
    /// run recovery after a server restart keeps working, and the handle is
    /// keyed by `run_id` in the agents' registry — NOT by this row's `id`.
    /// The row is bookkeeping + API visibility; the agent run owns the
    /// lifecycle (the expiration sweeper skips rows with `agent_run_id`).
    pub async fn create_for_agent_run(
        &self,
        config: SandboxCreateConfig,
    ) -> Result<temps_agents::sandbox::SandboxHandle, SandboxError> {
        let run_id = config.run_id;

        // A run can recreate its sandbox after a dead container — retire
        // any previous non-destroyed row for the same run first so the API
        // never shows two "running" sandboxes for one run.
        let stale = sandboxes::Entity::find()
            .filter(sandboxes::Column::AgentRunId.eq(run_id))
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .all(self.db.as_ref())
            .await?;
        for row in stale {
            self.mark_destroyed(row.id).await.ok();
            self.record_event(
                row.id,
                "destroyed",
                Some(serde_json::json!({ "reason": "superseded by new container for agent run" })),
            )
            .await;
        }

        let public_id_value = public_id::generate();
        let now = Utc::now();
        let timeout = config
            .idle_timeout
            .as_secs()
            .clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS);
        let active = sandboxes::ActiveModel {
            public_id: Set(public_id_value.clone()),
            user_id: Set(config.owner_user_id),
            agent_run_id: Set(Some(run_id)),
            name: Set(format!("temps-sandbox-{}", run_id)),
            status: Set("running".to_string()),
            image: Set(config.image.clone()),
            work_dir: Set(temps_agents::sandbox::SANDBOX_WORK_DIR.to_string()),
            timeout_secs: Set(timeout as i32),
            metadata: Set(Some(serde_json::json!({ "source": "agent_run" }))),
            created_at: Set(now),
            last_activity_at: Set(now),
            expires_at: Set(now + chrono::Duration::seconds(timeout as i64)),
            ..Default::default()
        };
        let row = active.insert(self.db.as_ref()).await?;

        let handle = match self.registry.provider_arc().create(config).await {
            Ok(h) => h,
            Err(e) => {
                // Roll the row over to destroyed so the API doesn't show a
                // zombie "running" sandbox for a container that never started.
                self.mark_destroyed(row.id).await.ok();
                return Err(SandboxError::CreateFailed {
                    user_id: 0,
                    reason: format!("agent run {}: {}", run_id, e),
                });
            }
        };

        if let Err(e) = self
            .record_backend(
                row.id,
                handle.backend,
                (!handle.image.is_empty()).then(|| handle.image.clone()),
            )
            .await
        {
            tracing::warn!(
                "failed to record backend/image for agent-run sandbox {} (run {}): {}",
                public_id_value,
                run_id,
                e
            );
        }

        self.record_event(
            row.id,
            "created",
            Some(serde_json::json!({
                "agent_run_id": run_id,
                "backend": handle.backend.to_string(),
                "image": handle.image,
            })),
        )
        .await;

        tracing::info!(
            "Created agent-run sandbox {} (internal {}) for run {}",
            public_id_value,
            row.id,
            run_id
        );

        Ok(handle)
    }

    /// Destroy the container backing an agent run's sandbox and mark its
    /// row(s) destroyed. `handle` is the agents' registry cached handle
    /// when available; otherwise the container is recovered by run id.
    pub async fn release_for_agent_run(
        &self,
        run_id: i32,
        handle: Option<&temps_agents::sandbox::SandboxHandle>,
    ) -> Result<(), SandboxError> {
        let provider = self.registry.provider_arc();
        let resolved = match handle {
            Some(h) => Some(h.clone()),
            None => provider.recover(run_id).await.unwrap_or_else(|e| {
                tracing::warn!(
                    "release_for_agent_run: recover for run {} failed: {}",
                    run_id,
                    e
                );
                None
            }),
        };
        if let Some(h) = resolved {
            // Agent runs are ephemeral — purge the home volume too.
            if let Err(e) = provider.destroy(&h, true).await {
                tracing::warn!(
                    "Failed to destroy agent-run sandbox {} for run {}: {}",
                    h.sandbox_name,
                    run_id,
                    e
                );
            }
        }

        let rows = sandboxes::Entity::find()
            .filter(sandboxes::Column::AgentRunId.eq(run_id))
            .filter(sandboxes::Column::Status.ne("destroyed"))
            .all(self.db.as_ref())
            .await?;
        for row in rows {
            self.mark_destroyed(row.id).await?;
            self.record_event(
                row.id,
                "destroyed",
                Some(serde_json::json!({ "agent_run_id": run_id })),
            )
            .await;
        }
        Ok(())
    }

    // ── Lifecycle ────────────────────────────────────────────────────────

    /// Create a new standalone sandbox. Inserts the DB row first (to get
    /// the internal ID the provider indexes by), then asks the provider
    /// to create the container. On provider failure the DB row is marked
    /// "destroyed" so list doesn't show zombie entries.
    pub async fn create_sandbox(
        &self,
        user_id: i32,
        req: CreateSandboxRequest,
    ) -> Result<sandboxes::Model, SandboxError> {
        let timeout = req
            .timeout_secs
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS);
        let public_id_value = public_id::generate();
        let name = req.name.clone().unwrap_or_else(|| public_id_value.clone());
        let now = Utc::now();
        let expires_at = now + chrono::Duration::seconds(timeout as i64);

        // Validate the requested backend before any DB/container work.
        // Only the two public backends are accepted — "local" is a dev
        // fallback, never a caller choice. `None` = host default (docker
        // unless the operator changed it), so existing clients see no
        // behavior change.
        let backend = match req.backend.as_deref() {
            None => None,
            Some("docker") => Some(temps_agents::sandbox::SandboxBackend::Docker),
            Some("firecracker") => Some(temps_agents::sandbox::SandboxBackend::Firecracker),
            Some(other) => {
                return Err(SandboxError::Validation {
                    message: format!(
                        "unknown backend '{}' (expected \"docker\" or \"firecracker\")",
                        other
                    ),
                })
            }
        };

        // Fail closed if the caller asked for a backend this host can't
        // provide (e.g. `firecracker` on a Docker-only host). Isolation
        // level is a security property — silently downgrading to Docker
        // would be worse than a clear error.
        if let Some(b) = backend {
            if !self.registry.provider_arc().supports_backend(b) {
                return Err(SandboxError::Validation {
                    message: format!("backend '{}' is not available on this host", b),
                });
            }
        }

        // Validate + hash the optional preview password *before* any
        // container/workdir work starts. A caller passing junk should fail
        // fast with a 400 rather than leaving an orphan container behind.
        let preview = match req.preview_password.as_deref() {
            Some(pw) => {
                crate::services::preview_password::validate(pw)
                    .map_err(|message| SandboxError::Validation { message })?;
                let hp =
                    crate::services::preview_password::hash_password(pw).map_err(|reason| {
                        SandboxError::PasswordHashFailed {
                            sandbox_id: public_id_value.clone(),
                            reason,
                        }
                    })?;
                Some(hp)
            }
            None => None,
        };

        let metadata_value = {
            let mut meta = serde_json::Map::new();
            if !req.ports.is_empty() {
                meta.insert("ports".into(), serde_json::json!(req.ports));
            }
            if let Some(disk) = req.disk_size_mb {
                meta.insert("disk_size_mb".into(), serde_json::json!(disk));
            }
            (!meta.is_empty()).then_some(serde_json::Value::Object(meta))
        };
        let active = sandboxes::ActiveModel {
            public_id: Set(public_id_value.clone()),
            user_id: Set(Some(user_id)),
            name: Set(name.clone()),
            status: Set("running".to_string()),
            image: Set(req.image.clone()),
            work_dir: Set("/workspace".to_string()),
            timeout_secs: Set(timeout as i32),
            metadata: Set(metadata_value),
            created_at: Set(now),
            last_activity_at: Set(now),
            expires_at: Set(expires_at),
            preview_password_hash: Set(preview.as_ref().map(|p| p.hash.clone())),
            preview_password_hint: Set(preview.as_ref().map(|p| p.hint.clone())),
            ..Default::default()
        };
        let mut row = active.insert(self.db.as_ref()).await?;

        // Allocate host-side working directory.
        let host_work_dir = self.data_root.join(&public_id_value);
        if let Err(e) = tokio::fs::create_dir_all(&host_work_dir).await {
            // Roll back the DB row so a failed-to-create sandbox doesn't
            // linger as a "running" record with no container.
            self.mark_destroyed(row.id).await.ok();
            return Err(SandboxError::CreateFailed {
                user_id,
                reason: format!("create work dir: {}", e),
            });
        }

        let container_label = container_label_for(&public_id_value).to_string();

        let config = SandboxCreateConfig {
            owner_user_id: None,
            run_id: row.id,
            container_name_override: Some(container_label.clone()),
            host_work_dir,
            workspace_volume: None,
            image: req.image.clone(),
            cpu_limit: req.cpu_limit,
            memory_limit_mb: req.memory_limit_mb,
            pids_limit: req.pids_limit,
            disk_size_mb: req.disk_size_mb,
            network_mode: None,
            env_vars: req.env,
            idle_timeout: Duration::from_secs(timeout),
            backend,
        };

        let handle = match self.registry.create(config).await {
            Ok(h) => h,
            Err(e) => {
                // Tear the container down before touching the work dir.
                // A provider `create` can fail *after* the container is
                // running — the ownership-normalisation step propagates its
                // error — and containers carry `restart_policy: unless-stopped`,
                // so skipping this leaves a live container holding the
                // caller's env vars, unreachable through the API once the
                // row is destroyed, with its /workspace deleted underneath
                // it. Same order as the seeding-failure arm below.
                let _ = self.registry.destroy(row.id, &public_id_value).await;
                // The work dir was already created above; without this the
                // row goes to "destroyed" and no later `destroy_sandbox`
                // can ever reach the directory again.
                self.remove_work_dir(&public_id_value).await;
                self.mark_destroyed(row.id).await.ok();
                return Err(SandboxError::CreateFailed {
                    user_id,
                    reason: e.to_string(),
                });
            }
        };

        // Persist the *effective* backend + image the provider actually
        // used. When the request omitted them, the host default / backend
        // default decided; the handle carries the real answers. Stored so
        // the API/UI show what actually booted (e.g. "alpine:3.20") instead
        // of a vague "platform default".
        let effective_image =
            (req.image.is_none() && !handle.image.is_empty()).then(|| handle.image.clone());
        if let Err(e) = self
            .record_backend(row.id, handle.backend, effective_image.clone())
            .await
        {
            tracing::warn!(
                "failed to record backend/image for sandbox {}: {}",
                public_id_value,
                e
            );
        }
        // Mirror the recorded values into the in-memory row so the create
        // response matches what a later GET returns (record_backend wrote
        // them to the DB after the initial insert).
        row.backend = Some(handle.backend.to_string());
        if let Some(image) = effective_image {
            row.image = Some(image);
        }

        // If the caller asked us to seed the work dir, run the clone /
        // extract now. On failure we tear the sandbox down so the user
        // isn't left with a half-initialized container that's billing
        // their timeout budget.
        if let Some(source) = req.source {
            if let Err(e) = self
                .seed_source(row.id, &public_id_value, user_id, &source)
                .await
            {
                tracing::warn!(
                    "Seeding source into sandbox {} failed: {} — destroying",
                    public_id_value,
                    e
                );
                let _ = self.registry.destroy(row.id, &public_id_value).await;
                // Seeding runs after the clone/extract, so by here the work
                // dir can already hold a full repository — the largest
                // single thing this service puts on disk.
                self.remove_work_dir(&public_id_value).await;
                self.mark_destroyed(row.id).await.ok();
                return Err(e);
            }
            self.record_event(
                row.id,
                "source_seeded",
                Some(serde_json::json!({ "type": source_kind(&source) })),
            )
            .await;
        }

        self.record_event(
            row.id,
            "created",
            Some(serde_json::json!({
                "backend": row.backend,
                "image": row.image,
                "disk_size_mb": req.disk_size_mb,
            })),
        )
        .await;

        tracing::info!(
            "Created standalone sandbox {} (internal {}) for user {}",
            public_id_value,
            row.id,
            user_id
        );
        Ok(row)
    }

    /// Seed a fresh sandbox's `/workspace` with the requested content.
    /// Uses the provider's exec to keep the source-specific commands
    /// (`git`, `curl`, `tar`) out of the service crate.
    ///
    /// For git sources, credentials are injected via `GIT_ASKPASS` + a
    /// per-clone shim script rather than embedded in the URL or argv.
    /// This keeps the token out of `.git/config`, `ps`, and the provider's
    /// exec logs. The shim is shredded immediately after clone.
    pub(crate) async fn seed_source(
        &self,
        internal_id: i32,
        public_id: &str,
        user_id: i32,
        source: &SandboxSource,
    ) -> Result<(), SandboxError> {
        let handle = self
            .registry
            .get(internal_id, public_id)
            .await
            .map_err(|e| SandboxError::ExecFailed {
                sandbox_id: handle_id_fallback(internal_id),
                reason: format!("resolve handle for seed: {}", e),
            })?;

        let work_dir = handle.work_dir.to_string_lossy().to_string();

        match source {
            SandboxSource::Git {
                url,
                revision,
                depth,
                username,
                password,
                git_connection_id,
            } => {
                // Resolve credentials. Priority: explicit (username,password)
                // pair > git_connection_id > anonymous. The validator rejects
                // the "both set" combination before we get here.
                let creds = if let Some(conn_id) = git_connection_id {
                    Some(self.resolve_connection_creds(user_id, *conn_id).await?)
                } else if let (Some(u), Some(p)) = (username.as_deref(), password.as_deref()) {
                    Some((u.to_string(), p.to_string()))
                } else {
                    None
                };

                self.run_git_clone(
                    &handle,
                    internal_id,
                    &work_dir,
                    url,
                    revision.as_deref(),
                    *depth,
                    creds,
                )
                .await
            }
            SandboxSource::Tarball { url } => {
                // Stream the tarball straight into tar so we don't
                // materialize the whole archive on disk. `tar -xzf -`
                // handles both plain tar and gzip.
                let script = format!(
                    "set -eu; mkdir -p {wd} && curl -fsSL {url} | tar -C {wd} -xzf -",
                    wd = shell_escape_service(&work_dir),
                    url = shell_escape_service(url)
                );
                self.exec_seed_script(&handle, internal_id, script).await
            }
        }
    }

    /// Resolve a stored git provider connection to an HTTP-Basic
    /// (username, password) pair. Enforces that the connection is owned
    /// by the calling user so one user can't clone repos using another
    /// user's token.
    async fn resolve_connection_creds(
        &self,
        user_id: i32,
        connection_id: i32,
    ) -> Result<(String, String), SandboxError> {
        let connection = self
            .git_provider_manager
            .get_connection(connection_id)
            .await
            .map_err(|e| SandboxError::Validation {
                message: format!("Git connection {} not available: {}", connection_id, e),
            })?;

        // Ownership check. Connections without a user_id are
        // organization/platform-level and not usable from per-user
        // sandboxes.
        match connection.user_id {
            Some(owner) if owner == user_id => {}
            _ => {
                return Err(SandboxError::Validation {
                    message: format!(
                        "Git connection {} is not owned by the requesting user",
                        connection_id
                    ),
                });
            }
        }

        let token = self
            .git_provider_manager
            .get_connection_token(connection_id)
            .await
            .map_err(|e| SandboxError::ExecFailed {
                sandbox_id: format!("connection#{}", connection_id),
                reason: format!("resolve git token: {}", e),
            })?;

        // GitHub/GitLab both accept `x-access-token` as the username for
        // token-based HTTPS auth. The token goes in the password slot and
        // is injected via GIT_ASKPASS (never argv, never URL).
        Ok(("x-access-token".to_string(), token))
    }

    /// Run the actual `git clone` inside the sandbox. Credentials (if any)
    /// are injected via an ephemeral `GIT_ASKPASS` shim and exported env
    /// vars. We never log `env_map` values and never embed the token in
    /// argv or the URL.
    #[allow(clippy::too_many_arguments)]
    async fn run_git_clone(
        &self,
        handle: &temps_agents::sandbox::SandboxHandle,
        internal_id: i32,
        work_dir: &str,
        url: &str,
        revision: Option<&str>,
        depth: Option<u32>,
        creds: Option<(String, String)>,
    ) -> Result<(), SandboxError> {
        let askpass_path = "/tmp/.temps-askpass.sh";

        // Build the clone command. `-c credential.helper=` disables any
        // host-level credential helper so the password lands only via
        // the askpass shim and is never persisted to `.git/config`.
        let mut clone_cmd = String::from("git -c credential.helper= clone");
        if let Some(d) = depth {
            clone_cmd.push_str(&format!(" --depth {}", d));
        }
        if let Some(r) = revision {
            if !r.is_empty() {
                // `--branch` accepts branches and tags. For raw commit
                // SHAs this fails, and we fall back to a post-clone
                // `checkout` below.
                clone_cmd.push_str(&format!(" --branch {}", shell_escape_service(r)));
            }
        }
        clone_cmd.push_str(&format!(
            " {} {}",
            shell_escape_service(url),
            shell_escape_service(work_dir)
        ));

        // If revision didn't resolve to a branch/tag (e.g. raw SHA),
        // fall back to a post-clone checkout. Harmless when --branch
        // already did the right thing.
        let checkout_cmd = match revision {
            Some(r) if !r.is_empty() => format!(
                " || (git -C {wd} fetch origin {rev} && git -C {wd} checkout {rev})",
                wd = shell_escape_service(work_dir),
                rev = shell_escape_service(r)
            ),
            _ => String::new(),
        };

        // Compose the shell script. When creds are present we write the
        // askpass shim, chmod it, and point git at it via env; the shim
        // reads GIT_USER/GIT_PASS from its environment. We always
        // `shred`/`rm -f` the shim before returning so a subsequent user
        // shell in the sandbox can't read stale state.
        let script = if creds.is_some() {
            format!(
                "set -eu; \
                 mkdir -p {wd}; \
                 cat > {ask} <<'TEMPS_ASKPASS_EOF'\n\
#!/bin/sh\n\
case \"$1\" in\n\
  Username*) printf '%s' \"$GIT_USER\" ;;\n\
  *)         printf '%s' \"$GIT_PASS\" ;;\n\
esac\n\
TEMPS_ASKPASS_EOF\n\
                 chmod 700 {ask}; \
                 trap 'shred -u {ask} 2>/dev/null || rm -f {ask}' EXIT; \
                 GIT_ASKPASS={ask} GIT_TERMINAL_PROMPT=0 {clone}{checkout}",
                wd = shell_escape_service(work_dir),
                ask = askpass_path,
                clone = clone_cmd,
                checkout = checkout_cmd,
            )
        } else {
            format!(
                "set -eu; mkdir -p {wd}; GIT_TERMINAL_PROMPT=0 {clone}{checkout}",
                wd = shell_escape_service(work_dir),
                clone = clone_cmd,
                checkout = checkout_cmd,
            )
        };

        let mut env_map: HashMap<String, String> = HashMap::new();
        if let Some((u, p)) = creds {
            env_map.insert("GIT_USER".into(), u);
            env_map.insert("GIT_PASS".into(), p);
        }

        self.exec_seed_script_with_env(handle, internal_id, script, env_map)
            .await
    }

    /// Run a seed script with no environment overrides. Helper used by
    /// non-git sources (tarball today, potentially more later).
    async fn exec_seed_script(
        &self,
        handle: &temps_agents::sandbox::SandboxHandle,
        internal_id: i32,
        script: String,
    ) -> Result<(), SandboxError> {
        self.exec_seed_script_with_env(handle, internal_id, script, HashMap::new())
            .await
    }

    /// Execute a seed script with an env map. Never logs env values — the
    /// map may contain tokens. On non-zero exit we surface stdout/stderr
    /// but the sandbox layer scrubs them before they reach the user (the
    /// provider's exec impl is expected to honor this).
    async fn exec_seed_script_with_env(
        &self,
        handle: &temps_agents::sandbox::SandboxHandle,
        internal_id: i32,
        script: String,
        env_map: HashMap<String, String>,
    ) -> Result<(), SandboxError> {
        let cmd = vec!["sh".to_string(), "-c".to_string(), script];
        let result = self
            .registry
            .provider()
            .exec(handle, cmd, env_map, None)
            .await
            .map_err(|e| SandboxError::ExecFailed {
                sandbox_id: handle_id_fallback(internal_id),
                reason: format!("seed source exec: {}", e),
            })?;

        if result.exit_code != 0 {
            return Err(SandboxError::ExecFailed {
                sandbox_id: handle_id_fallback(internal_id),
                reason: format!(
                    "seed source exited with code {}: {}{}",
                    result.exit_code, result.stdout, result.stderr
                ),
            });
        }
        Ok(())
    }

    /// Stop + destroy a sandbox. Aborts any background jobs, asks the
    /// provider to tear down the container + volumes, and marks the
    /// DB row "destroyed".
    ///
    /// Agent-run sandboxes (`agent_run_id` set) are special-cased: their
    /// container is named `temps-sandbox-<run_id>`, so the standalone
    /// registry (which recovers by `public_id`) would miss it, flip the
    /// row to "destroyed", and leave the real container running. While
    /// the run is active we refuse with [`SandboxError::ManagedByAgentRun`]
    /// (destroying the sandbox under a live run would break it); once the
    /// run is terminal we route through `release_for_agent_run`, which
    /// targets the container by run id.
    pub async fn destroy_sandbox(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<(), SandboxError> {
        let row = self.find_by_public_id(public_id_value, user_id).await?;

        if let Some(run_id) = row.agent_run_id {
            let run = agent_runs::Entity::find_by_id(run_id)
                .one(self.db.as_ref())
                .await?;
            let terminal = run_status_is_terminal(run.as_ref().map(|r| r.status.as_str()));
            if !terminal {
                return Err(SandboxError::ManagedByAgentRun {
                    sandbox_id: public_id_value.to_string(),
                    run_id,
                });
            }
            self.jobs.abort_all(row.id).await;
            return self.release_for_agent_run(run_id, None).await;
        }

        self.jobs.abort_all(row.id).await;
        if let Err(e) = self.registry.destroy(row.id, public_id_value).await {
            // Even if the container destroy failed, mark the row
            // destroyed — otherwise the user is stuck with a zombie
            // they can't delete. Log the provider error loudly.
            tracing::error!(
                "Provider destroy failed for sandbox {} (internal {}): {} — marking row destroyed anyway",
                public_id_value,
                row.id,
                e
            );
        }
        // Remove the work dir even when the provider destroy failed. There
        // is no background sweep to fall back on — this call is the only
        // thing that ever frees the directory, so skipping it here means
        // leaking it permanently, which is the bug this fixes. The tradeoff
        // is that a failed destroy may leave a container still running with
        // this directory bind-mounted at /workspace, and the delete then
        // races that writer. Deleting is still the right call: the user
        // explicitly asked for the sandbox to be gone, and the row is
        // marked destroyed either way.
        self.remove_work_dir(public_id_value).await;
        self.record_event(row.id, "destroyed", None).await;
        self.mark_destroyed(row.id).await?;
        Ok(())
    }

    /// Delete the host-side `/workspace` directory backing a destroyed
    /// sandbox.
    ///
    /// `create_sandbox` makes `data_root/<public_id>` and bind-mounts it
    /// into the container; nothing used to remove it, so every
    /// create+destroy cycle left the full working tree (node_modules, build
    /// output, cloned repos — routinely hundreds of MB) on the host
    /// forever. Deliberately best-effort and non-fatal: the sandbox is
    /// already gone from the user's point of view, and failing the API call
    /// over leftover bytes would strand the row instead.
    ///
    /// Only called for standalone sandboxes. Agent-run sandboxes get their
    /// work dir from the executor, which owns that directory's lifecycle.
    async fn remove_work_dir(&self, public_id_value: &str) {
        let Some(work_dir) = work_dir_to_remove(&self.data_root, public_id_value) else {
            tracing::warn!(
                "Refusing to remove sandbox work dir for malformed public id {:?}",
                public_id_value
            );
            return;
        };
        match tokio::fs::remove_dir_all(&work_dir).await {
            Ok(()) => tracing::debug!("Removed sandbox work dir {}", work_dir.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(
                "Failed to remove sandbox work dir {}: {} — disk space stays claimed",
                work_dir.display(),
                e
            ),
        }
    }

    /// Pause a running sandbox (non-destructive). Stops the underlying
    /// container but leaves the DB row, volumes, and filesystem intact so
    /// the user can resume later. Idempotent on already-stopped sandboxes.
    pub async fn pause_sandbox(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<sandboxes::Model, SandboxError> {
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        ensure_not_agent_run(&row)?;
        if row.status == "stopped" {
            return Ok(row);
        }
        if row.status != "running" {
            return Err(SandboxError::InvalidState {
                sandbox_id: public_id_value.to_string(),
                state: row.status.clone(),
                operation: "pause".into(),
            });
        }
        self.jobs.abort_all(row.id).await;
        self.registry
            .stop(row.id, public_id_value)
            .await
            .map_err(|e| from_agent_error(public_id_value, e))?;
        let now = Utc::now();
        let sandbox_id = row.id;
        let mut active: sandboxes::ActiveModel = row.into();
        active.status = Set("stopped".to_string());
        active.last_activity_at = Set(now);
        let updated = active.update(self.db.as_ref()).await?;
        self.record_event(sandbox_id, "stopped", None).await;
        Ok(updated)
    }

    /// Resume a paused sandbox. Restarts the container and bumps
    /// `expires_at` to `now + timeout_secs` so the user gets a fresh
    /// idle window. Idempotent on already-running sandboxes.
    pub async fn resume_sandbox(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<sandboxes::Model, SandboxError> {
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        ensure_not_agent_run(&row)?;
        if row.status == "running" {
            return Ok(row);
        }
        if row.status != "stopped" {
            return Err(SandboxError::InvalidState {
                sandbox_id: public_id_value.to_string(),
                state: row.status.clone(),
                operation: "resume".into(),
            });
        }
        self.registry
            .start(row.id, public_id_value)
            .await
            .map_err(|e| from_agent_error(public_id_value, e))?;
        let now = Utc::now();
        let new_expires = now + chrono::Duration::seconds(row.timeout_secs as i64);
        let sandbox_id = row.id;
        let mut active: sandboxes::ActiveModel = row.into();
        active.status = Set("running".to_string());
        active.last_activity_at = Set(now);
        active.expires_at = Set(new_expires);
        let updated = active.update(self.db.as_ref()).await?;
        self.record_event(sandbox_id, "resumed", None).await;
        Ok(updated)
    }

    /// Restart a running sandbox in-place (stop + start). Filesystem and
    /// volumes survive. Rejected on stopped sandboxes (use resume instead).
    pub async fn restart_sandbox(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<sandboxes::Model, SandboxError> {
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        ensure_not_agent_run(&row)?;
        if row.status != "running" {
            return Err(SandboxError::InvalidState {
                sandbox_id: public_id_value.to_string(),
                state: row.status.clone(),
                operation: "restart".into(),
            });
        }
        self.jobs.abort_all(row.id).await;
        self.registry
            .restart(row.id, public_id_value)
            .await
            .map_err(|e| from_agent_error(public_id_value, e))?;
        let now = Utc::now();
        let sandbox_id = row.id;
        let mut active: sandboxes::ActiveModel = row.into();
        active.last_activity_at = Set(now);
        let updated = active.update(self.db.as_ref()).await?;
        self.record_event(sandbox_id, "restarted", None).await;
        Ok(updated)
    }

    /// Grow the sandbox's root disk to `new_size_mb`. Firecracker only —
    /// the resize is done offline (stop → grow ext4 → start), so the VM
    /// reboots but its filesystem and data survive. Grow-only. Records a
    /// `resized` event and persists the new size in `metadata`.
    pub async fn resize_sandbox(
        &self,
        public_id_value: &str,
        user_id: i32,
        new_size_mb: u64,
    ) -> Result<sandboxes::Model, SandboxError> {
        const MIN_DISK_MB: u64 = 256;
        const MAX_DISK_MB: u64 = 64 * 1024;
        if !(MIN_DISK_MB..=MAX_DISK_MB).contains(&new_size_mb) {
            return Err(SandboxError::Validation {
                message: format!(
                    "disk_size_mb must be between {} and {}",
                    MIN_DISK_MB, MAX_DISK_MB
                ),
            });
        }
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        ensure_not_agent_run(&row)?;
        if row.backend.as_deref() != Some("firecracker") {
            return Err(SandboxError::Validation {
                message: "disk resize is only supported on Firecracker sandboxes".into(),
            });
        }
        let old_mb = row
            .metadata
            .as_ref()
            .and_then(|v| v.get("disk_size_mb"))
            .and_then(|v| v.as_u64())
            .unwrap_or(1024);
        let sandbox_id = row.id;

        self.registry
            .resize_disk(row.id, public_id_value, new_size_mb)
            .await
            .map_err(|e| from_agent_error(public_id_value, e))?;

        // Persist the new size into metadata (preserving ports).
        let mut meta = row
            .metadata
            .clone()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        meta.insert("disk_size_mb".into(), serde_json::json!(new_size_mb));
        let mut active: sandboxes::ActiveModel = row.into();
        active.metadata = Set(Some(serde_json::Value::Object(meta)));
        active.last_activity_at = Set(Utc::now());
        let updated = active.update(self.db.as_ref()).await?;

        self.record_event(
            sandbox_id,
            "resized",
            Some(serde_json::json!({ "from_mb": old_mb, "to_mb": new_size_mb })),
        )
        .await;
        Ok(updated)
    }

    /// Seed an already-running sandbox with additional content. Mirrors
    /// the SDK's ability to attach a source *after* create; useful when
    /// the caller wants to clone a repo using a token that wasn't
    /// available at create time, or to layer a second repo on top.
    ///
    /// Rejects non-running sandboxes with `InvalidState`. The underlying
    /// `seed_source` applies the same credential-safe flow used on create.
    pub async fn clone_source(
        &self,
        public_id_value: &str,
        user_id: i32,
        source: &SandboxSource,
    ) -> Result<sandboxes::Model, SandboxError> {
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        if row.status != "running" {
            return Err(SandboxError::InvalidState {
                sandbox_id: public_id_value.to_string(),
                state: row.status.clone(),
                operation: "clone source".into(),
            });
        }
        self.seed_source(row.id, &row.public_id, user_id, source)
            .await?;
        let mut active: sandboxes::ActiveModel = row.into();
        active.last_activity_at = Set(Utc::now());
        let updated = active.update(self.db.as_ref()).await?;
        Ok(updated)
    }

    /// Extend the sandbox's `expires_at` by `extra_secs`. Does not
    /// change `timeout_secs` — just pushes the deadline forward. Used
    /// by the SDK's `extendTimeout()` so long-running operations can
    /// keep the sandbox alive without recreating it.
    pub async fn extend_timeout(
        &self,
        public_id_value: &str,
        user_id: i32,
        extra_secs: u64,
    ) -> Result<sandboxes::Model, SandboxError> {
        if extra_secs == 0 {
            return Err(SandboxError::Validation {
                message: "extra_secs must be greater than zero".into(),
            });
        }
        if extra_secs > MAX_TIMEOUT_SECS {
            return Err(SandboxError::Validation {
                message: format!(
                    "extra_secs {} exceeds maximum of {}",
                    extra_secs, MAX_TIMEOUT_SECS
                ),
            });
        }
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        let sandbox_id = row.id;
        let new_expires = row.expires_at + chrono::Duration::seconds(extra_secs as i64);
        let mut active: sandboxes::ActiveModel = row.into();
        active.expires_at = Set(new_expires);
        active.last_activity_at = Set(Utc::now());
        let updated = active.update(self.db.as_ref()).await?;
        self.record_event(
            sandbox_id,
            "timeout_extended",
            Some(serde_json::json!({
                "extra_secs": extra_secs,
                "expires_at": new_expires.to_rfc3339(),
            })),
        )
        .await;
        Ok(updated)
    }

    /// Update `last_activity_at` — called by exec/fs ops to keep the
    /// expiry sweeper honest. Swallows DB errors because activity
    /// bumps are best-effort.
    pub async fn touch(&self, sandbox_id: i32) {
        let now = Utc::now();
        let active = sandboxes::ActiveModel {
            id: Set(sandbox_id),
            last_activity_at: Set(now),
            ..Default::default()
        };
        if let Err(e) = active.update(self.db.as_ref()).await {
            tracing::debug!(
                "touch: failed to bump last_activity_at for {}: {}",
                sandbox_id,
                e
            );
        }
    }

    async fn mark_destroyed(&self, id: i32) -> Result<(), SandboxError> {
        let now = Utc::now();
        let active = sandboxes::ActiveModel {
            id: Set(id),
            status: Set("destroyed".to_string()),
            last_activity_at: Set(now),
            ..Default::default()
        };
        active.update(self.db.as_ref()).await?;
        Ok(())
    }

    /// Rootfs storage inventory across backends (Firecracker cache + per-VM
    /// disks). Powers the management API's inspection endpoint.
    pub async fn rootfs_report(&self) -> Result<temps_agents::sandbox::RootfsReport, SandboxError> {
        self.registry
            .provider_arc()
            .rootfs_report()
            .await
            .map_err(|e| SandboxError::Unavailable {
                reason: format!("rootfs report: {}", e),
            })
    }

    /// Reclaim rootfs cache entries not backing any live sandbox.
    pub async fn gc_rootfs(&self) -> Result<temps_agents::sandbox::RootfsGcReport, SandboxError> {
        self.registry
            .provider_arc()
            .gc_rootfs()
            .await
            .map_err(|e| SandboxError::Unavailable {
                reason: format!("rootfs gc: {}", e),
            })
    }

    /// Append one operation to a sandbox's timeline. Best-effort — a failed
    /// insert is logged, never fatal to the operation it records.
    async fn record_event(
        &self,
        sandbox_id: i32,
        event_type: &str,
        detail: Option<serde_json::Value>,
    ) {
        let active = sandbox_events::ActiveModel {
            sandbox_id: Set(sandbox_id),
            event_type: Set(event_type.to_string()),
            detail: Set(detail),
            created_at: Set(Utc::now()),
            ..Default::default()
        };
        if let Err(e) = active.insert(self.db.as_ref()).await {
            tracing::warn!(
                "failed to record '{}' event for sandbox {}: {}",
                event_type,
                sandbox_id,
                e
            );
        }
    }

    /// The operations timeline for a sandbox, newest first. Ownership is
    /// enforced by resolving the sandbox through `find_by_public_id`.
    pub async fn list_events(
        &self,
        public_id_value: &str,
        user_id: i32,
        limit: u64,
    ) -> Result<Vec<sandbox_events::Model>, SandboxError> {
        // Bounded — the timeline is append-only and a UI only ever shows the
        // most recent slice. Cap the page so a long-lived sandbox can't force
        // an unbounded scan.
        let limit = limit.clamp(1, 500);
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        let events = sandbox_events::Entity::find()
            .filter(sandbox_events::Column::SandboxId.eq(row.id))
            .order_by_desc(sandbox_events::Column::CreatedAt)
            .order_by_desc(sandbox_events::Column::Id)
            .limit(limit)
            .all(self.db.as_ref())
            .await?;
        Ok(events)
    }

    /// Persist the effective backend (and, when the request didn't specify
    /// one, the resolved image) a sandbox runs on.
    async fn record_backend(
        &self,
        id: i32,
        backend: temps_agents::sandbox::SandboxBackend,
        effective_image: Option<String>,
    ) -> Result<(), SandboxError> {
        let mut active = sandboxes::ActiveModel {
            id: Set(id),
            backend: Set(Some(backend.to_string())),
            ..Default::default()
        };
        if let Some(image) = effective_image {
            active.image = Set(Some(image));
        }
        active.update(self.db.as_ref()).await?;
        Ok(())
    }

    // ── Preview password ────────────────────────────────────────────────

    /// Set (or rotate) the preview password for a sandbox. The plaintext
    /// is hashed with argon2id and only the hash + last-4 hint are stored.
    /// Returns the hint to the caller — the plaintext is never persisted
    /// or echoed back (the caller already has it).
    ///
    /// Rotating an existing password invalidates every live preview cookie
    /// immediately: the proxy folds a digest of the argon2 hash into the
    /// cookie payload, so a new hash = a new fingerprint = every existing
    /// cookie fails verification.
    pub async fn set_preview_password(
        &self,
        public_id_value: &str,
        user_id: i32,
        plaintext: &str,
    ) -> Result<String, SandboxError> {
        crate::services::preview_password::validate(plaintext)
            .map_err(|message| SandboxError::Validation { message })?;
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        let hp = crate::services::preview_password::hash_password(plaintext).map_err(|reason| {
            SandboxError::PasswordHashFailed {
                sandbox_id: public_id_value.to_string(),
                reason,
            }
        })?;
        let sandbox_id = row.id;
        let mut active: sandboxes::ActiveModel = row.into();
        active.preview_password_hash = Set(Some(hp.hash));
        active.preview_password_hint = Set(Some(hp.hint.clone()));
        active.update(self.db.as_ref()).await?;
        self.record_event(sandbox_id, "preview_password_set", None)
            .await;
        Ok(hp.hint)
    }

    /// Remove the preview password. Subsequent preview requests fall back
    /// to URL-only protection (the unguessable hex public_id). Idempotent —
    /// clearing an already-unset password is a no-op, not an error.
    pub async fn clear_preview_password(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<(), SandboxError> {
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        let had_password = row.preview_password_hash.is_some();
        let sandbox_id = row.id;
        let mut active: sandboxes::ActiveModel = row.into();
        active.preview_password_hash = Set(None);
        active.preview_password_hint = Set(None);
        active.update(self.db.as_ref()).await?;
        if had_password {
            self.record_event(sandbox_id, "preview_password_cleared", None)
                .await;
        }
        Ok(())
    }

    // ── Helpers shared with the exec/fs modules ──────────────────────────

    /// Load + authorize + return the internal ID, or a typed error that
    /// already includes the public ID. Exec/fs modules call this first.
    /// Rejects stopped sandboxes with `InvalidState` (→ HTTP 409) — the
    /// user must call `/resume` before running commands.
    pub async fn resolve_id(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<(sandboxes::Model, i32), SandboxError> {
        let row = self.find_by_public_id(public_id_value, user_id).await?;
        if row.status == "stopped" {
            return Err(SandboxError::InvalidState {
                sandbox_id: public_id_value.to_string(),
                state: row.status.clone(),
                operation: "exec or filesystem operation".into(),
            });
        }
        let id = row.id;
        Ok((row, id))
    }

    /// Build a typed provider error into a `SandboxError` carrying the
    /// public ID. Thin wrapper — module-private modules call this.
    pub(crate) fn provider_err(
        public_id_value: &str,
        err: temps_agents::error::AgentError,
    ) -> SandboxError {
        from_agent_error(public_id_value, err)
    }

    // ── Preview URL (`sandbox.domain(port)`) ─────────────────────────────

    /// Resolve the public URL for a port inside the sandbox. Returns the
    /// same `ws-<id>-<port>.<domain>` hostname the preview gateway already
    /// routes for workspace sessions, so standalone sandboxes don't
    /// require any gateway changes.
    ///
    /// Validation: `port` must be in [1, 65535]. Port `0` is rejected
    /// because the gateway matches exact numbers — surfacing a URL with
    /// `port=0` would be useless.
    pub async fn domain(
        &self,
        public_id_value: &str,
        user_id: i32,
        port: u16,
    ) -> Result<String, SandboxError> {
        if port == 0 {
            return Err(SandboxError::Validation {
                message: "port must be between 1 and 65535".into(),
            });
        }
        // Ownership + validity check. The numeric id is intentionally
        // discarded — the preview URL never embeds it.
        let _ = self.resolve_id(public_id_value, user_id).await?;
        let parts = self.preview_parts().await;
        Ok(parts.url_for(public_id_value, port))
    }

    /// Build a shareable preview link that carries its own authorization.
    ///
    /// `domain()` returns the bare preview URL, which is useless to anyone who
    /// does not already hold the preview password — so sharing a protected
    /// preview meant sharing the password itself, which then cannot be revoked
    /// for one recipient. This returns that URL plus a minted `session_grant`
    /// aimed at the login bridge: the recipient's browser exchanges it for the
    /// ordinary preview cookie and lands on `path`.
    ///
    /// The grant is scoped to this one sandbox, expires, and is never
    /// forwarded to the sandbox itself.
    pub async fn preview_share_link(
        &self,
        public_id_value: &str,
        user_id: i32,
        port: u16,
        path: &str,
        ttl: Duration,
    ) -> Result<(String, u64), SandboxError> {
        if port == 0 {
            return Err(SandboxError::Validation {
                message: "port must be between 1 and 65535".into(),
            });
        }
        let (row, _) = self.resolve_id(public_id_value, user_id).await?;
        let password_hash =
            row.preview_password_hash
                .as_deref()
                .ok_or_else(|| SandboxError::InvalidState {
                    sandbox_id: public_id_value.to_string(),
                    state: "preview password disabled".into(),
                    operation: "mint an expiring preview link; set a preview password first".into(),
                })?;

        let now = SystemTime::now();
        let (grant, expires_at) = temps_core::encode_preview_session_grant(
            &self.cookie_crypto,
            public_id_value,
            password_hash,
            ttl,
            now,
        )
        .map_err(|source| SandboxError::PreviewGrantFailed {
            sandbox_id: public_id_value.to_string(),
            source,
        })?;
        let granted_ttl = ttl.min(temps_core::PREVIEW_SESSION_GRANT_MAX_TTL);

        let next = temps_core::sanitize_preview_next(path);

        let base = self.preview_parts().await.url_for(public_id_value, port);
        let url = format!(
            "{}/__temps/preview/login?grant=1&next={}#session_grant={}",
            base.trim_end_matches('/'),
            url::form_urlencoded::byte_serialize(next.as_bytes()).collect::<String>(),
            url::form_urlencoded::byte_serialize(grant.as_bytes()).collect::<String>(),
        );
        self.record_event(
            row.id,
            "preview_share_link_created",
            Some(serde_json::json!({
                "actor_user_id": user_id,
                "port": port,
                "path": next,
                "ttl_seconds": granted_ttl.as_secs(),
                "expires_at": expires_at,
            })),
        )
        .await;
        Ok((url, expires_at))
    }
}

/// Placeholder "public id" used in error messages when the source-seed
/// step fails before we propagate it upward. We already mapped the
/// real public ID into the top-level Create error; this just gives the
/// inner ExecFailed a non-empty identifier.
fn handle_id_fallback(internal_id: i32) -> String {
    format!("sandbox#{}", internal_id)
}

/// POSIX-style single-quoted escape for embedding into `sh -c` scripts
/// from the service layer. Duplicated from `services::exec::shell_escape`
/// so we don't introduce a module cycle for a 10-line helper.
fn shell_escape_service(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:@".contains(c))
    {
        s.to_string()
    } else {
        let escaped = s.replace('\'', "'\\''");
        format!("'{}'", escaped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_core::{Job, JobQueue, JobReceiver, QueueError};

    struct NoopJobQueue;

    #[async_trait::async_trait]
    impl JobQueue for NoopJobQueue {
        async fn send(&self, _job: Job) -> Result<(), QueueError> {
            Ok(())
        }

        fn subscribe(&self) -> Box<dyn JobReceiver> {
            panic!("preview link tests never subscribe to the job queue")
        }
    }

    fn test_server_config() -> temps_config::ServerConfig {
        temps_config::ServerConfig {
            address: "127.0.0.1:0".into(),
            database_url: "postgres://test".into(),
            tls_address: None,
            console_address: "127.0.0.1:0".into(),
            console_admin_address: None,
            admin_allowed_ips: Vec::new(),
            admin_allowed_hosts: Vec::new(),
            admin_trust_forwarded_for: false,
            data_dir: PathBuf::from("/tmp/temps-sandbox-preview-tests"),
            auth_secret: "default-32-byte-key-for-testing!".into(),
            encryption_key: "another-32-byte-key-for-testing!".into(),
            api_base_url: "/api".into(),
            postgres_max_connections: None,
            postgres_min_connections: None,
            postgres_connect_timeout_secs: None,
            postgres_acquire_timeout_secs: None,
            postgres_idle_timeout_secs: None,
            postgres_max_lifetime_secs: None,
            clickhouse_url: None,
            clickhouse_database: None,
            clickhouse_user: None,
            clickhouse_password: None,
            docker_extra_networks: Vec::new(),
        }
    }

    fn preview_test_service(
        query_results: Vec<Vec<sandboxes::Model>>,
    ) -> (SandboxService, Arc<DatabaseConnection>) {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(query_results)
                .into_connection(),
        );
        let service = preview_test_service_with_db(db.clone());
        (service, db)
    }

    fn preview_test_service_with_db(db: Arc<DatabaseConnection>) -> SandboxService {
        let platform_config = Arc::new(ConfigService::new(
            Arc::new(test_server_config()),
            db.clone(),
        ));
        let git_provider_manager = Arc::new(GitProviderManager::new(
            db.clone(),
            Arc::new(temps_core::EncryptionService::new_from_password("test")),
            Arc::new(NoopJobQueue),
            platform_config.clone(),
        ));
        let registry = Arc::new(StandaloneSandboxRegistry::new(Arc::new(
            temps_agents::sandbox::local::LocalSandboxProvider::new(),
        )));
        SandboxService::new(
            db.clone(),
            registry,
            Arc::new(JobTracker::new()),
            platform_config,
            Arc::new(
                temps_core::CookieCrypto::new("default-32-byte-key-for-testing!")
                    .expect("valid test cookie key"),
            ),
            git_provider_manager,
            PathBuf::from("/tmp/temps-sandbox-preview-tests"),
        )
    }

    #[test]
    fn default_request_is_empty() {
        let r = CreateSandboxRequest::default();
        assert!(r.image.is_none());
        assert!(r.env.is_empty());
        assert!(r.timeout_secs.is_none());
        assert!(r.preview_password.is_none());
    }

    #[test]
    fn request_carries_preview_password() {
        // The field is plumbed through the service input DTO so handlers
        // don't need to reach around the service to wire it in.
        let r = CreateSandboxRequest {
            preview_password: Some("hunter2secret".to_string()),
            ..Default::default()
        };
        assert_eq!(r.preview_password.as_deref(), Some("hunter2secret"));
    }

    /// Destroy must actually target the directory create allocated —
    /// nothing removed it before, so every create/destroy cycle left the
    /// full working tree (node_modules, build output, cloned repos) on the
    /// host and the disk filled up with directories no API could reach.
    #[test]
    fn work_dir_to_remove_targets_the_directory_create_allocated() {
        let root = Path::new("/var/lib/temps/sandboxes");
        let id = "sbx_deadbeef01234567";
        assert_eq!(
            work_dir_to_remove(root, id),
            // Same expression create_sandbox uses to build host_work_dir.
            Some(root.join(id))
        );
    }

    /// The guard in front of a recursive delete. A public id is only ever
    /// generated internally, but if a malformed one ever reached here
    /// `data_root.join()` would happily resolve out of the data dir — an
    /// absolute id replaces the root outright.
    #[test]
    fn work_dir_to_remove_refuses_ids_that_escape_the_data_root() {
        let root = Path::new("/var/lib/temps/sandboxes");
        for bad in [
            "../../etc",
            "sbx_../../etc",
            "/etc",
            "sbx_",
            "sbx_short",
            "sbx_zzzzzzzzzzzzzzzz",
            "",
        ] {
            assert_eq!(
                work_dir_to_remove(root, bad),
                None,
                "{:?} must not resolve to a directory we would recursively delete",
                bad
            );
        }
    }

    /// The cross-crate half of the naming agreement.
    ///
    /// `temps-sandbox` decides the container label; `temps-agents` turns a
    /// container name into a volume name. Nothing at the type level forces
    /// those to line up, and the leak this PR fixes was exactly one identity
    /// derived independently in two places. If either side changes its
    /// scheme without the other, this goes red — and it needs no Docker.
    #[test]
    fn sandbox_service_and_provider_agree_on_volume_naming() {
        let public_id = "sbx_a1b2c3d4e5f60718";
        let label = container_label_for(public_id);
        assert_eq!(label, "a1b2c3d4e5f60718", "label is public_id minus sbx_");

        // What the provider will name the volume for a container built from
        // this label, i.e. what `destroy` will try to remove.
        assert_eq!(
            temps_agents::sandbox::home_volume_name_for_label(label),
            "temps-sandbox-home-v2-a1b2c3d4e5f60718"
        );

        // And it must not be a name from the pre-fix namespace, where a
        // stranded volume from another tenant could still be sitting.
        assert!(!temps_agents::sandbox::home_volume_name_for_label(label)
            .trim_start_matches("temps-sandbox-home-")
            .chars()
            .all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn timeout_constants_are_sane() {
        const _: () = assert!(MIN_TIMEOUT_SECS < DEFAULT_TIMEOUT_SECS);
        const _: () = assert!(DEFAULT_TIMEOUT_SECS < MAX_TIMEOUT_SECS);
        assert_eq!(MAX_TIMEOUT_SECS, 86400);
    }

    fn agent_run_row(run_id: Option<i32>) -> sandboxes::Model {
        let now = Utc::now();
        sandboxes::Model {
            id: 7,
            public_id: "sbx_deadbeef01234567".into(),
            user_id: Some(1),
            agent_run_id: run_id,
            name: match run_id {
                Some(id) => format!("temps-sandbox-{}", id),
                None => "sbx-7".into(),
            },
            status: "running".into(),
            image: None,
            work_dir: "/workspace".into(),
            timeout_secs: 3600,
            metadata: None,
            backend: None,
            created_at: now,
            last_activity_at: now,
            expires_at: now,
            preview_password_hash: None,
            preview_password_hint: None,
        }
    }

    fn protected_preview_row(user_id: i32) -> sandboxes::Model {
        let mut row = agent_run_row(None);
        row.user_id = Some(user_id);
        row.preview_password_hash = Some("argon2-current".into());
        row.preview_password_hint = Some("rent".into());
        row
    }

    #[tokio::test]
    async fn preview_share_link_uses_fragment_clamps_ttl_and_records_safe_event() {
        let row = protected_preview_row(1);
        let (service, db) = preview_test_service(vec![vec![row]]);

        let before = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("test clock after epoch")
            .as_secs();
        let (url, expires_at) = service
            .preview_share_link(
                "sbx_deadbeef01234567",
                1,
                3000,
                "/\\evil.example",
                temps_core::PREVIEW_SESSION_GRANT_MAX_TTL * 30,
            )
            .await
            .expect("protected sandbox should mint a link");

        assert!(url.starts_with(
            "https://ws-deadbeef01234567-3000.localho.st/__temps/preview/login?grant=1&next=%2F#session_grant="
        ));
        assert!(!url.contains("?session_grant="));
        assert!(expires_at >= before + temps_core::PREVIEW_SESSION_GRANT_MAX_TTL.as_secs());
        assert!(expires_at <= before + temps_core::PREVIEW_SESSION_GRANT_MAX_TTL.as_secs() + 1);

        drop(service);
        let log = Arc::try_unwrap(db)
            .expect("service owns no database references after drop")
            .into_transaction_log();
        let statements = format!("{log:?}");
        assert!(statements.contains("sandbox_events"), "log: {statements}");
        assert!(
            statements.contains("preview_share_link_created"),
            "log: {statements}"
        );
        assert!(!statements.contains("session_grant"), "log: {statements}");
    }

    #[tokio::test]
    async fn preview_share_link_requires_a_preview_password() {
        let row = agent_run_row(None);
        let (service, _) = preview_test_service(vec![vec![row]]);

        let error = service
            .preview_share_link(
                "sbx_deadbeef01234567",
                1,
                3000,
                "/pricing",
                Duration::from_secs(60),
            )
            .await
            .expect_err("open sandbox must not receive a non-expiring share link");

        assert!(matches!(error, SandboxError::InvalidState { .. }));
        assert!(error.to_string().contains("set a preview password first"));
    }

    #[tokio::test]
    async fn preview_share_link_hides_cross_user_sandboxes() {
        let row = protected_preview_row(2);
        let (service, _) = preview_test_service(vec![vec![row]]);

        let error = service
            .preview_share_link(
                "sbx_deadbeef01234567",
                1,
                3000,
                "/",
                Duration::from_secs(60),
            )
            .await
            .expect_err("another user's sandbox must stay hidden");

        assert!(matches!(error, SandboxError::NotFound { .. }));
    }

    #[tokio::test]
    async fn preview_share_link_rejects_zero_port_before_database_access() {
        let (service, _) = preview_test_service(Vec::new());

        let error = service
            .preview_share_link("sbx_deadbeef01234567", 1, 0, "/", Duration::from_secs(60))
            .await
            .expect_err("port zero must be rejected");

        assert!(matches!(error, SandboxError::Validation { .. }));
    }

    #[tokio::test]
    async fn preview_share_link_preserves_database_errors() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors([sea_orm::DbErr::Custom(
                    "preview lookup unavailable".to_string(),
                )])
                .into_connection(),
        );
        let service = preview_test_service_with_db(db);

        let error = service
            .preview_share_link(
                "sbx_deadbeef01234567",
                1,
                3000,
                "/",
                Duration::from_secs(60),
            )
            .await
            .expect_err("database failures must not be collapsed into not-found");

        assert!(matches!(error, SandboxError::Database(_)));
        assert!(error.to_string().contains("preview lookup unavailable"));
    }

    /// Lifecycle ops on agent-run rows must be rejected with a typed
    /// error naming both the sandbox and the run — pre-fix, pause/resume/
    /// restart went through the standalone registry, missed the run's
    /// `temps-sandbox-<run_id>` container, and let the DB row drift from
    /// the real container state.
    #[test]
    fn ensure_not_agent_run_rejects_attributed_rows() {
        let row = agent_run_row(Some(42));
        let err = ensure_not_agent_run(&row).expect_err("agent-run row must be rejected");
        match err {
            SandboxError::ManagedByAgentRun { sandbox_id, run_id } => {
                assert_eq!(sandbox_id, "sbx_deadbeef01234567");
                assert_eq!(run_id, 42);
            }
            other => panic!("expected ManagedByAgentRun, got {:?}", other),
        }
    }

    #[test]
    fn ensure_not_agent_run_allows_standalone_rows() {
        let row = agent_run_row(None);
        assert!(ensure_not_agent_run(&row).is_ok());
    }

    /// Terminal decision shared by `destroy_sandbox` (agent-run branch)
    /// and `release_orphaned_agent_run_sandboxes`: every terminal status
    /// allows cleanup, every active status keeps the run's ownership, and
    /// a missing run row (deleted run) counts as terminal so the sandbox
    /// isn't orphaned forever.
    #[test]
    fn run_status_is_terminal_matches_run_lifecycle() {
        for s in TERMINAL_RUN_STATUSES {
            assert!(run_status_is_terminal(Some(s)), "{} must be terminal", s);
        }
        for s in temps_agents::services::run_service::ACTIVE_RUN_STATUSES {
            assert!(
                !run_status_is_terminal(Some(s)),
                "{} must NOT be terminal",
                s
            );
        }
        assert!(
            run_status_is_terminal(None),
            "missing run row must count as terminal so cleanup can proceed"
        );
    }

    #[test]
    fn summary_from_model_copies_fields() {
        let now = Utc::now();
        let m = sandboxes::Model {
            id: 1_000_042,
            public_id: "sbx_abc1234567890def".into(),
            user_id: Some(7),
            agent_run_id: Some(42),
            name: "my-sbx".into(),
            status: "running".into(),
            image: Some("node:20".into()),
            work_dir: "/workspace".into(),
            timeout_secs: 3600,
            metadata: None,
            backend: None,
            created_at: now,
            last_activity_at: now,
            expires_at: now,
            preview_password_hash: None,
            preview_password_hint: None,
        };
        let s = SandboxSummary::from(&m);
        assert_eq!(s.public_id, "sbx_abc1234567890def");
        assert_eq!(s.status, "running");
        assert_eq!(s.image.as_deref(), Some("node:20"));
        assert_eq!(s.agent_run_id, Some(42));
    }
}

/// Service-level tests for the storage-cleanup paths.
///
/// These exist because the unit tests above only cover the *guard*
/// (`work_dir_to_remove`), not the call sites — and the call sites are where
/// the bug was. Deleting the `remove_work_dir` call from `destroy_sandbox`,
/// or from either create-failure arm, used to leave every test green.
///
/// No Docker daemon required: the provider is faked and the database is
/// `MockDatabase`. The work dir is real, under a unique temp directory, so
/// a `remove_dir_all` → `remove_dir` regression (non-recursive) fails here.
#[cfg(test)]
mod storage_cleanup_tests {
    use super::*;
    use async_trait::async_trait;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use temps_agents::error::AgentError;
    use temps_agents::sandbox::{
        SandboxBackend, SandboxCreateConfig, SandboxExecResult, SandboxHandle, SandboxProvider,
    };

    /// Minimal provider. Each knob corresponds to a failure the service is
    /// supposed to clean up after.
    struct FakeProvider {
        /// `create` fails — the provider-failure arm of `create_sandbox`.
        fail_create: bool,
        /// `exec` returns a non-zero exit — the seeding-failure arm.
        fail_exec: bool,
        /// `destroy` errors — the case where the container may still be
        /// running and we delete the work dir anyway.
        fail_destroy: bool,
        destroys: AtomicUsize,
    }

    impl FakeProvider {
        fn new() -> Self {
            Self {
                fail_create: false,
                fail_exec: false,
                fail_destroy: false,
                destroys: AtomicUsize::new(0),
            }
        }
    }

    fn handle_for(name: &str) -> SandboxHandle {
        SandboxHandle {
            sandbox_id: format!("docker-id-{}", name),
            sandbox_name: format!("temps-sandbox-{}", name),
            work_dir: PathBuf::from("/workspace"),
            backend: SandboxBackend::Docker,
            image: String::new(),
        }
    }

    #[async_trait]
    impl SandboxProvider for FakeProvider {
        async fn create(&self, config: SandboxCreateConfig) -> Result<SandboxHandle, AgentError> {
            if self.fail_create {
                return Err(AgentError::SandboxCreationFailed {
                    run_id: config.run_id,
                    provider: "fake".into(),
                    reason: "image pull failed".into(),
                });
            }
            Ok(handle_for(
                config.container_name_override.as_deref().unwrap_or("x"),
            ))
        }

        async fn exec(
            &self,
            _handle: &SandboxHandle,
            _cmd: Vec<String>,
            _env: HashMap<String, String>,
            _on_output: Option<temps_agents::ai_cli::OnEventCallback>,
        ) -> Result<SandboxExecResult, AgentError> {
            Ok(SandboxExecResult {
                exit_code: if self.fail_exec { 1 } else { 0 },
                stdout: String::new(),
                stderr: if self.fail_exec {
                    "clone failed".into()
                } else {
                    String::new()
                },
            })
        }

        async fn is_alive(&self, _handle: &SandboxHandle) -> Result<bool, AgentError> {
            Ok(true)
        }

        async fn write_file(
            &self,
            _handle: &SandboxHandle,
            _path: &str,
            _contents: &[u8],
            _mode: u32,
        ) -> Result<(), AgentError> {
            Ok(())
        }

        async fn read_file(
            &self,
            _handle: &SandboxHandle,
            _path: &str,
        ) -> Result<Vec<u8>, AgentError> {
            Ok(Vec::new())
        }

        async fn write_directory(
            &self,
            _handle: &SandboxHandle,
            _local_dir: &std::path::Path,
            _target_path: &str,
        ) -> Result<(), AgentError> {
            Ok(())
        }

        async fn kill_processes(
            &self,
            _handle: &SandboxHandle,
            _pattern: &str,
            _signal: temps_agents::sandbox::KillSignal,
        ) -> Result<(), AgentError> {
            Ok(())
        }

        async fn destroy(
            &self,
            handle: &SandboxHandle,
            _purge_volumes: bool,
        ) -> Result<(), AgentError> {
            self.destroys.fetch_add(1, Ordering::SeqCst);
            if self.fail_destroy {
                return Err(AgentError::SandboxProviderUnavailable {
                    provider: "fake".into(),
                    reason: format!(
                        "daemon unreachable while destroying {}",
                        handle.sandbox_name
                    ),
                });
            }
            Ok(())
        }

        async fn recover(&self, _run_id: i32) -> Result<Option<SandboxHandle>, AgentError> {
            Ok(None)
        }

        async fn recover_by_name(
            &self,
            container_name: &str,
        ) -> Result<Option<SandboxHandle>, AgentError> {
            Ok(Some(handle_for(container_name)))
        }

        fn name(&self) -> &str {
            "fake"
        }

        async fn is_available(&self) -> bool {
            true
        }

        async fn image_status(&self) -> Result<(bool, String), AgentError> {
            Ok((true, "fake:latest".into()))
        }

        async fn rebuild_image(&self) -> Result<String, AgentError> {
            Ok("fake:latest".into())
        }
    }

    /// A `ServerConfig` built by struct literal rather than `ServerConfig::new`,
    /// which has filesystem side effects (creates `~/.temps` and writes an
    /// auth secret + encryption key). A test must not touch the developer's
    /// real data dir.
    fn test_server_config(data_dir: PathBuf) -> temps_config::ServerConfig {
        temps_config::ServerConfig {
            address: "127.0.0.1:3000".into(),
            database_url: "postgresql://test".into(),
            tls_address: None,
            console_address: "127.0.0.1:3001".into(),
            console_admin_address: None,
            admin_allowed_ips: Vec::new(),
            admin_allowed_hosts: Vec::new(),
            admin_trust_forwarded_for: false,
            data_dir,
            auth_secret: "test-auth-secret".into(),
            encryption_key: "0123456789abcdef0123456789abcdef\
                             0123456789abcdef0123456789abcdef"
                .into(),
            api_base_url: "http://127.0.0.1:3000".into(),
            postgres_max_connections: None,
            postgres_min_connections: None,
            postgres_connect_timeout_secs: None,
            postgres_acquire_timeout_secs: None,
            postgres_idle_timeout_secs: None,
            postgres_max_lifetime_secs: None,
            clickhouse_url: None,
            clickhouse_database: None,
            clickhouse_user: None,
            clickhouse_password: None,
            docker_extra_networks: Vec::new(),
        }
    }

    struct PanicOnSendJobQueue;

    #[async_trait]
    impl temps_core::JobQueue for PanicOnSendJobQueue {
        async fn send(&self, job: temps_core::Job) -> Result<(), temps_core::QueueError> {
            panic!("no job should be queued in these tests, got: {job:?}");
        }

        fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
            panic!("subscribe not needed for these tests")
        }
    }

    /// A unique data root per test so concurrent tests can't collide.
    fn unique_data_root(tag: &str) -> PathBuf {
        let uniq = std::process::id() as u64
            + std::sync::atomic::AtomicU64::new(0).fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("temps-sbx-test-{}-{}", tag, uniq))
    }

    fn build_service(
        db: sea_orm::DatabaseConnection,
        provider: FakeProvider,
        data_root: PathBuf,
    ) -> (Arc<SandboxService>, Arc<StandaloneSandboxRegistry>) {
        let db = Arc::new(db);
        let registry = Arc::new(StandaloneSandboxRegistry::new(
            Arc::new(provider) as Arc<dyn SandboxProvider>
        ));
        let config = Arc::new(temps_config::ConfigService::new(
            Arc::new(test_server_config(data_root.clone())),
            db.clone(),
        ));
        let encryption = Arc::new(
            temps_core::EncryptionService::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .expect("test encryption key"),
        );
        let git = Arc::new(GitProviderManager::new(
            db.clone(),
            encryption,
            Arc::new(PanicOnSendJobQueue) as Arc<dyn temps_core::JobQueue>,
            config.clone(),
        ));
        let cookie_crypto = Arc::new(
            temps_core::CookieCrypto::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .expect("test cookie key"),
        );
        let service = Arc::new(SandboxService::new(
            db,
            registry.clone(),
            Arc::new(JobTracker::new()),
            config,
            cookie_crypto,
            git,
            data_root,
        ));
        (service, registry)
    }

    fn row(public_id: &str, agent_run_id: Option<i32>) -> sandboxes::Model {
        let now = Utc::now();
        sandboxes::Model {
            id: 7,
            public_id: public_id.to_string(),
            user_id: Some(1),
            agent_run_id,
            name: "sbx-7".into(),
            status: "running".into(),
            image: None,
            work_dir: "/workspace".into(),
            timeout_secs: 3600,
            metadata: None,
            backend: None,
            created_at: now,
            last_activity_at: now,
            expires_at: now + chrono::Duration::seconds(3600),
            preview_password_hash: None,
            preview_password_hint: None,
        }
    }

    /// Seed a work dir with nested, non-empty content — a shallow
    /// `remove_dir` would fail on this, a recursive delete succeeds.
    fn seed_work_dir(data_root: &Path, public_id: &str) -> PathBuf {
        let dir = data_root.join(public_id);
        let nested = dir.join("node_modules").join("pkg");
        std::fs::create_dir_all(&nested).expect("seed work dir");
        std::fs::write(nested.join("index.js"), b"module.exports = {}").expect("seed file");
        dir
    }

    const PUBLIC_ID: &str = "sbx_deadbeef01234567";

    #[tokio::test]
    async fn destroy_sandbox_removes_the_work_dir() {
        let data_root = unique_data_root("destroy-ok");
        let dir = seed_work_dir(&data_root, PUBLIC_ID);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // find_by_public_id
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            // record_event insert (RETURNING)
            .append_query_results([vec![sandbox_events::Model {
                id: 1,
                sandbox_id: 7,
                event_type: "destroyed".into(),
                detail: None,
                created_at: Utc::now(),
            }]])
            // mark_destroyed update (RETURNING)
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .into_connection();

        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
        service
            .destroy_sandbox(PUBLIC_ID, 1)
            .await
            .expect("destroy should succeed");

        assert!(
            !dir.exists(),
            "destroy must remove the work dir {} — nothing else ever will, \
             so leaving it is a permanent leak",
            dir.display()
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    /// The documented tradeoff: with no background sweep, skipping cleanup
    /// when the provider destroy fails would leak the directory forever, so
    /// we delete regardless. If someone reintroduces a `container_gone`
    /// guard around the call, this goes red.
    #[tokio::test]
    async fn destroy_sandbox_removes_the_work_dir_even_when_provider_destroy_fails() {
        let data_root = unique_data_root("destroy-fail");
        let dir = seed_work_dir(&data_root, PUBLIC_ID);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .append_query_results([vec![sandbox_events::Model {
                id: 1,
                sandbox_id: 7,
                event_type: "destroyed".into(),
                detail: None,
                created_at: Utc::now(),
            }]])
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .into_connection();

        let mut provider = FakeProvider::new();
        provider.fail_destroy = true;
        let (service, _) = build_service(db, provider, data_root.clone());

        // The user must still be able to delete a sandbox when Docker is
        // unhappy — the call succeeds and the row is marked destroyed.
        service
            .destroy_sandbox(PUBLIC_ID, 1)
            .await
            .expect("destroy must succeed even when the provider fails");

        assert!(
            !dir.exists(),
            "work dir {} must be removed even when the provider destroy \
             failed — there is no sweeper to reclaim it later",
            dir.display()
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    /// Agent-run sandboxes take an early return through
    /// `release_for_agent_run`, and their work dir belongs to the executor,
    /// not to `data_root`. If that early return is ever lost, this test
    /// catches it before it recursively deletes a live run's workspace.
    #[tokio::test]
    async fn destroy_sandbox_for_agent_run_leaves_the_work_dir_alone() {
        let data_root = unique_data_root("agent-run");
        let dir = seed_work_dir(&data_root, PUBLIC_ID);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // find_by_public_id → row owned by agent run 42
            .append_query_results([vec![row(PUBLIC_ID, Some(42))]])
            // agent_runs lookup → empty, i.e. run is gone ⇒ terminal
            .append_query_results([Vec::<agent_runs::Model>::new()])
            // release_for_agent_run: rows for this run
            .append_query_results([Vec::<sandboxes::Model>::new()])
            .into_connection();

        let (service, _) = build_service(db, FakeProvider::new(), data_root.clone());
        let _ = service.destroy_sandbox(PUBLIC_ID, 1).await;

        assert!(
            dir.exists(),
            "an agent-run sandbox must not have {} deleted — that directory \
             is owned by the executor, and this path is only for standalone \
             sandboxes",
            dir.display()
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[tokio::test]
    async fn create_sandbox_provider_failure_leaves_no_work_dir() {
        let data_root = unique_data_root("create-fail");
        std::fs::create_dir_all(&data_root).expect("data root");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // insert of the new row (RETURNING)
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            // mark_destroyed update
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .into_connection();

        let mut provider = FakeProvider::new();
        provider.fail_create = true;
        let (service, _) = build_service(db, provider, data_root.clone());

        let err = service
            .create_sandbox(1, CreateSandboxRequest::default())
            .await
            .expect_err("provider create failed, so create_sandbox must fail");
        assert!(
            matches!(err, SandboxError::CreateFailed { .. }),
            "{:?}",
            err
        );

        let leftovers: Vec<_> = std::fs::read_dir(&data_root)
            .expect("read data root")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert!(
            leftovers.is_empty(),
            "a failed create must not strand a work dir; found {:?}",
            leftovers
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }

    /// The costly one: seeding runs after the clone, so by the time it fails
    /// the directory can hold an entire repository, and the row is already
    /// destroyed so no later `destroy_sandbox` can reach it.
    #[tokio::test]
    async fn create_sandbox_seed_failure_leaves_no_work_dir() {
        let data_root = unique_data_root("seed-fail");
        std::fs::create_dir_all(&data_root).expect("data root");

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // insert of the new row
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            // update after create (status/metadata write-back)
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            // mark_destroyed
            .append_query_results([vec![row(PUBLIC_ID, None)]])
            .into_connection();

        let mut provider = FakeProvider::new();
        provider.fail_exec = true;
        let (service, _) = build_service(db, provider, data_root.clone());

        let req = CreateSandboxRequest {
            source: Some(SandboxSource::Tarball {
                url: "https://example.invalid/src.tar.gz".into(),
            }),
            ..Default::default()
        };
        let err = service
            .create_sandbox(1, req)
            .await
            .expect_err("seeding failed, so create_sandbox must fail");
        assert!(matches!(err, SandboxError::ExecFailed { .. }), "{:?}", err);

        let leftovers: Vec<_> = std::fs::read_dir(&data_root)
            .expect("read data root")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert!(
            leftovers.is_empty(),
            "a failed seed must not strand a work dir holding a cloned \
             repository; found {:?}",
            leftovers
        );
        let _ = std::fs::remove_dir_all(&data_root);
    }
}
