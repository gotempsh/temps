//! Preview gateway supervisor.
//!
//! Reconciles a single shared `temps-preview-gateway` container on the local
//! Docker host. Called from `temps serve` startup, but always runs in a
//! background task — the proxy server (80/443) MUST NOT be blocked on this.
//!
//! What it guarantees, in order:
//! 1. The shared sandbox network exists (so workspace sandboxes and the
//!    gateway can resolve each other by container name via Docker DNS).
//! 2. The pinned gateway image is present locally (pulled if missing).
//! 3. A container named `temps-preview-gateway` is running, attached to that
//!    network, with the right image and the right host-port publish on
//!    `127.0.0.1:<port>`. Any drift causes a recreate.
//!
//! Failure mode: log loudly, return Err. The caller will log the error and
//! continue serving. The workspace preview feature is degraded until the
//! gateway is up, but the rest of Temps keeps running.

use anyhow::{anyhow, Context, Result};
use bollard::models::{
    ContainerCreateBody, HostConfig, NetworkCreateRequest, PortBinding, RestartPolicy,
    RestartPolicyNameEnum,
};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptions, InspectContainerOptions,
    ListContainersOptions, ListNetworksOptions, LogsOptions, RemoveContainerOptions,
    StartContainerOptions,
};
use bollard::Docker;
use futures::StreamExt;
use futures::TryStreamExt;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, DatabaseConnection, EntityTrait};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use temps_core::PreviewGatewaySettings;
use tracing::{debug, info, warn};

/// Immutable multi-platform manifest reference. Bumped per release.
pub const PREVIEW_GATEWAY_IMAGE: &str = "ghcr.io/gotempsh/temps-preview-gateway@sha256:a16d4346f2f857470fdd28c9ed46809f6db4f7e577888d6250338f8d5dcf04b9";

/// Filename inside `TEMPS_DATA_DIR` that holds the gateway shared secret.
/// The file is created with 0600 perms on first boot if missing; the same
/// value is then injected into (a) the gateway container env and (b) the
/// `PREVIEW_GATEWAY_SHARED_SECRET` env of the current process so the
/// host-side Pingora can pick it up and inject the `X-Temps-Preview-Token`
/// header on every forwarded preview request.
const PREVIEW_GATEWAY_SECRET_FILE: &str = "preview_gateway.secret";

