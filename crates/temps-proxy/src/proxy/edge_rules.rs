// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Applying a project's edge rules (ADR-089) to a request.

use super::*;

impl LoadBalancer {
    /// for its DNS check.
    /// Applies the project's edge rules (ADR-089). `Ok(true)` when a rule
    /// answered the request (block, redirect or challenge).
    ///
    /// Cost for a project without rules: one lock-free map lookup. With
    /// rules: the engine's evaluation, a GeoIP lookup only when a rule tests
    /// geography, and a clearance check only when a challenge rule matches.
    pub(super) async fn enforce_edge_rules(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
        project_id: i32,
        environment_id: i32,
        client_ip: Option<std::net::IpAddr>,
    ) -> Result<bool> {
        use temps_edge_rules::Decision;

        let Some(rules) = self
            .edge_rules
            .as_ref()
            .and_then(|gate| gate.rules_for(project_id))
        else {
            return Ok(false);
        };
        if crate::edge_rules::is_exempt_path(&ctx.path) {
            return Ok(false);
        }
        let geo = match (&self.edge_geo, client_ip) {
            (Some(geo), Some(ip)) if rules.needs_geo => geo
                .geolocate(ip)
                .await
                .ok()
                .map(crate::edge_rules::GeoSnapshot::from),
            _ => None,
        };
        let ja4 = ctx
            .client_fingerprint
            .and_then(|fingerprint| fingerprint.ja4);
        let ja4_text = ja4.as_ref().map(|ja4| ja4.as_str());
        let identity_source = match ja4_text {
            Some(ja4) => std::borrow::Cow::Borrowed(ja4),
            None => std::borrow::Cow::Owned(format!("none:{}", shield_source(ctx))),
        };
        let identity =
            temps_shield::VisitorIdentity::compute(client_ip, &ctx.user_agent, &identity_source);
        let now = temps_shield::unix_now();
        let (evaluation, navigation) = {
            let header = session.req_header();
            let gate = self.shield.engine.gate();
            let clearance = || {
                find_request_cookie(
                    header,
                    &temps_shield::clearance_cookie_name(environment_id),
                    |value| Some(value.to_string()),
                )
                .is_some_and(|cookie| {
                    gate.admit(&cookie, environment_id, &identity, now)
                        == temps_shield::Admission::Admitted
                })
            };
            let view = crate::edge_rules::ProxyRequestView::new(
                header,
                &ctx.host,
                client_ip,
                &ctx.user_agent,
                ja4_text,
                geo.as_ref(),
                environment_id,
                &clearance,
            );
            let evaluation = rules.program.evaluate(&view);
            let accept = header
                .headers
                .get("accept")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default();
            let navigation = (header.method == pingora::http::Method::GET
                || header.method == pingora::http::Method::HEAD)
                && accept
                    .split(',')
                    .any(|part| part.trim().starts_with("text/html"));
            (evaluation, navigation)
        };
        if let Some(matched) = evaluation.first_match {
            debug!(
                project_id,
                environment_id,
                rule_id = matched.rule_id,
                mode = matched.mode.as_str(),
                request_id = %ctx.request_id,
                "Edge rule matched"
            );
        }

        match evaluation.decision {
            Decision::Forward | Decision::Allow { .. } => Ok(false),
            Decision::Block {
                rule_id,
                status,
                body,
            } => {
                let status = StatusCode::from_u16(status).unwrap_or(StatusCode::FORBIDDEN);
                let body = if body.is_empty() {
                    format!(
                        "{}\n",
                        status.canonical_reason().unwrap_or("Request blocked")
                    )
                } else {
                    body
                };
                let body = Bytes::from(body);
                let mut response = ResponseHeader::build(status, None)?;
                response.insert_header("Content-Type", "text/plain; charset=utf-8")?;
                response.insert_header("Content-Length", body.len().to_string())?;
                response.insert_header("Cache-Control", "no-store")?;
                response.insert_header("X-Request-ID", &ctx.request_id)?;
                session
                    .write_response_header(Box::new(response), false)
                    .await?;
                let length = body.len() as i64;
                session.write_response_body(Some(body), true).await?;
                ctx.routing_status = "edge_rule_blocked".to_string();
                ctx.error_message = Some(format!("Blocked by edge rule {rule_id}"));
                self.log_failed_request(ctx, status.as_u16(), Some(length));
                Ok(true)
            }
            Decision::Redirect {
                rule_id,
                status,
                location,
            } => {
                let status = StatusCode::from_u16(status).unwrap_or(StatusCode::TEMPORARY_REDIRECT);
                let mut response = ResponseHeader::build(status, None)?;
                response.insert_header("Location", location.as_str())?;
                response.insert_header("Content-Length", "0")?;
                response.insert_header("X-Request-ID", &ctx.request_id)?;
                session
                    .write_response_header(Box::new(response), true)
                    .await?;
                ctx.routing_status = "edge_rule_redirected".to_string();
                ctx.error_message = Some(format!("Redirected by edge rule {rule_id}"));
                self.log_failed_request(ctx, status.as_u16(), Some(0));
                Ok(true)
            }
            Decision::Challenge { rule_id } => {
                // The engine only decides `challenge` for a visitor without
                // a valid clearance; serve Shield's challenge for it.
                let tls = ctx.tls_fingerprint.is_some() || self.is_tls_connection(session);
                let outcome = if !navigation {
                    temps_shield::Outcome::TooManyRequests
                } else if !tls {
                    temps_shield::Outcome::HttpsRequired
                } else {
                    let difficulty = temps_shield::difficulty::FIRST;
                    match self.shield.engine.gate().issue(
                        environment_id,
                        &identity,
                        difficulty,
                        now,
                    ) {
                        Ok(token) => temps_shield::Outcome::Challenge { token, difficulty },
                        Err(e) => {
                            warn!(
                                project_id,
                                environment_id,
                                rule_id,
                                error = %e,
                                "Edge rule challenge could not be issued"
                            );
                            temps_shield::Outcome::Unavailable
                        }
                    }
                };
                ctx.error_message = Some(format!("Challenged by edge rule {rule_id}"));
                self.answer_shield(session, ctx, outcome, Some("edge_rule_challenged"))
                    .await
            }
        }
    }
}
