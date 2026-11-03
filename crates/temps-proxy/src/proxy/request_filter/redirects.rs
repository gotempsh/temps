// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What can answer a request without a project: challenges, then redirects.

use super::super::*;
use super::Flow;

impl LoadBalancer {
    /// The challenge solver's files, for a host without a project: a project's host was answered with Shield.
    pub(super) async fn solver_files_stage(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<Flow> {
        // The challenge solver, for hosts without a project (project hosts
        // were answered with Shield above).
        if self.serve_solver_asset(session, ctx).await? {
            return Ok(Flow::Answered);
        }

        Ok(Flow::Continue)
    }

    /// Answer a stored ACME HTTP-01 challenge. It runs before the redirects, so
    /// a domain configured as one can still complete certificate provisioning,
    /// and again once the upstream request is prepared, for a challenge stored
    /// in between.
    pub(super) async fn acme_challenge_stage(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
        before_redirects: bool,
    ) -> Result<Flow> {
        if let Some(key_authorization) = self
            .handle_acme_http_challenge(&ctx.host, &ctx.path)
            .await?
        {
            let (lookup_note, done_note) = if before_redirects {
                (" - before redirect check", " (redirect domain)")
            } else {
                ("", "")
            };
            debug!(
                "Serving ACME HTTP-01 challenge response for {}{} (request_id={}){}",
                ctx.host, ctx.path, ctx.request_id, lookup_note
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

            info!(
                "ACME challenge completed{}: {} {} - 200 OK - {}ms",
                done_note,
                ctx.method,
                ctx.path,
                ctx.start_time.elapsed().as_millis()
            );

            ctx.routing_status = "acme_challenge".to_string();
            return Ok(Flow::Answered);
        }

        Ok(Flow::Continue)
    }

    /// On-demand TLS's answer on plain HTTP (ADR-018 §5).
    pub(super) async fn on_demand_tls_stage(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<Flow> {
        // On-demand HTTP-01 TLS UX (ADR-018 §5). Only engaged when the manager
        // is wired (on-demand TLS enabled) — otherwise zero overhead, no extra
        // settings fetch. MUST come after ACME challenge handling so a challenge
        // can always complete, and before the HTTPS redirect so a host whose
        // cert is still provisioning gets the 503 instead of a redirect to a
        // non-existent cert.
        // Gate on the cheap, in-memory checks FIRST so the common case (HTTPS
        // traffic, and HTTP hosts with no on-demand cert state) costs nothing.
        // `get_settings()` is TTL-cached (no Postgres round-trip), but the lazy
        // fetch is still kept inside `handle_on_demand_http` so it only runs for
        // the rare ephemeral `redirect_to_env` branch rather than for every
        // plain-HTTP request.
        if self.on_demand_cert_manager.is_some()
            && !self.is_tls_connection(session)
            && self.handle_on_demand_http(session, ctx).await?
        {
            return Ok(Flow::Answered);
        }

        Ok(Flow::Continue)
    }

    /// HTTP to HTTPS, when the host or its environment asks for it.
    pub(super) async fn https_redirect_stage(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<Flow> {
        // HTTP to HTTPS redirect for non-TLS connections.
        // This MUST come after ACME challenge handling to allow Let's Encrypt
        // HTTP-01 validation. `should_redirect_to_https` additionally exempts the
        // whole `/.well-known/acme-challenge/` prefix, so a validation request
        // that did NOT match a stored token (renewal in flight, token written to
        // a sibling domain row, wildcard parent) still falls through to normal
        // routing instead of being 301'd to a certificate that has expired or
        // does not exist yet.
        //
        // By default the redirect is per-domain: we only redirect when the
        // requesting host actually has an active TLS certificate in the database
        // (exact match or wildcard parent). This means HTTP-only installs
        // (sslip.io quick/local modes, no cert provisioned) never get redirected,
        // while hosts that have gone through SSL provisioning get automatic HTTPS
        // enforcement.
        //
        // The environment resolved above can override that default in either
        // direction (`force_https`): `Some(true)` for sites whose TLS is
        // terminated by an upstream CDN — no local cert exists, so the default
        // heuristic would leave plain HTTP serving a full 200 alongside HTTPS —
        // and `Some(false)` for environments that must stay reachable over HTTP.
        // Reading it off `ctx.environment` costs nothing extra: the environment
        // was already resolved and cloned into the context earlier in this
        // filter, so there is no additional lookup on the hot path.
        //
        // `disable_https_redirect` is a global escape hatch (set by the service
        // unit in local/testing mode) that bypasses the check entirely and
        // outranks the per-environment override.
        // WS3: cert-host check is now a lock-free ArcSwap snapshot read; the
        // background `CertHostCache::run_refresh_loop` keeps it current (±30 s).
        let env_force_https = ctx.environment.as_ref().and_then(|env| env.force_https);
        // An unresolved Host falls through to the console upstream. The console
        // has its own operator-set override rather than inheriting a project's,
        // and it is consulted only when no environment matched — `get_settings`
        // is TTL-cached, so this costs nothing on the project hot path.
        //
        // On a settings read failure this yields `None` (inherit the per-host
        // certificate heuristic), not `Some(true)`: guessing "redirect" here
        // would turn a transient database blip into a console that 301s to a
        // certificate it may not have.
        let console_force_https = if ctx.environment.is_none() {
            match self.config_service.get_settings().await {
                Ok(settings) => {
                    let request_host = ctx
                        .host
                        .split(':')
                        .next()
                        .unwrap_or(&ctx.host)
                        .trim_end_matches('.')
                        .to_ascii_lowercase();
                    if settings.console_hostname().as_deref() == Some(request_host.as_str()) {
                        settings.console_force_https
                    } else {
                        None
                    }
                }
                Err(error) => {
                    warn!(
                        error = %error,
                        "Failed to read app settings; falling back to the per-host certificate heuristic for HTTPS policy"
                    );
                    None
                }
            }
        } else {
            None
        };
        // Exactly one of these can be set: `console_force_https` is only
        // computed when no environment resolved.
        let force_https = env_force_https.or(console_force_https);
        let production_https = if should_apply_production_https_default(
            self.disable_https_redirect,
            self.is_tls_connection(session),
            &ctx.path,
            force_https,
            ctx.environment.is_some(),
        ) {
            match self.config_service.get_url_scheme().await {
                Ok(scheme) => scheme != "http",
                Err(error) => {
                    warn!(
                        error = %error,
                        "Failed to read external URL scheme; enforcing HTTPS"
                    );
                    true
                }
            }
        } else {
            false
        };
        let needs_redirect = should_redirect_to_https(
            self.disable_https_redirect,
            self.is_tls_connection(session),
            &ctx.path,
            force_https,
            // Lock-free ArcSwap snapshot read, and only reached when the
            // environment has no explicit override.
            || {
                inherited_https_policy(
                    production_https,
                    self.cert_host_cache.has_cert_for_host(&ctx.host),
                )
            },
        );
        if needs_redirect {
            // Build the HTTPS redirect URL preserving path and query string
            let redirect_url = if let Some(query) = &ctx.query_string {
                format!(
                    "https://{}{}{}",
                    ctx.host,
                    ctx.path,
                    if query.is_empty() {
                        String::new()
                    } else {
                        format!("?{}", query)
                    }
                )
            } else {
                format!("https://{}{}", ctx.host, ctx.path)
            };

            debug!(
                request_id = %ctx.request_id,
                host = %ctx.host,
                path = %ctx.path,
                redirect_url = %redirect_url,
                "Redirecting HTTP to HTTPS"
            );

            // Use 301 Permanent Redirect for HTTP→HTTPS
            let resp = https_redirect_response(&redirect_url, &ctx.request_id)?;
            // Managed monitors use this marker to distinguish the proxy's
            // pre-upstream protocol upgrade from an application's own 3xx.
            // It carries no trust decision by itself: monitors still require
            // a same-host HTTPS target and pin the follow-up to the configured
            // local TLS listener.
            ctx.routing_status = "http_to_https_redirect".to_string();

            session.write_response_header(Box::new(resp), true).await?;
            return Ok(Flow::Answered);
        }

        Ok(Flow::Continue)
    }

    /// The redirect configured for this domain.
    pub(super) async fn domain_redirect_stage(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<Flow> {
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

        Ok(Flow::Continue)
    }

    /// A redirect that appeared after `domain_redirect_stage` looked. It answers
    /// with less than that stage, and leaves the routing status as it found it.
    pub(super) async fn late_domain_redirect_stage(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<Flow> {
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

        Ok(Flow::Continue)
    }
}
