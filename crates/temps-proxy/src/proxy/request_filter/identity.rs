// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The proxy's answer to this machine asking who holds its ports (host
//! facts). Something else bound to the proxy's address answers a TCP
//! connection too, so only an answer to a challenge under the installation
//! key shows it is Temps. Answered only to a signed probe: to anyone else
//! the path is an ordinary request.

use super::super::*;
use super::Flow;
use temps_core::probe_auth;

impl LoadBalancer {
    pub(super) async fn proxy_identity_stage(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<Flow> {
        if ctx.path != probe_auth::PROXY_IDENTITY_PATH || !self.authenticated_probe(session) {
            return Ok(Flow::Continue);
        }
        let nonce = session
            .req_header()
            .headers
            .get(probe_auth::PROXY_IDENTITY_NONCE_HEADER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        let key = self
            .config_service
            .get_server_config()
            .encryption_key
            .clone();
        let Some(answer) = probe_auth::identity_answer(&key, nonce) else {
            return Ok(Flow::Continue);
        };
        let mut response = ResponseHeader::build(204, None)?;
        response.insert_header(probe_auth::PROXY_IDENTITY_HEADER, answer)?;
        response.insert_header("Cache-Control", "private, no-store")?;
        response.insert_header("Content-Length", "0")?;
        session
            .write_response_header(Box::new(response), true)
            .await?;
        ctx.routing_status = "proxy_identity".to_string();
        Ok(Flow::Answered)
    }
}