/// Ensure a shared-secret file exists under `data_dir`, generating a fresh
/// 32-byte random secret (hex-encoded) if missing. Sets restrictive perms on
/// first write. Returns the secret as a hex string.
///
/// Also exports the secret into `PREVIEW_GATEWAY_SHARED_SECRET` for the
/// current process so in-process subsystems (notably the Pingora proxy's
/// preview route) can read it via `std::env::var` without plumbing state.
pub fn ensure_shared_secret(data_dir: &std::path::Path) -> Result<String> {
    let path = data_dir.join(PREVIEW_GATEWAY_SECRET_FILE);

    let secret = match std::fs::read_to_string(&path) {
        Ok(existing) => {
            let trimmed = existing.trim().to_string();
            if trimmed.is_empty() {
                return Err(anyhow!(
                    "preview gateway secret file {} is empty",
                    path.display()
                ));
            }
            trimmed
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Generate a fresh 32-byte random secret.
            use rand::Rng;
            let mut bytes = [0u8; 32];
            rand::rng().fill_bytes(&mut bytes);
            let hex = hex::encode(bytes);

            // Make sure the parent dir exists.
            if !data_dir.exists() {
                std::fs::create_dir_all(data_dir)
                    .with_context(|| format!("failed to create data dir {}", data_dir.display()))?;
            }
            std::fs::write(&path, &hex)
                .with_context(|| format!("failed to write {}", path.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            }
            info!(
                path = %path.display(),
                "generated new preview gateway shared secret"
            );
            hex
        }
        Err(e) => {
            return Err(anyhow!(
                "failed to read preview gateway secret file {}: {}",
                path.display(),
                e
            ));
        }
    };

    // Export for the in-process proxy to pick up at request time.
    // SAFETY: set_var is marked unsafe on newer Rust due to multi-threaded
    // env races. We call this only during single-threaded startup before
    // the proxy begins serving, so it is safe in practice.
    #[allow(unused_unsafe)]
    unsafe {
        std::env::set_var("PREVIEW_GATEWAY_SHARED_SECRET", &secret);
    }

    Ok(secret)
}

/// Container name for the singleton gateway on this host.
pub const PREVIEW_GATEWAY_CONTAINER: &str = "temps-preview-gateway";

/// Shared docker network used for sandbox <-> gateway DNS resolution.
pub const PREVIEW_GATEWAY_NETWORK: &str = "temps-sandbox-net";

/// Default host port the gateway publishes to. Bound on 127.0.0.1 only —
/// the host-side Pingora reaches it via this port after authenticating.
pub const DEFAULT_PREVIEW_GATEWAY_HOST_PORT: u16 = 8090;

/// Internal port the gateway listens on inside its container.
const GATEWAY_CONTAINER_PORT: u16 = 8080;

#[derive(Debug, Clone)]
pub struct PreviewGatewaySpec {
    pub image: String,
    pub container_name: String,
    pub network: String,
    pub host_port: u16,
    /// Shared secret the gateway will require on every request via the
    /// `X-Temps-Preview-Token` header. When empty, the gateway is started
    /// in legacy open mode — callers SHOULD always pass a non-empty value.
    pub shared_secret: String,
}

impl Default for PreviewGatewaySpec {
    fn default() -> Self {
        Self {
            image: PREVIEW_GATEWAY_IMAGE.to_string(),
            container_name: PREVIEW_GATEWAY_CONTAINER.to_string(),
            network: PREVIEW_GATEWAY_NETWORK.to_string(),
            host_port: DEFAULT_PREVIEW_GATEWAY_HOST_PORT,
            shared_secret: String::new(),
        }
    }
}

impl PreviewGatewaySpec {
    /// Build a spec from persisted settings, falling back to compile-time
    /// constants for any field that hasn't been customised. Does NOT enforce
    /// `auto_upgrade` semantics — that's the caller's job.
    pub fn from_settings(settings: &PreviewGatewaySettings) -> Self {
        let image = if settings.image.trim().is_empty() {
            PREVIEW_GATEWAY_IMAGE.to_string()
        } else {
            settings.image.clone()
        };
        let host_port = if settings.host_port == 0 {
            DEFAULT_PREVIEW_GATEWAY_HOST_PORT
        } else {
            settings.host_port
        };
        Self {
            image,
            container_name: PREVIEW_GATEWAY_CONTAINER.to_string(),
            network: PREVIEW_GATEWAY_NETWORK.to_string(),
            host_port,
            shared_secret: settings.shared_secret.clone(),
        }
    }
}

/// Read the persisted `preview_gateway` settings from the DB. Falls back to
/// defaults if the row or field is missing — never errors.
pub async fn load_settings(db: &DatabaseConnection) -> PreviewGatewaySettings {
    let row = match temps_entities::settings::Entity::find_by_id(1)
        .one(db)
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return PreviewGatewaySettings::default(),
        Err(e) => {
            warn!("failed to load preview gateway settings: {}", e);
            return PreviewGatewaySettings::default();
        }
    };
    row.data
        .get("preview_gateway")
        .cloned()
        .and_then(|v| serde_json::from_value::<PreviewGatewaySettings>(v).ok())
        .unwrap_or_default()
}

/// Reconcile the gateway to match `spec`. Idempotent.
pub async fn reconcile(docker: Arc<Docker>, spec: PreviewGatewaySpec) -> Result<()> {
    info!(
        image = %spec.image,
        container = %spec.container_name,
        network = %spec.network,
        host_port = spec.host_port,
        "reconciling preview gateway"
    );

    ensure_network(&docker, &spec.network).await?;
    ensure_image(&docker, &spec.image).await?;

    match inspect(&docker, &spec.container_name).await? {
        Some(existing) if container_matches(&existing, &spec) && existing.running => {
            debug!("preview gateway already running with desired spec");
        }
        Some(existing) => {
            info!(
                running = existing.running,
                image_match = existing.image == spec.image,
                "preview gateway drift detected — recreating"
            );
            remove(&docker, &spec.container_name).await?;
            create_and_start(&docker, &spec).await?;
        }
        None => {
            info!("preview gateway not present — creating");
            create_and_start(&docker, &spec).await?;
        }
    }

    info!(
        "preview gateway ready on 127.0.0.1:{} → {}:{}",
        spec.host_port, spec.container_name, GATEWAY_CONTAINER_PORT
    );
    Ok(())
}

