// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Route table with O(1) lookup and automatic PostgreSQL LISTEN/NOTIFY synchronization
//!
//! This module provides a cached routing table that maps hostnames to backend addresses
//! and project IDs. The cache is automatically kept in sync with the database using
//! PostgreSQL triggers and LISTEN/NOTIFY.
//!
//! ## Route Types
//!
//! Routes can be of two types:
//! - **HTTP**: Match on HTTP Host header (Layer 7) - default for most routes
//! - **TLS**: Match on TLS SNI hostname (Layer 4/5) - for TCP passthrough
//!
//! ## Wildcard Support
//!
//! Wildcard patterns like `*.example.com` are supported for both route types.
//! Matching follows DNS/Cloudflare conventions:
//! - `*.example.com` matches `api.example.com` ✓
//! - `*.example.com` does NOT match `sub.api.example.com` ✗
//! - `*.example.com` does NOT match `example.com` ✗

use crate::wildcard_matcher::WildcardMatcher;
use arc_swap::ArcSwap;
use parking_lot::RwLock;
use sea_orm::{DatabaseConnection, EntityTrait};
use sqlx::postgres::{PgListener, PgPool};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use temps_core::public_hostname_resolver::match_strategy;
use temps_core::{
    AppSettings, ExecutionEnvironment, PublicHostnameStrategy, RuntimeContext,
    ServiceEndpointScheme,
};
use temps_entities::custom_routes::RouteType;
use temps_entities::preset::ComposePublicPort;
use temps_entities::{deployments, environments, nodes, projects};
use tracing::{debug, error, info, warn};

/// Look up the address a container's node publishes its ports on (see
/// `nodes::Model::data_address`: the mesh address for a node that joined with
/// a public one), caching results. Returns None for local containers
/// (node_id is None).
async fn resolve_node_private_address(
    node_id: Option<i32>,
    nodes_cache: &mut HashMap<i32, String>,
    db: &sea_orm::DatabaseConnection,
) -> Option<String> {
    let node_id = node_id?;
    if let Some(addr) = nodes_cache.get(&node_id) {
        return Some(addr.clone());
    }
    // Fetch node from DB and cache
    if let Ok(Some(node)) = nodes::Entity::find_by_id(node_id).one(db).await {
        let addr = node.data_address().to_string();
        nodes_cache.insert(node_id, addr.clone());
        Some(addr)
    } else {
        warn!(
            node_id,
            "Node not found for container routing; remote backend will be skipped"
        );
        None
    }
}

/// Build a `BackendEntry` for a container, including its network address and metadata.
fn build_backend_entry(
    container: &temps_entities::deployment_containers::Model,
    node_private_address: Option<&str>,
    runtime_context: &RuntimeContext,
) -> Option<BackendEntry> {
    if container.node_id.is_some() && node_private_address.is_none() {
        return None;
    }
    // Host-routed and remote containers are reachable only through a live,
    // Docker-discovered published port. Falling back to container_port could
    // dial an unrelated process after the old binding was released.
    if (node_private_address.is_some()
        || runtime_context.execution_environment() == ExecutionEnvironment::Host)
        && container.host_port.is_none()
    {
        return None;
    }
    let address = build_container_backend_addr(
        &container.container_name,
        container.container_port,
        container.host_port,
        node_private_address,
        runtime_context,
    );
    Some(BackendEntry {
        address,
        container_id: Some(container.container_id.clone()),
        container_name: Some(container.container_name.clone()),
    })
}

/// Build the route backend for an explicitly published Compose mapping.
///
/// `ComposePublicPort::port` selects the stable container target. The public
/// configuration's `published` value is only a repository/UI hint: it is
/// user-controlled and must never select an arbitrary host or remote-node
/// socket. Routing uses the live Docker-discovered host mapping exclusively.
fn build_public_compose_backend_addr(
    container_name: &str,
    recorded_container_port: i32,
    recorded_host_port: Option<i32>,
    node_private_address: Option<&str>,
    public_port: &ComposePublicPort,
    runtime_context: &RuntimeContext,
) -> Option<String> {
    if recorded_container_port != i32::from(public_port.port) {
        return None;
    }
    if (node_private_address.is_some()
        || runtime_context.execution_environment() == ExecutionEnvironment::Host)
        && recorded_host_port.is_none()
    {
        return None;
    }
    Some(build_container_backend_addr(
        container_name,
        i32::from(public_port.port),
        recorded_host_port,
        node_private_address,
        runtime_context,
    ))
}

/// Build the route backend for a Traefik-label discovered container.
///
/// Returns `None` when this deployment mode cannot actually reach the
/// container, in which case the caller must skip the route rather than build an
/// address that points somewhere else.
///
/// Discovered containers are always local to this node (remote workers run
/// their own discovery against their own daemon), so there is never a node
/// private address to fall back to. What is left is exactly the distinction
/// [`build_public_compose_backend_addr`] already encodes:
///
/// * **Docker mode** — Temps runs on the Docker network and reaches the
///   container as `container_name:target_port` over the internal DNS. The
///   container port is genuinely reachable, published or not.
/// * **Baremetal mode** — Temps runs on the host and
///   [`build_container_backend_addr`] would resolve to
///   `127.0.0.1:<host_port>`. With no published host port it falls back to
///   `host_port.unwrap_or(container_port)`, i.e. `127.0.0.1:<container port>`,
///   which is a *different, unrelated service on the host* — very possibly a
///   database or the Docker API. Refuse to build an address at all.
fn build_discovered_backend_addr(
    container_name: &str,
    container_port: i32,
    host_port: Option<i32>,
    runtime_context: &RuntimeContext,
) -> Option<String> {
    if runtime_context.execution_environment() == ExecutionEnvironment::Host && host_port.is_none()
    {
        return None;
    }
    Some(build_container_backend_addr(
        container_name,
        container_port,
        host_port,
        None,
        runtime_context,
    ))
}

fn build_public_compose_backend_entry(
    container: &temps_entities::deployment_containers::Model,
    node_private_address: Option<&str>,
    public_port: &ComposePublicPort,
    runtime_context: &RuntimeContext,
) -> Option<BackendEntry> {
    if container.node_id.is_some() && node_private_address.is_none() {
        return None;
    }
    // A service may expose several public ports; each routes through the
    // binding Docker published for that specific target.
    let target = i32::from(public_port.port);
    let (recorded_container_port, recorded_host_port) = if container.exposes_port(target) {
        (target, container.host_port_for(target))
    } else {
        (container.container_port, container.host_port)
    };
    let address = build_public_compose_backend_addr(
        &container.container_name,
        recorded_container_port,
        recorded_host_port,
        node_private_address,
        public_port,
        runtime_context,
    )?;
    Some(BackendEntry {
        address,
        container_id: Some(container.container_id.clone()),
        container_name: Some(container.container_name.clone()),
    })
}

/// Select only the explicitly public Compose service for a generic project URL.
/// Non-Compose deployments continue to route across all replicas. A Compose
/// stack without a public-port selection stays private instead of accidentally
/// round-robining requests across databases, queues, and application services.
fn select_public_route_containers<'a>(
    containers: &'a [temps_entities::deployment_containers::Model],
    public_port: Option<&ComposePublicPort>,
) -> Option<Vec<&'a temps_entities::deployment_containers::Model>> {
    if !containers
        .iter()
        .any(|container| container.service_name.is_some())
    {
        return Some(containers.iter().collect());
    }

    let public_port = public_port?;
    let selected: Vec<_> = containers
        .iter()
        .filter(|container| {
            container.service_name.as_deref() == Some(public_port.service.as_str())
                && container.exposes_port(i32::from(public_port.port))
        })
        .collect();

    (!selected.is_empty()).then_some(selected)
}

/// Public per-service hostnames of a Compose project's environment, one per
/// configured public port, in `public_ports` order. Empty for non-Compose
/// projects. Sleeping on-demand environments register these alongside the
/// environment hostname so a request to any public service URL wakes them.
fn compose_public_service_hostnames(
    preset_config: Option<&temps_entities::preset::PresetConfig>,
    preview_domain: &str,
    strategy: PublicHostnameStrategy,
    environment_subdomain: &str,
) -> Vec<String> {
    let Some(temps_entities::preset::PresetConfig::DockerCompose(config)) = preset_config else {
        return Vec::new();
    };
    temps_entities::preset::compose_public_route_labels(&config.public_ports)
        .iter()
        .map(|label| strategy.service_hostname(preview_domain, environment_subdomain, label))
        .collect()
}

/// Build a backend address for a container based on deployment mode and node location
///
/// For local containers (node_private_address is None):
///   Docker mode: Returns container_name:container_port for container-to-container communication
///   Baremetal mode: Returns 127.0.0.1:host_port for host-based access
///
/// For remote containers (node_private_address is Some):
///   Always returns node_private_address:host_port (reachable via WireGuard or private network)
fn build_container_backend_addr(
    container_name: &str,
    container_port: i32,
    host_port: Option<i32>,
    node_private_address: Option<&str>,
    runtime_context: &RuntimeContext,
) -> String {
    if let Some(private_addr) = node_private_address {
        // Remote node: use the node's private/WireGuard IP with host_port.
        // `SocketAddr`'s own Display brackets IPv6 automatically
        // ("[fc00::1]:5432") -- a bare `format!("{ip}:{port}")` produces an
        // unparsable authority for any IPv6 private address, since nothing
        // marks where the address ends and the port begins.
        let port = host_port.unwrap_or(container_port);
        match private_addr.parse::<std::net::IpAddr>() {
            Ok(ip) => std::net::SocketAddr::new(ip, port as u16).to_string(),
            // nodes.private_address is validated as a bare IP at
            // registration; this only defends a pre-existing row from
            // before that validation existed.
            Err(_) => format!("{}:{}", private_addr, port),
        }
    } else {
        let endpoint = runtime_context.resolve_service_endpoint(
            container_name,
            ServiceEndpointScheme::Http,
            container_port as u16,
            host_port.unwrap_or(container_port) as u16,
        );
        endpoint.authority()
    }
}

/// Serializes tests that mutate the process-global `DEPLOYMENT_MODE` env var.
///
/// Lives at module scope (not inside `mod tests`) because `route_table_test.rs`
/// needs the same lock: its database-backed tests assert addresses that depend
/// on the deployment mode, and would race a unit test flipping it.
#[cfg(test)]
pub(crate) static DEPLOYMENT_MODE_ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Information about a sleeping on-demand environment, returned from route loading.
#[derive(Clone, Debug)]
pub struct SleepingEnvironmentEntry {
    pub domain: String,
    pub environment_id: i32,
    pub project_id: i32,
    pub deployment_id: i32,
    pub wake_timeout_seconds: i32,
}

/// On-demand config for an awake environment that should be tracked for idle timeout.
#[derive(Clone, Debug)]
pub struct OnDemandConfigEntry {
    pub environment_id: i32,
    pub idle_timeout_seconds: i32,
    pub wake_timeout_seconds: i32,
}

/// Why an application hostname currently has no live upstream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnavailableReason {
    /// The current deployment has no running container the proxy can reach:
    /// every container exited, was stopped, or lost its published port.
    NoLiveBackend,
    /// The current deployment was paused by the user.
    Paused,
}

impl UnavailableReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoLiveBackend => "no_live_backend",
            Self::Paused => "paused",
        }
    }
}

/// An application hostname whose deployment exists but cannot serve traffic
/// right now (issue #1334).
///
/// These hosts are kept out of the routable maps, so nothing ever dials a
/// dead or reassigned port, but they are remembered so the proxy can answer
/// with a 503 instead of falling back to the console. Without this, a
/// stopped app's hostname served the console SPA with HTTP 200 and uptime
/// monitors reported the app as operational.
#[derive(Clone, Debug)]
pub struct UnavailableRoute {
    pub project: Arc<projects::Model>,
    pub environment: Arc<environments::Model>,
    pub deployment: Arc<deployments::Model>,
    pub reason: UnavailableReason,
}

/// Hostnames of a deployment that has no live upstream, keyed like the route
/// maps. Wildcard hosts are stored by their base domain so a lookup never
/// allocates.
#[derive(Clone, Debug, Default)]
struct UnavailableHosts {
    exact: HashMap<String, UnavailableRoute>,
    wildcard_bases: HashMap<String, UnavailableRoute>,
}

impl UnavailableHosts {
    /// Record `host` unless an earlier pass already did. Like the live route
    /// maps, the first section of `load_routes` to claim a host owns it.
    fn record(&mut self, host: &str, route: &UnavailableRoute) {
        let (map, key) = match host.strip_prefix("*.") {
            Some(base) if !base.is_empty() => (&mut self.wildcard_bases, base),
            Some(_) => return,
            None => (&mut self.exact, host),
        };
        if !map.contains_key(key) {
            map.insert(key.to_string(), route.clone());
        }
    }

    fn get(&self, host: &str) -> Option<&UnavailableRoute> {
        self.exact.get(host).or_else(|| {
            let (label, base) = host.split_once('.')?;
            if label.is_empty() {
                return None;
            }
            self.wildcard_bases.get(base)
        })
    }

    fn len(&self) -> usize {
        self.exact.len() + self.wildcard_bases.len()
    }
}

/// Describe a deployment whose hostnames are skipped for lack of a live
/// upstream. `None` when the project is unknown, since the proxy needs the
/// full context to attribute the failed request.
fn unavailable_route_for(
    project: Option<&Arc<projects::Model>>,
    environment: &Arc<environments::Model>,
    deployment: &Arc<deployments::Model>,
) -> Option<UnavailableRoute> {
    let reason = if deployment.state == "paused" {
        UnavailableReason::Paused
    } else {
        UnavailableReason::NoLiveBackend
    };
    Some(UnavailableRoute {
        project: Arc::clone(project?),
        environment: Arc::clone(environment),
        deployment: Arc::clone(deployment),
        reason,
    })
}

/// Whether a Compose project deliberately has no public URL: without a
/// public-port selection its generic hostnames stay private, so an empty
/// backend there is the configured state, not an outage.
fn compose_without_public_ports(project: Option<&projects::Model>) -> bool {
    matches!(
        project.and_then(|project| project.preset_config.as_ref()),
        Some(temps_entities::preset::PresetConfig::DockerCompose(config))
            if config.public_ports.is_empty()
    )
}

/// A single backend entry: network address plus container metadata for tracking.
#[derive(Clone, Debug)]
pub struct BackendEntry {
    /// Network address (e.g., "127.0.0.1:8080" or "container-name:3000")
    pub address: String,
    /// Docker container ID (short hash), if available
    pub container_id: Option<String>,
    /// Human-readable container name (e.g., "my-app-abc123")
    pub container_name: Option<String>,
}

/// Result of selecting a backend via round-robin.
#[derive(Clone, Debug)]
pub struct BackendSelection {
    /// Network address to connect to
    pub address: String,
    /// Docker container ID that will handle the request
    pub container_id: Option<String>,
    /// Human-readable container name
    pub container_name: Option<String>,
}

/// Backend type for a route
#[derive(Clone, Debug)]
pub enum BackendType {
    /// Proxy to backend addresses (containers)
    Upstream {
        /// Backend entries for load balancing
        backends: Vec<BackendEntry>,
        /// Round-robin counter for load balancing
        round_robin_counter: Arc<AtomicUsize>,
    },
    /// Serve static files from a directory
    StaticDir {
        /// Path to the static files directory
        path: String,
    },
}

impl BackendType {
    /// Get the next backend using round-robin load balancing.
    /// Returns None for StaticDir backends.
    pub fn get_backend(&self) -> Option<BackendSelection> {
        match self {
            BackendType::Upstream {
                backends,
                round_robin_counter,
            } => {
                if backends.is_empty() {
                    return Some(BackendSelection {
                        address: "127.0.0.1:8080".to_string(),
                        container_id: None,
                        container_name: None,
                    });
                }

                let entry = if backends.len() == 1 {
                    &backends[0]
                } else {
                    let index =
                        round_robin_counter.fetch_add(1, Ordering::Relaxed) % backends.len();
                    &backends[index]
                };

                Some(BackendSelection {
                    address: entry.address.clone(),
                    container_id: entry.container_id.clone(),
                    container_name: entry.container_name.clone(),
                })
            }
            BackendType::StaticDir { .. } => None,
        }
    }

    /// Get the next backend address string using round-robin.
    /// Convenience wrapper for callers that only need the address.
    pub fn get_backend_addr(&self) -> Option<String> {
        self.get_backend().map(|s| s.address)
    }

    /// Check if this is a static directory backend
    pub fn is_static(&self) -> bool {
        matches!(self, BackendType::StaticDir { .. })
    }

    /// Get the static directory path if this is a StaticDir backend
    pub fn static_dir(&self) -> Option<&str> {
        match self {
            BackendType::StaticDir { path } => Some(path),
            _ => None,
        }
    }
}

/// Route information for a single host with cached models
#[derive(Clone, Debug)]
pub struct RouteInfo {
    /// Backend type (upstream addresses or static directory)
    pub backend: BackendType,
    /// Optional redirect URL for project custom domains
    pub redirect_to: Option<String>,
    /// Optional status code for redirects
    pub status_code: Option<i32>,
    /// Cached project model (None for custom_routes without project)
    pub project: Option<Arc<projects::Model>>,
    /// Cached environment model (None for custom_routes)
    pub environment: Option<Arc<environments::Model>>,
    /// Cached deployment model (None for custom_routes)
    pub deployment: Option<Arc<deployments::Model>>,
    /// Whether this hostname is eligible for on-demand TLS issuance (ADR-018 §2,
    /// third gate check). `true` for STABLE, low-cardinality hostnames — the
    /// per-environment alias and console host — whose cert lives for the life of
    /// the environment. `false` for EPHEMERAL, high-cardinality per-deployment
    /// hostnames (the deployment-slug fallback) and for operator-configured
    /// custom domains / internal-only names, which must NOT trigger on-demand
    /// issuance (issuing a cert for `myapp-prod-42` is wasted against the shared
    /// sslip.io community bucket and trips the LE per-hostname limits on a
    /// redeploy loop). The proxy's on-demand cert gate reads this O(1) so the
    /// TLS callback never needs a DB lookup to exclude ephemeral hostnames.
    pub cert_eligible: bool,
}

impl RouteInfo {
    /// Select the next backend (address + container metadata) using round-robin.
    /// Returns a fallback selection if this is a static directory backend.
    pub fn select_backend(&self) -> BackendSelection {
        self.backend
            .get_backend()
            .unwrap_or_else(|| BackendSelection {
                address: "127.0.0.1:8080".to_string(),
                container_id: None,
                container_name: None,
            })
    }

    /// Get the next backend address using round-robin load balancing.
    /// Convenience wrapper — use `select_backend()` when you also need container info.
    pub fn get_backend_addr(&self) -> String {
        self.select_backend().address
    }

    /// Check if this route serves static files
    pub fn is_static(&self) -> bool {
        self.backend.is_static()
    }

    /// Get the static directory path if this is a static deployment
    pub fn static_dir(&self) -> Option<&str> {
        self.backend.static_dir()
    }
}

/// In-memory routing table with O(1) lookup
///
/// Routes are organized into four categories:
/// - `http_routes`: Exact hostname matches for HTTP Host header routing
/// - `tls_routes`: Exact hostname matches for TLS SNI routing
/// - `http_wildcards`: Wildcard patterns for HTTP Host header routing
/// - `tls_wildcards`: Wildcard patterns for TLS SNI routing
///
/// Callback invoked after each route table reload with sleeping environments and on-demand configs.
pub type OnSleepingCallback =
    Arc<dyn Fn(Vec<SleepingEnvironmentEntry>, Vec<OnDemandConfigEntry>) + Send + Sync>;

/// Async callback fired after every successful `load_routes()`. Used by
/// the deployment-DNS publisher to reconcile internal `*.temps.local`
/// records in lockstep with the L7 route table — same trigger, single
/// source of truth for "what is the current deployment of each env".
///
/// Returns a boxed future so the implementation can await DB work
/// (`temps-routes` doesn't depend on `temps-dns`; the binary wires the
/// concrete publisher in here).
pub type OnReloadCallback =
    Arc<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync>;

/// Async callback fired after every successful `load_routes()` with the list of
/// hostnames that have `cert_eligible = true` in the new route table.
///
/// Used by the on-demand TLS manager (ADR-018) to eagerly pre-provision
/// certificates the moment a deployment goes live, so the cert is ready before
/// the user's first HTTPS request rather than provisioning on the first handshake
/// (which produces a visible `ERR_TLS_HANDSHAKE` for the first visitor).
///
/// The callback receives only cert-eligible hostnames (stable per-environment
/// aliases, not ephemeral per-deployment slugs). The on-demand manager's own gate
/// checks (dedup, backoff, rate-limit, zone) still apply inside the callback, so
/// calling it on every reload is idempotent.
pub type OnCertEligibleCallback = Arc<
    dyn Fn(Vec<String>) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

