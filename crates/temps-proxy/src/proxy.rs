use crate::service::challenge_service::ChallengeService;
use crate::service::ip_access_control_service::IpAccessControlService;
use crate::service::proxy_log_service::{CreateProxyLogRequest, ProxyLogService};
use crate::tls_fingerprint;
use crate::traits::*;
use async_trait::async_trait;
use axum::http::header;
use bytes::Bytes;
use cookie::Cookie;
use flate2::write::GzEncoder;
use flate2::Compression;
use pingora::http::StatusCode;
use pingora::Error;
use pingora_core::{
    upstreams::peer::{HttpPeer, Peer},
    Result,
};
use pingora_http::ResponseHeader;
use pingora_proxy::{FailToProxy, ProxyHttp, Session as PingoraSession};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;
use std::time::Instant;
use temps_database::DbConnection;
use temps_entities::{deployments, domains, environments, projects};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

// Constants
pub const VISITOR_ID_COOKIE: &str = "_temps_visitor_id";
pub const SESSION_ID_COOKIE: &str = "_temps_sid";
pub const ROUTE_PREFIX_TEMPS: &str = "/api/_temps";

// Helper functions for project-scoped cookie names
fn get_visitor_cookie_name(_project_id: Option<i32>) -> String {
    VISITOR_ID_COOKIE.to_string()
}

fn get_session_cookie_name(_project_id: Option<i32>) -> String {
    SESSION_ID_COOKIE.to_string()
}
pub const SERVER_NAME: &[u8; 5] = b"Temps";
pub const LB_SEED: u64 = 42;
pub const MAX_WEBHOOK_BODY_SIZE: usize = 16 * 1024;
pub const LOG_STATIC_ASSETS: bool = false;

/// Proxy context for tracking request state
pub struct ProxyContext {
    pub response_modified: bool,
    pub response_compressed: bool,
    pub upstream_response_headers: Option<ResponseHeader>,
    pub content_type: Option<String>,
    pub buffer: Vec<u8>,
    pub project: Option<Arc<projects::Model>>,
    pub environment: Option<Arc<environments::Model>>,
    pub deployment: Option<Arc<deployments::Model>>,
    pub request_id: String,
    pub start_time: Instant,
    pub method: String,
    pub path: String,
    pub query_string: Option<String>,
    pub host: String,
    pub user_agent: String,
    pub referrer: Option<String>,
    pub ip_address: Option<String>,
    pub visitor_id: Option<String>,
    pub visitor_id_i32: Option<i32>,
    pub session_id: Option<String>,
    pub session_id_i32: Option<i32>,
    pub is_new_session: bool,
    pub request_headers: Option<HashMap<String, String>>,
    pub response_headers: Option<HashMap<String, String>>,
    pub request_visitor_cookie: Option<String>,
    pub request_session_cookie: Option<String>,
    pub is_sse: bool,
    pub is_websocket: bool,
    pub skip_tracking: bool,
    pub routing_status: String,
    pub error_message: Option<String>,
    pub upstream_host: Option<String>,
    pub container_id: Option<String>,
    pub tls_fingerprint: Option<String>,
    pub tls_version: Option<String>,
    pub tls_cipher: Option<String>,
    /// SNI hostname from TLS handshake (for SNI-based routing)
    pub sni_hostname: Option<String>,
}

impl ProxyContext {
    /// Build a ProjectContext from the individual fields if all are present
    fn get_project_context(&self) -> Option<ProjectContext> {
        if let (Some(project), Some(environment), Some(deployment)) =
            (&self.project, &self.environment, &self.deployment)
        {
            Some(ProjectContext {
                project: project.clone(),
                environment: environment.clone(),
                deployment: deployment.clone(),
            })
        } else {
            None
        }
    }
}

/// Main load balancer proxy implementation using traits
pub struct LoadBalancer {
    upstream_resolver: Arc<dyn UpstreamResolver>,
    request_logger: Arc<dyn RequestLogger>,
    proxy_log_service: Arc<ProxyLogService>,
    project_context_resolver: Arc<dyn ProjectContextResolver>,
    visitor_manager: Arc<dyn VisitorManager>,
    session_manager: Arc<dyn SessionManager>,
    crypto: Arc<temps_core::CookieCrypto>,
    db: Arc<DbConnection>,
    config_service: Arc<temps_config::ConfigService>,
    ip_access_control_service: Arc<IpAccessControlService>,
    challenge_service: Arc<ChallengeService>,
}