async fn ensure_network(docker: &Docker, name: &str) -> Result<()> {
    let networks = docker
        .list_networks(None::<ListNetworksOptions>)
        .await
        .context("failed to list docker networks")?;
    if networks.iter().any(|n| n.name.as_deref() == Some(name)) {
        return Ok(());
    }
    info!(network = %name, "creating shared sandbox network");
    docker
        .create_network(NetworkCreateRequest {
            name: name.to_string(),
            driver: Some("bridge".to_string()),
            ..Default::default()
        })
        .await
        .with_context(|| format!("failed to create network {}", name))?;
    Ok(())
}

async fn ensure_image(docker: &Docker, image: &str) -> Result<()> {
    // Locally-built dev tag — don't try to pull, the registry copy may not
    // exist yet. The operator is expected to have built it.
    if image.ends_with(":dev") {
        debug!(image = %image, "skipping pull for :dev tag");
        return Ok(());
    }
    info!(image = %image, "pulling preview gateway image (if needed)");
    let mut stream = docker.create_image(
        Some(CreateImageOptions {
            from_image: Some(image.to_string()),
            ..Default::default()
        }),
        None,
        None,
    );
    while let Some(item) = stream.next().await {
        if let Err(e) = item {
            return Err(anyhow!("failed to pull image {}: {}", image, e));
        }
    }
    Ok(())
}

#[derive(Debug)]
struct ExistingContainer {
    image: String,
    running: bool,
    host_port_binding: Option<(String, String)>, // (host_ip, host_port)
    network_attached: bool,
    shared_secret_env: Option<String>,
}

async fn inspect(docker: &Docker, name: &str) -> Result<Option<ExistingContainer>> {
    // list_containers with `all=true` and a name filter — we need stopped
    // containers too so we can recreate them with the right config.
    let mut filters: HashMap<String, Vec<String>> = HashMap::new();
    filters.insert("name".to_string(), vec![format!("^/{}$", name)]);
    let listed = docker
        .list_containers(Some(ListContainersOptions {
            all: true,
            filters: Some(filters),
            ..Default::default()
        }))
        .await
        .context("failed to list containers for preview gateway lookup")?;
    if listed.is_empty() {
        return Ok(None);
    }

    let inspected = docker
        .inspect_container(name, None::<InspectContainerOptions>)
        .await
        .context("failed to inspect preview gateway container")?;

    let image = inspected
        .config
        .as_ref()
        .and_then(|c| c.image.clone())
        .unwrap_or_default();
    let running = inspected
        .state
        .as_ref()
        .and_then(|s| s.running)
        .unwrap_or(false);

    let host_port_binding = inspected
        .host_config
        .as_ref()
        .and_then(|h| h.port_bindings.as_ref())
        .and_then(|pb| pb.get(&format!("{}/tcp", GATEWAY_CONTAINER_PORT)).cloned())
        .and_then(|bindings| bindings.into_iter().flatten().next())
        .and_then(|b| match (b.host_ip, b.host_port) {
            (Some(ip), Some(port)) => Some((ip, port)),
            _ => None,
        });

    let network_attached = inspected
        .network_settings
        .as_ref()
        .and_then(|ns| ns.networks.as_ref())
        .map(|nets| nets.contains_key(PREVIEW_GATEWAY_NETWORK))
        .unwrap_or(false);

    let shared_secret_env = inspected
        .config
        .as_ref()
        .and_then(|c| c.env.as_ref())
        .and_then(|env| {
            env.iter().find_map(|v| {
                v.strip_prefix("PREVIEW_GATEWAY_SHARED_SECRET=")
                    .filter(|val| !val.is_empty())
                    .map(|val| val.to_string())
            })
        });

    Ok(Some(ExistingContainer {
        image,
        running,
        host_port_binding,
        network_attached,
        shared_secret_env,
    }))
}

