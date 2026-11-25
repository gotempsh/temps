// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Visitor and session identification: the tracking cookies and the in-process visitor/session resolution.

use super::*;

pub const VISITOR_ID_COOKIE: &str = "_temps_visitor_id";

pub const SESSION_ID_COOKIE: &str = "_temps_sid";

/// Whether the request's project counts visitors without cookies (ADR 109
/// D3). Read from the route table's project row: no I/O per request.
pub(super) fn is_cookieless(ctx: &ProxyContext) -> bool {
    ctx.project
        .as_ref()
        .is_some_and(|project| project.analytics_identity_mode.is_cookieless())
}

/// The tracking cookies a cookieless project's response expires: whichever
/// of the two the browser still sends from before the switch.
pub(super) fn tracking_cookies_to_expire(
    sends_visitor_cookie: bool,
    sends_session_cookie: bool,
) -> Vec<&'static str> {
    [
        (sends_visitor_cookie, VISITOR_ID_COOKIE),
        (sends_session_cookie, SESSION_ID_COOKIE),
    ]
    .into_iter()
    .filter_map(|(sent, name)| sent.then_some(name))
    .collect()
}

// Helper functions for project-scoped cookie names
pub(super) fn get_visitor_cookie_name(_project_id: Option<i32>) -> String {
    VISITOR_ID_COOKIE.to_string()
}

pub(super) fn get_session_cookie_name(_project_id: Option<i32>) -> String {
    SESSION_ID_COOKIE.to_string()
}

impl LoadBalancer {
    /// Resolve visitor and session identifiers from cookies — entirely in-process,
    /// no database round-trips. A [`TrackingEvent`] is enqueued for the background
    /// batch writer, which upserts visitor/session rows asynchronously.
    pub(super) async fn ensure_visitor_session(&self, ctx: &mut ProxyContext) {
        // Only resolve once per request
        if ctx.visitor_id.is_some() {
            return;
        }

        // A cookieless project is never given a visitor or a session here:
        // nothing on the request may identify the browser across requests,
        // and its analytics ingest derives a daily id instead.
        if is_cookieless(ctx) {
            return;
        }

        // Skip crawlers — only track real humans
        if let Some(crawler_name) =
            crate::crawler_detector::CrawlerDetector::get_crawler_name(Some(&ctx.user_agent))
        {
            debug!(
                "Crawler detected: {} ({}), skipping visitor/session for project {}",
                crawler_name,
                ctx.user_agent,
                ctx.project.as_ref().map(|p| p.id).unwrap_or(0)
            );
            return;
        }

        // ── Stateless visitor decision (no DB) ──────────────────────────────
        let visitor_uuid =
            parse_visitor_cookie(ctx.request_visitor_cookie.as_deref(), &self.crypto);

        // ── Stateless session decision (no DB) ──────────────────────────────
        let session_decision = parse_session_cookie(
            ctx.request_session_cookie.as_deref(),
            &self.crypto,
            self.cookie_config.session_max_age_minutes,
        );

        // ── Compute attribution (used only for new visitors) ─────────────────
        let utm = ctx
            .query_string
            .as_deref()
            .map(temps_analytics::parse_utm_params)
            .unwrap_or_default();
        let referrer_hostname = ctx
            .referrer
            .as_deref()
            .and_then(temps_analytics::extract_referrer_hostname);
        let channel =
            temps_analytics::get_channel(&utm, referrer_hostname.as_deref(), Some(&ctx.host));

        let attribution = crate::traits::FirstVisitAttribution {
            referrer: ctx.referrer.clone(),
            referrer_hostname: referrer_hostname.clone(),
            channel: Some(channel.to_string()),
            utm_source: utm.utm_source.clone(),
            utm_medium: utm.utm_medium.clone(),
            utm_campaign: utm.utm_campaign.clone(),
        };

        // ── Enqueue background upsert ─────────────────────────────────────────
        self.tracking_handle.send(TrackingEvent {
            visitor_uuid: visitor_uuid.clone(),
            session_uuid: session_decision.session_uuid.clone(),
            project_id: ctx.project.as_ref().map(|p| p.id).unwrap_or(0),
            environment_id: ctx.environment.as_ref().map(|e| e.id),
            last_seen: chrono::Utc::now(),
            client_ip: ctx.ip_address.clone(),
            user_agent: Some(ctx.user_agent.clone()),
            is_crawler: false,
            crawler_name: None,
            is_new_session: session_decision.is_new_session,
            session_referrer: ctx.referrer.clone(),
            session_referrer_hostname: referrer_hostname,
            session_utm_source: utm.utm_source,
            session_utm_medium: utm.utm_medium,
            session_utm_campaign: utm.utm_campaign,
            session_utm_content: utm.utm_content,
            session_utm_term: utm.utm_term,
            session_channel: Some(channel.to_string()),
            attribution,
        });

        // ── Set context fields ────────────────────────────────────────────────
        ctx.visitor_id = Some(visitor_uuid.clone());
        ctx.session_id = Some(session_decision.session_uuid.clone());
        ctx.is_new_session = session_decision.is_new_session;

        debug!(
            "HTML request from visitor {} with session {} (new: {}) for project {}",
            visitor_uuid,
            session_decision.session_uuid,
            session_decision.is_new_session,
            ctx.project.as_ref().map(|p| p.id).unwrap_or(0)
        );
    }

