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
use parking_lot::RwLock;
use sea_orm::{DatabaseConnection, EntityTrait};
use sqlx::postgres::{PgListener, PgPool};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use temps_core::DeploymentMode;
use temps_entities::custom_routes::RouteType;
use temps_entities::{deployments, environments, nodes, projects};
use tracing::{debug, error, info, warn};

/// Look up the private address for a container's node, caching results.
/// Returns None for local containers (node_id is None).
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
        let addr = node.private_address.clone();
        nodes_cache.insert(node_id, addr.clone());
        Some(addr)
    } else {
        warn!(
            node_id,
            "Node not found for container routing, treating as local"
        );
        None
    }
}

/// Build a `BackendEntry` for a container, including its network address and metadata.
fn build_backend_entry(
    container: &temps_entities::deployment_containers::Model,
    node_private_address: Option<&str>,
) -> BackendEntry {
    let address = build_container_backend_addr(
        &container.container_name,
        container.container_port,
        container.host_port,
        node_private_address,
    );
    BackendEntry {
        address,
        container_id: Some(container.container_id.clone()),
        container_name: Some(container.container_name.clone()),
    }
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
) -> String {
    if let Some(private_addr) = node_private_address {
        // Remote node: use the node's private/WireGuard IP with host_port
        let port = host_port.unwrap_or(container_port);
        format!("{}:{}", private_addr, port)
    } else {
        // Local node: use existing logic
        let (host, port) = DeploymentMode::get_effective_host_port(
            container_name,
            container_port as u16,
            host_port.unwrap_or(container_port) as u16,
        );
        format!("{}:{}", host, port)
    }
}

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

pub struct CachedPeerTable {
    /// Exact hostname -> RouteInfo for HTTP routes (route_type = 'http')
    /// Used for matching on HTTP Host header (Layer 7)
    http_routes: Arc<RwLock<HashMap<String, RouteInfo>>>,

    /// Exact hostname -> RouteInfo for TLS routes (route_type = 'tls')
    /// Used for matching on TLS SNI hostname (Layer 4/5)
    tls_routes: Arc<RwLock<HashMap<String, RouteInfo>>>,

    /// Wildcard patterns for HTTP routes
    http_wildcards: Arc<RwLock<WildcardMatcher>>,

    /// Wildcard patterns for TLS routes
    tls_wildcards: Arc<RwLock<WildcardMatcher>>,

    /// Legacy routes map (for backward compatibility during transition)
    /// Contains all environment domains, project custom domains, etc.
    routes: Arc<RwLock<HashMap<String, RouteInfo>>>,

    /// Database connection for loading routes
    db: Arc<DatabaseConnection>,

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
    /// before returning the current snapshot. Restart-safe: a CP
    /// restart resets the counter; agents detect this (current < their
    /// applied) and re-fetch a fresh snapshot.
    generation: std::sync::atomic::AtomicU64,

    /// Notify hookup so long-poll handlers can sleep until the next
    /// generation bump rather than spinning. Awoken on every
    /// `load_routes()` success.
    generation_changed: Arc<tokio::sync::Notify>,
}

impl CachedPeerTable {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self {
            http_routes: Arc::new(RwLock::new(HashMap::new())),
            tls_routes: Arc::new(RwLock::new(HashMap::new())),
            http_wildcards: Arc::new(RwLock::new(WildcardMatcher::new())),
            tls_wildcards: Arc::new(RwLock::new(WildcardMatcher::new())),
            routes: Arc::new(RwLock::new(HashMap::new())),
            db,
            on_sleeping_callback: parking_lot::Mutex::new(None),
            on_reload_callback: parking_lot::Mutex::new(None),
            on_cert_eligible_callback: parking_lot::Mutex::new(None),
            generation: std::sync::atomic::AtomicU64::new(0),
            generation_changed: Arc::new(tokio::sync::Notify::new()),
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
        self.routes
            .read()
            .iter()
            .filter(|(_, r)| r.cert_eligible)
            .map(|(host, _)| host.clone())
            .collect()
    }

    /// Get route by HTTP Host header
    ///
    /// Used for route_type = 'http' routes.
    /// Checks exact matches first, then wildcard patterns.
    pub fn get_route_by_host(&self, host: &str) -> Option<RouteInfo> {
        // 1. Try exact match in HTTP routes
        if let Some(route) = self.http_routes.read().get(host) {
            return Some(route.clone());
        }

        // 2. Try wildcard match in HTTP wildcards
        if let Some(route) = self.http_wildcards.read().match_domain(host) {
            return Some(route.clone());
        }

        // 3. Fall back to legacy routes (for non-custom_routes entries)
        self.routes.read().get(host).cloned()
    }

