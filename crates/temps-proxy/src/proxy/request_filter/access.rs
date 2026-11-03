// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The responses to the access decision (ADR-055 §11).

use super::super::*;
use super::Flow;

impl LoadBalancer {
    /// Answer the access decision (ADR-055 §11), or pass the request on when
    /// a later stage decides it.
    pub(super) async fn access_outcome_stage(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
        project_ctx: &ProjectContext,
        access_outcome: crate::ingress::AccessOutcome,
        parsed_ip: Option<std::net::IpAddr>,
    ) -> Result<Flow> {
        match access_outcome {
            crate::ingress::AccessOutcome::Allow | crate::ingress::AccessOutcome::PasswordWall => {}
            crate::ingress::AccessOutcome::IpForbidden
            | crate::ingress::AccessOutcome::Forbidden => {
                // Counted for every denial; logged through a
                // process-wide limiter so a scan against a locked
                // project cannot flood the log (the counter keeps
                // the true rate).
                self.proxy_metrics.record_ip_access_denied();
                let now_secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                if let Some(suppressed) = IP_DENY_LOG_LIMITER.admit(now_secs) {
                    warn!(
                        environment_id = project_ctx.environment.id,
                        project_id = project_ctx.project.id,
                        ip = %parsed_ip.map(|ip| ip.to_string()).unwrap_or_else(|| "unresolved".to_string()),
                        outcome = access_outcome.label(),
                        suppressed_since_last_log = suppressed,
                        "Request denied by project IP restriction"
                    );
                }
                // A generic 403 with no detail: an unauthenticated
                // caller must not be able to tell "this project is
                // restricted" from "this project does not exist".
                let mut response = ResponseHeader::build(StatusCode::FORBIDDEN, None)?;
                response.insert_header("Cache-Control", "no-store")?;
                response.insert_header("X-Request-ID", &ctx.request_id)?;
                response.insert_header("Content-Type", "text/plain; charset=utf-8")?;
                session
                    .write_response_header(Box::new(response), false)
                    .await?;
                session
                    .write_response_body(Some(Bytes::from_static(b"Forbidden\n")), true)
                    .await?;
                ctx.routing_status =
                    if access_outcome == crate::ingress::AccessOutcome::IpForbidden {
                        "project_ip_restricted"
                    } else {
                        "ingress_forbidden"
                    }
                    .to_string();
                return Ok(Flow::Answered);
            }
            crate::ingress::AccessOutcome::Blacklisted => {
                self.proxy_metrics.record_rate_limited();
                warn!(
                    project_id = project_ctx.project.id,
                    environment_id = project_ctx.environment.id,
                    ip = %parsed_ip.map(|ip| ip.to_string()).unwrap_or_else(|| "unresolved".to_string()),
                    "Request denied by rate-limit blacklist"
                );
                // Same generic 403 as the IP restriction above.
                let mut response = ResponseHeader::build(StatusCode::FORBIDDEN, None)?;
                response.insert_header("Cache-Control", "no-store")?;
                response.insert_header("X-Request-ID", &ctx.request_id)?;
                response.insert_header("Content-Type", "text/plain; charset=utf-8")?;
                session
                    .write_response_header(Box::new(response), false)
                    .await?;
                session
                    .write_response_body(Some(Bytes::from_static(b"Forbidden\n")), true)
                    .await?;
                ctx.routing_status = "rate_limit_blacklisted".to_string();
                return Ok(Flow::Answered);
            }
            crate::ingress::AccessOutcome::TooManyRequests {
                window,
                retry_after_secs,
            } => {
                self.proxy_metrics.record_rate_limited();
                debug!(
                    project_id = project_ctx.project.id,
                    environment_id = project_ctx.environment.id,
                    ip = %parsed_ip.map(|ip| ip.to_string()).unwrap_or_else(|| "unresolved".to_string()),
                    window = window.as_str(),
                    "Request rejected by rate limit"
                );
                let mut response = ResponseHeader::build(StatusCode::TOO_MANY_REQUESTS, None)?;
                response.insert_header("Retry-After", retry_after_secs.to_string())?;
                response.insert_header("Cache-Control", "no-store")?;
                response.insert_header("X-Request-ID", &ctx.request_id)?;
                response.insert_header("Content-Type", "text/plain; charset=utf-8")?;
                session
                    .write_response_header(Box::new(response), false)
                    .await?;
                session
                    .write_response_body(Some(Bytes::from_static(b"Too Many Requests\n")), true)
                    .await?;
                ctx.routing_status = "rate_limited".to_string();
                return Ok(Flow::Answered);
            }
            crate::ingress::AccessOutcome::Unavailable => {
                // Logged (rate-limited, with the reason) where it was decided.
                let mut response = ResponseHeader::build(StatusCode::SERVICE_UNAVAILABLE, None)?;
                response.insert_header("Cache-Control", "no-store")?;
                response.insert_header("X-Request-ID", &ctx.request_id)?;
                response.insert_header("Content-Type", "text/plain; charset=utf-8")?;
                session
                    .write_response_header(Box::new(response), false)
                    .await?;
                session
                        .write_response_body(
                            Some(Bytes::from_static(
                                b"Service unavailable: this environment's access policy is not available\n",
                            )),
                            true,
                        )
                        .await?;
                ctx.routing_status = "ingress_endpoint_unavailable".to_string();
                return Ok(Flow::Answered);
            }
            crate::ingress::AccessOutcome::LoginRequired
                if ctx.path == crate::ingress::GRANT_EXCHANGE_PATH =>
            {
                let secure = self.is_https_request(session);
                // A path-routed target (resolved from the exchange's
                // `next`) gets a cookie scoped to its prefix.
                let exchange = self.exchange_ingress_grant(
                    project_ctx.environment.id,
                    &ctx.host,
                    ctx.query_string.as_deref(),
                    secure,
                    ctx.path_route.as_ref().map(|route| route.prefix.as_str()),
                );
                // The query holds the grant: keep it out of the proxy
                // log, which outlives the grant and is readable by
                // project members.
                ctx.query_string = None;
                let mut response = match exchange {
                    Ok(exchange) => {
                        info!(
                            project_id = project_ctx.project.id,
                            environment_id = project_ctx.environment.id,
                            grant_id = exchange.grant_id,
                            user_id = exchange.user_id,
                            "Temps login grant exchanged for an ingress session"
                        );
                        let mut response = ResponseHeader::build(StatusCode::SEE_OTHER, None)?;
                        response.insert_header("Location", exchange.location)?;
                        response.insert_header("Set-Cookie", exchange.set_cookie)?;
                        response
                    }
                    Err(error) => {
                        warn!(
                            project_id = project_ctx.project.id,
                            environment_id = project_ctx.environment.id,
                            error = %error,
                            "Refused a Temps login grant exchange"
                        );
                        ResponseHeader::build(StatusCode::FORBIDDEN, None)?
                    }
                };
                let refused = response.status != StatusCode::SEE_OTHER;
                response.insert_header("Cache-Control", "no-store")?;
                response.insert_header("Referrer-Policy", "no-referrer")?;
                response.insert_header("X-Request-ID", &ctx.request_id)?;
                response.insert_header("Content-Type", "text/plain; charset=utf-8")?;
                session
                    .write_response_header(Box::new(response), false)
                    .await?;
                let body: &'static [u8] = if refused {
                    b"This sign-in link is invalid or has expired. Reload the page to sign in again.\n"
                } else {
                    b"Signed in.\n"
                };
                session
                    .write_response_body(Some(Bytes::from_static(body)), true)
                    .await?;
                ctx.routing_status = if refused {
                    "ingress_grant_refused"
                } else {
                    "ingress_grant_exchanged"
                }
                .to_string();
                return Ok(Flow::Answered);
            }
            // An edge rule challenge runs before this decision, so its
            // solver and verification must work before sign-in; Shield
            // answers them below.
            crate::ingress::AccessOutcome::LoginRequired
                if crate::ingress::answered_before_login(&ctx.path) => {}
            crate::ingress::AccessOutcome::LoginRequired => {
                let external_url = self
                    .config_service
                    .get_settings()
                    .await
                    .ok()
                    .and_then(|settings| settings.external_url);
                let next = match ctx.query_string.as_deref() {
                    Some(qs) if !qs.is_empty() => format!("{}?{}", ctx.path, qs),
                    _ => ctx.path.clone(),
                };
                let scheme = if self.is_https_request(session) {
                    crate::ingress::LoginScheme::Https
                } else {
                    crate::ingress::LoginScheme::Http
                };
                let now_secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let port = self
                    .request_authority(session)
                    .ok()
                    .and_then(|authority| authority.port);
                let location = external_url.as_deref().and_then(|base| {
                    crate::ingress::login_redirect_url(
                        base,
                        self.crypto.ingress_grant_master_key(),
                        temps_workload_ingress::ProductRef {
                            kind: temps_workload_ingress::ProductKind::Environment,
                            id: i64::from(project_ctx.environment.id),
                        },
                        crate::ingress::LoginSite {
                            scheme,
                            host: &ctx.host,
                            port,
                        },
                        &next,
                        now_secs,
                    )
                });
                let mut response = match location {
                    Some(location) => {
                        let mut response = ResponseHeader::build(StatusCode::SEE_OTHER, None)?;
                        response.insert_header("Location", location)?;
                        response
                    }
                    None => {
                        warn!(
                                project_id = project_ctx.project.id,
                                environment_id = project_ctx.environment.id,
                                "Environment requires a Temps login but the instance has no valid external URL to send the visitor to"
                            );
                        ResponseHeader::build(StatusCode::FORBIDDEN, None)?
                    }
                };
                response.insert_header("Cache-Control", "no-store")?;
                response.insert_header("Referrer-Policy", "no-referrer")?;
                response.insert_header("X-Request-ID", &ctx.request_id)?;
                response.insert_header("Content-Type", "text/plain; charset=utf-8")?;
                session
                    .write_response_header(Box::new(response), false)
                    .await?;
                session
                    .write_response_body(
                        Some(Bytes::from_static(b"Sign in to Temps to open this page.\n")),
                        true,
                    )
                    .await?;
                ctx.routing_status = "ingress_login_required".to_string();
                return Ok(Flow::Answered);
            }
            // Bot challenges are Shield's (ADR 066), which runs right
            // after this decision; the access decision defers to it.
            crate::ingress::AccessOutcome::HttpsRequired
            | crate::ingress::AccessOutcome::CaptchaChallenge => {}
        }

        Ok(Flow::Continue)
    }
}