impl LoadBalancer {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        upstream_resolver: Arc<dyn UpstreamResolver>,
        request_logger: Arc<dyn RequestLogger>,
        proxy_log_service: Arc<ProxyLogService>,
        project_context_resolver: Arc<dyn ProjectContextResolver>,
        visitor_manager: Arc<dyn VisitorManager>,
        session_manager: Arc<dyn SessionManager>,
        crypto: Arc<temps_core::CookieCrypto>,
        db: Arc<DbConnection>,
        config_service: Arc<temps_config::ConfigService>,
        ip_access_control_service: Arc<IpAccessControlService>,
        challenge_service: Arc<ChallengeService>,
    ) -> Self {
        Self {
            upstream_resolver,
            request_logger,
            proxy_log_service,
            project_context_resolver,
            visitor_manager,
            session_manager,
            crypto,
            db,
            config_service,
            ip_access_control_service,
            challenge_service,
        }
    }

    // Test-only accessors for integration tests
    #[cfg(test)]
    pub fn upstream_resolver(&self) -> &Arc<dyn UpstreamResolver> {
        &self.upstream_resolver
    }

    #[cfg(test)]
    pub fn project_context_resolver(&self) -> &Arc<dyn ProjectContextResolver> {
        &self.project_context_resolver
    }

    #[cfg(test)]
    pub fn visitor_manager(&self) -> &Arc<dyn VisitorManager> {
        &self.visitor_manager
    }

    #[cfg(test)]
    pub fn session_manager(&self) -> &Arc<dyn SessionManager> {
        &self.session_manager
    }

    /// Check if a request should be logged to proxy_logs based on path
    fn should_log_request(path: &str) -> bool {
        if LOG_STATIC_ASSETS {
            return true;
        }

        // Common static file extensions to skip
        let static_extensions = [
            ".js", ".mjs", ".cjs", ".css", ".scss", ".sass", ".less", ".map", ".png", ".jpg",
            ".jpeg", ".gif", ".svg", ".ico", ".webp", ".avif", ".woff", ".woff2", ".ttf", ".eot",
            ".otf", ".mp4", ".webm", ".ogg", ".mp3", ".wav", ".pdf", ".zip", ".tar", ".gz",
        ];

        let path_lower = path.to_lowercase();
        !static_extensions
            .iter()
            .any(|ext| path_lower.ends_with(ext))
    }

    fn get_host_header(&self, session: &PingoraSession) -> Result<String> {
        let host_with_port = if let Some(host) = session.req_header().headers.get("host") {
            host.to_str()
                .map_err(|_| Error::new_str("Invalid host header encoding"))?
                .to_string()
        } else if let Some(host) = session.req_header().uri.host() {
            // Try to get the :authority pseudo-header first (used in HTTP/2)
            host.to_string()
        } else {
            return Err(Error::new_str("Missing Host or :authority header"));
        };

        // Remove port from host before returning (e.g., "example.com:3000" -> "example.com")
        // This ensures we match against domain names in the route table correctly
        let host = host_with_port.split(':').next().unwrap_or(&host_with_port);
        Ok(host.to_string())
    }

    /// Extract TLS fingerprint with client characteristics
    ///
    /// Returns a fingerprint including:
    /// - TLS version and cipher (from TLS handshake)
    /// - Client IP address
    /// - User-Agent header
    ///
    /// This creates a unique identifier per person/device, ensuring
    /// each different visitor gets a different fingerprint.
    fn extract_tls_info(&self, session: &PingoraSession, ctx: &mut ProxyContext) {
        // Access SSL digest from the downstream session's digest
        // digest() returns Option<&Digest>, and Digest contains ssl_digest: Option<Arc<SslDigest>>
        if let Some(digest) = session.downstream_session.digest() {
            if let Some(ssl_digest) = &digest.ssl_digest {
                // Compute fingerprint with IP and user agent
                if let Some(fingerprint) = tls_fingerprint::compute_fingerprint_from_arc(
                    ssl_digest,
                    ctx.ip_address.as_deref(),
                    &ctx.user_agent,
                ) {
                    ctx.tls_fingerprint = Some(fingerprint.clone());

                    debug!(
                        "Extracted fingerprint: {} (IP: {}, UA: {}) for request_id={}",
                        fingerprint,
                        ctx.ip_address.as_ref().unwrap_or(&"unknown".to_string()),
                        ctx.user_agent,
                        ctx.request_id
                    );
                }

                // Extract TLS version and cipher for logging
                ctx.tls_version = Some(ssl_digest.version.to_string());
                ctx.tls_cipher = Some(ssl_digest.cipher.to_string());

                // Note: SNI hostname is not available in SslDigest in pingora-core 0.6.0
                // The SNI is captured during the TLS handshake callback in server.rs
                // For TLS routes, we use the HTTP Host header which typically matches the SNI

                debug!(
                    "TLS connection: {} with cipher {} for request_id={}",
                    ssl_digest.version, ssl_digest.cipher, ctx.request_id
                );
            } else {
                debug!(
                    "No SSL digest available in Digest for request_id={}",
                    ctx.request_id
                );
            }
        } else {
            debug!(
                "No digest available from downstream_session for request_id={}",
                ctx.request_id
            );
        }
    }

    /// Generate HTML for CAPTCHA challenge page
    fn generate_challenge_html(
        project_name: &str,
        environment_id: i32,
        ip_address: &str,
        identifier: &str,
        identifier_type: &str,
    ) -> String {
        // Generate a random challenge (32 hex characters)
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let bytes: Vec<u8> = (0..16).map(|_| rng.gen()).collect();
        let challenge = hex::encode(bytes);

        // Difficulty: 20 leading zero bits (~1 million attempts)
        // Typical solutions take ~2-5 seconds on modern browsers
        let difficulty = 20;

        // Load HTML template from file
        const CHALLENGE_HTML: &str = include_str!("../captcha/challenge.html");

        // Replace placeholders
        CHALLENGE_HTML
            .replace("{{PROJECT_NAME}}", project_name)
            .replace("{{ENVIRONMENT_ID}}", &environment_id.to_string())
            .replace("{{IP_ADDRESS}}", ip_address)
            .replace("{{CHALLENGE}}", &challenge)
            .replace("{{DIFFICULTY}}", &difficulty.to_string())
            .replace("{{IDENTIFIER}}", identifier)
            .replace("{{IDENTIFIER_TYPE}}", identifier_type)
    }

    async fn ensure_visitor_session(&self, ctx: &mut ProxyContext) -> Result<()> {
        // Only create visitor/session if we don't already have one
        if ctx.visitor_id.is_some() {
            return Ok(());
        }

        // Decrypt visitor cookie if present
        let visitor_id = ctx.request_visitor_cookie.as_ref().and_then(|encrypted| {
            match self.crypto.decrypt(encrypted) {
                Ok(decrypted) => Some(decrypted),
                Err(e) => {
                    debug!("Failed to decrypt visitor_id cookie: {}", e);
                    None
                }
            }
        });

        // Project context is already resolved in request_filter, use it here
        let project_context = if let (Some(project), Some(environment), Some(deployment)) =
            (&ctx.project, &ctx.environment, &ctx.deployment)
        {
            Some(ProjectContext {
                project: project.clone(),
                environment: environment.clone(),
                deployment: deployment.clone(),
            })
        } else {
            None
        };

        // Skip visitor/session creation for crawlers - only track real humans
        if let Some(crawler_name) =
            crate::crawler_detector::CrawlerDetector::get_crawler_name(Some(&ctx.user_agent))
        {
            debug!(
                "Crawler detected: {} ({}), skipping visitor/session creation for project {}",
                crawler_name,
                ctx.user_agent,
                project_context.as_ref().map(|p| p.project.id).unwrap_or(0)
            );
            return Ok(());
        }

        // Create visitor using the trait (only for non-crawlers)
        let visitor = match self
            .visitor_manager
            .get_or_create_visitor(
                visitor_id.as_deref(),
                project_context.as_ref(),
                &ctx.user_agent,
                ctx.ip_address.as_deref(),
            )
            .await
        {
            Ok(visitor) => visitor,
            Err(e) => {
                error!("Failed to get/create visitor: {:?}", e);
                return Err(Error::new_str("Failed to get/create visitor"));
            }
        };

        // Create session using the trait - pass encrypted cookie, not decrypted value
        let session = match self
            .session_manager
            .get_or_create_session(
                ctx.request_session_cookie.as_deref(),
                &visitor,
                project_context.as_ref(),
                ctx.referrer.as_deref(),
            )
            .await
        {
            Ok(session) => session,
            Err(e) => {
                error!("Failed to get/create session: {:?}", e);
                return Err(Error::new_str("Failed to get/create session"));
            }
        };

        ctx.visitor_id = Some(visitor.visitor_id.clone());
        ctx.visitor_id_i32 = Some(visitor.visitor_id_i32);
        ctx.session_id = Some(session.session_id.clone());
        ctx.session_id_i32 = Some(session.session_id_i32);
        ctx.is_new_session = session.is_new_session;

        // Log visitor debug
        debug!(
            "HTML request from visitor {} with session {} (new: {}) for project {}",
            visitor.visitor_id,
            session.session_id,
            session.is_new_session,
            project_context.as_ref().map(|p| p.project.id).unwrap_or(0)
        );

        Ok(())
    }

    async fn finalize_response(
        &self,
        session: &mut PingoraSession,
        upstream_response: &mut ResponseHeader,
        ctx: &mut ProxyContext,
    ) -> Result<()> {
        upstream_response.insert_header("X-Request-ID", &ctx.request_id)?;

        if let Some(project) = &ctx.project {
            upstream_response.insert_header("X-Project-ID", project.id.to_string())?;
        }
        if let Some(environment) = &ctx.environment {
            upstream_response.insert_header("X-Environment-ID", environment.id.to_string())?;
        }
        if let Some(deployment) = &ctx.deployment {
            upstream_response.insert_header("X-Deployment-ID", deployment.id.to_string())?;
        }

        // Apply security headers from project settings or global config
        self.apply_security_headers(upstream_response, ctx.project.as_deref())
            .await?;

        // Set visitor and session cookies
        self.set_tracking_cookies(session, upstream_response, ctx)
            .await?;

        // Capture response headers before logging
        let response_headers: HashMap<String, String> = upstream_response
            .headers
            .iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|val| (k.to_string(), val.to_string())))
            .collect();
        ctx.response_headers = Some(response_headers);

        self.log_request(session, upstream_response, ctx).await?;
        self.add_response_timing(upstream_response, ctx)?;

        Ok(())
    }

    /// Apply security headers from project settings or global config
    ///
    /// Attempts to use project-level security settings first (via temps-routes),
    /// then falls back to global config service settings if project is unavailable
    async fn apply_security_headers(
        &self,
        response: &mut ResponseHeader,
        project: Option<&projects::Model>,
    ) -> Result<()> {
        use temps_entities::deployment_config::SecurityHeadersConfig;

        // Map preset names to default header values
        fn get_preset_headers(preset: &str) -> SecurityHeadersConfig {
            match preset.to_lowercase().as_str() {
                "strict" => SecurityHeadersConfig {
                    preset: Some("strict".to_string()),
                    content_security_policy: Some(
                        "default-src 'self'; script-src 'self' 'unsafe-inline' 'unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data: https:; font-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'".to_string()
                    ),
                    x_frame_options: Some("DENY".to_string()),
                    strict_transport_security: Some("max-age=31536000; includeSubDomains; preload".to_string()),
                    referrer_policy: Some("strict-origin-when-cross-origin".to_string()),
                },
                "moderate" => SecurityHeadersConfig {
                    preset: Some("moderate".to_string()),
                    content_security_policy: Some(
                        "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: https:; font-src 'self' data:; connect-src 'self' https:; frame-ancestors 'self'".to_string()
                    ),
                    x_frame_options: Some("SAMEORIGIN".to_string()),
                    strict_transport_security: Some("max-age=31536000; includeSubDomains".to_string()),
                    referrer_policy: Some("no-referrer-when-downgrade".to_string()),
                },
                "permissive" => SecurityHeadersConfig {
                    preset: Some("permissive".to_string()),
                    content_security_policy: Some(
                        "default-src 'self'; script-src 'self' 'unsafe-inline' 'unsafe-eval' https:; style-src 'self' 'unsafe-inline' https:; img-src 'self' data: https:; font-src 'self' data: https:; connect-src 'self' https:; frame-ancestors *".to_string()
                    ),
                    x_frame_options: Some("ALLOW-FROM *".to_string()),
                    strict_transport_security: Some("max-age=31536000".to_string()),
                    referrer_policy: Some("origin".to_string()),
                },
                "disabled" => SecurityHeadersConfig {
                    preset: Some("disabled".to_string()),
                    content_security_policy: None,
                    x_frame_options: None,
                    strict_transport_security: None,
                    referrer_policy: None,
                },
                _ => SecurityHeadersConfig {
                    preset: Some(preset.to_string()),
                    content_security_policy: None,
                    x_frame_options: None,
                    strict_transport_security: None,
                    referrer_policy: None,
                },
            }
        }

        // Try to get security headers from project configuration first
        // Returns: None = no config (should check global), Some(config) = explicit config from project
        let (project_has_explicit_config, headers_config) = if let Some(proj) = project {
            debug!(
                "Applying security headers for project id={}, slug={}",
                proj.id, proj.slug
            );
            if let Some(ref deploy_config) = proj.deployment_config {
                debug!(
                    "Project {} has deployment_config, security field: {}",
                    proj.id,
                    deploy_config.security.is_some()
                );
                if let Some(ref security) = deploy_config.security {
                    debug!(
                        "Security config present: enabled={}, headers={}, rate_limiting={}, attack_mode={}",
                        security.enabled.unwrap_or(true),
                        security.headers.is_some(),
                        security.rate_limiting.is_some(),
                        security.attack_mode.is_some()
                    );

                    // Check if security is explicitly disabled at project level
                    if security.enabled == Some(false) {
                        debug!("Security headers are explicitly disabled at project level - skipping global fallback");
                        return Ok(());
                    }

                    if let Some(ref headers) = security.headers {
                        // Check if we have a preset but no individual headers configured
                        let has_preset = headers.preset.is_some();
                        let has_individual_headers = headers.content_security_policy.is_some()
                            || headers.x_frame_options.is_some()
                            || headers.strict_transport_security.is_some()
                            || headers.referrer_policy.is_some();

                        // Check if preset is "disabled"
                        let preset_disabled = has_preset
                            && headers.preset.as_ref().map(|p| p.to_lowercase())
                                == Some("disabled".to_string());

                        if preset_disabled {
                            debug!("Project has security headers preset set to 'disabled' - skipping global fallback");
                            return Ok(());
                        }

                        if has_preset && !has_individual_headers {
                            // Use preset to generate default headers
                            let preset_name = headers.preset.as_ref().unwrap();
                            debug!(
                                "Using preset '{}' to generate security headers from project config",
                                preset_name
                            );
                            (true, Some(get_preset_headers(preset_name)))
                        } else if has_individual_headers {
                            // Use individual headers as configured
                            debug!(
                                "Using custom security headers from project: preset={:?}, csp={}, x_frame={}, hsts={}, referrer={}",
                                headers.preset,
                                headers.content_security_policy.is_some(),
                                headers.x_frame_options.is_some(),
                                headers.strict_transport_security.is_some(),
                                headers.referrer_policy.is_some()
                            );
                            (true, Some(headers.clone()))
                        } else {
                            // No preset and no individual headers - project has config but empty, don't fall back to global
                            debug!("Project has security config but no headers or preset configured - skipping global fallback");
                            (true, None)
                        }
                    } else {
                        debug!("Project has security config but no headers configured (headers field is None) - allowing global fallback");
                        (false, None)
                    }
                } else {
                    debug!("Project has deployment_config but no security config (security field is None) - allowing global fallback");
                    (false, None)
                }
            } else {
                debug!("Project {} has no deployment_config field (is None) - allowing global fallback", proj.id);
                (false, None)
            }
        } else {
            debug!("No project context available for security headers - allowing global fallback");
            (false, None)
        };

        // If project didn't have explicit config, check global settings
        let headers_config = if !project_has_explicit_config && headers_config.is_none() {
            debug!("No explicit project-level security headers, checking global settings");
            match self.config_service.get_settings().await {
                Ok(settings) => {
                    let headers = &settings.security_headers;
                    if !headers.enabled {
                        debug!("Security headers are disabled in global settings");
                        return Ok(());
                    }
                    debug!("Using global security headers: preset={}", headers.preset);
                    Some(SecurityHeadersConfig {
                        preset: Some(headers.preset.clone()),
                        content_security_policy: headers.content_security_policy.clone(),
                        x_frame_options: Some(headers.x_frame_options.clone()),
                        strict_transport_security: Some(headers.strict_transport_security.clone()),
                        referrer_policy: Some(headers.referrer_policy.clone()),
                    })
                }
                Err(e) => {
                    warn!("Failed to get settings for security headers: {}", e);
                    return Ok(()); // Don't fail the request if we can't get settings
                }
            }
        } else {
            headers_config
        };

        // Apply headers from configuration
        if let Some(config) = headers_config {
            let mut headers_applied = Vec::new();

            // Apply Content-Security-Policy
            if let Some(ref csp) = config.content_security_policy {
                if !csp.is_empty() {
                    if let Err(e) = response.insert_header("Content-Security-Policy", csp) {
                        warn!("Failed to set Content-Security-Policy header: {}", e);
                    } else {
                        headers_applied.push("Content-Security-Policy");
                    }
                }
            }

            // Apply X-Frame-Options
            if let Some(ref x_frame) = config.x_frame_options {
                if !x_frame.is_empty() {
                    if let Err(e) = response.insert_header("X-Frame-Options", x_frame) {
                        warn!("Failed to set X-Frame-Options header: {}", e);
                    } else {
                        headers_applied.push("X-Frame-Options");
                    }
                }
            }

            // Apply Strict-Transport-Security
            if let Some(ref hsts) = config.strict_transport_security {
                if !hsts.is_empty() {
                    if let Err(e) = response.insert_header("Strict-Transport-Security", hsts) {
                        warn!("Failed to set Strict-Transport-Security header: {}", e);
                    } else {
                        headers_applied.push("Strict-Transport-Security");
                    }
                }
            }

            // Apply Referrer-Policy
            if let Some(ref policy) = config.referrer_policy {
                if !policy.is_empty() {
                    if let Err(e) = response.insert_header("Referrer-Policy", policy) {
                        warn!("Failed to set Referrer-Policy header: {}", e);
                    } else {
                        headers_applied.push("Referrer-Policy");
                    }
                }
            }

            if headers_applied.is_empty() {
                debug!("No security headers to apply (all configs empty)");
            } else {
                debug!(
                    "Applied {} security headers: {:?}",
                    headers_applied.len(),
                    headers_applied
                );
            }
        } else {
            debug!("No security headers configuration available");
        }

        Ok(())
    }

    fn is_https_request(&self, session: &PingoraSession) -> bool {
        session
            .req_header()
            .headers
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            .map(|proto| proto == "https")
            .unwrap_or_else(|| session.req_header().uri.scheme_str() == Some("https"))
    }

    async fn handle_acme_http_challenge(&self, host: &str, path: &str) -> Result<Option<String>> {
        const ACME_CHALLENGE_PREFIX: &str = "/.well-known/acme-challenge/";

        if !path.starts_with(ACME_CHALLENGE_PREFIX) {
            return Ok(None);
        }

        let token = &path[ACME_CHALLENGE_PREFIX.len()..];
        if token.is_empty() {
            debug!("Empty ACME challenge token in path: {}", path);
            return Ok(None);
        }

        debug!(
            "Looking up ACME HTTP-01 challenge for domain: {}, token: {}",
            host, token
        );

        let domain_record = domains::Entity::find()
            .filter(domains::Column::Domain.eq(host))
            .filter(domains::Column::HttpChallengeToken.eq(token))
            .one(self.db.as_ref())
            .await
            .map_err(|e| {
                error!("Database error looking up ACME challenge: {:?}", e);
                Error::new_str("Database error during ACME challenge lookup")
            })?;

        if let Some(domain) = domain_record {
            if let Some(key_auth) = domain.http_challenge_key_authorization {
                debug!(
                    "Found ACME HTTP-01 challenge for domain: {}, returning key authorization",
                    host
                );
                return Ok(Some(key_auth));
            } else {
                debug!(
                    "Domain {} has matching token but no key authorization",
                    host
                );
            }
        } else {
            debug!(
                "No matching ACME challenge found for domain: {}, token: {}",
                host, token
            );
        }

        Ok(None)
    }

    async fn log_request(
        &self,
        session: &PingoraSession,
        upstream_response: &ResponseHeader,
        ctx: &ProxyContext,
    ) -> Result<()> {
        let headers_map: HashMap<String, String> = upstream_response
            .headers
            .iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|val| (k.to_string(), val.to_string())))
            .collect();

        let response_headers_json = serde_json::to_value(&headers_map)
            .map_err(|_| Error::new_str("Failed to serialize response headers."))?;

        let request_headers_json = if ctx.request_headers.is_none() {
            let req_headers_map: HashMap<String, String> = session
                .req_header()
                .headers
                .iter()
                .filter_map(|(k, v)| v.to_str().ok().map(|val| (k.to_string(), val.to_string())))
                .collect();
            Some(
                serde_json::to_value(&req_headers_map)
                    .map_err(|_| Error::new_str("Failed to serialize request headers."))?,
            )
        } else {
            ctx.request_headers
                .as_ref()
                .map(serde_json::to_value)
                .transpose()
                .map_err(|_| Error::new_str("Failed to serialize request headers."))?
        };

        // Skip logging for internal temps API routes
        if ctx.path.starts_with(ROUTE_PREFIX_TEMPS) {
            return Ok(());
        }

        // Log ALL requests (not just page visits)
        let project_context = if let (Some(project), Some(environment), Some(deployment)) =
            (&ctx.project, &ctx.environment, &ctx.deployment)
        {
            Some(ProjectContext {
                project: project.clone(),
                environment: environment.clone(),
                deployment: deployment.clone(),
            })
        } else {
            None
        };

        let visitor = if let (Some(visitor_id), Some(visitor_id_i32)) =
            (&ctx.visitor_id, ctx.visitor_id_i32)
        {
            Some(Visitor {
                visitor_id: visitor_id.clone(),
                visitor_id_i32,
                is_crawler: false, // We'd need to track this properly
                crawler_name: None,
            })
        } else {
            None
        };

        let session_obj = if let (Some(session_id), Some(session_id_i32), Some(visitor_id_i32)) =
            (&ctx.session_id, ctx.session_id_i32, ctx.visitor_id_i32)
        {
            Some(crate::traits::Session {
                session_id: session_id.clone(),
                session_id_i32,
                visitor_id_i32,
                is_new_session: ctx.is_new_session,
            })
        } else {
            None
        };

        let status_code = upstream_response.status.as_u16() as i32;
        let started_at = match chrono::Duration::from_std(ctx.start_time.elapsed()) {
            Ok(duration) => chrono::Utc::now() - duration,
            Err(e) => {
                error!("Failed to convert duration: {:?}", e);
                chrono::Utc::now()
            }
        };
        let finished_at = chrono::Utc::now();

        let log_data = RequestLogData {
            request_id: ctx.request_id.clone(),
            host: ctx.host.clone(),
            method: ctx.method.clone(),
            path: ctx.path.clone(),
            status_code,
            user_agent: ctx.user_agent.clone(),
            referrer: ctx.referrer.clone(),
            ip_address: ctx.ip_address.clone(),
            started_at,
            finished_at,
            request_headers: request_headers_json.unwrap_or(serde_json::Value::Null),
            response_headers: response_headers_json,
            visitor,
            session: session_obj,
            project_context,
        };

        if let Err(e) = self.request_logger.log_request(log_data).await {
            error!("Failed to log request: {:?}", e);
        }

        // Asynchronously log to proxy_logs table (skip static assets)
        if Self::should_log_request(&ctx.path) {
            // Extract request size from Content-Length header
            let request_size = ctx
                .request_headers
                .as_ref()
                .and_then(|h| h.get("content-length"))
                .and_then(|v| v.parse::<i64>().ok());

            // Extract response size from Content-Length header
            let response_size = ctx
                .response_headers
                .as_ref()
                .and_then(|h| h.get("content-length"))
                .and_then(|v| v.parse::<i64>().ok());

            // Extract cache status from response headers
            let cache_status = ctx
                .response_headers
                .as_ref()
                .and_then(|h| h.get("x-cache").or_else(|| h.get("cf-cache-status")))
                .cloned();

            let proxy_log_service = self.proxy_log_service.clone();
            let proxy_log_request = CreateProxyLogRequest {
                method: ctx.method.clone(),
                path: ctx.path.clone(),
                query_string: ctx.query_string.clone(),
                host: ctx.host.clone(),
                status_code: status_code as i16,
                response_time_ms: Some(ctx.start_time.elapsed().as_millis() as i32),
                request_source: "proxy".to_string(),
                is_system_request: ctx.path.starts_with(ROUTE_PREFIX_TEMPS),
                routing_status: ctx.routing_status.clone(),
                project_id: ctx.project.as_ref().map(|p| p.id),
                environment_id: ctx.environment.as_ref().map(|e| e.id),
                deployment_id: ctx.deployment.as_ref().map(|d| d.id),
                session_id: ctx.session_id_i32,
                visitor_id: ctx.visitor_id_i32,
                container_id: ctx.container_id.clone(),
                upstream_host: ctx.upstream_host.clone(),
                error_message: ctx.error_message.clone(),
                client_ip: ctx.ip_address.clone(),
                user_agent: Some(ctx.user_agent.clone()),
                referrer: ctx.referrer.clone(),
                request_id: ctx.request_id.clone(),
                // Service will enrich these fields
                ip_geolocation_id: None,
                browser: None,
                browser_version: None,
                operating_system: None,
                device_type: None,
                is_bot: None,
                bot_name: None,
                request_size_bytes: request_size,
                response_size_bytes: response_size,
                cache_status,
                request_headers: ctx
                    .request_headers
                    .as_ref()
                    .and_then(|h| serde_json::to_value(h).ok()),
                response_headers: ctx
                    .response_headers
                    .as_ref()
                    .and_then(|h| serde_json::to_value(h).ok()),
            };

            // Only log HTML pages (skip static assets like .js, .css, .svg, etc.)
            let should_log = ctx
                .response_headers
                .as_ref()
                .and_then(|h| h.get("content-type"))
                .map(|ct| ct.starts_with("text/html"))
                .unwrap_or(false);

            if should_log {
                // Spawn async task to avoid blocking the response
                tokio::spawn(async move {
                    if let Err(e) = proxy_log_service.create(proxy_log_request).await {
                        warn!("Failed to create proxy log: {:?}", e);
                    }
                });
            }
        }

        Ok(())
    }

    #[allow(dead_code)]
    fn is_page_visit(&self, upstream_response: &ResponseHeader, _ctx: &ProxyContext) -> bool {
        let mut is_page_visit = upstream_response
            .headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|content_type| {
                content_type.starts_with("text/html")
                    || content_type.starts_with("text/plain")
                    || content_type.starts_with("application/json")
            })
            .unwrap_or(false);

        // Note: Removed is_web_app check - all projects are now preset-based
        // Page visits are determined by URL patterns

        let status_code = upstream_response.status.as_u16();
        if status_code >= 400 {
            is_page_visit = true;
        }

        is_page_visit
    }

    fn add_response_timing(
        &self,
        upstream_response: &mut ResponseHeader,
        ctx: &ProxyContext,
    ) -> Result<()> {
        let duration = ctx.start_time.elapsed();
        info!(
            "[{}] {} {} {} - {}ms - {}",
            ctx.method,
            ctx.host,
            ctx.path,
            upstream_response.status.as_u16(),
            duration.as_millis(),
            ctx.ip_address.clone().unwrap_or_default()
        );
        upstream_response
            .insert_header("X-Response-Time", format!("{}ms", duration.as_millis()))?;
        Ok(())
    }

    /// Check if a request path should be logged (HTML pages only, skip static assets)
    fn should_log_static_request(path: &str) -> bool {
        path == "/" || path.ends_with(".html") || path.ends_with(".htm") || !path.contains('.')
        // SPA routes without extension
    }

    /// Create and spawn proxy log for static file serving
    fn log_static_request(
        &self,
        ctx: &ProxyContext,
        status_code: i16,
        routing_status: &str,
        static_dir: &str,
        error_message: Option<String>,
        response_size: Option<i64>,
    ) {
        // Only log HTML pages (skip .js, .css, .svg, etc.)
        if !Self::should_log_static_request(&ctx.path) {
            return;
        }

        let proxy_log_service = self.proxy_log_service.clone();
        let proxy_log_request = CreateProxyLogRequest {
            method: ctx.method.clone(),
            path: ctx.path.clone(),
            query_string: ctx.query_string.clone(),
            host: ctx.host.clone(),
            status_code,
            response_time_ms: Some(ctx.start_time.elapsed().as_millis() as i32),
            request_source: "proxy".to_string(),
            is_system_request: ctx.path.starts_with(ROUTE_PREFIX_TEMPS),
            routing_status: routing_status.to_string(),
            project_id: ctx.project.as_ref().map(|p| p.id),
            environment_id: ctx.environment.as_ref().map(|e| e.id),
            deployment_id: ctx.deployment.as_ref().map(|d| d.id),
            session_id: ctx.session_id_i32,
            visitor_id: ctx.visitor_id_i32,
            container_id: None,
            upstream_host: Some(format!("static://{}", static_dir)),
            error_message,
            client_ip: ctx.ip_address.clone(),
            user_agent: Some(ctx.user_agent.clone()),
            referrer: ctx.referrer.clone(),
            request_id: ctx.request_id.clone(),
            ip_geolocation_id: None,
            browser: None,
            browser_version: None,
            operating_system: None,
            device_type: None,
            is_bot: None,
            bot_name: None,
            request_size_bytes: None,
            response_size_bytes: response_size,
            cache_status: None,
            request_headers: ctx
                .request_headers
                .as_ref()
                .and_then(|h| serde_json::to_value(h).ok()),
            response_headers: None,
        };

        tokio::spawn(async move {
            if let Err(e) = proxy_log_service.create(proxy_log_request).await {
                warn!("Failed to create proxy log for static file: {:?}", e);
            }
        });
    }

    /// Set visitor and session cookies on the response
    /// This can be called from both finalize_response and early_request_filter (for static files)
    async fn set_tracking_cookies(
        &self,
        session: &mut PingoraSession,
        response: &mut ResponseHeader,
        ctx: &ProxyContext,
    ) -> Result<()> {
        // Set visitor cookie using the trait
        if let Some(visitor_id) = &ctx.visitor_id {
            let project_id = ctx.project.as_ref().map(|p| p.id);
            let expected_cookie_name = get_visitor_cookie_name(project_id);

            let has_valid_visitor_cookie = session
                .req_header()
                .headers
                .get_all("Cookie")
                .iter()
                .filter_map(|cookie_header| cookie_header.to_str().ok())
                .flat_map(|cookie_str| Cookie::split_parse(cookie_str).filter_map(Result::ok))
                .any(|cookie| {
                    cookie.name() == expected_cookie_name
                        && self.crypto.decrypt(cookie.value()).is_ok()
                });

            if !has_valid_visitor_cookie {
                let visitor = Visitor {
                    visitor_id: visitor_id.clone(),
                    visitor_id_i32: ctx.visitor_id_i32.unwrap_or(0),
                    is_crawler: false, // We'd need to track this properly
                    crawler_name: None,
                };

                let is_https = self.is_https_request(session);
                let visitor_cookie = match self
                    .visitor_manager
                    .generate_visitor_cookie(&visitor, is_https, ctx.get_project_context().as_ref())
                    .await
                {
                    Ok(cookie) => cookie,
                    Err(e) => {
                        error!("Failed to generate visitor cookie: {:?}", e);
                        return Err(Error::new_str("Failed to generate visitor cookie"));
                    }
                };
                response.append_header("Set-Cookie", visitor_cookie)?;
            }
        }

        // Set session cookie using the trait
        // IMPORTANT: Always regenerate the cookie to refresh the max_age expiration time
        // This prevents the cookie from expiring after 30 minutes even though the session is still active
        if let Some(session_id) = &ctx.session_id {
            let session_obj = crate::traits::Session {
                session_id: session_id.clone(),
                session_id_i32: ctx.session_id_i32.unwrap_or(0),
                visitor_id_i32: ctx.visitor_id_i32.unwrap_or(0),
                is_new_session: ctx.is_new_session,
            };

            let is_https = self.is_https_request(session);
            let session_cookie = match self
                .session_manager
                .generate_session_cookie(&session_obj, is_https, ctx.get_project_context().as_ref())
                .await
            {
                Ok(cookie) => cookie,
                Err(e) => {
                    error!("Failed to generate session cookie: {:?}", e);
                    return Err(Error::new_str("Failed to generate session cookie"));
                }
            };
            response.append_header("Set-Cookie", session_cookie)?;
        }

        Ok(())
    }

    /// Serve a static file from the filesystem
    /// Returns Ok(true) if file was served, Ok(false) if file not found, Err on error
    async fn serve_static_file(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
        static_dir: &str,
    ) -> Result<bool> {
        use std::path::PathBuf;
        use tokio::fs;

        let mut requested_path = ctx.path.trim_start_matches('/');

        // Handle root path -> index.html
        if requested_path.is_empty() {
            requested_path = "index.html";
        }

        // Security: ALWAYS join with base static directory
        // Never trust absolute paths from database - always enforce that static files
        // must be within the configured static directory to prevent path traversal
        let static_dir_path = PathBuf::from(static_dir);

        // Strip leading slash if present (treat all paths as relative)
        let relative_static_dir = static_dir_path
            .strip_prefix("/")
            .unwrap_or(&static_dir_path);

        // Always join with base static directory from config
        let absolute_static_dir = self.config_service.static_dir().join(relative_static_dir);

        let file_path = absolute_static_dir.join(requested_path);

        // Security check: ensure the resolved path is still within static_dir
        let canonical_static_dir = fs::canonicalize(&absolute_static_dir).await.map_err(|e| {
            Error::because(
                pingora::ErrorType::FileOpenError,
                format!("Failed to canonicalize static dir: {}", e),
                e,
            )
        })?;

        // Try to canonicalize the file path, but handle the case where it doesn't exist
        let canonical_file_path = match fs::canonicalize(&file_path).await {
            Ok(path) => path,
            Err(_) => {
                // File doesn't exist - try with index.html for SPA routing
                if !requested_path.contains('.') {
                    // Likely a SPA route, serve index.html
                    let index_path = absolute_static_dir.join("index.html");
                    match fs::canonicalize(&index_path).await {
                        Ok(path) => path,
                        Err(_) => return Ok(false), // No index.html, file not found
                    }
                } else {
                    return Ok(false); // File not found
                }
            }
        };

        // Ensure the file is within the static directory (prevent path traversal)
        if !canonical_file_path.starts_with(&canonical_static_dir) {
            warn!(
                "Path traversal attempt detected: {} -> {}",
                requested_path,
                canonical_file_path.display()
            );
            return Ok(false);
        }

        // Check if it's a directory -> serve index.html
        let final_path = if canonical_file_path.is_dir() {
            canonical_file_path.join("index.html")
        } else {
            canonical_file_path
        };

        // Read the file
        let file_content = fs::read(&final_path).await.map_err(|e| {
            Error::because(
                pingora::ErrorType::FileOpenError,
                format!("Failed to read file: {}", e),
                e,
            )
        })?;

        // Generate ETag for cache validation
        let etag = Self::generate_etag(&file_content);

        // Check If-None-Match header for 304 Not Modified response
        if let Some(if_none_match) = session
            .req_header()
            .headers
            .get("if-none-match")
            .and_then(|v| v.to_str().ok())
        {
            if if_none_match == etag {
                debug!("ETag match - returning 304 Not Modified for: {}", ctx.path);
                let mut resp = ResponseHeader::build(StatusCode::NOT_MODIFIED, None)?;
                resp.insert_header("ETag", &etag)?;
                resp.insert_header("X-Request-ID", &ctx.request_id)?;

                // Add cache headers
                if Self::is_cacheable_static_asset(requested_path) {
                    resp.insert_header(
                        header::CACHE_CONTROL,
                        "public, max-age=31536000, immutable",
                    )?;
                } else {
                    resp.insert_header(
                        header::CACHE_CONTROL,
                        "public, max-age=0, must-revalidate",
                    )?;
                }

                // CRITICAL: Set tracking cookies even for 304 responses to keep sessions alive
                // Without this, visitors won't get cookies on cached root URLs (/) and events will fail
                self.set_tracking_cookies(session, &mut resp, ctx).await?;

                session.write_response_header(Box::new(resp), false).await?;
                session.write_response_body(None, true).await?;
                return Ok(true);
            }
        }

        // Infer content type
        let content_type = Self::infer_content_type(final_path.to_str().unwrap_or("index.html"));

        // Check if we should compress the content
        let client_accepts_gzip = Self::accepts_gzip(session);
        let should_compress =
            client_accepts_gzip && Self::should_compress_content(content_type, file_content.len());

        // Compress content if appropriate
        let (final_content, is_compressed) = if should_compress {
            match Self::compress_gzip(&file_content) {
                Ok(compressed) => {
                    // Only use compression if it actually reduces size
                    if compressed.len() < file_content.len() {
                        debug!(
                            "Compressed {} from {} to {} bytes ({:.1}% reduction)",
                            ctx.path,
                            file_content.len(),
                            compressed.len(),
                            (1.0 - (compressed.len() as f64 / file_content.len() as f64)) * 100.0
                        );
                        (compressed, true)
                    } else {
                        debug!(
                            "Skipping compression for {} - compressed size ({}) >= original ({})",
                            ctx.path,
                            compressed.len(),
                            file_content.len()
                        );
                        (file_content, false)
                    }
                }
                Err(e) => {
                    warn!("Failed to compress {}: {:?}", ctx.path, e);
                    (file_content, false)
                }
            }
        } else {
            (file_content, false)
        };

        // Build response
        let mut resp = ResponseHeader::build(200, None)?;
        resp.insert_header(header::CONTENT_TYPE, content_type)?;
        resp.insert_header(header::CONTENT_LENGTH, final_content.len().to_string())?;
        resp.insert_header("X-Request-ID", &ctx.request_id)?;
        resp.insert_header("ETag", &etag)?;

        // Add compression header if compressed
        if is_compressed {
            resp.insert_header("Content-Encoding", "gzip")?;
            resp.insert_header("Vary", "Accept-Encoding")?;
        }

        // Add cache headers for static assets
        if Self::is_cacheable_static_asset(requested_path) {
            resp.insert_header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")?;
        } else {
            resp.insert_header(header::CACHE_CONTROL, "public, max-age=0, must-revalidate")?;
        }

        // Set visitor and session tracking cookies for static file responses
        self.set_tracking_cookies(session, &mut resp, ctx).await?;

        // Write response
        session.write_response_header(Box::new(resp), false).await?;
        session
            .write_response_body(Some(Bytes::from(final_content)), true)
            .await?;

        Ok(true)
    }

    /// Serve embedded WASM files for CAPTCHA solver
    /// Returns Ok(true) if file was served, Ok(false) if path doesn't match
    async fn serve_wasm_file(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<bool> {
        // Check if this is a WASM file request (use actual wasm-bindgen generated filenames)
        if ctx.path == "/api/__temps/temps_captcha_wasm.js" {
            let content = include_str!("../../temps-captcha-wasm/pkg/temps_captcha_wasm.js");
            let mut resp = ResponseHeader::build(StatusCode::OK, None)?;
            resp.insert_header(
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            )?;
            resp.insert_header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")?;
            resp.insert_header("X-Request-ID", &ctx.request_id)?;

            session.write_response_header(Box::new(resp), false).await?;
            session
                .write_response_body(Some(Bytes::from(content.as_bytes().to_vec())), true)
                .await?;

            debug!("Served WASM JavaScript bindings: {}", ctx.path);
            return Ok(true);
        } else if ctx.path == "/api/__temps/temps_captcha_wasm_bg.wasm" {
            let content = include_bytes!("../../temps-captcha-wasm/pkg/temps_captcha_wasm_bg.wasm");
            let mut resp = ResponseHeader::build(StatusCode::OK, None)?;
            resp.insert_header(header::CONTENT_TYPE, "application/wasm")?;
            resp.insert_header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")?;
            resp.insert_header("X-Request-ID", &ctx.request_id)?;

            session.write_response_header(Box::new(resp), false).await?;
            session
                .write_response_body(Some(Bytes::from(content.to_vec())), true)
                .await?;

            debug!("Served WASM binary module: {}", ctx.path);
            return Ok(true);
        }

        Ok(false) // Not a WASM file request
    }

    /// Infer content type from file extension
    pub fn infer_content_type(file_path: &str) -> &'static str {
        let extension = std::path::Path::new(file_path)
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("");

        match extension.to_lowercase().as_str() {
            "html" => "text/html; charset=utf-8",
            "css" => "text/css; charset=utf-8",
            "js" | "mjs" | "cjs" => "application/javascript; charset=utf-8",
            "json" => "application/json; charset=utf-8",
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "svg" => "image/svg+xml",
            "webp" => "image/webp",
            "ico" => "image/x-icon",
            "woff" => "font/woff",
            "woff2" => "font/woff2",
            "ttf" => "font/ttf",
            "eot" => "application/vnd.ms-fontobject",
            "pdf" => "application/pdf",
            "txt" | "log" => "text/plain; charset=utf-8",
            "xml" => "application/xml; charset=utf-8",
            "zip" => "application/zip",
            _ => "application/octet-stream",
        }
    }

    /// Check if a file should have long-term caching headers
    pub fn is_cacheable_static_asset(path: &str) -> bool {
        let cacheable_patterns = [
            "/assets/",
            "/static/",
            "/_next/static/",
            ".chunk.",
            ".hash.",
        ];

        cacheable_patterns
            .iter()
            .any(|pattern| path.contains(pattern))
    }

    /// Generate ETag from file content using SHA-256 hash
    fn generate_etag(content: &[u8]) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        content.hash(&mut hasher);
        let hash = hasher.finish();
        format!("W/\"{:x}\"", hash)
    }

    /// Check if content should be compressed based on Content-Type
    fn should_compress_content(content_type: &str, content_length: usize) -> bool {
        // Don't compress if content is too small (overhead not worth it)
        if content_length < 1024 {
            return false;
        }

        // Compress text-based content types
        let compressible_types = [
            "text/html",
            "text/css",
            "text/javascript",
            "text/plain",
            "text/xml",
            "application/javascript",
            "application/json",
            "application/xml",
            "application/x-javascript",
            "image/svg+xml",
        ];

        compressible_types
            .iter()
            .any(|ct| content_type.starts_with(ct))
    }

    /// Compress content using gzip
    fn compress_gzip(content: &[u8]) -> Result<Vec<u8>> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(content)
            .map_err(|_| Error::new_str("Failed to compress content"))?;
        encoder
            .finish()
            .map_err(|_| Error::new_str("Failed to finish compression"))
    }

    /// Check if client accepts gzip encoding
    fn accepts_gzip(session: &PingoraSession) -> bool {
        session
            .req_header()
            .headers
            .get("accept-encoding")
            .and_then(|v| v.to_str().ok())
            .map(|ae| ae.contains("gzip"))
            .unwrap_or(false)
    }
}

