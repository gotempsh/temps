use crate::config::*;
use crate::crawler_detector::CrawlerDetector;
use crate::service::lb_service::LbService;
use crate::traits::*;
use async_trait::async_trait;
use cookie::Cookie;
use pingora_core::{upstreams::peer::HttpPeer, Result as PingoraResult};
use sea_orm::*;
use std::sync::Arc;
use temps_database::DbConnection;
use temps_entities::{request_sessions, visitor};
use temps_routes::CachedPeerTable;
use tracing::{debug, error, warn};
use uuid::Uuid;

const ROUTE_PREFIX_TEMPS: &str = "/api/_temps";
const VISITOR_ID_COOKIE: &str = "_temps_visitor_id";
const SESSION_ID_COOKIE: &str = "_temps_sid";

/// Generate project-scoped cookie name for visitor
fn get_visitor_cookie_name(_project_id: Option<i32>) -> String {
    VISITOR_ID_COOKIE.to_string()
}

/// Generate project-scoped cookie name for session
fn get_session_cookie_name(_project_id: Option<i32>) -> String {
    SESSION_ID_COOKIE.to_string()
}

/// Implementation of UpstreamResolver trait
pub struct UpstreamResolverImpl {
    server_config: Arc<ProxyConfig>,
    lb_service: Arc<LbService>,
    route_table: Arc<CachedPeerTable>,
}

impl UpstreamResolverImpl {
    pub fn new(
        server_config: Arc<ProxyConfig>,
        lb_service: Arc<LbService>,
        route_table: Arc<CachedPeerTable>,
    ) -> Self {
        Self {
            server_config,
            lb_service,
            route_table,
        }
    }
}

#[async_trait]
impl UpstreamResolver for UpstreamResolverImpl {
    async fn resolve_peer(
        &self,
        host: &str,
        path: &str,
        sni_hostname: Option<&str>,
    ) -> PingoraResult<Box<HttpPeer>> {
        debug!(
            "Resolving peer for host: {}, path: {}, sni: {:?}",
            host, path, sni_hostname
        );

        // Check if it's a temps API route first
        if path.starts_with(ROUTE_PREFIX_TEMPS) {
            debug!(
                "Routing temps API request to console: {}",
                self.server_config.console_address
            );
            let peer = Box::new(HttpPeer::new(
                self.server_config.console_address.clone(),
                false,
                "".to_string(),
            ));
            return Ok(peer);
        }

        // 1. First try TLS/SNI-based routing
        // Note: In pingora-core 0.6.0, SNI is not available in SslDigest
        // We use the Host header which typically matches the SNI for TLS connections
        // If SNI was provided (from future pingora versions), use it; otherwise use host
        let sni_or_host = sni_hostname.unwrap_or(host);
        if let Some(route_info) = self.route_table.get_route_by_sni(sni_or_host) {
            let backend_addr = route_info.get_backend_addr();
            debug!(
                "Found TLS route via SNI/Host {} -> {}",
                sni_or_host, backend_addr
            );
            let peer = Box::new(HttpPeer::new(backend_addr, false, "".to_string()));
            return Ok(peer);
        }

        // 2. Try HTTP Host-based routing (HTTP routes)
        if let Some(route_info) = self.route_table.get_route_by_host(host) {
            let project_id = route_info.project.as_ref().map(|p| p.id);
            let env_id = route_info.environment.as_ref().map(|e| e.id);
            let backend_addr = route_info.get_backend_addr(); // Get next backend using round-robin
            debug!(
                "Found HTTP route for {} -> {} (project_id: {:?}, env_id: {:?})",
                host, backend_addr, project_id, env_id
            );

            // Note: Redirects are now handled in proxy.rs request_filter before peer resolution
            // If we reach here, no redirect is configured and we route to backend normally

            let peer = Box::new(HttpPeer::new(backend_addr, false, "".to_string()));
            return Ok(peer);
        }

        // 3. Legacy: Check the old get_route method for backwards compatibility
        if let Some(route_info) = self.route_table.get_route(host) {
            let project_id = route_info.project.as_ref().map(|p| p.id);
            let env_id = route_info.environment.as_ref().map(|e| e.id);
            let backend_addr = route_info.get_backend_addr();
            debug!(
                "Found legacy route for {} -> {} (project_id: {:?}, env_id: {:?})",
                host, backend_addr, project_id, env_id
            );

            let peer = Box::new(HttpPeer::new(backend_addr, false, "".to_string()));
            return Ok(peer);
        }

        // No route found - route to console address as default
        debug!(
            "No route found in table for host: {}, routing to console",
            host
        );
        let peer = Box::new(HttpPeer::new(
            self.server_config.console_address.clone(),
            false,
            "".to_string(),
        ));
        Ok(peer)
    }

    async fn has_custom_route(&self, host: &str) -> bool {
        self.lb_service.get_route(host).await.is_ok()
    }

    async fn get_lb_strategy(&self, _host: &str) -> Option<String> {
        Some("round_robin".to_string())
    }
}

/// Implementation of RequestLogger trait
pub struct RequestLoggerImpl {
    config: LoggingConfig,
    db: Arc<sea_orm::DatabaseConnection>,
    ip_service: Arc<temps_geo::IpAddressService>,
}

impl RequestLoggerImpl {
    pub fn new(
        config: LoggingConfig,
        db: Arc<sea_orm::DatabaseConnection>,
        ip_service: Arc<temps_geo::IpAddressService>,
    ) -> Self {
        Self {
            config,
            db,
            ip_service,
        }
    }
}

