// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The order of `request_filter`.
//!
//! The order is what keeps these checks meaningful: each one protects what
//! comes after it, so moving one can open a bypass without any single check
//! changing. `order_tests` pins it: every test sets up a request that two
//! checks would both answer and asserts which one does.
//!
//! 1. Request facts, then inbound stripping of what only the proxy may set
//!    (`X-Temps-Demo-Mode`, identity headers, the status-host tag).
//! 2. Status-page hostname: answered here and nowhere else. A connection
//!    negotiated for such a hostname cannot carry any other host (421).
//! 3. Sandbox preview hostname. The public ingress hostname is never one.
//! 4. Signed-in gate (ADR 065 D8), before anything can wake or reach the app.
//! 5. Wake a sleeping host, after reserving a wake slot.
//! 6. Path route (ADR 050): match, wake a sleeping target after reserving a
//!    slot, and answer 502 when it has no target, so the host's own service
//!    never answers in its place.
//! 7. Project context: the path route's target if there is one, otherwise the
//!    host's. With a project, in order:
//!    1. connection cap;
//!    2. request policy gate (`Unavailable` 503, `Deny` 403);
//!    3. edge rules (ADR-089), except a password wall's own verify POST;
//!    4. the access decision (ADR-055 §11): IP restriction, blacklist, rate
//!       limit, ingress endpoint, Temps login. A login never blocks Shield's
//!       own paths;
//!    5. Shield's verify POST and solver files, a challenge stored for this
//!       host, then Shield itself (ADR 066);
//!    6. password wall, whose verify POST is rate limited before the password
//!       is checked.
//! 8. The solver files for a host without a project.
//! 9. ACME HTTP-01, before anything that could redirect the CA away.
//! 10. On-demand TLS's 503 on plain HTTP (ADR-018), before the HTTPS redirect.
//! 11. HTTPS redirect, then the domain's own redirect.
//! 12. Upstream request: strip client-supplied address headers, capture the
//!     headers for the request log, set the trusted forwarded headers.
//! 13. ACME and the domain redirect again, static files, persisted assets.
//! 14. Admin gate (ADR-051), last: it only decides requests about to fall
//!     through to the console.

/// Runs one stage: anything but `Continue` ends the stages around it.
macro_rules! stage {
    ($stage:expr) => {
        match $stage.await? {
            Flow::Continue => {}
            finished => return Ok(finished),
        }
    };
}

mod access;
mod hostname;
mod inbound;
#[cfg(test)]
mod order_tests;
mod password_wall;
mod path_route;
mod project;
mod redirects;
mod wake;

use super::*;

/// What a stage of `request_filter` decided.
#[must_use]
pub(super) enum Flow {
    /// The next stage decides.
    Continue,
    /// The proxy answered the request itself: no later stage runs and nothing
    /// goes upstream.
    Answered,
    /// Send the request upstream as it is now, without running later stages.
    Forward,
}

impl Flow {
    /// A request is finished either way: `answered` says whether by the proxy
    /// or by sending it upstream.
    fn finished(answered: bool) -> Self {
        if answered {
            Self::Answered
        } else {
            Self::Forward
        }
    }
}