fn container_matches(existing: &ExistingContainer, spec: &PreviewGatewaySpec) -> bool {
    if existing.image != spec.image {
        return false;
    }
    if !existing.network_attached {
        return false;
    }
    // The running container's actual secret must equal the one this instance
    // is about to inject, not merely be present. Presence-only matching can
    // never detect drift: a container created (or last reconciled) by a
    // different Temps instance/database keeps whatever secret it was born
    // with forever, so this instance's proxy injects its own DB secret, the
    // gateway compares against a different one, and every preview request
    // gets rejected with "missing or invalid X-Temps-Preview-Token" even
    // though a secret genuinely is configured — just the wrong one.
    if !spec.shared_secret.is_empty()
        && existing.shared_secret_env.as_deref() != Some(spec.shared_secret.as_str())
    {
        return false;
    }
    match &existing.host_port_binding {
        Some((ip, port)) => ip == "127.0.0.1" && port == &spec.host_port.to_string(),
        None => false,
    }
}

async fn remove(docker: &Docker, name: &str) -> Result<()> {
    docker
        .remove_container(
            name,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await
        .with_context(|| format!("failed to remove existing container {}", name))?;
    Ok(())
}

async fn create_and_start(docker: &Docker, spec: &PreviewGatewaySpec) -> Result<()> {
    let container_port_key = format!("{}/tcp", GATEWAY_CONTAINER_PORT);

    let exposed_ports: Vec<String> = vec![container_port_key.clone()];

    let mut port_bindings: HashMap<String, Option<Vec<PortBinding>>> = HashMap::new();
    port_bindings.insert(
        container_port_key,
        Some(vec![PortBinding {
            host_ip: Some("127.0.0.1".to_string()),
            host_port: Some(spec.host_port.to_string()),
        }]),
    );

    let host_config = HostConfig {
        port_bindings: Some(port_bindings),
        network_mode: Some(spec.network.clone()),
        restart_policy: Some(RestartPolicy {
            name: Some(RestartPolicyNameEnum::UNLESS_STOPPED),
            maximum_retry_count: None,
        }),
        ..Default::default()
    };

    let body = ContainerCreateBody {
        image: Some(spec.image.clone()),
        exposed_ports: Some(exposed_ports),
        host_config: Some(host_config),
        env: Some({
            let mut e = vec![
                // Inside the container we MUST bind 0.0.0.0 — the host loopback
                // restriction is enforced by the `-p 127.0.0.1:…` publish above.
                format!("LISTEN_ADDR=0.0.0.0:{}", GATEWAY_CONTAINER_PORT),
                "RUST_LOG=info".to_string(),
            ];
            if !spec.shared_secret.is_empty() {
                e.push(format!(
                    "PREVIEW_GATEWAY_SHARED_SECRET={}",
                    spec.shared_secret
                ));
            }
            e
        }),
        ..Default::default()
    };

    docker
        .create_container(
            Some(
                CreateContainerOptionsBuilder::new()
                    .name(&spec.container_name)
                    .build(),
            ),
            body,
        )
        .await
        .with_context(|| format!("failed to create container {}", spec.container_name))?;

    docker
        .start_container(&spec.container_name, None::<StartContainerOptions>)
        .await
        .with_context(|| format!("failed to start container {}", spec.container_name))?;

    Ok(())
}

/// Ensure the shared secret is persisted in the DB, generating one if missing.
///
/// Precedence:
/// 1. If `settings.preview_gateway.shared_secret` is non-empty → use it.
/// 2. Else, if a legacy `preview_gateway.secret` file exists under `data_dir`
///    → adopt its contents, persist to DB (backwards-compat migration).
/// 3. Else, generate a fresh 32-byte hex secret and persist it.
///
/// Always exports the secret into `PREVIEW_GATEWAY_SHARED_SECRET` for the
/// current process so the in-process Pingora can read it via `std::env::var`.
///
/// Never fails hard: on any DB error we fall back to the legacy file helper
/// so workspace previews keep working even if the settings row is unreachable.
pub async fn ensure_shared_secret_db(
    db: &DatabaseConnection,
    data_dir: &std::path::Path,
) -> String {
    // Load current settings row.
    let row = match temps_entities::settings::Entity::find_by_id(1)
        .one(db)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!(
                "failed to load settings row for preview-gateway secret: {} — falling back to file",
                e
            );
            return ensure_shared_secret(data_dir).unwrap_or_default();
        }
    };

    let mut full: serde_json::Value = row
        .as_ref()
        .map(|r| r.data.clone())
        .unwrap_or_else(|| serde_json::json!({}));

    let mut pg: PreviewGatewaySettings = full
        .get("preview_gateway")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();

    // Short-circuit: already have one in DB.
    if !pg.shared_secret.is_empty() {
        export_env(&pg.shared_secret);
        return pg.shared_secret;
    }

    // Adopt legacy file if present. Only read — do not create a new file.
    let legacy_path = data_dir.join(PREVIEW_GATEWAY_SECRET_FILE);
    let secret = match std::fs::read_to_string(&legacy_path) {
        Ok(contents) => {
            let trimmed = contents.trim().to_string();
            if !trimmed.is_empty() {
                info!(
                    path = %legacy_path.display(),
                    "adopting legacy preview gateway secret file into DB"
                );
                trimmed
            } else {
                generate_secret()
            }
        }
        Err(_) => generate_secret(),
    };

    pg.shared_secret = secret.clone();

    // Persist back.
    full.as_object_mut()
        .map(|m| m.insert("preview_gateway".into(), serde_json::to_value(&pg).unwrap()));

    let now = chrono::Utc::now();
    let persist_result = match row {
        Some(existing) => {
            let mut am: temps_entities::settings::ActiveModel = existing.into();
            am.data = Set(full);
            am.updated_at = Set(now);
            am.update(db).await.map(|_| ())
        }
        None => {
            let am = temps_entities::settings::ActiveModel {
                id: Set(1),
                data: Set(full),
                created_at: Set(now),
                updated_at: Set(now),
            };
            am.insert(db).await.map(|_| ())
        }
    };

    if let Err(e) = persist_result {
        warn!(
            "failed to persist preview-gateway secret to DB: {} — continuing with in-memory value",
            e
        );
    }

    export_env(&secret);
    secret
}

