// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a request carries when it goes upstream.

use super::super::*;

impl LoadBalancer {
    /// Make the request what the upstream may see, and keep what the request
    /// log and tracking read from it. Only a request about to be forwarded
    /// gets this far: nothing an earlier stage answered pays for it.
    pub(super) fn prepare_upstream_request(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<()> {
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

        Ok(())
    }
}