    /// Returns true when a page view should be tracked (visitor/session created).
    /// This replaces the old `VisitorManager::should_track_visitor` trait method.
    pub fn should_track_page(
        path: &str,
        content_type: Option<&str>,
        method: &str,
        accept: Option<&str>,
        fetch_destination: Option<&str>,
        upgrade_insecure_requests: Option<&str>,
    ) -> bool {
        // API responses never represent a browser page, even when a framework
        // returns an HTML error document for an API-prefixed route.
        if path == "/api"
            || path.starts_with("/api/")
            || path == "/_temps"
            || path.starts_with("/_temps/")
            || path.starts_with(ROUTE_PREFIX_TEMPS)
        {
            return false;
        }

        // Visitor analytics describe browser navigations, not every HTTP
        // client that happens to receive HTML. Browsers advertise document
        // navigation with both an HTML Accept value and Fetch Metadata that
        // identifies a top-level document. Requiring both excludes generic
        // HTTP clients, framework data fetches, and embedded resources.
        if !is_browser_document_request(
            method,
            accept,
            fetch_destination,
            upgrade_insecure_requests,
        ) {
            return false;
        }

        // Don't track static assets
        if path.contains('.')
            && (path.ends_with(".js")
                || path.ends_with(".css")
                || path.ends_with(".png")
                || path.ends_with(".jpg")
                || path.ends_with(".svg")
                || path.ends_with(".ico"))
        {
            return false;
        }

        // A status code cannot distinguish a browser document from an API
        // response. Only HTML documents create visitor/session state; this
        // still includes genuine HTML 4xx/5xx error pages.
        content_type
            .and_then(|value| value.split(';').next())
            .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/html"))
    }

    pub(super) async fn ensure_static_visitor_session(
        &self,
        session: &PingoraSession,
        ctx: &mut ProxyContext,
        content_type: &str,
    ) {
        let request_accept = session
            .req_header()
            .headers
            .get("accept")
            .and_then(|value| value.to_str().ok());
        let fetch_destination = session
            .req_header()
            .headers
            .get("sec-fetch-dest")
            .and_then(|value| value.to_str().ok());
        let upgrade_insecure_requests = session
            .req_header()
            .headers
            .get("upgrade-insecure-requests")
            .and_then(|value| value.to_str().ok());

        if Self::should_track_page(
            &ctx.path,
            Some(content_type),
            &ctx.method,
            request_accept,
            fetch_destination,
            upgrade_insecure_requests,
        ) {
            self.ensure_visitor_session(ctx).await;
        }
    }