fn generate_secret() -> String {
    use rand::Rng;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn export_env(secret: &str) {
    #[allow(unused_unsafe)]
    unsafe {
        std::env::set_var("PREVIEW_GATEWAY_SHARED_SECRET", secret);
    }
}

/// Spawn the reconciler on the given runtime. Logs failures but never panics.
/// Returns immediately — the actual reconcile runs in the background so the
/// caller (the proxy server bootstrap) is never blocked.
///
/// Behavior:
/// - Ensures the gateway shared secret exists in the DB (generating one on
///   first boot, adopting the legacy file if present).
/// - Reads PreviewGatewaySettings from the DB (or defaults if missing).
/// - If `auto_upgrade = true` (default), applies the settings image directly.
/// - If `auto_upgrade = false`, leaves the *image* of an existing container
///   alone but still ensures the container is running on the desired
///   host_port and network. New installs (no container yet) always use the
///   settings image — there's nothing to preserve.
pub fn spawn_reconcile(
    rt: &tokio::runtime::Runtime,
    docker: Arc<Docker>,
    db: Arc<DatabaseConnection>,
    data_dir: std::path::PathBuf,
) {
    rt.spawn(async move {
        // DB-backed secret so the value is stable across restarts, cwd
        // changes, and `TEMPS_DATA_DIR` overrides. Falls back to the legacy
        // file path for migration.
        let shared_secret = ensure_shared_secret_db(&db, &data_dir).await;
        if shared_secret.is_empty() {
            warn!(
                "❌ preview gateway shared secret is empty after DB+file resolution — workspace previews disabled"
            );
        }

        let settings = load_settings(&db).await;
        let mut spec = PreviewGatewaySpec::from_settings(&settings);
        spec.shared_secret = shared_secret;

        if !settings.auto_upgrade {
            // Honor whatever image is currently running. Falls back to the
            // settings image if the container doesn't exist yet.
            if let Ok(Some(existing)) = inspect(&docker, &spec.container_name).await {
                if !existing.image.is_empty() {
                    debug!(
                        running_image = %existing.image,
                        settings_image = %spec.image,
                        "auto_upgrade=false — keeping running image"
                    );
                    spec.image = existing.image;
                }
            }
        }

        match reconcile(docker, spec).await {
            Ok(()) => {
                info!("✅ preview gateway reconciled");
            }
            Err(e) => {
                warn!(
                    "❌ preview gateway reconcile failed: {} \
                     — workspace preview URLs will not work until this is fixed. \
                     Other Temps functionality is unaffected.",
                    e
                );
            }
        }
    });
}

// ────────────────────────────────────────────────────────────────────────────
// Status + logs helpers used by the settings UI handlers
// ────────────────────────────────────────────────────────────────────────────

/// Detailed gateway container status surfaced to the settings UI.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct GatewayStatus {
    /// Whether the container exists at all.
    pub present: bool,
    /// Whether the container is currently running.
    pub running: bool,
    /// Higher-level health label: "running" | "restarting" | "crash_looping"
    /// | "stopped" | "missing". UI should prefer this over `running`.
    pub health: String,
    /// Image reference the container was created with (e.g. an immutable
    /// `ghcr.io/gotempsh/temps-preview-gateway@sha256:…` reference).
    pub image: Option<String>,
    /// Image digest if available (e.g. `sha256:…`).
    pub image_digest: Option<String>,
    /// Container name.
    pub container_name: String,
    /// Network the container is attached to (should be `temps-sandbox-net`).
    pub network: Option<String>,
    /// Host port that the container's :8080 is published on.
    pub host_port: Option<u16>,
    /// ISO 8601 timestamp the container was started at, if running.
    pub started_at: Option<String>,
    /// Number of times Docker has restarted the container.
    pub restart_count: Option<i64>,
    /// Exit code of the last run, if the container is not currently running.
    pub last_exit_code: Option<i64>,
    /// Error string Docker recorded for the container (e.g. startup failure).
    pub last_error: Option<String>,
    /// The image the supervisor *expects* (from settings/constant). If this
    /// differs from `image`, the UI shows a "drift" badge.
    pub expected_image: String,
    /// True when `image != expected_image` and the container is present.
    pub drift: bool,
    /// True if `auto_upgrade` is enabled in settings.
    pub auto_upgrade: bool,
}