#[async_trait]
impl ProxyHttp for LoadBalancer {
    type CTX = ProxyContext;

    fn new_ctx(&self) -> Self::CTX {
        ProxyContext {
            response_modified: false,
            response_compressed: false,
            upstream_response_headers: None,
            content_type: None,
            buffer: vec![],
            project: None,
            environment: None,
            deployment: None,
            request_id: Uuid::new_v4().to_string(),
            start_time: Instant::now(),
            method: String::new(),
            path: String::new(),
            query_string: None,
            host: String::new(),
            user_agent: String::new(),
            referrer: None,
            ip_address: None,
            visitor_id: None,
            visitor_id_i32: None,
            session_id: None,
            session_id_i32: None,
            is_new_session: false,
            request_headers: None,
            response_headers: None,
            request_visitor_cookie: None,
            request_session_cookie: None,
            is_sse: false,
            is_websocket: false,
            skip_tracking: false,
            routing_status: "pending".to_string(),
            error_message: None,
            upstream_host: None,
            container_id: None,
            tls_fingerprint: None,
            tls_version: None,
            tls_cipher: None,
            sni_hostname: None,
        }
    }

    async fn early_request_filter(
        &self,
        session: &mut PingoraSession,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        // Extract client IP address FIRST (needed for TLS fingerprinting)
        let client_ip = session
            .client_addr()
            .map(|addr| {
                let addr_str = addr.to_string();
                addr_str.split(':').next().unwrap_or("unknown").to_string()
            })
            .unwrap_or_else(|| "unknown".to_string());
        ctx.ip_address = Some(client_ip.clone());

        // Extract user-agent FIRST (needed for TLS fingerprinting)
        ctx.user_agent = session
            .req_header()
            .headers
            .get("user-agent")
            .map(|h| h.to_str().unwrap_or_default().to_string())
            .unwrap_or_default();

        // Extract TLS fingerprint AFTER IP and user-agent are set
        self.extract_tls_info(session, ctx);

        // Get the request path early to check if this is a CAPTCHA/WASM request
        let path = session.req_header().uri.path();

        // WASM files must bypass IP access control since they're needed for challenge solving
        let is_wasm_request = path.starts_with("/api/__temps/temps_captcha_wasm");

        // Check if IP is blocked - this happens at infrastructure level before any processing
        // WASM routes bypass this check since they're needed for challenge solving
        if !is_wasm_request {
            match self.ip_access_control_service.is_blocked(&client_ip).await {
                Ok(is_blocked) => {
                    if is_blocked {
                        warn!("Blocked request from IP: {}", client_ip);

                        // Return 403 Forbidden immediately
                        let mut response = ResponseHeader::build(StatusCode::FORBIDDEN, None)?;
                        response.insert_header("Content-Type", "text/plain")?;
                        response.insert_header("X-Blocked-Reason", "IP address blocked")?;

                        session
                            .write_response_header(Box::new(response), true)
                            .await?;
                        session
                            .write_response_body(
                                Some(Bytes::from("Access denied: IP address blocked")),
                                true,
                            )
                            .await?;

                        // Return error to stop request processing
                        return Err(Error::because(
                            pingora::ErrorType::HTTPStatus(403),
                            "IP address blocked",
                            pingora_core::Error::new(pingora::ErrorType::HTTPStatus(403)),
                        ));
                    }
                }
                Err(e) => {
                    // Log error but don't block request if IP check fails
                    error!("Failed to check IP access control for {}: {}", client_ip, e);
                }
            }
        }

        // Check if client accepts SSE (Server-Sent Events)
        let accepts_sse = session
            .req_header()
            .headers
            .get("accept")
            .and_then(|v| v.to_str().ok())
            .map(|accept| accept.contains("text/event-stream"))
            .unwrap_or(false);
        let is_chunked = session
            .req_header()
            .headers
            .get("transfer-encoding")
            .and_then(|v| v.to_str().ok())
            .map(|transfer_encoding| transfer_encoding.to_lowercase().contains("chunked"))
            .unwrap_or(false);
        // Check if this is a WebSocket upgrade request
        let is_websocket_upgrade = session
            .req_header()
            .headers
            .get("upgrade")
            .and_then(|v| v.to_str().ok())
            .map(|upgrade| upgrade.to_lowercase().contains("websocket"))
            .unwrap_or(false);

        // Check if the request path suggests it might return streaming data
        let req_path = session.req_header().uri.path().to_string();
        let is_streaming_path = req_path.starts_with("/api/")
            || req_path.contains("/stream")
            || req_path.contains("/events")
            || req_path.contains("/logs")
            || req_path.contains("/webhook");

        if accepts_sse || is_websocket_upgrade || is_chunked || is_streaming_path {
            // Disable compression for SSE/WebSocket/streaming paths
            // compression requires buffering which breaks streaming responses
            session.upstream_compression.adjust_level(0);
            debug!(
                "Disabling compression for: sse={}, ws={}, chunked={}, path={}",
                accepts_sse, is_websocket_upgrade, is_chunked, req_path
            );

            if accepts_sse {
                ctx.is_sse = true;
                debug!("SSE request detected, disabling compression for streaming");
            }

            if is_websocket_upgrade {
                ctx.is_websocket = true;
                debug!("WebSocket upgrade detected, disabling compression for streaming");
            }

            if is_streaming_path {
                debug!(
                    "Streaming path detected: {}, disabling compression",
                    req_path
                );
            }
        } else {
            // Enable compression for normal requests
            session.upstream_compression.adjust_level(6);
        }

        Ok(())
    }

