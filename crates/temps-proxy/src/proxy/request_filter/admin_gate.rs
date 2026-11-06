// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The admin gate (ADR-051): what may reach the console.

use super::super::*;
use super::Flow;

impl LoadBalancer {
    /// The admin gate, for a request about to fall through to the console.
    pub(super) async fn admin_gate_stage(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<Flow> {
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
                            let body = Bytes::from(crate::branded_404::render_with_identity(
                                &ctx.host,
                                &ctx.request_id,
                                self.hide_platform_identity(),
                            ));
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