#[async_trait]
impl RequestLogger for RequestLoggerImpl {
    async fn log_request(
        &self,
        data: RequestLogData,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use sea_orm::{ActiveModelTrait, Set};
        use temps_entities::proxy_logs;

        // Skip logging if no project context
        let Some(ref context) = data.project_context else {
            debug!("Skipping request log - no project context");
            return Ok(());
        };

        let elapsed_time = (data.finished_at - data.started_at).num_milliseconds() as i32;

        // Note: is_static_file and is_entry_page are not used in proxy_logs
        // These were part of request_logs but proxy_logs doesn't track these fields

        // Parse user agent with woothee
        let parser = woothee::parser::Parser::new();
        let ua_result = parser.parse(&data.user_agent);

        let (browser, browser_version, operating_system, is_mobile) = if let Some(ua) = ua_result {
            let is_mob = ua.category == "smartphone" || ua.category == "mobilephone";
            (
                Some(ua.name.to_string()),
                Some(ua.version.to_string()),
                Some(ua.os.to_string()),
                is_mob,
            )
        } else {
            (None, None, None, false)
        };

        // Get crawler info from visitor, or detect if not already detected
        let (is_crawler, crawler_name) = if let Some(visitor) = data.visitor.as_ref() {
            (visitor.is_crawler, visitor.crawler_name.clone())
        } else {
            // Fall back to CrawlerDetector if visitor didn't detect it
            let detected_crawler = CrawlerDetector::is_bot(Some(&data.user_agent));
            let detected_name = if detected_crawler {
                CrawlerDetector::get_crawler_name(Some(&data.user_agent))
            } else {
                None
            };
            (detected_crawler, detected_name)
        };

        // Geolocate IP address
        let ip_address_id = if let Some(ref ip) = data.ip_address {
            match self.ip_service.get_or_create_ip(ip).await {
                Ok(ip_info) => Some(ip_info.id),
                Err(e) => {
                    warn!("Failed to geolocate IP {}: {:?}", ip, e);
                    None
                }
            }
        } else {
            None
        };

        // Clone values needed for debug logging before moving into ActiveModel
        let method_clone = data.method.clone();
        let path_clone = data.path.clone();
        let status_code = data.status_code;
        let visitor_id = data.visitor.as_ref().map(|v| v.visitor_id_i32);
        let session_id = data.session.as_ref().map(|s| s.session_id_i32);

        // Determine routing status
        let routing_status = if context.deployment.id > 0 {
            "routed"
        } else {
            "no_deployment"
        }
        .to_string();

        // Convert status_code to i16
        let status_code_i16 = data.status_code as i16;

        // Headers are already JSON values
        let response_headers_json = data.response_headers;
        let request_headers_json = data.request_headers;

        // Determine device type from is_mobile
        let device_type = if is_mobile {
            Some("mobile".to_string())
        } else {
            Some("desktop".to_string())
        };

        let log_entry = proxy_logs::ActiveModel {
            timestamp: Set(data.started_at),
            method: Set(data.method),
            path: Set(data.path),
            query_string: Set(None), // TODO: Extract query string from path if needed
            host: Set(data.host),
            status_code: Set(status_code_i16),
            response_time_ms: Set(Some(elapsed_time)),
            request_source: Set("proxy".to_string()),
            is_system_request: Set(false),
            routing_status: Set(routing_status),
            project_id: Set(Some(context.project.id)),
            environment_id: Set(Some(context.environment.id)),
            deployment_id: Set(Some(context.deployment.id)),
            container_id: Set(None),  // TODO: Add container info if available
            upstream_host: Set(None), // TODO: Add upstream host if available
            error_message: Set(None),
            client_ip: Set(data.ip_address),
            user_agent: Set(Some(data.user_agent)),
            referrer: Set(data.referrer),
            request_id: Set(data.request_id),
            ip_geolocation_id: Set(ip_address_id),
            browser: Set(browser),
            browser_version: Set(browser_version),
            operating_system: Set(operating_system),
            device_type: Set(device_type),
            is_bot: Set(Some(is_crawler)),
            bot_name: Set(crawler_name),
            request_size_bytes: Set(None),  // TODO: Add if available
            response_size_bytes: Set(None), // TODO: Add if available
            cache_status: Set(None),
            request_headers: Set(Some(request_headers_json)),
            response_headers: Set(Some(response_headers_json)),
            created_date: Set(data.started_at.date_naive()),
            session_id: Set(data.session.as_ref().map(|s| s.session_id_i32)),
            visitor_id: Set(data.visitor.as_ref().map(|v| v.visitor_id_i32)),
            ..Default::default()
        };

        match log_entry.insert(self.db.as_ref()).await {
            Ok(_) => {
                debug!(
                    "Request logged to DB: {} deployment_id={} {} - status: {}, visitor: {:?}, session: {:?}",
                    method_clone,
                    context.deployment.id,
                    &path_clone[..path_clone.len().min(50)],
                    status_code,
                    visitor_id,
                    session_id
                );
                Ok(())
            }
            Err(e) => {
                error!("Failed to insert request log: {:?}", e);
                Err(Box::new(e))
            }
        }
    }

    async fn log_error(
        &self,
        request_id: &str,
        host: &str,
        path: &str,
        error: &str,
        _context: Option<&ProjectContext>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        error!(
            "Request error [{}] {}{}  - {}",
            request_id, host, path, error
        );
        Ok(())
    }

    async fn should_log_request(&self, _context: Option<&ProjectContext>) -> bool {
        self.config.log_all_requests
    }
}

