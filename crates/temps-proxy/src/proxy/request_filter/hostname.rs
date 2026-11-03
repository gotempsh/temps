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
}