impl LoadBalancer {
    /// `request_filter`, in the order the module documentation states.
    pub(super) async fn run_request_stages(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<Flow> {
        self.read_request_facts(session, ctx)?;
        Self::strip_inbound_trust_headers(session);

        stage!(self.status_page_host_stage(session, ctx));

        stage!(self.preview_stage(session, ctx));

        stage!(self.signed_in_gate_stage(session, ctx));

        stage!(self.wake_sleeping_host_stage(session, ctx));

        self.match_path_route(ctx);

        stage!(self.wake_path_target_stage(session, ctx));

        stage!(self.unavailable_path_target_stage(session, ctx));

        let project_context = self.resolve_project_context(ctx).await;
        match &project_context {
            Some(project_ctx) => stage!(self.project_stage(session, ctx, project_ctx)),
            None => ctx.routing_status = "no_project".to_string(),
        }

        stage!(self.solver_files_stage(session, ctx));

        stage!(self.acme_challenge_stage(session, ctx, true));

        stage!(self.on_demand_tls_stage(session, ctx));

        stage!(self.https_redirect_stage(session, ctx));

        // Check if this host should redirect
        if let Some((redirect_target, status_code)) = self
            .project_context_resolver
            .get_redirect_info(&ctx.host)
            .await
        {
            let redirect_url =
                domain_redirect_location(&redirect_target, &ctx.path, ctx.query_string.as_deref());
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
            resp.insert_header("X-Temps-Proxy-Probe-Capable", "1")?;

            // Add CORS headers for redirect responses
            resp.insert_header("Access-Control-Allow-Origin", "*")?;

            // Update context for logging
            ctx.routing_status = "redirected".to_string();

            session.write_response_header(Box::new(resp), true).await?;
            return Ok(Flow::Answered); // Skip proxying
        }

        // RFC 7239 Forwarded, X-Real-IP, and CF-Connecting-IP are all
        // client-controlled at this trust boundary (any direct client can
        // set them, bypassing Bunny/Cloudflare entirely). We emit a
        // complete trusted X-Forwarded-* set below from the already-
        // resolved `ctx.ip_address`, so do not let a tenant app read a raw,
        // possibly-spoofed client-supplied header instead.
        strip_untrusted_client_ip_headers(
            session.req_header_mut(),
            self.loopback_ja4_header.load().as_deref(),
        );

        // Browsers split cookies across several fields over HTTP/2; the
        // upstream must receive them as one `Cookie` header or it keeps only
        // the first (see `coalesce_cookie_headers`).
        coalesce_cookie_headers(session.req_header_mut());

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

        ctx.request_visitor_cookie =
            find_request_cookie(session.req_header(), &visitor_cookie_name, |value| {
                Some(value.to_string())
            });

        ctx.request_session_cookie =
            find_request_cookie(session.req_header(), &session_cookie_name, |value| {
                Some(value.to_string())
            });

        // Get IP from the connection
        // Add X-Forwarded-For header with client IP (already extracted in request_filter)
        if let Some(ref ip) = ctx.ip_address {
            session
                .req_header_mut()
                .insert_header("X-Forwarded-For", ip.as_str())?;
        }

        // Overwrite the complete public authority forwarded upstream. Apps
        // such as Keycloak trust this set when constructing absolute URLs.
        // Forwarding only the scheme loses non-default ports and produces
        // redirects to port 80/443 instead of the Temps proxy.
        let is_https = self.is_https_request(session);
        let proto = if is_https { "https" } else { "http" };
        let public_authority = self.request_authority(session)?;
        let forwarded_port = public_authority
            .port
            .unwrap_or(if is_https { 443 } else { 80 });
        // The same parsed authority controls routing and reaches the upstream.
        // Never pass the raw client Host after making a routing decision.
        session
            .req_header_mut()
            .insert_header("Host", public_authority.forwarded_host.clone())?;
        session
            .req_header_mut()
            .insert_header("X-Forwarded-Proto", proto)?;
        session
            .req_header_mut()
            .insert_header("X-Forwarded-Host", public_authority.forwarded_host)?;
        session
            .req_header_mut()
            .insert_header("X-Forwarded-Port", forwarded_port.to_string())?;

        ctx.referrer = session
            .req_header()
            .headers
            .get("referer")
            .map(|h| h.to_str().unwrap_or_default().to_string());

        stage!(self.acme_challenge_stage(session, ctx, false));

        // Check for redirects or static file serving
        if let Some((redirect_target, status_code)) = self
            .project_context_resolver
            .get_redirect_info(&ctx.host)
            .await
        {
            let location =
                domain_redirect_location(&redirect_target, &ctx.path, ctx.query_string.as_deref());
            let mut resp = ResponseHeader::build(status_code, None)?;
            resp.insert_header(header::LOCATION, &location)?;
            session.write_response_header(Box::new(resp), true).await?;
            return Ok(Flow::Answered);
        }

        // Check if this is a static deployment using route table. A path
        // route serves from its target instead: its files if the target is
        // static, the upstream otherwise.
        let static_dir = match ctx.path_route.as_ref() {
            Some(route) => route
                .target
                .as_ref()
                .and_then(|target| target.static_dir())
                .map(str::to_string),
            None => {
                self.project_context_resolver
                    .get_static_path(&ctx.host)
                    .await
            }
        };
        if let Some(static_dir) = static_dir {
            debug!(
                "Static deployment detected for {}: {}",
                ctx.host, static_dir
            );

            // IMPORTANT: Skip static file serving for /api/_temps/* paths
            // These must ALWAYS be proxied to the console address (admin API)
            if !ctx.path.starts_with("/api/_temps/") {
                let served = self.serve_static_target(session, ctx, &static_dir).await;
                match served {
                    Ok(StaticFileServeOutcome::Served) => {
                        debug!("Served static file: {}", ctx.path);
                        ctx.routing_status = "static_file".to_string();
                        self.log_static_request(ctx, 200, "static_file", &static_dir, None, None);
                        return Ok(Flow::Answered);
                    }
                    Ok(StaticFileServeOutcome::ServedNotFoundPage) => {
                        debug!(
                            request_path = %bounded_log_value(&ctx.path),
                            stored_static_dir = %bounded_log_value(&static_dir),
                            "Static file request answered with the deployment's 404 page"
                        );
                        ctx.routing_status = "static_file_not_found_page".to_string();
                        self.log_static_request(
                            ctx,
                            404,
                            "static_file_not_found_page",
                            &static_dir,
                            Some("Static file not found".to_string()),
                            None,
                        );
                        return Ok(Flow::Answered);
                    }
                    Ok(StaticFileServeOutcome::Redirected) => {
                        debug!(
                            request_path = %bounded_log_value(&ctx.path),
                            "Static file request redirected to its slashless page"
                        );
                        ctx.routing_status = "static_file_redirect".to_string();
                        self.log_static_request(
                            ctx,
                            308,
                            "static_file_redirect",
                            &static_dir,
                            None,
                            None,
                        );
                        return Ok(Flow::Answered);
                    }
                    Ok(StaticFileServeOutcome::NotFound) => {
                        debug!(
                            request_path = %bounded_log_value(&ctx.path),
                            stored_static_dir = %bounded_log_value(&static_dir),
                            "Static file request returned the uniform not-found response"
                        );
                        let contract = static_not_found_contract(&ctx.method);
                        let mut resp = ResponseHeader::build(contract.status, None)?;
                        resp.insert_header(header::CONTENT_TYPE, contract.content_type)?;
                        resp.insert_header(
                            header::CONTENT_LENGTH,
                            contract.content_length.to_string(),
                        )?;
                        resp.insert_header(header::CACHE_CONTROL, contract.cache_control)?;
                        resp.insert_header("X-Request-ID", &ctx.request_id)?;

                        self.ensure_static_visitor_session(session, ctx, contract.content_type)
                            .await;
                        self.set_tracking_cookies(session, &mut resp, ctx).await?;
                        session.write_response_header(Box::new(resp), false).await?;
                        if contract.send_body {
                            session
                                .write_response_body(
                                    Some(Bytes::from_static(STATIC_NOT_FOUND_BODY)),
                                    true,
                                )
                                .await?;
                        } else {
                            session.write_response_body(None, true).await?;
                        }

                        self.log_static_request(
                            ctx,
                            404,
                            "static_file_not_found",
                            &static_dir,
                            Some("Static file not found".to_string()),
                            Some(contract.content_length as i64),
                        );
                        return Ok(Flow::Answered);
                    }
                    Err(error) => return Err(error),
                }
            }
            // If we reach here and path starts with /api/_temps/,
            // fall through to normal proxying logic (will be proxied to console)
        }

        // Serve persisted static assets via deployment-prefixed URLs.
        // /_temps/assets/{deployment_slug}/path → DB lookup → CAS blob
        if ctx.path.starts_with("/_temps/assets/") {
            let after_prefix = &ctx.path["/_temps/assets/".len()..];
            if let Some(slash_pos) = after_prefix.find('/') {
                let deployment_slug = &after_prefix[..slash_pos];
                let asset_path = after_prefix[slash_pos + 1..].to_string();
                let mut legacy_source = None;
                let mut legacy_current_slug = None;
                let mut asset_scope = ctx.deployment.as_ref().and_then(|deployment| {
                    let context = deployment.context_vars.as_ref();
                    let source_slug = context
                        .and_then(|value| value.get("source_deployment_slug"))
                        .and_then(serde_json::Value::as_str);
                    let source_deployment_id = context
                        .and_then(|value| value.get("source_deployment_id"))
                        .and_then(serde_json::Value::as_i64)
                        .and_then(|value| i32::try_from(value).ok());
                    if source_slug.is_none() {
                        legacy_source = source_deployment_id;
                        legacy_current_slug = Some(deployment.slug.clone());
                    }
                    deployment_asset_scope(
                        &deployment.slug,
                        deployment.environment_id,
                        deployment.id,
                        source_slug,
                        context
                            .and_then(|value| value.get("source_environment_id"))
                            .and_then(serde_json::Value::as_i64)
                            .and_then(|value| i32::try_from(value).ok()),
                        source_deployment_id,
                        deployment_slug,
                    )
                });
                if let (Some(project_id), Some(source_deployment_id)) = (
                    ctx.project.as_ref().map(|project| project.id),
                    legacy_source,
                ) {
                    if let Some(origin) = self
                        .static_asset_lookup
                        .resolve_legacy_asset_origin(project_id, source_deployment_id)
                        .await
                    {
                        asset_scope = legacy_deployment_asset_scope(
                            legacy_current_slug.as_deref().unwrap_or_default(),
                            &origin,
                            deployment_slug,
                        );
                    }
                }
                if Self::is_cacheable_static_asset(&asset_path) {
                    if let Some(asset_scope) = asset_scope {
                        if let Ok(true) = self
                            .serve_asset_from_store(session, ctx, &asset_path, Some(asset_scope))
                            .await
                        {
                            ctx.routing_status = "prefixed_asset".to_string();
                            return Ok(Flow::Answered);
                        }
                    }
                }
            }
        }

        // Fallback: serve immutable static assets from file store.
        // For container deployments where the upstream didn't have the asset,
        // check the path-keyed file store (stale-chunk fallback).
        if Self::is_cacheable_static_asset(&ctx.path)
            && ctx.path_route.is_none()
            && !self
                .project_context_resolver
                .is_static_deployment(&ctx.host)
                .await
        {
            let url_path = ctx.path.trim_start_matches('/').to_string();
            if let Ok(true) = self
                .serve_asset_from_store(session, ctx, &url_path, None)
                .await
            {
                ctx.routing_status = "stale_chunk_fallback".to_string();
                return Ok(Flow::Answered);
            }
        }

        // Admin gate: when a non-noop gate is wired and the request is
        // about to fall back to the console (no deployed app for this host,
        // not a public ingest path under /api/_temps/*, not a preview),
        // require the (IP, Host) tuple to pass the gate. If it doesn't, the
        // management surface must stay invisible from non-admin hosts.
        //
        // The public ingress hostname (ADR-051) is not gated at all: the
        // resolver pins it to the public-only listener for every client, so
        // there is no management surface behind it to protect.
        if let Some(gate) = self
            .admin_gate
            .as_ref()
            .filter(|_| !self.is_public_ingress_request(&ctx.host))
        {
            let config = gate.current();
            if Self::should_consult_admin_gate(&config, &ctx.path, ctx.preview_route.is_some()) {
                let host_has_route = self.upstream_resolver.has_route_for_host(&ctx.host).await;
                if !host_has_route {
                    let client_ip = ctx
                        .ip_address
                        .as_deref()
                        .and_then(|s| s.parse::<std::net::IpAddr>().ok())
                        .unwrap_or_else(|| std::net::IpAddr::from([127, 0, 0, 1]));
                    match Self::console_fall_through(
                        &config,
                        client_ip,
                        &ctx.host,
                        self.upstream_resolver.console_ingress_address().is_some(),
                    ) {
                        ConsoleFallThrough::Console => {}
                        // A denied client is still a legitimate caller of the
                        // public routes: a mail client fetching a tracking
                        // pixel, a git provider delivering a push. Hand it to
                        // the listener that mounts those and nothing else.
                        // That listener answers everything that is not a
                        // public route with a 404, so the console stays as
                        // invisible as when the proxy answered itself.
                        ConsoleFallThrough::PublicSurfaceOnly => {
                            debug!(
                                host = %ctx.host,
                                client_ip = %client_ip,
                                path = %ctx.path,
                                "admin gate denied request; serving the public-only surface"
                            );
                            ctx.public_surface_only = true;
                        }
                        ConsoleFallThrough::NotFound => {
                            warn!(
                                host = %ctx.host,
                                client_ip = %client_ip,
                                path = %ctx.path,
                                "admin gate denied request to non-admin host"
                            );
                            let mut response = ResponseHeader::build(StatusCode::NOT_FOUND, None)?;
                            response.insert_header("Cache-Control", "no-store")?;
                            response.insert_header("X-Request-ID", &ctx.request_id)?;
                            response.insert_header("Content-Type", "text/html; charset=utf-8")?;
                            let body =
                                Bytes::from(crate::branded_404::render(&ctx.host, &ctx.request_id));
                            response.insert_header("Content-Length", body.len().to_string())?;
                            session
                                .write_response_header(Box::new(response), false)
                                .await?;
                            session.write_response_body(Some(body), true).await?;
                            ctx.routing_status = "admin_gate_denied".to_string();
                            return Ok(Flow::Answered);
                        }
                    }
                }
            }
        }

        Ok(Flow::Continue)
    }
}
