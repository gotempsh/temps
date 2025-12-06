use async_trait::async_trait;
use pingora_core::{upstreams::peer::HttpPeer, Result as PingoraResult};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_core::UtcDateTime;
use temps_entities::{deployments, environments, projects};

/// Context information about a request's project, environment, and deployment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectContext {
    pub project: Arc<projects::Model>,
    pub environment: Arc<environments::Model>,
    pub deployment: Arc<deployments::Model>,
}

/// Visitor information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Visitor {
    pub visitor_id: String,
    pub visitor_id_i32: i32,
    pub is_crawler: bool,
    pub crawler_name: Option<String>,
}

/// Session information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub session_id: String,
    pub session_id_i32: i32,
    pub visitor_id_i32: i32,
    pub is_new_session: bool,
}

/// Request metadata for logging
#[derive(Debug, Clone, Serialize)]
pub struct RequestLogData {
    pub request_id: String,
    pub host: String,
    pub method: String,
    pub path: String,
    pub status_code: i32,
    pub user_agent: String,
    pub referrer: Option<String>,
    pub ip_address: Option<String>,
    pub started_at: UtcDateTime,
    pub finished_at: UtcDateTime,
    pub request_headers: serde_json::Value,
    pub response_headers: serde_json::Value,
    pub visitor: Option<Visitor>,
    pub session: Option<Session>,
    pub project_context: Option<ProjectContext>,
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
    ) -> PingoraResult<Box<HttpPeer>>;

    /// Check if a host has custom routing configured
    async fn has_custom_route(&self, host: &str) -> bool;

    /// Get load balancing strategy for a host (for future use)
    async fn get_lb_strategy(&self, host: &str) -> Option<String>;
}

/// Trait for logging request/response data
#[async_trait]
pub trait RequestLogger: Send + Sync {
    /// Log a completed request with all metadata
    async fn log_request(
        &self,
        data: RequestLogData,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;

    /// Log an error that occurred during request processing
    async fn log_error(
        &self,
        request_id: &str,
        host: &str,
        path: &str,
        error: &str,
        context: Option<&ProjectContext>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;

    /// Check if logging is enabled for a specific project/environment
    async fn should_log_request(&self, context: Option<&ProjectContext>) -> bool;
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

/// Trait for managing visitors
#[async_trait]
pub trait VisitorManager: Send + Sync {
    /// Get or create a visitor from encrypted cookie
    async fn get_or_create_visitor(
        &self,
        visitor_cookie: Option<&str>,
        context: Option<&ProjectContext>,
        user_agent: &str,
        ip_address: Option<&str>,
    ) -> Result<Visitor, Box<dyn std::error::Error + Send + Sync>>;

    /// Generate encrypted visitor cookie
    async fn generate_visitor_cookie(
        &self,
        visitor: &Visitor,
        is_https: bool,
        context: Option<&ProjectContext>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>>; // Returns Set-Cookie header value

    /// Check if visitor tracking should be enabled for this request
    async fn should_track_visitor(
        &self,
        path: &str,
        content_type: Option<&str>,
        status_code: u16,
        context: Option<&ProjectContext>,
    ) -> bool;

    /// Get visitor cookie configuration
    fn get_visitor_cookie_config(&self) -> &CookieConfig;
}

/// Trait for managing sessions
#[async_trait]
pub trait SessionManager: Send + Sync {
    /// Get or create a session from encrypted cookie
    async fn get_or_create_session(
        &self,
        session_cookie: Option<&str>,
        visitor: &Visitor,
        context: Option<&ProjectContext>,
        referrer: Option<&str>,
    ) -> Result<Session, Box<dyn std::error::Error + Send + Sync>>;

    /// Generate encrypted session cookie
    async fn generate_session_cookie(
        &self,
        session: &Session,
        is_https: bool,
        context: Option<&ProjectContext>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>>; // Returns Set-Cookie header value

    /// Extend session expiry time
    async fn extend_session(
        &self,
        session: &Session,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;

    /// Get session cookie configuration
    fn get_session_cookie_config(&self) -> &CookieConfig;
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
