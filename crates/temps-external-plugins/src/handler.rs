//! HTTP handlers for external plugin management endpoints.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};
use temps_auth::{permission_guard, RequireAuth};
use temps_core::error_builder;
use temps_core::external_plugin::{NavEntry, NavSection, PluginManifest, UiManifest, UiRoute};
use temps_core::problemdetails::Problem;
use utoipa::{OpenApi as OpenApiTrait, ToSchema};

use crate::catalog::CatalogRejection;
use crate::install::{InstallError, PlatformAsset, PluginInstaller, PluginRegistryManifest};

use crate::service::ExternalPluginsService;

// ---------------------------------------------------------------------------
// Audit
// ---------------------------------------------------------------------------

/// Recorded on every successful plugin install.
///
/// Captures the resolved version and the SHA-256 actually verified, not just
/// the plugin name: "who installed a plugin" is not enough to answer "which
/// bytes are running on this host", which is the question that matters after
/// a registry compromise.
#[derive(Debug, Clone, Serialize)]
struct PluginInstalledAudit {
    context: temps_core::audit::AuditContext,
    plugin_name: String,
    version: String,
    manifest_url: String,
    platform: String,
    sha256: String,
    install_path: String,
    process_started: bool,
}

impl temps_core::audit::AuditOperation for PluginInstalledAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_PLUGIN_INSTALLED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation: {}", e))
    }
}

/// Recorded on every plugin reload. Reloading restarts plugin processes, so it
/// is a privileged write even though it installs nothing.
#[derive(Debug, Clone, Serialize)]
struct PluginsReloadedAudit {
    context: temps_core::audit::AuditContext,
    loaded: usize,
    plugins: Vec<String>,
}

impl temps_core::audit::AuditOperation for PluginsReloadedAudit {
    fn operation_type(&self) -> String {
        "EXTERNAL_PLUGINS_RELOADED".to_string()
    }

    fn user_id(&self) -> Option<i32> {
        Some(self.context.user_id)
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> anyhow::Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation: {}", e))
    }
}

/// Build the audit context from the authenticated caller and request metadata.
fn audit_context(
    auth: &temps_auth::AuthContext,
    metadata: &temps_core::RequestMetadata,
) -> temps_core::audit::AuditContext {
    temps_core::audit::AuditContext {
        user_id: auth.user_id(),
        ip_address: Some(metadata.ip_address.clone()),
        user_agent: metadata.user_agent.clone(),
    }
}

/// Write an audit entry, logging but not propagating a failure.
///
/// A failed audit write must not roll back an install that already succeeded —
/// the binary is on disk either way, and returning an error would tell the
/// operator the opposite. The ERROR log is the escalation path.
async fn record_audit(
    state: &ExternalPluginsAppState,
    operation: &dyn temps_core::audit::AuditOperation,
) {
    if let Err(e) = state.audit_service.create_audit_log(operation).await {
        tracing::error!(
            operation = %operation.operation_type(),
            "Failed to write audit log: {e}"
        );
    }
}

/// Map a typed install failure to its HTTP status and title.
///
/// Matching on the variant rather than substring-matching the rendered message
/// is what keeps the status codes stable when a message is reworded.
fn install_problem(error: &InstallError) -> Problem {
    use InstallError::*;
    let (status, title) = match error {
        UnsupportedPlatform { .. } | NoAssetForPlatform { .. } => {
            (StatusCode::BAD_REQUEST, "Unsupported Platform")
        }
        // The registry published something we refuse to act on. It is not the
        // caller's fault, and retrying will not help until it is republished.
        InsecureAssetUrl { .. } => (StatusCode::BAD_GATEWAY, "Insecure Asset URL"),
        ChecksumMismatch { .. } => (StatusCode::BAD_GATEWAY, "Checksum Verification Failed"),
        EntryNotFound { .. } | Tarball { .. } => {
            (StatusCode::BAD_GATEWAY, "Tarball Extraction Failed")
        }
        ManifestParse { .. } => (StatusCode::BAD_GATEWAY, "Malformed Manifest"),
        TooLarge { .. } | ExtractedTooLarge { .. } => {
            (StatusCode::BAD_GATEWAY, "Plugin Artifact Too Large")
        }
        DownloadStatus { .. } | Download { .. } => (StatusCode::BAD_GATEWAY, "Download Failed"),
        Write { .. } => (StatusCode::INTERNAL_SERVER_ERROR, "Plugin Install Failed"),
    };
    temps_core::problemdetails::new(status)
        .with_title(title)
        .with_detail(error.to_string())
}