    /// Get route by TLS SNI hostname
    ///
    /// Used for route_type = 'tls' routes.
    /// Checks exact matches first, then wildcard patterns.
    pub fn get_route_by_sni(&self, sni: &str) -> Option<RouteInfo> {
        // 1. Try exact match in TLS routes
        if let Some(route) = self.tls_routes.read().get(sni) {
            return Some(route.clone());
        }

        // 2. Try wildcard match in TLS wildcards
        if let Some(route) = self.tls_wildcards.read().match_domain(sni) {
            return Some(route.clone());
        }

        None
    }

    /// Resolve a hostname across every lookup strategy in the same order the
    /// proxy's `UpstreamResolver` uses: TLS/SNI exact+wildcard, then HTTP-host
    /// exact+wildcard, then the legacy `routes` map. Stable per-environment
    /// hostnames (env_domains, the env subdomain, the env preview alias) live in
    /// the legacy map, so the on-demand TLS gate (ADR-018 §2 second/third check)
    /// MUST consult all three — checking only `get_route_by_sni` would miss them
    /// and reject every certable host. O(1) per map, no I/O.
    pub fn resolve_route_for_sni(&self, sni: &str) -> Option<RouteInfo> {
        if let Some(route) = self.get_route_by_sni(sni) {
            return Some(route);
        }
        // get_route_by_host covers http_routes exact, http_wildcards, and the
        // legacy routes map (its step 3), which together hold the stable env
        // hostnames the on-demand gate cares about.
        self.get_route_by_host(sni)
    }

    /// Insert a route directly into the legacy routes map. Test/seed support for
    /// callers in other crates (e.g. the proxy's on-demand cert gate tests) that
    /// need a populated route table without standing up a database. Not used on
    /// the production load path — `load_routes` owns that.
    #[doc(hidden)]
    pub fn insert_route_for_test(&self, host: &str, route: RouteInfo) {
        self.routes.write().insert(host.to_string(), route);
    }

    /// Load all routes from the database into the cache with full models.
    /// This queries environment_domains, custom_routes, and project_custom_domains.
    /// Returns a list of sleeping on-demand environments that were skipped during route loading.
    pub async fn load_routes(&self) -> Result<Vec<SleepingEnvironmentEntry>, sea_orm::DbErr> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use temps_entities::{
            custom_routes, deployments, environment_domains, environments, project_custom_domains,
            settings,
        };

        let mut routes = HashMap::new();
        let mut sleeping_environments: Vec<SleepingEnvironmentEntry> = Vec::new();

        // Build entity caches as we go - only cache what we actually need for routing
        let mut projects_cache: HashMap<i32, Arc<projects::Model>> = HashMap::new();
        let mut environments_cache: HashMap<i32, Arc<environments::Model>> = HashMap::new();
        let mut deployments_cache: HashMap<i32, Arc<deployments::Model>> = HashMap::new();
        // Node cache: maps node_id -> private_address for multi-node routing
        let mut nodes_cache: HashMap<i32, String> = HashMap::new();

