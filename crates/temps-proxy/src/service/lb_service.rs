use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use chrono::Utc;
use sea_orm::*;
use std::net::IpAddr;
use temps_entities::custom_routes::RouteType;
use tracing::{info, warn};

#[derive(Error, Debug)]
pub enum LbServiceError {
    #[error("Database connection error")]
    DatabaseConnectionError(String),

    #[error("Route already exists for domain: {domain}")]
    RouteAlreadyExists { domain: String },

    #[error("Route not found for domain: {domain}")]
    RouteNotFound { domain: String },

    #[error("Database error")]
    DatabaseError(sea_orm::DbErr),

    #[error("Route not found: {0}")]
    NotFound(String),

    #[error("Failed to get database connection: {source}")]
    ConnectionError {
        #[from]
        source: sea_orm::DbErr,
    },

    #[error("Failed to get public IP address")]
    PublicIpError(String),

    #[error("DNS resolution error for domain {domain}: {source}")]
    DnsResolutionError {
        domain: String,
        source: anyhow::Error,
    },

    #[error(
        "Domain {domain} does not point to expected IP {expected_ip}. Found IPs: {found_ips:?}"
    )]
    DomainNotPointingToServer {
        domain: String,
        expected_ip: IpAddr,
        found_ips: Vec<IpAddr>,
    },
}

/// How often the in-memory custom-route snapshot is refreshed from the database.
///
/// `custom_routes` is a tiny operator-curated table that changes only when an
/// admin explicitly adds, edits, or removes a route override. Write paths in
/// `LbService` refresh the snapshot on the instance that received the write
/// (write-through), but in the current process topology admin-API writes are
/// handled by the console-owned `LbService`, which is a separate object from the
/// instance the Pingora traffic-serving proxy reads. As a result, this 60-second
/// periodic loop is the primary (and only) propagation mechanism for route changes
/// to reach the hot-path reader; a newly-created or deleted route becomes visible
/// to real traffic within at most 60 seconds.
const CUSTOM_ROUTE_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// In-memory snapshot of all *enabled* custom routes.
///
/// Exact routes (non-wildcard) are indexed by domain for O(1) lookup.
/// Wildcard routes (`domain` starting with `*.`) are kept in a small Vec
/// and checked via [`LbService::matches_wildcard`] — the set is expected
/// to be tiny (single digits in typical operator configs).
#[derive(Default)]
pub struct CustomRouteSnapshot {
    /// Non-wildcard enabled routes keyed by their `domain` column value.
    pub exact: HashMap<String, temps_entities::custom_routes::Model>,
    /// Wildcard enabled routes (`domain` starts with `*.`).
    pub wildcards: Vec<temps_entities::custom_routes::Model>,
}

pub struct LbService {
    db: Arc<DatabaseConnection>,
    /// Lock-free in-memory snapshot of all enabled custom routes. Re-synced
    /// from the DB every [`CUSTOM_ROUTE_REFRESH_INTERVAL`] seconds (the primary
    /// propagation path), and also refreshed on this instance after every write
    /// (write-through). Write-through is only useful when the instance that
    /// processes writes is also the one serving hot-path reads; in the current
    /// process topology the hot-path instance never receives admin-API writes
    /// directly, so the periodic loop is the sole propagation mechanism for
    /// route changes reaching real traffic (worst-case staleness: 60 seconds).
    snapshot: Arc<ArcSwap<CustomRouteSnapshot>>,
}

