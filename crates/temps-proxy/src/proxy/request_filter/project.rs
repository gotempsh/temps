// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a request for a project must pass, in order.

use super::super::*;
use super::Flow;

impl LoadBalancer {
    /// The project the request belongs to: the path route's target if it has
    /// one, otherwise the host's.
    pub(super) async fn resolve_project_context(
        &self,
        ctx: &ProxyContext,
    ) -> Option<ProjectContext> {
        // Resolve project context early to set routing status for all requests
        let path_target_context = ctx
            .path_route
            .as_ref()
            .and_then(|route| route.target.as_ref())
            .and_then(|target| {
                Some(ProjectContext {
                    project: target.project.clone()?,
                    environment: target.environment.clone()?,
                    deployment: target.deployment.clone()?,
                    // A target is only attached once it has a live backend.
                    upstream_unavailable: false,
                })
            });
        match path_target_context {
            Some(context) => Some(context),
            None => {
                self.project_context_resolver
                    .resolve_context(&ctx.host)
                    .await
            }
        }
    }

    /// Everything a request for a project must pass, in order.
    pub(super) async fn project_stage(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
        project_ctx: &ProjectContext,
    ) -> Result<Flow> {
        ctx.project = Some(project_ctx.project.clone());
        ctx.environment = Some(project_ctx.environment.clone());
        ctx.deployment = Some(project_ctx.deployment.clone());
        ctx.routing_status = "routed".to_string();

        // Record activity for on-demand idle tracking. A request that
        // will be answered 503 (no live upstream) is not activity: it must
        // not keep an environment with dead containers from idling to
        // sleep, after which the next request wakes it cleanly.
        if let Some(ref on_demand) = self.on_demand_manager {
            if !project_ctx.upstream_unavailable {
                on_demand.record_activity(project_ctx.environment.id);
            }
        }

        // Per-project/environment concurrent-connection cap (issue #646): a slow
        // or malicious upstream must not be able to exhaust the proxy's own
        // connection budget now that request timeouts are opt-in (PR #642). Keyed
        // on environment id (the actual upstream-instance granularity); global
        // default and the project/environment DeploymentConfig override are
        // resolved the same way as the timeout settings above.
        // One TTL-cached settings read serves both the connection cap and
        // the rate limits below (an in-memory read, not a DB round-trip).
        let (connection_limits, instance_rate_limiting) = self
            .config_service
            .get_settings()
            .await
            .map(|settings| (settings.connection_limits, settings.rate_limiting))
            .unwrap_or_default();
        let project_config = project_ctx
            .project
            .deployment_config
            .clone()
            .unwrap_or_default();
        let effective_config = project_ctx
            .environment
            .get_effective_deployment_config(&project_config);
        let connection_limit = effective_config
            .max_concurrent_connections
            .map(|v| v.max(0) as u32)
            .unwrap_or(connection_limits.default_max_concurrent_connections);

        stage!(self.connection_cap_stage(session, ctx, project_ctx, connection_limit));

        // Per-project/environment IP restriction. Synchronous,
        // lock-free — see temps_core::ProjectIpGate's contract. Denial
        // is a generic 403 with no detail: an unauthenticated caller
        // must not be able to distinguish "this project doesn't exist"
        // from "this project is restricted and you're not on the
        // allowlist" by response shape.
        //
        // `ctx.ip_address` can fail to resolve to a parseable `IpAddr`
        // (non-INET socket, missing/garbled forwarded-for value —
        // resolve_session_client_ip falls back to the literal
        // "unknown"). We must not silently skip enforcement in that
        // case: fail closed (deny) when this project/environment
        // actually has an active restriction policy, since an
        // unresolvable IP under an active policy is exactly what an
        // attacker (or a misconfigured proxy) would produce. When there
        // is no policy at all for this project/environment — the common
        // case — an unresolvable IP must NOT deny, matching today's
        // behavior; this feature is opt-in and most traffic has nothing
        // to do with it.
        let parsed_ip = ctx
            .ip_address
            .as_deref()
            .and_then(|s| s.parse::<std::net::IpAddr>().ok())
            .map(normalize_client_ip);
        let decision = evaluate_request_policy(
            self.request_policy_gate.as_ref(),
            session.req_header(),
            &ctx.host,
            project_ctx.project.id,
            project_ctx.environment.id,
            parsed_ip,
        );
        let ip_restricted = legacy_ip_gate_denies(
            decision,
            self.project_ip_gate.as_ref(),
            project_ctx.project.id,
            project_ctx.environment.id,
            parsed_ip,
        );
        stage!(self.request_policy_stage(session, ctx, project_ctx, decision));

        let password_protection = project_ctx
            .environment
            .deployment_config
            .as_ref()
            .and_then(|dc| dc.security.as_ref())
            .and_then(|s| s.password_protection.as_ref())
            .filter(|pp| pp.enabled);
        // The password form's POST: with a wall, the wall always handles
        // it (it never reaches the app), so a rule must not stop visitors
        // from unlocking. Without one, the path is the app's.
        let password_verify_post = password_protection.is_some()
            && ctx.path == crate::handler::password_wall::PASSWORD_VERIFY_PATH
            && ctx.method == "POST";

        // Project edge rules (ADR-089): after the operator's IP gate,
        // before rate limits, Shield and the password wall, none of which
        // an `allow` rule skips. ACME, Temps' own challenge endpoints and
        // the password form are never subject to them.
        if !password_verify_post
            && self
                .enforce_edge_rules(
                    session,
                    ctx,
                    project_ctx.project.id,
                    project_ctx.environment.id,
                    parsed_ip,
                )
                .await?
        {
            return Ok(Flow::Answered);
        }

        // The access decision (ADR-055 §11): the legacy chain, or the
        // environment's compiled ingress endpoint, as its
        // `ingress_mode` says. Either way it is one `AccessOutcome`,
        // answered below by the same responses.
        let access_outcome = self
            .decide_environment_access(
                project_ctx,
                AccessRequest {
                    request: session.req_header(),
                    host: &ctx.host,
                    path: &ctx.path,
                    tls_fingerprint: ctx.tls_fingerprint.as_deref(),
                    policy_decision: decision,
                    ip_restricted,
                    client_ip: parsed_ip,
                    rate_limit: crate::rate_limiter::resolve_policy(
                        &instance_rate_limiting,
                        effective_config.security.as_ref(),
                        project_ctx.environment.id,
                    ),
                    password: password_protection.map(|pp| crate::ingress::PasswordFacts {
                        verify_post: ctx.path
                            == crate::handler::password_wall::PASSWORD_VERIFY_PATH
                            && ctx.method == "POST",
                        cookie_valid: find_request_cookie(
                            session.req_header(),
                            crate::handler::password_wall::PASSWORD_COOKIE_NAME,
                            |value| {
                                crate::handler::password_wall::validate_cookie(
                                    value,
                                    project_ctx.environment.id,
                                    &pp.password_hash,
                                )
                                .then_some(true)
                            },
                        )
                        .unwrap_or(false),
                    }),
                },
            )
            .await;

        stage!(self.access_outcome_stage(session, ctx, project_ctx, access_outcome, parsed_ip));

        stage!(self.shield_stage(session, ctx, project_ctx, parsed_ip));

        self.password_wall_stage(
            session,
            ctx,
            project_ctx,
            password_protection,
            access_outcome,
        )
        .await
    }

