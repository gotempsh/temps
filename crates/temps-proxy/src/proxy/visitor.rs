// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Visitor and session identification: the tracking cookies and the in-process visitor/session resolution.

use super::*;

pub const VISITOR_ID_COOKIE: &str = "_temps_visitor_id";

pub const SESSION_ID_COOKIE: &str = "_temps_sid";

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
            environment_id: ctx.environment.as_ref().map(|e| e.id).unwrap_or(0),
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
