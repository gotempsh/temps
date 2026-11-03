// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The password wall in front of an environment.

use super::super::*;
use super::Flow;

impl LoadBalancer {
    /// The password wall, when the access decision left the request to it.
    pub(super) async fn password_wall_stage(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
        project_ctx: &ProjectContext,
        password_protection: Option<&temps_entities::deployment_config::PasswordProtectionConfig>,
        access_outcome: crate::ingress::AccessOutcome,
    ) -> Result<Flow> {
        // Password wall: the outcome says the wall decides this request
        // (no valid cookie, or the wall's own verify POST).
        if access_outcome == crate::ingress::AccessOutcome::PasswordWall
            && password_protection.is_none()
        {
            // The compiled endpoint asked for a wall this environment no
            // longer has (a reload in flight): refuse rather than guess.
            let mut response = ResponseHeader::build(StatusCode::SERVICE_UNAVAILABLE, None)?;
            response.insert_header("Retry-After", "1")?;
            response.insert_header("Cache-Control", "no-store")?;
            response.insert_header("X-Request-ID", &ctx.request_id)?;
            response.insert_header("Content-Type", "text/plain; charset=utf-8")?;
            session
                .write_response_header(Box::new(response), false)
                .await?;
            session
                .write_response_body(Some(Bytes::from_static(b"Service unavailable\n")), true)
                .await?;
            ctx.routing_status = "ingress_endpoint_unavailable".to_string();
            return Ok(Flow::Answered);
        }
        if let Some(pp) = password_protection
            .filter(|_| access_outcome == crate::ingress::AccessOutcome::PasswordWall)
        {
            let password_hash = pp.password_hash.clone();
            let env_id = project_ctx.environment.id;
            let project_name = &project_ctx.project.name;
            let environment_name = &project_ctx.environment.name;
            // Check if this is the password verify POST endpoint
            if ctx.path == crate::handler::password_wall::PASSWORD_VERIFY_PATH
                && ctx.method == "POST"
            {
                // Read the POST body to get the password
                let body = session.read_request_body().await.map_err(|e| {
                    error!("Failed to read password verify body: {}", e);
                    e
                })?;

                let body_str = body
                    .as_ref()
                    .map(|b| String::from_utf8_lossy(b).to_string())
                    .unwrap_or_default();

                // Parse form data (application/x-www-form-urlencoded)
                let params: Vec<(String, String)> =
                    url::form_urlencoded::parse(body_str.as_bytes())
                        .into_owned()
                        .collect();

                let password = params
                    .iter()
                    .find(|(k, _)| k == "password")
                    .map(|(_, v)| v.as_str())
                    .unwrap_or("");

                // The destination is client input: reduce it to a
                // same-origin path before it becomes a Location header.
                let redirect = crate::handler::password_wall::sanitize_redirect_path(
                    params
                        .iter()
                        .find(|(k, _)| k == "redirect")
                        .map(|(_, v)| v.as_str())
                        .unwrap_or("/"),
                );

                // Guesses are limited per (client IP, environment) with
                // the same sliding window as the sandbox preview login,
                // so the wall cannot be brute-forced. The attempt is
                // taken atomically before the password is checked, so a
                // concurrent burst cannot outrun the count. An unparsable
                // client address shares one bucket rather than escaping
                // the limit.
                let client_ip = ctx
                    .ip_address
                    .as_deref()
                    .and_then(|s| s.parse::<std::net::IpAddr>().ok())
                    .unwrap_or_else(|| std::net::IpAddr::from([127, 0, 0, 1]));
                if !self
                    .preview_auth_limiter
                    .try_admit_password_wall(client_ip, env_id)
                {
                    warn!(
                        environment_id = env_id,
                        client_ip = %client_ip,
                        "password-wall: verify POST rate limited"
                    );
                    let mut resp = ResponseHeader::build(StatusCode::TOO_MANY_REQUESTS, None)?;
                    resp.insert_header("Retry-After", "60")?;
                    resp.insert_header("Cache-Control", "no-store")?;
                    resp.insert_header("X-Request-ID", &ctx.request_id)?;
                    resp.insert_header("Content-Type", "text/plain; charset=utf-8")?;
                    resp.insert_header("Referrer-Policy", "no-referrer")?;
                    resp.insert_header("X-Frame-Options", "DENY")?;
                    session.write_response_header(Box::new(resp), false).await?;
                    session
                        .write_response_body(
                            Some(Bytes::from_static(
                                b"Too many failed attempts. Try again in a minute.\n",
                            )),
                            true,
                        )
                        .await?;
                    ctx.routing_status = "password_rate_limited".to_string();
                    return Ok(Flow::Answered);
                }

                if crate::handler::password_wall::verify_password(password, &password_hash) {
                    // Password correct — set cookie and redirect
                    self.preview_auth_limiter
                        .clear_password_wall(client_ip, env_id);
                    let host = ctx.host.clone();
                    let set_cookie =
                        crate::handler::password_wall::build_set_cookie_header_for_path(
                            env_id,
                            &password_hash,
                            &host,
                            // A path-routed target is unlocked under
                            // its prefix only, so its cookie coexists
                            // with the host's own `Path=/` one.
                            ctx.path_route
                                .as_ref()
                                .map_or("/", |route| route.prefix.as_str()),
                        );

                    let mut resp = ResponseHeader::build(303, None)?;
                    resp.insert_header("Location", redirect)?;
                    resp.insert_header("Set-Cookie", &set_cookie)?;
                    resp.insert_header("Cache-Control", "no-store")?;
                    resp.insert_header("Referrer-Policy", "no-referrer")?;
                    resp.insert_header("X-Request-ID", &ctx.request_id)?;

                    session.write_response_header(Box::new(resp), true).await?;
                    ctx.routing_status = "password_verified".to_string();
                    return Ok(Flow::Answered);
                } else {
                    // Wrong password — the attempt is already counted;
                    // show the form again with an error
                    let html = crate::handler::password_wall::generate_password_form_html_for_route(
                        redirect,
                        true,
                        project_name,
                        environment_name,
                        // Behind a path route the form posts back
                        // the routed path, so the next attempt
                        // resolves to this environment again.
                        crate::handler::password_wall::routed_form_path(
                            ctx.path_route.is_some(),
                            &ctx.path,
                            ctx.query_string.as_deref(),
                        )
                        .as_deref(),
                    );
                    let html_bytes = Bytes::from(html);

                    let mut resp = ResponseHeader::build(StatusCode::OK, None)?;
                    resp.insert_header("Content-Type", "text/html; charset=utf-8")?;
                    resp.insert_header("Cache-Control", "no-store")?;
                    resp.insert_header("X-Request-ID", &ctx.request_id)?;
                    // A credential prompt must not be framed or leak its URL.
                    resp.insert_header("Referrer-Policy", "no-referrer")?;
                    resp.insert_header("X-Frame-Options", "DENY")?;

                    session.write_response_header(Box::new(resp), false).await?;
                    session.write_response_body(Some(html_bytes), true).await?;
                    ctx.routing_status = "password_wrong".to_string();
                    return Ok(Flow::Answered);
                }
            }

            // Check for valid password cookie. Several cookies may carry
            // this name: the host's own environment (`Path=/`) and a
            // path-routed target's (`Path=<prefix>`) coexist, and the
            // browser sends both on a routed path. Each is bound to its
            // environment id and password hash, so any one that
            // validates for THIS environment unlocks it.
            let has_valid_cookie = find_request_cookie(
                session.req_header(),
                crate::handler::password_wall::PASSWORD_COOKIE_NAME,
                |value| {
                    crate::handler::password_wall::validate_cookie(value, env_id, &password_hash)
                        .then_some(true)
                },
            )
            .unwrap_or(false);

            if !has_valid_cookie {
                // No valid cookie — show password form
                let current_path = if let Some(ref qs) = ctx.query_string {
                    if qs.is_empty() {
                        ctx.path.clone()
                    } else {
                        format!("{}?{}", ctx.path, qs)
                    }
                } else {
                    ctx.path.clone()
                };

                let html = crate::handler::password_wall::generate_password_form_html_for_route(
                    &current_path,
                    false,
                    project_name,
                    environment_name,
                    // Behind a path route the form posts back the routed
                    // path, so the verify request resolves to this
                    // (target) environment, not the host's own.
                    crate::handler::password_wall::routed_form_path(
                        ctx.path_route.is_some(),
                        &ctx.path,
                        ctx.query_string.as_deref(),
                    )
                    .as_deref(),
                );
                let html_bytes = Bytes::from(html);

                let mut resp = ResponseHeader::build(StatusCode::OK, None)?;
                resp.insert_header("Content-Type", "text/html; charset=utf-8")?;
                resp.insert_header("Cache-Control", "no-store")?;
                resp.insert_header("X-Request-ID", &ctx.request_id)?;
                // A credential prompt must not be framed or leak its URL.
                resp.insert_header("Referrer-Policy", "no-referrer")?;
                resp.insert_header("X-Frame-Options", "DENY")?;

                session.write_response_header(Box::new(resp), false).await?;
                session.write_response_body(Some(html_bytes), true).await?;
                ctx.routing_status = "password_wall".to_string();
                return Ok(Flow::Answered);
            }
        }

        Ok(Flow::Continue)
    }
}