/// Gather a complete status snapshot for the gateway container.
pub async fn inspect_status(
    docker: &Docker,
    settings: &PreviewGatewaySettings,
) -> Result<GatewayStatus> {
    let expected_image = if settings.image.trim().is_empty() {
        PREVIEW_GATEWAY_IMAGE.to_string()
    } else {
        settings.image.clone()
    };

    let inspected = docker
        .inspect_container(PREVIEW_GATEWAY_CONTAINER, None::<InspectContainerOptions>)
        .await;

    let inspected = match inspected {
        Ok(c) => c,
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => {
            return Ok(GatewayStatus {
                present: false,
                running: false,
                health: "missing".to_string(),
                image: None,
                image_digest: None,
                container_name: PREVIEW_GATEWAY_CONTAINER.to_string(),
                network: None,
                host_port: None,
                started_at: None,
                restart_count: None,
                last_exit_code: None,
                last_error: None,
                expected_image,
                drift: false,
                auto_upgrade: settings.auto_upgrade,
            });
        }
        Err(e) => return Err(anyhow!("docker inspect failed: {}", e)),
    };

    let image = inspected.config.as_ref().and_then(|c| c.image.clone());
    let image_digest = inspected.image.clone();
    let running = inspected
        .state
        .as_ref()
        .and_then(|s| s.running)
        .unwrap_or(false);
    let restarting = inspected
        .state
        .as_ref()
        .and_then(|s| s.restarting)
        .unwrap_or(false);
    let started_at = inspected
        .state
        .as_ref()
        .and_then(|s| s.started_at.clone())
        .filter(|s| !s.is_empty() && s != "0001-01-01T00:00:00Z");
    let restart_count = inspected.restart_count;
    let last_exit_code = inspected.state.as_ref().and_then(|s| s.exit_code);
    let last_error = inspected
        .state
        .as_ref()
        .and_then(|s| s.error.clone())
        .filter(|s| !s.is_empty());

    // Infer a sensible health label. Docker's `running` flag stays true
    // across restart policies even when the process exits immediately, so
    // we treat any container that has restarted AND exited non-zero as
    // crash-looping. Pure `restarting` state is also surfaced distinctly.
    let health = if restarting {
        "restarting"
    } else if running {
        match (restart_count, last_exit_code) {
            (Some(n), Some(code)) if n > 0 && code != 0 => "crash_looping",
            _ => "running",
        }
    } else {
        "stopped"
    }
    .to_string();

    let network = inspected
        .network_settings
        .as_ref()
        .and_then(|ns| ns.networks.as_ref())
        .and_then(|nets| nets.keys().next().cloned());

    let host_port = inspected
        .host_config
        .as_ref()
        .and_then(|h| h.port_bindings.as_ref())
        .and_then(|pb| pb.get(&format!("{}/tcp", GATEWAY_CONTAINER_PORT)).cloned())
        .and_then(|bindings| bindings.into_iter().flatten().next())
        .and_then(|b| b.host_port.and_then(|p| p.parse::<u16>().ok()));

    let drift = image
        .as_deref()
        .map(|img| img != expected_image)
        .unwrap_or(false);

    Ok(GatewayStatus {
        present: true,
        running,
        health,
        image,
        image_digest,
        container_name: PREVIEW_GATEWAY_CONTAINER.to_string(),
        network,
        host_port,
        started_at,
        restart_count,
        last_exit_code,
        last_error,
        expected_image,
        drift,
        auto_upgrade: settings.auto_upgrade,
    })
}

