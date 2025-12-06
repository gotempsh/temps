use std::sync::Arc;
use thiserror::Error;

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::*;
use std::net::IpAddr;
use temps_entities::custom_routes::RouteType;
use tracing::{error, info};

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

pub struct LbService {
    db: Arc<DatabaseConnection>,
}

impl LbService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
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
        custom_routes::Entity::find()
            .filter(custom_routes::Column::Domain.eq(domain_val))
            .one(self.db.as_ref())
            .await
            .map_err(LbServiceError::DatabaseError)?
            .ok_or_else(|| anyhow::anyhow!("Route not found after update"))
    }

    pub async fn delete_route(&self, domain_val: &str) -> Result<()> {
        use temps_entities::custom_routes;

        custom_routes::Entity::delete_many()
            .filter(custom_routes::Column::Domain.eq(domain_val))
            .exec(self.db.as_ref())
            .await
            .context("Failed to delete custom route")?;

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