impl LbService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self {
            db,
            snapshot: Arc::new(ArcSwap::from_pointee(CustomRouteSnapshot::default())),
        }
    }

    /// Reload the in-memory snapshot from the database.
    ///
    /// Loads all rows with `enabled = true` and splits them into exact and
    /// wildcard buckets. The resulting snapshot is atomically swapped in via
    /// [`ArcSwap`], making any concurrent snapshot reads immediately consistent.
    pub async fn refresh_snapshot(&self) -> Result<(), LbServiceError> {
        use temps_entities::custom_routes;

        let rows = custom_routes::Entity::find()
            .filter(custom_routes::Column::Enabled.eq(true))
            .all(self.db.as_ref())
            .await
            .map_err(LbServiceError::DatabaseError)?;

        let mut exact: HashMap<String, temps_entities::custom_routes::Model> = HashMap::new();
        let mut wildcards: Vec<temps_entities::custom_routes::Model> = Vec::new();

        for row in rows {
            if row.domain.starts_with("*.") {
                wildcards.push(row);
            } else {
                exact.insert(row.domain.clone(), row);
            }
        }

        let total = exact.len() + wildcards.len();
        self.snapshot
            .store(Arc::new(CustomRouteSnapshot { exact, wildcards }));
        tracing::debug!("Refreshed custom-route snapshot: {} route(s)", total);
        Ok(())
    }

    /// Run the periodic custom-route snapshot refresh loop.
    ///
    /// Loads immediately on first call, then every
    /// [`CUSTOM_ROUTE_REFRESH_INTERVAL`] seconds. Mirrors the
    /// `CertHostCache::run_refresh_loop` pattern. Spawn once at startup in a
    /// dedicated thread alongside the other refresh loops in `server.rs`.
    pub async fn run_refresh_loop(self: Arc<Self>) {
        loop {
            if let Err(e) = self.refresh_snapshot().await {
                warn!("Failed to refresh custom-route snapshot: {}", e);
            }
            tokio::time::sleep(CUSTOM_ROUTE_REFRESH_INTERVAL).await;
        }
    }

    /// Check whether `host` has an enabled custom route in the current snapshot.
    ///
    /// Lock-free: performs a single atomic [`ArcSwap::load`] then an O(1) hash
    /// lookup (exact) or O(k) scan (wildcards, k ≈ single digits). Never queries
    /// the database.
    pub fn has_route_in_snapshot(&self, host: &str) -> bool {
        let snap = self.snapshot.load();
        if snap.exact.contains_key(host) {
            return true;
        }
        snap.wildcards
            .iter()
            .any(|r| Self::matches_wildcard(host, &r.domain))
    }

    /// Check if a domain matches a wildcard pattern
    /// e.g., "api.example.com" matches "*.example.com"
    fn matches_wildcard(domain: &str, pattern: &str) -> bool {
        if !pattern.starts_with("*.") {
            return domain == pattern;
        }

        let wildcard_base = &pattern[2..]; // Remove "*."

        // Check if domain ends with the wildcard base
        if domain.ends_with(wildcard_base) {
            // Make sure there's at least one subdomain
            let prefix_len = domain.len() - wildcard_base.len();
            if prefix_len > 0 {
                // Check that the character before the base is a dot
                domain.chars().nth(prefix_len - 1) == Some('.')
            } else {
                false
            }
        } else {
            domain == wildcard_base // Also match the base domain itself if configured
        }
    }

    pub async fn create_route(
        &self,
        domain: String,
        host: String,
        port: i32,
        route_type: Option<RouteType>,
    ) -> Result<temps_entities::custom_routes::Model, LbServiceError> {
        info!(
            "Creating new route for domain: {} (type: {:?})",
            domain, route_type
        );
        // Check if route already exists
        match self.get_route(&domain).await {
            Ok(_) => {
                return Err(LbServiceError::RouteAlreadyExists {
                    domain: domain.clone(),
                });
            }
            Err(LbServiceError::NotFound(_)) => {
                // Route does not exist, continue
            }
            Err(e) => {
                return Err(e);
            }
        }

        use temps_entities::custom_routes;

        let new_route = custom_routes::ActiveModel {
            domain: Set(domain.clone()),
            host: Set(host),
            port: Set(port),
            domain_id: Set(None),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            enabled: Set(true),
            route_type: Set(route_type.unwrap_or_default()),
            ..Default::default()
        };

        let route = custom_routes::Entity::insert(new_route)
            .exec_with_returning(self.db.as_ref())
            .await
            .map_err(LbServiceError::DatabaseError)?;

        // Write-through: refresh the snapshot on this instance after the DB
        // write. This is immediately visible to any hot-path reads on the SAME
        // instance, but in the current process topology admin-API writes are
        // handled by the console-owned LbService, which is a separate object
        // from the Pingora hot-path instance. The 60-second periodic loop in
        // that instance is what propagates this change to real traffic.
        if let Err(e) = self.refresh_snapshot().await {
            warn!(
                "Failed to refresh custom-route snapshot after create: {}",
                e
            );
        }

        Ok(route)
    }

    pub async fn get_route(
        &self,
        domain_val: &str,
    ) -> Result<temps_entities::custom_routes::Model, LbServiceError> {
        use temps_entities::custom_routes;

        // First try exact match
        let route = custom_routes::Entity::find()
            .filter(custom_routes::Column::Domain.eq(domain_val))
            .one(self.db.as_ref())
            .await
            .map_err(LbServiceError::DatabaseError)?;

        if let Some(route) = route {
            return Ok(route);
        }

        // If no exact match, try wildcard matching
        let all_routes = custom_routes::Entity::find()
            .filter(custom_routes::Column::Domain.starts_with("*."))
            .all(self.db.as_ref())
            .await
            .map_err(LbServiceError::DatabaseError)?;

        // Find the first wildcard route that matches
        for route in all_routes {
            if Self::matches_wildcard(domain_val, &route.domain) {
                return Ok(route);
            }
        }

        Err(LbServiceError::NotFound(domain_val.to_string()))
    }

    pub async fn list_routes(&self) -> Result<Vec<temps_entities::custom_routes::Model>> {
        use temps_entities::custom_routes;

        let routes = custom_routes::Entity::find()
            .all(self.db.as_ref())
            .await
            .context("Failed to list custom routes")?;

        Ok(routes)
    }

    pub async fn update_route(
        &self,
        domain_val: &str,
        host_val: String,
        port_val: i32,
        enabled_val: bool,
        route_type: Option<RouteType>,
    ) -> Result<temps_entities::custom_routes::Model> {
        use temps_entities::custom_routes;

        let mut update_model = custom_routes::ActiveModel {
            updated_at: Set(Utc::now()),
            enabled: Set(enabled_val),
            host: Set(host_val),
            port: Set(port_val),
            ..Default::default()
        };

        // Only update route_type if provided
        if let Some(rt) = route_type {
            update_model.route_type = Set(rt);
        }

        custom_routes::Entity::update_many()
            .filter(custom_routes::Column::Domain.eq(domain_val))
            .set(update_model)
            .exec(self.db.as_ref())
            .await
            .map_err(LbServiceError::DatabaseError)?;

        // Return the updated route
        let updated = custom_routes::Entity::find()
            .filter(custom_routes::Column::Domain.eq(domain_val))
            .one(self.db.as_ref())
            .await
            .map_err(LbServiceError::DatabaseError)?
            .ok_or_else(|| anyhow::anyhow!("Route not found after update"))?;

        // Write-through: refresh the snapshot on this instance after the DB
        // write. This is immediately visible to hot-path reads on the SAME
        // instance, but in the current process topology admin-API writes are
        // handled by the console-owned LbService, which is a separate object
        // from the Pingora hot-path instance. The 60-second periodic loop in
        // that instance is what propagates this change to real traffic.
        if let Err(e) = self.refresh_snapshot().await {
            warn!(
                "Failed to refresh custom-route snapshot after update: {}",
                e
            );
        }

        Ok(updated)
    }

    pub async fn delete_route(&self, domain_val: &str) -> Result<()> {
        use temps_entities::custom_routes;

        custom_routes::Entity::delete_many()
            .filter(custom_routes::Column::Domain.eq(domain_val))
            .exec(self.db.as_ref())
            .await
            .context("Failed to delete custom route")?;

        // Write-through: refresh the snapshot on this instance after the DB
        // write. This is immediately visible to hot-path reads on the SAME
        // instance, but in the current process topology admin-API writes are
        // handled by the console-owned LbService, which is a separate object
        // from the Pingora hot-path instance. The 60-second periodic loop in
        // that instance is what propagates this deletion to real traffic.
        if let Err(e) = self.refresh_snapshot().await {
            warn!(
                "Failed to refresh custom-route snapshot after delete: {}",
                e
            );
        }

        Ok(())
    }

    pub async fn get_route_by_host(
        &self,
        host_val: &str,
    ) -> Result<Option<temps_entities::custom_routes::Model>> {
        use temps_entities::custom_routes;

        // Strip port from host if present
        let domain_val = host_val.split(':').next().unwrap_or(host_val);

        // First try exact match
        let route = custom_routes::Entity::find()
            .filter(custom_routes::Column::Domain.eq(domain_val))
            .filter(custom_routes::Column::Enabled.eq(true))
            .one(self.db.as_ref())
            .await
            .context("Failed to get custom route")?;

        if route.is_some() {
            return Ok(route);
        }

        // If no exact match, try wildcard matching
        let all_routes = custom_routes::Entity::find()
            .filter(custom_routes::Column::Enabled.eq(true))
            .filter(custom_routes::Column::Domain.starts_with("*."))
            .all(self.db.as_ref())
            .await
            .context("Failed to get wildcard routes")?;

        // Find the first wildcard route that matches
        for route in all_routes {
            if Self::matches_wildcard(domain_val, &route.domain) {
                return Ok(Some(route));
            }
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_entities::custom_routes;

    /// Build a minimal custom_routes::Model for use in MockDatabase results.
    fn route_model(domain: &str, host: &str, port: i32, enabled: bool) -> custom_routes::Model {
        let now = Utc::now();
        custom_routes::Model {
            id: 1,
            domain: domain.to_string(),
            host: host.to_string(),
            port,
            domain_id: None,
            created_at: now,
            updated_at: now,
            enabled,
            route_type: RouteType::Http,
        }
    }

    // -----------------------------------------------------------------------
    // Snapshot lookup correctness
    // -----------------------------------------------------------------------

    /// An exact route stored in the snapshot is found by `has_route_in_snapshot`.
    #[tokio::test]
    async fn snapshot_exact_lookup_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = LbService::new(Arc::new(db));

        // Populate snapshot directly without DB.
        let mut exact = HashMap::new();
        exact.insert(
            "api.example.com".to_string(),
            route_model("api.example.com", "10.0.0.1", 8080, true),
        );
        svc.snapshot.store(Arc::new(CustomRouteSnapshot {
            exact,
            wildcards: vec![],
        }));

        assert!(svc.has_route_in_snapshot("api.example.com"));
        assert!(!svc.has_route_in_snapshot("other.example.com"));
    }

    /// A wildcard route stored in the snapshot matches subdomains correctly.
    #[tokio::test]
    async fn snapshot_wildcard_lookup_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = LbService::new(Arc::new(db));

        svc.snapshot.store(Arc::new(CustomRouteSnapshot {
            exact: HashMap::new(),
            wildcards: vec![route_model("*.example.com", "10.0.0.2", 80, true)],
        }));

        assert!(svc.has_route_in_snapshot("app.example.com")); // subdomain matches
        assert!(!svc.has_route_in_snapshot("app.other.com")); // wrong base domain
    }

    /// An empty snapshot returns false for every host.
    #[tokio::test]
    async fn snapshot_empty_returns_false() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let svc = LbService::new(Arc::new(db));
        // Default snapshot is empty.
        assert!(!svc.has_route_in_snapshot("any.example.com"));
    }

    // -----------------------------------------------------------------------
    // Refresh picks up newly added rows
    // -----------------------------------------------------------------------

    /// After `refresh_snapshot` the snapshot reflects the routes in the DB.
    #[tokio::test]
    async fn refresh_populates_snapshot() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![route_model(
                "new.example.com",
                "10.0.0.3",
                9000,
                true,
            )]])
            .into_connection();

        let svc = LbService::new(Arc::new(db));

        // Before refresh: snapshot is empty.
        assert!(!svc.has_route_in_snapshot("new.example.com"));

        svc.refresh_snapshot()
            .await
            .expect("refresh should succeed");

        // After refresh: the new route is visible.
        assert!(svc.has_route_in_snapshot("new.example.com"));
    }

    /// A second refresh replaces the snapshot entirely with the new DB state.
    #[tokio::test]
    async fn refresh_replaces_old_snapshot() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![
                // First refresh: two routes.
                vec![
                    route_model("keep.example.com", "10.0.0.1", 80, true),
                    route_model("old.example.com", "10.0.0.2", 80, true),
                ],
                // Second refresh: only the kept route remains.
                vec![route_model("keep.example.com", "10.0.0.1", 80, true)],
            ])
            .into_connection();

        let svc = LbService::new(Arc::new(db));

        svc.refresh_snapshot().await.expect("first refresh ok");
        assert!(svc.has_route_in_snapshot("keep.example.com"));
        assert!(svc.has_route_in_snapshot("old.example.com"));

        svc.refresh_snapshot().await.expect("second refresh ok");
        assert!(svc.has_route_in_snapshot("keep.example.com"));
        assert!(!svc.has_route_in_snapshot("old.example.com")); // removed
    }
}
