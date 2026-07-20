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
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect,
};

use temps_agents::sandbox::SandboxCreateConfig;
use temps_config::ConfigService;
use temps_entities::{sandbox_events, sandboxes};
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
        git_provider_manager: Arc<GitProviderManager>,
        data_root: PathBuf,
    ) -> Self {
        Self {
            db,
            registry,
            jobs,
            platform_config,
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
        if row.user_id != user_id {
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
            user_id: Set(user_id),
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

        // Use the hex-only label (strip `sbx_`) as the container name
        // suffix — same label the preview hostname embeds so the gateway
        // can DNS-resolve `temps-sandbox-<label>` directly from the URL.
        let container_label = public_id_value
            .strip_prefix("sbx_")
            .unwrap_or(&public_id_value)
            .to_string();

        let config = SandboxCreateConfig {
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
    pub async fn destroy_sandbox(
        &self,
        public_id_value: &str,
        user_id: i32,
    ) -> Result<(), SandboxError> {
        let row = self.find_by_public_id(public_id_value, user_id).await?;
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
        self.record_event(row.id, "destroyed", None).await;
        self.mark_destroyed(row.id).await?;
        Ok(())
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

    #[test]
    fn timeout_constants_are_sane() {
        const _: () = assert!(MIN_TIMEOUT_SECS < DEFAULT_TIMEOUT_SECS);
        const _: () = assert!(DEFAULT_TIMEOUT_SECS < MAX_TIMEOUT_SECS);
        assert_eq!(MAX_TIMEOUT_SECS, 86400);
    }

    #[test]
    fn summary_from_model_copies_fields() {
        let now = Utc::now();
        let m = sandboxes::Model {
            id: 1_000_042,
            public_id: "sbx_abc1234567890def".into(),
            user_id: 7,
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
    }
}