    /// Reserve the request's place under the environment's connection cap.
    async fn connection_cap_stage(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
        project_ctx: &ProjectContext,
        connection_limit: u32,
    ) -> Result<Flow> {
        match self
            .connection_limiter
            .try_acquire(project_ctx.environment.id, connection_limit)
        {
            Some(permit) => ctx.connection_permit = Some(permit),
            None => {
                warn!(
                    environment_id = project_ctx.environment.id,
                    project_id = project_ctx.project.id,
                    limit = connection_limit,
                    "Environment at concurrent-connection capacity; rejecting request"
                );
                let mut response = ResponseHeader::build(StatusCode::SERVICE_UNAVAILABLE, None)?;
                response.insert_header("Retry-After", "1")?;
                response.insert_header("Cache-Control", "no-store")?;
                response.insert_header("X-Request-ID", &ctx.request_id)?;
                response.insert_header("Content-Type", "application/json")?;
                // Generic body/message: this must not be distinguishable from the
                // proxy's other 503 responses (e.g. no upstream route found), or
                // an unauthenticated caller could use it as an oracle to confirm
                // that a given hostname routes to a real project/environment.
                let body_bytes = Bytes::from_static(
                        br#"{"status":"service_unavailable","message":"Service temporarily unavailable, please retry"}"#,
                    );
                session
                    .write_response_header(Box::new(response), false)
                    .await?;
                session.write_response_body(Some(body_bytes), true).await?;
                ctx.routing_status = "connection_limit_exceeded".to_string();
                return Ok(Flow::Answered);
            }
        }

        Ok(Flow::Continue)
    }