#[derive(Clone, Debug, Default)]
struct LegacyRouteTable {
    exact: HashMap<String, RouteInfo>,
    wildcards: WildcardMatcher,
    reserved_console_host: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct RouteTableSnapshot {
    http_routes: HashMap<String, RouteInfo>,
    tls_routes: HashMap<String, RouteInfo>,
    http_wildcards: WildcardMatcher,
    tls_wildcards: WildcardMatcher,
    legacy: LegacyRouteTable,
    ownership: RouteOwnershipSnapshot,
    /// Application hosts with no live upstream. Deliberately not part of
    /// `ownership`, so on-demand wake gating is unchanged.
    unavailable: UnavailableHosts,
}

impl RouteTableSnapshot {
    /// Whether any routable map resolves `host`, in the same order as
    /// `resolve_route_for_sni`, without cloning a `RouteInfo`.
    fn has_live_route(&self, host: &str) -> bool {
        self.tls_routes.contains_key(host)
            || self.tls_wildcards.match_domain(host).is_some()
            || self.http_routes.contains_key(host)
            || self.legacy.exact.contains_key(host)
            || self.http_wildcards.match_domain(host).is_some()
            || self.legacy.wildcards.match_domain(host).is_some()
    }
}

impl LegacyRouteTable {
    fn get(&self, host: &str) -> Option<&RouteInfo> {
        if self.reserved_console_host.as_deref() == Some(host) {
            return None;
        }
        self.exact
            .get(host)
            .or_else(|| self.wildcards.match_domain(host))
    }
}

/// Minimal, immutable hostname ownership index for wake-on-request gating.
/// It deliberately stores no `RouteInfo`, so hot-path existence checks neither
/// take route-table locks nor clone backend/project models.
#[derive(Clone, Debug, Default)]
struct RouteOwnershipSnapshot {
    exact: HashSet<String>,
    wildcard_bases: HashSet<String>,
    reserved_hosts: HashSet<String>,
}

impl RouteOwnershipSnapshot {
    fn owns(&self, host: &str) -> bool {
        if self.reserved_hosts.contains(host) || self.exact.contains(host) {
            return true;
        }
        let Some((label, base)) = host.split_once('.') else {
            return false;
        };
        !label.is_empty() && !base.is_empty() && self.wildcard_bases.contains(base)
    }

    fn include(&mut self, other: &Self) {
        self.exact.extend(other.exact.iter().cloned());
        self.wildcard_bases
            .extend(other.wildcard_bases.iter().cloned());
        self.reserved_hosts
            .extend(other.reserved_hosts.iter().cloned());
    }
}

fn build_route_ownership_snapshot<'a>(
    legacy_hosts: impl IntoIterator<Item = &'a String>,
    http_hosts: impl IntoIterator<Item = &'a String>,
    tls_hosts: impl IntoIterator<Item = &'a String>,
    reserved_console_host: Option<String>,
) -> RouteOwnershipSnapshot {
    let mut snapshot = RouteOwnershipSnapshot::default();
    if let Some(host) = reserved_console_host {
        snapshot.reserved_hosts.insert(host);
    }
    for host in legacy_hosts.into_iter().chain(http_hosts).chain(tls_hosts) {
        if let Some(base) = host.strip_prefix("*.") {
            if !base.is_empty() {
                snapshot.wildcard_bases.insert(base.to_string());
            }
        } else {
            snapshot.exact.insert(host.clone());
        }
    }
    snapshot
}

/// Whether a process numbers route generations from the durable
/// `route_generation` row or only for itself.
///
/// The row is what `mark_deployment_complete` waits for every worker to ACK,
/// and workers ACK the generation the route-sync snapshot endpoint gave them.
/// So the row must have exactly one writer: the process serving that endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RouteGenerationRole {
    /// Serves `/internal/nodes/{id}/routes/snapshot` (`temps serve`): every
    /// reload claims its generation from `route_generation`.
    #[default]
    Authoritative,
    /// Loads routes without answering workers (split-mode `temps proxy`):
    /// generations are local to the process and the row is never written.
    Local,
}

/// Why a reload could not claim its generation from `route_generation`.
#[derive(Debug, thiserror::Error)]
pub enum RouteGenerationError {
    #[error(
        "Failed to claim the next route generation after in-memory generation {previous} \
         from route_generation (id = 1): {source}"
    )]
    Claim {
        previous: u64,
        #[source]
        source: sea_orm::DbErr,
    },

    #[error(
        "route_generation singleton row (id = 1) is missing, so the route generation after \
         {previous} cannot be persisted and the worker completion gate cannot advance"
    )]
    MissingRow { previous: u64 },

    #[error("route_generation (id = 1) returned negative generation {value}")]
    Negative { value: i64 },

    #[error(
        "Claiming the route generation after in-memory generation {previous} from \
         route_generation (id = 1) did not finish within {timeout_ms} ms"
    )]
    Timeout { previous: u64, timeout_ms: u128 },
}

/// How long a reload may wait to claim its generation before numbering it
/// locally. Every reload waits for the claim before waking route waiters
/// (readiness checks, worker long-polls), so a stalled database must not hold
/// them asleep for longer than this.
pub(crate) const ROUTE_GENERATION_CLAIM_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(2);

/// Extra time the client allows past the server-side statement timeout, for
/// a connection that stalls outside a statement (acquire, `BEGIN`, `COMMIT`).
const ROUTE_GENERATION_CLAIM_GRACE: std::time::Duration = std::time::Duration::from_millis(500);

/// Atomically claim the generation after `previous` from the durable counter,
/// bounded by [`ROUTE_GENERATION_CLAIM_TIMEOUT`].
pub(crate) async fn claim_route_generation(
    db: &DatabaseConnection,
    previous: u64,
) -> Result<u64, RouteGenerationError> {
    claim_route_generation_within(db, previous, ROUTE_GENERATION_CLAIM_TIMEOUT).await
}

/// Atomically claim the generation after `previous`, giving up after
/// `timeout`.
///
/// `GREATEST(current, previous, <highest stored node ACK>) + 1` in a single
/// `UPDATE ... RETURNING`: the row lock serializes concurrent claims, so each
/// caller gets a distinct value, and the result is above the stored value,
/// the in-memory value, and every generation a node has acknowledged. The
/// last term is what stops a restarted process from reissuing a number an
/// earlier process published without persisting it (a lost commit, or a
/// locally numbered reload): a worker that already holds that number would
/// treat the reissued snapshot as one it has, and keep stale routes. It is
/// one aggregate over one row per node.
///
/// Two phases share one deadline (`timeout` plus a short grace for a
/// connection that stalls outside a statement):
///
/// 1. `BEGIN`, `SET LOCAL statement_timeout`, the `UPDATE`. Giving up here is
///    always safe: Postgres cancels the statement, and a transaction dropped
///    before `COMMIT` is sent rolls back, so nothing is claimed.
/// 2. `COMMIT`. Once the `UPDATE` has returned a value, the claim is used
///    whatever the commit outcome. If the commit succeeded (even with a
///    late or lost reply) the row now holds that value and the reload must
///    publish it, or workers could never reach the gate's target. If it did
///    not, publishing it anyway is still safe: the row stays below what
///    workers are given, and the next claim starts above both.
pub(crate) async fn claim_route_generation_within(
    db: &DatabaseConnection,
    previous: u64,
    timeout: std::time::Duration,
) -> Result<u64, RouteGenerationError> {
    let timeout_ms = timeout.as_millis();
    let deadline = tokio::time::Instant::now() + timeout + ROUTE_GENERATION_CLAIM_GRACE;
    let (txn, claimed) = match tokio::time::timeout_at(
        deadline,
        claim_uncommitted(db, previous, timeout_ms),
    )
    .await
    {
        Ok(result) => result?,
        Err(_) => {
            return Err(RouteGenerationError::Timeout {
                previous,
                timeout_ms,
            })
        }
    };
    match tokio::time::timeout_at(deadline, txn.commit()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => warn!(
            previous_generation = previous,
            claimed_generation = claimed,
            %error,
            "Committing route generation {claimed} failed; publishing it anyway so the \
             durable generation can never be ahead of what workers are given"
        ),
        Err(_) => warn!(
            previous_generation = previous,
            claimed_generation = claimed,
            timeout_ms,
            "Committing route generation {claimed} did not answer within {timeout_ms} ms; \
             publishing it anyway, since the commit may have succeeded"
        ),
    }
    Ok(claimed)
}

/// Phase 1 of [`claim_route_generation_within`]: the claimed value and the
/// still-open transaction that holds it.
async fn claim_uncommitted(
    db: &DatabaseConnection,
    previous: u64,
    timeout_ms: u128,
) -> Result<(sea_orm::DatabaseTransaction, u64), RouteGenerationError> {
    use sea_orm::{ConnectionTrait, TransactionTrait};
    let claim_error = |source| RouteGenerationError::Claim { previous, source };
    // Leave room for the `+ 1` below; 9.2e18 reloads is unreachable anyway.
    let previous_i64 = i64::try_from(previous)
        .unwrap_or(i64::MAX - 1)
        .min(i64::MAX - 1);
    let txn = db.begin().await.map_err(claim_error)?;
    // An integer literal, not user input; `SET` cannot take a bind parameter.
    txn.execute(sea_orm::Statement::from_string(
        sea_orm::DatabaseBackend::Postgres,
        format!("SET LOCAL statement_timeout = {}", timeout_ms.max(1)),
    ))
    .await
    .map_err(claim_error)?;
    let row = txn
        .query_one(sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "UPDATE route_generation SET current = GREATEST(current, $1, \
             (SELECT COALESCE(MAX(applied_generation), 0) FROM node_route_state)) + 1, \
             updated_at = now() WHERE id = 1 RETURNING current",
            [previous_i64.into()],
        ))
        .await
        .map_err(claim_error)?
        .ok_or(RouteGenerationError::MissingRow { previous })?;
    let value = row.try_get::<i64>("", "current").map_err(claim_error)?;
    let claimed = u64::try_from(value).map_err(|_| RouteGenerationError::Negative { value })?;
    Ok((txn, claimed))
}

/// Raise `counter` to a generation claimed from the database and return the
/// generation this reload publishes.
///
/// Always strictly above the value the counter held, so every reload wakes
/// the long-poll waiters even if `claimed` were somehow not ahead of it, and
/// never below `claimed`, so the in-memory sequence matches the durable one
/// whenever the claim is ahead (the normal case).
pub(crate) fn adopt_claimed_generation(
    counter: &std::sync::atomic::AtomicU64,
    claimed: u64,
) -> u64 {
    use std::sync::atomic::Ordering;
    let mut current = counter.load(Ordering::Acquire);
    loop {
        let next = claimed.max(current.saturating_add(1));
        match counter.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return next,
            Err(actual) => current = actual,
        }
    }
}

pub struct CachedPeerTable {
    /// All route indexes and wake-gate ownership published as one immutable,
    /// lock-free snapshot. A reload can never expose mismatched generations.
    route_snapshot: ArcSwap<RouteTableSnapshot>,

    /// Serializes reloads so their three-phase snapshot publication cannot
    /// interleave, even when manual refresh and database notification race.
    route_reload_lock: tokio::sync::Mutex<()>,

    /// Database connection for loading routes
    db: Arc<DatabaseConnection>,

    /// Immutable execution environment and endpoint resolver selected at startup.
    runtime_context: Arc<RuntimeContext>,

    /// Optional callback invoked after each route reload with sleeping environment entries.
    on_sleeping_callback: parking_lot::Mutex<Option<OnSleepingCallback>>,

    /// Optional async callback invoked after each successful reload.
    /// Used to publish per-deployment internal DNS records (ADR-012-lite)
    /// in lockstep with the route table.
    on_reload_callback: parking_lot::Mutex<Option<OnReloadCallback>>,

    /// Optional async callback invoked after each successful reload with
    /// the list of cert-eligible hostnames. Used for eager TLS pre-provisioning
    /// (ADR-018): the proxy wires the on-demand cert manager here so new
    /// deployment routes get a cert issued before the first HTTPS request.
    on_cert_eligible_callback: parking_lot::Mutex<Option<OnCertEligibleCallback>>,

    /// Monotonically increasing version of the in-memory `routes` map.
    /// Bumped at the end of every successful `load_routes()`. Workers
    /// long-poll `GET /internal/.../routes/snapshot?since=N` and the
    /// handler waits until this counter exceeds `N` (or a timeout)
    /// before returning the current snapshot.
    ///
    /// In the [`RouteGenerationRole::Authoritative`] process each value is
    /// claimed from the durable `route_generation` row (see
    /// [`Self::advance_generation`]), so the numbering agents ACK and the
    /// completion gate compares is one sequence, monotonic across restarts.
    generation: std::sync::atomic::AtomicU64,

    /// Whether this process owns the durable `route_generation` row
    /// ([`RouteGenerationRole::Authoritative`], the default) or only numbers
    /// its own reloads ([`RouteGenerationRole::Local`]). Read once per reload,
    /// never on the request path.
    generation_authoritative: std::sync::atomic::AtomicBool,

    /// Notify hookup so long-poll handlers can sleep until the next
    /// generation bump rather than spinning. Awoken on every
    /// `load_routes()` success.
    generation_changed: Arc<tokio::sync::Notify>,

    /// Docker network this process adopts Traefik-labelled containers from, or
    /// `None` when label discovery is not enabled here.
    ///
    /// Section 6 of [`Self::load_routes`] loads **nothing** while this is
    /// `None`, and only rows for this exact network otherwise. That is what
    /// makes turning discovery off (or repointing it at another network)
    /// actually take effect: `traefik_discovered_routes` rows outlive the
    /// configuration that created them, and a disabled reconciler will never
    /// come back to delete them.
    ///
    /// Deliberately injected by the process bootstrap (`temps serve` /
    /// `temps proxy`) rather than read from the environment here — this crate
    /// does not parse configuration.
    traefik_discovery_network: RwLock<Option<String>>,
}

