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
}