/// Configuration for request logging
#[derive(Debug, Clone)]
pub struct LoggingConfig {
    pub log_all_requests: bool,
    pub log_static_assets: bool,
    pub log_internal_api: bool,
    pub log_non_project_requests: bool,
    pub log_request_headers: bool,
    pub log_response_headers: bool,
    pub max_header_size: usize,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            log_all_requests: true,
            log_static_assets: false,
            log_internal_api: false,
            log_non_project_requests: true,
            log_request_headers: true,
            log_response_headers: true,
            max_header_size: 16 * 1024,
        }
    }
}

/// Implementation of ProjectContextResolver trait
pub struct ProjectContextResolverImpl {
    route_table: Arc<CachedPeerTable>,
}

impl ProjectContextResolverImpl {
    pub fn new(route_table: Arc<CachedPeerTable>) -> Self {
        Self { route_table }
    }
}

#[async_trait]
impl ProjectContextResolver for ProjectContextResolverImpl {
    async fn resolve_context(&self, host: &str) -> Option<ProjectContext> {
        // Get route info from O(1) route table lookup with cached models
        let route_info = self.route_table.get_route(host)?;

        // Return cached models directly - no database queries!
        Some(ProjectContext {
            project: route_info.project?,
            environment: route_info.environment?,
            deployment: route_info.deployment?,
        })
    }

    async fn is_static_deployment(&self, host: &str) -> bool {
        // Use route_info.is_static() to check if backend is static directory
        if let Some(route_info) = self.route_table.get_route(host) {
            return route_info.is_static();
        }
        false
    }

    async fn get_redirect_info(&self, host: &str) -> Option<(String, u16)> {
        // Use cached redirect info from route table
        let route_info = self.route_table.get_route(host)?;
        let redirect_to = route_info.redirect_to?;
        let status_code = route_info.status_code? as u16;
        Some((redirect_to, status_code))
    }

    async fn get_static_path(&self, host: &str) -> Option<String> {
        // Use route_info.static_dir() to get static directory path
        let route_info = self.route_table.get_route(host)?;
        route_info.static_dir().map(|s| s.to_string())
    }
}

/// Implementation of VisitorManager trait
pub struct VisitorManagerImpl {
    db: Arc<DbConnection>,
    crypto: Arc<temps_core::CookieCrypto>,
    config: CookieConfig,
    ip_service: Arc<temps_geo::IpAddressService>,
}

impl VisitorManagerImpl {
    pub fn new(
        db: Arc<DbConnection>,
        crypto: Arc<temps_core::CookieCrypto>,
        ip_service: Arc<temps_geo::IpAddressService>,
    ) -> Self {
        Self {
            db,
            crypto,
            config: CookieConfig::default(),
            ip_service,
        }
    }
}

#[async_trait]
impl VisitorManager for VisitorManagerImpl {
    async fn get_or_create_visitor(
        &self,
        visitor_cookie: Option<&str>,
        context: Option<&ProjectContext>,
        user_agent: &str,
        ip_address: Option<&str>,
    ) -> Result<Visitor, Box<dyn std::error::Error + Send + Sync>> {
        let project_id = context.as_ref().map(|c| c.project.id).unwrap_or(1);
        let environment_id = context.as_ref().map(|c| c.environment.id).unwrap_or(1);

        // Try to find existing visitor
        if let Some(cookie_value) = visitor_cookie {
            if let Ok(visitor_id) = self.crypto.decrypt(cookie_value) {
                if let Ok(Some(visitor)) = visitor::Entity::find()
                    .filter(visitor::Column::VisitorId.eq(&visitor_id))
                    .filter(visitor::Column::ProjectId.eq(project_id))
                    .one(self.db.as_ref())
                    .await
                {
                    // Update last_seen
                    let mut active_visitor: visitor::ActiveModel = visitor.clone().into();
                    active_visitor.last_seen = Set(chrono::Utc::now());
                    let _ = active_visitor.update(self.db.as_ref()).await;

                    return Ok(Visitor {
                        visitor_id: visitor.visitor_id,
                        visitor_id_i32: visitor.id,
                        is_crawler: visitor.is_crawler,
                        crawler_name: visitor.crawler_name,
                    });
                }
            }
        }

        // Create new visitor (crawlers should be filtered out before calling this method)
        let new_visitor_id = Uuid::new_v4().to_string();

        // Geolocate IP address if provided
        let ip_address_id = if let Some(ip) = ip_address {
            match self.ip_service.get_or_create_ip(ip).await {
                Ok(ip_info) => Some(ip_info.id),
                Err(e) => {
                    warn!("Failed to geolocate IP {}: {:?}", ip, e);
                    None
                }
            }
        } else {
            None
        };

        // Detect if user agent is a crawler/bot
        let is_crawler = CrawlerDetector::is_bot(Some(user_agent));
        let crawler_name = if is_crawler {
            CrawlerDetector::get_crawler_name(Some(user_agent))
        } else {
            None
        };

        let visitor = visitor::ActiveModel {
            visitor_id: Set(new_visitor_id.clone()),
            project_id: Set(project_id),
            environment_id: Set(environment_id),
            first_seen: Set(chrono::Utc::now()),
            last_seen: Set(chrono::Utc::now()),
            user_agent: Set(Some(user_agent.to_string())),
            ip_address_id: Set(ip_address_id),
            is_crawler: Set(is_crawler),
            crawler_name: Set(crawler_name),
            ..Default::default()
        };

        let visitor = visitor.insert(self.db.as_ref()).await?;

        Ok(Visitor {
            visitor_id: visitor.visitor_id,
            visitor_id_i32: visitor.id,
            is_crawler: visitor.is_crawler,
            crawler_name: visitor.crawler_name,
        })
    }