/// Handler state for the external plugins API.
#[derive(Clone)]
pub struct ExternalPluginsAppState {
    pub service: Arc<ExternalPluginsService>,
    /// Installing a plugin downloads and executes a binary on the host — the
    /// most privileged write this API exposes — so it must leave a trail.
    pub audit_service: Arc<dyn temps_core::AuditLogger>,
}

/// List all running external plugins and their manifests.
///
/// Requires only a valid session/token (no specific permission) since the
/// manifest drives sidebar navigation rendering for every authenticated
/// user, not just admins.
#[utoipa::path(
    tag = "External Plugins",
    get,
    path = "/x/plugins",
    responses(
        (status = 200, description = "List of all running external plugins", body = Vec<PluginManifest>),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
async fn list_external_plugins(
    RequireAuth(_auth): RequireAuth,
    State(state): State<ExternalPluginsAppState>,
) -> Json<Vec<PluginManifest>> {
    Json(state.service.manifests().await)
}

/// Response from the reload endpoint.
#[derive(Debug, Serialize, ToSchema)]
pub struct ReloadResponse {
    /// Number of plugins successfully loaded after reload
    pub loaded: usize,
    /// Names of loaded plugins
    pub plugins: Vec<String>,
    /// Human-readable status message
    pub message: String,
}

/// Reload all external plugins.
///
/// Stops all running plugin processes, re-scans the plugins directory,
/// starts any discovered binaries, and hot-swaps the proxy router so new
/// and removed plugins take effect immediately without a server restart.
///
/// Requires `SystemAdmin` permission.
#[utoipa::path(
    tag = "External Plugins",
    post,
    path = "/x/plugins/reload",
    responses(
        (status = 200, description = "Plugins reloaded successfully", body = ReloadResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
async fn reload_plugins(
    RequireAuth(auth): RequireAuth,
    State(state): State<ExternalPluginsAppState>,
    Extension(metadata): Extension<temps_core::RequestMetadata>,
) -> Result<(StatusCode, Json<ReloadResponse>), Problem> {
    permission_guard!(auth, SystemAdmin);

    tracing::info!("Admin triggered plugin reload");

    let manifests = state.service.reload_plugins().await;
    let names: Vec<String> = manifests.iter().map(|m| m.name.clone()).collect();
    let count = names.len();

    record_audit(
        &state,
        &PluginsReloadedAudit {
            context: audit_context(&auth, &metadata),
            loaded: count,
            plugins: names.clone(),
        },
    )
    .await;

    Ok((
        StatusCode::OK,
        Json(ReloadResponse {
            loaded: count,
            plugins: names,
            message: format!("Reload complete. {} plugin(s) loaded.", count),
        }),
    ))
}

// ---------------------------------------------------------------------------
// Plugin install endpoints
// ---------------------------------------------------------------------------

/// A plugin this instance knows how to fetch and install: its registry
/// manifest URL and the binary filename it installs as inside the plugins
/// directory.
///
/// `name` is the identity key everywhere it matters — it is the plugin's own
/// manifest-declared name once running, and `ExternalPluginManager` indexes
/// running processes by exactly this string (see
/// `ExternalPluginManager::start_plugin`, which inserts under
/// `result_manifest.name`). Any status/reload/is_running check MUST use
/// `name`, never a derived binary filename — a prior version of this code
/// checked `is_running(&binary_name)`, which never matched the manager's key
/// and made a running plugin permanently report as "not configured", and
/// made the install flow always take the "start fresh" branch instead of
/// "reload", leaking the old process (it was silently overwritten in the
/// process map without ever being shut down).
struct KnownPlugin {
    /// User-facing plugin name and the manager's process-table key.
    name: &'static str,
    /// Registry manifest URL. This is the **only** URL ever fetched for this
    /// plugin — never taken from an untrusted caller, to prevent SSRF / RCE.
    /// If the release host changes this must be updated and the binary
    /// redeployed.
    manifest_url: &'static str,
    /// Binary filename as it appears inside the release tarball and as it is
    /// written into the plugins directory.
    binary_name: &'static str,
}

/// Fixed set of plugins this instance knows how to install. Intentionally
/// small and compile-time — this is not a general marketplace where a caller
/// picks an arbitrary URL, both because the manifest is the trust root for
/// the whole install flow (its asset URLs and SHA-256 digests are what get
/// downloaded and executed) and because every self-hosted instance needs the
/// same known-good set. Add an entry here to make a new plugin installable.
const KNOWN_PLUGINS: &[KnownPlugin] = &[KnownPlugin {
    name: "vibetemps",
    // Served by temps.sh rather than a code-hosting release page: the
    // manifest is the trust root for the whole install flow, so it has to
    // live on a host we control and can serve to every self-hosted instance.
    manifest_url: "https://temps.sh/api/plugins/vibetemps/manifest.json",
    binary_name: "temps-vibetemps-plugin",
}];

fn known_plugin(name: &str) -> Option<&'static KnownPlugin> {
    KNOWN_PLUGINS.iter().find(|p| p.name == name)
}

fn known_plugin_names() -> String {
    KNOWN_PLUGINS
        .iter()
        .map(|p| p.name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// One entry in the installable-plugin registry listing.
#[derive(Debug, Serialize, ToSchema)]
pub struct PluginRegistryEntry {
    /// Plugin name (registry key).
    pub name: String,
    /// Whether the plugin binary is already installed (present on disk).
    pub installed: bool,
    /// The manifest fetched from the registry, if reachable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest: Option<PluginRegistryManifest>,
    /// Human-readable reason when the manifest could not be fetched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Request body for the plugin install endpoint.
#[derive(Debug, Deserialize, ToSchema)]
pub struct InstallPluginRequest {
    /// Name of the plugin to install — must match a `KNOWN_PLUGINS` entry.
    pub name: String,
    /// Specific version hint (currently unused; install always fetches latest).
    pub version: Option<String>,
}

/// Response for the plugin install endpoint.
#[derive(Debug, Serialize, ToSchema)]
pub struct InstallPluginResponse {
    /// Name of the installed plugin.
    pub name: String,
    /// Version that was installed.
    pub version: String,
    /// Absolute path of the installed binary.
    pub path: String,
    /// Whether the plugin process was reloaded after install.
    pub reloaded: bool,
    /// Human-readable status message.
    pub message: String,
}

/// Response for the per-plugin status endpoint.
#[derive(Debug, Serialize, ToSchema)]
pub struct PluginStatusResponse {
    /// Whether the plugin binary is present in the plugins directory **and**
    /// the plugin process is currently running.
    pub configured: bool,
    /// Why the plugin is not configured (when `configured` is false).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Console path the operator should visit to configure or install it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup_path: Option<String>,
}

/// List every plugin this instance knows how to install, whether it's
/// already installed, and its registry manifest.
///
/// Iterates `KNOWN_PLUGINS` — today that's a single entry (VibeTemps), but
/// the endpoint returns a list rather than a singular "the one plugin"
/// response so adding a second installable plugin never needs an API
/// change, only a new `KNOWN_PLUGINS` entry. Requires `SystemAdmin`
/// permission.
#[utoipa::path(
    tag = "External Plugins",
    get,
    path = "/x/plugins/registry",
    responses(
        (status = 200, description = "Installable-plugin registry", body = Vec<PluginRegistryEntry>),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
async fn list_plugin_registry(
    RequireAuth(auth): RequireAuth,
    State(state): State<ExternalPluginsAppState>,
) -> Result<(StatusCode, Json<Vec<PluginRegistryEntry>>), Problem> {
    permission_guard!(auth, SystemAdmin);

    let plugins_dir = state.service.manager().config().plugins_dir.clone();
    let mut entries = Vec::with_capacity(KNOWN_PLUGINS.len());

    for known in KNOWN_PLUGINS {
        let installed = plugins_dir.join(known.binary_name).exists();
        let (manifest, reason) = match PluginInstaller::fetch_manifest(known.manifest_url).await {
            Ok(manifest) => (Some(manifest), None),
            Err(e) => (
                None,
                Some(format!(
                    "Could not fetch {} manifest from {}: {}",
                    known.name, known.manifest_url, e
                )),
            ),
        };
        entries.push(PluginRegistryEntry {
            name: known.name.to_string(),
            installed,
            manifest,
            reason,
        });
    }

    Ok((StatusCode::OK, Json(entries)))
}

/// Download, verify, and install an external plugin binary.
///
/// `name` must match a `KNOWN_PLUGINS` entry. After a successful install the
/// plugin process is (re)started automatically. Requires `SystemAdmin`
/// permission.
#[utoipa::path(
    tag = "External Plugins",
    post,
    path = "/x/plugins/install",
    request_body = InstallPluginRequest,
    responses(
        (status = 200, description = "Plugin installed and started", body = InstallPluginResponse),
        (status = 400, description = "Invalid or unsupported plugin name"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
        (status = 500, description = "Download, checksum, or install failure"),
    ),
    security(("bearer_auth" = []))
)]
async fn install_plugin(
    RequireAuth(auth): RequireAuth,
    State(state): State<ExternalPluginsAppState>,
    Extension(metadata): Extension<temps_core::RequestMetadata>,
    Json(request): Json<InstallPluginRequest>,
) -> Result<(StatusCode, Json<InstallPluginResponse>), Problem> {
    permission_guard!(auth, SystemAdmin);

    let known = known_plugin(&request.name).ok_or_else(|| {
        error_builder::bad_request()
            .title("Unsupported Plugin")
            .detail(format!(
                "'{}' is not a known installable plugin. Known plugins: {}.",
                request.name,
                known_plugin_names()
            ))
            .build()
    })?;

    // Version pinning is not implemented: the manifest URL always resolves to
    // the current release. Honouring the field silently would be worse than
    // refusing it — an operator pinning a known-good version would believe
    // they had pinned it while receiving whatever the registry now serves.
    if let Some(requested) = request
        .version
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        return Err(error_builder::bad_request()
            .title("Version Pinning Not Supported")
            .detail(format!(
                "Cannot install '{}' at version '{requested}': this instance always installs the \
                 current release named by the registry manifest. Omit `version` to proceed.",
                known.name
            ))
            .build());
    }

    let manifest = PluginInstaller::fetch_manifest(known.manifest_url)
        .await
        .map_err(|e| install_problem(&e))?;

    let plugins_dir = state.service.manager().config().plugins_dir.clone();
    let installer = PluginInstaller::new();

    let platform = crate::install::platform_target().map_err(|e| install_problem(&e))?;
    let sha256 = manifest
        .platforms
        .get(&platform)
        .map(|asset| asset.sha256.clone())
        .unwrap_or_default();

    let installed_path = installer
        .install(known.binary_name, &manifest, &plugins_dir)
        .await
        .map_err(|e| install_problem(&e))?;

    // Start or reload the plugin process — non-fatal on failure (binary is
    // installed; operator can trigger a manual reload). `manifest.name` (the
    // plugin's own declared identity, e.g. "vibetemps") is what
    // ExternalPluginManager indexes running processes by — NOT
    // `known.binary_name` — so it must be the identity passed here for
    // is_running/reload to find the right entry. `known.binary_name` is only
    // needed for the fresh-start filesystem path.
    //
    // The identity passed here is `known.name`, the compile-time constant —
    // NOT `manifest.name`, which is remote data. Keying the running-process
    // lookup on a field the registry controls means a manifest declaring
    // another plugin's name would reload *that* plugin and leave the binary
    // just installed here unstarted, while still reporting success.
    let reloaded = match state
        .service
        .start_or_reload_plugin(known.name, known.binary_name)
        .await
    {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!(
                plugin = %manifest.name,
                "Plugin binary installed but process start failed: {}. \
                 Trigger a manual reload via POST /x/plugins/reload.",
                e
            );
            false
        }
    };

    record_audit(
        &state,
        &PluginInstalledAudit {
            context: audit_context(&auth, &metadata),
            // The registry-declared name is recorded alongside the local
            // identity: if they ever diverge, the audit trail is where that
            // shows up.
            plugin_name: known.name.to_string(),
            version: manifest.version.clone(),
            manifest_url: known.manifest_url.to_string(),
            platform,
            sha256,
            install_path: installed_path.display().to_string(),
            process_started: reloaded,
        },
    )
    .await;

    Ok((
        StatusCode::OK,
        Json(InstallPluginResponse {
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            path: installed_path.display().to_string(),
            reloaded,
            message: if reloaded {
                format!(
                    "{} plugin v{} installed and started successfully.",
                    manifest.name, manifest.version
                )
            } else {
                format!(
                    "{} plugin v{} installed. Process start failed — use POST /x/plugins/reload to activate it.",
                    manifest.name, manifest.version
                )
            },
        }),
    ))
}

/// Get the running status of a named external plugin.
///
/// Returns `configured: true` when the plugin binary is present on disk
/// **and** the plugin process is currently running. Any authenticated user
/// may call this endpoint (same permission level as `GET /x/plugins`).
#[utoipa::path(
    tag = "External Plugins",
    get,
    path = "/x/plugins/{name}/status",
    params(
        ("name" = String, Path, description = "Plugin name (e.g. 'vibetemps')")
    ),
    responses(
        (status = 200, description = "Plugin status", body = PluginStatusResponse),
        (status = 401, description = "Unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
async fn get_plugin_status(
    RequireAuth(_auth): RequireAuth,
    Path(name): Path<String>,
    State(state): State<ExternalPluginsAppState>,
) -> Result<(StatusCode, Json<PluginStatusResponse>), Problem> {
    // The manager indexes running processes by the plugin's own
    // manifest-declared name (see ExternalPluginManager::start_plugin, which
    // inserts under `result_manifest.name`) — so `name` itself, not a
    // derived binary filename, is the correct key here.
    if state.service.manager().is_running(&name).await {
        return Ok((
            StatusCode::OK,
            Json(PluginStatusResponse {
                configured: true,
                reason: None,
                setup_path: None,
            }),
        ));
    }

    // Not running. For a plugin we know how to install, distinguish "never
    // installed" from "binary present but process not running" using the
    // registry's known binary filename. For anything else (a plugin dropped
    // in manually, outside the install flow) we have no reliable filename to
    // check, so just report that it isn't running.
    let reason = match known_plugin(&name) {
        Some(known) => {
            let plugins_dir = state.service.manager().config().plugins_dir.clone();
            if plugins_dir.join(known.binary_name).exists() {
                format!(
                    "The {} plugin binary is installed but the process is not running. \
                     Trigger a reload via the plugin settings page.",
                    name
                )
            } else {
                format!(
                    "The {} plugin is not installed. Install it from the plugin settings page.",
                    name
                )
            }
        }
        None => format!("The {} plugin is not currently running.", name),
    };

    Ok((
        StatusCode::OK,
        Json(PluginStatusResponse {
            configured: false,
            reason: Some(reason),
            setup_path: Some("/settings/plugins".to_string()),
        }),
    ))
}

/// One entry in the browsable plugin catalogue, after local verification.
#[derive(Debug, Serialize, ToSchema)]
pub struct PluginCatalogEntry {
    /// Registry key.
    pub name: String,
    /// Display name.
    pub title: String,
    /// One-line description.
    pub summary: String,
    /// Longer description.
    pub description: String,
    /// Maintainer.
    pub author: String,
    /// Grouping label.
    pub category: String,
    /// Source repository, when public.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// Documentation URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docs_url: Option<String>,
    /// Latest published version, or `None` when nothing is released yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<String>,
    /// Platform keys the published release covers.
    pub platforms: Vec<String>,
    /// Whether the plugin binary is already present on this host.
    pub installed: bool,
    /// Whether **this build** would accept an install request for it.
    pub installable: bool,
    /// Machine-readable reason it is not installable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rejection: Option<CatalogRejection>,
    /// Operator-facing reason it is not installable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The catalogue response, including the unreachable-registry state.
#[derive(Debug, Serialize, ToSchema)]
pub struct PluginCatalogResponse {
    /// Whether the registry was reachable and returned a parseable catalogue.
    pub available: bool,
    /// Why the catalogue is unavailable, when `available` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Registry endpoint consulted. Shown so an operator behind a proxy or an
    /// air gap can see exactly which host failed rather than guessing.
    pub source: String,
    /// Locally verified catalogue entries. Empty when unavailable.
    pub plugins: Vec<PluginCatalogEntry>,
}

/// Browse the plugins published in the registry.
///
/// Distinct from `/x/plugins/registry`, which reports on the plugins this
/// build already knows how to install. This endpoint answers the broader
/// question — what exists at all — and is therefore the only place a plugin
/// released after this binary was built can appear.
///
/// Every entry is verified locally before it is returned (see
/// [`crate::catalog`]): a name outside `KNOWN_PLUGINS` comes back
/// `installable: false` with "upgrade temps" rather than being dropped, and a
/// manifest URL that disagrees with this build's compile-time value comes
/// back refused. Nothing the registry says is ever passed to the installer.
///
/// A registry outage returns `200` with `available: false` and a reason, not
/// an error status: "the catalogue is unreachable" is a state the plugins
/// screen must render, and a 5xx would leave the client unable to tell it
/// apart from "this endpoint does not exist on this version".
///
/// Requires `SystemAdmin` permission — the same gate as the rest of this
/// module, since the catalogue exists to drive host-level installs.
#[utoipa::path(
    tag = "External Plugins",
    get,
    path = "/x/plugins/catalog",
    responses(
        (status = 200, description = "Published plugin catalogue", body = PluginCatalogResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Insufficient permissions"),
    ),
    security(("bearer_auth" = []))
)]
async fn list_plugin_catalog(
    RequireAuth(auth): RequireAuth,
    State(state): State<ExternalPluginsAppState>,
) -> Result<(StatusCode, Json<PluginCatalogResponse>), Problem> {
    permission_guard!(auth, SystemAdmin);

    let remote = match crate::catalog::fetch_catalog(crate::catalog::CATALOG_URL).await {
        Ok(entries) => entries,
        Err(e) => {
            // WARN, not ERROR: an unreachable registry is expected on an
            // air-gapped host and must not read as a fault in this binary.
            tracing::warn!(error = %e, "Plugin catalog unavailable");
            return Ok((
                StatusCode::OK,
                Json(PluginCatalogResponse {
                    available: false,
                    reason: Some(e.to_string()),
                    source: crate::catalog::CATALOG_URL.to_string(),
                    plugins: Vec::new(),
                }),
            ));
        }
    };

    let plugins_dir = state.service.manager().config().plugins_dir.clone();
    let plugins = remote
        .into_iter()
        .map(|entry| {
            let known = known_plugin(&entry.name);
            let verification = crate::catalog::verify(&entry, known.map(|k| k.manifest_url));

            if let Some(reason) = verification.reason.as_deref() {
                // A mismatched manifest URL is a security-relevant event, not
                // a cosmetic one — log it loudly enough to be greppable.
                tracing::warn!(plugin = %entry.name, "{}", reason);
            }

            PluginCatalogEntry {
                installed: known.is_some_and(|k| plugins_dir.join(k.binary_name).exists()),
                installable: verification.installable,
                rejection: verification.rejection,
                reason: verification.reason,
                name: entry.name,
                title: entry.title,
                summary: entry.summary,
                description: entry.description,
                author: entry.author,
                category: entry.category,
                repository: entry.repository,
                docs_url: entry.docs_url,
                latest_version: entry.latest_version,
                platforms: entry.platforms,
            }
        })
        .collect();

    Ok((
        StatusCode::OK,
        Json(PluginCatalogResponse {
            available: true,
            reason: None,
            source: crate::catalog::CATALOG_URL.to_string(),
            plugins,
        }),
    ))
}

/// Build the router for external plugin management endpoints.
pub fn configure_routes() -> Router<ExternalPluginsAppState> {
    Router::new()
        .route("/x/plugins", get(list_external_plugins))
        .route("/x/plugins/reload", post(reload_plugins))
        .route("/x/plugins/registry", get(list_plugin_registry))
        .route("/x/plugins/install", post(install_plugin))
        .route("/x/plugins/{name}/status", get(get_plugin_status))
        .route("/x/plugins/catalog", get(list_plugin_catalog))
}

#[derive(OpenApiTrait)]
#[openapi(
    paths(
        list_external_plugins,
        reload_plugins,
        list_plugin_registry,
        install_plugin,
        get_plugin_status,
        list_plugin_catalog,
    ),
    components(
        schemas(
            PluginManifest,
            NavEntry,
            NavSection,
            UiManifest,
            UiRoute,
            ReloadResponse,
            PluginRegistryEntry,
            PluginRegistryManifest,
            PlatformAsset,
            InstallPluginRequest,
            InstallPluginResponse,
            PluginStatusResponse,
            PluginCatalogEntry,
            PluginCatalogResponse,
            CatalogRejection,
        )
    ),
    tags(
        (name = "External Plugins", description = "External plugin management and discovery")
    )
)]
pub struct ExternalPluginsApiDoc;

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::Utc;
    use temps_auth::context::AuthContext;
    use temps_auth::permissions::Role;
    use temps_entities::users;

    use crate::manager::ExternalPluginConfig;

    fn mock_db() -> Arc<sea_orm::DatabaseConnection> {
        Arc::new(sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection())
    }

    /// Captures audit operations so tests can assert a privileged write was
    /// actually recorded, not merely that it returned 200.
    #[derive(Default)]
    struct RecordingAuditLogger {
        operations: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl temps_core::AuditLogger for RecordingAuditLogger {
        async fn create_audit_log(
            &self,
            operation: &dyn temps_core::audit::AuditOperation,
        ) -> anyhow::Result<()> {
            self.operations
                .lock()
                .unwrap()
                .push(operation.operation_type());
            Ok(())
        }
    }

    fn test_state() -> (ExternalPluginsAppState, Arc<RecordingAuditLogger>) {
        let config = ExternalPluginConfig::new(
            std::env::temp_dir().join("temps-external-plugins-handler-test"),
            "postgres://localhost/test".to_string(),
        );
        let audit = Arc::new(RecordingAuditLogger::default());
        (
            ExternalPluginsAppState {
                service: Arc::new(ExternalPluginsService::new_empty(config, None, mock_db())),
                audit_service: audit.clone(),
            },
            audit,
        )
    }

    fn test_metadata() -> Extension<temps_core::RequestMetadata> {
        Extension(temps_core::RequestMetadata {
            ip_address: "203.0.113.7".to_string(),
            user_agent: "test-agent".to_string(),
            headers: Default::default(),
            visitor_id_cookie: None,
            session_id_cookie: None,
            base_url: "http://localhost".to_string(),
            scheme: "http".to_string(),
            host: "localhost".to_string(),
            is_secure: false,
        })
    }

    fn test_user(id: i32) -> users::Model {
        let now = Utc::now();
        users::Model {
            id,
            name: "Test User".to_string(),
            email: format!("user{id}@example.com"),
            password_hash: None,
            email_verified: true,
            email_verification_token: None,
            email_verification_expires: None,
            password_reset_token: None,
            password_reset_expires: None,
            must_change_password: false,
            deleted_at: None,
            mfa_secret: None,
            mfa_enabled: false,
            mfa_recovery_codes: None,
            oidc_subject: None,
            oidc_provider_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn user_auth(role: Role) -> RequireAuth {
        RequireAuth(AuthContext::new_session(test_user(1), role))
    }

    // Regression tests for the unauthenticated-access finding: `reload_plugins`
    // stopped/restarted every plugin process and `list_external_plugins`
    // leaked the full plugin manifest to any caller because neither handler
    // had a `RequireAuth` extractor, despite the OpenAPI docs on this file
    // claiming `SystemAdmin` was required for reload.

    #[tokio::test]
    async fn reload_plugins_rejects_non_admin() {
        let (state, audit) = test_state();
        let err = reload_plugins(user_auth(Role::User), State(state), test_metadata())
            .await
            .expect_err("a plain User role must not be able to reload plugins");
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
        assert!(
            audit.operations.lock().unwrap().is_empty(),
            "a rejected reload must not be recorded as one that happened"
        );
    }

    #[tokio::test]
    async fn reload_plugins_allows_platform_admin() {
        let (state, audit) = test_state();
        let (status, _) = reload_plugins(
            user_auth(Role::PlatformAdmin),
            State(state),
            test_metadata(),
        )
        .await
        .expect("a PlatformAdmin must be able to reload plugins");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            audit.operations.lock().unwrap().as_slice(),
            ["EXTERNAL_PLUGINS_RELOADED"],
            "restarting plugin processes must leave an audit trail"
        );
    }

    /// Version pinning is unimplemented, and silently ignoring the field would
    /// leave an operator believing they had pinned a known-good release.
    #[tokio::test]
    async fn install_rejects_a_version_pin() {
        let (state, audit) = test_state();
        let err = install_plugin(
            user_auth(Role::PlatformAdmin),
            State(state),
            test_metadata(),
            Json(InstallPluginRequest {
                name: "vibetemps".to_string(),
                version: Some("1.2.3".to_string()),
            }),
        )
        .await
        .expect_err("a version pin must be refused rather than silently ignored");
        assert_eq!(err.status_code, StatusCode::BAD_REQUEST);
        assert!(
            audit.operations.lock().unwrap().is_empty(),
            "a refused install must not be audited as an install"
        );
    }

    #[tokio::test]
    async fn install_rejects_unknown_plugin_name() {
        let (state, _audit) = test_state();
        let err = install_plugin(
            user_auth(Role::PlatformAdmin),
            State(state),
            test_metadata(),
            Json(InstallPluginRequest {
                name: "../../etc/passwd".to_string(),
                version: None,
            }),
        )
        .await
        .expect_err("only allowlisted plugin names are installable");
        assert_eq!(err.status_code, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn install_rejects_non_admin() {
        let (state, _audit) = test_state();
        let err = install_plugin(
            user_auth(Role::User),
            State(state),
            test_metadata(),
            Json(InstallPluginRequest {
                name: "vibetemps".to_string(),
                version: None,
            }),
        )
        .await
        .expect_err("installing a binary must require SystemAdmin");
        assert_eq!(err.status_code, StatusCode::FORBIDDEN);
    }

    /// The status→variant mapping replaced substring-matching on the rendered
    /// message. Pin the classes so a reworded message can't silently change a
    /// 502 into a 500.
    #[test]
    fn install_errors_map_to_stable_status_codes() {
        let cases: Vec<(InstallError, StatusCode)> = vec![
            (
                InstallError::UnsupportedPlatform {
                    os: "plan9".into(),
                    arch: "sparc".into(),
                },
                StatusCode::BAD_REQUEST,
            ),
            (
                InstallError::InsecureAssetUrl {
                    plugin: "p".into(),
                    url: "http://x".into(),
                },
                StatusCode::BAD_GATEWAY,
            ),
            (
                InstallError::ChecksumMismatch {
                    plugin: "p".into(),
                    version: "1".into(),
                    platform: "linux-amd64".into(),
                    reason: "r".into(),
                },
                StatusCode::BAD_GATEWAY,
            ),
            (
                InstallError::TooLarge {
                    what: "Download",
                    url: "https://x".into(),
                    limit: 1,
                },
                StatusCode::BAD_GATEWAY,
            ),
            (
                InstallError::Write {
                    plugin: "p".into(),
                    path: "/tmp/x".into(),
                    reason: "r".into(),
                },
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(
                install_problem(&error).status_code,
                expected,
                "unexpected status for {error:?}"
            );
        }
    }

    #[tokio::test]
    async fn list_external_plugins_allows_any_authenticated_role() {
        // Any signed-in user must be able to list plugins — the sidebar nav
        // for every authenticated user depends on this endpoint. Only
        // unauthenticated (no session at all) callers should be rejected,
        // which `RequireAuth`'s extractor enforces at the HTTP layer before
        // this handler body ever runs.
        let (state, _audit) = test_state();
        let Json(manifests) = list_external_plugins(user_auth(Role::User), State(state)).await;
        assert!(manifests.is_empty());
    }

    #[test]
    fn test_openapi_spec_has_plugins_path() {
        let spec = ExternalPluginsApiDoc::openapi();
        assert!(
            spec.paths.paths.contains_key("/x/plugins"),
            "OpenAPI spec must contain /x/plugins path"
        );
    }

    #[test]
    fn test_openapi_spec_has_schemas() {
        let spec = ExternalPluginsApiDoc::openapi();
        let components = spec.components.expect("should have components");
        assert!(
            components.schemas.contains_key("PluginManifest"),
            "OpenAPI spec must contain PluginManifest schema"
        );
        assert!(
            components.schemas.contains_key("NavEntry"),
            "OpenAPI spec must contain NavEntry schema"
        );
        assert!(
            components.schemas.contains_key("NavSection"),
            "OpenAPI spec must contain NavSection schema"
        );
    }

    #[test]
    fn test_openapi_spec_has_reload_path() {
        let spec = ExternalPluginsApiDoc::openapi();
        assert!(
            spec.paths.paths.contains_key("/x/plugins/reload"),
            "OpenAPI spec must contain /x/plugins/reload path"
        );
    }

    #[test]
    fn test_openapi_spec_has_reload_response_schema() {
        let spec = ExternalPluginsApiDoc::openapi();
        let components = spec.components.expect("should have components");
        assert!(
            components.schemas.contains_key("ReloadResponse"),
            "OpenAPI spec must contain ReloadResponse schema"
        );
    }

    #[test]
    fn test_reload_response_serialization() {
        let response = ReloadResponse {
            loaded: 2,
            plugins: vec!["seo-analyzer".into(), "monitoring".into()],
            message: "Reload complete. 2 plugin(s) loaded.".into(),
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["loaded"], 2);
        assert_eq!(json["plugins"][0], "seo-analyzer");
        assert_eq!(json["plugins"][1], "monitoring");
    }
}