    /// Answer what the request policy gate refused.
    async fn request_policy_stage(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
        project_ctx: &ProjectContext,
        decision: temps_core::RequestPolicyDecision,
    ) -> Result<Flow> {
        if let temps_core::RequestPolicyDecision::Unavailable { reason } = decision {
            warn!(
                project_id = project_ctx.project.id,
                environment_id = project_ctx.environment.id,
                reason,
                "Project policy unavailable; denying request"
            );
            let mut response = ResponseHeader::build(StatusCode::SERVICE_UNAVAILABLE, None)?;
            response.insert_header("Cache-Control", "no-store")?;
            response.insert_header("X-Request-ID", &ctx.request_id)?;
            response.insert_header("Content-Type", "text/plain; charset=utf-8")?;
            session
                .write_response_header(Box::new(response), false)
                .await?;
            session
                .write_response_body(Some(Bytes::from_static(b"Service unavailable\n")), true)
                .await?;
            ctx.routing_status = "request_policy_unavailable".to_string();
            return Ok(Flow::Answered);
        }
        if let temps_core::RequestPolicyDecision::Deny {
            reason,
            rule_id,
            revision,
        } = decision
        {
            warn!(
                project_id = project_ctx.project.id,
                environment_id = project_ctx.environment.id,
                reason,
                rule_id,
                revision,
                "Request denied by project policy"
            );
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
            ctx.routing_status = "request_policy_denied".to_string();
            return Ok(Flow::Answered);
        }

        Ok(Flow::Continue)
    }

    /// Shield (ADR 066): its own paths, a stored challenge, then its verdict.
    async fn shield_stage(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
        project_ctx: &ProjectContext,
        parsed_ip: Option<std::net::IpAddr>,
    ) -> Result<Flow> {
        // ADR 066 (I4): Shield, after the operator's IP gate and the
        // rate limits, so an operator deny rule or a spent rate budget
        // answers first and is not a Shield decision (D1). The verify
        // POST and the solver the page loads are answered here, whichever
        // environment this host or path resolved to: the clearance is for
        // the environment in the signed token, and a page served just
        // before Shield was turned off can still finish. Neither they nor
        // ACME HTTP-01 are ever challenged (D7).
        if ctx.path == crate::shield::VERIFY_PATH
            || ctx.path == temps_shield::assets::NEUTRAL_VERIFY_PATH
        {
            return self
                .answer_challenge_verify(
                    session,
                    ctx,
                    &project_ctx.project,
                    &project_ctx.environment,
                    parsed_ip,
                )
                .await
                .map(Flow::finished);
        }
        if self.serve_solver_asset(session, ctx).await? {
            return Ok(Flow::Answered);
        }
        // Only a challenge stored for this host skips Shield; any other
        // path under the ACME prefix is decided like the rest.
        if self.answer_stored_acme_challenge(session, ctx).await? {
            return Ok(Flow::Answered);
        }
        if self
            .enforce_shield(
                session,
                ctx,
                &project_ctx.project,
                &project_ctx.environment,
                parsed_ip,
            )
            .await?
        {
            return Ok(Flow::Answered);
        }

        Ok(Flow::Continue)
    }
}
