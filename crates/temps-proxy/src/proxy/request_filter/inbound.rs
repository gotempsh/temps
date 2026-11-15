// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a request says and what it may not.

use super::super::*;
use super::Flow;

impl LoadBalancer {
    /// What every later stage reads about the request: its start time, id,
    /// host, method, path, query, user agent and client address.
    ///
    /// A request with no usable Host is answered here: nothing later can route it.
    pub(super) async fn read_request_facts(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<Flow> {
        // Set the started_at time here
        ctx.start_time = Instant::now();

        // Add the request ID to the request headers
        session
            .req_header_mut()
            .insert_header("X-Request-ID", &ctx.request_id)?;

        ctx.host = match self.get_host_header(session) {
            Ok(host) => host,
            Err(error) => {
                return self
                    .answer_invalid_host(session, ctx, &error)
                    .await
                    .map(Flow::finished);
            }
        };
        ctx.method = session.req_header().method.to_string();
        ctx.path = session.req_header().uri.path().to_string();
        ctx.query_string = session.req_header().uri.query().map(|q| q.to_string());
        ctx.user_agent = session
            .req_header()
            .headers
            .get("user-agent")
            .map(|h| h.to_str().unwrap_or_default().to_string())
            .unwrap_or_default();

        // Extract client IP address early (needed for Shield)
        if let Some(client_ip) = resolve_session_client_ip(
            session,
            self.trust_loopback_forwarded_ip.load(Ordering::Relaxed),
        ) {
            ctx.ip_address = Some(client_ip);
        }

        Ok(Flow::Continue)
    }

    /// Remove what only the proxy may set, before anything can read it.
    pub(super) fn strip_inbound_trust_headers(session: &mut PingoraSession) {
        // SECURITY: Strip any inbound X-Temps-Demo-Mode header. Demo mode
        // has been removed; clients sending this header should never have it
        // honored by downstream auth middleware.
        let _ = session.req_header_mut().remove_header("X-Temps-Demo-Mode");

        // SECURITY: only the signed-in gate tells an app who is signed in
        // (ADR 065 D8); a client's own identity headers never get through.
        crate::handler::gate::strip_client_identity(session.req_header_mut());

        // SECURITY: only the status-host branch below may tag a request with
        // the custom hostname it was served for (#1412).
        let _ = session
            .req_header_mut()
            .remove_header(temps_core::public_status_page::STATUS_HOST_HEADER);
    }
}
