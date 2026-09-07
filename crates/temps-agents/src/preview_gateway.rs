// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Preview gateway supervisor.
//!
//! Reconciles this instance's preview-gateway container on the local Docker
//! host. Called from `temps serve` startup, but always runs in a background
//! task — the proxy server (80/443) MUST NOT be blocked on this.
//!
//! The container is named `temps-preview-gateway` by default, which is right
//! for a normal install that owns the whole host. It is a *setting* rather
//! than a constant so that several Temps instances can share one Docker
//! daemon — see `PreviewGatewaySettings::container_name` for why sharing the
//! name silently breaks previews.
//!
//! What it guarantees, in order:
//! 1. An internal control network and a dedicated ingress network exist.
//! 2. The pinned routing-gateway and ingress-relay images are present.
//! 3. The routing gateway runs only on internal networks. A separate,
//!    hardened TCP relay owns the `127.0.0.1:<port>` publish and connects to
//!    the router over the internal control network. Any drift recreates both.
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
use regex::Regex;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, DatabaseConnection, EntityTrait};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use temps_core::PreviewGatewaySettings;
use tracing::{debug, info, warn};

/// Immutable multi-platform manifest reference. Bumped per release.
pub const PREVIEW_GATEWAY_IMAGE: &str = "ghcr.io/gotempsh/temps-preview-gateway@sha256:02d5cdd382c3285d569032e84321d5ce8fc089372a3f08651119f6eda8cb1448";

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

/// Internal control network between the host-published ingress relay and the
/// routing gateway. The routing gateway also joins each per-sandbox internal
/// network, but never receives an externally routed interface.
pub const PREVIEW_GATEWAY_NETWORK: &str = "temps-preview-gateway-control-v7";
const PREVIEW_GATEWAY_INGRESS_NETWORK: &str = "temps-preview-gateway-ingress-v3";
const PREVIEW_GATEWAY_LABEL: &str = "sh.temps.preview-gateway";
const PREVIEW_GATEWAY_NETWORK_LABEL: &str = "sh.temps.preview-gateway-control";
const PREVIEW_GATEWAY_NETWORK_POLICY_VERSION: &str = "2";
const PREVIEW_GATEWAY_INGRESS_LABEL: &str = "sh.temps.preview-gateway-ingress";
const PREVIEW_GATEWAY_SECURITY_PROTOCOL_LABEL: &str = "sh.temps.preview-gateway-security-protocol";
const PREVIEW_GATEWAY_SECURITY_PROTOCOL_VERSION: &str = "strip-token-v1";
const BRIDGE_ENABLE_ICC_OPTION: &str = "com.docker.network.bridge.enable_icc";
const BRIDGE_ENABLE_MASQUERADE_OPTION: &str = "com.docker.network.bridge.enable_ip_masquerade";
const BRIDGE_GATEWAY_MODE_IPV4_OPTION: &str = "com.docker.network.bridge.gateway_mode_ipv4";
const SANDBOX_NETWORK_OWNER_LABEL: &str = "sh.temps.sandbox-network-for";
const PREVIEW_GATEWAY_INGRESS_SCRIPT: &str = r#"
const net = require("net");
const upstreamHost = process.env.PREVIEW_GATEWAY_UPSTREAM;
if (!upstreamHost || !/^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(upstreamHost)) {
  throw new Error("PREVIEW_GATEWAY_UPSTREAM must be a Docker DNS name");
}
const server = net.createServer((client) => {
  const upstream = net.connect({ host: upstreamHost, port: 8080 });
  const close = () => { client.destroy(); upstream.destroy(); };
  client.on("error", close);
  upstream.on("error", close);
  client.pipe(upstream);
  upstream.pipe(client);
});
server.listen(8080, "0.0.0.0");
"#;

/// The container name this instance's gateway uses: the configured one, or
/// the default. Every code path that names the container goes through here,
/// so `inspect`, `reconcile`, `status` and `logs` can never disagree about
/// which container they are talking about.
pub fn container_name(settings: &PreviewGatewaySettings) -> String {
    let name = settings.container_name.trim();
    if name.is_empty() {
        PREVIEW_GATEWAY_CONTAINER.to_string()
    } else {
        name.to_string()
    }
}

/// Default host port the gateway publishes to. Bound on 127.0.0.1 only —
/// the host-side Pingora reaches it via this port after authenticating.
pub const DEFAULT_PREVIEW_GATEWAY_HOST_PORT: u16 = 8090;

/// Internal port the gateway listens on inside its container.
const GATEWAY_CONTAINER_PORT: u16 = 8080;

const MAX_GATEWAY_DIAGNOSTIC_CHARS: usize = 4_000;

