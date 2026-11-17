// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Path routes (ADR 050): another service of the project serving a path.

use super::super::*;
use super::Flow;

impl LoadBalancer {
    /// Which service of the project serves this path, if not the host's own.
    pub(super) fn match_path_route(&self, ctx: &mut ProxyContext) {
        // Path routes (ADR 050): `frontend: /api -> backend` serves this
        // path from another service of the project. In-memory lookup only.
        //
        // The password form rendered for a path-routed target posts to the
        // reserved `/_temps/password-verify`, which never matches a path
        // route; `resolve_request_path_route` matches it by the routed path
        // the form carries, so the password is checked (and the cookie
        // issued) for the target environment the form was rendered for,
        // never the host's own. The Temps-login exchange callback is matched
        // the same way, by the `next` path its sign-in started from, so the
        // grant is traded (and the cookie scoped) for the routed target.
        // Client input only selects among the prefixes this host already
        // serves.
        if ctx.preview_route.is_none() {
            ctx.path_route = crate::handler::password_wall::resolve_request_path_route(
                &ctx.method,
                &ctx.path,
                ctx.query_string.as_deref(),
                |path| {
                    self.project_context_resolver
                        .match_path_route(&ctx.host, path)
                },
            );
        }
    }

    /// A path route whose target cannot serve answers for itself.
    pub(super) async fn unavailable_path_target_stage(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<Flow> {
        if let Some(path_route) = ctx.path_route.as_ref() {
            if path_route.target.is_none() {
                // Never let the host's own service (or another environment)
                // answer in the target's place: that hides the problem
                // behind the frontend's index.html. The body carries only
                // the requested prefix; the service name goes to the log.
                warn!(
                    host = %ctx.host,
                    prefix = %path_route.prefix,
                    target_service = %path_route.target_service,
                    "path route target has no running deployment in this environment"
                );
                let body = Bytes::from(
                    serde_json::json!({
                        "status": "path_route_unavailable",
                        "message": format!(
                            "The service that serves {} is not deployed in this environment",
                            path_route.prefix
                        ),
                    })
                    .to_string(),
                );
                let mut response = ResponseHeader::build(StatusCode::BAD_GATEWAY, None)?;
                response.insert_header("Cache-Control", "no-store")?;
                response.insert_header("X-Request-ID", &ctx.request_id)?;
                response.insert_header("Content-Type", "application/json")?;
                response.insert_header("Content-Length", body.len().to_string())?;
                session
                    .write_response_header(Box::new(response), false)
                    .await?;
                session.write_response_body(Some(body), true).await?;
                ctx.routing_status = "path_route_unavailable".to_string();
                return Ok(Flow::Answered);
            }
        }
        Ok(Flow::Continue)
    }
}
