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
}