impl CachedPeerTable {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self::new_with_runtime_context(db, Arc::new(RuntimeContext::host()))
    }

    pub fn new_with_runtime_context(
        db: Arc<DatabaseConnection>,
        runtime_context: Arc<RuntimeContext>,
    ) -> Self {
        Self {
            route_snapshot: ArcSwap::from_pointee(RouteTableSnapshot::default()),
            route_reload_lock: tokio::sync::Mutex::new(()),
            db,
            runtime_context,
            on_sleeping_callback: parking_lot::Mutex::new(None),
            on_reload_callback: parking_lot::Mutex::new(None),
            on_cert_eligible_callback: parking_lot::Mutex::new(None),
            generation: std::sync::atomic::AtomicU64::new(0),
            generation_authoritative: std::sync::atomic::AtomicBool::new(true),
            generation_changed: Arc::new(tokio::sync::Notify::new()),
            // Off until the bootstrap says otherwise: label discovery is
            // opt-in, so the safe default is "adopt nothing".
            traefik_discovery_network: RwLock::new(None),
        }
    }

    /// Point Section 6 of `load_routes()` at the Docker network this process
    /// adopts Traefik-labelled containers from.
    ///
    /// `Some(network)` when label discovery is enabled here, `None` (the
    /// default) to load no discovered routes at all. Call it before the initial
    /// route load; the next `load_routes()` picks it up.
    ///
    /// The split-mode `temps proxy` process must set this too even though it
    /// never runs the reconciler: it is a *reader* of `traefik_discovered_routes`
    /// and would otherwise serve none of them.
    pub fn set_traefik_discovery_network(&self, network: Option<String>) {
        let network = network
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty());
        match &network {
            Some(n) => info!(
                "Traefik label discovery is enabled for this process: routes discovered on Docker \
                 network '{}' will be served",
                n
            ),
            None => debug!(
                "Traefik label discovery is not enabled for this process: no discovered routes \
                 will be served"
            ),
        }
        *self.traefik_discovery_network.write() = network;
    }

    /// Choose whether this process owns the durable `route_generation` row.
    ///
    /// Exactly one process per control plane numbers the generations workers
    /// ACK: the one serving `GET /internal/nodes/{id}/routes/snapshot`
    /// (`temps serve`). The split-mode `temps proxy` loads the same routes
    /// from the same database but never answers workers, so it uses
    /// [`RouteGenerationRole::Local`]. When both wrote the row, whichever
    /// process had reloaded more often could set the completion gate's target
    /// to a generation the snapshot endpoint had never issued, and a worker
    /// deployment then timed out waiting for ACKs that could not arrive.
    ///
    /// Call it before the first `load_routes()`.
    pub fn set_route_generation_role(&self, role: RouteGenerationRole) {
        let authoritative = role == RouteGenerationRole::Authoritative;
        self.generation_authoritative
            .store(authoritative, std::sync::atomic::Ordering::Release);
        if authoritative {
            debug!("Route generations are claimed from the durable route_generation row");
        } else {
            info!(
                "Route generations are numbered locally in this process; route_generation is \
                 owned by the process serving the worker route-sync endpoint"
            );
        }
    }

    /// The role set by [`Self::set_route_generation_role`].
    pub fn route_generation_role(&self) -> RouteGenerationRole {
        if self
            .generation_authoritative
            .load(std::sync::atomic::Ordering::Acquire)
        {
            RouteGenerationRole::Authoritative
        } else {
            RouteGenerationRole::Local
        }
    }

    /// Number the reload that just finished and return its generation.
    ///
    /// Authoritative: one atomic statement claims the next value of the
    /// durable counter, `GREATEST(current, <in-memory>) + 1`. A single-row
    /// update cannot interleave with another writer's, so two claims never
    /// return the same value, and taking the greater of the two counters means
    /// neither the durable nor the in-memory sequence goes backwards: a
    /// restored database below this process's numbering, or a process that
    /// restarted below the database's, both continue from the higher one.
    ///
    /// When the claim fails (the database is briefly unreachable, or stalls
    /// past `ROUTE_GENERATION_CLAIM_TIMEOUT`) the reload still has to wake
    /// waiters, so the in-memory counter advances on its own
    /// and the row is left alone; the next successful claim starts above it.
    /// An agent that ACKs such a generation has the newest routes, and the
    /// completion gate's target (the row) is at or below its ACK.
    ///
    /// Local: the in-memory counter only; the row is never touched.
    ///
    /// Runs once per reload (control plane, serialized by
    /// `route_reload_lock`), never per request.
    async fn advance_generation(&self) -> u64 {
        use std::sync::atomic::Ordering;
        if !self.generation_authoritative.load(Ordering::Acquire) {
            return self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        }
        let previous = self.generation.load(Ordering::Acquire);
        match claim_route_generation(self.db.as_ref(), previous).await {
            Ok(claimed) => adopt_claimed_generation(&self.generation, claimed),
            Err(error) => {
                warn!(
                    previous_generation = previous,
                    "{error}; numbering this reload locally until a later reload can claim \
                     from the database"
                );
                self.generation.fetch_add(1, Ordering::AcqRel) + 1
            }
        }
    }

    /// Current in-memory route table generation. Bumped on every
    /// successful `load_routes()`. Workers poll this via the sync
    /// endpoint to know when to refetch.
    pub fn current_generation(&self) -> u64 {
        self.generation.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Subscribe to generation-bump notifications. Each waiter is
    /// woken on the next successful reload.
    pub fn generation_notifier(&self) -> Arc<tokio::sync::Notify> {
        self.generation_changed.clone()
    }

    /// Whether the route table has completed at least one successful load.
    ///
    /// `generation` only ever increments at the very end of a successful
    /// `load_routes()`, so `generation == 0` reliably means "never loaded".
    /// Used by the proxy to decide whether to wait for the first load before
    /// falling back to the console for an unmatched host (the proxy now binds
    /// its listeners before the initial route load completes).
    pub fn has_loaded(&self) -> bool {
        self.current_generation() > 0
    }

    /// Wait until the route table has loaded at least once, up to `timeout`.
    ///
    /// Returns `true` if the table is (or became) loaded, `false` on timeout.
    /// Mirrors `OnDemandManager::wait_for_route_reload`: build the `Notified`
    /// future *before* re-checking `has_loaded()` so a generation bump that
    /// races this call can't be missed (lost-wakeup safe).
    pub async fn wait_until_loaded(&self, timeout: std::time::Duration) -> bool {
        if self.has_loaded() {
            return true;
        }
        let notified = self.generation_changed.notified();
        // Re-check after arming the notification to close the race window.
        if self.has_loaded() {
            return true;
        }
        match tokio::time::timeout(timeout, notified).await {
            Ok(()) => self.has_loaded(),
            Err(_) => self.has_loaded(),
        }
    }

    /// Set a callback that fires after each `load_routes()` with the sleeping environment entries.
    pub fn set_on_sleeping_callback(&self, callback: OnSleepingCallback) {
        *self.on_sleeping_callback.lock() = Some(callback);
    }

    /// Set an async callback fired after every successful `load_routes()`.
    /// Used by `temps-dns::DeploymentDnsPublisher` to reconcile internal
    /// FQDN records in lockstep with the route table.
    pub fn set_on_reload_callback(&self, callback: OnReloadCallback) {
        *self.on_reload_callback.lock() = Some(callback);
    }

    /// Set an async callback fired after every successful `load_routes()` with
    /// the list of cert-eligible hostnames in the new route table. Used by the
    /// proxy to eagerly pre-provision TLS certificates (ADR-018) so new
    /// deployments get a cert before the first HTTPS request arrives.
    pub fn set_on_cert_eligible_callback(&self, callback: OnCertEligibleCallback) {
        *self.on_cert_eligible_callback.lock() = Some(callback);
    }

    /// Return all currently-loaded hostnames with `cert_eligible = true`.
    ///
    /// Used for an immediate one-time provisioning pass after the cert manager
    /// is wired up (covers domains already in the table from the initial load).
    pub fn cert_eligible_hosts(&self) -> Vec<String> {
        self.route_snapshot
            .load()
            .legacy
            .exact
            .iter()
            .filter(|(host, r)| r.cert_eligible && !host.starts_with("*."))
            .map(|(host, _)| host.clone())
            .collect()
    }

    /// Get route by HTTP Host header
    ///
    /// Used for route_type = 'http' routes.
    /// Checks exact matches first, then wildcard patterns.
    pub fn get_route_by_host(&self, host: &str) -> Option<RouteInfo> {
        let snapshot = self.route_snapshot.load();
        if snapshot.legacy.reserved_console_host.as_deref() == Some(host) {
            return None;
        }

        // 1. Try exact match in HTTP routes
        if let Some(route) = snapshot.http_routes.get(host) {
            return Some(route.clone());
        }

        // 2. Exact project/environment domains take precedence over every
        // wildcard route, including operator-configured custom wildcards.
        if let Some(route) = snapshot.legacy.exact.get(host) {
            return Some(route.clone());
        }

        // 3. Try wildcard match in HTTP wildcards
        if let Some(route) = snapshot.http_wildcards.match_domain(host) {
            return Some(route.clone());
        }

        // 4. Fall back to project/environment wildcard routes.
        snapshot.legacy.wildcards.match_domain(host).cloned()
    }

    /// Get route by TLS SNI hostname
    ///
    /// Used for route_type = 'tls' routes.
    /// Checks exact matches first, then wildcard patterns.
    pub fn get_route_by_sni(&self, sni: &str) -> Option<RouteInfo> {
        let snapshot = self.route_snapshot.load();
        if snapshot.legacy.reserved_console_host.as_deref() == Some(sni) {
            return None;
        }

        // 1. Try exact match in TLS routes
        if let Some(route) = snapshot.tls_routes.get(sni) {
            return Some(route.clone());
        }

        // 2. Try wildcard match in TLS wildcards
        if let Some(route) = snapshot.tls_wildcards.match_domain(sni) {
            return Some(route.clone());
        }

        None
    }

    /// Resolve a hostname across every lookup strategy in proxy order: TLS
    /// routes first, then HTTP and legacy routes. Stable per-environment
    /// hostnames (env_domains, the env subdomain, the env preview alias) live in
    /// the legacy map, so the on-demand TLS gate (ADR-018 §2 second/third check)
    /// MUST consult all three — checking only `get_route_by_sni` would miss them
    /// and reject every certable host. O(1) per map, no I/O.
    pub fn resolve_route_for_sni(&self, sni: &str) -> Option<RouteInfo> {
        let snapshot = self.route_snapshot.load();
        if snapshot.legacy.reserved_console_host.as_deref() == Some(sni) {
            return None;
        }
        snapshot
            .tls_routes
            .get(sni)
            .or_else(|| snapshot.tls_wildcards.match_domain(sni))
            .or_else(|| snapshot.http_routes.get(sni))
            .or_else(|| snapshot.legacy.exact.get(sni))
            .or_else(|| snapshot.http_wildcards.match_domain(sni))
            .or_else(|| snapshot.legacy.wildcards.match_domain(sni))
            .cloned()
    }

    /// Insert a route directly into the legacy routes map. Test/seed support for
    /// callers in other crates (e.g. the proxy's on-demand cert gate tests) that
    /// need a populated route table without standing up a database. Not used on
    /// the production load path — `load_routes` owns that.
    #[doc(hidden)]
    pub fn insert_route_for_test(&self, host: &str, route: RouteInfo) {
        let mut snapshot = (*self.route_snapshot.load_full()).clone();
        if host.starts_with("*.") {
            let mut wildcard_route = route.clone();
            wildcard_route.cert_eligible = false;
            snapshot.legacy.wildcards.insert(host, wildcard_route);
        }
        snapshot.legacy.exact.insert(host.to_string(), route);
        snapshot.ownership = build_route_ownership_snapshot(
            snapshot.legacy.exact.keys(),
            snapshot.http_routes.keys(),
            snapshot.tls_routes.keys(),
            snapshot.legacy.reserved_console_host.clone(),
        );
        self.route_snapshot.store(Arc::new(snapshot));
    }

    /// Insert a TLS route for cross-crate tests without database setup.
    #[doc(hidden)]
    pub fn insert_tls_route_for_test(&self, host: &str, route: RouteInfo) {
        let mut snapshot = (*self.route_snapshot.load_full()).clone();
        snapshot.tls_routes.insert(host.to_string(), route);
        snapshot.ownership.exact.insert(host.to_string());
        self.route_snapshot.store(Arc::new(snapshot));
    }

    /// Reserve the console hostname for cross-crate tests without database setup.
    #[doc(hidden)]
    pub fn reserve_hostname_for_test(&self, host: &str) {
        let mut snapshot = (*self.route_snapshot.load_full()).clone();
        snapshot.legacy.reserved_console_host = Some(host.to_string());
        snapshot.ownership.reserved_hosts.insert(host.to_string());
        self.route_snapshot.store(Arc::new(snapshot));
    }

    /// Load all routes from the database into the cache with full models.
    /// This queries environment_domains, custom_routes, and project_custom_domains.
    /// Returns a list of sleeping on-demand environments that were skipped during route loading.
    pub async fn load_routes(&self) -> Result<Vec<SleepingEnvironmentEntry>, sea_orm::DbErr> {
        let _reload_guard = self.route_reload_lock.lock().await;
        use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter};
        use temps_entities::{
            custom_routes, deployments, environment_domains, environments, project_custom_domains,
            settings,
        };

        let mut routes = HashMap::new();
        let mut sleeping_environments: Vec<SleepingEnvironmentEntry> = Vec::new();
        // App hostnames skipped below because their deployment has no live
        // upstream. Served as a 503 by the proxy instead of the console.
        let mut unavailable = UnavailableHosts::default();

        // Build entity caches as we go - only cache what we actually need for routing
        let mut projects_cache: HashMap<i32, Arc<projects::Model>> = HashMap::new();
        let mut environments_cache: HashMap<i32, Arc<environments::Model>> = HashMap::new();
        let mut deployments_cache: HashMap<i32, Arc<deployments::Model>> = HashMap::new();
        // Node cache: maps node_id -> private_address for multi-node routing
        let mut nodes_cache: HashMap<i32, String> = HashMap::new();

        // Fetch preview-domain and hostname settings once per rebuild.
        let app_settings = settings::Entity::find()
            .one(self.db.as_ref())
            .await?
            .map(|s| AppSettings::from_json(s.data))
            .unwrap_or_default();
        let preview_domain = app_settings.preview_domain.clone();

        // Build a base-domain -> strategy map from managed domains once per
        // rebuild. Only the per-service hostname layout varies by strategy; env
        // and deployment hosts are strategy-independent.
        let hostname_strategies: std::collections::HashMap<String, PublicHostnameStrategy> =
            temps_entities::dns_managed_domains::Entity::find()
                .all(self.db.as_ref())
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|d| {
                    (
                        d.domain.to_ascii_lowercase(),
                        PublicHostnameStrategy::from_db_str(&d.generated_hostname_mode),
                    )
                })
                .collect();

        debug!(
            "Loaded public hostname settings: preview_domain={}, managed_domain_modes={}",
            preview_domain,
            hostname_strategies.len()
        );

        debug!("Loading route table from database...");

        // 1. Load environment_domains (e.g., preview-123.temps.dev)
        let env_domains = environment_domains::Entity::find()
            .all(self.db.as_ref())
            .await?;

        debug!(
            "Section 1: Loading {} environment domains",
            env_domains.len()
        );

        for env_domain in env_domains {
            // Fetch environment if not cached (skip soft-deleted)
            if !environments_cache.contains_key(&env_domain.environment_id) {
                if let Ok(Some(env)) = environments::Entity::find_by_id(env_domain.environment_id)
                    .filter(environments::Column::DeletedAt.is_null())
                    .one(self.db.as_ref())
                    .await
                {
                    environments_cache.insert(env.id, Arc::new(env));
                }
            }

            if let Some(environment) = environments_cache.get(&env_domain.environment_id) {
                if let Some(deployment_id) = environment.current_deployment_id {
                    // Skip sleeping on-demand environments — record them separately
                    if environment.sleeping {
                        let wake_timeout = environment
                            .deployment_config
                            .as_ref()
                            .map(|c| c.wake_timeout_seconds)
                            .unwrap_or(30);
                        sleeping_environments.push(SleepingEnvironmentEntry {
                            domain: env_domain.domain.clone(),
                            environment_id: environment.id,
                            project_id: environment.project_id,
                            deployment_id,
                            wake_timeout_seconds: wake_timeout,
                        });
                        debug!(
                            "Skipping sleeping environment domain: {} (env={}, deploy={})",
                            env_domain.domain, environment.id, deployment_id
                        );
                        continue;
                    }

                    // Fetch deployment if not cached
                    if !deployments_cache.contains_key(&deployment_id) {
                        if let Ok(Some(dep)) = deployments::Entity::find_by_id(deployment_id)
                            .one(self.db.as_ref())
                            .await
                        {
                            deployments_cache.insert(dep.id, Arc::new(dep));
                        }
                    }

                    if let Some(deployment) = deployments_cache.get(&deployment_id) {
                        // Load all active containers for this deployment
                        use temps_entities::deployment_containers;
                        let containers = if deployment.state == "paused" {
                            // `pause_deployment` flips this row to "paused" BEFORE
                            // touching any container, and per-container `status`
                            // writes happen afterward with best-effort retry — if
                            // one of those writes fails even after retrying, the
                            // container row can be left stuck at "running" even
                            // though Docker actually stopped it. Trusting
                            // `deployment.state` here (rather than only the
                            // per-container status) closes that gap: a paused
                            // deployment is never routable, independent of
                            // whether every container's own status row caught up.
                            Vec::new()
                        } else {
                            deployment_containers::Entity::find()
                                .filter(
                                    deployment_containers::Column::DeploymentId.eq(deployment_id),
                                )
                                .filter(deployment_containers::Column::DeletedAt.is_null())
                                // A container row survives (deleted_at stays NULL) for the
                                // deployment's whole lifecycle, but `status` still moves
                                // through "running" -> "stopped"/"removing"/"removed" (e.g.
                                // deployment pause, or a manual per-container stop) without
                                // ever being soft-deleted. Only route live traffic to
                                // containers that are actually up, or where a container has
                                // never had a status recorded yet.
                                .filter(
                                    Condition::any()
                                        .add(deployment_containers::Column::Status.is_null())
                                        .add(deployment_containers::Column::Status.eq("running")),
                                )
                                .all(self.db.as_ref())
                                .await
                                .unwrap_or_default()
                        };

                        // Fetch project if not cached
                        if !projects_cache.contains_key(&environment.project_id) {
                            if let Ok(Some(proj)) =
                                projects::Entity::find_by_id(environment.project_id)
                                    .one(self.db.as_ref())
                                    .await
                            {
                                projects_cache.insert(proj.id, Arc::new(proj));
                            }
                        }

                        let project = projects_cache.get(&environment.project_id);

                        // Determine backend type: static directory or upstream containers
                        let backend = if let Some(static_dir) = &deployment.static_dir_location {
                            // Static deployment - serve from directory
                            Some(BackendType::StaticDir {
                                path: static_dir.clone(),
                            })
                        } else if !containers.is_empty() {
                            let public_port = project
                                .and_then(|project| project.preset_config.as_ref())
                                .and_then(|config| match config {
                                    temps_entities::preset::PresetConfig::DockerCompose(config) => {
                                        config.public_ports.first()
                                    }
                                    _ => None,
                                });
                            match select_public_route_containers(&containers, public_port) {
                                // A Compose stack without a public port is private.
                                None if public_port.is_none() => continue,
                                // The public service has no running container.
                                None => None,
                                Some(route_containers) => {
                                    let mut backend_entries =
                                        Vec::with_capacity(route_containers.len());
                                    for c in route_containers {
                                        let node_addr = resolve_node_private_address(
                                            c.node_id,
                                            &mut nodes_cache,
                                            self.db.as_ref(),
                                        )
                                        .await;
                                        let entry = match public_port {
                                            Some(port) => build_public_compose_backend_entry(
                                                c,
                                                node_addr.as_deref(),
                                                port,
                                                self.runtime_context.as_ref(),
                                            ),
                                            None => build_backend_entry(
                                                c,
                                                node_addr.as_deref(),
                                                self.runtime_context.as_ref(),
                                            ),
                                        };
                                        if let Some(entry) = entry {
                                            backend_entries.push(entry);
                                        }
                                    }
                                    (!backend_entries.is_empty()).then(|| BackendType::Upstream {
                                        backends: backend_entries,
                                        round_robin_counter: Arc::new(AtomicUsize::new(0)),
                                    })
                                }
                            }
                        } else {
                            None
                        };

                        // No live backend (containers stopped, crashed or
                        // unreachable, or the deployment is paused): keep the
                        // host out of the routable maps, but remember it so
                        // the proxy answers 503 instead of the console.
                        let Some(backend) = backend else {
                            if !compose_without_public_ports(project.map(Arc::as_ref)) {
                                if let Some(route) =
                                    unavailable_route_for(project, environment, deployment)
                                {
                                    debug!(
                                        "Environment domain has no live backend: {} (project={}, env={}, deploy={}, reason={})",
                                        env_domain.domain, environment.project_id, environment.id, deployment_id, route.reason.as_str()
                                    );
                                    unavailable.record(&env_domain.domain, &route);
                                }
                            }
                            continue;
                        };

                        routes.insert(
                            env_domain.domain.clone(),
                            RouteInfo {
                                backend: backend.clone(),
                                redirect_to: None,
                                status_code: None,
                                project: project.cloned(),
                                environment: Some(Arc::clone(environment)),
                                deployment: Some(Arc::clone(deployment)),
                                // STABLE per-environment alias — certable (ADR-018 §2).
                                cert_eligible: true,
                            },
                        );

                        match &backend {
                            BackendType::Upstream { backends, .. } => {
                                let addresses: Vec<&str> =
                                    backends.iter().map(|b| b.address.as_str()).collect();
                                debug!(
                                    "Loaded environment domain route: {} -> {:?} ({} containers, project={}, env={}, deploy={})",
                                    env_domain.domain, addresses, addresses.len(), environment.project_id, environment.id, deployment_id
                                );
                            }
                            BackendType::StaticDir { path } => {
                                debug!(
                                    "Loaded environment domain route (static): {} -> {} (project={}, env={}, deploy={})",
                                    env_domain.domain, path, environment.project_id, environment.id, deployment_id
                                );
                            }
                        }
                    }
                }
            }
        }

        // 2. Load custom_routes (custom domain mappings with host:port)
        // These are separated into HTTP and TLS routes based on route_type
        let custom_routes_data = custom_routes::Entity::find()
            .filter(custom_routes::Column::Enabled.eq(true))
            .all(self.db.as_ref())
            .await?;

        debug!(
            "Section 2: Loading {} custom routes",
            custom_routes_data.len()
        );

        // Prepare route caches for custom_routes
        let mut http_routes_map: HashMap<String, RouteInfo> = HashMap::new();
        let mut tls_routes_map: HashMap<String, RouteInfo> = HashMap::new();
        let mut http_wildcards_matcher = WildcardMatcher::new();
        let mut tls_wildcards_matcher = WildcardMatcher::new();

        for custom_route in custom_routes_data {
            let backend_addr = format!("{}:{}", custom_route.host, custom_route.port);
            let route_info = RouteInfo {
                backend: BackendType::Upstream {
                    backends: vec![BackendEntry {
                        address: backend_addr.clone(),
                        container_id: None,
                        container_name: None,
                    }],
                    round_robin_counter: Arc::new(AtomicUsize::new(0)),
                },
                redirect_to: None,
                status_code: None,
                project: None, // Custom routes don't have project context
                environment: None,
                deployment: None,
                // Operator-configured custom route mapping, not an on-demand zone
                // host — never trigger on-demand issuance (ADR-018 §2).
                cert_eligible: false,
            };

            let is_wildcard = custom_route.domain.starts_with("*.");
            let route_type_str = match custom_route.route_type {
                RouteType::Http => "http",
                RouteType::Tls => "tls",
            };

            match custom_route.route_type {
                RouteType::Http => {
                    if is_wildcard {
                        http_wildcards_matcher.insert(&custom_route.domain, route_info.clone());
                        debug!(
                            "Loaded HTTP wildcard custom route: {} -> {} (type={})",
                            custom_route.domain, backend_addr, route_type_str
                        );
                    } else {
                        http_routes_map.insert(custom_route.domain.clone(), route_info.clone());
                        debug!(
                            "Loaded HTTP custom route: {} -> {} (type={})",
                            custom_route.domain, backend_addr, route_type_str
                        );
                    }
                }
                RouteType::Tls => {
                    if is_wildcard {
                        tls_wildcards_matcher.insert(&custom_route.domain, route_info.clone());
                        debug!(
                            "Loaded TLS wildcard custom route: {} -> {} (type={})",
                            custom_route.domain, backend_addr, route_type_str
                        );
                    } else {
                        tls_routes_map.insert(custom_route.domain.clone(), route_info.clone());
                        debug!(
                            "Loaded TLS custom route: {} -> {} (type={})",
                            custom_route.domain, backend_addr, route_type_str
                        );
                    }
                }
            }

            // Also add to legacy routes map for backward compatibility
            routes.insert(custom_route.domain.clone(), route_info);
        }

        // 3. Load project_custom_domains (custom domains with redirects or environment mapping)
        // Note: We load ALL custom domains regardless of status to allow immediate routing
        let custom_domains = project_custom_domains::Entity::find()
            .all(self.db.as_ref())
            .await?;

        debug!(
            "Section 3: Loading {} project custom domains",
            custom_domains.len()
        );

        for custom_domain in custom_domains {
            // Fetch environment if not cached (skip soft-deleted)
            if !environments_cache.contains_key(&custom_domain.environment_id) {
                if let Ok(Some(env)) =
                    environments::Entity::find_by_id(custom_domain.environment_id)
                        .filter(environments::Column::DeletedAt.is_null())
                        .one(self.db.as_ref())
                        .await
                {
                    environments_cache.insert(env.id, Arc::new(env));
                }
            }

            if let Some(environment) = environments_cache.get(&custom_domain.environment_id) {
                if let Some(deployment_id) = environment.current_deployment_id {
                    // Skip sleeping on-demand environments — record them separately
                    if environment.sleeping {
                        let wake_timeout = environment
                            .deployment_config
                            .as_ref()
                            .map(|c| c.wake_timeout_seconds)
                            .unwrap_or(30);
                        sleeping_environments.push(SleepingEnvironmentEntry {
                            domain: custom_domain.domain.clone(),
                            environment_id: environment.id,
                            project_id: environment.project_id,
                            deployment_id,
                            wake_timeout_seconds: wake_timeout,
                        });
                        debug!(
                            "Skipping sleeping environment custom domain: {} (env={}, deploy={})",
                            custom_domain.domain, environment.id, deployment_id
                        );
                        continue;
                    }

                    // Fetch deployment if not cached
                    if !deployments_cache.contains_key(&deployment_id) {
                        if let Ok(Some(dep)) = deployments::Entity::find_by_id(deployment_id)
                            .one(self.db.as_ref())
                            .await
                        {
                            deployments_cache.insert(dep.id, Arc::new(dep));
                        }
                    }

                    if let Some(deployment) = deployments_cache.get(&deployment_id) {
                        // Load all active containers for this deployment
                        use temps_entities::deployment_containers;
                        let containers = if deployment.state == "paused" {
                            // See the matching comment in the primary-domain branch
                            // above: trust `deployment.state` over per-container
                            // `status`, which can lag behind on a retry-exhausted
                            // write.
                            Vec::new()
                        } else {
                            deployment_containers::Entity::find()
                                .filter(
                                    deployment_containers::Column::DeploymentId.eq(deployment_id),
                                )
                                .filter(deployment_containers::Column::DeletedAt.is_null())
                                // A container row survives (deleted_at stays NULL) for the
                                // deployment's whole lifecycle, but `status` still moves
                                // through "running" -> "stopped"/"removing"/"removed" (e.g.
                                // deployment pause, or a manual per-container stop) without
                                // ever being soft-deleted. Only route live traffic to
                                // containers that are actually up, or where a container has
                                // never had a status recorded yet.
                                .filter(
                                    Condition::any()
                                        .add(deployment_containers::Column::Status.is_null())
                                        .add(deployment_containers::Column::Status.eq("running")),
                                )
                                .all(self.db.as_ref())
                                .await
                                .unwrap_or_default()
                        };

                        // Fetch project if not cached
                        if !projects_cache.contains_key(&custom_domain.project_id) {
                            if let Ok(Some(proj)) =
                                projects::Entity::find_by_id(custom_domain.project_id)
                                    .one(self.db.as_ref())
                                    .await
                            {
                                projects_cache.insert(proj.id, Arc::new(proj));
                            }
                        }

                        let project = projects_cache.get(&custom_domain.project_id);

                        // Filter containers by service_name if specified (docker-compose service targeting)
                        let target_containers: Vec<_> =
                            if let Some(ref sn) = custom_domain.service_name {
                                containers
                                    .iter()
                                    .filter(|c| c.service_name.as_deref() == Some(sn.as_str()))
                                    .collect()
                            } else {
                                containers.iter().collect()
                            };

                        // Determine backend type: static directory or upstream containers
                        let backend = if let Some(static_dir) = &deployment.static_dir_location {
                            // Static deployment - serve from directory
                            Some(BackendType::StaticDir {
                                path: static_dir.clone(),
                            })
                        } else if !target_containers.is_empty() {
                            // Container deployment - proxy to containers
                            let mut backend_entries = Vec::with_capacity(target_containers.len());
                            for c in &target_containers {
                                let node_addr = resolve_node_private_address(
                                    c.node_id,
                                    &mut nodes_cache,
                                    self.db.as_ref(),
                                )
                                .await;
                                if let Some(entry) = build_backend_entry(
                                    c,
                                    node_addr.as_deref(),
                                    self.runtime_context.as_ref(),
                                ) {
                                    backend_entries.push(entry);
                                }
                            }
                            (!backend_entries.is_empty()).then(|| BackendType::Upstream {
                                backends: backend_entries,
                                round_robin_counter: Arc::new(AtomicUsize::new(0)),
                            })
                        } else {
                            None
                        };

                        // An operator-attached domain with no live backend is
                        // an outage, never a console URL (see section 1).
                        let Some(backend) = backend else {
                            if let Some(route) =
                                unavailable_route_for(project, environment, deployment)
                            {
                                debug!(
                                    "Custom domain has no live backend: {} (project={}, env={}, deploy={}, reason={})",
                                    custom_domain.domain, custom_domain.project_id, environment.id, deployment_id, route.reason.as_str()
                                );
                                unavailable.record(&custom_domain.domain, &route);
                            }
                            continue;
                        };

                        routes.insert(
                            custom_domain.domain.clone(),
                            RouteInfo {
                                backend: backend.clone(),
                                redirect_to: custom_domain.redirect_to.clone(),
                                status_code: custom_domain.status_code,
                                project: project.cloned(),
                                environment: Some(Arc::clone(environment)),
                                deployment: Some(Arc::clone(deployment)),
                                // Operator-configured custom domain — out of the
                                // on-demand sslip.io zone; not on-demand certable
                                // (ADR-018 §2). Custom-domain TLS is provisioned
                                // through the normal manual/DNS-01 path.
                                cert_eligible: false,
                            },
                        );

                        if let Some(ref redirect) = custom_domain.redirect_to {
                            debug!(
                                "Loaded custom domain with redirect: {} -> {} (status: {:?})",
                                custom_domain.domain, redirect, custom_domain.status_code
                            );
                        } else {
                            match &backend {
                                BackendType::Upstream { backends, .. } => {
                                    let addresses: Vec<&str> =
                                        backends.iter().map(|b| b.address.as_str()).collect();
                                    debug!(
                                        "Loaded custom domain route: {} -> {:?} ({} containers, project={}, env={}, deploy={})",
                                        custom_domain.domain, addresses, addresses.len(), custom_domain.project_id, environment.id, deployment_id
                                    );
                                }
                                BackendType::StaticDir { path } => {
                                    debug!(
                                        "Loaded custom domain route (static): {} -> {} (project={}, env={}, deploy={})",
                                        custom_domain.domain, path, custom_domain.project_id, environment.id, deployment_id
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        // 4. Load all environments with main_url (for preview domain routing)
        // This handles environments that don't have explicit environment_domains entries
        // Only fetch environments that have main_url and current_deployment_id
        let all_envs = environments::Entity::find()
            .filter(environments::Column::Subdomain.is_not_null())
            .filter(environments::Column::CurrentDeploymentId.is_not_null())
            .filter(environments::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;

        debug!(
            "Section 4: Loading {} environments with main_url",
            all_envs.len()
        );

        for env in all_envs {
            if let Some(deployment_id) = env.current_deployment_id {
                let main_url = &env.subdomain;

                // Skip sleeping on-demand environments — record them separately
                if env.sleeping {
                    let wake_timeout = env
                        .deployment_config
                        .as_ref()
                        .map(|c| c.wake_timeout_seconds)
                        .unwrap_or(30);
                    // Record both the raw main_url and the full preview domain
                    sleeping_environments.push(SleepingEnvironmentEntry {
                        domain: main_url.clone(),
                        environment_id: env.id,
                        project_id: env.project_id,
                        deployment_id,
                        wake_timeout_seconds: wake_timeout,
                    });
                    let full_domain = PublicHostnameStrategy::Standard
                        .environment_hostname(&preview_domain, main_url);
                    sleeping_environments.push(SleepingEnvironmentEntry {
                        domain: full_domain,
                        environment_id: env.id,
                        project_id: env.project_id,
                        deployment_id,
                        wake_timeout_seconds: wake_timeout,
                    });
                    // Public Compose service URLs (`<service>--<env>`, and
                    // `<service>-<port>--<env>` for extra ports) must wake the
                    // environment too, not only its main hostname.
                    if !projects_cache.contains_key(&env.project_id) {
                        if let Ok(Some(proj)) = projects::Entity::find_by_id(env.project_id)
                            .one(self.db.as_ref())
                            .await
                        {
                            projects_cache.insert(proj.id, Arc::new(proj));
                        }
                    }
                    if let Some(project) = projects_cache.get(&env.project_id) {
                        let strategy = match_strategy(&hostname_strategies, &preview_domain);
                        for domain in compose_public_service_hostnames(
                            project.preset_config.as_ref(),
                            &preview_domain,
                            strategy,
                            main_url,
                        ) {
                            sleeping_environments.push(SleepingEnvironmentEntry {
                                domain,
                                environment_id: env.id,
                                project_id: env.project_id,
                                deployment_id,
                                wake_timeout_seconds: wake_timeout,
                            });
                        }
                    }
                    debug!(
                        "Skipping sleeping environment: {} (env={}, deploy={})",
                        main_url, env.id, deployment_id
                    );
                    continue;
                }

                // Cache environment if not already cached
                environments_cache
                    .entry(env.id)
                    .or_insert_with(|| Arc::new(env.clone()));

                // Fetch deployment if not cached.
                // Accept any state — if current_deployment_id points here, it should be routable.
                // The previous "completed" filter caused a race: mark_deployment_complete sets
                // current_deployment_id (fires PG NOTIFY) BEFORE setting state="completed",
                // so the route table reload would skip the deployment and never confirm.
                if !deployments_cache.contains_key(&deployment_id) {
                    if let Ok(Some(dep)) = deployments::Entity::find_by_id(deployment_id)
                        .one(self.db.as_ref())
                        .await
                    {
                        deployments_cache.insert(dep.id, Arc::new(dep));
                    }
                }

                if let Some(deployment) = deployments_cache.get(&deployment_id) {
                    // Load all active containers for this deployment
                    use temps_entities::deployment_containers;
                    // "paused" is deliberately checked here rather than folded into
                    // the "accept any state" comment above it: that comment is about
                    // NOT filtering on state (e.g. "completed") to avoid a race with
                    // `mark_deployment_complete`'s write ordering. Excluding "paused"
                    // is unrelated to that race — it's a deliberate, terminal,
                    // user-initiated state, not a transient one a deploy passes
                    // through — and (per the matching comment two branches up) it's
                    // more trustworthy than per-container `status`, which can lag
                    // behind on a retry-exhausted write.
                    let containers = if deployment.state == "paused" {
                        Vec::new()
                    } else {
                        deployment_containers::Entity::find()
                            .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
                            .filter(deployment_containers::Column::DeletedAt.is_null())
                            // See the matching comment above: `status` (not just
                            // `deleted_at`) governs routability, so a paused/stopped
                            // deployment's containers don't keep serving live traffic.
                            .filter(
                                Condition::any()
                                    .add(deployment_containers::Column::Status.is_null())
                                    .add(deployment_containers::Column::Status.eq("running")),
                            )
                            .all(self.db.as_ref())
                            .await
                            .unwrap_or_default()
                    };

                    // Fetch project if not cached
                    if !projects_cache.contains_key(&env.project_id) {
                        if let Ok(Some(proj)) = projects::Entity::find_by_id(env.project_id)
                            .one(self.db.as_ref())
                            .await
                        {
                            projects_cache.insert(proj.id, Arc::new(proj));
                        }
                    }

                    let project = projects_cache.get(&env.project_id);
                    let environment = environments_cache.get(&env.id);

                    // Determine backend type: static directory or upstream containers
                    let backend = if let Some(static_dir) = &deployment.static_dir_location {
                        // Static deployment - serve from directory
                        Some(BackendType::StaticDir {
                            path: static_dir.clone(),
                        })
                    } else if !containers.is_empty() {
                        // For Compose deployments, the main route uses only
                        // the first explicitly configured public port. A stack
                        // with no public ports remains private instead of
                        // exposing whichever container happened to be
                        // discovered first. When the public service has no
                        // running container (or a stale service reference),
                        // nothing is routed and the hosts are reported
                        // unavailable below.
                        let is_compose = containers.iter().any(|c| c.service_name.is_some());
                        let (route_containers, public_port): (
                            Vec<&deployment_containers::Model>,
                            Option<ComposePublicPort>,
                        ) = if is_compose {
                            // Check for public_ports config
                            let first_public = project
                                .and_then(|p| p.preset_config.as_ref())
                                .and_then(|pc| {
                                    if let temps_entities::preset::PresetConfig::DockerCompose(
                                        cfg,
                                    ) = pc
                                    {
                                        cfg.public_ports.first().cloned()
                                    } else {
                                        None
                                    }
                                });

                            match first_public {
                                Some(pp) => {
                                    let cs: Vec<_> = containers
                                        .iter()
                                        .filter(|c| c.service_name.as_deref() == Some(&pp.service))
                                        .collect();
                                    (cs, Some(pp))
                                }
                                None => continue,
                            }
                        } else {
                            (containers.iter().collect(), None)
                        };

                        let mut backend_entries = Vec::with_capacity(route_containers.len());
                        for c in &route_containers {
                            let node_addr = resolve_node_private_address(
                                c.node_id,
                                &mut nodes_cache,
                                self.db.as_ref(),
                            )
                            .await;
                            let entry = match public_port.as_ref() {
                                Some(port) => build_public_compose_backend_entry(
                                    c,
                                    node_addr.as_deref(),
                                    port,
                                    self.runtime_context.as_ref(),
                                ),
                                None => build_backend_entry(
                                    c,
                                    node_addr.as_deref(),
                                    self.runtime_context.as_ref(),
                                ),
                            };
                            if let Some(entry) = entry {
                                backend_entries.push(entry);
                            }
                        }
                        (!backend_entries.is_empty()).then(|| BackendType::Upstream {
                            backends: backend_entries,
                            round_robin_counter: Arc::new(AtomicUsize::new(0)),
                        })
                    } else {
                        None
                    };

                    // No live backend: report every hostname this section
                    // would have routed for the environment as unavailable
                    // (see section 1), including the public Compose service
                    // URLs and the internal `*.temps.local` name.
                    let Some(backend) = backend else {
                        let route = environment
                            .filter(|_| !compose_without_public_ports(project.map(Arc::as_ref)))
                            .and_then(|environment| {
                                unavailable_route_for(project, environment, deployment)
                            });
                        if let Some(route) = route {
                            let mut hosts = vec![
                                main_url.clone(),
                                PublicHostnameStrategy::Standard
                                    .environment_hostname(&preview_domain, main_url),
                            ];
                            let env_slug = env.slug.trim();
                            let proj_slug = route.project.slug.trim();
                            if !env_slug.is_empty() && !proj_slug.is_empty() {
                                hosts.push(format!("{}.{}.temps.local", env_slug, proj_slug));
                            }
                            hosts.extend(compose_public_service_hostnames(
                                route.project.preset_config.as_ref(),
                                &preview_domain,
                                match_strategy(&hostname_strategies, &preview_domain),
                                main_url,
                            ));
                            debug!(
                                "Environment has no live backend: {:?} (project={}, env={}, deploy={}, reason={})",
                                hosts, env.project_id, env.id, deployment_id, route.reason.as_str()
                            );
                            for host in &hosts {
                                unavailable.record(host, &route);
                            }
                        }
                        continue;
                    };

                    // Add route with main_url as-is
                    if !routes.contains_key(main_url) {
                        routes.insert(
                            main_url.clone(),
                            RouteInfo {
                                backend: backend.clone(),
                                redirect_to: None,
                                status_code: None,
                                project: project.cloned(),
                                environment: environment.cloned(),
                                deployment: Some(Arc::clone(deployment)),
                                // STABLE per-environment hostname — certable (ADR-018 §2).
                                cert_eligible: true,
                            },
                        );
                        match &backend {
                            BackendType::Upstream { backends, .. } => {
                                let addresses: Vec<&str> =
                                    backends.iter().map(|b| b.address.as_str()).collect();
                                debug!(
                                    "Loaded environment route: {} -> {:?} ({} containers, project={}, env={}, deploy={})",
                                    main_url, addresses, addresses.len(), env.project_id, env.id, deployment_id
                                );
                            }
                            BackendType::StaticDir { path } => {
                                debug!(
                                    "Loaded environment route (static): {} -> {} (project={}, env={}, deploy={})",
                                    main_url, path, env.project_id, env.id, deployment_id
                                );
                            }
                        }
                    }

                    // Also add route with preview_domain suffix if configured
                    let full_domain = PublicHostnameStrategy::Standard
                        .environment_hostname(&preview_domain, main_url);
                    if !routes.contains_key(&full_domain) {
                        routes.insert(
                            full_domain.clone(),
                            RouteInfo {
                                backend: backend.clone(),
                                redirect_to: None,
                                status_code: None,
                                project: project.cloned(),
                                environment: environment.cloned(),
                                deployment: Some(Arc::clone(deployment)),
                                // STABLE per-environment preview hostname —
                                // certable (ADR-018 §2). This is the env alias,
                                // one per environment, not a per-deployment name.
                                cert_eligible: true,
                            },
                        );
                        match &backend {
                            BackendType::Upstream { backends, .. } => {
                                let addresses: Vec<&str> =
                                    backends.iter().map(|b| b.address.as_str()).collect();
                                debug!(
                                    "Loaded environment route with preview domain: {} -> {:?} ({} containers, project={}, env={}, deploy={})",
                                    full_domain, addresses, addresses.len(), env.project_id, env.id, deployment_id
                                );
                            }
                            BackendType::StaticDir { path } => {
                                debug!(
                                    "Loaded environment route with preview domain (static): {} -> {} (project={}, env={}, deploy={})",
                                    full_domain, path, env.project_id, env.id, deployment_id
                                );
                            }
                        }
                    }

                    // ADR-012-lite: stable per-deployment FQDN under the
                    // internal `*.temps.local` zone. Format
                    // `<env-slug>.<project-slug>.temps.local` resolves to
                    // the edge proxy and fans out to whatever containers
                    // are currently `running`+`ready_at IS NOT NULL` for
                    // this deployment, so client redeploys are invisible
                    // even when the client caches DNS aggressively.
                    //
                    // Skip when either slug is missing or empty; without
                    // both we can't construct a non-ambiguous label, and
                    // emitting `..temps.local` would clobber the parent
                    // zone. Slug uniqueness is enforced at create time
                    // for environments and projects so collisions inside
                    // a project are impossible.
                    if let Some(proj) = project {
                        let env_slug = env.slug.trim();
                        let proj_slug = proj.slug.trim();
                        if !env_slug.is_empty() && !proj_slug.is_empty() {
                            let internal_fqdn = format!("{}.{}.temps.local", env_slug, proj_slug);
                            if !routes.contains_key(&internal_fqdn) {
                                routes.insert(
                                    internal_fqdn.clone(),
                                    RouteInfo {
                                        backend: backend.clone(),
                                        redirect_to: None,
                                        status_code: None,
                                        project: project.cloned(),
                                        environment: environment.cloned(),
                                        deployment: Some(Arc::clone(deployment)),
                                        // Internal-only `*.temps.local` name — not
                                        // publicly reachable by Let's Encrypt, so
                                        // never on-demand certable (ADR-018 §2).
                                        cert_eligible: false,
                                    },
                                );
                                debug!(
                                    "Loaded internal temps.local route: {} (project={}, env={}, deploy={})",
                                    internal_fqdn, env.project_id, env.id, deployment_id
                                );
                            }
                        }
                    }

                    // Docker Compose: create per-service routes ONLY for explicitly public ports.
                    // All ports are private by default — users must mark ports as public
                    // in the project's preset_config.public_ports.
                    let has_compose_services = containers.iter().any(|c| c.service_name.is_some());
                    if has_compose_services {
                        // Read public_ports from project's preset_config
                        let public_ports: Vec<ComposePublicPort> = project
                            .and_then(|p| p.preset_config.as_ref())
                            .and_then(|pc| {
                                if let temps_entities::preset::PresetConfig::DockerCompose(cfg) = pc
                                {
                                    Some(cfg.public_ports.clone())
                                } else {
                                    None
                                }
                            })
                            .unwrap_or_default();

                        if !public_ports.is_empty() {
                            // Group containers by service_name
                            let mut services: HashMap<String, Vec<&deployment_containers::Model>> =
                                HashMap::new();
                            for c in &containers {
                                if let Some(ref svc) = c.service_name {
                                    services.entry(svc.clone()).or_default().push(c);
                                }
                            }

                            let route_labels =
                                temps_entities::preset::compose_public_route_labels(&public_ports);
                            // A public service with no running container (or
                            // no reachable port) is down while the rest of
                            // the stack runs: its URL answers 503.
                            let svc_unavailable = environment.and_then(|environment| {
                                unavailable_route_for(project, environment, deployment)
                            });
                            let svc_strategy =
                                match_strategy(&hostname_strategies, &preview_domain);
                            for (public_port, route_label) in public_ports.iter().zip(&route_labels)
                            {
                                let svc_domain = svc_strategy.service_hostname(
                                    &preview_domain,
                                    main_url,
                                    route_label,
                                );
                                let svc_containers = match services.get(&public_port.service) {
                                    Some(c) => c,
                                    None => {
                                        if let Some(route) = &svc_unavailable {
                                            unavailable.record(&svc_domain, route);
                                        }
                                        continue;
                                    }
                                };

                                let mut svc_backends = Vec::with_capacity(svc_containers.len());
                                for c in svc_containers {
                                    let node_addr = resolve_node_private_address(
                                        c.node_id,
                                        &mut nodes_cache,
                                        self.db.as_ref(),
                                    )
                                    .await;
                                    let Some(entry) = build_public_compose_backend_entry(
                                        c,
                                        node_addr.as_deref(),
                                        public_port,
                                        self.runtime_context.as_ref(),
                                    ) else {
                                        warn!(
                                            service = %public_port.service,
                                            configured_target = public_port.port,
                                            recorded_target = c.container_port,
                                            recorded_host_port = ?c.host_port,
                                            "Skipping public Compose route without a matching live Docker port mapping"
                                        );
                                        continue;
                                    };
                                    svc_backends.push(entry);
                                }

                                if svc_backends.is_empty() {
                                    if let Some(route) = &svc_unavailable {
                                        unavailable.record(&svc_domain, route);
                                    }
                                    continue;
                                }

                                let svc_backend = BackendType::Upstream {
                                    backends: svc_backends.clone(),
                                    round_robin_counter: Arc::new(AtomicUsize::new(0)),
                                };

                                let svc_route_info = RouteInfo {
                                    backend: svc_backend,
                                    redirect_to: None,
                                    status_code: None,
                                    project: project.cloned(),
                                    environment: environment.cloned(),
                                    deployment: Some(Arc::clone(deployment)),
                                    // STABLE per-service env hostname
                                    // (`<service>--<env>.<preview>`, or
                                    // `<service>-<port>--<env>` for a service's
                                    // additional public ports) — one per
                                    // environment+route, certable (ADR-018 §2).
                                    cert_eligible: true,
                                };

                                if let std::collections::hash_map::Entry::Vacant(e) =
                                    routes.entry(svc_domain.clone())
                                {
                                    let addresses: Vec<&str> =
                                        svc_backends.iter().map(|b| b.address.as_str()).collect();
                                    debug!(
                                        "Loaded compose public port route: {} -> {:?} (service={}, target_port={}, published_port={:?}, project={}, env={})",
                                        svc_domain,
                                        addresses,
                                        public_port.service,
                                        public_port.port,
                                        public_port.published,
                                        env.project_id,
                                        env.id
                                    );
                                    e.insert(svc_route_info);
                                }
                            }
                        }
                    }
                }
            }
        }

        debug!(
            "Loaded {} projects, {} environments, {} deployments into cache (on-demand)",
            projects_cache.len(),
            environments_cache.len(),
            deployments_cache.len()
        );

        // 5. Load all active deployments for all environments
        // This ensures we have complete coverage of all running deployments
        debug!("Loading all active deployments for environments...");

        // Get all environments with current_deployment_id (exclude soft-deleted)
        let all_active_envs = environments::Entity::find()
            .filter(environments::Column::CurrentDeploymentId.is_not_null())
            .filter(environments::Column::DeletedAt.is_null())
            .all(self.db.as_ref())
            .await?;

        for env in all_active_envs {
            // Skip sleeping on-demand environments — they are already recorded
            if env.sleeping {
                continue;
            }

            // Cache environment if not already cached
            environments_cache
                .entry(env.id)
                .or_insert_with(|| Arc::new(env.clone()));

            if let Some(deployment_id) = env.current_deployment_id {
                // Fetch deployment if not cached (accept any state — same rationale as section 4)
                if !deployments_cache.contains_key(&deployment_id) {
                    if let Ok(Some(dep)) = deployments::Entity::find_by_id(deployment_id)
                        .one(self.db.as_ref())
                        .await
                    {
                        deployments_cache.insert(dep.id, Arc::new(dep));
                    }
                }

                // Fetch project if not cached
                if !projects_cache.contains_key(&env.project_id) {
                    if let Ok(Some(proj)) = projects::Entity::find_by_id(env.project_id)
                        .one(self.db.as_ref())
                        .await
                    {
                        projects_cache.insert(proj.id, Arc::new(proj));
                    }
                }

                // Check if we have all required data cached
                if let (Some(deployment), Some(project), Some(environment)) = (
                    deployments_cache.get(&deployment_id),
                    projects_cache.get(&env.project_id),
                    environments_cache.get(&env.id),
                ) {
                    // Load all active containers for this deployment
                    use temps_entities::deployment_containers;
                    let containers = if deployment.state == "paused" {
                        // See the matching comment further up: trust
                        // `deployment.state` over per-container `status`, which
                        // can lag behind on a retry-exhausted write.
                        Vec::new()
                    } else {
                        deployment_containers::Entity::find()
                            .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
                            .filter(deployment_containers::Column::DeletedAt.is_null())
                            // See the matching comment above: `status` (not just
                            // `deleted_at`) governs routability, so a paused/stopped
                            // deployment's containers don't keep serving live traffic.
                            .filter(
                                Condition::any()
                                    .add(deployment_containers::Column::Status.is_null())
                                    .add(deployment_containers::Column::Status.eq("running")),
                            )
                            .all(self.db.as_ref())
                            .await
                            .unwrap_or_default()
                    };

                    // Determine backend type: static directory or upstream containers
                    let backend = if let Some(static_dir) = &deployment.static_dir_location {
                        // Static deployment - serve from directory
                        Some(BackendType::StaticDir {
                            path: static_dir.clone(),
                        })
                    } else if !containers.is_empty() {
                        let public_port =
                            project
                                .preset_config
                                .as_ref()
                                .and_then(|config| match config {
                                    temps_entities::preset::PresetConfig::DockerCompose(config) => {
                                        config.public_ports.first()
                                    }
                                    _ => None,
                                });
                        match select_public_route_containers(&containers, public_port) {
                            // A Compose stack without a public port is private.
                            None if public_port.is_none() => continue,
                            // The public service has no running container.
                            None => None,
                            Some(route_containers) => {
                                let mut backend_entries =
                                    Vec::with_capacity(route_containers.len());
                                for c in route_containers {
                                    let node_addr = resolve_node_private_address(
                                        c.node_id,
                                        &mut nodes_cache,
                                        self.db.as_ref(),
                                    )
                                    .await;
                                    let entry = match public_port {
                                        Some(port) => build_public_compose_backend_entry(
                                            c,
                                            node_addr.as_deref(),
                                            port,
                                            self.runtime_context.as_ref(),
                                        ),
                                        None => build_backend_entry(
                                            c,
                                            node_addr.as_deref(),
                                            self.runtime_context.as_ref(),
                                        ),
                                    };
                                    if let Some(entry) = entry {
                                        backend_entries.push(entry);
                                    }
                                }
                                (!backend_entries.is_empty()).then(|| BackendType::Upstream {
                                    backends: backend_entries,
                                    round_robin_counter: Arc::new(AtomicUsize::new(0)),
                                })
                            }
                        }
                    } else {
                        None
                    };

                    // Generate a fallback route using deployment slug if no other routes exist
                    // This ensures every active deployment is accessible
                    let fallback_domain = PublicHostnameStrategy::Standard
                        .deployment_hostname(&preview_domain, &deployment.slug);

                    // No live backend: the deployment URL answers 503 (see
                    // section 1) rather than falling through to the console.
                    let Some(backend) = backend else {
                        if !compose_without_public_ports(Some(project.as_ref())) {
                            if let Some(route) =
                                unavailable_route_for(Some(project), environment, deployment)
                            {
                                debug!(
                                    "Deployment fallback host has no live backend: {} (project={}, env={}, deploy={}, reason={})",
                                    fallback_domain, env.project_id, env.id, deployment_id, route.reason.as_str()
                                );
                                unavailable.record(&fallback_domain, &route);
                            }
                        }
                        continue;
                    };

                    if !routes.contains_key(&fallback_domain) {
                        routes.insert(
                            fallback_domain.clone(),
                            RouteInfo {
                                backend: backend.clone(),
                                redirect_to: None,
                                status_code: None,
                                project: Some(Arc::clone(project)),
                                environment: Some(Arc::clone(environment)),
                                deployment: Some(Arc::clone(deployment)),
                                // EPHEMERAL per-deployment fallback hostname
                                // (`<deployment-slug>.<preview>`) — a new name on
                                // every deploy. NEVER on-demand certable: it would
                                // churn certs against the shared sslip.io bucket
                                // and trip LE per-hostname limits (ADR-018 §2).
                                cert_eligible: false,
                            },
                        );
                        match &backend {
                            BackendType::Upstream { backends, .. } => {
                                let addresses: Vec<&str> =
                                    backends.iter().map(|b| b.address.as_str()).collect();
                                debug!(
                                    "Loaded fallback route for active deployment: {} -> {:?} ({} containers, project={}, env={}, deploy={})",
                                    fallback_domain, addresses, addresses.len(), env.project_id, env.id, deployment_id
                                );
                            }
                            BackendType::StaticDir { path } => {
                                debug!(
                                    "Loaded fallback route for active deployment (static): {} -> {} (project={}, env={}, deploy={})",
                                    fallback_domain, path, env.project_id, env.id, deployment_id
                                );
                            }
                        }
                    }
                }
            }
        }

        debug!("Loaded all active deployments. Final cache: {} projects, {} environments, {} deployments",
            projects_cache.len(), environments_cache.len(), deployments_cache.len());

        // 6. Load Traefik-label discovered routes (containers Temps did NOT
        // deploy — an operator's existing docker-compose/Coolify/Dokploy
        // stack). Written by `temps_deployer::traefik_discovery`; opt-in via
        // TEMPS_TRAEFIK_DISCOVERY_ENABLED, so this section is empty on a
        // default install.
        //
        // Loaded LAST on purpose. The discovery reconciler already refuses to
        // write a host owned by a deployment, custom route, custom domain, or
        // environment subdomain, but that check races a concurrent write and
        // cannot see rows another control plane node added a millisecond ago.
        // This merge is the authoritative precedence rule: a discovered route
        // is only ever placed on a hostname that no DB-driven route claimed in
        // this same rebuild. An adversarial (or merely careless) workload can
        // therefore never take a real deployment's domain by relabelling
        // itself — the worst it can do is fail to be routed and get logged.
        //
        // Scoped to the network this node is *currently* configured to adopt
        // from, and skipped entirely when discovery is off here. Without that
        // scope, turning discovery off (or repointing it at another network)
        // would leave every previously-adopted row still routing forever: the
        // rows outlive the configuration that created them, and only the
        // reconciler — which no longer runs — would ever delete them.
        let discovery_network = self.traefik_discovery_network.read().clone();
        let discovered_routes = match discovery_network.as_deref() {
            Some(network) => {
                temps_entities::traefik_discovered_routes::Entity::find()
                    .filter(temps_entities::traefik_discovered_routes::Column::Enabled.eq(true))
                    .filter(
                        temps_entities::traefik_discovered_routes::Column::Network
                            .eq(network.to_string()),
                    )
                    .all(self.db.as_ref())
                    .await?
            }
            None => Vec::new(),
        };

        if !discovered_routes.is_empty() {
            debug!(
                "Section 6: Loading {} Traefik-discovered routes (network={})",
                discovered_routes.len(),
                discovery_network.as_deref().unwrap_or("<disabled>")
            );
        }

        for discovered in discovered_routes {
            let host = discovered.host.trim().to_ascii_lowercase();
            if host.is_empty() {
                continue;
            }

            // Precedence: any DB-driven route for this host wins, whether it
            // is an exact HTTP/TLS route, a wildcard pattern that covers the
            // host, or a legacy environment/custom-domain entry.
            let claimed_by = if routes.contains_key(&host) {
                Some("environment or custom domain route")
            } else if http_routes_map.contains_key(&host) {
                Some("HTTP custom route")
            } else if tls_routes_map.contains_key(&host) {
                Some("TLS custom route")
            } else if http_wildcards_matcher.match_domain(&host).is_some() {
                Some("HTTP wildcard custom route")
            } else if tls_wildcards_matcher.match_domain(&host).is_some() {
                Some("TLS wildcard custom route")
            } else {
                None
            };
            if let Some(owner) = claimed_by {
                warn!(
                    "Ignoring Traefik-discovered route for '{}' (container '{}'): the host \
                     already resolves to a {}. The existing route is kept.",
                    host, discovered.target_container_name, owner
                );
                continue;
            }

            // Same address construction as Temps-deployed containers, so
            // baremetal installs (where the proxy cannot resolve Docker's
            // internal DNS) use the published host port. The discovered
            // container is always local to this node — remote workers run their
            // own discovery against their own daemon.
            //
            // `None` means this deployment mode genuinely cannot reach the
            // container. Say so loudly instead of routing the host somewhere
            // else: a baremetal install has no way to reach an unpublished
            // container port, and guessing `127.0.0.1:<container port>` lands
            // on whatever unrelated service owns that port on the host.
            let Some(address) = build_discovered_backend_addr(
                &discovered.target_container_name,
                discovered.target_port,
                discovered.target_host_port,
                self.runtime_context.as_ref(),
            ) else {
                warn!(
                    "Skipping Traefik-discovered route for '{}' (container '{}', port {}): this \
                     install runs outside Docker (baremetal mode) and the container publishes no \
                     host port, so there is no address that reaches it. Publish the port on the \
                     container (e.g. `ports: - \"8080:{}\"`) or run Temps in Docker mode.",
                    host,
                    discovered.target_container_name,
                    discovered.target_port,
                    discovered.target_port
                );
                continue;
            };

            routes.insert(
                host.clone(),
                RouteInfo {
                    backend: BackendType::Upstream {
                        backends: vec![BackendEntry {
                            address: address.clone(),
                            container_id: Some(discovered.target_container_id.clone()),
                            container_name: Some(discovered.target_container_name.clone()),
                        }],
                        round_robin_counter: Arc::new(AtomicUsize::new(0)),
                    },
                    redirect_to: None,
                    status_code: None,
                    // Not a Temps deployment: no project/environment/deployment
                    // context exists for these containers by definition.
                    project: None,
                    environment: None,
                    deployment: None,
                    // TODO(security): pre-merge review finding — a discovered
                    // container's own `traefik...tls` label used to be mirrored
                    // here, which let any container on the watched network
                    // drive ACME issuance for a hostname *it* chose, burning
                    // Let's Encrypt rate limits and minting certs the operator
                    // never asked for. Hard-coded off until issuance for
                    // discovered hosts is gated on an explicit operator
                    // allowlist. `discovered.tls` is still persisted and shown
                    // in the admin API, so nothing is lost but auto-issuance.
                    cert_eligible: false,
                },
            );
            debug!(
                "Loaded Traefik-discovered route: {} -> {} (container={}, network={}, tls={})",
                host, address, discovered.target_container_name, discovered.network, discovered.tls
            );
        }

        // The console hostname is owned by the control plane, never by a
        // project. A route pointing it at a deployment locks the operator out
        // of the console entirely (issue #478) — the create/update API now
        // refuses such domains, but installs that already stored one would
        // otherwise stay bricked until the row is deleted over the public IP.
        // Dropping it here makes the next reload self-heal.
        if let Some(console_host) = app_settings.console_hostname() {
            let removed = routes.remove(&console_host).is_some()
                | http_routes_map.remove(&console_host).is_some()
                | tls_routes_map.remove(&console_host).is_some();
            if removed {
                warn!(
                    "Ignoring project route for reserved console hostname '{}' — the Temps console keeps it (issue #478)",
                    console_host
                );
            }
        }

        // A live route always wins over an unavailable entry for the same
        // host, whichever section recorded it, and the console hostname is
        // never reported as an application host. Lookups re-check live
        // wildcard routes; this keeps the map down to hosts that matter.
        unavailable.exact.retain(|host, _| {
            !routes.contains_key(host)
                && !http_routes_map.contains_key(host)
                && !tls_routes_map.contains_key(host)
        });
        unavailable
            .wildcard_bases
            .retain(|base, _| !routes.contains_key(&format!("*.{base}")));
        if let Some(console_host) = app_settings.console_hostname() {
            unavailable.exact.remove(&console_host);
        }

        // Build the wildcard index for legacy project/environment routes. A
        // concrete hostname resolved through a wildcard is deliberately not
        // eligible for on-demand HTTP-01 issuance: one stored wildcard may
        // cover an unbounded number of subdomains, while its certificate is
        // provisioned separately through DNS-01 and found by the TLS loader.
        let mut legacy_wildcards_matcher = WildcardMatcher::new();
        for (host, route) in routes.iter().filter(|(host, _)| {
            host.starts_with("*.")
                && !http_wildcards_matcher.contains_pattern(host)
                && !tls_wildcards_matcher.contains_pattern(host)
        }) {
            let mut wildcard_route = route.clone();
            wildcard_route.cert_eligible = false;
            legacy_wildcards_matcher.insert(host, wildcard_route);
        }

        let route_ownership = build_route_ownership_snapshot(
            routes.keys(),
            http_routes_map.keys(),
            tls_routes_map.keys(),
            app_settings.console_hostname(),
        );

        // Atomically replace all route tables
        let route_count = routes.len();
        let http_routes_count = http_routes_map.len();
        let tls_routes_count = tls_routes_map.len();
        let http_wildcards_count = http_wildcards_matcher.len();
        let tls_wildcards_count = tls_wildcards_matcher.len();
        let legacy_wildcards_count = legacy_wildcards_matcher.len();
        let unavailable_count = unavailable.len();

        let new_snapshot = Arc::new(RouteTableSnapshot {
            http_routes: http_routes_map,
            tls_routes: tls_routes_map,
            http_wildcards: http_wildcards_matcher,
            tls_wildcards: tls_wildcards_matcher,
            legacy: LegacyRouteTable {
                exact: routes,
                wildcards: legacy_wildcards_matcher,
                reserved_console_host: app_settings.console_hostname(),
            },
            ownership: route_ownership,
            unavailable,
        });

        // Collect on-demand configs for awake environments so the idle sweep can track them.
        let on_demand_configs: Vec<OnDemandConfigEntry> = environments_cache
            .values()
            .filter(|env| !env.sleeping)
            .filter_map(|env| {
                let dc = env.deployment_config.as_ref()?;
                if dc.on_demand {
                    Some(OnDemandConfigEntry {
                        environment_id: env.id,
                        idle_timeout_seconds: dc.idle_timeout_seconds,
                        wake_timeout_seconds: dc.wake_timeout_seconds,
                    })
                } else {
                    None
                }
            })
            .collect();

        // Three-phase publication keeps route ownership conservative while
        // the separate sleeping-domain snapshot changes:
        // old routes/old sleeping -> old routes/(old ∪ new) ownership/new sleeping
        // -> new routes/new ownership/new sleeping. Thus a transition can only
        // suppress a wake briefly; it can never wake the wrong environment.
        let old_snapshot = self.route_snapshot.load_full();
        let mut transition = (*old_snapshot).clone();
        transition.ownership.include(&new_snapshot.ownership);
        self.route_snapshot.store(Arc::new(transition));

        let on_sleeping = self.on_sleeping_callback.lock().as_ref().cloned();
        if let Some(callback) = on_sleeping {
            callback(sleeping_environments.clone(), on_demand_configs);
        }

        self.route_snapshot.store(new_snapshot);

        if !sleeping_environments.is_empty() {
            info!(
                "Route table: {} sleeping on-demand environments skipped",
                sleeping_environments.len()
            );
        }

        info!(
            "Route table loaded with {} legacy routes; typed caches contain {} HTTP exact, {} TLS exact, {} HTTP wildcards, {} TLS wildcards, {} legacy wildcards; {} application hosts have no live backend",
            route_count, http_routes_count, tls_routes_count, http_wildcards_count, tls_wildcards_count, legacy_wildcards_count, unavailable_count
        );
        debug!(
            "Found {} on-demand configs for idle tracking",
            environments_cache
                .values()
                .filter(|environment| !environment.sleeping)
                .filter(|environment| environment
                    .deployment_config
                    .as_ref()
                    .is_some_and(|config| config.on_demand))
                .count()
        );

        // Bump the in-memory generation and wake any long-poll waiters
        // on the routes-sync endpoint, plus anything waiting for the first
        // load via `wait_until_loaded`. Order matters: bump first, then
        // notify, so a wake-up that races with a subsequent fetch always
        // sees the new value. Do this BEFORE the DNS reconcile so readiness
        // waiters are released as soon as the in-memory maps are live —
        // they must never be gated on DNS work.
        //
        // The authoritative process claims the number from the durable
        // counter, so it continues across restarts. Restarting at 1 would
        // leave every agent long-polling with a `since` above it -- not woken
        // by new generations until its 25s poll expires -- and leave their
        // old, higher ACKs satisfying the completion gate for routes they
        // never received. The claim is also the only write of that row, so
        // `mark_deployment_complete`'s target is always a generation the
        // snapshot endpoint has issued.
        let new_gen = self.advance_generation().await;
        debug!(
            generation = new_gen,
            role = ?self.route_generation_role(),
            "Route table generation advanced"
        );
        self.generation_changed.notify_waiters();

        // Fire the async on-reload hook used by the deployment-DNS publisher
        // as a detached task. It's whole-set idempotent and must not block the
        // route load (or, by extension, the proxy's first-load readiness wait).
        // We snapshot the Arc out of the mutex first so the mutex isn't held
        // across the spawn.
        let on_reload = self.on_reload_callback.lock().as_ref().cloned();
        if let Some(callback) = on_reload {
            tokio::spawn(async move {
                callback().await;
            });
        }

        // Eager TLS pre-provisioning (ADR-018): fire the cert-eligible callback
        // with the stable per-environment hostnames from the just-loaded table.
        // The proxy wires the on-demand cert manager here so every new deployment
        // gets a cert issued immediately rather than on the first failing handshake.
        let on_cert_eligible = self.on_cert_eligible_callback.lock().as_ref().cloned();
        if let Some(callback) = on_cert_eligible {
            let cert_hosts: Vec<String> = self
                .route_snapshot
                .load()
                .legacy
                .exact
                .iter()
                .filter(|(host, r)| r.cert_eligible && !host.starts_with("*."))
                .map(|(host, _)| host.clone())
                .collect();
            if !cert_hosts.is_empty() {
                tokio::spawn(async move {
                    callback(cert_hosts).await;
                });
            }
        }

        Ok(sleeping_environments)
    }

    /// Get route information for a host (O(1) lookup)
    pub fn get_route(&self, host: &str) -> Option<RouteInfo> {
        self.route_snapshot.load().legacy.get(host).cloned()
    }

    /// Look up an application hostname whose deployment currently has no live
    /// upstream (stopped or crashed containers, or a paused deployment).
    ///
    /// Returns `None` whenever a live route resolves the host, so a routable
    /// host is never reported as unavailable, and for the reserved console
    /// hostname. Sleeping on-demand environments are never recorded here:
    /// they keep going through the wake path. One snapshot load; meant for
    /// the proxy's route-miss path, not for every request.
    pub fn get_unavailable_route(&self, host: &str) -> Option<UnavailableRoute> {
        let snapshot = self.route_snapshot.load();
        // The unavailable map is usually empty, so check it first: console
        // and unknown-host requests pay one hash lookup, not the full chain.
        let route = snapshot.unavailable.get(host)?;
        if snapshot.legacy.reserved_console_host.as_deref() == Some(host)
            || snapshot.has_live_route(host)
        {
            return None;
        }
        Some(route.clone())
    }

    /// Record an unavailable hostname for cross-crate tests without database
    /// setup. Not used on the production load path.
    #[doc(hidden)]
    pub fn insert_unavailable_route_for_test(&self, host: &str, route: UnavailableRoute) {
        let mut snapshot = (*self.route_snapshot.load_full()).clone();
        snapshot.unavailable.record(host, &route);
        self.route_snapshot.store(Arc::new(snapshot));
    }

    /// Whether this hostname is reserved for the control-plane console.
    /// Wake-on-request checks this before consulting sleeping wildcards.
    pub fn is_reserved_hostname(&self, host: &str) -> bool {
        self.route_snapshot
            .load()
            .legacy
            .reserved_console_host
            .as_deref()
            == Some(host)
    }

    /// Return whether an active route or reserved control-plane hostname owns
    /// `host`. This is a single lock-free snapshot load and does not clone a
    /// `RouteInfo`; it is intended for the per-request wake gate.
    pub fn owns_hostname(&self, host: &str) -> bool {
        self.route_snapshot.load().ownership.owns(host)
    }

    /// Get current number of routes in the table
    pub fn len(&self) -> usize {
        self.route_snapshot.load().legacy.exact.len()
    }

    /// Check if the route table is empty
    pub fn is_empty(&self) -> bool {
        self.route_snapshot.load().legacy.exact.is_empty()
    }

    /// Check if any route in the table points to a specific deployment.
    ///
    /// Used by `mark_deployment_complete` to verify the proxy's in-memory route
    /// table has actually loaded the new deployment — not just that the DB row
    /// was written (which would always be true since we just wrote it).
    pub fn has_route_for_deployment(&self, deployment_id: i32) -> bool {
        let snapshot = self.route_snapshot.load();
        snapshot.legacy.exact.values().any(|route| {
            route
                .deployment
                .as_ref()
                .is_some_and(|d| d.id == deployment_id)
        })
    }

    /// Snapshot of every `*.temps.local` route currently in the table.
    /// Returned as a flat `(host, RouteInfo)` vector cloned out of the
    /// current immutable route snapshot. Used by the internal
    /// route-sync endpoint to fan out the worker-side proxy table.
    ///
    /// Filters to the internal zone only. Custom-domain and preview
    /// routes are not part of this contract — workers don't need them
    /// (only the public edge proxy does).
    pub fn snapshot_internal_routes(&self) -> Vec<(String, RouteInfo)> {
        let snapshot = self.route_snapshot.load();
        snapshot
            .legacy
            .exact
            .iter()
            .filter(|(host, _)| host.ends_with(".temps.local"))
            .map(|(h, r)| (h.clone(), r.clone()))
            .collect()
    }

    /// Snapshot public container routes which can be served without bypassing
    /// control-plane request policy. Routes requiring redirect, wake-up,
    /// attack-mode, or per-project security processing stay on the control
    /// plane until those policies have a worker-side representation.
    pub fn snapshot_worker_public_routes(&self) -> Vec<(String, RouteInfo)> {
        let snapshot = self.route_snapshot.load();
        snapshot
            .legacy
            .exact
            .iter()
            .filter(|(host, route)| {
                !host.ends_with(".temps.local")
                    && route.redirect_to.is_none()
                    && matches!(route.backend, BackendType::Upstream { .. })
                    && route.environment.as_ref().is_some_and(|environment| {
                        !environment.sleeping
                            && environment.attack_mode != Some(true)
                            && environment
                                .deployment_config
                                .as_ref()
                                .and_then(|config| config.security.as_ref())
                                .is_none()
                    })
                    && route.project.as_ref().is_some_and(|project| {
                        !project.attack_mode
                            && project
                                .deployment_config
                                .as_ref()
                                .and_then(|config| config.security.as_ref())
                                .is_none()
                    })
            })
            .map(|(host, route)| (host.clone(), route.clone()))
            .collect()
    }

    pub fn worker_public_route_count(&self) -> usize {
        self.route_snapshot
            .load()
            .legacy
            .exact
            .keys()
            .filter(|host| !host.ends_with(".temps.local"))
            .count()
    }
}

#[temps_core::async_trait::async_trait]
impl temps_core::route_table::RouteTableRefresher for CachedPeerTable {
    async fn refresh_routes(&self) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        self.load_routes().await?;
        let snapshot = self.route_snapshot.load();
        let count =
            snapshot.legacy.exact.len() + snapshot.http_routes.len() + snapshot.tls_routes.len();
        Ok(count)
    }
}

/// Listens for PostgreSQL notifications and automatically reloads the route table
pub struct RouteTableListener {
    peer_table: Arc<CachedPeerTable>,
    database_url: String,
    queue: Arc<dyn temps_core::JobQueue>,
    task_handle: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl RouteTableListener {
    pub fn new(
        peer_table: Arc<CachedPeerTable>,
        database_url: String,
        queue: Arc<dyn temps_core::JobQueue>,
    ) -> Self {
        Self {
            peer_table,
            database_url,
            queue,
            task_handle: std::sync::Mutex::new(None),
        }
    }

    /// Start listening for route table changes.
    ///
    /// Subscribes to PostgreSQL NOTIFYs first, then kicks off the initial route
    /// load on a detached task. Returning as soon as the LISTEN socket is up
    /// (rather than after the initial load) lets the proxy bind its listeners
    /// without waiting for the full, DB-heavy route load — the table fills in
    /// asynchronously and the proxy's first-load readiness wait covers the gap.
    ///
    /// Subscribing *before* the load (instead of after) closes a race: a NOTIFY
    /// fired during the initial load is buffered by the already-subscribed
    /// `PgListener` and drained by the recv loop, so no change is missed.
    pub async fn start_listening(self: Arc<Self>) -> anyhow::Result<()> {
        // Create PostgreSQL listener using sqlx and subscribe BEFORE the load.
        let pool = PgPool::connect(&self.database_url).await?;
        let mut listener = PgListener::connect_with(&pool).await?;

        listener.listen("route_table_changes").await?;
        debug!(
            "Started listening for route table changes on PostgreSQL channel 'route_table_changes'"
        );

        // Kick off the initial load in the background — do NOT await it here.
        let initial_peer_table = self.peer_table.clone();
        tokio::spawn(async move {
            debug!("Loading initial route table (background)...");
            match initial_peer_table.load_routes().await {
                Ok(_) => debug!(
                    "Initial route table loaded with {} entries",
                    initial_peer_table.len()
                ),
                Err(e) => error!("Initial route table load failed: {}", e),
            }
        });

        // Spawn background task driven purely by PG NOTIFY events. Reloads
        // happen on demand: a NOTIFY arrives (insert/update/delete via DB
        // trigger) or the listener reconnects after an error. There is no
        // periodic timer — a quiet system stays quiet.
        let peer_table = self.peer_table.clone();
        let queue = self.queue.clone();
        let handle = tokio::spawn(async move {
            loop {
                match listener.recv().await {
                    Ok(n) => {
                        debug!("Received route table change notification: {}", n.payload());
                    }
                    Err(e) => {
                        error!("Listener error: {}", e);

                        // Attempt to reconnect after error
                        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

                        match PgListener::connect_with(&pool).await {
                            Ok(mut new_listener) => {
                                if let Err(e) = new_listener.listen("route_table_changes").await {
                                    error!("Failed to re-subscribe to notifications: {}", e);
                                } else {
                                    listener = new_listener;
                                    info!("Reconnected to route table notification listener");
                                }
                            }
                            Err(e) => {
                                error!("Failed to reconnect listener: {}", e);
                                warn!("Route table updates will not be received until reconnection succeeds");
                            }
                        }
                        // Fall through to reload once after reconnect to
                        // catch any changes missed during the gap.
                    }
                }

                // Reload routes after a NOTIFY or a reconnect
                if let Err(e) = peer_table.load_routes().await {
                    error!("Failed to reload routes: {}", e);
                } else {
                    let route_count = peer_table.len();
                    debug!("Route table synchronized ({} entries)", route_count);

                    let event =
                        temps_core::Job::RouteTableUpdated(temps_core::RouteTableUpdatedJob {
                            environment_id: None,
                            deployment_id: None,
                            route_count,
                        });
                    if let Err(e) = queue.send(event).await {
                        error!("Failed to send RouteTableUpdated event: {}", e);
                    }
                }
            }
        });

        // Store the handle so it can be aborted on drop
        if let Ok(mut guard) = self.task_handle.lock() {
            *guard = Some(handle);
        }

        Ok(())
    }

    /// Stop the background listener task
    pub fn shutdown(&self) {
        if let Ok(mut guard) = self.task_handle.lock() {
            if let Some(handle) = guard.take() {
                handle.abort();
                info!("Route table listener stopped");
            }
        }
    }
}

impl Drop for RouteTableListener {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot_test_route() -> RouteInfo {
        RouteInfo {
            backend: BackendType::StaticDir {
                path: "/tmp/test-route".to_string(),
            },
            redirect_to: None,
            status_code: None,
            project: None,
            environment: None,
            deployment: None,
            cert_eligible: false,
        }
    }

    #[test]
    fn route_and_ownership_indexes_publish_as_one_generation() {
        let table = Arc::new(CachedPeerTable::new(Arc::new(
            sea_orm::DatabaseConnection::Disconnected,
        )));
        let writer = Arc::clone(&table);
        let handle = std::thread::spawn(move || {
            for generation in 0..2_000 {
                let host = if generation % 2 == 0 {
                    "blue.example.com"
                } else {
                    "green.example.com"
                };
                let mut snapshot = RouteTableSnapshot::default();
                snapshot
                    .legacy
                    .exact
                    .insert(host.to_string(), snapshot_test_route());
                snapshot.ownership.exact.insert(host.to_string());
                writer.route_snapshot.store(Arc::new(snapshot));
            }
        });

        for _ in 0..10_000 {
            let snapshot = table.route_snapshot.load();
            for host in ["blue.example.com", "green.example.com"] {
                assert_eq!(
                    snapshot.legacy.exact.contains_key(host),
                    snapshot.ownership.owns(host),
                    "one atomic snapshot must never mix route and ownership generations"
                );
            }
        }
        handle.join().expect("snapshot writer thread panicked");
    }

    /// Create a no-op queue for tests that don't need queue functionality
    fn test_queue() -> Arc<dyn temps_core::JobQueue> {
        struct NoOpQueue;
        #[temps_core::async_trait::async_trait]
        impl temps_core::JobQueue for NoOpQueue {
            async fn send(&self, _job: temps_core::Job) -> Result<(), temps_core::QueueError> {
                Ok(())
            }
            fn subscribe(&self) -> Box<dyn temps_core::JobReceiver> {
                unimplemented!("not needed in tests")
            }
        }
        Arc::new(NoOpQueue)
    }

    #[test]
    fn test_route_info_creation() {
        let route = RouteInfo {
            backend: BackendType::Upstream {
                backends: vec![BackendEntry {
                    address: "127.0.0.1:8080".to_string(),
                    container_id: None,
                    container_name: None,
                }],
                round_robin_counter: Arc::new(AtomicUsize::new(0)),
            },
            redirect_to: None,
            status_code: None,
            project: None,
            environment: None,
            deployment: None,
            cert_eligible: false,
        };

        assert_eq!(route.get_backend_addr(), "127.0.0.1:8080");
        assert!(!route.is_static());
        assert!(route.project.is_none());
        assert!(route.environment.is_none());
        assert!(route.deployment.is_none());
        assert!(route.redirect_to.is_none());
    }

    #[test]
    fn test_route_info_with_redirect() {
        let route = RouteInfo {
            backend: BackendType::Upstream {
                backends: vec![BackendEntry {
                    address: "127.0.0.1:8080".to_string(),
                    container_id: None,
                    container_name: None,
                }],
                round_robin_counter: Arc::new(AtomicUsize::new(0)),
            },
            redirect_to: Some("https://example.com".to_string()),
            status_code: Some(301),
            project: None,
            environment: None,
            deployment: None,
            cert_eligible: false,
        };

        assert_eq!(route.redirect_to, Some("https://example.com".to_string()));
        assert_eq!(route.status_code, Some(301));
    }

    #[test]
    fn test_route_info_custom_route() {
        let route = RouteInfo {
            backend: BackendType::Upstream {
                backends: vec![BackendEntry {
                    address: "192.168.1.100:3000".to_string(),
                    container_id: None,
                    container_name: None,
                }],
                round_robin_counter: Arc::new(AtomicUsize::new(0)),
            },
            redirect_to: None,
            status_code: None,
            project: None,
            environment: None,
            deployment: None,
            cert_eligible: false,
        };

        assert_eq!(route.get_backend_addr(), "192.168.1.100:3000");
        assert!(!route.is_static());
        assert!(route.project.is_none());
        assert!(route.environment.is_none());
        assert!(route.deployment.is_none());
    }

    #[test]
    fn test_route_info_load_balancing() {
        let route = RouteInfo {
            backend: BackendType::Upstream {
                backends: vec![
                    BackendEntry {
                        address: "127.0.0.1:8080".to_string(),
                        container_id: None,
                        container_name: None,
                    },
                    BackendEntry {
                        address: "127.0.0.1:8081".to_string(),
                        container_id: None,
                        container_name: None,
                    },
                    BackendEntry {
                        address: "127.0.0.1:8082".to_string(),
                        container_id: None,
                        container_name: None,
                    },
                ],
                round_robin_counter: Arc::new(AtomicUsize::new(0)),
            },
            redirect_to: None,
            status_code: None,
            project: None,
            environment: None,
            deployment: None,
            cert_eligible: false,
        };

        // Test round-robin load balancing
        assert_eq!(route.get_backend_addr(), "127.0.0.1:8080");
        assert_eq!(route.get_backend_addr(), "127.0.0.1:8081");
        assert_eq!(route.get_backend_addr(), "127.0.0.1:8082");
        assert_eq!(route.get_backend_addr(), "127.0.0.1:8080"); // Wraps around
    }

    #[test]
    fn test_route_info_static_backend() {
        let route = RouteInfo {
            backend: BackendType::StaticDir {
                path: "/var/www/static".to_string(),
            },
            redirect_to: None,
            status_code: None,
            project: None,
            environment: None,
            deployment: None,
            cert_eligible: false,
        };

        assert!(route.is_static());
        assert_eq!(route.static_dir(), Some("/var/www/static"));
        assert_eq!(route.get_backend_addr(), "127.0.0.1:8080"); // Fallback for static
    }

    #[test]
    fn test_backend_type_upstream() {
        let backend = BackendType::Upstream {
            backends: vec![
                BackendEntry {
                    address: "127.0.0.1:8080".to_string(),
                    container_id: None,
                    container_name: None,
                },
                BackendEntry {
                    address: "127.0.0.1:8081".to_string(),
                    container_id: None,
                    container_name: None,
                },
            ],
            round_robin_counter: Arc::new(AtomicUsize::new(0)),
        };

        assert!(!backend.is_static());
        assert_eq!(backend.static_dir(), None);
        assert_eq!(
            backend.get_backend_addr(),
            Some("127.0.0.1:8080".to_string())
        );
        assert_eq!(
            backend.get_backend_addr(),
            Some("127.0.0.1:8081".to_string())
        );
        assert_eq!(
            backend.get_backend_addr(),
            Some("127.0.0.1:8080".to_string())
        ); // Wraps
    }

    #[test]
    fn test_backend_type_static_dir() {
        let backend = BackendType::StaticDir {
            path: "/opt/static-files".to_string(),
        };

        assert!(backend.is_static());
        assert_eq!(backend.static_dir(), Some("/opt/static-files"));
        assert_eq!(backend.get_backend_addr(), None); // No backend addr for static
    }

    #[test]
    fn test_backend_type_upstream_empty_addresses() {
        let backend = BackendType::Upstream {
            backends: vec![],
            round_robin_counter: Arc::new(AtomicUsize::new(0)),
        };

        assert!(!backend.is_static());
        // Should return fallback address for empty upstream list
        assert_eq!(
            backend.get_backend_addr(),
            Some("127.0.0.1:8080".to_string())
        );
    }

    #[test]
    fn test_backend_type_upstream_single_address() {
        let backend = BackendType::Upstream {
            backends: vec![BackendEntry {
                address: "192.168.1.100:3000".to_string(),
                container_id: None,
                container_name: None,
            }],
            round_robin_counter: Arc::new(AtomicUsize::new(0)),
        };

        // Should always return the same address for single upstream
        assert_eq!(
            backend.get_backend_addr(),
            Some("192.168.1.100:3000".to_string())
        );
        assert_eq!(
            backend.get_backend_addr(),
            Some("192.168.1.100:3000".to_string())
        );
        assert_eq!(
            backend.get_backend_addr(),
            Some("192.168.1.100:3000".to_string())
        );
    }

    #[test]
    fn test_route_info_methods_with_static_backend() {
        let route = RouteInfo {
            backend: BackendType::StaticDir {
                path: "/srv/static".to_string(),
            },
            redirect_to: None,
            status_code: None,
            project: None,
            environment: None,
            deployment: None,
            cert_eligible: false,
        };

        // Test all convenience methods
        assert!(route.is_static());
        assert_eq!(route.static_dir(), Some("/srv/static"));
        assert_eq!(route.get_backend_addr(), "127.0.0.1:8080"); // Fallback
    }

    #[test]
    fn test_route_info_methods_with_upstream_backend() {
        let route = RouteInfo {
            backend: BackendType::Upstream {
                backends: vec![BackendEntry {
                    address: "10.0.0.1:9000".to_string(),
                    container_id: None,
                    container_name: None,
                }],
                round_robin_counter: Arc::new(AtomicUsize::new(0)),
            },
            redirect_to: None,
            status_code: None,
            project: None,
            environment: None,
            deployment: None,
            cert_eligible: false,
        };

        // Test all convenience methods
        assert!(!route.is_static());
        assert_eq!(route.static_dir(), None);
        assert_eq!(route.get_backend_addr(), "10.0.0.1:9000");
    }

    // ========================================================================
    // RouteTableListener lifecycle tests
    // ========================================================================

    #[test]
    fn test_route_table_listener_new_has_no_task() {
        let db = Arc::new(sea_orm::DatabaseConnection::Disconnected);
        let peer_table = Arc::new(CachedPeerTable::new(db));
        let listener = RouteTableListener::new(
            peer_table,
            "postgresql://fake:fake@localhost/fake".to_string(),
            test_queue(),
        );

        let guard = listener.task_handle.lock().unwrap();
        assert!(guard.is_none(), "New listener should have no task handle");
    }

    #[test]
    fn test_route_table_listener_shutdown_without_start_is_safe() {
        let db = Arc::new(sea_orm::DatabaseConnection::Disconnected);
        let peer_table = Arc::new(CachedPeerTable::new(db));
        let listener = RouteTableListener::new(
            peer_table,
            "postgresql://fake:fake@localhost/fake".to_string(),
            test_queue(),
        );

        // Calling shutdown before start should not panic
        listener.shutdown();

        let guard = listener.task_handle.lock().unwrap();
        assert!(guard.is_none());
    }

    #[test]
    fn test_route_table_listener_drop_without_start_is_safe() {
        let db = Arc::new(sea_orm::DatabaseConnection::Disconnected);
        let peer_table = Arc::new(CachedPeerTable::new(db));
        let listener = RouteTableListener::new(
            peer_table,
            "postgresql://fake:fake@localhost/fake".to_string(),
            test_queue(),
        );

        // Dropping without starting should not panic
        drop(listener);
    }

    #[test]
    fn test_build_container_backend_addr_local_docker() {
        // In Docker mode, local containers use container_name:container_port
        let addr = build_container_backend_addr(
            "my-app",
            3000,
            Some(8080),
            None,
            &RuntimeContext::docker(),
        );
        assert_eq!(addr, "my-app:3000");
    }

    #[test]
    fn test_build_container_backend_addr_local_baremetal() {
        // In baremetal mode (default), local containers use 127.0.0.1:host_port
        let addr =
            build_container_backend_addr("my-app", 3000, Some(8080), None, &RuntimeContext::host());
        assert_eq!(addr, "127.0.0.1:8080");
    }

    // ── Traefik-discovered backend addresses ─────────────────────────────

    #[test]
    fn discovered_backend_addr_uses_the_published_host_port_on_baremetal() {
        let addr =
            build_discovered_backend_addr("whoami", 8000, Some(18000), &RuntimeContext::host());
        assert_eq!(addr.as_deref(), Some("127.0.0.1:18000"));
    }

    /// The SSRF case: without a published host port, `127.0.0.1:<container
    /// port>` is a *different service on the Temps host* — Postgres on 5432,
    /// the Docker API on 2375, the console. Refuse to build an address at all.
    #[test]
    fn discovered_backend_addr_refuses_an_unpublished_port_on_baremetal() {
        let addr = build_discovered_backend_addr("whoami", 5432, None, &RuntimeContext::host());
        assert_eq!(
            addr, None,
            "a container port must never be reinterpreted as a loopback port on the host"
        );
    }

    /// In Docker mode the container really is reachable at
    /// `container_name:container_port` over the network's internal DNS, so an
    /// unpublished port is fine — the restriction is mode-specific, exactly as
    /// `build_public_compose_backend_addr` already encodes it.
    #[test]
    fn discovered_backend_addr_allows_an_unpublished_port_in_docker_mode() {
        let addr = build_discovered_backend_addr("whoami", 8000, None, &RuntimeContext::docker());
        assert_eq!(addr.as_deref(), Some("whoami:8000"));
    }

    fn route_test_container(
        id: i32,
        service_name: Option<&str>,
        container_port: i32,
    ) -> temps_entities::deployment_containers::Model {
        let now = chrono::Utc::now();
        temps_entities::deployment_containers::Model {
            id,
            deployment_id: 1,
            container_id: format!("container-{id}"),
            container_name: format!("container-{id}"),
            container_port,
            host_port: Some(10_000 + id),
            image_name: None,
            status: Some("running".to_string()),
            service_name: service_name.map(str::to_string),
            created_at: now,
            deployed_at: now,
            ready_at: Some(now),
            deleted_at: None,
            node_id: None,
            exit_code: None,
            exit_reason: None,
            oom_killed: None,
            error_message: None,
            finished_at: None,
            started_at: Some(now),
            cpu_limit_cores: None,
            port_bindings: None,
        }
    }

    #[test]
    fn ordinary_remote_route_omits_container_without_live_host_mapping() {
        let mut container = route_test_container(1, None, 5432);
        container.host_port = None;

        let entry = build_backend_entry(&container, Some("10.100.0.5"), &RuntimeContext::docker());

        assert!(entry.is_none());
    }

    #[test]
    fn ordinary_remote_route_omits_container_when_node_lookup_fails() {
        let mut container = route_test_container(1, None, 3000);
        container.node_id = Some(42);

        assert!(build_backend_entry(&container, None, &RuntimeContext::docker()).is_none());
    }

    #[test]
    fn compose_remote_route_omits_container_when_node_lookup_fails() {
        let mut container = route_test_container(1, Some("web"), 3000);
        container.node_id = Some(42);
        let public_port = ComposePublicPort {
            service: "web".to_string(),
            port: 3000,
            published: Some(10_001),
            health_check_path: None,
        };

        assert!(build_public_compose_backend_entry(
            &container,
            None,
            &public_port,
            &RuntimeContext::docker(),
        )
        .is_none());
    }

    #[test]
    fn ordinary_host_route_omits_container_without_live_host_mapping() {
        let mut container = route_test_container(1, None, 5432);
        container.host_port = None;

        assert!(build_backend_entry(&container, None, &RuntimeContext::host()).is_none());
    }

    #[test]
    fn ordinary_docker_network_route_keeps_unpublished_container() {
        let mut container = route_test_container(1, None, 3000);
        container.host_port = None;

        let entry = build_backend_entry(&container, None, &RuntimeContext::docker())
            .expect("Docker-network-local containers use their internal address");

        assert_eq!(entry.address, "container-1:3000");
    }

    #[test]
    fn compose_route_selects_only_the_configured_public_service_and_port() {
        let containers = vec![
            route_test_container(1, Some("database"), 5432),
            route_test_container(2, Some("web"), 8080),
            route_test_container(3, Some("web"), 9090),
        ];
        let public_port = ComposePublicPort {
            service: "web".to_string(),
            port: 8080,
            published: Some(18080),
            health_check_path: None,
        };

        let selected = select_public_route_containers(&containers, Some(&public_port)).unwrap();

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].service_name.as_deref(), Some("web"));
        assert_eq!(selected[0].container_port, 8080);
    }

    #[test]
    fn compose_route_without_public_port_stays_private() {
        let containers = vec![
            route_test_container(1, Some("database"), 5432),
            route_test_container(2, Some("web"), 8080),
        ];

        assert!(select_public_route_containers(&containers, None).is_none());
    }

    #[test]
    fn sleeping_environment_wakes_on_every_public_compose_service_hostname() {
        use temps_entities::preset::{DockerComposeConfig, PresetConfig};
        let route = |service: &str, port: u16| ComposePublicPort {
            service: service.to_string(),
            port,
            ..Default::default()
        };
        let config = PresetConfig::DockerCompose(DockerComposeConfig {
            public_ports: vec![
                route("trawl", 3000),
                route("trawl", 9222),
                route("api", 8080),
            ],
            ..Default::default()
        });

        let hosts = compose_public_service_hostnames(
            Some(&config),
            "localho.st",
            PublicHostnameStrategy::Standard,
            "app-production",
        );
        assert_eq!(
            hosts,
            vec![
                "trawl--app-production.localho.st",
                "trawl-9222--app-production.localho.st",
                "api--app-production.localho.st",
            ]
        );
        assert!(compose_public_service_hostnames(
            None,
            "localho.st",
            PublicHostnameStrategy::Standard,
            "app-production",
        )
        .is_empty());
    }

    #[test]
    fn public_compose_entry_routes_each_port_through_its_own_host_mapping() {
        use temps_entities::deployment_containers::{ContainerPortBinding, ContainerPortBindings};
        let mut container = route_test_container(1, Some("trawl"), 3000);
        container.host_port = Some(13_000);
        container.port_bindings = Some(ContainerPortBindings(vec![
            ContainerPortBinding {
                container_port: 3000,
                host_port: 13_000,
            },
            ContainerPortBinding {
                container_port: 9222,
                host_port: 19_222,
            },
        ]));
        let route = |port: u16| ComposePublicPort {
            service: "trawl".to_string(),
            port,
            ..Default::default()
        };
        let address = |port: u16, runtime: &RuntimeContext| {
            build_public_compose_backend_entry(&container, None, &route(port), runtime)
                .map(|entry| entry.address)
        };

        let host = RuntimeContext::host();
        assert_eq!(address(3000, &host).as_deref(), Some("127.0.0.1:13000"));
        assert_eq!(address(9222, &host).as_deref(), Some("127.0.0.1:19222"));
        // An unpublished port must never fall back to another mapping.
        assert_eq!(address(4000, &host), None);

        let docker = RuntimeContext::docker();
        assert_eq!(address(9222, &docker).as_deref(), Some("container-1:9222"));

        let containers = [container.clone()];
        let selected = select_public_route_containers(&containers, Some(&route(9222))).unwrap();
        assert_eq!(selected.len(), 1);
    }

    #[test]
    fn public_compose_mapping_uses_docker_recorded_port_on_baremetal() {
        let mapping = ComposePublicPort {
            service: "web".to_string(),
            port: 80,
            published: Some(65535),
            health_check_path: None,
        };

        let addr = build_public_compose_backend_addr(
            "web",
            80,
            Some(15455),
            None,
            &mapping,
            &RuntimeContext::host(),
        );

        assert_eq!(addr.as_deref(), Some("127.0.0.1:15455"));
    }

    #[test]
    fn public_compose_mapping_uses_container_port_in_docker() {
        let mapping = ComposePublicPort {
            service: "web".to_string(),
            port: 80,
            published: Some(15455),
            health_check_path: None,
        };

        let addr = build_public_compose_backend_addr(
            "web",
            80,
            Some(15455),
            None,
            &mapping,
            &RuntimeContext::docker(),
        );

        assert_eq!(addr.as_deref(), Some("web:80"));
    }

    #[test]
    fn test_public_compose_mapping_docker_without_host_port_uses_container_port() {
        let mapping = ComposePublicPort {
            service: "web".to_string(),
            port: 80,
            published: None,
            health_check_path: None,
        };

        let addr = build_public_compose_backend_addr(
            "web",
            80,
            None,
            None,
            &mapping,
            &RuntimeContext::docker(),
        );

        assert_eq!(addr.as_deref(), Some("web:80"));
    }

    #[test]
    fn public_compose_mapping_uses_published_port_for_remote_node() {
        let mapping = ComposePublicPort {
            service: "web".to_string(),
            port: 80,
            published: Some(65535),
            health_check_path: None,
        };

        let addr = build_public_compose_backend_addr(
            "web",
            80,
            Some(15455),
            Some("10.100.0.5"),
            &mapping,
            &RuntimeContext::host(),
        );

        assert_eq!(addr.as_deref(), Some("10.100.0.5:15455"));
    }

    #[test]
    fn legacy_public_compose_mapping_uses_recorded_host_port() {
        let mapping = ComposePublicPort {
            service: "web".to_string(),
            port: 80,
            published: None,
            health_check_path: None,
        };

        let addr = build_public_compose_backend_addr(
            "web",
            80,
            Some(15455),
            None,
            &mapping,
            &RuntimeContext::host(),
        );

        assert_eq!(addr.as_deref(), Some("127.0.0.1:15455"));
    }

    #[test]
    fn public_compose_mapping_rejects_unmatched_target_port() {
        let mapping = ComposePublicPort {
            service: "web".to_string(),
            port: 8211,
            published: Some(8211),
            health_check_path: None,
        };

        assert_eq!(
            build_public_compose_backend_addr(
                "web",
                80,
                Some(15455),
                None,
                &mapping,
                &RuntimeContext::host(),
            ),
            None
        );
    }

    #[test]
    fn public_compose_mapping_requires_discovered_host_port_off_docker_network() {
        let mapping = ComposePublicPort {
            service: "web".to_string(),
            port: 80,
            published: Some(8211),
            health_check_path: None,
        };

        let addr = build_public_compose_backend_addr(
            "web",
            80,
            None,
            None,
            &mapping,
            &RuntimeContext::host(),
        );

        assert_eq!(addr, None);
    }

    #[test]
    fn test_build_container_backend_addr_remote_with_host_port() {
        // Remote containers always use private_address:host_port
        let addr = build_container_backend_addr(
            "my-app",
            3000,
            Some(8080),
            Some("10.100.0.5"),
            &RuntimeContext::host(),
        );
        assert_eq!(addr, "10.100.0.5:8080");
    }

    #[test]
    fn test_build_container_backend_addr_remote_without_host_port() {
        // When host_port is None, remote falls back to container_port
        let addr = build_container_backend_addr(
            "my-app",
            3000,
            None,
            Some("10.100.0.5"),
            &RuntimeContext::host(),
        );
        assert_eq!(addr, "10.100.0.5:3000");
    }

    #[test]
    fn test_build_container_backend_addr_remote_brackets_ipv6() {
        // Regression guard: a bare "{ip}:{port}" is unparsable for an IPv6
        // node's private address -- nothing marks where the address ends
        // and the port begins. The proxy must dial "[fc00::1]:8080", not
        // "fc00::1:8080" (which parses as a different, wrong IPv6 address).
        let addr = build_container_backend_addr(
            "my-app",
            3000,
            Some(8080),
            Some("fc00::1"),
            &RuntimeContext::host(),
        );
        assert_eq!(addr, "[fc00::1]:8080");
    }

    #[test]
    fn test_build_container_backend_addr_remote_ignores_deployment_mode() {
        // Remote address should be the same regardless of deployment mode
        let addr_docker = build_container_backend_addr(
            "my-app",
            3000,
            Some(8080),
            Some("10.100.0.5"),
            &RuntimeContext::docker(),
        );
        let addr_baremetal = build_container_backend_addr(
            "my-app",
            3000,
            Some(8080),
            Some("10.100.0.5"),
            &RuntimeContext::host(),
        );

        assert_eq!(addr_docker, addr_baremetal);
        assert_eq!(addr_docker, "10.100.0.5:8080");
    }

    // ========================================================================
    // First-load readiness (has_loaded / wait_until_loaded)
    // ========================================================================

    #[test]
    fn test_has_loaded_is_false_before_first_load() {
        let db = Arc::new(sea_orm::DatabaseConnection::Disconnected);
        let table = CachedPeerTable::new(db);
        assert!(
            !table.has_loaded(),
            "a freshly constructed table must report not-loaded (generation 0)"
        );
    }

    #[tokio::test]
    async fn test_wait_until_loaded_times_out_when_never_loaded() {
        let db = Arc::new(sea_orm::DatabaseConnection::Disconnected);
        let table = CachedPeerTable::new(db);
        // Never loaded → must return false within the (short) timeout.
        let loaded = table
            .wait_until_loaded(std::time::Duration::from_millis(50))
            .await;
        assert!(!loaded, "wait_until_loaded must report false on timeout");
    }

    #[tokio::test]
    async fn test_wait_until_loaded_returns_immediately_when_already_loaded() {
        let db = Arc::new(sea_orm::DatabaseConnection::Disconnected);
        let table = CachedPeerTable::new(db);
        // Simulate a completed load by bumping the generation directly.
        table
            .generation
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        assert!(table.has_loaded());
        let loaded = table
            .wait_until_loaded(std::time::Duration::from_secs(5))
            .await;
        assert!(loaded, "already-loaded table must report true immediately");
    }

    #[tokio::test]
    async fn test_wait_until_loaded_wakes_on_generation_bump() {
        let db = Arc::new(sea_orm::DatabaseConnection::Disconnected);
        let table = Arc::new(CachedPeerTable::new(db));
        let waiter = table.clone();
        let handle = tokio::spawn(async move {
            waiter
                .wait_until_loaded(std::time::Duration::from_secs(5))
                .await
        });
        // Give the waiter a moment to arm its Notified future, then simulate a
        // load completing (bump generation + notify), mirroring load_routes().
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        table
            .generation
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        table.generation_changed.notify_waiters();

        let loaded = handle.await.expect("waiter task panicked");
        assert!(loaded, "waiter must wake and report loaded after a bump");
    }
}

#[cfg(test)]
mod unavailable_route_tests {
    use super::CachedPeerTable;
    use crate::test_utils::TestDBMockOperations;
    use sea_orm::{ActiveModelTrait, Set};
    use temps_database::test_utils::TestDatabase;
    use temps_entities::{custom_routes, environment_domains};

    // ── Hosts without a live backend (issue #1334) ────────────────────
    //
    // A deployment whose containers are all down used to vanish from the
    // route table, so the proxy fell back to the console and answered the
    // app's hostname with the console SPA and HTTP 200. These hosts must stay
    // unroutable but be reported through `get_unavailable_route`.

    /// Isolated database, or `None` when Docker is unavailable.
    async fn database_or_skip() -> Option<TestDatabase> {
        match TestDatabase::with_migrations().await {
            Ok(database) => Some(database),
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) =>
            {
                eprintln!("Skipping unavailable-route test: Docker runtime unavailable: {error}");
                None
            }
            Err(error) => panic!("Could not create isolated test database: {error}"),
        }
    }

    /// One container row for `deployment_id` in `status`.
    async fn insert_container(
        test_db: &TestDBMockOperations,
        deployment_id: i32,
        status: &str,
        host_port: Option<i32>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use temps_entities::deployment_containers;
        deployment_containers::ActiveModel {
            deployment_id: Set(deployment_id),
            container_id: Set(format!("container-{status}-{deployment_id}")),
            container_name: Set(format!("container-{status}-{deployment_id}")),
            container_port: Set(3000),
            host_port: Set(host_port),
            image_name: Set(Some("test-image:latest".to_string())),
            status: Set(Some(status.to_string())),
            deployed_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(test_db.db.as_ref())
        .await?;
        Ok(())
    }

    async fn insert_environment_domain(
        test_db: &TestDBMockOperations,
        environment_id: i32,
        domain: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        environment_domains::ActiveModel {
            domain: Set(domain.to_string()),
            environment_id: Set(environment_id),
            ..Default::default()
        }
        .insert(test_db.db.as_ref())
        .await?;
        Ok(())
    }

    /// Assert `host` is not routable and is reported unavailable for exactly
    /// this project/environment/deployment.
    fn assert_unavailable(
        route_table: &CachedPeerTable,
        host: &str,
        ids: (i32, i32, i32),
        reason: crate::route_table::UnavailableReason,
    ) {
        assert!(
            route_table.get_route_by_host(host).is_none()
                && route_table.get_route(host).is_none()
                && route_table.resolve_route_for_sni(host).is_none(),
            "{host} has no live backend and must not be routable"
        );
        let unavailable = route_table
            .get_unavailable_route(host)
            .unwrap_or_else(|| panic!("{host} must be reported as unavailable"));
        assert_eq!(
            (
                unavailable.project.id,
                unavailable.environment.id,
                unavailable.deployment.id
            ),
            ids,
            "{host} must be attributed to its own project/environment/deployment"
        );
        assert_eq!(unavailable.reason, reason, "{host}");
    }

    #[tokio::test]
    async fn exited_only_container_reports_app_hosts_unavailable(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crate::route_table::UnavailableReason;
        use temps_entities::environments;

        let Some(database) = database_or_skip().await else {
            return Ok(());
        };
        let test_db = TestDBMockOperations::new(database.db.clone()).await?;
        let (project, environment, deployment) = test_db
            .create_test_project_with_domain("exited-app.example.com")
            .await?;
        environments::ActiveModel {
            id: Set(environment.id),
            subdomain: Set("exited-app-production".to_string()),
            ..Default::default()
        }
        .update(test_db.db.as_ref())
        .await?;
        // Docker reported the container as exited (`docker stop` or a crash).
        insert_container(&test_db, deployment.id, "exited", Some(9700)).await?;
        insert_environment_domain(&test_db, environment.id, "exited-app.preview.example.com")
            .await?;

        let route_table = CachedPeerTable::new(test_db.db.clone());
        route_table.load_routes().await?;

        let ids = (project.id, environment.id, deployment.id);
        // Section 1 (environment domain), section 4 (the environment's own
        // hostname and its internal name).
        for host in [
            "exited-app.preview.example.com".to_string(),
            "exited-app-production".to_string(),
            format!("production.{}.temps.local", project.slug),
        ] {
            assert_unavailable(&route_table, &host, ids, UnavailableReason::NoLiveBackend);
        }
        assert!(
            route_table
                .get_unavailable_route("unknown.example.com")
                .is_none(),
            "a host no project owns is not an application outage"
        );

        Ok(())
    }

    #[tokio::test]
    async fn host_mode_container_without_published_port_is_unavailable(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crate::route_table::UnavailableReason;

        let Some(database) = database_or_skip().await else {
            return Ok(());
        };
        let test_db = TestDBMockOperations::new(database.db.clone()).await?;
        let (project, environment, deployment) = test_db
            .create_test_project_with_domain("no-port.example.com")
            .await?;
        // Still "running" in the database, but the health monitor cleared
        // the host port because Docker no longer publishes one.
        insert_container(&test_db, deployment.id, "running", None).await?;
        insert_environment_domain(&test_db, environment.id, "no-port.preview.example.com").await?;

        // `CachedPeerTable::new` runs in Host execution mode, where a
        // container is only reachable through its published port.
        let route_table = CachedPeerTable::new(test_db.db.clone());
        route_table.load_routes().await?;

        assert_unavailable(
            &route_table,
            "no-port.preview.example.com",
            (project.id, environment.id, deployment.id),
            UnavailableReason::NoLiveBackend,
        );

        Ok(())
    }

    #[tokio::test]
    async fn paused_deployment_hosts_are_unavailable() -> Result<(), Box<dyn std::error::Error>> {
        use crate::route_table::UnavailableReason;
        use temps_entities::deployments;

        let Some(database) = database_or_skip().await else {
            return Ok(());
        };
        let test_db = TestDBMockOperations::new(database.db.clone()).await?;
        let (project, environment, deployment) = test_db
            .create_test_project_with_domain("paused-app.example.com")
            .await?;
        // Even a container row that still says "running" is not routed
        // while the deployment is paused.
        insert_container(&test_db, deployment.id, "running", Some(9701)).await?;
        deployments::ActiveModel {
            id: Set(deployment.id),
            state: Set("paused".to_string()),
            ..Default::default()
        }
        .update(test_db.db.as_ref())
        .await?;
        insert_environment_domain(&test_db, environment.id, "paused-app.preview.example.com")
            .await?;

        let route_table = CachedPeerTable::new(test_db.db.clone());
        route_table.load_routes().await?;

        assert_unavailable(
            &route_table,
            "paused-app.preview.example.com",
            (project.id, environment.id, deployment.id),
            UnavailableReason::Paused,
        );

        Ok(())
    }

    #[tokio::test]
    async fn sleeping_on_demand_environment_is_left_to_the_wake_path(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use temps_entities::environments;

        let Some(database) = database_or_skip().await else {
            return Ok(());
        };
        let test_db = TestDBMockOperations::new(database.db.clone()).await?;
        let (_project, environment, deployment) = test_db
            .create_test_project_with_domain("sleeping-app.example.com")
            .await?;
        environments::ActiveModel {
            id: Set(environment.id),
            sleeping: Set(true),
            ..Default::default()
        }
        .update(test_db.db.as_ref())
        .await?;
        insert_container(&test_db, deployment.id, "exited", Some(9702)).await?;
        insert_environment_domain(&test_db, environment.id, "sleeping-app.preview.example.com")
            .await?;

        let route_table = CachedPeerTable::new(test_db.db.clone());
        let sleeping = route_table.load_routes().await?;

        let host = "sleeping-app.preview.example.com";
        assert!(route_table.get_route_by_host(host).is_none());
        assert!(
            route_table.get_unavailable_route(host).is_none(),
            "a sleeping environment must keep waking on request, not answer 503"
        );
        assert!(
            sleeping
                .iter()
                .any(|entry| entry.domain == host && entry.environment_id == environment.id),
            "the sleeping environment must still be handed to the wake path: {sleeping:?}"
        );

        Ok(())
    }

    #[tokio::test]
    async fn live_route_always_wins_over_an_unavailable_entry(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crate::route_table::UnavailableReason;

        let Some(database) = database_or_skip().await else {
            return Ok(());
        };
        let test_db = TestDBMockOperations::new(database.db.clone()).await?;
        let (project, environment, deployment) = test_db
            .create_test_project_with_domain("down-app.example.com")
            .await?;
        insert_container(&test_db, deployment.id, "exited", Some(9703)).await?;
        for domain in [
            "shared.example.com",
            "app.wild.example.com",
            "only-down.example.com",
        ] {
            insert_environment_domain(&test_db, environment.id, domain).await?;
        }
        // Operator routes that resolve two of those hosts to a live upstream:
        // one exact, one through a wildcard.
        for domain in ["shared.example.com", "*.wild.example.com"] {
            custom_routes::ActiveModel {
                domain: Set(domain.to_string()),
                host: Set("localhost".to_string()),
                port: Set(8080),
                enabled: Set(true),
                ..Default::default()
            }
            .insert(test_db.db.as_ref())
            .await?;
        }

        let route_table = CachedPeerTable::new(test_db.db.clone());
        route_table.load_routes().await?;

        for host in ["shared.example.com", "app.wild.example.com"] {
            assert!(
                route_table.get_route_by_host(host).is_some(),
                "{host} has a live route"
            );
            assert!(
                route_table.get_unavailable_route(host).is_none(),
                "{host} has a live route and must not be reported unavailable"
            );
        }
        assert_unavailable(
            &route_table,
            "only-down.example.com",
            (project.id, environment.id, deployment.id),
            UnavailableReason::NoLiveBackend,
        );

        Ok(())
    }
}

#[cfg(test)]
mod route_generation_tests {
    // ── One authoritative route generation (issue #1356) ──────────────
    //
    // `route_generation` is the target the worker completion gate waits for
    // every node to ACK, and nodes ACK what the route-sync endpoint of
    // `temps serve` gave them. The split-mode `temps proxy` used to write the
    // same row from its own reload counter, so the target could be a number
    // no worker would ever be given.

    use super::{
        adopt_claimed_generation, claim_route_generation, claim_route_generation_within,
        CachedPeerTable, RouteGenerationError, RouteGenerationRole, ROUTE_GENERATION_CLAIM_TIMEOUT,
    };
    use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
    use std::collections::HashSet;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;
    use temps_database::test_utils::TestDatabase;

    /// Isolated database, or `None` when Docker is unavailable.
    async fn database_or_skip() -> Option<TestDatabase> {
        match TestDatabase::with_migrations().await {
            Ok(database) => Some(database),
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) =>
            {
                eprintln!("Skipping route generation test: Docker runtime unavailable: {error}");
                None
            }
            Err(error) => panic!("Could not create isolated test database: {error}"),
        }
    }

    async fn persisted(db: &DatabaseConnection) -> Option<i64> {
        db.query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT current FROM route_generation WHERE id = 1".to_string(),
        ))
        .await
        .expect("read route_generation")
        .map(|row| row.try_get::<i64>("", "current").expect("current column"))
    }

    async fn execute(db: &DatabaseConnection, sql: &str) {
        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            sql.to_string(),
        ))
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }

    #[test]
    fn adopted_generations_strictly_increase_and_never_fall_below_a_claim() {
        let counter = AtomicU64::new(0);
        // The normal case: the claim is ahead, and is used as-is.
        assert_eq!(adopt_claimed_generation(&counter, 41), 41);
        assert_eq!(adopt_claimed_generation(&counter, 42), 42);
        // A claim at or below the current value still advances by one, so
        // the reload wakes its waiters and never repeats a generation.
        assert_eq!(adopt_claimed_generation(&counter, 42), 43);
        assert_eq!(adopt_claimed_generation(&counter, 7), 44);
        // A claim far ahead (a restored database) is adopted.
        assert_eq!(adopt_claimed_generation(&counter, 1_000), 1_000);
        // Saturates instead of wrapping to 0 ("never loaded").
        let full = AtomicU64::new(u64::MAX);
        assert_eq!(adopt_claimed_generation(&full, 3), u64::MAX);
    }

    #[test]
    fn concurrent_adoption_never_hands_out_a_generation_twice() {
        let counter = Arc::new(AtomicU64::new(0));
        let handles: Vec<_> = (0..8u64)
            .map(|thread| {
                let counter = counter.clone();
                std::thread::spawn(move || {
                    (0..500u64)
                        .map(|i| adopt_claimed_generation(&counter, (i * 8 + thread) / 3))
                        .collect::<Vec<u64>>()
                })
            })
            .collect();
        let mut seen = HashSet::new();
        for handle in handles {
            let values = handle.join().expect("adopting thread");
            assert!(
                values.windows(2).all(|pair| pair[0] < pair[1]),
                "one caller's generations must strictly increase"
            );
            for value in values {
                assert!(
                    seen.insert(value),
                    "generation {value} was handed out twice"
                );
            }
        }
        assert_eq!(seen.len(), 8 * 500);
    }

    #[test]
    fn route_generation_role_defaults_to_authoritative_and_can_be_made_local() {
        let table = CachedPeerTable::new(Arc::new(DatabaseConnection::Disconnected));
        assert_eq!(
            table.route_generation_role(),
            RouteGenerationRole::Authoritative
        );
        table.set_route_generation_role(RouteGenerationRole::Local);
        assert_eq!(table.route_generation_role(), RouteGenerationRole::Local);
    }

    /// A restarted control plane continues the persisted route generation
    /// instead of starting again at 1 below every agent's last ACK, and the
    /// row always equals the generation the snapshot endpoint serves.
    #[tokio::test]
    async fn authoritative_generation_continues_across_restarts() {
        let Some(database) = database_or_skip().await else {
            return;
        };
        let db = database.db.clone();

        let first = CachedPeerTable::new(db.clone());
        first.load_routes().await.expect("first load");
        first.load_routes().await.expect("second load");
        assert_eq!(first.current_generation(), 2);
        assert_eq!(persisted(&db).await, Some(2));

        // A previous process got further than this one ever will on its own.
        execute(&db, "UPDATE route_generation SET current = 41 WHERE id = 1").await;
        let restarted = CachedPeerTable::new(db.clone());
        restarted.load_routes().await.expect("load after restart");
        assert_eq!(restarted.current_generation(), 42);
        assert_eq!(persisted(&db).await, Some(42));
        restarted.load_routes().await.expect("reload");
        assert_eq!(restarted.current_generation(), 43);
        assert_eq!(persisted(&db).await, Some(43));

        // A database restored below the running process never takes the
        // numbering backwards: the next claim starts above both.
        execute(&db, "UPDATE route_generation SET current = 3 WHERE id = 1").await;
        restarted.load_routes().await.expect("reload after restore");
        assert_eq!(restarted.current_generation(), 44);
        assert_eq!(persisted(&db).await, Some(44));
    }

    /// The split-mode regression: a `temps proxy` that reloads more often
    /// than `temps serve` must not move the completion gate's target.
    #[tokio::test]
    async fn a_local_table_never_moves_the_persisted_generation() {
        let Some(database) = database_or_skip().await else {
            return;
        };
        let db = database.db.clone();

        let serve = CachedPeerTable::new(db.clone());
        let proxy = CachedPeerTable::new(db.clone());
        proxy.set_route_generation_role(RouteGenerationRole::Local);

        serve.load_routes().await.expect("serve load");
        assert_eq!(serve.current_generation(), 1);
        for _ in 0..5 {
            proxy.load_routes().await.expect("proxy load");
        }
        assert_eq!(proxy.current_generation(), 5, "the proxy still counts");
        assert_eq!(
            persisted(&db).await,
            Some(1),
            "the gate's target is still the generation serve gave the workers"
        );

        serve.load_routes().await.expect("serve reload");
        assert_eq!(serve.current_generation(), 2);
        assert_eq!(persisted(&db).await, Some(2));
        // A proxy restart neither continues from nor writes the row.
        let restarted_proxy = CachedPeerTable::new(db.clone());
        restarted_proxy.set_route_generation_role(RouteGenerationRole::Local);
        restarted_proxy.load_routes().await.expect("proxy restart");
        assert_eq!(restarted_proxy.current_generation(), 1);
        assert_eq!(persisted(&db).await, Some(2));
    }

    /// Concurrent claimers (two control-plane replicas, or a racing reload)
    /// each get a distinct generation, and the row ends at the highest one.
    #[tokio::test]
    async fn concurrent_claims_never_collide_or_go_backwards() {
        let Some(database) = database_or_skip().await else {
            return;
        };
        let db = database.db.clone();

        let tasks: Vec<_> = (0..32u64)
            .map(|i| {
                let db = db.clone();
                tokio::spawn(async move {
                    let mut claimed = Vec::new();
                    for _ in 0..5 {
                        // Stale and fresh in-memory values alike.
                        claimed.push(
                            claim_route_generation(db.as_ref(), i % 4)
                                .await
                                .expect("claim"),
                        );
                    }
                    claimed
                })
            })
            .collect();
        let mut seen = HashSet::new();
        for task in tasks {
            let claimed = task.await.expect("claim task");
            assert!(
                claimed.windows(2).all(|pair| pair[0] < pair[1]),
                "one claimer's generations must strictly increase: {claimed:?}"
            );
            for value in claimed {
                assert!(seen.insert(value), "generation {value} was claimed twice");
            }
        }
        assert_eq!(seen.len(), 32 * 5);
        let highest = seen.iter().copied().max().expect("claims");
        assert_eq!(persisted(&db).await, Some(highest as i64));

        // Two authoritative tables sharing the row interleave without
        // repeating a number.
        let a = CachedPeerTable::new(db.clone());
        let b = CachedPeerTable::new(db.clone());
        a.load_routes().await.expect("a");
        b.load_routes().await.expect("b");
        a.load_routes().await.expect("a again");
        assert_eq!(b.current_generation(), highest + 2);
        assert_eq!(a.current_generation(), highest + 3);
        assert_eq!(persisted(&db).await, Some(highest as i64 + 3));
    }

    /// A reload whose claim fails still advances (it must wake waiters) and
    /// leaves the row alone; the next successful claim starts above it, so
    /// neither counter ever repeats or lowers a generation.
    #[tokio::test]
    async fn a_failed_claim_numbers_locally_and_the_next_claim_starts_above_it() {
        let Some(database) = database_or_skip().await else {
            return;
        };
        let db = database.db.clone();

        let table = CachedPeerTable::new(db.clone());
        table.load_routes().await.expect("load");
        assert_eq!(table.current_generation(), 1);

        execute(&db, "DELETE FROM route_generation WHERE id = 1").await;
        let missing = claim_route_generation(db.as_ref(), 1)
            .await
            .expect_err("no row to claim from");
        assert!(
            missing
                .to_string()
                .contains("route_generation singleton row (id = 1)"),
            "{missing}"
        );
        table.load_routes().await.expect("load without the row");
        table
            .load_routes()
            .await
            .expect("second load without the row");
        assert_eq!(table.current_generation(), 3);
        assert_eq!(persisted(&db).await, None, "nothing re-creates the row");

        execute(
            &db,
            "INSERT INTO route_generation (id, current) VALUES (1, 0)",
        )
        .await;
        table.load_routes().await.expect("load with the row back");
        assert_eq!(table.current_generation(), 4);
        assert_eq!(persisted(&db).await, Some(4));
    }

    /// A stalled database cannot hold a reload's waiters asleep: the claim
    /// gives up within its bound, Postgres cancels it so it never commits
    /// behind the reload's back, and the reload numbers itself locally.
    #[tokio::test]
    async fn a_stalled_claim_gives_up_without_committing_and_the_reload_still_advances() {
        use sea_orm::TransactionTrait;
        let Some(database) = database_or_skip().await else {
            return;
        };
        let db = database.db.clone();

        let table = CachedPeerTable::new(db.clone());
        table.load_routes().await.expect("load");
        assert_eq!(table.current_generation(), 1);

        // Another session holds the row, so every claim waits on its lock.
        let blocker = db.begin().await.expect("blocking transaction");
        blocker
            .execute(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT current FROM route_generation WHERE id = 1 FOR UPDATE".to_string(),
            ))
            .await
            .expect("lock the row");

        let started = std::time::Instant::now();
        let error =
            claim_route_generation_within(db.as_ref(), 1, std::time::Duration::from_millis(200))
                .await
                .expect_err("a claim behind a held lock gives up");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "the claim must give up near its bound, took {:?}",
            started.elapsed()
        );
        assert!(
            matches!(
                error,
                RouteGenerationError::Claim { previous: 1, .. }
                    | RouteGenerationError::Timeout { previous: 1, .. }
            ),
            "{error:?}"
        );

        let started = std::time::Instant::now();
        table.load_routes().await.expect("load during the stall");
        assert!(
            started.elapsed() < ROUTE_GENERATION_CLAIM_TIMEOUT + std::time::Duration::from_secs(3),
            "the reload must not wait on the database beyond the claim bound, took {:?}",
            started.elapsed()
        );
        assert_eq!(table.current_generation(), 2, "numbered locally");

        blocker.rollback().await.expect("release the row");
        // Neither cancelled claim committed once the lock was released.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(persisted(&db).await, Some(1));

        table.load_routes().await.expect("load after the stall");
        assert_eq!(table.current_generation(), 3);
        assert_eq!(persisted(&db).await, Some(3));
    }

    /// Once the `UPDATE` has returned a generation, the reload publishes it
    /// whatever happens to the commit: a commit that succeeded with a lost
    /// reply must not leave the row ahead of every worker, and one that
    /// failed leaves the row below the published value, which the next claim
    /// starts above.
    #[tokio::test]
    async fn a_claimed_generation_is_published_even_when_its_commit_fails() {
        let Some(database) = database_or_skip().await else {
            return;
        };
        let db = database.db.clone();
        execute(
            &db,
            "UPDATE route_generation SET current = 100 WHERE id = 1",
        )
        .await;
        // Fails every transaction that changed the row, at COMMIT time.
        execute(
            &db,
            "CREATE FUNCTION fail_route_generation_commit() RETURNS trigger \
             LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'synthetic commit failure'; END $$",
        )
        .await;
        execute(
            &db,
            "CREATE CONSTRAINT TRIGGER fail_route_generation_commit AFTER UPDATE ON \
             route_generation DEFERRABLE INITIALLY DEFERRED FOR EACH ROW \
             EXECUTE FUNCTION fail_route_generation_commit()",
        )
        .await;

        let claimed = claim_route_generation(db.as_ref(), 0)
            .await
            .expect("the value the UPDATE returned is used despite the failed commit");
        assert_eq!(claimed, 101);
        assert_eq!(persisted(&db).await, Some(100), "the commit rolled back");

        let table = CachedPeerTable::new(db.clone());
        table
            .load_routes()
            .await
            .expect("load with failing commits");
        assert_eq!(table.current_generation(), 101);

        execute(
            &db,
            "DROP TRIGGER fail_route_generation_commit ON route_generation",
        )
        .await;
        table
            .load_routes()
            .await
            .expect("load once commits succeed");
        assert_eq!(table.current_generation(), 102);
        assert_eq!(persisted(&db).await, Some(102));
    }

    /// A number published without being persisted (here: a lost commit,
    /// then a restart) is never issued again once a node has acknowledged
    /// it, so a worker holding it cannot mistake a new snapshot for its own.
    #[tokio::test]
    async fn a_restarted_process_never_reissues_a_generation_a_node_acknowledged() {
        let Some(database) = database_or_skip().await else {
            return;
        };
        let db = database.db.clone();
        execute(
            &db,
            "UPDATE route_generation SET current = 100 WHERE id = 1",
        )
        .await;
        execute(
            &db,
            "INSERT INTO nodes (name, token_hash, address, private_address, role, status, \
             labels, capacity) VALUES ('worker-1', 'synthetic-hash', '127.0.0.1', '10.0.0.2', \
             'worker', 'active', '{}', '{}')",
        )
        .await;
        // The previous process published 101, its commit was lost, and the
        // worker acknowledged 101 before the process restarted.
        execute(
            &db,
            "INSERT INTO node_route_state (node_id, applied_generation, health) \
             SELECT id, 101, 'healthy' FROM nodes WHERE name = 'worker-1'",
        )
        .await;

        let restarted = CachedPeerTable::new(db.clone());
        restarted
            .load_routes()
            .await
            .expect("first load after restart");
        assert_eq!(restarted.current_generation(), 102);
        assert_eq!(persisted(&db).await, Some(102));
    }
}