/// Redact credential-shaped values and bound Docker diagnostics before they
/// cross an HTTP or log boundary. The complete safe context/source chain is
/// retained so operational failures remain diagnosable.
pub(crate) fn sanitize_gateway_diagnostic(diagnostic: &str, sensitive_values: &[&str]) -> String {
    let mut sanitized = diagnostic.to_string();
    for value in sensitive_values {
        if !value.is_empty() {
            sanitized = sanitized.replace(value, "[redacted]");
        }
    }

    for (pattern, replacement) in [
        (
            r#"(?i)(bearer\s+)[A-Za-z0-9._~+/-]+={0,2}"#,
            "${1}[redacted]",
        ),
        (
            r#"(?i)((?:authorization|proxy-authorization|x-registry-auth)\s*[:=]\s*)[^\r\n,;]+"#,
            "${1}[redacted]",
        ),
        (r#"(://)[^\s/@]+@"#, "${1}[redacted]@"),
        (
            r#"\b[^\s/:@]+:[^\s/@]+@([A-Za-z0-9.-]+)"#,
            "[redacted]@${1}",
        ),
        (
            r#"(?i)([a-z][a-z0-9+.-]*://[^\s?]+)\?[^\s]+"#,
            "${1}?[redacted]",
        ),
        (
            r#"(?i)([\"']?\b[a-z0-9_-]*(?:password|passwd|token|secret|api[_-]?key|authorization)[a-z0-9_-]*[\"']?\s*[:=]\s*)(?:\"[^\"]*\"|'[^']*'|[^\s,;}\]]+)"#,
            "${1}[redacted]",
        ),
    ] {
        if let Ok(regex) = Regex::new(pattern) {
            sanitized = regex.replace_all(&sanitized, replacement).into_owned();
        }
    }

    let mut chars = sanitized.chars();
    let bounded: String = chars.by_ref().take(MAX_GATEWAY_DIAGNOSTIC_CHARS).collect();
    if chars.next().is_some() {
        format!("{bounded}\n… diagnostic truncated; inspect authenticated server logs for more")
    } else {
        bounded
    }
}

fn docker_operation_error(
    context: impl Into<String>,
    error: bollard::errors::Error,
) -> anyhow::Error {
    anyhow::Error::new(error).context(context.into())
}

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
            container_name: container_name(settings),
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
    // Tell the in-process proxy where to dial before we touch Docker: even if
    // the reconcile fails, the port it should be using is the configured one.
    export_host_port(spec.host_port);

    disable_unsafe_existing_gateway(&docker, &spec.container_name).await?;
    ensure_network(&docker, &spec.network).await?;
    ensure_ingress_network(&docker).await?;
    let desired_image_id = match ensure_image(&docker, &spec.image).await {
        Ok(image_id) => image_id,
        Err(error) => {
            // An older router forwards the node-wide authentication token into a
            // tenant sandbox. Leaving that container reachable after rejecting
            // its image would not be a real fail-closed policy, so remove both
            // halves before surfacing the incompatibility.
            remove_gateway_pair(&docker, &spec.container_name)
                .await
                .context("failed to disable an unverified preview gateway")?;
            return Err(error.context("preview gateway disabled because its image is unverified"));
        }
    };
    let ingress_image = crate::sandbox::docker::image_name_for_runtime("node");
    ensure_image_present(&docker, &ingress_image).await?;

    match inspect(&docker, &spec.container_name).await? {
        Some(existing)
            if container_matches(&existing, &spec, &desired_image_id)
                && existing.running
                && ingress_matches(&docker, &spec, &ingress_image).await? =>
        {
            debug!("preview gateway already running with desired spec");
        }
        Some(existing) => {
            info!(
                running = existing.running,
                image_match = existing.image == spec.image,
                "preview gateway drift detected — recreating"
            );
            remove_gateway_pair(&docker, &spec.container_name).await?;
            create_and_start(&docker, &spec, &ingress_image).await?;
        }
        None => {
            info!("preview gateway not present — creating");
            remove_if_present(&docker, &ingress_container_name(&spec.container_name)).await?;
            create_and_start(&docker, &spec, &ingress_image).await?;
        }
    }

    connect_sandbox_networks(&docker, &spec.container_name).await?;

    info!(
        "preview gateway ready on 127.0.0.1:{} → {}:{}",
        spec.host_port, spec.container_name, GATEWAY_CONTAINER_PORT
    );
    Ok(())
}

async fn connect_sandbox_networks(docker: &Docker, container_name: &str) -> Result<()> {
    let networks = docker
        .list_networks(None::<ListNetworksOptions>)
        .await
        .context("failed to discover isolated sandbox networks")?;
    for network in networks {
        let managed = network
            .labels
            .as_ref()
            .and_then(|labels| labels.get(SANDBOX_NETWORK_OWNER_LABEL))
            .is_some();
        if !managed {
            continue;
        }
        let Some(network_name) = network.name.as_deref() else {
            continue;
        };
        let request = bollard::models::NetworkConnectRequest {
            container: container_name.to_string(),
            endpoint_config: None,
        };
        if let Err(error) = docker.connect_network(network_name, request).await {
            let message = error.to_string();
            if !message.contains("already exists") && !message.contains("already connected") {
                return Err(anyhow!(
                    "failed to attach preview gateway to isolated sandbox network {network_name}: {message}"
                ));
            }
        }
    }
    Ok(())
}

