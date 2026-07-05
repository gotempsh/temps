use async_trait::async_trait;
use pingora_core::{upstreams::peer::HttpPeer, Result as PingoraResult};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_entities::{deployments, environments, projects};

/// Context information about a request's project, environment, and deployment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectContext {
    pub project: Arc<projects::Model>,
    pub environment: Arc<environments::Model>,
    pub deployment: Arc<deployments::Model>,
}

/// Visitor information resolved from the stateless cookie codec.
/// No i32 DB id — the background batch writer resolves UUIDs to IDs asynchronously.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Visitor {
    pub visitor_id: String,
    pub is_crawler: bool,
    pub crawler_name: Option<String>,
}

/// Cookie configuration for visitor/session tracking
#[derive(Debug, Clone)]
pub struct CookieConfig {
    pub visitor_cookie_name: String,
    pub session_cookie_name: String,
    pub visitor_max_age_days: i64,
    pub session_max_age_minutes: i64,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: Option<String>,
}

impl Default for CookieConfig {
    fn default() -> Self {
        Self {
            visitor_cookie_name: "_temps_visitor_id".to_string(),
            session_cookie_name: "_temps_sid".to_string(),
            visitor_max_age_days: 365,
            session_max_age_minutes: 30,
            secure: true,
            http_only: true,
            same_site: Some("Lax".to_string()),
        }
    }
}

/// Result of resolving an upstream peer, including optional container metadata
pub struct PeerSelection {
    pub peer: Box<HttpPeer>,
    pub container_id: Option<String>,
    pub container_name: Option<String>,
}

/// Trait for resolving upstream peers based on host and request information
#[async_trait]
pub trait UpstreamResolver: Send + Sync {
    /// Resolve the upstream peer for a given host, path, and optional SNI hostname
    ///
    /// The resolver will:
    /// 1. First try SNI-based routing if sni_hostname is provided (for TLS routes)
    /// 2. Then try HTTP Host-based routing (for HTTP routes)
    /// 3. Fall back to console address if no route is found
    async fn resolve_peer(
        &self,
        host: &str,
        path: &str,
        sni_hostname: Option<&str>,
    ) -> PingoraResult<PeerSelection>;

    /// Check if a host has custom routing configured
    async fn has_custom_route(&self, host: &str) -> bool;

    /// Check if any routing source knows about this host.
    ///
    /// Returns true when the host would resolve to a real upstream — including
    /// project deployment hosts (HTTP route table, SNI/TLS table, wildcards)
    /// AND operator-defined custom routes. Used by the admin gate so that
    /// requests for legitimate project hosts are never short-circuited as
    /// "unknown host" just because they aren't in `custom_routes`.
    ///
    /// Default implementation falls back to `has_custom_route` for legacy
    /// implementors; production impls should override it.
    async fn has_route_for_host(&self, host: &str) -> bool {
        self.has_custom_route(host).await
    }

    /// Get load balancing strategy for a host (for future use)
    async fn get_lb_strategy(&self, host: &str) -> Option<String>;
}

/// Trait for resolving project context from request information
#[async_trait]
pub trait ProjectContextResolver: Send + Sync {
    /// Get project context (project, environment, deployment) from host
    async fn resolve_context(&self, host: &str) -> Option<ProjectContext>;

    /// Check if a host corresponds to a static file deployment
    async fn is_static_deployment(&self, host: &str) -> bool;

    /// Get redirect information for a host (if it should redirect)
    async fn get_redirect_info(&self, host: &str) -> Option<(String, u16)>; // (url, status_code)

    /// Get static file path for a host (if it serves static files)
    async fn get_static_path(&self, host: &str) -> Option<String>;
}

/// First-visit attribution data for a new visitor
#[derive(Debug, Clone, Default)]
pub struct FirstVisitAttribution {
    /// Full referrer URL
    pub referrer: Option<String>,
    /// Hostname extracted from referrer
    pub referrer_hostname: Option<String>,
    /// Marketing channel (e.g. "Organic Search", "Direct")
    pub channel: Option<String>,
    /// UTM source parameter
    pub utm_source: Option<String>,
    /// UTM medium parameter
    pub utm_medium: Option<String>,
    /// UTM campaign parameter
    pub utm_campaign: Option<String>,
}

/// Error types for proxy services
#[derive(Debug, thiserror::Error)]
pub enum ProxyServiceError {
    #[error("Upstream resolution failed: {0}")]
    UpstreamResolution(String),

    #[error("Request logging failed: {0}")]
    RequestLogging(String),

    #[error("Project context resolution failed: {0}")]
    ProjectContext(String),

    #[error("Visitor management failed: {0}")]
    Visitor(String),

    #[error("Session management failed: {0}")]
    Session(String),

    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}