/// Force-restart the gateway: ensures network/image, then removes any
/// existing container and recreates it fresh. Unlike `reconcile`, this
/// always replaces the container even if it already matches the spec.
pub async fn force_restart(docker: Arc<Docker>, spec: PreviewGatewaySpec) -> Result<()> {
    info!(
        image = %spec.image,
        container = %spec.container_name,
        "force-restarting preview gateway"
    );
    ensure_network(&docker, &spec.network).await?;
    ensure_image(&docker, &spec.image).await?;

    if inspect(&docker, &spec.container_name).await?.is_some() {
        remove(&docker, &spec.container_name).await?;
    }
    create_and_start(&docker, &spec).await?;
    info!("preview gateway restarted");
    Ok(())
}

/// Tail the gateway container's stdout+stderr. `tail` caps the number of
/// lines returned (e.g. 200). Returns lines newest-last.
pub async fn tail_logs(docker: &Docker, tail: usize) -> Result<Vec<String>> {
    let stream = docker.logs(
        PREVIEW_GATEWAY_CONTAINER,
        Some(LogsOptions {
            stdout: true,
            stderr: true,
            tail: tail.to_string(),
            timestamps: false,
            ..Default::default()
        }),
    );

    let chunks: Vec<_> = stream
        .map(|chunk| chunk.map(|c| String::from_utf8_lossy(&c.into_bytes()).to_string()))
        .try_collect()
        .await
        .map_err(|e| anyhow!("failed to tail gateway logs: {}", e))?;

    let joined = chunks.join("");
    Ok(joined.lines().map(|l| l.to_string()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn default_gateway_image_is_immutable() {
        assert_eq!(
            PREVIEW_GATEWAY_IMAGE,
            PreviewGatewaySettings::default().image
        );
        assert!(PREVIEW_GATEWAY_IMAGE.contains("@sha256:"));
    }

    #[test]
    fn ensure_shared_secret_creates_file_on_first_call() {
        let dir = TempDir::new().unwrap();
        let secret = ensure_shared_secret(dir.path()).unwrap();
        // 32 random bytes → 64 hex chars
        assert_eq!(secret.len(), 64, "secret should be 64 hex chars");
        // File should exist with the same content
        let on_disk =
            std::fs::read_to_string(dir.path().join(PREVIEW_GATEWAY_SECRET_FILE)).unwrap();
        assert_eq!(on_disk, secret);
    }

    #[test]
    fn ensure_shared_secret_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let first = ensure_shared_secret(dir.path()).unwrap();
        let second = ensure_shared_secret(dir.path()).unwrap();
        assert_eq!(first, second, "second call should return the same secret");
    }

    #[test]
    fn ensure_shared_secret_rejects_empty_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(PREVIEW_GATEWAY_SECRET_FILE);
        std::fs::write(&path, "").unwrap();
        let result = ensure_shared_secret(dir.path());
        assert!(result.is_err(), "empty secret file should be an error");
    }

    #[cfg(unix)]
    #[test]
    fn ensure_shared_secret_sets_restrictive_perms() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let _ = ensure_shared_secret(dir.path()).unwrap();
        let meta = std::fs::metadata(dir.path().join(PREVIEW_GATEWAY_SECRET_FILE)).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "secret file should be 0600, got {:o}", mode);
    }

    #[test]
    fn ensure_shared_secret_creates_parent_dirs() {
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("a").join("b").join("c");
        let secret = ensure_shared_secret(&nested).unwrap();
        assert_eq!(secret.len(), 64);
        assert!(nested.join(PREVIEW_GATEWAY_SECRET_FILE).exists());
    }
}