async fn ensure_network(docker: &Docker, name: &str) -> Result<()> {
    let networks = docker
        .list_networks(None::<ListNetworksOptions>)
        .await
        .context("failed to list docker networks")?;
    if let Some(network) = networks.iter().find(|n| n.name.as_deref() == Some(name)) {
        let policy_label = network
            .labels
            .as_ref()
            .and_then(|labels| labels.get(PREVIEW_GATEWAY_NETWORK_LABEL));
        let options = network.options.as_ref();
        if network.driver.as_deref() != Some("bridge")
            || network.internal != Some(true)
            || network.enable_ipv6 != Some(false)
            || policy_label.map(String::as_str) != Some(PREVIEW_GATEWAY_NETWORK_POLICY_VERSION)
            || options
                .and_then(|value| value.get(BRIDGE_ENABLE_MASQUERADE_OPTION))
                .map(String::as_str)
                != Some("false")
            || options
                .and_then(|value| value.get(BRIDGE_GATEWAY_MODE_IPV4_OPTION))
                .map(String::as_str)
                != Some("isolated")
        {
            return Err(anyhow!(
                "existing preview network {name} does not match the managed internal control policy"
            ));
        }
        return Ok(());
    }
    info!(network = %name, "creating internal preview gateway control network");
    docker
        .create_network(preview_gateway_network_request(name))
        .await
        .with_context(|| format!("failed to create network {}", name))?;
    Ok(())
}

fn preview_gateway_network_request(name: &str) -> NetworkCreateRequest {
    NetworkCreateRequest {
        name: name.to_string(),
        driver: Some("bridge".to_string()),
        internal: Some(true),
        enable_ipv6: Some(false),
        labels: Some(HashMap::from([(
            PREVIEW_GATEWAY_NETWORK_LABEL.to_string(),
            PREVIEW_GATEWAY_NETWORK_POLICY_VERSION.to_string(),
        )])),
        // The relay and router must be able to communicate on this private
        // network, so ICC remains enabled. Isolated gateway mode removes the
        // host-side bridge address as well as external routing; disabled
        // masquerading is retained as an explicit defense-in-depth policy.
        options: Some(HashMap::from([
            (
                BRIDGE_ENABLE_MASQUERADE_OPTION.to_string(),
                "false".to_string(),
            ),
            (
                BRIDGE_GATEWAY_MODE_IPV4_OPTION.to_string(),
                "isolated".to_string(),
            ),
        ])),
        ..Default::default()
    }
}

async fn ensure_ingress_network(docker: &Docker) -> Result<()> {
    let networks = docker
        .list_networks(None::<ListNetworksOptions>)
        .await
        .context("failed to list Docker networks for preview ingress")?;
    if let Some(network) = networks
        .iter()
        .find(|network| network.name.as_deref() == Some(PREVIEW_GATEWAY_INGRESS_NETWORK))
    {
        let policy_matches = network
            .labels
            .as_ref()
            .and_then(|labels| labels.get(PREVIEW_GATEWAY_INGRESS_LABEL))
            .is_some_and(|value| value == PREVIEW_GATEWAY_NETWORK_POLICY_VERSION);
        let options = network.options.as_ref();
        if network.driver.as_deref() != Some("bridge")
            || network.internal != Some(false)
            || network.enable_ipv6 != Some(false)
            || !policy_matches
            || options
                .and_then(|value| value.get(BRIDGE_ENABLE_ICC_OPTION))
                .map(String::as_str)
                != Some("false")
            || options
                .and_then(|value| value.get(BRIDGE_ENABLE_MASQUERADE_OPTION))
                .map(String::as_str)
                != Some("false")
        {
            return Err(anyhow!(
                "existing preview ingress network {} does not match the managed bridge policy",
                PREVIEW_GATEWAY_INGRESS_NETWORK
            ));
        }
        return Ok(());
    }

    docker
        .create_network(preview_gateway_ingress_network_request())
        .await
        .context("failed to create preview gateway ingress network")?;
    Ok(())
}

fn preview_gateway_ingress_network_request() -> NetworkCreateRequest {
    NetworkCreateRequest {
        name: PREVIEW_GATEWAY_INGRESS_NETWORK.to_string(),
        driver: Some("bridge".to_string()),
        internal: Some(false),
        enable_ipv6: Some(false),
        labels: Some(HashMap::from([(
            PREVIEW_GATEWAY_INGRESS_LABEL.to_string(),
            PREVIEW_GATEWAY_NETWORK_POLICY_VERSION.to_string(),
        )])),
        options: Some(HashMap::from([
            (BRIDGE_ENABLE_ICC_OPTION.to_string(), "false".to_string()),
            (
                BRIDGE_ENABLE_MASQUERADE_OPTION.to_string(),
                "false".to_string(),
            ),
        ])),
        ..Default::default()
    }
}

async fn ensure_image(docker: &Docker, image: &str) -> Result<String> {
    // Locally-built dev tags and immutable local image IDs are inspected in
    // place. Docker's image-create endpoint cannot pull a bare `sha256:…` ID.
    if !should_pull_gateway_image(image) {
        debug!(image = %image, "using locally available gateway image");
    } else {
        pull_image(docker, image).await?;
    }
    verified_gateway_image_id(docker, image).await
}

