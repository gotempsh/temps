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
use sea_orm::DatabaseConnection;
use sqlx::postgres::{PgListener, PgPool};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use temps_entities::custom_routes::RouteType;
use temps_entities::{deployments, environments, projects};
use tracing::{debug, error, info, warn};

/// Backend type for a route
#[derive(Clone, Debug)]
pub enum BackendType {
    /// Proxy to backend addresses (containers)
    Upstream {
        /// Backend addresses for load balancing (e.g., ["127.0.0.1:8080", "127.0.0.1:8081"])
        addresses: Vec<String>,
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
    /// Get the next backend address using round-robin load balancing
    /// Returns None for StaticDir backends
    pub fn get_backend_addr(&self) -> Option<String> {
        match self {
            BackendType::Upstream {
                addresses,
                round_robin_counter,
            } => {
                if addresses.is_empty() {
                    return Some("127.0.0.1:8080".to_string()); // Fallback
                }

                if addresses.len() == 1 {
                    return Some(addresses[0].clone());
                }

                // Round-robin load balancing
                let index = round_robin_counter.fetch_add(1, Ordering::Relaxed) % addresses.len();
                Some(addresses[index].clone())
            }
            BackendType::StaticDir { .. } => None,
        }
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
}

impl RouteInfo {
    /// Get the next backend address using round-robin load balancing
    /// Returns fallback address if this is a static directory backend
    pub fn get_backend_addr(&self) -> String {
        self.backend
            .get_backend_addr()
            .unwrap_or_else(|| "127.0.0.1:8080".to_string())
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
        }
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

    /// Load all routes from the database into the cache with full models
    /// This queries environment_domains, custom_routes, and project_custom_domains
    pub async fn load_routes(&self) -> Result<(), sea_orm::DbErr> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use temps_entities::{
            custom_routes, deployments, environment_domains, environments, project_custom_domains,
            settings,
        };

        let mut routes = HashMap::new();

        // Build entity caches as we go - only cache what we actually need for routing
        let mut projects_cache: HashMap<i32, Arc<projects::Model>> = HashMap::new();
        let mut environments_cache: HashMap<i32, Arc<environments::Model>> = HashMap::new();
        let mut deployments_cache: HashMap<i32, Arc<deployments::Model>> = HashMap::new();

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
            // Fetch environment if not cached
            if !environments_cache.contains_key(&env_domain.environment_id) {
                if let Ok(Some(env)) = environments::Entity::find_by_id(env_domain.environment_id)
                    .one(self.db.as_ref())
                    .await
                {
                    environments_cache.insert(env.id, Arc::new(env));
                }
            }

            if let Some(environment) = environments_cache.get(&env_domain.environment_id) {
                if let Some(deployment_id) = environment.current_deployment_id {
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
                            let backend_addrs: Vec<String> = containers
                                .iter()
                                .map(|c| {
                                    format!("127.0.0.1:{}", c.host_port.unwrap_or(c.container_port))
                                })
                                .collect();
                            BackendType::Upstream {
                                addresses: backend_addrs,
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
                            },
                        );

                        match &backend {
                            BackendType::Upstream { addresses, .. } => {
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
                    addresses: vec![backend_addr.clone()],
                    round_robin_counter: Arc::new(AtomicUsize::new(0)),
                },
                redirect_to: None,
                status_code: None,
                project: None, // Custom routes don't have project context
                environment: None,
                deployment: None,
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
            // Fetch environment if not cached
            if !environments_cache.contains_key(&custom_domain.environment_id) {
                if let Ok(Some(env)) =
                    environments::Entity::find_by_id(custom_domain.environment_id)
                        .one(self.db.as_ref())
                        .await
                {
                    environments_cache.insert(env.id, Arc::new(env));
                }
            }

            if let Some(environment) = environments_cache.get(&custom_domain.environment_id) {
                if let Some(deployment_id) = environment.current_deployment_id {
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

                        // Determine backend type: static directory or upstream containers
                        let backend = if let Some(static_dir) = &deployment.static_dir_location {
                            // Static deployment - serve from directory
                            BackendType::StaticDir {
                                path: static_dir.clone(),
                            }
                        } else if !containers.is_empty() {
                            // Container deployment - proxy to containers
                            let backend_addrs: Vec<String> = containers
                                .iter()
                                .map(|c| {
                                    format!("127.0.0.1:{}", c.host_port.unwrap_or(c.container_port))
                                })
                                .collect();
                            BackendType::Upstream {
                                addresses: backend_addrs,
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
                            },
                        );

                        if let Some(ref redirect) = custom_domain.redirect_to {
                            debug!(
                                "Loaded custom domain with redirect: {} -> {} (status: {:?})",
                                custom_domain.domain, redirect, custom_domain.status_code
                            );
                        } else {
                            match &backend {
                                BackendType::Upstream { addresses, .. } => {
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
            .all(self.db.as_ref())
            .await?;

        debug!(
            "Section 4: Loading {} environments with main_url",
            all_envs.len()
        );

        for env in all_envs {
            if let Some(deployment_id) = env.current_deployment_id {
                let main_url = &env.subdomain;
                // Cache environment if not already cached
                environments_cache
                    .entry(env.id)
                    .or_insert_with(|| Arc::new(env.clone()));

                // Fetch deployment if not cached
                if !deployments_cache.contains_key(&deployment_id) {
                    if let Ok(Some(dep)) = deployments::Entity::find_by_id(deployment_id)
                        .one(self.db.as_ref())
                        .await
                    {
                        if dep.state == "completed" {
                            deployments_cache.insert(dep.id, Arc::new(dep));
                        }
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
                        // Container deployment - proxy to containers
                        let backend_addrs: Vec<String> = containers
                            .iter()
                            .map(|c| {
                                format!("127.0.0.1:{}", c.host_port.unwrap_or(c.container_port))
                            })
                            .collect();
                        BackendType::Upstream {
                            addresses: backend_addrs,
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
                            },
                        );
                        match &backend {
                            BackendType::Upstream { addresses, .. } => {
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
                            },
                        );
                        match &backend {
                            BackendType::Upstream { addresses, .. } => {
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

        // Get all environments with current_deployment_id
        let all_active_envs = environments::Entity::find()
            .filter(environments::Column::CurrentDeploymentId.is_not_null())
            .all(self.db.as_ref())
            .await?;

        for env in all_active_envs {
            // Cache environment if not already cached
            environments_cache
                .entry(env.id)
                .or_insert_with(|| Arc::new(env.clone()));

            if let Some(deployment_id) = env.current_deployment_id {
                // Fetch deployment if not cached
                if !deployments_cache.contains_key(&deployment_id) {
                    if let Ok(Some(dep)) = deployments::Entity::find_by_id(deployment_id)
                        .one(self.db.as_ref())
                        .await
                    {
                        if dep.state == "completed" {
                            deployments_cache.insert(dep.id, Arc::new(dep));
                        }
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
                        let backend_addrs: Vec<String> = containers
                            .iter()
                            .map(|c| {
                                format!("127.0.0.1:{}", c.host_port.unwrap_or(c.container_port))
                            })
                            .collect();
                        BackendType::Upstream {
                            addresses: backend_addrs,
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
                            },
                        );
                        match &backend {
                            BackendType::Upstream { addresses, .. } => {
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

        debug!(
            "Route table loaded with {} total entries ({} HTTP exact, {} TLS exact, {} HTTP wildcards, {} TLS wildcards)",
            route_count, http_routes_count, tls_routes_count, http_wildcards_count, tls_wildcards_count
        );
        Ok(())
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
}

/// Listens for PostgreSQL notifications and automatically reloads the route table
pub struct RouteTableListener {
    peer_table: Arc<CachedPeerTable>,
    database_url: String,
}

impl RouteTableListener {
    pub fn new(peer_table: Arc<CachedPeerTable>, database_url: String) -> Self {
        Self {
            peer_table,
            database_url,
        }
    }

    /// Start listening for route table changes
    /// This performs an initial load and then listens for PostgreSQL notifications
    pub async fn start_listening(self: Arc<Self>) -> anyhow::Result<()> {
        // Initial load
        debug!("Loading initial route table...");
        self.peer_table.load_routes().await?;
        debug!(
            "Initial route table loaded with {} entries",
            self.peer_table.len()
        );

        // Create PostgreSQL listener using sqlx
        let pool = PgPool::connect(&self.database_url).await?;
        let mut listener = PgListener::connect_with(&pool).await?;

        listener.listen("route_table_changes").await?;
        debug!(
            "Started listening for route table changes on PostgreSQL channel 'route_table_changes'"
        );

        // Spawn background task to handle notifications
        tokio::spawn(async move {
            loop {
                match listener.recv().await {
                    Ok(notification) => {
                        debug!(
                            "Received route table change notification: {}",
                            notification.payload()
                        );

                        debug!("🔄 Route table synchronizing...");

                        if let Err(e) = self.peer_table.load_routes().await {
                            error!("Failed to reload routes: {}", e);
                        } else {
                            debug!(
                                "✅ Route table synchronized ({} entries)",
                                self.peer_table.len()
                            );
                        }
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
                    }
                }
            }
        });

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_route_info_creation() {
        let route = RouteInfo {
            backend: BackendType::Upstream {
                addresses: vec!["127.0.0.1:8080".to_string()],
                round_robin_counter: Arc::new(AtomicUsize::new(0)),
            },
            redirect_to: None,
            status_code: None,
            project: None,
            environment: None,
            deployment: None,
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
                addresses: vec!["127.0.0.1:8080".to_string()],
                round_robin_counter: Arc::new(AtomicUsize::new(0)),
            },
            redirect_to: Some("https://example.com".to_string()),
            status_code: Some(301),
            project: None,
            environment: None,
            deployment: None,
        };

        assert_eq!(route.redirect_to, Some("https://example.com".to_string()));
        assert_eq!(route.status_code, Some(301));
    }

    #[test]
    fn test_route_info_custom_route() {
        let route = RouteInfo {
            backend: BackendType::Upstream {
                addresses: vec!["192.168.1.100:3000".to_string()],
                round_robin_counter: Arc::new(AtomicUsize::new(0)),
            },
            redirect_to: None,
            status_code: None,
            project: None,
            environment: None,
            deployment: None,
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
                addresses: vec![
                    "127.0.0.1:8080".to_string(),
                    "127.0.0.1:8081".to_string(),
                    "127.0.0.1:8082".to_string(),
                ],
                round_robin_counter: Arc::new(AtomicUsize::new(0)),
            },
            redirect_to: None,
            status_code: None,
            project: None,
            environment: None,
            deployment: None,
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
        };

        assert!(route.is_static());
        assert_eq!(route.static_dir(), Some("/var/www/static"));
        assert_eq!(route.get_backend_addr(), "127.0.0.1:8080"); // Fallback for static
    }

    #[test]
    fn test_backend_type_upstream() {
        let backend = BackendType::Upstream {
            addresses: vec!["127.0.0.1:8080".to_string(), "127.0.0.1:8081".to_string()],
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
            addresses: vec![],
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
            addresses: vec!["192.168.1.100:3000".to_string()],
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
                addresses: vec!["10.0.0.1:9000".to_string()],
                round_robin_counter: Arc::new(AtomicUsize::new(0)),
            },
            redirect_to: None,
            status_code: None,
            project: None,
            environment: None,
            deployment: None,
        };

        // Test all convenience methods
        assert!(!route.is_static());
        assert_eq!(route.static_dir(), None);
        assert_eq!(route.get_backend_addr(), "10.0.0.1:9000");
    }
}