    async fn generate_visitor_cookie(
        &self,
        visitor: &Visitor,
        is_https: bool,
        context: Option<&ProjectContext>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let encrypted_visitor_id = self.crypto.encrypt(&visitor.visitor_id)?;
        let project_id = context.map(|c| c.project.id);
        let cookie_name = get_visitor_cookie_name(project_id);
        let mut cookie_builder = Cookie::build((cookie_name, encrypted_visitor_id))
            .path("/")
            .max_age(cookie::time::Duration::days(
                self.config.visitor_max_age_days,
            ))
            .http_only(self.config.http_only)
            .secure(is_https && self.config.secure);

        // Add SameSite attribute if configured
        if let Some(ref same_site_value) = self.config.same_site {
            let same_site = match same_site_value.to_lowercase().as_str() {
                "strict" => cookie::SameSite::Strict,
                "lax" => cookie::SameSite::Lax,
                "none" => cookie::SameSite::None,
                _ => cookie::SameSite::Lax, // Default to Lax
            };
            cookie_builder = cookie_builder.same_site(same_site);
        }

        let cookie = cookie_builder.build();
        Ok(cookie.to_string())
    }

    async fn should_track_visitor(
        &self,
        path: &str,
        content_type: Option<&str>,
        status_code: u16,
        _context: Option<&ProjectContext>,
    ) -> bool {
        // Don't track static assets
        if path.contains(".")
            && (path.ends_with(".js")
                || path.ends_with(".css")
                || path.ends_with(".png")
                || path.ends_with(".jpg")
                || path.ends_with(".svg")
                || path.ends_with(".ico"))
        {
            return false;
        }

        // Don't track internal API calls
        if path.starts_with(ROUTE_PREFIX_TEMPS) {
            return false;
        }

        // Track HTML pages or error pages
        let is_html = content_type
            .map(|ct| ct.starts_with("text/html"))
            .unwrap_or(false);

        is_html || status_code >= 400
    }

    fn get_visitor_cookie_config(&self) -> &CookieConfig {
        &self.config
    }
}

/// Implementation of SessionManager trait
pub struct SessionManagerImpl {
    db: Arc<DbConnection>,
    crypto: Arc<temps_core::CookieCrypto>,
    config: CookieConfig,
}

impl SessionManagerImpl {
    pub fn new(db: Arc<DbConnection>, crypto: Arc<temps_core::CookieCrypto>) -> Self {
        Self {
            db,
            crypto,
            config: CookieConfig::default(),
        }
    }
}

#[async_trait]
impl SessionManager for SessionManagerImpl {
    async fn get_or_create_session(
        &self,
        session_cookie: Option<&str>,
        visitor: &Visitor,
        _context: Option<&ProjectContext>,
        referrer: Option<&str>,
    ) -> Result<Session, Box<dyn std::error::Error + Send + Sync>> {
        let now = chrono::Utc::now();

        // Try to find existing session from cookie
        if let Some(cookie_value) = session_cookie {
            debug!("Session cookie received: {} bytes", cookie_value.len());
            match self.crypto.decrypt(cookie_value) {
                Ok(session_id) => {
                    debug!("Decrypted session ID: {}", session_id);
                    // Look up session in database
                    match request_sessions::Entity::find()
                        .filter(request_sessions::Column::SessionId.eq(&session_id))
                        .one(self.db.as_ref())
                        .await
                    {
                        Ok(Some(session)) => {
                            debug!("Found session in database: {}", session.session_id);
                            // Check if session has expired (30 minutes)
                            let expiry_time = session.last_accessed_at
                                + chrono::Duration::minutes(self.config.session_max_age_minutes);

                            if now < expiry_time {
                                // Session is still valid - update last_accessed_at
                                let mut active_session: request_sessions::ActiveModel =
                                    session.clone().into();
                                active_session.last_accessed_at = Set(now);
                                let updated_session =
                                    active_session.update(self.db.as_ref()).await?;

                                debug!(
                                    "✓ Reusing existing session {} for visitor {} (last accessed: {:?})",
                                    updated_session.session_id, visitor.visitor_id, session.last_accessed_at
                                );

                                return Ok(Session {
                                    session_id: updated_session.session_id,
                                    session_id_i32: updated_session.id,
                                    visitor_id_i32: visitor.visitor_id_i32,
                                    is_new_session: false,
                                });
                            }
                            // Session expired - will create new one below
                            debug!(
                                "Session {} expired (last accessed: {:?}), creating new session",
                                session.session_id, session.last_accessed_at
                            );
                        }
                        Ok(None) => {
                            debug!("Session {} not found in database", session_id);
                        }
                        Err(e) => {
                            debug!("Database error looking up session: {:?}", e);
                        }
                    }
                }
                Err(e) => {
                    debug!("Failed to decrypt session cookie: {:?}", e);
                }
            }
        } else {
            debug!("No session cookie provided in request");
        }

        // Create new session
        let new_session_id = Uuid::new_v4().to_string();

        let session = request_sessions::ActiveModel {
            session_id: Set(new_session_id.clone()),
            started_at: Set(now),
            last_accessed_at: Set(now),
            ip_address: Set(None), // IP will be set by request logger if needed
            user_agent: Set(None), // User agent will be set by request logger if needed
            referrer: Set(referrer.map(|r| r.to_string())),
            data: Set("{}".to_string()), // Empty JSON object
            visitor_id: Set(Some(visitor.visitor_id_i32)),
            ..Default::default()
        };

        let session = session.insert(self.db.as_ref()).await?;

        debug!(
            "Created new session {} for visitor {}",
            session.session_id, visitor.visitor_id
        );

        Ok(Session {
            session_id: session.session_id,
            session_id_i32: session.id,
            visitor_id_i32: visitor.visitor_id_i32,
            is_new_session: true,
        })
    }

