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

    #[error("Invalid route domain '{domain}': {reason}")]
    InvalidDomain { domain: String, reason: String },

    #[error("Route domain '{domain}' overlaps existing route '{conflict}'")]
    RouteOverlap { domain: String, conflict: String },

    #[error("Route domain '{domain}' overlaps managed domain '{conflict}'; pass force_override only when this traffic override is intentional")]
    ManagedDomainConflict { domain: String, conflict: String },

    #[error("Invalid upstream host '{host}': {reason}")]
    InvalidUpstream { host: String, reason: String },

    #[error("Failed to resolve upstream host '{host}': {source}")]
    UpstreamResolution {
        host: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Upstream '{host}' resolves to private or loopback address {ip}; pass allow_private_upstream only when public exposure is intentional")]
    PrivateUpstreamRequiresAcknowledgement { host: String, ip: IpAddr },

    #[error("Upstream '{host}' resolves to blocked address {ip}: {reason}")]
    BlockedUpstream {
        host: String,
        ip: IpAddr,
        reason: temps_core::url_validation::UrlValidationError,
    },

    #[error("Route not found for domain: {domain}")]
    RouteNotFound { domain: String },

    #[error("Database error: {0}")]
    DatabaseError(#[source] sea_orm::DbErr),

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
        self.create_route_with_options(domain, host, port, route_type, false, false)
            .await
    }

    pub async fn create_route_with_options(
        &self,
        domain: String,
        host: String,
        port: i32,
        route_type: Option<RouteType>,
        force_override: bool,
        allow_private_upstream: bool,
    ) -> Result<temps_entities::custom_routes::Model, LbServiceError> {
        use temps_entities::custom_routes;

        let domain = normalize_route_domain(&domain)?;
        let host = normalize_and_validate_upstream(&host, port, allow_private_upstream).await?;
        info!(
            "Creating new route for domain: {} (type: {:?})",
            domain, route_type
        );
        let transaction = self
            .db
            .begin()
            .await
            .map_err(LbServiceError::DatabaseError)?;
        // Serialize overlap checks and inserts. The normalized unique index
        // handles exact duplicates; this transaction-scoped lock also makes
        // wildcard/exact overlap enforcement race-free across API instances.
        transaction
            .execute(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT pg_advisory_xact_lock(hashtext('temps_custom_routes_write'))".to_string(),
            ))
            .await
            .map_err(LbServiceError::DatabaseError)?;

        // Exact duplicate checks are backed by a normalized unique DB index;
        // this preflight provides a clearer error before attempting the insert.
        let existing = custom_routes::Entity::find()
            .filter(custom_routes::Column::Domain.eq(&domain))
            .one(&transaction)
            .await
            .map_err(LbServiceError::DatabaseError)?;
        if existing.is_some() {
            return Err(LbServiceError::RouteAlreadyExists {
                domain: domain.clone(),
            });
        }

        self.ensure_no_route_overlap(&transaction, &domain, force_override)
            .await?;

        let new_route = custom_routes::ActiveModel {
            domain: Set(domain.clone()),
            host: Set(host),
            port: Set(port),
            domain_id: Set(None),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            enabled: Set(true),
            route_type: Set(route_type.unwrap_or_default()),
            force_override: Set(force_override),
            ..Default::default()
        };

        let route = custom_routes::Entity::insert(new_route)
            .exec_with_returning(&transaction)
            .await
            .map_err(|error| {
                if is_unique_violation(&error) {
                    LbServiceError::RouteAlreadyExists {
                        domain: domain.clone(),
                    }
                } else {
                    LbServiceError::DatabaseError(error)
                }
            })?;
        transaction
            .commit()
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

    async fn ensure_no_route_overlap<C: ConnectionTrait>(
        &self,
        connection: &C,
        domain: &str,
        force_override: bool,
    ) -> Result<(), LbServiceError> {
        use temps_entities::custom_routes;

        let custom = custom_routes::Entity::find()
            .all(connection)
            .await
            .map_err(LbServiceError::DatabaseError)?;
        if let Some(conflict) = custom
            .into_iter()
            .find(|route| domains_overlap(domain, &route.domain))
        {
            return Err(LbServiceError::RouteOverlap {
                domain: domain.to_string(),
                conflict: conflict.domain,
            });
        }

        let managed = load_managed_route_domains(connection).await?;

        if let Some(conflict) = managed
            .into_iter()
            .find(|managed_domain| domains_overlap(domain, managed_domain))
        {
            if !force_override {
                return Err(LbServiceError::ManagedDomainConflict {
                    domain: domain.to_string(),
                    conflict,
                });
            }
            warn!(
                domain,
                conflict, "Creating explicit managed-domain route override"
            );
        }

        Ok(())
    }

    pub async fn get_route_exact(
        &self,
        domain_val: &str,
    ) -> Result<temps_entities::custom_routes::Model, LbServiceError> {
        use temps_entities::custom_routes;
        let domain_val = normalize_route_domain(domain_val)?;

        custom_routes::Entity::find()
            .filter(custom_routes::Column::Domain.eq(&domain_val))
            .one(self.db.as_ref())
            .await
            .map_err(LbServiceError::DatabaseError)?
            .ok_or_else(|| LbServiceError::NotFound(domain_val))
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

    pub async fn list_routes(
        &self,
    ) -> Result<Vec<temps_entities::custom_routes::Model>, LbServiceError> {
        use temps_entities::custom_routes;

        let routes = custom_routes::Entity::find()
            .all(self.db.as_ref())
            .await
            .map_err(LbServiceError::DatabaseError)?;

        Ok(routes)
    }

    pub async fn update_route(
        &self,
        domain_val: &str,
        host_val: String,
        port_val: i32,
        enabled_val: bool,
        route_type: Option<RouteType>,
    ) -> Result<temps_entities::custom_routes::Model, LbServiceError> {
        self.update_route_with_options(
            domain_val,
            host_val,
            port_val,
            enabled_val,
            route_type,
            false,
        )
        .await
    }

    pub async fn update_route_with_options(
        &self,
        domain_val: &str,
        host_val: String,
        port_val: i32,
        enabled_val: bool,
        route_type: Option<RouteType>,
        allow_private_upstream: bool,
    ) -> Result<temps_entities::custom_routes::Model, LbServiceError> {
        use temps_entities::custom_routes;
        let domain_val = normalize_route_domain(domain_val)?;
        let host_val =
            normalize_and_validate_upstream(&host_val, port_val, allow_private_upstream).await?;

        self.get_route_exact(&domain_val).await?;

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
            .filter(custom_routes::Column::Domain.eq(&domain_val))
            .set(update_model)
            .exec(self.db.as_ref())
            .await
            .map_err(LbServiceError::DatabaseError)?;

        // Return the updated route
        let updated = custom_routes::Entity::find()
            .filter(custom_routes::Column::Domain.eq(&domain_val))
            .one(self.db.as_ref())
            .await
            .map_err(LbServiceError::DatabaseError)?
            .ok_or_else(|| LbServiceError::RouteNotFound {
                domain: domain_val.clone(),
            })?;

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

    pub async fn delete_route(&self, domain_val: &str) -> Result<(), LbServiceError> {
        use temps_entities::custom_routes;
        let domain_val = normalize_route_domain(domain_val)?;

        let result = custom_routes::Entity::delete_many()
            .filter(custom_routes::Column::Domain.eq(&domain_val))
            .exec(self.db.as_ref())
            .await
            .map_err(LbServiceError::DatabaseError)?;

        if result.rows_affected == 0 {
            return Err(LbServiceError::RouteNotFound { domain: domain_val });
        }

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

async fn load_managed_route_domains<C: ConnectionTrait>(
    connection: &C,
) -> Result<Vec<String>, LbServiceError> {
    use temps_entities::{
        deployment_domains, deployments, environment_domains, environments, project_custom_domains,
        projects, settings,
    };

    let mut domains = environment_domains::Entity::find()
        .all(connection)
        .await
        .map_err(LbServiceError::DatabaseError)?
        .into_iter()
        .map(|row| row.domain)
        .collect::<Vec<_>>();
    domains.extend(
        project_custom_domains::Entity::find()
            .all(connection)
            .await
            .map_err(LbServiceError::DatabaseError)?
            .into_iter()
            .map(|row| row.domain),
    );
    domains.extend(
        deployment_domains::Entity::find()
            .all(connection)
            .await
            .map_err(LbServiceError::DatabaseError)?
            .into_iter()
            .map(|row| row.domain),
    );

    let preview_domain = settings::Entity::find()
        .one(connection)
        .await
        .map_err(LbServiceError::DatabaseError)?
        .and_then(|row| {
            row.data
                .get("preview_domain")
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "localho.st".to_string());
    let projects = projects::Entity::find()
        .filter(projects::Column::DeletedAt.is_null())
        .all(connection)
        .await
        .map_err(LbServiceError::DatabaseError)?
        .into_iter()
        .map(|project| (project.id, project))
        .collect::<HashMap<_, _>>();
    let deployments = deployments::Entity::find()
        .all(connection)
        .await
        .map_err(LbServiceError::DatabaseError)?
        .into_iter()
        .map(|deployment| (deployment.id, deployment))
        .collect::<HashMap<_, _>>();
    let environments = environments::Entity::find()
        .filter(environments::Column::DeletedAt.is_null())
        .filter(environments::Column::CurrentDeploymentId.is_not_null())
        .all(connection)
        .await
        .map_err(LbServiceError::DatabaseError)?;

    for environment in environments {
        let subdomain = environment.subdomain.trim();
        if !subdomain.is_empty() {
            domains.push(subdomain.to_string());
            domains.push(format!("{subdomain}.{preview_domain}"));
        }
        let Some(project) = projects.get(&environment.project_id) else {
            continue;
        };
        if !environment.slug.trim().is_empty() && !project.slug.trim().is_empty() {
            domains.push(format!(
                "{}.{}.temps.local",
                environment.slug.trim(),
                project.slug.trim()
            ));
        }
        if let Some(deployment) = environment
            .current_deployment_id
            .and_then(|id| deployments.get(&id))
        {
            domains.push(format!("{}.{}", deployment.slug, preview_domain));
        }
        if let Some(temps_entities::preset::PresetConfig::DockerCompose(config)) =
            project.preset_config.as_ref()
        {
            for public_port in &config.public_ports {
                let label = format!("{}-{subdomain}", public_port.service);
                let label = label.chars().take(63).collect::<String>();
                let label = label.trim_end_matches('-');
                if !label.is_empty() {
                    domains.push(format!("{label}.{preview_domain}"));
                }
            }
        }
    }
    Ok(domains)
}

pub fn normalize_route_domain(input: &str) -> Result<String, LbServiceError> {
    let normalized = input.trim().trim_end_matches('.').to_ascii_lowercase();
    let hostname = normalized.strip_prefix("*.").unwrap_or(&normalized);
    let wildcard_count = normalized.matches('*').count();

    if normalized.is_empty() || hostname.is_empty() {
        return Err(LbServiceError::InvalidDomain {
            domain: input.to_string(),
            reason: "domain must not be empty".to_string(),
        });
    }
    if wildcard_count > usize::from(normalized.starts_with("*.")) {
        return Err(LbServiceError::InvalidDomain {
            domain: input.to_string(),
            reason: "wildcard is only allowed as the complete leftmost label".to_string(),
        });
    }
    if hostname.len() > 253 {
        return Err(LbServiceError::InvalidDomain {
            domain: input.to_string(),
            reason: "hostname exceeds 253 characters".to_string(),
        });
    }
    if !hostname.contains('.') {
        return Err(LbServiceError::InvalidDomain {
            domain: input.to_string(),
            reason: "route domain must contain at least two labels".to_string(),
        });
    }
    if let Err(reason) = validate_hostname_labels(hostname) {
        return Err(LbServiceError::InvalidDomain {
            domain: input.to_string(),
            reason,
        });
    }

    Ok(normalized)
}

fn domains_overlap(left: &str, right: &str) -> bool {
    let left = left.trim().trim_end_matches('.').to_ascii_lowercase();
    let right = right.trim().trim_end_matches('.').to_ascii_lowercase();
    let left_base = left.strip_prefix("*.").unwrap_or(&left);
    let right_base = right.strip_prefix("*.").unwrap_or(&right);

    left == right
        || (left.starts_with("*.")
            && (right == left_base || right.ends_with(&format!(".{left_base}"))))
        || (right.starts_with("*.")
            && (left == right_base || left.ends_with(&format!(".{right_base}"))))
}

fn is_unique_violation(error: &sea_orm::DbErr) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("23505") || message.contains("duplicate key value")
}

fn validate_hostname_labels(hostname: &str) -> Result<(), String> {
    if hostname.len() > 253 {
        return Err("hostname exceeds 253 characters".to_string());
    }
    for label in hostname.split('.') {
        let valid = !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
        if !valid {
            return Err(format!("invalid hostname label '{label}'"));
        }
    }
    Ok(())
}

async fn normalize_and_validate_upstream(
    input: &str,
    port: i32,
    allow_private: bool,
) -> Result<String, LbServiceError> {
    if !(1..=u16::MAX as i32).contains(&port) {
        return Err(LbServiceError::InvalidUpstream {
            host: input.to_string(),
            reason: "port must be in 1..=65535".to_string(),
        });
    }

    let trimmed = input.trim();
    let unbracketed = trimmed
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(trimmed);
    if unbracketed.is_empty() || unbracketed.chars().any(char::is_whitespace) {
        return Err(LbServiceError::InvalidUpstream {
            host: input.to_string(),
            reason: "host must not be empty or contain whitespace".to_string(),
        });
    }

    let (canonical, addresses, resolved_hostname) = match unbracketed.parse::<IpAddr>() {
        Ok(ip) => {
            let canonical = match ip {
                IpAddr::V4(ip) => ip.to_string(),
                IpAddr::V6(ip) => format!("[{ip}]"),
            };
            (canonical, vec![ip], false)
        }
        Err(_) => {
            if unbracketed.contains('*') {
                return Err(LbServiceError::InvalidUpstream {
                    host: input.to_string(),
                    reason: "upstream host cannot contain a wildcard".to_string(),
                });
            }
            validate_hostname_labels(unbracketed).map_err(|reason| {
                LbServiceError::InvalidUpstream {
                    host: input.to_string(),
                    reason,
                }
            })?;
            let resolved = tokio::net::lookup_host((unbracketed, port as u16)).await;
            match resolved {
                Ok(addresses) => (
                    unbracketed.to_ascii_lowercase(),
                    addresses.map(|address| address.ip()).collect(),
                    true,
                ),
                Err(source) => {
                    return Err(LbServiceError::UpstreamResolution {
                        host: unbracketed.to_string(),
                        source,
                    });
                }
            }
        }
    };

    if addresses.is_empty() {
        return Err(LbServiceError::InvalidUpstream {
            host: input.to_string(),
            reason: "hostname resolved without any addresses".to_string(),
        });
    }

    for ip in &addresses {
        let validation = match ip {
            IpAddr::V4(ip) => temps_core::url_validation::validate_ipv4(ip),
            IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
                Some(mapped) => temps_core::url_validation::validate_ipv4(&mapped),
                None => temps_core::url_validation::validate_ipv6(ip),
            },
        };
        if let Err(reason) = validation {
            if allow_private
                && matches!(
                    reason,
                    temps_core::url_validation::UrlValidationError::PrivateIp
                        | temps_core::url_validation::UrlValidationError::LoopbackIp
                )
            {
                continue;
            }
            if matches!(
                reason,
                temps_core::url_validation::UrlValidationError::PrivateIp
                    | temps_core::url_validation::UrlValidationError::LoopbackIp
            ) {
                return Err(LbServiceError::PrivateUpstreamRequiresAcknowledgement {
                    host: canonical,
                    ip: *ip,
                });
            }
            return Err(LbServiceError::BlockedUpstream {
                host: canonical,
                ip: *ip,
                reason,
            });
        }
    }

    if resolved_hostname {
        let mut addresses = addresses;
        addresses.sort();
        let pinned = addresses[0];
        return Ok(match pinned {
            IpAddr::V4(ip) => ip.to_string(),
            IpAddr::V6(ip) => format!("[{ip}]"),
        });
    }

    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
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
            force_override: false,
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

    #[test]
    fn route_domains_are_canonicalized_and_validated() {
        assert_eq!(
            normalize_route_domain("  API.Example.COM. ").expect("valid domain"),
            "api.example.com"
        );
        assert_eq!(
            normalize_route_domain("*.Example.COM").expect("valid wildcard"),
            "*.example.com"
        );
        for invalid in [
            "",
            "localhost",
            "*example.com",
            "api.*.example.com",
            "*.com",
            "-api.example.com",
            "api..example.com",
            "api.example.com/path",
        ] {
            assert!(
                matches!(
                    normalize_route_domain(invalid),
                    Err(LbServiceError::InvalidDomain { .. })
                ),
                "accepted invalid route domain {invalid}"
            );
        }
    }

    #[test]
    fn wildcard_overlap_detects_hijacks_but_not_siblings() {
        assert!(domains_overlap("*.example.com", "api.example.com"));
        assert!(domains_overlap("api.example.com", "*.example.com"));
        assert!(domains_overlap("API.Example.com.", "api.example.com"));
        assert!(!domains_overlap("api.example.com", "web.example.com"));
        assert!(!domains_overlap("example.com", "notexample.com"));
    }

    #[tokio::test]
    async fn private_upstream_requires_explicit_acknowledgement() {
        let error = normalize_and_validate_upstream("127.0.0.1", 8080, false)
            .await
            .expect_err("loopback must be rejected by default");
        assert!(matches!(
            error,
            LbServiceError::PrivateUpstreamRequiresAcknowledgement { .. }
        ));
        assert_eq!(
            normalize_and_validate_upstream("127.0.0.1", 8080, true)
                .await
                .expect("explicitly acknowledged loopback"),
            "127.0.0.1"
        );
    }

    #[tokio::test]
    async fn special_upstream_is_blocked_even_with_private_acknowledgement() {
        for address in [
            "169.254.169.254",
            "100.64.0.1",
            "192.0.0.1",
            "198.18.0.1",
            "240.0.0.1",
            "2001:db8::1",
            "::ffff:169.254.169.254",
            "::ffff:100.64.0.1",
        ] {
            let error = normalize_and_validate_upstream(address, 80, true)
                .await
                .expect_err("special-use address must always be blocked");
            assert!(matches!(error, LbServiceError::BlockedUpstream { .. }));
        }
    }

    #[tokio::test]
    async fn ipv6_upstream_is_stored_bracketed_for_socket_formatting() {
        assert_eq!(
            normalize_and_validate_upstream("::1", 8080, true)
                .await
                .expect("explicitly acknowledged loopback"),
            "[::1]"
        );
    }

    #[tokio::test]
    async fn hostname_upstreams_are_pinned_and_dns_failure_is_fail_closed() {
        let pinned = normalize_and_validate_upstream("localhost", 8080, true)
            .await
            .expect("localhost resolves in the test environment");
        let unbracketed = pinned.trim_start_matches('[').trim_end_matches(']');
        assert!(
            unbracketed.parse::<IpAddr>().is_ok(),
            "persisted upstream must be a validated IP, got {pinned}"
        );

        let error = normalize_and_validate_upstream("this-host-must-not-exist.invalid", 8080, true)
            .await
            .expect_err("allow-private must not make DNS failure fail open");
        assert!(matches!(error, LbServiceError::UpstreamResolution { .. }));
    }

    #[tokio::test]
    async fn exact_lookup_does_not_return_a_wildcard_route() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<custom_routes::Model>::new()])
            .into_connection();
        let service = LbService::new(Arc::new(db));
        let error = service
            .get_route_exact("API.Example.COM.")
            .await
            .expect_err("exact route is absent");
        assert!(matches!(error, LbServiceError::NotFound(domain) if domain == "api.example.com"));
    }

    #[tokio::test]
    async fn deleting_an_absent_route_returns_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();
        let service = LbService::new(Arc::new(db));
        let error = service
            .delete_route("missing.example.com")
            .await
            .expect_err("missing route must not report success");
        assert!(matches!(
            error,
            LbServiceError::RouteNotFound { domain } if domain == "missing.example.com"
        ));
    }

    #[test]
    fn unique_violation_detection_uses_postgres_sqlstate() {
        assert!(is_unique_violation(&sea_orm::DbErr::Custom(
            "duplicate key (SQLSTATE 23505)".to_string()
        )));
        assert!(!is_unique_violation(&sea_orm::DbErr::Custom(
            "connection closed".to_string()
        )));
    }
}
