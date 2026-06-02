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
use tracing::{debug, warn};
use uuid::Uuid;

const ROUTE_PREFIX_TEMPS: &str = "/api/_temps";
const ROUTE_PREFIX_OTEL: &str = "/api/otel";
const VISITOR_ID_COOKIE: &str = "_temps_visitor_id";
const SESSION_ID_COOKIE: &str = "_temps_sid";

/// How long a request will wait for the route table's first load to complete
/// before falling back to the console. The proxy now binds its listeners before
/// the initial (DB-heavy) route load finishes, so a request that arrives in that
/// brief startup window would otherwise be sent to the console instead of its
/// real backend. This only applies until the first successful load; afterwards an
/// unmatched host falls through immediately, exactly as before.
const FIRST_LOAD_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

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
    ) -> PingoraResult<PeerSelection> {
        debug!(
            "Resolving peer for host: {}, path: {}, sni: {:?}",
            host, path, sni_hostname
        );

        // Route API and OTLP ingest paths directly to the console (API server),
        // regardless of the Host header — needed for OTLP push from containers
        // that use host.docker.internal as the host.
        if path.starts_with(ROUTE_PREFIX_TEMPS) || path.starts_with(ROUTE_PREFIX_OTEL) {
            debug!(
                "Routing temps API request to console: {}",
                self.server_config.console_address
            );
            let peer = Box::new(HttpPeer::new(
                self.server_config.console_address.clone(),
                false,
                "".to_string(),
            ));
            return Ok(PeerSelection {
                peer,
                container_id: None,
                container_name: None,
            });
        }

        // Try the in-memory route table across all three lookup strategies
        // (TLS/SNI, HTTP host, legacy). Returns the matched peer, if any.
        let sni_or_host = sni_hostname.unwrap_or(host);
        let lookup = |label: &str| -> Option<PeerSelection> {
            // 1. TLS/SNI-based routing
            if let Some(route_info) = self.route_table.get_route_by_sni(sni_or_host) {
                let selection = route_info.select_backend();
                debug!(
                    "{}: found TLS route via SNI/Host {} -> {}",
                    label, sni_or_host, selection.address
                );
                return Some(PeerSelection {
                    peer: Box::new(HttpPeer::new(
                        selection.address.clone(),
                        false,
                        "".to_string(),
                    )),
                    container_id: selection.container_id,
                    container_name: selection.container_name,
                });
            }

            // 2. HTTP Host-based routing (HTTP routes)
            if let Some(route_info) = self.route_table.get_route_by_host(host) {
                let project_id = route_info.project.as_ref().map(|p| p.id);
                let env_id = route_info.environment.as_ref().map(|e| e.id);
                let selection = route_info.select_backend();
                debug!(
                    "{}: found HTTP route for {} -> {} (project_id: {:?}, env_id: {:?})",
                    label, host, selection.address, project_id, env_id
                );
                return Some(PeerSelection {
                    peer: Box::new(HttpPeer::new(
                        selection.address.clone(),
                        false,
                        "".to_string(),
                    )),
                    container_id: selection.container_id,
                    container_name: selection.container_name,
                });
            }

            // 3. Legacy: the old get_route method for backwards compatibility
            if let Some(route_info) = self.route_table.get_route(host) {
                let project_id = route_info.project.as_ref().map(|p| p.id);
                let env_id = route_info.environment.as_ref().map(|e| e.id);
                let selection = route_info.select_backend();
                debug!(
                    "{}: found legacy route for {} -> {} (project_id: {:?}, env_id: {:?})",
                    label, host, selection.address, project_id, env_id
                );
                return Some(PeerSelection {
                    peer: Box::new(HttpPeer::new(
                        selection.address.clone(),
                        false,
                        "".to_string(),
                    )),
                    container_id: selection.container_id,
                    container_name: selection.container_name,
                });
            }

            None
        };

        if let Some(selection) = lookup("route lookup") {
            return Ok(selection);
        }

        // No route matched. If the table has never loaded — i.e. the proxy just
        // bound its listeners and the initial load is still running in the
        // background — hold this request briefly for the first load, then retry
        // the lookup before falling back to the console. After the first load
        // this branch is skipped and an unmatched host falls through immediately.
        if !self.route_table.has_loaded() {
            debug!(
                "Route table not loaded yet; waiting up to {:?} for first load (host: {})",
                FIRST_LOAD_WAIT, host
            );
            self.route_table.wait_until_loaded(FIRST_LOAD_WAIT).await;
            if let Some(selection) = lookup("route lookup (after first load)") {
                return Ok(selection);
            }
        }

        // No route found - route to console address as default
        warn!(
            "No route found in table for host: {}, routing to console (route_count={})",
            host,
            self.route_table.len()
        );
        let peer = Box::new(HttpPeer::new(
            self.server_config.console_address.clone(),
            false,
            "".to_string(),
        ));
        Ok(PeerSelection {
            peer,
            container_id: None,
            container_name: None,
        })
    }

    async fn has_custom_route(&self, host: &str) -> bool {
        self.lb_service.get_route(host).await.is_ok()
    }

    async fn has_route_for_host(&self, host: &str) -> bool {
        // Project deployment hosts live in the in-memory route table
        // (HTTP host map, SNI/TLS map, legacy lookup, wildcards). Operator
        // overrides live in the `custom_routes` table. The admin gate must
        // recognize a host as "known" if any of these match — otherwise it
        // 404s real project traffic the moment an admin host is configured.
        if self.route_table.get_route_by_host(host).is_some()
            || self.route_table.get_route_by_sni(host).is_some()
            || self.route_table.get_route(host).is_some()
        {
            return true;
        }
        self.lb_service.get_route(host).await.is_ok()
    }

    async fn get_lb_strategy(&self, _host: &str) -> Option<String> {
        Some("round_robin".to_string())
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
        attribution: &FirstVisitAttribution,
    ) -> Result<Visitor, Box<dyn std::error::Error + Send + Sync>> {
        let Some(ctx) = context else {
            return Err("Cannot create visitor without a project/environment context".into());
        };
        let project_id = ctx.project.id;
        let environment_id = ctx.environment.id;

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
            // First-visit attribution (set once, never overwritten)
            first_referrer: Set(attribution.referrer.clone()),
            first_referrer_hostname: Set(attribution.referrer_hostname.clone()),
            first_channel: Set(attribution.channel.clone()),
            first_utm_source: Set(attribution.utm_source.clone()),
            first_utm_medium: Set(attribution.utm_medium.clone()),
            first_utm_campaign: Set(attribution.utm_campaign.clone()),
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
        query_string: Option<&str>,
        current_hostname: Option<&str>,
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

        // Parse UTM parameters from query string
        let utm = query_string
            .map(temps_analytics::parse_utm_params)
            .unwrap_or_default();

        // Extract referrer hostname
        let referrer_hostname = referrer.and_then(temps_analytics::extract_referrer_hostname);

        // Compute marketing channel
        let channel =
            temps_analytics::get_channel(&utm, referrer_hostname.as_deref(), current_hostname);

        debug!(
            "UTM params: source={:?}, medium={:?}, campaign={:?}, channel={}",
            utm.utm_source, utm.utm_medium, utm.utm_campaign, channel
        );

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
            // UTM tracking fields
            utm_source: Set(utm.utm_source),
            utm_medium: Set(utm.utm_medium),
            utm_campaign: Set(utm.utm_campaign),
            utm_content: Set(utm.utm_content),
            utm_term: Set(utm.utm_term),
            // Channel attribution
            channel: Set(Some(channel.to_string())),
            referrer_hostname: Set(referrer_hostname),
            ..Default::default()
        };

        let session = session.insert(self.db.as_ref()).await?;

        debug!(
            "Created new session {} for visitor {} (channel: {})",
            session.session_id, visitor.visitor_id, channel
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
        deployments, environments, preset::Preset, projects, upstream_config::UpstreamList, visitor,
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
            .get_or_create_session(None, &visitor, Some(&context), None, None, None)
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
            .get_or_create_session(
                Some(&encrypted_session_id),
                &visitor,
                Some(&context),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(
            session1.session_id, session2.session_id,
            "Should reuse same session"
        );
        assert!(!session2.is_new_session, "Second session should not be new");

        // Third request - should still reuse
        let session3 = session_manager
            .get_or_create_session(
                Some(&encrypted_session_id),
                &visitor,
                Some(&context),
                None,
                None,
                None,
            )
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
            .get_or_create_session(None, &visitor, Some(&context), None, None, None)
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
            .get_or_create_session(
                Some(&encrypted_session_id),
                &visitor,
                Some(&context),
                None,
                None,
                None,
            )
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
                None,
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
            .get_or_create_session(None, &visitor, Some(&context), None, None, None)
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
            .get_or_create_session(None, &visitor, Some(&context), None, None, None)
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
            .get_or_create_session(
                Some(&encrypted_session_id),
                &visitor,
                Some(&context),
                None,
                None,
                None,
            )
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

    #[tokio::test]
    async fn test_new_visitor_stores_first_referrer_attribution() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let crypto = Arc::new(
            temps_core::CookieCrypto::new(
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .unwrap(),
        );
        let ip_service = create_mock_ip_service(test_db.connection_arc().clone());
        let visitor_manager =
            VisitorManagerImpl::new(test_db.connection_arc().clone(), crypto.clone(), ip_service);

        let context = create_test_project_context(&test_db.connection_arc()).await;

        // Create visitor with referrer attribution from Google organic search
        let attribution = FirstVisitAttribution {
            referrer: Some("https://www.google.com/search?q=temps+deploy".to_string()),
            referrer_hostname: Some("www.google.com".to_string()),
            channel: Some("Organic Search".to_string()),
            utm_source: None,
            utm_medium: None,
            utm_campaign: None,
        };

        let new_visitor = visitor_manager
            .get_or_create_visitor(
                None,
                Some(&context),
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)",
                Some("8.8.8.8"),
                &attribution,
            )
            .await
            .unwrap();

        // Verify attribution was stored on the visitor record
        let db_visitor = visitor::Entity::find_by_id(new_visitor.visitor_id_i32)
            .one(test_db.connection_arc().as_ref())
            .await
            .unwrap()
            .expect("Visitor should exist in database");

        assert_eq!(
            db_visitor.first_referrer,
            Some("https://www.google.com/search?q=temps+deploy".to_string())
        );
        assert_eq!(
            db_visitor.first_referrer_hostname,
            Some("www.google.com".to_string())
        );
        assert_eq!(db_visitor.first_channel, Some("Organic Search".to_string()));
        assert_eq!(db_visitor.first_utm_source, None);
        assert_eq!(db_visitor.first_utm_medium, None);
        assert_eq!(db_visitor.first_utm_campaign, None);
    }

    #[tokio::test]
    async fn test_new_visitor_stores_utm_attribution() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let crypto = Arc::new(
            temps_core::CookieCrypto::new(
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .unwrap(),
        );
        let ip_service = create_mock_ip_service(test_db.connection_arc().clone());
        let visitor_manager =
            VisitorManagerImpl::new(test_db.connection_arc().clone(), crypto.clone(), ip_service);

        let context = create_test_project_context(&test_db.connection_arc()).await;

        // Create visitor with UTM campaign attribution
        let attribution = FirstVisitAttribution {
            referrer: Some("https://twitter.com/post/123".to_string()),
            referrer_hostname: Some("twitter.com".to_string()),
            channel: Some("Paid Social".to_string()),
            utm_source: Some("twitter".to_string()),
            utm_medium: Some("paid_social".to_string()),
            utm_campaign: Some("launch_2026".to_string()),
        };

        let new_visitor = visitor_manager
            .get_or_create_visitor(
                None,
                Some(&context),
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64)",
                Some("1.2.3.4"),
                &attribution,
            )
            .await
            .unwrap();

        // Verify all UTM fields were stored
        let db_visitor = visitor::Entity::find_by_id(new_visitor.visitor_id_i32)
            .one(test_db.connection_arc().as_ref())
            .await
            .unwrap()
            .expect("Visitor should exist");

        assert_eq!(
            db_visitor.first_referrer,
            Some("https://twitter.com/post/123".to_string())
        );
        assert_eq!(
            db_visitor.first_referrer_hostname,
            Some("twitter.com".to_string())
        );
        assert_eq!(db_visitor.first_channel, Some("Paid Social".to_string()));
        assert_eq!(db_visitor.first_utm_source, Some("twitter".to_string()));
        assert_eq!(db_visitor.first_utm_medium, Some("paid_social".to_string()));
        assert_eq!(
            db_visitor.first_utm_campaign,
            Some("launch_2026".to_string())
        );
    }

    #[tokio::test]
    async fn test_returning_visitor_does_not_overwrite_first_referrer() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let crypto = Arc::new(
            temps_core::CookieCrypto::new(
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .unwrap(),
        );
        let ip_service = create_mock_ip_service(test_db.connection_arc().clone());
        let visitor_manager =
            VisitorManagerImpl::new(test_db.connection_arc().clone(), crypto.clone(), ip_service);

        let context = create_test_project_context(&test_db.connection_arc()).await;

        // First visit: from Google organic search
        let first_attribution = FirstVisitAttribution {
            referrer: Some("https://www.google.com/search?q=temps".to_string()),
            referrer_hostname: Some("www.google.com".to_string()),
            channel: Some("Organic Search".to_string()),
            utm_source: None,
            utm_medium: None,
            utm_campaign: None,
        };

        let new_visitor = visitor_manager
            .get_or_create_visitor(
                None,
                Some(&context),
                "Mozilla/5.0",
                Some("8.8.8.8"),
                &first_attribution,
            )
            .await
            .unwrap();

        // Generate encrypted cookie for the visitor
        let cookie = visitor_manager
            .generate_visitor_cookie(&new_visitor, false, Some(&context))
            .await
            .unwrap();
        let encrypted_visitor_id = cookie
            .split(';')
            .next()
            .unwrap()
            .trim()
            .split('=')
            .nth(1)
            .unwrap()
            .to_string();

        // Second visit: from Twitter (different referrer)
        let second_attribution = FirstVisitAttribution {
            referrer: Some("https://twitter.com/someone/status/123".to_string()),
            referrer_hostname: Some("twitter.com".to_string()),
            channel: Some("Organic Social".to_string()),
            utm_source: Some("twitter".to_string()),
            utm_medium: None,
            utm_campaign: None,
        };

        let returning_visitor = visitor_manager
            .get_or_create_visitor(
                Some(&encrypted_visitor_id),
                Some(&context),
                "Mozilla/5.0",
                Some("8.8.8.8"),
                &second_attribution,
            )
            .await
            .unwrap();

        // Should be the same visitor
        assert_eq!(new_visitor.visitor_id_i32, returning_visitor.visitor_id_i32);

        // Verify the FIRST referrer is still preserved (not overwritten)
        let db_visitor = visitor::Entity::find_by_id(returning_visitor.visitor_id_i32)
            .one(test_db.connection_arc().as_ref())
            .await
            .unwrap()
            .expect("Visitor should exist");

        assert_eq!(
            db_visitor.first_referrer,
            Some("https://www.google.com/search?q=temps".to_string()),
            "First referrer should NOT be overwritten on return visit"
        );
        assert_eq!(
            db_visitor.first_referrer_hostname,
            Some("www.google.com".to_string()),
            "First referrer hostname should NOT be overwritten"
        );
        assert_eq!(
            db_visitor.first_channel,
            Some("Organic Search".to_string()),
            "First channel should NOT be overwritten"
        );
    }

    #[tokio::test]
    async fn test_direct_visitor_has_direct_channel() {
        let test_db = TestDatabase::with_migrations().await.unwrap();
        let crypto = Arc::new(
            temps_core::CookieCrypto::new(
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .unwrap(),
        );
        let ip_service = create_mock_ip_service(test_db.connection_arc().clone());
        let visitor_manager =
            VisitorManagerImpl::new(test_db.connection_arc().clone(), crypto.clone(), ip_service);

        let context = create_test_project_context(&test_db.connection_arc()).await;

        // Direct visit: no referrer, no UTM
        let attribution = FirstVisitAttribution {
            referrer: None,
            referrer_hostname: None,
            channel: Some("Direct".to_string()),
            utm_source: None,
            utm_medium: None,
            utm_campaign: None,
        };

        let new_visitor = visitor_manager
            .get_or_create_visitor(
                None,
                Some(&context),
                "Mozilla/5.0",
                Some("1.2.3.4"),
                &attribution,
            )
            .await
            .unwrap();

        let db_visitor = visitor::Entity::find_by_id(new_visitor.visitor_id_i32)
            .one(test_db.connection_arc().as_ref())
            .await
            .unwrap()
            .expect("Visitor should exist");

        assert_eq!(db_visitor.first_referrer, None);
        assert_eq!(db_visitor.first_referrer_hostname, None);
        assert_eq!(db_visitor.first_channel, Some("Direct".to_string()));
    }
}