    /// Set visitor and session cookies on the response.
    ///
    /// Visitor cookie: set only when the request doesn't already carry a valid one.
    /// Session cookie: always re-issued with the current timestamp embedded in the
    /// v2 payload so the server-side freshness check stays accurate.
    pub(super) async fn set_tracking_cookies(
        &self,
        session: &mut PingoraSession,
        response: &mut ResponseHeader,
        ctx: &ProxyContext,
    ) -> Result<()> {
        let is_https = self.is_https_request(session);
        let project_id = ctx.project.as_ref().map(|p| p.id);

        // ── Cookieless project: set nothing, expire what is left ───────────
        if is_cookieless(ctx) {
            let sends = |name: &str| {
                find_request_cookie(session.req_header(), name, |_| Some(())).is_some()
            };
            for name in
                tracking_cookies_to_expire(sends(VISITOR_ID_COOKIE), sends(SESSION_ID_COOKIE))
            {
                let expired =
                    self.build_cookie_string(name, "", cookie::time::Duration::ZERO, is_https);
                response.append_header("Set-Cookie", expired)?;
            }
            return Ok(());
        }

        // ── Visitor cookie ──────────────────────────────────────────────────
        if let Some(visitor_id) = &ctx.visitor_id {
            let cookie_name = get_visitor_cookie_name(project_id);

            let has_valid_visitor_cookie =
                find_request_cookie(session.req_header(), &cookie_name, |value| {
                    self.crypto.decrypt(value).ok().map(|_| ())
                })
                .is_some();

            if !has_valid_visitor_cookie {
                let encrypted = match self.crypto.encrypt(visitor_id) {
                    Ok(e) => e,
                    Err(err) => {
                        error!("Failed to encrypt visitor cookie: {:?}", err);
                        return Err(Error::new_str("Failed to encrypt visitor cookie"));
                    }
                };
                let cookie_value = self.build_cookie_string(
                    &cookie_name,
                    &encrypted,
                    cookie::time::Duration::days(self.cookie_config.visitor_max_age_days),
                    is_https,
                );
                response.append_header("Set-Cookie", cookie_value)?;
            }
        }

        // ── Session cookie ──────────────────────────────────────────────────
        // Always re-issue with the current timestamp to keep the sliding window fresh.
        if let Some(session_id) = &ctx.session_id {
            let cookie_name = get_session_cookie_name(project_id);
            let now_secs = chrono::Utc::now().timestamp();
            let payload = make_v2_session_payload(session_id, now_secs);
            let encrypted = match self.crypto.encrypt(&payload) {
                Ok(e) => e,
                Err(err) => {
                    error!("Failed to encrypt session cookie: {:?}", err);
                    return Err(Error::new_str("Failed to encrypt session cookie"));
                }
            };
            let cookie_value = self.build_cookie_string(
                &cookie_name,
                &encrypted,
                cookie::time::Duration::minutes(self.cookie_config.session_max_age_minutes),
                is_https,
            );
            response.append_header("Set-Cookie", cookie_value)?;
        }

        Ok(())
    }

    /// Build a `Set-Cookie` header value with the configured attributes.
    fn build_cookie_string(
        &self,
        name: &str,
        value: &str,
        max_age: cookie::time::Duration,
        is_https: bool,
    ) -> String {
        let mut builder = Cookie::build((name.to_owned(), value.to_owned()))
            .path("/")
            .max_age(max_age)
            .http_only(self.cookie_config.http_only)
            .secure(is_https && self.cookie_config.secure);

        if let Some(ref same_site) = self.cookie_config.same_site {
            let ss = match same_site.to_lowercase().as_str() {
                "strict" => cookie::SameSite::Strict,
                "lax" => cookie::SameSite::Lax,
                "none" => cookie::SameSite::None,
                _ => cookie::SameSite::Lax,
            };
            builder = builder.same_site(ss);
        }

        builder.build().to_string()
    }
}

#[cfg(test)]
mod visitor_tests {
    use super::super::static_serving::static_path_route_redirect_tests::load_balancer;
    use super::{ProxyContext, SESSION_ID_COOKIE, VISITOR_ID_COOKIE};
    use pingora_proxy::ProxyHttp;
    use temp_dir::TempDir;

    const BROWSER: &str = "Mozilla/5.0 (X11; Linux x86_64) Firefox/131.0";

    fn browser_ctx(lb: &super::LoadBalancer) -> ProxyContext {
        let mut ctx = lb.new_ctx();
        ctx.user_agent = BROWSER.to_string();
        ctx.host = "app.example.com".to_string();
        ctx
    }

    #[tokio::test]
    async fn a_browser_without_cookies_gets_a_new_visitor_and_session() -> anyhow::Result<()> {
        let dir = TempDir::new()?;
        let lb = load_balancer(dir.path())?;
        let mut ctx = browser_ctx(&lb);

        lb.ensure_visitor_session(&mut ctx).await;

        let visitor = ctx.visitor_id.clone().expect("a visitor id is assigned");
        let session = ctx.session_id.clone().expect("a session id is assigned");
        assert!(ctx.is_new_session);
        assert_ne!(visitor, session);

        // Resolved once per request: a second call keeps the same ids.
        lb.ensure_visitor_session(&mut ctx).await;
        assert_eq!(ctx.visitor_id.as_deref(), Some(visitor.as_str()));
        assert_eq!(ctx.session_id.as_deref(), Some(session.as_str()));
        Ok(())
    }

    #[tokio::test]
    async fn crawlers_are_never_given_a_visitor_or_session() -> anyhow::Result<()> {
        let dir = TempDir::new()?;
        let lb = load_balancer(dir.path())?;
        let mut ctx = browser_ctx(&lb);
        ctx.user_agent =
            "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)".to_string();

        lb.ensure_visitor_session(&mut ctx).await;

        assert!(ctx.visitor_id.is_none());
        assert!(ctx.session_id.is_none());
        Ok(())
    }