fn should_pull_gateway_image(image: &str) -> bool {
    !image.ends_with(":dev") && !image.starts_with("sha256:")
}

/// Pull an arbitrary image without applying preview-gateway-specific policy.
/// Supporting images do not and should not carry the gateway protocol label.
async fn pull_image(docker: &Docker, image: &str) -> Result<()> {
    info!(image = %image, "pulling container image (if needed)");
    let mut stream = docker.create_image(
        Some(CreateImageOptions {
            from_image: Some(image.to_string()),
            ..Default::default()
        }),
        None,
        None,
    );
    while let Some(item) = stream.next().await {
        if let Err(error) = item {
            return Err(docker_operation_error(
                format!("failed to pull image {image}"),
                error,
            ));
        }
    }
    Ok(())
}

async fn verified_gateway_image_id(docker: &Docker, image: &str) -> Result<String> {
    let inspected = docker.inspect_image(image).await.map_err(|error| {
        docker_operation_error(format!("failed to inspect gateway image {image}"), error)
    })?;
    if !has_required_gateway_protocol(
        inspected
            .config
            .as_ref()
            .and_then(|config| config.labels.as_ref()),
    ) {
        return Err(anyhow!(
            "preview gateway image {image} does not declare {PREVIEW_GATEWAY_SECURITY_PROTOCOL_LABEL}={PREVIEW_GATEWAY_SECURITY_PROTOCOL_VERSION}; refusing to expose the proxy authentication token to an unverified router"
        ));
    }
    inspected
        .id
        .filter(|id| !id.is_empty())
        .ok_or_else(|| anyhow!("preview gateway image {image} has no content-addressable image ID"))
}

fn has_required_gateway_protocol(labels: Option<&HashMap<String, String>>) -> bool {
    labels
        .and_then(|labels| labels.get(PREVIEW_GATEWAY_SECURITY_PROTOCOL_LABEL))
        .is_some_and(|value| value == PREVIEW_GATEWAY_SECURITY_PROTOCOL_VERSION)
}

fn should_preserve_existing_image(auto_upgrade: bool, protocol_verified: bool) -> bool {
    !auto_upgrade && protocol_verified
}

/// Ensure a supporting image exists without refreshing a tag that is already
/// usable locally. The relay reuses the configured sandbox runtime image; a
/// registry outage must not break previews on an otherwise self-contained
/// host that already has that image.
async fn ensure_image_present(docker: &Docker, image: &str) -> Result<()> {
    match docker.inspect_image(image).await {
        Ok(_) => {
            debug!(image = %image, "supporting preview image already exists");
            Ok(())
        }
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => pull_image(docker, image).await,
        Err(error) => Err(docker_operation_error(
            format!("failed to inspect supporting preview image {image}"),
            error,
        )),
    }
}

#[derive(Debug)]
struct ExistingContainer {
    image: String,
    image_id: String,
    running: bool,
    directly_published: bool,
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
    let image_id = inspected.image.clone().unwrap_or_default();
    let running = inspected
        .state
        .as_ref()
        .and_then(|s| s.running)
        .unwrap_or(false);
    let directly_published = inspected
        .host_config
        .as_ref()
        .and_then(|config| config.port_bindings.as_ref())
        .is_some_and(|bindings| {
            bindings.values().any(|bindings| {
                bindings
                    .as_ref()
                    .is_some_and(|bindings| !bindings.is_empty())
            })
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
        image_id,
        running,
        directly_published,
        network_attached,
        shared_secret_env,
    }))
}

/// Remove any pre-split or unverified router before performing other
/// reconciliation work. This must run before network creation or image pulls:
/// otherwise a failure in those steps can leave a legacy token-forwarding
/// router reachable through its old host port.
async fn disable_unsafe_existing_gateway(docker: &Docker, name: &str) -> Result<()> {
    let existing = match inspect(docker, name).await {
        Ok(existing) => existing,
        Err(error) => {
            remove_gateway_pair(docker, name)
                .await
                .context("failed to disable a preview gateway that could not be inspected")?;
            return Err(error.context("preview gateway inspection failed; gateway disabled"));
        }
    };
    let Some(existing) = existing else {
        return Ok(());
    };

    let protocol_verified = !existing.image_id.is_empty()
        && verified_gateway_image_id(docker, &existing.image_id)
            .await
            .is_ok();
    if existing.directly_published || !protocol_verified {
        warn!(
            image = %existing.image,
            image_id = %existing.image_id,
            directly_published = existing.directly_published,
            protocol_verified,
            "disabling legacy or unverified preview gateway before reconciliation"
        );
        remove_gateway_pair(docker, name).await?;
    }
    Ok(())
}