    async fn generate_session_cookie(
        &self,
        session: &Session,
        is_https: bool,
        context: Option<&ProjectContext>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let encrypted_session_id = self.crypto.encrypt(&session.session_id)?;
        let project_id = context.map(|c| c.project.id);
        let cookie_name = get_session_cookie_name(project_id);
        let mut cookie_builder = Cookie::build((cookie_name, encrypted_session_id))
            .path("/")
            .max_age(cookie::time::Duration::minutes(
                self.config.session_max_age_minutes,
            ))
            .http_only(self.config.http_only)
            .secure(is_https && self.config.secure);

        // Add SameSite attribute if configured
        if let Some(ref same_site_value) = self.config.same_site {
            let same_site = match same_site_value.to_lowercase().as_str() {
                "strict" => cookie::SameSite::Strict,
                "lax" => cookie::SameSite::Lax,
                "none" => cookie::SameSite::None,
                _ => cookie::SameSite::Lax, // Default to Lax
            };
            cookie_builder = cookie_builder.same_site(same_site);
        }

        let cookie = cookie_builder.build();
        Ok(cookie.to_string())
    }

    async fn extend_session(
        &self,
        session: &Session,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Update last_accessed_at to extend the session
        if let Ok(Some(db_session)) = request_sessions::Entity::find()
            .filter(request_sessions::Column::SessionId.eq(&session.session_id))
            .one(self.db.as_ref())
            .await
        {
            let mut active_session: request_sessions::ActiveModel = db_session.into();
            active_session.last_accessed_at = Set(chrono::Utc::now());
            active_session.update(self.db.as_ref()).await?;
        }
        Ok(())
    }

    fn get_session_cookie_config(&self) -> &CookieConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use temps_database::test_utils::TestDatabase;
    use temps_entities::{
        deployments, environments, preset::Preset, projects, proxy_logs,
        upstream_config::UpstreamList, visitor,
    };

    fn create_mock_ip_service(db: Arc<DatabaseConnection>) -> Arc<temps_geo::IpAddressService> {
        let geoip_service = Arc::new(temps_geo::GeoIpService::Mock(
            temps_geo::MockGeoIpService::new(),
        ));
        Arc::new(temps_geo::IpAddressService::new(db, geoip_service))
    }

    async fn create_test_visitor(
        db: &Arc<DatabaseConnection>,
        visitor_id: &str,
        project_id: i32,
        environment_id: i32,
    ) -> i32 {
        use chrono::Utc;
        use sea_orm::ActiveValue::Set;

        let visitor_model = visitor::ActiveModel {
            visitor_id: Set(visitor_id.to_string()),
            project_id: Set(project_id),
            environment_id: Set(environment_id),
            first_seen: Set(Utc::now()),
            last_seen: Set(Utc::now()),
            is_crawler: Set(false),
            ..Default::default()
        };

        let visitor = visitor_model.insert(db.as_ref()).await.unwrap();
        visitor.id
    }

    async fn create_test_session(
        db: &Arc<DatabaseConnection>,
        session_id: &str,
        visitor_id_i32: i32,
    ) -> i32 {
        use chrono::Utc;
        use sea_orm::ActiveValue::Set;
        use temps_entities::request_sessions;

        let session_model = request_sessions::ActiveModel {
            session_id: Set(session_id.to_string()),
            started_at: Set(Utc::now()),
            last_accessed_at: Set(Utc::now()),
            visitor_id: Set(Some(visitor_id_i32)),
            data: Set("{}".to_string()),
            ..Default::default()
        };

        let session = session_model.insert(db.as_ref()).await.unwrap();
        session.id
    }

    async fn create_test_project_context(db: &Arc<DatabaseConnection>) -> ProjectContext {
        // Create test project
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            slug: Set("test-project".to_string()),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            preset: Set(Preset::Nixpacks),
            ..Default::default()
        };
        let project = project.insert(db.as_ref()).await.unwrap();

        // Create test environment
        let environment = environments::ActiveModel {
            name: Set("production".to_string()),
            slug: Set("prod".to_string()),
            subdomain: Set("test".to_string()),
            host: Set("test.example.com".to_string()),
            upstreams: Set(UpstreamList::default()),
            project_id: Set(project.id),
            ..Default::default()
        };
        let environment = environment.insert(db.as_ref()).await.unwrap();

        // Create test deployment
        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("test-deployment".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            state: Set("completed".to_string()),
            ..Default::default()
        };
        let deployment = deployment.insert(db.as_ref()).await.unwrap();