    fn cookieless_ctx(lb: &super::LoadBalancer) -> ProxyContext {
        let mut ctx = browser_ctx(lb);
        // Every other column at the default a new project gets.
        let project: temps_entities::projects::Model = serde_json::from_value(serde_json::json!({
            "id": 7, "name": "site", "slug": "site", "repo_name": "site",
            "repo_owner": "example", "directory": ".", "main_branch": "main",
            "preset": "dockerfile", "created_at": chrono::Utc::now(),
            "updated_at": chrono::Utc::now(), "pull_only_root_directory": false,
            "is_deleted": false, "is_public_repo": false, "attack_mode": false, "shield_mode": "log",
            "ai_write_actions_enabled": false, "error_source_context_enabled": false,
            "vulnerability_scanning_enabled": false, "enable_preview_environments": false,
            "preview_envs_on_demand": false, "preview_envs_idle_timeout_seconds": 300,
            "preview_envs_wake_timeout_seconds": 30, "source_type": "docker_image",
            "project_type": temps_entities::types::ProjectType::Server,
            "hosting": temps_entities::types::Hosting::Hosted,
            "cross_project_trace_sharing": true, "cloud_telemetry_fidelity": "metered",
            "cloud_telemetry_attribute_allowlist": [], "cloud_telemetry_write_mode": "local",
            "cloud_analytics_write_mode": "local", "analytics_identity_mode": "cookieless"
        }))
        .expect("test project fixture deserializes");
        ctx.project = Some(std::sync::Arc::new(project));
        ctx
    }

    #[tokio::test]
    async fn a_cookieless_project_gives_no_visitor_or_session() -> anyhow::Result<()> {
        let dir = TempDir::new()?;
        let lb = load_balancer(dir.path())?;
        let mut ctx = cookieless_ctx(&lb);
        // Even a browser still carrying an old cookie is not recognised.
        ctx.request_visitor_cookie = Some("old-sealed-visitor".to_string());

        lb.ensure_visitor_session(&mut ctx).await;

        assert!(super::is_cookieless(&ctx));
        assert!(ctx.visitor_id.is_none());
        assert!(ctx.session_id.is_none());
        Ok(())
    }

    #[test]
    fn only_the_cookies_a_browser_still_sends_are_expired() {
        assert!(super::tracking_cookies_to_expire(false, false).is_empty());
        assert_eq!(
            super::tracking_cookies_to_expire(true, false),
            vec![VISITOR_ID_COOKIE]
        );
        assert_eq!(
            super::tracking_cookies_to_expire(true, true),
            vec![VISITOR_ID_COOKIE, SESSION_ID_COOKIE]
        );
    }

    #[test]
    fn an_expired_tracking_cookie_has_no_value_and_max_age_zero() -> anyhow::Result<()> {
        let dir = TempDir::new()?;
        let lb = load_balancer(dir.path())?;
        let expired =
            lb.build_cookie_string(VISITOR_ID_COOKIE, "", cookie::time::Duration::ZERO, true);
        assert!(
            expired.starts_with(&format!("{VISITOR_ID_COOKIE}=;")),
            "{expired}"
        );
        assert!(expired.contains("Max-Age=0"), "{expired}");
        assert!(expired.contains("Path=/"), "{expired}");
        Ok(())
    }

    #[test]
    fn tracking_cookies_are_scoped_to_the_site_and_secure_only_over_https() -> anyhow::Result<()> {
        let dir = TempDir::new()?;
        let lb = load_balancer(dir.path())?;
        let max_age = cookie::time::Duration::days(365);

        let https = lb.build_cookie_string(VISITOR_ID_COOKIE, "abc", max_age, true);
        assert!(https.starts_with(&format!("{VISITOR_ID_COOKIE}=abc")));
        assert!(https.contains("Path=/"));
        assert!(https.contains("Max-Age=31536000"));
        assert!(https.contains("HttpOnly"));
        assert!(https.contains("SameSite=Lax"));
        assert!(https.contains("Secure"));

        let http = lb.build_cookie_string(SESSION_ID_COOKIE, "xyz", max_age, false);
        assert!(http.starts_with(&format!("{SESSION_ID_COOKIE}=xyz")));
        assert!(
            !http.contains("Secure"),
            "a cookie set over plain HTTP must not be Secure: {http}"
        );
        Ok(())
    }
}