        // Fetch preview_domain from settings
        let preview_domain = settings::Entity::find()
            .one(self.db.as_ref())
            .await?
            .and_then(|s| {
                s.data
                    .get("preview_domain")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| "localho.st".to_string());

        debug!("Loaded preview_domain from settings: {}", preview_domain);

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
                        let containers = deployment_containers::Entity::find()
                            .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
                            .filter(deployment_containers::Column::DeletedAt.is_null())
                            .all(self.db.as_ref())
                            .await
                            .unwrap_or_default();

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
                            BackendType::StaticDir {
                                path: static_dir.clone(),
                            }
                        } else if !containers.is_empty() {
                            // Container deployment - proxy to containers
                            let mut backend_entries = Vec::with_capacity(containers.len());
                            for c in &containers {
                                let node_addr = resolve_node_private_address(
                                    c.node_id,
                                    &mut nodes_cache,
                                    self.db.as_ref(),
                                )
                                .await;
                                backend_entries.push(build_backend_entry(c, node_addr.as_deref()));
                            }
                            BackendType::Upstream {
                                backends: backend_entries,
                                round_robin_counter: Arc::new(AtomicUsize::new(0)),
                            }
                        } else {
                            // No backend available, skip this route
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
                        let containers = deployment_containers::Entity::find()
                            .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
                            .filter(deployment_containers::Column::DeletedAt.is_null())
                            .all(self.db.as_ref())
                            .await
                            .unwrap_or_default();

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
                            BackendType::StaticDir {
                                path: static_dir.clone(),
                            }
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
                                backend_entries.push(build_backend_entry(c, node_addr.as_deref()));
                            }
                            BackendType::Upstream {
                                backends: backend_entries,
                                round_robin_counter: Arc::new(AtomicUsize::new(0)),
                            }
                        } else {
                            // No backend available, skip this route
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
                    let full_domain = format!("{}.{}", main_url, preview_domain);
                    sleeping_environments.push(SleepingEnvironmentEntry {
                        domain: full_domain,
                        environment_id: env.id,
                        project_id: env.project_id,
                        deployment_id,
                        wake_timeout_seconds: wake_timeout,
                    });
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
                    let containers = deployment_containers::Entity::find()
                        .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
                        .filter(deployment_containers::Column::DeletedAt.is_null())
                        .all(self.db.as_ref())
                        .await
                        .unwrap_or_default();

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
                        BackendType::StaticDir {
                            path: static_dir.clone(),
                        }
                    } else if !containers.is_empty() {
                        // For compose deployments, the main route uses:
                        // 1. The first public port's service (if public_ports configured)
                        // 2. The first service (fallback for non-compose or no public_ports)
                        let is_compose = containers.iter().any(|c| c.service_name.is_some());
                        let (route_containers, override_port): (
                            Vec<&deployment_containers::Model>,
                            Option<u16>,
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
                                    if cs.is_empty() {
                                        // Fallback to first service
                                        let first_svc = containers
                                            .iter()
                                            .filter_map(|c| c.service_name.as_ref())
                                            .next()
                                            .cloned();
                                        (
                                            match first_svc {
                                                Some(ref svc) => containers
                                                    .iter()
                                                    .filter(|c| {
                                                        c.service_name.as_ref() == Some(svc)
                                                    })
                                                    .collect(),
                                                None => containers.iter().collect(),
                                            },
                                            None,
                                        )
                                    } else {
                                        (cs, Some(pp.port))
                                    }
                                }
                                None => {
                                    // No public ports configured — use first service
                                    let first_svc = containers
                                        .iter()
                                        .filter_map(|c| c.service_name.as_ref())
                                        .next()
                                        .cloned();
                                    (
                                        match first_svc {
                                            Some(ref svc) => containers
                                                .iter()
                                                .filter(|c| c.service_name.as_ref() == Some(svc))
                                                .collect(),
                                            None => containers.iter().collect(),
                                        },
                                        None,
                                    )
                                }
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
                            let mut entry = build_backend_entry(c, node_addr.as_deref());
                            // Override port if a public port is configured
                            if let Some(port) = override_port {
                                if let Some(colon_pos) = entry.address.rfind(':') {
                                    entry.address =
                                        format!("{}{}", &entry.address[..=colon_pos], port);
                                }
                            }
                            backend_entries.push(entry);
                        }
                        BackendType::Upstream {
                            backends: backend_entries,
                            round_robin_counter: Arc::new(AtomicUsize::new(0)),
                        }
                    } else {
                        // No backend available, skip this route
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
                    let full_domain = format!("{}.{}", main_url, preview_domain);
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
                        let public_ports: Vec<(String, u16)> = project
                            .and_then(|p| p.preset_config.as_ref())
                            .and_then(|pc| {
                                if let temps_entities::preset::PresetConfig::DockerCompose(cfg) = pc
                                {
                                    Some(
                                        cfg.public_ports
                                            .iter()
                                            .map(|pp| (pp.service.clone(), pp.port))
                                            .collect(),
                                    )
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

                            for (pub_service, pub_port) in &public_ports {
                                let svc_containers = match services.get(pub_service) {
                                    Some(c) => c,
                                    None => continue,
                                };

                                let mut svc_backends = Vec::with_capacity(svc_containers.len());
                                for c in svc_containers {
                                    // Override container_port with the public port for routing
                                    let node_addr = resolve_node_private_address(
                                        c.node_id,
                                        &mut nodes_cache,
                                        self.db.as_ref(),
                                    )
                                    .await;
                                    let mut entry = build_backend_entry(c, node_addr.as_deref());
                                    // Replace port in address with the public port
                                    if let Some(colon_pos) = entry.address.rfind(':') {
                                        entry.address =
                                            format!("{}{}", &entry.address[..=colon_pos], pub_port);
                                    }
                                    svc_backends.push(entry);
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
                                    // (`<service>-<env>.<preview>`) — one per
                                    // environment+service, certable (ADR-018 §2).
                                    cert_eligible: true,
                                };

                                // Route: {service}-{env_subdomain}.{preview_domain}
                                // DNS labels must be ≤63 chars, truncate if needed
                                let svc_label = format!("{}-{}", pub_service, main_url);
                                let svc_label = if svc_label.len() > 63 {
                                    svc_label[..63].trim_end_matches('-').to_string()
                                } else {
                                    svc_label
                                };
                                let svc_domain = format!("{}.{}", svc_label, preview_domain);
                                if let std::collections::hash_map::Entry::Vacant(e) =
                                    routes.entry(svc_domain.clone())
                                {
                                    let addresses: Vec<&str> =
                                        svc_backends.iter().map(|b| b.address.as_str()).collect();
                                    debug!(
                                        "Loaded compose public port route: {} -> {:?} (service={}, port={}, project={}, env={})",
                                        svc_domain, addresses, pub_service, pub_port, env.project_id, env.id
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
                    let containers = deployment_containers::Entity::find()
                        .filter(deployment_containers::Column::DeploymentId.eq(deployment_id))
                        .filter(deployment_containers::Column::DeletedAt.is_null())
                        .all(self.db.as_ref())
                        .await
                        .unwrap_or_default();

                    // Determine backend type: static directory or upstream containers
                    let backend = if let Some(static_dir) = &deployment.static_dir_location {
                        // Static deployment - serve from directory
                        BackendType::StaticDir {
                            path: static_dir.clone(),
                        }
                    } else if !containers.is_empty() {
                        // Container deployment - proxy to containers
                        let mut backend_entries = Vec::with_capacity(containers.len());
                        for c in &containers {
                            let node_addr = resolve_node_private_address(
                                c.node_id,
                                &mut nodes_cache,
                                self.db.as_ref(),
                            )
                            .await;
                            backend_entries.push(build_backend_entry(c, node_addr.as_deref()));
                        }
                        BackendType::Upstream {
                            backends: backend_entries,
                            round_robin_counter: Arc::new(AtomicUsize::new(0)),
                        }
                    } else {
                        // No backend available, skip this route
                        continue;
                    };

                    // Generate a fallback route using deployment slug if no other routes exist
                    // This ensures every active deployment is accessible
                    let fallback_domain = format!("{}.{}", deployment.slug, preview_domain);

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

        // Atomically replace all route tables
        let route_count = routes.len();
        let http_routes_count = http_routes_map.len();
        let tls_routes_count = tls_routes_map.len();
        let http_wildcards_count = http_wildcards_matcher.len();
        let tls_wildcards_count = tls_wildcards_matcher.len();

        // Replace legacy routes
        *self.routes.write() = routes;

        // Replace HTTP and TLS route caches
        *self.http_routes.write() = http_routes_map;
        *self.tls_routes.write() = tls_routes_map;
        *self.http_wildcards.write() = http_wildcards_matcher;
        *self.tls_wildcards.write() = tls_wildcards_matcher;

        if !sleeping_environments.is_empty() {
            info!(
                "Route table: {} sleeping on-demand environments skipped",
                sleeping_environments.len()
            );
        }

        info!(
            "Route table loaded with {} total entries ({} HTTP exact, {} TLS exact, {} HTTP wildcards, {} TLS wildcards)",
            route_count, http_routes_count, tls_routes_count, http_wildcards_count, tls_wildcards_count
        );
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

        debug!(
            "Found {} on-demand configs for idle tracking",
            on_demand_configs.len()
        );

        // Notify callback with sleeping environments and on-demand configs
        if let Some(callback) = self.on_sleeping_callback.lock().as_ref() {
            callback(sleeping_environments.clone(), on_demand_configs);
        }

        // Bump the in-memory generation and wake any long-poll waiters
        // on the routes-sync endpoint, plus anything waiting for the first
        // load via `wait_until_loaded`. Order matters: bump first, then
        // notify, so a wake-up that races with a subsequent fetch always
        // sees the new value. Do this BEFORE the DNS reconcile so readiness
        // waiters are released as soon as the in-memory maps are live —
        // they must never be gated on DNS work.
        let new_gen = self
            .generation
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
            + 1;
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
                .routes
                .read()
                .iter()
                .filter(|(_, r)| r.cert_eligible)
                .map(|(host, _)| host.clone())
                .collect();
            if !cert_hosts.is_empty() {
                tokio::spawn(async move {
                    callback(cert_hosts).await;
                });
            }
        }

        // Persist the new generation into the durable singleton so
        // `mark_deployment_complete` can wait until every active
        // worker's `node_route_state.applied_generation` reaches it.
        // Best-effort — a transient DB error here doesn't block the
        // route table itself, and the next successful reload will
        // overwrite the stale value.
        let new_gen_i64: i64 = new_gen.try_into().unwrap_or(i64::MAX);
        let stmt = sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            "UPDATE route_generation SET current = $1, updated_at = now() WHERE id = 1",
            [new_gen_i64.into()],
        );
        if let Err(e) = sea_orm::ConnectionTrait::execute(self.db.as_ref(), stmt).await {
            tracing::warn!(error = %e, "failed to persist route_generation");
        }

        Ok(sleeping_environments)
    }

    /// Get route information for a host (O(1) lookup)
    pub fn get_route(&self, host: &str) -> Option<RouteInfo> {
        self.routes.read().get(host).cloned()
    }

    /// Get current number of routes in the table
    pub fn len(&self) -> usize {
        self.routes.read().len()
    }

    /// Check if the route table is empty
    pub fn is_empty(&self) -> bool {
        self.routes.read().is_empty()
    }

    /// Check if any route in the table points to a specific deployment.
    ///
    /// Used by `mark_deployment_complete` to verify the proxy's in-memory route
    /// table has actually loaded the new deployment — not just that the DB row
    /// was written (which would always be true since we just wrote it).
    pub fn has_route_for_deployment(&self, deployment_id: i32) -> bool {
        let routes = self.routes.read();
        routes.values().any(|route| {
            route
                .deployment
                .as_ref()
                .is_some_and(|d| d.id == deployment_id)
        })
    }

    /// Snapshot of every `*.temps.local` route currently in the table.
    /// Returned as a flat `(host, RouteInfo)` vector cloned out of the
    /// `routes` map under a single read lock. Used by the internal
    /// route-sync endpoint to fan out the worker-side proxy table.
    ///
    /// Filters to the internal zone only. Custom-domain and preview
    /// routes are not part of this contract — workers don't need them
    /// (only the public edge proxy does).
    pub fn snapshot_internal_routes(&self) -> Vec<(String, RouteInfo)> {
        let routes = self.routes.read();
        routes
            .iter()
            .filter(|(host, _)| host.ends_with(".temps.local"))
            .map(|(h, r)| (h.clone(), r.clone()))
            .collect()
    }
}

#[temps_core::async_trait::async_trait]
impl temps_core::route_table::RouteTableRefresher for CachedPeerTable {
    async fn refresh_routes(&self) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        self.load_routes().await?;
        let count = self.len() + self.http_routes.read().len() + self.tls_routes.read().len();
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

    /// Mutex to serialize tests that mutate the DEPLOYMENT_MODE env var.
    /// Env vars are process-global, so parallel tests would race.
    static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        // In Docker mode, local containers use container_name:container_port
        unsafe { std::env::set_var("DEPLOYMENT_MODE", "docker") };
        let addr = build_container_backend_addr("my-app", 3000, Some(8080), None);
        unsafe { std::env::remove_var("DEPLOYMENT_MODE") };
        assert_eq!(addr, "my-app:3000");
    }

    #[test]
    fn test_build_container_backend_addr_local_baremetal() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        // In baremetal mode (default), local containers use 127.0.0.1:host_port
        unsafe { std::env::set_var("DEPLOYMENT_MODE", "baremetal") };
        let addr = build_container_backend_addr("my-app", 3000, Some(8080), None);
        unsafe { std::env::remove_var("DEPLOYMENT_MODE") };
        assert_eq!(addr, "127.0.0.1:8080");
    }

    #[test]
    fn test_build_container_backend_addr_remote_with_host_port() {
        // Remote containers always use private_address:host_port
        let addr = build_container_backend_addr("my-app", 3000, Some(8080), Some("10.100.0.5"));
        assert_eq!(addr, "10.100.0.5:8080");
    }

    #[test]
    fn test_build_container_backend_addr_remote_without_host_port() {
        // When host_port is None, remote falls back to container_port
        let addr = build_container_backend_addr("my-app", 3000, None, Some("10.100.0.5"));
        assert_eq!(addr, "10.100.0.5:3000");
    }

    #[test]
    fn test_build_container_backend_addr_remote_ignores_deployment_mode() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        // Remote address should be the same regardless of deployment mode
        unsafe { std::env::set_var("DEPLOYMENT_MODE", "docker") };
        let addr_docker =
            build_container_backend_addr("my-app", 3000, Some(8080), Some("10.100.0.5"));
        unsafe { std::env::set_var("DEPLOYMENT_MODE", "baremetal") };
        let addr_baremetal =
            build_container_backend_addr("my-app", 3000, Some(8080), Some("10.100.0.5"));
        unsafe { std::env::remove_var("DEPLOYMENT_MODE") };

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