        ProjectContext {
            project: Arc::new(project),
            environment: Arc::new(environment),
            deployment: Arc::new(deployment),
        }
    }

    #[tokio::test]
    async fn test_request_logger_user_agent_parsing() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let ip_service = create_mock_ip_service(test_db.connection_arc().clone());
        let logger = RequestLoggerImpl::new(
            LoggingConfig::default(),
            test_db.connection_arc().clone(),
            ip_service,
        );

        let context = create_test_project_context(&test_db.connection_arc()).await;

        // Test Chrome user agent
        let chrome_ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
        let log_data = RequestLogData {
            request_id: "test-req-1".to_string(),
            host: "test.example.com".to_string(),
            method: "GET".to_string(),
            path: "/test".to_string(),
            status_code: 200,
            user_agent: chrome_ua.to_string(),
            referrer: None,
            ip_address: Some("8.8.8.8".to_string()),
            started_at: chrono::Utc::now(),
            finished_at: chrono::Utc::now(),
            request_headers: serde_json::json!({}),
            response_headers: serde_json::json!({}),
            visitor: None,
            session: None,
            project_context: Some(context.clone()),
        };

        logger.log_request(log_data).await.unwrap();

        // Verify log was created with parsed user agent data
        let logs = proxy_logs::Entity::find()
            .filter(proxy_logs::Column::RequestId.eq("test-req-1"))
            .one(test_db.connection_arc().as_ref())
            .await
            .unwrap()
            .expect("Log should be created");

        assert_eq!(logs.browser, Some("Chrome".to_string()));
        assert!(logs.browser_version.is_some());
        assert_eq!(logs.operating_system, Some("Windows 10".to_string()));
        assert_ne!(logs.device_type, Some("mobile".to_string()));
    }

    #[tokio::test]
    async fn test_request_logger_mobile_detection() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let ip_service = create_mock_ip_service(test_db.connection_arc().clone());
        let logger = RequestLoggerImpl::new(
            LoggingConfig::default(),
            test_db.connection_arc().clone(),
            ip_service,
        );

        let context = create_test_project_context(&test_db.connection_arc()).await;

        // Test mobile Safari user agent
        let mobile_ua = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1";
        let log_data = RequestLogData {
            request_id: "test-req-mobile".to_string(),
            host: "test.example.com".to_string(),
            method: "GET".to_string(),
            path: "/test".to_string(),
            status_code: 200,
            user_agent: mobile_ua.to_string(),
            referrer: None,
            ip_address: Some("1.2.3.4".to_string()),
            started_at: chrono::Utc::now(),
            finished_at: chrono::Utc::now(),
            request_headers: serde_json::json!({}),
            response_headers: serde_json::json!({}),
            visitor: None,
            session: None,
            project_context: Some(context),
        };

        logger.log_request(log_data).await.unwrap();

        // Verify mobile detection
        let logs = proxy_logs::Entity::find()
            .filter(proxy_logs::Column::RequestId.eq("test-req-mobile"))
            .one(test_db.connection_arc().as_ref())
            .await
            .unwrap()
            .expect("Log should be created");

        assert_eq!(logs.device_type, Some("mobile".to_string()));
        assert_eq!(logs.operating_system, Some("iPhone".to_string()));
    }

    #[tokio::test]
    async fn test_request_logger_crawler_detection() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let ip_service = create_mock_ip_service(test_db.connection_arc().clone());
        let logger = RequestLoggerImpl::new(
            LoggingConfig::default(),
            test_db.connection_arc().clone(),
            ip_service,
        );

        let context = create_test_project_context(&test_db.connection_arc()).await;

        // Test Googlebot user agent
        let bot_ua = "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)";
        let log_data = RequestLogData {
            request_id: "test-req-bot".to_string(),
            host: "test.example.com".to_string(),
            method: "GET".to_string(),
            path: "/test".to_string(),
            status_code: 200,
            user_agent: bot_ua.to_string(),
            referrer: None,
            ip_address: None,
            started_at: chrono::Utc::now(),
            finished_at: chrono::Utc::now(),
            request_headers: serde_json::json!({}),
            response_headers: serde_json::json!({}),
            visitor: None,
            session: None,
            project_context: Some(context),
        };

        logger.log_request(log_data).await.unwrap();

        // Verify crawler detection
        let logs = proxy_logs::Entity::find()
            .filter(proxy_logs::Column::RequestId.eq("test-req-bot"))
            .one(test_db.connection_arc().as_ref())
            .await
            .unwrap()
            .expect("Log should be created");

        assert_eq!(logs.is_bot, Some(true));
        assert!(logs.bot_name.is_some());
        assert!(logs.bot_name.unwrap().contains("Google"));
    }

    #[tokio::test]
    async fn test_request_logger_ip_geolocation() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let ip_service = create_mock_ip_service(test_db.connection_arc().clone());
        let logger = RequestLoggerImpl::new(
            LoggingConfig::default(),
            test_db.connection_arc().clone(),
            ip_service.clone(),
        );

        let context = create_test_project_context(&test_db.connection_arc()).await;

        // Test with a real IP address
        let test_ip = "8.8.8.8"; // Google DNS
        let log_data = RequestLogData {
            request_id: "test-req-ip".to_string(),
            host: "test.example.com".to_string(),
            method: "GET".to_string(),
            path: "/test".to_string(),
            status_code: 200,
            user_agent: "Mozilla/5.0".to_string(),
            referrer: None,
            ip_address: Some(test_ip.to_string()),
            started_at: chrono::Utc::now(),
            finished_at: chrono::Utc::now(),
            request_headers: serde_json::json!({}),
            response_headers: serde_json::json!({}),
            visitor: None,
            session: None,
            project_context: Some(context),
        };

        logger.log_request(log_data).await.unwrap();

        // Verify IP geolocation was created
        let logs = proxy_logs::Entity::find()
            .filter(proxy_logs::Column::RequestId.eq("test-req-ip"))
            .one(test_db.connection_arc().as_ref())
            .await
            .unwrap()
            .expect("Log should be created");

        assert!(
            logs.ip_geolocation_id.is_some(),
            "IP address should be geolocated"
        );
        assert_eq!(logs.client_ip, Some(test_ip.to_string()));

        // Verify the IP address record was created with geolocation data
        let ip_record =
            temps_entities::ip_geolocations::Entity::find_by_id(logs.ip_geolocation_id.unwrap())
                .one(test_db.connection_arc().as_ref())
                .await
                .unwrap()
                .expect("IP address record should exist");

        assert_eq!(ip_record.ip_address, test_ip);
        // Country should be populated by the geolocation service (country is required field)
        assert!(!ip_record.country.is_empty());
    }

    #[tokio::test]
    async fn test_request_logger_with_visitor_and_session() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let ip_service = create_mock_ip_service(test_db.connection_arc().clone());
        let logger = RequestLoggerImpl::new(
            LoggingConfig::default(),
            test_db.connection_arc().clone(),
            ip_service,
        );

        let context = create_test_project_context(&test_db.connection_arc()).await;

        // Create visitor record in database first
        let visitor_id_i32 = create_test_visitor(
            &test_db.connection_arc(),
            "test-visitor-123",
            context.project.id,
            context.environment.id,
        )
        .await;

        // Create session record in database
        let session_id_i32 = create_test_session(
            &test_db.connection_arc(),
            "test-session-456",
            visitor_id_i32,
        )
        .await;

        // Create test visitor
        let visitor_data = Visitor {
            visitor_id: "test-visitor-123".to_string(),
            visitor_id_i32,
            is_crawler: false,
            crawler_name: None,
        };

        // Create test session
        let session_data = Session {
            session_id: "test-session-456".to_string(),
            session_id_i32,
            visitor_id_i32,
            is_new_session: true,
        };

        let log_data = RequestLogData {
            request_id: "test-req-with-visitor".to_string(),
            host: "test.example.com".to_string(),
            method: "GET".to_string(),
            path: "/test".to_string(),
            status_code: 200,
            user_agent: "Mozilla/5.0".to_string(),
            referrer: Some("https://google.com".to_string()),
            ip_address: Some("1.2.3.4".to_string()),
            started_at: chrono::Utc::now(),
            finished_at: chrono::Utc::now(),
            request_headers: serde_json::json!({}),
            response_headers: serde_json::json!({}),
            visitor: Some(visitor_data),
            session: Some(session_data),
            project_context: Some(context),
        };

        logger.log_request(log_data).await.unwrap();

        // Verify visitor and session IDs are stored
        let logs = proxy_logs::Entity::find()
            .filter(proxy_logs::Column::RequestId.eq("test-req-with-visitor"))
            .one(test_db.connection_arc().as_ref())
            .await
            .unwrap()
            .expect("Log should be created");

        assert_eq!(logs.visitor_id, Some(visitor_id_i32));
        assert_eq!(logs.session_id, Some(session_id_i32));
        // Note: proxy_logs doesn't track is_entry_page like request_logs did
        assert_eq!(logs.referrer, Some("https://google.com".to_string()));
    }

    #[tokio::test]
    async fn test_session_creation_and_reuse() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let crypto = Arc::new(
            temps_core::CookieCrypto::new(
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .unwrap(),
        );
        let session_manager =
            SessionManagerImpl::new(test_db.connection_arc().clone(), crypto.clone());

        let context = create_test_project_context(&test_db.connection_arc()).await;

        // Create visitor record in database first
        let visitor_id_i32 = create_test_visitor(
            &test_db.connection_arc(),
            "test-visitor-1",
            context.project.id,
            context.environment.id,
        )
        .await;

        let visitor = Visitor {
            visitor_id: "test-visitor-1".to_string(),
            visitor_id_i32,
            is_crawler: false,
            crawler_name: None,
        };

        // First request - should create new session
        let session1 = session_manager
            .get_or_create_session(None, &visitor, Some(&context), None)
            .await
            .unwrap();

        assert!(session1.is_new_session, "First session should be new");

        // Generate encrypted cookie
        let cookie = session_manager
            .generate_session_cookie(&session1, false, None)
            .await
            .unwrap();

        // Extract encrypted session ID from cookie
        let encrypted_session_id = cookie
            .split(';')
            .next()
            .unwrap()
            .trim()
            .split('=')
            .nth(1)
            .unwrap()
            .to_string();

        // Second request with same cookie - should reuse session
        let session2 = session_manager
            .get_or_create_session(Some(&encrypted_session_id), &visitor, Some(&context), None)
            .await
            .unwrap();

        assert_eq!(
            session1.session_id, session2.session_id,
            "Should reuse same session"
        );
        assert!(!session2.is_new_session, "Second session should not be new");

        // Third request - should still reuse
        let session3 = session_manager
            .get_or_create_session(Some(&encrypted_session_id), &visitor, Some(&context), None)
            .await
            .unwrap();

        assert_eq!(
            session1.session_id, session3.session_id,
            "Should still reuse same session"
        );
        assert!(!session3.is_new_session, "Third session should not be new");
    }

    #[tokio::test]
    async fn test_session_expiry_after_30_minutes() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let crypto = Arc::new(
            temps_core::CookieCrypto::new(
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .unwrap(),
        );
        let session_manager =
            SessionManagerImpl::new(test_db.connection_arc().clone(), crypto.clone());

        let context = create_test_project_context(&test_db.connection_arc()).await;

        // Create visitor record in database first
        let visitor_id_i32 = create_test_visitor(
            &test_db.connection_arc(),
            "test-visitor-2",
            context.project.id,
            context.environment.id,
        )
        .await;

        let visitor = Visitor {
            visitor_id: "test-visitor-2".to_string(),
            visitor_id_i32,
            is_crawler: false,
            crawler_name: None,
        };

        // Create initial session
        let session1 = session_manager
            .get_or_create_session(None, &visitor, Some(&context), None)
            .await
            .unwrap();

        // Generate cookie
        let cookie = session_manager
            .generate_session_cookie(&session1, false, None)
            .await
            .unwrap();

        let encrypted_session_id = cookie
            .split(';')
            .next()
            .unwrap()
            .trim()
            .split('=')
            .nth(1)
            .unwrap()
            .to_string();

        // Manually expire the session by setting last_accessed_at to 31 minutes ago
        use temps_entities::request_sessions;
        let db_session = request_sessions::Entity::find()
            .filter(request_sessions::Column::SessionId.eq(&session1.session_id))
            .one(test_db.connection_arc().as_ref())
            .await
            .unwrap()
            .unwrap();

        let mut active_session: request_sessions::ActiveModel = db_session.into();
        active_session.last_accessed_at = Set(chrono::Utc::now() - chrono::Duration::minutes(31));
        active_session
            .update(test_db.connection_arc().as_ref())
            .await
            .unwrap();

        // Try to reuse with expired session - should create new one
        let session2 = session_manager
            .get_or_create_session(Some(&encrypted_session_id), &visitor, Some(&context), None)
            .await
            .unwrap();

        assert_ne!(
            session1.session_id, session2.session_id,
            "Should create new session after expiry"
        );
        assert!(
            session2.is_new_session,
            "Expired session should result in new session"
        );
    }

    #[tokio::test]
    async fn test_session_with_invalid_cookie() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let crypto = Arc::new(
            temps_core::CookieCrypto::new(
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .unwrap(),
        );
        let session_manager =
            SessionManagerImpl::new(test_db.connection_arc().clone(), crypto.clone());

        let context = create_test_project_context(&test_db.connection_arc()).await;

        // Create visitor record in database first
        let visitor_id_i32 = create_test_visitor(
            &test_db.connection_arc(),
            "test-visitor-3",
            context.project.id,
            context.environment.id,
        )
        .await;

        let visitor = Visitor {
            visitor_id: "test-visitor-3".to_string(),
            visitor_id_i32,
            is_crawler: false,
            crawler_name: None,
        };

        // Request with invalid/corrupted cookie - should create new session
        let session = session_manager
            .get_or_create_session(
                Some("invalid-encrypted-data"),
                &visitor,
                Some(&context),
                None,
            )
            .await
            .unwrap();

        assert!(
            session.is_new_session,
            "Invalid cookie should result in new session"
        );
    }

    #[tokio::test]
    async fn test_session_cookie_encryption_decryption() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let crypto = Arc::new(
            temps_core::CookieCrypto::new(
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .unwrap(),
        );
        let session_manager =
            SessionManagerImpl::new(test_db.connection_arc().clone(), crypto.clone());

        let context = create_test_project_context(&test_db.connection_arc()).await;

        // Create visitor record in database first
        let visitor_id_i32 = create_test_visitor(
            &test_db.connection_arc(),
            "test-visitor-4",
            context.project.id,
            context.environment.id,
        )
        .await;

        let visitor = Visitor {
            visitor_id: "test-visitor-4".to_string(),
            visitor_id_i32,
            is_crawler: false,
            crawler_name: None,
        };

        // Create session
        let session = session_manager
            .get_or_create_session(None, &visitor, Some(&context), None)
            .await
            .unwrap();

        // Generate cookie
        let cookie = session_manager
            .generate_session_cookie(&session, false, None)
            .await
            .unwrap();

        // Extract encrypted value from cookie string
        let encrypted_session_id = cookie
            .split(';')
            .next()
            .unwrap()
            .trim()
            .split('=')
            .nth(1)
            .unwrap();

        // Verify we can decrypt it
        let decrypted = crypto.decrypt(encrypted_session_id).unwrap();
        assert_eq!(
            decrypted, session.session_id,
            "Decrypted session ID should match original"
        );

        // Verify double-decryption fails (prevents the bug we fixed)
        let double_decrypt_result = crypto.decrypt(&decrypted);
        assert!(
            double_decrypt_result.is_err(),
            "Double decryption should fail"
        );
    }

    #[tokio::test]
    async fn test_session_last_accessed_updated() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let crypto = Arc::new(
            temps_core::CookieCrypto::new(
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .unwrap(),
        );
        let session_manager =
            SessionManagerImpl::new(test_db.connection_arc().clone(), crypto.clone());

        let context = create_test_project_context(&test_db.connection_arc()).await;

        // Create visitor record in database first
        let visitor_id_i32 = create_test_visitor(
            &test_db.connection_arc(),
            "test-visitor-5",
            context.project.id,
            context.environment.id,
        )
        .await;

        let visitor = Visitor {
            visitor_id: "test-visitor-5".to_string(),
            visitor_id_i32,
            is_crawler: false,
            crawler_name: None,
        };

        // Create initial session
        let session1 = session_manager
            .get_or_create_session(None, &visitor, Some(&context), None)
            .await
            .unwrap();

        // Get initial last_accessed_at
        use temps_entities::request_sessions;
        let db_session1 = request_sessions::Entity::find()
            .filter(request_sessions::Column::SessionId.eq(&session1.session_id))
            .one(test_db.connection_arc().as_ref())
            .await
            .unwrap()
            .unwrap();
        let first_access = db_session1.last_accessed_at;

        // Wait a bit
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Generate cookie
        let cookie = session_manager
            .generate_session_cookie(&session1, false, None)
            .await
            .unwrap();

        let encrypted_session_id = cookie
            .split(';')
            .next()
            .unwrap()
            .trim()
            .split('=')
            .nth(1)
            .unwrap()
            .to_string();

        // Reuse session
        session_manager
            .get_or_create_session(Some(&encrypted_session_id), &visitor, Some(&context), None)
            .await
            .unwrap();

        // Check that last_accessed_at was updated
        let db_session2 = request_sessions::Entity::find()
            .filter(request_sessions::Column::SessionId.eq(&session1.session_id))
            .one(test_db.connection_arc().as_ref())
            .await
            .unwrap()
            .unwrap();
        let second_access = db_session2.last_accessed_at;

        assert!(
            second_access > first_access,
            "last_accessed_at should be updated on reuse"
        );
    }
}