    async fn request_filter(
        &self,
        session: &mut PingoraSession,
        ctx: &mut Self::CTX,
    ) -> Result<bool>
    where
        Self::CTX: Send + Sync,
    {
        // Set the started_at time here
        ctx.start_time = Instant::now();

        // Add the request ID to the request headers
        session
            .req_header_mut()
            .insert_header("X-Request-ID", &ctx.request_id)?;

        ctx.host = self.get_host_header(session)?;
        ctx.method = session.req_header().method.to_string();
        ctx.path = session.req_header().uri.path().to_string();
        ctx.query_string = session.req_header().uri.query().map(|q| q.to_string());
        ctx.user_agent = session
            .req_header()
            .headers
            .get("user-agent")
            .map(|h| h.to_str().unwrap_or_default().to_string())
            .unwrap_or_default();

        // Extract client IP address early (needed for attack mode checks)
        if let Some(addr) = session.client_addr() {
            let addr_str = addr.to_string();
            let client_ip = addr_str.split(':').next().unwrap_or_default();
            ctx.ip_address = Some(client_ip.to_string());
        }

        // Resolve project context early to set routing status for all requests
        let project_context = self
            .project_context_resolver
            .resolve_context(&ctx.host)
            .await;

        if let Some(project_ctx) = &project_context {
            ctx.project = Some(project_ctx.project.clone());
            ctx.environment = Some(project_ctx.environment.clone());
            ctx.deployment = Some(project_ctx.deployment.clone());
            ctx.routing_status = "routed".to_string();

            // Check if this is a CAPTCHA endpoint - allow these to bypass attack mode
            // This includes:
            // - /api/_temps/captcha/* - Challenge verification endpoints
            // - /api/__temps/temps_captcha_wasm.js - WASM JavaScript bindings
            // - /api/__temps/temps_captcha_wasm_bg.wasm - WASM binary module
            let is_captcha_endpoint = ctx.path.starts_with("/api/_temps/captcha")
                || ctx.path.starts_with("/api/__temps/temps_captcha_wasm");

            // Check if attack mode is enabled (project-wide setting)
            if !is_captcha_endpoint && project_ctx.project.attack_mode {
                // Attack mode REQUIRES HTTPS for JA4 fingerprinting
                // Reject HTTP connections to prevent bot bypass
                debug!(
                    "Attack mode enabled for environment {}, fingerprint: {:?}, user_agent: {}",
                    project_ctx.environment.id, ctx.tls_fingerprint, ctx.user_agent
                );

                let (identifier_type, identifier) = if let Some(ref fingerprint) =
                    ctx.tls_fingerprint
                {
                    ("ja4", fingerprint.as_str())
                } else {
                    // No TLS fingerprint means HTTP connection - reject it
                    debug!(
                        "Attack mode: HTTPS required for environment {} (HTTP request from {})",
                        project_ctx.environment.id,
                        ctx.ip_address.as_ref().unwrap_or(&"unknown".to_string())
                    );

                    // Return 426 Upgrade Required
                    let mut response =
                        ResponseHeader::build(StatusCode::from_u16(426).unwrap(), None)?;
                    response.insert_header("Content-Type", "text/html; charset=utf-8")?;
                    response.insert_header("Upgrade", "TLS/1.2, TLS/1.3")?;
                    response.insert_header("Connection", "Upgrade")?;

                    session
                        .write_response_header(Box::new(response), true)
                        .await?;

                    let html = format!(
                        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>HTTPS Required</title>
    <style>
        body {{ font-family: system-ui, -apple-system, sans-serif; display: flex; align-items: center; justify-content: center; min-height: 100vh; margin: 0; background: linear-gradient(135deg, #667eea 0%, #764ba2 100%); }}
        .container {{ background: white; border-radius: 16px; padding: 40px; max-width: 500px; text-align: center; box-shadow: 0 20px 60px rgba(0,0,0,0.3); }}
        h1 {{ color: #1a202c; margin-bottom: 16px; }}
        p {{ color: #4a5568; line-height: 1.6; }}
        .icon {{ font-size: 64px; margin-bottom: 16px; }}
    </style>
</head>
<body>
    <div class="container">
        <div class="icon">🔒</div>
        <h1>HTTPS Required</h1>
        <p>This site requires a secure connection (HTTPS) for enhanced security and bot protection.</p>
        <p>Please use <strong>https://</strong> instead of http://</p>
    </div>
</body>
</html>"#
                    );

                    session
                        .write_response_body(Some(Bytes::from(html)), true)
                        .await?;

                    return Err(Error::because(
                        pingora::ErrorType::HTTPStatus(426),
                        "HTTPS required in attack mode",
                        pingora_core::Error::new(pingora::ErrorType::HTTPStatus(426)),
                    ));
                };

                let is_challenge_completed = self
                    .challenge_service
                    .is_challenge_completed(project_ctx.environment.id, identifier, identifier_type)
                    .await
                    .unwrap_or(false);

                if !is_challenge_completed {
                    debug!(
                        "Attack mode: Challenge required for {} {} on environment {}",
                        identifier_type, identifier, project_ctx.environment.id
                    );

                    // Return 403 with HTML challenge page
                    let mut response = ResponseHeader::build(StatusCode::FORBIDDEN, None)?;
                    response.insert_header("Content-Type", "text/html; charset=utf-8")?;
                    response.insert_header("X-Challenge-Required", "true")?;

                    session
                        .write_response_header(Box::new(response), true)
                        .await?;

                    // Generate HTML challenge page
                    let html = Self::generate_challenge_html(
                        &project_ctx.project.name,
                        project_ctx.environment.id,
                        ctx.ip_address.as_ref().unwrap_or(&"unknown".to_string()),
                        identifier,
                        identifier_type,
                    );

                    session
                        .write_response_body(Some(Bytes::from(html)), true)
                        .await?;

                    // Return error to stop request processing
                    return Err(Error::because(
                        pingora::ErrorType::HTTPStatus(403),
                        "Challenge required",
                        pingora_core::Error::new(pingora::ErrorType::HTTPStatus(403)),
                    ));
                }
            }
        } else {
            ctx.routing_status = "no_project".to_string();
        }

        // Serve embedded WASM files for CAPTCHA solver (must come before general request handling)
        if let Ok(true) = self.serve_wasm_file(session, ctx).await {
            ctx.routing_status = "captcha_wasm".to_string();
            return Ok(true); // Request handled
        }

        // Check if this host should redirect
        if let Some((redirect_url, status_code)) = self
            .project_context_resolver
            .get_redirect_info(&ctx.host)
            .await
        {
            debug!(
                request_id = %ctx.request_id,
                host = %ctx.host,
                redirect_url = %redirect_url,
                status_code = status_code,
                "Redirecting request"
            );

            // Build redirect response
            let mut resp = ResponseHeader::build(status_code, None)?;
            resp.insert_header("Location", &redirect_url)?;
            resp.insert_header("Content-Length", "0")?;

            // Add CORS headers for redirect responses
            resp.insert_header("Access-Control-Allow-Origin", "*")?;

            // Update context for logging
            ctx.routing_status = "redirected".to_string();

            session.write_response_header(Box::new(resp), true).await?;
            return Ok(true); // Skip proxying
        }

        // Capture request headers
        let request_headers: HashMap<String, String> = session
            .req_header()
            .headers
            .iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|val| (k.to_string(), val.to_string())))
            .collect();
        ctx.request_headers = Some(request_headers);

        debug!(
            request_id = %ctx.request_id,
            method = %ctx.method,
            host = %ctx.host,
            path = %ctx.path,
            user_agent = %ctx.user_agent,
            "Incoming request"
        );

        // Store encrypted cookie values for later processing
        // Use project-scoped cookie names if project context is available
        let project_id = ctx.project.as_ref().map(|p| p.id);
        let visitor_cookie_name = get_visitor_cookie_name(project_id);
        let session_cookie_name = get_session_cookie_name(project_id);

        ctx.request_visitor_cookie = session
            .req_header()
            .headers
            .get_all("Cookie")
            .iter()
            .filter_map(|cookie_header| cookie_header.to_str().ok())
            .flat_map(|cookie_str| Cookie::split_parse(cookie_str).filter_map(Result::ok))
            .find(|cookie| cookie.name() == visitor_cookie_name)
            .map(|cookie| cookie.value().to_string());

        ctx.request_session_cookie = session
            .req_header()
            .headers
            .get_all("Cookie")
            .iter()
            .filter_map(|cookie_header| cookie_header.to_str().ok())
            .flat_map(|cookie_str| Cookie::split_parse(cookie_str).filter_map(Result::ok))
            .find(|cookie| cookie.name() == session_cookie_name)
            .map(|cookie| cookie.value().to_string());

        // Get IP from the connection
        // Add X-Forwarded-For header with client IP (already extracted in request_filter)
        if let Some(ref ip) = ctx.ip_address {
            session
                .req_header_mut()
                .insert_header("X-Forwarded-For", ip.as_str())?;
        }

        // Add X-Forwarded-Proto header to indicate the original protocol (HTTP/HTTPS)
        let proto = if self.is_https_request(session) {
            "https"
        } else {
            "http"
        };
        session
            .req_header_mut()
            .insert_header("X-Forwarded-Proto", proto)?;

        ctx.referrer = session
            .req_header()
            .headers
            .get("referer")
            .map(|h| h.to_str().unwrap_or_default().to_string());

        // Handle ACME HTTP-01 challenges
        if let Some(key_authorization) = self
            .handle_acme_http_challenge(&ctx.host, &ctx.path)
            .await?
        {
            debug!(
                "Serving ACME HTTP-01 challenge response for {}{} (request_id={})",
                ctx.host, ctx.path, ctx.request_id
            );

            let key_auth_bytes = Bytes::from(key_authorization.clone());
            let content_length = key_auth_bytes.len();

            let mut resp = ResponseHeader::build(200, None)?;
            resp.insert_header("Content-Type", "text/plain")?;
            resp.insert_header("Cache-Control", "no-cache")?;
            resp.insert_header("X-Request-ID", &ctx.request_id)?;
            resp.insert_header("Content-Length", content_length.to_string())?;
            resp.insert_header("Connection", "close")?;

            session.write_response_header(Box::new(resp), false).await?;
            session
                .write_response_body(Some(key_auth_bytes), true)
                .await?;

            // Log this ACME challenge response for debugging
            info!(
                "ACME challenge completed: {} {} - 200 OK - {}ms",
                ctx.method,
                ctx.path,
                ctx.start_time.elapsed().as_millis()
            );

            // Update routing status for potential logging
            ctx.routing_status = "acme_challenge".to_string();

            return Ok(true);
        }

        // Check for redirects or static file serving
        if let Some(redirect_info) = self
            .project_context_resolver
            .get_redirect_info(&ctx.host)
            .await
        {
            let mut resp = ResponseHeader::build(redirect_info.1, None)?;
            resp.insert_header(header::LOCATION, &redirect_info.0)?;
            session.write_response_header(Box::new(resp), true).await?;
            return Ok(true);
        }

        // Check if this is a static deployment using route table
        if let Some(static_dir) = self
            .project_context_resolver
            .get_static_path(&ctx.host)
            .await
        {
            debug!(
                "Static deployment detected for {}: {}",
                ctx.host, static_dir
            );

            // IMPORTANT: Skip static file serving for /api/_temps/* paths
            // These must ALWAYS be proxied to the console address (admin API)
            if !ctx.path.starts_with("/api/_temps/") {
                // Create visitor and session BEFORE serving static file
                // This ensures tracking cookies are set for HTML pages
                if let Err(e) = self.ensure_visitor_session(ctx).await {
                    error!("Failed to ensure visitor session for static file: {:?}", e);
                    // Continue serving the file even if visitor/session creation fails
                }

                // Serve static file
                match self.serve_static_file(session, ctx, &static_dir).await {
                    Ok(served) => {
                        if served {
                            debug!("Served static file: {}", ctx.path);
                            ctx.routing_status = "static_file".to_string();

                            // Log successful static file serving (HTML only)
                            self.log_static_request(
                                ctx,
                                200,
                                "static_file",
                                &static_dir,
                                None,
                                None,
                            );

                            return Ok(true); // Request handled
                        } else {
                            // Static file not found - return 404 instead of falling through
                            error!(
                                "Static file not found: {} (static dir: {})",
                                ctx.path, static_dir
                            );
                            let mut resp = ResponseHeader::build(StatusCode::NOT_FOUND, None)?;
                            resp.insert_header(header::CONTENT_TYPE, "text/html")?;

                            // Set tracking cookies for 404 response
                            self.set_tracking_cookies(session, &mut resp, ctx).await?;

                            session.write_response_header(Box::new(resp), false).await?;
                            session
                                .write_response_body(
                                    Some(bytes::Bytes::from(
                                        b"<html><body><h1>404 - File Not Found</h1></body></html>"
                                            .to_vec(),
                                    )),
                                    true,
                                )
                                .await?;

                            // Log 404 static file not found (HTML only)
                            self.log_static_request(
                                ctx,
                                404,
                                "static_file_not_found",
                                &static_dir,
                                Some("Static file not found".to_string()),
                                Some(
                                    b"<html><body><h1>404 - File Not Found</h1></body></html>".len()
                                        as i64,
                                ),
                            );

                            return Ok(true); // Request handled with 404
                        }
                    }
                    Err(e) => {
                        // Static directory error (doesn't exist, permissions, etc.) - return 500
                        error!(
                            "Failed to serve static file {} from {}: {}",
                            ctx.path, static_dir, e
                        );
                        let mut resp =
                            ResponseHeader::build(StatusCode::INTERNAL_SERVER_ERROR, None)?;
                        resp.insert_header(header::CONTENT_TYPE, "text/html")?;

                        // Set tracking cookies for 500 response
                        self.set_tracking_cookies(session, &mut resp, ctx).await?;

                        session.write_response_header(Box::new(resp), false).await?;
                        session
                        .write_response_body(
                            Some(bytes::Bytes::from(
                                b"<html><body><h1>500 - Static Directory Error</h1><p>The static files directory could not be accessed.</p></body></html>"
                                    .to_vec(),
                            )),
                            true,
                        )
                        .await?;

                        // Log 500 static directory error (HTML only)
                        let error_msg = format!("Static directory error: {}", e);
                        self.log_static_request(
                        ctx,
                        500,
                        "static_directory_error",
                        &static_dir,
                        Some(error_msg),
                        Some(
                            b"<html><body><h1>500 - Static Directory Error</h1><p>The static files directory could not be accessed.</p></body></html>"
                                .len() as i64,
                        ),
                    );

                        return Ok(true); // Request handled with error response
                    }
                }
            }
            // If we reach here and path starts with /api/_temps/,
            // fall through to normal proxying logic (will be proxied to console)
        }

        Ok(false)
    }

    fn upstream_response_filter(
        &self,
        _session: &mut PingoraSession,
        upstream_response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        debug!("Upstream response filter headers: {:?}", upstream_response);
        ctx.upstream_response_headers = Some(upstream_response.clone());

        let headers_map: HashMap<String, String> = upstream_response
            .headers
            .iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|val| (k.to_string(), val.to_string())))
            .collect();
        ctx.response_headers = Some(headers_map.clone());

        // Detect SSE by content-type header from upstream
        let is_sse = upstream_response
            .headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|ct| ct.contains("text/event-stream"))
            .unwrap_or(false);

        if is_sse {
            ctx.is_sse = true;
            ctx.skip_tracking = true; // Skip visitor/session tracking for SSE streams
            debug!("SSE response detected from upstream");
        }

        Ok(())
    }

    fn response_body_filter(
        &self,
        _session: &mut PingoraSession,
        body: &mut Option<Bytes>,
        _end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> Result<Option<std::time::Duration>>
    where
        Self::CTX: Send + Sync,
    {
        // For SSE or WebSocket responses, pass through immediately without buffering
        if ctx.is_sse || ctx.is_websocket {
            if let Some(chunk) = body {
                let stream_type = if ctx.is_sse { "SSE" } else { "WebSocket" };
                debug!("Streaming {} chunk: {} bytes", stream_type, chunk.len());
            }
        }

        // Pass all responses through without buffering
        Ok(None)
    }

    async fn response_filter(
        &self,
        session: &mut PingoraSession,
        upstream_response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()>
    where
        Self::CTX: Send + Sync,
    {
        // Store content type for later use
        ctx.content_type = Some(
            upstream_response
                .headers
                .get("content-type")
                .and_then(|h| h.to_str().ok())
                .unwrap_or_default()
                .to_string(),
        );

        // Detect chunked transfer encoding in response
        let is_chunked_response = upstream_response
            .headers
            .get("transfer-encoding")
            .and_then(|v| v.to_str().ok())
            .map(|te| te.contains("chunked"))
            .unwrap_or(false);

        // For chunked responses, ensure Transfer-Encoding is preserved
        if is_chunked_response {
            debug!("Chunked transfer encoding response detected - preserving for streaming");
            debug!(
                "Current headers before preservation: {:?}",
                upstream_response.headers.get_all("transfer-encoding")
            );
            debug!(
                "Content-Encoding header: {:?}",
                upstream_response.headers.get("content-encoding")
            );

            // Ensure Transfer-Encoding header is present and set to chunked
            // This tells Pingora and the client that the response is streamed in chunks
            if !upstream_response.headers.contains_key("transfer-encoding") {
                upstream_response.insert_header("Transfer-Encoding", "chunked")?;
            }
        }

        // Handle SSE (Server-Sent Events) special headers
        if ctx.is_sse {
            // Ensure required SSE headers are present for proper streaming
            if !upstream_response.headers.contains_key("cache-control") {
                upstream_response.insert_header("Cache-Control", "no-cache")?;
            }
            if !upstream_response.headers.contains_key("connection") {
                upstream_response.insert_header("Connection", "keep-alive")?;
            }
            if !upstream_response.headers.contains_key("x-accel-buffering") {
                upstream_response.insert_header("X-Accel-Buffering", "no")?;
            }

            debug!(
                "SSE stream response for path={}, setting streaming headers",
                ctx.path
            );

            // Skip visitor tracking and session creation for SSE
            ctx.skip_tracking = true;
        }

        // Handle WebSocket upgrade responses
        if ctx.is_websocket {
            // WebSocket requires specific upgrade headers - don't modify them
            debug!(
                "WebSocket upgrade response for path={}, preserving upgrade headers",
                ctx.path
            );

            // Skip visitor tracking and session creation for WebSocket
            ctx.skip_tracking = true;
        }

        // Determine if this needs visitor tracking
        let is_html_content = ctx
            .content_type
            .as_ref()
            .map(|ct| ct.starts_with("text/html"))
            .unwrap_or(false);

        let status_code = upstream_response.status.as_u16();
        let is_error_page = status_code >= 400;

        let is_static_asset = ctx.path.contains(".")
            && (ctx.path.ends_with(".js")
                || ctx.path.ends_with(".css")
                || ctx.path.ends_with(".png")
                || ctx.path.ends_with(".jpg")
                || ctx.path.ends_with(".jpeg")
                || ctx.path.ends_with(".gif")
                || ctx.path.ends_with(".svg")
                || ctx.path.ends_with(".ico")
                || ctx.path.ends_with(".woff")
                || ctx.path.ends_with(".woff2")
                || ctx.path.ends_with(".ttf")
                || ctx.path.ends_with(".eot"));

        let is_api_endpoint = ctx.path.starts_with("/api/") || ctx.path.starts_with("/_temps/");

        // Check if we should track this visitor using the trait
        let should_track = self
            .visitor_manager
            .should_track_visitor(
                &ctx.path,
                ctx.content_type.as_deref(),
                status_code,
                None, // We'll pass project context if available
            )
            .await;

        // Only create visitor/session for appropriate requests (skip for SSE)
        if !ctx.skip_tracking
            && should_track
            && (is_html_content || is_error_page)
            && !is_static_asset
            && !is_api_endpoint
        {
            if let Err(e) = self.ensure_visitor_session(ctx).await {
                error!("Failed to ensure visitor session: {:?}", e);
            }
        } else {
            debug!(
                "Skipping visitor creation for: path={}, content_type={:?}, status={}, skip_tracking={}",
                ctx.path, ctx.content_type, status_code, ctx.skip_tracking
            );
        }

        // Finalize the response
        if let Err(e) = self
            .finalize_response(session, upstream_response, ctx)
            .await
        {
            error!("Failed to finalize response: {:?}", e);
            return Err(Error::new_str("Failed to finalize response"));
        }

        Ok(())
    }

    async fn upstream_peer(
        &self,
        session: &mut PingoraSession,
        ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        let domain = self.get_host_header(session)?;
        let path = session.req_header().uri.path().to_string();

        debug!(
            "Resolving upstream peer for domain: {}, path: {}",
            domain, path
        );

        // Use the upstream resolver trait
        // Pass SNI hostname for TLS-based routing
        let peer = self
            .upstream_resolver
            .resolve_peer(&domain, &path, ctx.sni_hostname.as_deref())
            .await?;

        // Populate context with upstream information
        // Use the Peer trait's address() method
        let addr = peer.address();
        ctx.upstream_host = Some(addr.to_string());

        // Try to extract container ID from peer metadata if available
        // The container ID might be set by the upstream resolver
        if let Some(deployment) = &ctx.deployment {
            // For now, we'll use the deployment ID as a proxy for container tracking
            // In the future, the upstream resolver could provide actual container IDs
            ctx.container_id = Some(format!("deployment-{}", deployment.id));
        }

        Ok(peer)
    }

    fn fail_to_connect(
        &self,
        _session: &mut PingoraSession,
        _peer: &HttpPeer,
        _ctx: &mut Self::CTX,
        e: Box<Error>,
    ) -> Box<Error> {
        error!("Failed to connect to upstream: {:?}", e);
        e
    }

    async fn fail_to_proxy(
        &self,
        session: &mut PingoraSession,
        e: &Error,
        ctx: &mut Self::CTX,
    ) -> FailToProxy
    where
        Self::CTX: Send + Sync,
    {
        error!(
            "Failed to proxy: {:?} | request_id={} client_ip={} host={} method={} path={}",
            e,
            ctx.request_id,
            ctx.ip_address.as_deref().unwrap_or("unknown"),
            ctx.host,
            ctx.method,
            ctx.path
        );

        let mut error_code = 500;
        let can_reuse_downstream = false;

        // Update context with error
        ctx.error_message = Some(e.to_string());
        ctx.routing_status = "error".to_string();

        let mut header = match ResponseHeader::build(503, None) {
            Ok(header) => header,
            Err(e) => {
                error!("Failed to build response header: {:?}", e);
                return FailToProxy {
                    error_code,
                    can_reuse_downstream,
                };
            }
        };

        if let Err(e) = header.insert_header(header::SERVER, &SERVER_NAME[..]) {
            error!("Failed to insert SERVER header: {:?}", e);
        }
        if let Err(e) = header.insert_header(header::DATE, "Sun, 06 Nov 1994 08:49:37 GMT") {
            error!("Failed to insert DATE header: {:?}", e);
        }
        if let Err(e) = header.insert_header(header::CACHE_CONTROL, "private, no-store") {
            error!("Failed to insert CACHE_CONTROL header: {:?}", e);
        }

        if let Err(e) = session.write_response_header(Box::new(header), false).await {
            error!("Failed to write response header: {:?}", e);
            return FailToProxy {
                error_code,
                can_reuse_downstream,
            };
        }

        if let Err(e) = session
            .write_response_body(Some(Bytes::from("Service Unavailable")), true)
            .await
        {
            error!("Failed to write response body: {:?}", e);
        }

        error_code = 503;

        // Asynchronously log failed proxy request (skip static assets)
        if Self::should_log_request(&ctx.path) {
            // Extract request size from Content-Length header
            let request_size = ctx
                .request_headers
                .as_ref()
                .and_then(|h| h.get("content-length"))
                .and_then(|v| v.parse::<i64>().ok());

            // For failed requests, response size is the error message size
            let response_size = Some("Service Unavailable".len() as i64);

            let proxy_log_service = self.proxy_log_service.clone();
            let proxy_log_request = CreateProxyLogRequest {
                method: ctx.method.clone(),
                path: ctx.path.clone(),
                query_string: None,
                host: ctx.host.clone(),
                status_code: error_code as i16,
                response_time_ms: Some(ctx.start_time.elapsed().as_millis() as i32),
                request_source: "proxy".to_string(),
                is_system_request: ctx.path.starts_with(ROUTE_PREFIX_TEMPS),
                routing_status: ctx.routing_status.clone(),
                project_id: ctx.project.as_ref().map(|p| p.id),
                environment_id: ctx.environment.as_ref().map(|e| e.id),
                deployment_id: ctx.deployment.as_ref().map(|d| d.id),
                session_id: ctx.session_id_i32,
                visitor_id: ctx.visitor_id_i32,
                container_id: None,
                upstream_host: None,
                error_message: ctx.error_message.clone(),
                client_ip: ctx.ip_address.clone(),
                user_agent: Some(ctx.user_agent.clone()),
                referrer: ctx.referrer.clone(),
                request_id: ctx.request_id.clone(),
                ip_geolocation_id: None,
                browser: None,
                browser_version: None,
                operating_system: None,
                device_type: None,
                is_bot: None,
                bot_name: None,
                request_size_bytes: request_size,
                response_size_bytes: response_size,
                cache_status: None,
                request_headers: ctx
                    .request_headers
                    .as_ref()
                    .and_then(|h| serde_json::to_value(h).ok()),
                response_headers: ctx
                    .response_headers
                    .as_ref()
                    .and_then(|h| serde_json::to_value(h).ok()),
            };

            // Spawn async task to avoid blocking
            tokio::spawn(async move {
                if let Err(e) = proxy_log_service.create(proxy_log_request).await {
                    warn!("Failed to create proxy log for failed request: {:?}", e);
                }
            });
        }

        FailToProxy {
            error_code,
            can_reuse_downstream,
        }
    }
}
