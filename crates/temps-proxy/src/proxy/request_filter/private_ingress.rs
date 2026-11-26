// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The control plane's private path (ADR-115 D3).
//!
//! A connection that arrived on the consented private address is never
//! matched against the public maps. It gets the answers a worker's private
//! listener gives:
//!
//! - `403 outside-range` when its TCP peer is outside the address's range,
//!   whatever the request says;
//! - `421` when the TLS server name is not the `Host`;
//! - `404 no-route` for a host node `0` does not serve privately;
//! - on plain HTTP, only a redirect to `https://`;
//! - otherwise the request continues with node `0`'s private route, which
//!   `upstream_peer` sends it to. It never falls through to the console.

use super::super::*;
use super::Flow;

use temps_core::private_ingress::PrivateArrival;

/// Set on what the private path refuses, so the inside check (ADR 064 D7)
/// tells the listener's own refusal from the app's answer. The worker's
/// private listener sets the same header.
pub(crate) const PRIVATE_REFUSAL_HEADER: &str = "x-temps-private-ingress";

/// What the private path needs: the shared consent state and node `0`'s
/// private view of the route table, with a project-context resolver over it.
pub struct PrivateIngressPath {
    pub(crate) runtime: Arc<temps_core::private_ingress::ControlPlanePrivateIngress>,
    pub(crate) view: Arc<temps_routes::CachedPeerTable>,
    pub(crate) project_context_resolver: Arc<dyn ProjectContextResolver>,
}

impl PrivateIngressPath {
    pub fn new(
        runtime: Arc<temps_core::private_ingress::ControlPlanePrivateIngress>,
        view: Arc<temps_routes::CachedPeerTable>,
    ) -> Self {
        let project_context_resolver: Arc<dyn ProjectContextResolver> = Arc::new(
            crate::services::ProjectContextResolverImpl::new(Arc::clone(&view)),
        );
        Self {
            runtime,
            view,
            project_context_resolver,
        }
    }
}

impl LoadBalancer {
    /// The project-context resolver for this request: node `0`'s private
    /// view on the private path, the public table otherwise.
    pub(super) fn project_contexts(&self, ctx: &ProxyContext) -> &Arc<dyn ProjectContextResolver> {
        match (&self.private_ingress, ctx.private_route.is_some()) {
            (Some(private), true) => &private.project_context_resolver,
            _ => &self.project_context_resolver,
        }
    }

    pub(super) async fn private_ingress_stage(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<Flow> {
        let Some(private) = self.private_ingress.as_deref() else {
            return Ok(Flow::Continue);
        };
        let peer = session
            .client_addr()
            .and_then(|a| a.as_inet())
            .map(|a| a.ip());
        let local = session.server_addr().and_then(|a| a.as_inet()).copied();
        match private.runtime.arrival(local, peer) {
            PrivateArrival::Public => return Ok(Flow::Continue),
            PrivateArrival::OutsideRange => {
                return self
                    .private_refusal(
                        session,
                        ctx,
                        StatusCode::FORBIDDEN,
                        "outside-range",
                        "this address is only for its private network",
                    )
                    .await;
            }
            PrivateArrival::Admitted => {}
        }
        // The visitor is the TCP peer; no forwarded header is read here.
        ctx.ip_address = peer.map(|ip| ip.to_string());

        let tls = self.is_tls_connection(session);
        if tls && ctx.sni_hostname.as_deref() != Some(ctx.host.as_str()) {
            return self
                .private_refusal(
                    session,
                    ctx,
                    StatusCode::MISDIRECTED_REQUEST,
                    "misdirected",
                    "TLS server name does not match Host",
                )
                .await;
        }
        let Some(address) = private.view.private_address_for(&ctx.host) else {
            return self
                .private_refusal(
                    session,
                    ctx,
                    StatusCode::NOT_FOUND,
                    "no-route",
                    "no private route for this host",
                )
                .await;
        };
        let in_range = match (peer, address.range.as_deref()) {
            (Some(peer), Some(range)) => temps_core::worker_ingress::ip_in_range(peer, range),
            _ => false,
        };
        if !in_range {
            return self
                .private_refusal(
                    session,
                    ctx,
                    StatusCode::FORBIDDEN,
                    "outside-range",
                    "this address is only for its private network",
                )
                .await;
        }
        if !tls {
            let mut response = ResponseHeader::build(StatusCode::PERMANENT_REDIRECT, None)?;
            let location = format!(
                "https://{}{}",
                ctx.host,
                session
                    .req_header()
                    .uri
                    .path_and_query()
                    .map_or("/", |p| p.as_str())
            );
            response.insert_header("Location", location)?;
            response.insert_header("Content-Length", "0")?;
            response.insert_header("X-Request-ID", &ctx.request_id)?;
            session
                .write_response_header(Box::new(response), true)
                .await?;
            ctx.routing_status = "private_https_redirect".to_string();
            return Ok(Flow::Answered);
        }
        let route = private
            .view
            .get_route_by_sni(&ctx.host)
            .or_else(|| private.view.get_route_by_host(&ctx.host))
            .or_else(|| private.view.get_route(&ctx.host));
        match route {
            Some(route) => {
                ctx.private_route = Some(route);
                Ok(Flow::Continue)
            }
            None => {
                self.private_refusal(
                    session,
                    ctx,
                    StatusCode::NOT_FOUND,
                    "no-route",
                    "no private route for this host",
                )
                .await
            }
        }
    }

    async fn private_refusal(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
        status: StatusCode,
        reason: &'static str,
        message: &'static str,
    ) -> Result<Flow> {
        let mut response = ResponseHeader::build(status, None)?;
        response.insert_header(PRIVATE_REFUSAL_HEADER, reason)?;
        response.insert_header("Cache-Control", "no-store")?;
        response.insert_header("X-Request-ID", &ctx.request_id)?;
        response.insert_header("Content-Type", "text/plain; charset=utf-8")?;
        response.insert_header("Content-Length", message.len().to_string())?;
        session
            .write_response_header(Box::new(response), false)
            .await?;
        session
            .write_response_body(Some(Bytes::from_static(message.as_bytes())), true)
            .await?;
        ctx.routing_status = format!("private_{}", reason.replace('-', "_"));
        Ok(Flow::Answered)
    }
}
