// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Stages that decide from the hostname alone, before any routing.

use super::super::*;
use super::Flow;

impl LoadBalancer {
    /// A status page's custom hostname, and a connection negotiated for one.
    pub(super) async fn status_page_host_stage(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<Flow> {
        // A status page's custom hostname (#1412) is answered here and only
        // here: before previews, wake-on-request, project routing, the admin
        // gate and the console fall-through, so no path or header can steer it
        // anywhere but the page it is attached to.
        if let Some(status_route) = self.upstream_resolver.status_page_host(&ctx.host) {
            return self
                .handle_status_page_host(session, ctx, status_route)
                .await
                .map(Flow::finished);
        }
        let sni = Self::tls_sni(session);
        self.refuse_misdirected_connection(session, ctx, sni.as_deref())
            .await
    }

    /// A TLS connection negotiated for a status hostname may only ever carry
    /// requests for that hostname: a different Host must not use it to reach
    /// the console or an application.
    pub(super) async fn refuse_misdirected_connection(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
        sni: Option<&str>,
    ) -> Result<Flow> {
        if let Some(sni) = sni {
            if self.upstream_resolver.status_page_host(sni).is_some() {
                return self
                    .write_misdirected(session, ctx)
                    .await
                    .map(Flow::finished);
            }
        }
        Ok(Flow::Continue)
    }

    /// A sandbox preview's hostname.
    pub(super) async fn preview_stage(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<Flow> {
        // Sandbox preview gateway: requests to `ws-<sid>-<port>.<preview_domain>`
        // are checked against the sandbox's preview allow-list, then its
        // access mode: a Temps login (the default), a password form, or
        // public. On success we mark the request as a preview route so
        // `upstream_peer` forwards it to the local gateway or worker relay;
        // otherwise `handle_preview_request` answers it.
        //
        // HTTP Basic auth is NOT supported — see `preview_auth.rs` for
        // rationale.
        //
        // The public ingress hostname (ADR-051) is never a preview, whatever
        // it looks like: it is reserved for the platform's callback routes,
        // and treating a `ws-` shaped one as a sandbox would answer provider
        // callbacks with a login redirect or a 404. The settings API refuses
        // such a hostname; this keeps the guarantee if one is stored anyway.
        if let Ok(settings) = self.config_service.get_settings().await {
            if let Some(preview_host) =
                preview_host_for_request(&ctx.host, &settings.preview_domain, || {
                    self.upstream_resolver.is_public_ingress_host(&ctx.host)
                })
            {
                if self
                    .handle_preview_request(session, ctx, preview_host, &settings)
                    .await?
                {
                    return Ok(Flow::Answered);
                }
            }
        }
        Ok(Flow::Continue)
    }

    /// Signed-in addresses (ADR 065 D8): the gate answers before anything can
    /// reach the app, including waking a sleeping environment. `Continue` when
    /// the host has no gate or the visitor is admitted (identity headers set,
    /// gate cookies removed).
    pub(super) async fn signed_in_gate_stage(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<Flow> {
        let Some(gate) = self.gate.as_ref() else {
            return Ok(Flow::Continue);
        };
        let hostname = crate::handler::gate::gate_hostname(&ctx.host);
        let Some(policy) = gate.policy_for(&hostname) else {
            return Ok(Flow::Continue);
        };
        ctx.gated_host = true;
        // ACME HTTP-01 first, as on the agent: a signed-in address must still
        // get and renew its certificate, and Let's Encrypt never signs in.
        // Only a challenge stored for this host is answered; any other path
        // under the prefix meets the gate, so the prefix is no way past it.
        if ctx.path.starts_with(ACME_HTTP01_PREFIX) {
            if let Some(key_authorization) = self
                .handle_acme_http_challenge(&ctx.host, &ctx.path)
                .await?
            {
                let body = Bytes::from(key_authorization);
                let mut resp = ResponseHeader::build(200, None)?;
                resp.insert_header("Content-Type", "text/plain")?;
                resp.insert_header("Cache-Control", "no-cache")?;
                resp.insert_header("X-Request-ID", &ctx.request_id)?;
                resp.insert_header("Content-Length", body.len().to_string())?;
                session.write_response_header(Box::new(resp), false).await?;
                session.write_response_body(Some(body), true).await?;
                ctx.routing_status = "acme_challenge".to_string();
                return Ok(Flow::Answered);
            }
        }
        // An operator who turned the HTTPS redirect off terminates TLS in
        // front of this listener; sending the visitor to HTTPS again would
        // loop. Either way the gate runs: this only picks the redirect.
        let secure = self.is_https_request(session) || self.disable_https_redirect;
        let outcome = {
            let header = session.req_header();
            let request = temps_core::gate::GateRequest {
                hostname: &hostname,
                method: &header.method,
                path: header.uri.path(),
                query: header.uri.query(),
                headers: &header.headers,
                secure,
                // Tunnels end at a worker's tunnel origin, never here.
                via_tunnel: false,
            };
            gate.evaluate(&policy, &request, temps_core::gate::unix_now())
                .await
        };
        match outcome {
            crate::handler::gate::GateOutcome::Admit(claims) => {
                crate::handler::gate::admit(session.req_header_mut(), &claims)?;
                ctx.gate_identity = Some(claims);
                Ok(Flow::Continue)
            }
            crate::handler::gate::GateOutcome::Respond(answer, routing_status) => {
                let (response, body) = crate::handler::gate::render(&answer, &ctx.request_id)?;
                session
                    .write_response_header(Box::new(response), body.is_none())
                    .await?;
                if let Some(body) = body {
                    session.write_response_body(Some(body), true).await?;
                }
                ctx.routing_status = routing_status.to_string();
                Ok(Flow::Answered)
            }
        }
    }
}