fn container_matches(
    existing: &ExistingContainer,
    spec: &PreviewGatewaySpec,
    desired_image_id: &str,
) -> bool {
    if !running_image_matches(&existing.image_id, desired_image_id) {
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
    true
}

fn running_image_matches(running_image_id: &str, desired_image_id: &str) -> bool {
    !running_image_id.is_empty()
        && !desired_image_id.is_empty()
        && running_image_id == desired_image_id
}

fn ingress_container_name(router_name: &str) -> String {
    format!("{router_name}-ingress")
}

async fn ingress_matches(
    docker: &Docker,
    spec: &PreviewGatewaySpec,
    expected_image: &str,
) -> Result<bool> {
    let name = ingress_container_name(&spec.container_name);
    let inspected = match docker
        .inspect_container(&name, None::<InspectContainerOptions>)
        .await
    {
        Ok(inspected) => inspected,
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => return Ok(false),
        Err(error) => {
            return Err(docker_operation_error(
                "failed to inspect preview gateway ingress",
                error,
            ))
        }
    };
    let networks = inspected
        .network_settings
        .as_ref()
        .and_then(|settings| settings.networks.as_ref());
    let networks_match = networks.is_some_and(|networks| {
        networks.len() == 2
            && networks.contains_key(PREVIEW_GATEWAY_INGRESS_NETWORK)
            && networks.contains_key(&spec.network)
    });
    let expected_host_port = spec.host_port.to_string();
    let binding_matches = inspected
        .host_config
        .as_ref()
        .and_then(|config| config.port_bindings.as_ref())
        .and_then(|bindings| bindings.get(&format!("{GATEWAY_CONTAINER_PORT}/tcp")))
        .and_then(|bindings| bindings.as_ref())
        .and_then(|bindings| bindings.first())
        .is_some_and(|binding| {
            binding.host_ip.as_deref() == Some("127.0.0.1")
                && binding.host_port.as_deref() == Some(expected_host_port.as_str())
        });
    let config = inspected.config.as_ref();
    let image_matches = config.and_then(|config| config.image.as_deref()) == Some(expected_image);
    let expected_command = ["-e", PREVIEW_GATEWAY_INGRESS_SCRIPT];
    let command_matches = config
        .and_then(|config| config.cmd.as_ref())
        .is_some_and(|command| command.iter().map(String::as_str).eq(expected_command));
    let upstream_matches =
        config
            .and_then(|config| config.env.as_ref())
            .is_some_and(|environment| {
                environment.iter().any(|entry| {
                    entry == &format!("PREVIEW_GATEWAY_UPSTREAM={}", spec.container_name)
                })
            });
    let running = inspected
        .state
        .as_ref()
        .and_then(|state| state.running)
        .unwrap_or(false);

    Ok(image_matches
        && command_matches
        && upstream_matches
        && networks_match
        && binding_matches
        && running)
}

async fn remove_if_present(docker: &Docker, name: &str) -> Result<()> {
    let result = docker
        .remove_container(
            name,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;
    match result {
        Ok(())
        | Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => Ok(()),
        Err(error) => Err(docker_operation_error(
            format!("failed to remove existing container {name}"),
            error,
        )),
    }
}

async fn remove_gateway_pair(docker: &Docker, router_name: &str) -> Result<()> {
    remove_if_present(docker, &ingress_container_name(router_name)).await?;
    remove_if_present(docker, router_name).await
}

async fn create_and_start(
    docker: &Docker,
    spec: &PreviewGatewaySpec,
    ingress_image: &str,
) -> Result<()> {
    let container_port_key = format!("{}/tcp", GATEWAY_CONTAINER_PORT);
    let exposed_ports: Vec<String> = vec![container_port_key.clone()];

    let host_config = HostConfig {
        network_mode: Some(spec.network.clone()),
        cap_drop: Some(vec!["ALL".to_string()]),
        security_opt: Some(vec!["no-new-privileges:true".to_string()]),
        readonly_rootfs: Some(true),
        memory: Some(128 * 1024 * 1024),
        memory_swap: Some(128 * 1024 * 1024),
        pids_limit: Some(64),
        nano_cpus: Some(500_000_000),
        init: Some(true),
        restart_policy: Some(RestartPolicy {
            name: Some(RestartPolicyNameEnum::UNLESS_STOPPED),
            maximum_retry_count: None,
        }),
        ..Default::default()
    };

    let body = ContainerCreateBody {
        image: Some(spec.image.clone()),
        exposed_ports: Some(exposed_ports),
        user: Some("65532:65532".to_string()),
        host_config: Some(host_config),
        labels: Some(HashMap::from([(
            PREVIEW_GATEWAY_LABEL.to_string(),
            "true".to_string(),
        )])),
        env: Some({
            let mut e = vec![
                // The router is reachable only from the internal control and
                // per-sandbox networks. Host loopback reaches it through the
                // separate hardened ingress relay below.
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

    if let Err(error) = create_and_start_ingress(docker, spec, ingress_image).await {
        let _ = remove_gateway_pair(docker, &spec.container_name).await;
        return Err(error);
    }

    Ok(())
}

async fn create_and_start_ingress(
    docker: &Docker,
    spec: &PreviewGatewaySpec,
    ingress_image: &str,
) -> Result<()> {
    let name = ingress_container_name(&spec.container_name);
    let container_port_key = format!("{GATEWAY_CONTAINER_PORT}/tcp");
    let port_bindings = HashMap::from([(
        container_port_key.clone(),
        Some(vec![PortBinding {
            host_ip: Some("127.0.0.1".to_string()),
            host_port: Some(spec.host_port.to_string()),
        }]),
    )]);
    let body = ContainerCreateBody {
        image: Some(ingress_image.to_string()),
        user: Some("1000:1000".to_string()),
        entrypoint: Some(vec!["node".to_string()]),
        cmd: Some(vec![
            "-e".to_string(),
            PREVIEW_GATEWAY_INGRESS_SCRIPT.to_string(),
        ]),
        env: Some(vec![format!(
            "PREVIEW_GATEWAY_UPSTREAM={}",
            spec.container_name
        )]),
        exposed_ports: Some(vec![container_port_key]),
        labels: Some(HashMap::from([(
            PREVIEW_GATEWAY_INGRESS_LABEL.to_string(),
            PREVIEW_GATEWAY_NETWORK_POLICY_VERSION.to_string(),
        )])),
        host_config: Some(HostConfig {
            network_mode: Some(PREVIEW_GATEWAY_INGRESS_NETWORK.to_string()),
            port_bindings: Some(port_bindings),
            cap_drop: Some(vec!["ALL".to_string()]),
            security_opt: Some(vec!["no-new-privileges:true".to_string()]),
            readonly_rootfs: Some(true),
            memory: Some(64 * 1024 * 1024),
            memory_swap: Some(64 * 1024 * 1024),
            pids_limit: Some(32),
            nano_cpus: Some(250_000_000),
            init: Some(true),
            restart_policy: Some(RestartPolicy {
                name: Some(RestartPolicyNameEnum::UNLESS_STOPPED),
                maximum_retry_count: None,
            }),
            ..Default::default()
        }),
        ..Default::default()
    };

    docker
        .create_container(
            Some(CreateContainerOptionsBuilder::new().name(&name).build()),
            body,
        )
        .await
        .with_context(|| format!("failed to create preview ingress container {name}"))?;
    docker
        .connect_network(
            &spec.network,
            bollard::models::NetworkConnectRequest {
                container: name.clone(),
                endpoint_config: None,
            },
        )
        .await
        .with_context(|| format!("failed to attach preview ingress {name} to control network"))?;
    docker
        .start_container(&name, None::<StartContainerOptions>)
        .await
        .with_context(|| format!("failed to start preview ingress container {name}"))?;
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

/// Publish the gateway's host port for the in-process Pingora to dial.
///
/// The proxy used to hardcode `127.0.0.1:8090` while the reconciler published
/// the container on `settings.host_port`. So `host_port` was decorative:
/// changing it moved the container and left the proxy talking to whatever
/// still held 8090 — nothing, or on a multi-instance host, *another
/// instance's* gateway, which rejects our token with "missing or invalid
/// X-Temps-Preview-Token". Exported the same way the shared secret is, so
/// both halves of this contract cross the crate boundary by the same
/// mechanism.
pub fn export_host_port(port: u16) {
    #[allow(unused_unsafe)]
    unsafe {
        std::env::set_var("PREVIEW_GATEWAY_HOST_PORT", port.to_string());
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
/// - If `auto_upgrade = false`, leaves the *image* of an existing compatible
///   container alone. Images predating the required token-stripping protocol
///   are never preserved; reconciliation uses the configured image instead
///   and fails closed if that image is also unverified.
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
            // Honor a compatible image currently running. A legacy image may
            // forward the global gateway token into tenant code, so it can
            // never be retained merely because automatic upgrades are off.
            if let Ok(Some(existing)) = inspect(&docker, &spec.container_name).await {
                if !existing.image_id.is_empty() {
                    match verified_gateway_image_id(&docker, &existing.image_id).await {
                        Ok(image_id) => {
                            if should_preserve_existing_image(settings.auto_upgrade, true) {
                                debug!(
                                    running_image = %existing.image,
                                    running_image_id = %image_id,
                                    settings_image = %spec.image,
                                    "auto_upgrade=false — keeping compatible running image"
                                );
                                // Pin this reconciliation to the immutable image
                                // which actually backs the running container.
                                spec.image = image_id;
                            }
                        }
                        Err(error) => warn!(
                            running_image = %existing.image,
                            %error,
                            "auto_upgrade=false could not verify the running gateway image; using configured image"
                        ),
                    }
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
    /// Trusted control network the gateway starts on. It is additionally attached
    /// to each per-sandbox isolated network during reconciliation.
    pub network: Option<String>,
    /// Host port that the ingress relay's :8080 is published on.
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

    let name = container_name(settings);
    let inspected = docker
        .inspect_container(&name, None::<InspectContainerOptions>)
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
                container_name: name,
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
        Err(e) => return Err(docker_operation_error("docker inspect failed", e)),
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
    let router_last_error = inspected
        .state
        .as_ref()
        .and_then(|s| s.error.clone())
        .filter(|s| !s.is_empty());

    let network = inspected
        .network_settings
        .as_ref()
        .and_then(|ns| ns.networks.as_ref())
        .and_then(|networks| {
            networks
                .contains_key(PREVIEW_GATEWAY_NETWORK)
                .then(|| PREVIEW_GATEWAY_NETWORK.to_string())
        });

    let ingress_name = ingress_container_name(&name);
    let ingress = match docker
        .inspect_container(&ingress_name, None::<InspectContainerOptions>)
        .await
    {
        Ok(container) => Some(container),
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => None,
        Err(error) => {
            return Err(docker_operation_error(
                format!("failed to inspect preview gateway ingress {ingress_name}"),
                error,
            ))
        }
    };
    let ingress_running = ingress
        .as_ref()
        .and_then(|container| container.state.as_ref())
        .and_then(|state| state.running)
        .unwrap_or(false);
    let ingress_restarting = ingress
        .as_ref()
        .and_then(|container| container.state.as_ref())
        .and_then(|state| state.restarting)
        .unwrap_or(false);
    let ingress_restart_count = ingress
        .as_ref()
        .and_then(|container| container.restart_count);
    let ingress_exit_code = ingress
        .as_ref()
        .and_then(|container| container.state.as_ref())
        .and_then(|state| state.exit_code);
    let ingress_last_error = ingress
        .as_ref()
        .and_then(|container| container.state.as_ref())
        .and_then(|state| state.error.clone())
        .filter(|error| !error.is_empty());
    let host_port = ingress
        .as_ref()
        .and_then(|container| container.host_config.as_ref())
        .and_then(|config| config.port_bindings.as_ref())
        .and_then(|bindings| bindings.get(&format!("{GATEWAY_CONTAINER_PORT}/tcp")))
        .and_then(|bindings| bindings.as_ref())
        .and_then(|bindings| bindings.first())
        .and_then(|binding| binding.host_port.as_deref())
        .and_then(|port| port.parse::<u16>().ok());

    let drift = image
        .as_deref()
        .map(|img| img != expected_image)
        .unwrap_or(false);

    let combined_health = if restarting || ingress_restarting {
        "restarting"
    } else if !running || !ingress_running {
        let crash_looping = matches!((restart_count, last_exit_code), (Some(n), Some(code)) if n > 0 && code != 0)
            || matches!((ingress_restart_count, ingress_exit_code), (Some(n), Some(code)) if n > 0 && code != 0);
        if crash_looping {
            "crash_looping"
        } else {
            "stopped"
        }
    } else {
        "running"
    };
    let last_error = router_last_error
        .or_else(|| {
            (!running).then(|| {
                format!(
                    "preview routing container {name} is not running (last exit code {})",
                    last_exit_code.unwrap_or(-1)
                )
            })
        })
        .or(ingress_last_error)
        .or_else(|| {
            ingress.as_ref().and_then(|_| {
                (!ingress_running).then(|| {
                    format!(
                        "preview ingress container {ingress_name} is not running (last exit code {})",
                        ingress_exit_code.unwrap_or(-1)
                    )
                })
            })
        })
        .or_else(|| {
            ingress
                .is_none()
                .then(|| format!("preview gateway ingress container {ingress_name} is missing"))
        });

    Ok(GatewayStatus {
        present: true,
        running: running && ingress_running,
        health: combined_health.to_string(),
        image,
        image_digest,
        container_name: name,
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
    disable_unsafe_existing_gateway(&docker, &spec.container_name).await?;
    ensure_network(&docker, &spec.network).await?;
    ensure_ingress_network(&docker).await?;
    ensure_image(&docker, &spec.image).await?;
    let ingress_image = crate::sandbox::docker::image_name_for_runtime("node");
    ensure_image_present(&docker, &ingress_image).await?;

    remove_gateway_pair(&docker, &spec.container_name).await?;
    create_and_start(&docker, &spec, &ingress_image).await?;
    info!("preview gateway restarted");
    Ok(())
}

/// Tail the gateway container's stdout+stderr. `tail` caps the number of
/// lines returned (e.g. 200). Returns lines newest-last.
pub async fn tail_logs(docker: &Docker, container: &str, tail: usize) -> Result<Vec<String>> {
    let stream = docker.logs(
        container,
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
        .map_err(|e| docker_operation_error("failed to tail gateway logs", e))?;

    let joined = chunks.join("");
    Ok(joined.lines().map(|l| l.to_string()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn docker_operation_error_preserves_bollard_source() {
        let source = bollard::errors::Error::DockerResponseServerError {
            status_code: 500,
            message: "daemon rejected the requested port mapping".to_string(),
        };
        let error = docker_operation_error("failed to start preview gateway", source);

        assert_eq!(
            format!("{error:#}"),
            "failed to start preview gateway: Docker responded with status code 500: daemon rejected the requested port mapping"
        );
    }

    #[test]
    fn gateway_diagnostic_redacts_structured_and_registry_credentials() {
        let diagnostic = concat!(
            "registry user:password@example.test rejected ",
            r#"{"token":"json-value","client_secret":"client-value"} "#,
            "Authorization: Basic basic-value; X-Registry-Auth: registry-value; Bearer bearer-value"
        );
        let sanitized = sanitize_gateway_diagnostic(diagnostic, &[]);

        assert!(sanitized.contains("[redacted]@example.test"));
        assert!(sanitized.contains(r#""token":[redacted]"#));
        assert!(sanitized.contains(r#""client_secret":[redacted]"#));
        assert!(sanitized.contains("Authorization: [redacted]"));
        assert!(sanitized.contains("X-Registry-Auth: [redacted]"));
        assert!(sanitized.contains("Bearer [redacted]"));
        for secret in [
            "user:password",
            "json-value",
            "client-value",
            "basic-value",
            "registry-value",
            "bearer-value",
        ] {
            assert!(!sanitized.contains(secret));
        }
    }

    #[test]
    fn default_gateway_image_is_immutable() {
        assert_eq!(
            PREVIEW_GATEWAY_IMAGE,
            PreviewGatewaySettings::default().image
        );
        assert!(PREVIEW_GATEWAY_IMAGE.contains("@sha256:"));
    }

    #[test]
    fn gateway_control_network_is_internal_and_has_no_outbound_route() {
        let request = preview_gateway_network_request("preview-test");

        assert_eq!(request.driver.as_deref(), Some("bridge"));
        assert_eq!(request.internal, Some(true));
        assert_eq!(request.enable_ipv6, Some(false));
        assert_eq!(
            request
                .labels
                .as_ref()
                .and_then(|labels| labels.get(PREVIEW_GATEWAY_NETWORK_LABEL))
                .map(String::as_str),
            Some(PREVIEW_GATEWAY_NETWORK_POLICY_VERSION)
        );
        assert!(request
            .options
            .as_ref()
            .is_some_and(|options| !options.contains_key(BRIDGE_ENABLE_ICC_OPTION)));
        assert_eq!(
            request
                .options
                .as_ref()
                .and_then(|options| options.get(BRIDGE_ENABLE_MASQUERADE_OPTION))
                .map(String::as_str),
            Some("false")
        );
        assert_eq!(
            request
                .options
                .as_ref()
                .and_then(|options| options.get(BRIDGE_GATEWAY_MODE_IPV4_OPTION))
                .map(String::as_str),
            Some("isolated")
        );
    }

    #[test]
    fn gateway_image_protocol_requires_exact_security_attestation() {
        let compatible = HashMap::from([(
            PREVIEW_GATEWAY_SECURITY_PROTOCOL_LABEL.to_string(),
            PREVIEW_GATEWAY_SECURITY_PROTOCOL_VERSION.to_string(),
        )]);
        let legacy = HashMap::from([(
            PREVIEW_GATEWAY_SECURITY_PROTOCOL_LABEL.to_string(),
            "legacy".to_string(),
        )]);

        assert!(has_required_gateway_protocol(Some(&compatible)));
        assert!(!has_required_gateway_protocol(Some(&legacy)));
        assert!(!has_required_gateway_protocol(None));
    }

    #[test]
    fn existing_image_is_preserved_only_when_upgrades_are_off_and_protocol_is_verified() {
        assert!(should_preserve_existing_image(false, true));
        assert!(!should_preserve_existing_image(false, false));
        assert!(!should_preserve_existing_image(true, true));
    }

    #[test]
    fn local_gateway_images_are_inspected_without_registry_pull() {
        assert!(!should_pull_gateway_image("temps-preview-gateway:dev"));
        assert!(!should_pull_gateway_image("sha256:verified-local-image"));
        assert!(should_pull_gateway_image(
            "ghcr.io/gotempsh/temps-preview-gateway:beta"
        ));
        assert!(should_pull_gateway_image(
            "ghcr.io/gotempsh/temps-preview-gateway@sha256:manifest"
        ));
    }

    #[test]
    fn mutable_tag_does_not_hide_a_stale_running_image_id() {
        let old_running_image_id = "sha256:legacy";
        let newly_pulled_image_id = "sha256:strip-token-v1";

        assert!(!running_image_matches(
            old_running_image_id,
            newly_pulled_image_id
        ));
        assert!(running_image_matches(
            newly_pulled_image_id,
            newly_pulled_image_id
        ));
        assert!(!running_image_matches("", newly_pulled_image_id));
    }

    #[test]
    fn gateway_ingress_network_publishes_loopback_without_container_peering() {
        let request = preview_gateway_ingress_network_request();

        assert_eq!(request.name, PREVIEW_GATEWAY_INGRESS_NETWORK);
        assert_eq!(request.driver.as_deref(), Some("bridge"));
        assert_eq!(request.internal, Some(false));
        assert_eq!(request.enable_ipv6, Some(false));
        assert_eq!(
            request
                .labels
                .as_ref()
                .and_then(|labels| labels.get(PREVIEW_GATEWAY_INGRESS_LABEL))
                .map(String::as_str),
            Some(PREVIEW_GATEWAY_NETWORK_POLICY_VERSION)
        );
        for option in [BRIDGE_ENABLE_ICC_OPTION, BRIDGE_ENABLE_MASQUERADE_OPTION] {
            assert_eq!(
                request
                    .options
                    .as_ref()
                    .and_then(|options| options.get(option))
                    .map(String::as_str),
                Some("false")
            );
        }
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
