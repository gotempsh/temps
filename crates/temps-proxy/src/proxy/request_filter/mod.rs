// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The order of `request_filter`.
//!
//! The order is what keeps these checks meaningful: each one protects what
//! comes after it, so moving one can open a bypass without any single check
//! changing. `order_tests` pins it: every test sets up a request that two
//! checks would both answer and asserts which one does.
//!
//! 1. Request facts, then inbound stripping of what only the proxy may set
//!    (`X-Temps-Demo-Mode`, identity headers, the status-host tag).
//! 2. Status-page hostname: answered here and nowhere else. A connection
//!    negotiated for such a hostname cannot carry any other host (421).
//! 3. Sandbox preview hostname. The public ingress hostname is never one.
//! 4. Signed-in gate (ADR 065 D8), before anything can wake or reach the app.
//! 5. Wake a sleeping host, after reserving a wake slot.
//! 6. Path route (ADR 050): match, wake a sleeping target after reserving a
//!    slot, and answer 502 when it has no target, so the host's own service
//!    never answers in its place.
//! 7. Project context: the path route's target if there is one, otherwise the
//!    host's. With a project, in order:
//!    1. connection cap;
//!    2. request policy gate (`Unavailable` 503, `Deny` 403);
//!    3. edge rules (ADR-089), except a password wall's own verify POST;
//!    4. the access decision (ADR-055 §11): IP restriction, blacklist, rate
//!       limit, ingress endpoint, Temps login. A login never blocks Shield's
//!       own paths;
//!    5. Shield's verify POST and solver files, a challenge stored for this
//!       host, then Shield itself (ADR 066);
//!    6. password wall, whose verify POST is rate limited before the password
//!       is checked.
//! 8. The solver files for a host without a project.
//! 9. ACME HTTP-01, before anything that could redirect the CA away.
//! 10. On-demand TLS's 503 on plain HTTP (ADR-018), before the HTTPS redirect.
//! 11. HTTPS redirect, then the domain's own redirect.
//! 12. Upstream request: strip client-supplied address headers, capture the
//!     headers for the request log, set the trusted forwarded headers.
//! 13. ACME and the domain redirect again, static files, persisted assets.
//! 14. Admin gate (ADR-051), last: it only decides requests about to fall
//!     through to the console.

/// Runs one stage: anything but `Continue` ends the stages around it.
macro_rules! stage {
    ($stage:expr) => {
        match $stage.await? {
            Flow::Continue => {}
            finished => return Ok(finished),
        }
    };
}

mod hostname;
mod inbound;
#[cfg(test)]
mod order_tests;

use super::*;

/// What a stage of `request_filter` decided.
#[must_use]
pub(super) enum Flow {
    /// The next stage decides.
    Continue,
    /// The proxy answered the request itself: no later stage runs and nothing
    /// goes upstream.
    Answered,
    /// Send the request upstream as it is now, without running later stages.
    Forward,
}

impl Flow {
    /// A request is finished either way: `answered` says whether by the proxy
    /// or by sending it upstream.
    fn finished(answered: bool) -> Self {
        if answered {
            Self::Answered
        } else {
            Self::Forward
        }
    }
}

impl LoadBalancer {
    /// `request_filter`, in the order the module documentation states.
    pub(super) async fn run_request_stages(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<Flow> {
        self.read_request_facts(session, ctx)?;
        Self::strip_inbound_trust_headers(session);

        stage!(self.status_page_host_stage(session, ctx));

        stage!(self.preview_stage(session, ctx));

        stage!(self.signed_in_gate_stage(session, ctx));

        // On-demand: check if this host maps to a sleeping environment.
        // Sleeping environments are excluded from the route table, so we must
        // check before project context resolution. Wake the environment inline
        // and hold the request until the container is ready and routes are reloaded.
        if let Some(ref on_demand) = self.on_demand_manager {
            let host_without_port = ctx.host.split(':').next().unwrap_or(&ctx.host);
            // An awake route always wins over a sleeping wildcard. Without
            // this check, `api.example.com` could wake an environment behind
            // `*.example.com` even when the exact host belongs to another
            // active project. Both lookups are in-memory.
            if let Some(sleeping_info) =
                should_lookup_sleeping_environment(self.route_table.as_deref(), host_without_port)
                    .then(|| on_demand.get_sleeping_environment(host_without_port))
                    .flatten()
            {
                info!(
                    environment_id = sleeping_info.environment_id,
                    host = %ctx.host,
                    "Request hit sleeping environment, waking inline"
                );

                let env_id = sleeping_info.environment_id;
                let wake_timeout = sleeping_info.wake_timeout_seconds;

                // Reserve a wake slot before parking this request. The wake path
                // can hold the request for several seconds; cap how many requests
                // may be parked here at once so an unauthenticated client that
                // knows a sleeping hostname can't pin proxy worker tasks. Held for
                // the duration of the wake + re-resolve via this guard.
                let _wake_slot = match on_demand.try_acquire_wake_slot() {
                    Some(permit) => permit,
                    None => {
                        warn!(
                            environment_id = env_id,
                            host = %ctx.host,
                            "Wake path at capacity; returning retryable 503 without parking"
                        );
                        let mut response =
                            ResponseHeader::build(StatusCode::SERVICE_UNAVAILABLE, None)?;
                        response.insert_header("Retry-After", "2")?;
                        response.insert_header("Cache-Control", "no-store")?;
                        response.insert_header("X-Request-ID", &ctx.request_id)?;
                        response.insert_header("Content-Type", "application/json")?;
                        // Body carries no environment_id: it has no authorization
                        // significance and the client keys retries off Retry-After,
                        // not the id. This response goes to an unauthenticated
                        // client (a sleeping env has no auth context yet).
                        let body_bytes = Bytes::from_static(
                            br#"{"status":"wake_pending","message":"Environment is starting, please retry"}"#,
                        );
                        session
                            .write_response_header(Box::new(response), false)
                            .await?;
                        session.write_response_body(Some(body_bytes), true).await?;
                        ctx.routing_status = "wake_throttled".to_string();
                        return Ok(Flow::Answered);
                    }
                };

                // Block until the environment is fully awake (containers healthy)
                match on_demand.wake_environment(env_id, wake_timeout).await {
                    Ok(()) => {
                        info!(
                            environment_id = env_id,
                            "Environment woke up, waiting for route reload"
                        );

                        // The woken environment was excluded from the route table
                        // while sleeping, so we must wait for the in-process
                        // reload (driven by Job::ForceRouteReload in do_wake)
                        // before the request can resolve. wait_for_route_reload
                        // is lost-wakeup-safe, but we still re-resolve in a
                        // bounded loop afterwards so a route that lands a few
                        // milliseconds late (or a missed signal) still serves THIS
                        // first request instead of falling back to the console.
                        let reload_timeout = std::time::Duration::from_secs(10);
                        let reloaded = on_demand.wait_for_route_reload(reload_timeout).await;
                        if !reloaded {
                            warn!(
                                environment_id = env_id,
                                "Route reload not observed within timeout after wake; \
                                 re-resolving route directly"
                            );
                        }

                        // Bounded re-resolve loop: poll resolve_context until the
                        // just-woken host is routable, or we exhaust the budget.
                        // We only need to CONFIRM the route is live here — the
                        // canonical resolve below does the actual context setup,
                        // attack-mode checks, and activity recording.
                        let resolve_deadline =
                            std::time::Instant::now() + std::time::Duration::from_secs(5);
                        // A context for a host without a live upstream (its
                        // containers not running yet) is not routable: keep
                        // polling rather than failing the first request.
                        let mut routable = self
                            .project_context_resolver
                            .resolve_context(&ctx.host)
                            .await
                            .is_some_and(|context| !context.upstream_unavailable);
                        while !routable && std::time::Instant::now() < resolve_deadline {
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                            routable = self
                                .project_context_resolver
                                .resolve_context(&ctx.host)
                                .await
                                .is_some_and(|context| !context.upstream_unavailable);
                        }

                        if routable {
                            info!(
                                environment_id = env_id,
                                "Route resolved after wake, serving first request"
                            );
                            // Fall through to normal request handling (the
                            // canonical resolve below now succeeds).
                        } else {
                            // Containers are awake but the route still isn't
                            // resolvable. Do NOT fall through — that would route
                            // the app's own domain to the console and serve a
                            // confusing error. Return an explicit, retryable 503
                            // so the client retries instead.
                            error!(
                                environment_id = env_id,
                                host = %ctx.host,
                                "Environment woke but route did not become resolvable; \
                                 returning retryable wake_pending"
                            );
                            let mut response =
                                ResponseHeader::build(StatusCode::SERVICE_UNAVAILABLE, None)?;
                            response.insert_header("Retry-After", "2")?;
                            response.insert_header("Cache-Control", "no-store")?;
                            response.insert_header("X-Request-ID", &ctx.request_id)?;
                            response.insert_header("Content-Type", "application/json")?;

                            // No environment_id in the body — see the wake_throttled
                            // response above. Detail stays server-side in the log line.
                            let body_bytes = Bytes::from_static(
                                br#"{"status":"wake_pending","message":"Environment is starting, please retry"}"#,
                            );

                            session
                                .write_response_header(Box::new(response), false)
                                .await?;
                            session.write_response_body(Some(body_bytes), true).await?;

                            ctx.routing_status = "wake_pending".to_string();
                            return Ok(Flow::Answered);
                        }
                    }
                    Err(e) => {
                        error!(
                            environment_id = env_id,
                            error = %e,
                            "Failed to wake environment"
                        );

                        // Wake failed — return 503 with Retry-After
                        let mut response =
                            ResponseHeader::build(StatusCode::SERVICE_UNAVAILABLE, None)?;
                        response.insert_header("Retry-After", "5")?;
                        response.insert_header("Cache-Control", "no-store")?;
                        response.insert_header("X-Request-ID", &ctx.request_id)?;
                        response.insert_header("Content-Type", "application/json")?;

                        // Static body: do not interpolate the OnDemandError Display
                        // string (it can carry container/deployment context) or the
                        // environment_id into a response served to an unauthenticated
                        // client. The detailed error is logged server-side above.
                        let body_bytes = Bytes::from_static(
                            br#"{"status":"wake_failed","message":"Failed to start environment, please retry"}"#,
                        );

                        session
                            .write_response_header(Box::new(response), false)
                            .await?;
                        session.write_response_body(Some(body_bytes), true).await?;

                        ctx.routing_status = "wake_failed".to_string();
                        return Ok(Flow::Answered);
                    }
                }
            }
        }

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

        // Path-route wake-on-demand: if the target service is sleeping
        // (scale-to-zero), wake it inline before resolving the route, using the
        // same mechanism as host-level wake-on-demand.
        if let Some(sleeping_env_id) = ctx
            .path_route
            .as_ref()
            .and_then(|r| (r.target.is_none()).then_some(r.sleeping_environment_id))
            .flatten()
        {
            if let Some(ref on_demand) = self.on_demand_manager {
                if let Some(sleeping_info) =
                    on_demand.get_sleeping_environment_by_id(sleeping_env_id)
                {
                    let wake_timeout = sleeping_info.wake_timeout_seconds;
                    info!(
                        environment_id = sleeping_env_id,
                        host = %ctx.host,
                        prefix = %ctx.path_route.as_ref().map(|r| r.prefix.as_str()).unwrap_or(""),
                        "Path-route target is sleeping, waking inline"
                    );
                    let _wake_slot = match on_demand.try_acquire_wake_slot() {
                        Some(permit) => permit,
                        None => {
                            warn!(
                                environment_id = sleeping_env_id,
                                host = %ctx.host,
                                "Wake path at capacity for path-route target; returning retryable 503"
                            );
                            let mut response =
                                ResponseHeader::build(StatusCode::SERVICE_UNAVAILABLE, None)?;
                            response.insert_header("Retry-After", "2")?;
                            response.insert_header("Cache-Control", "no-store")?;
                            response.insert_header("X-Request-ID", &ctx.request_id)?;
                            response.insert_header("Content-Type", "application/json")?;
                            let body_bytes = Bytes::from_static(
                                br#"{"status":"wake_pending","message":"Environment is starting, please retry"}"#,
                            );
                            session
                                .write_response_header(Box::new(response), false)
                                .await?;
                            session.write_response_body(Some(body_bytes), true).await?;
                            ctx.routing_status = "wake_throttled".to_string();
                            return Ok(Flow::Answered);
                        }
                    };
                    match on_demand
                        .wake_environment(sleeping_env_id, wake_timeout)
                        .await
                    {
                        Ok(()) => {
                            info!(
                                environment_id = sleeping_env_id,
                                "Path-route target woke up, waiting for route reload"
                            );
                            let reload_timeout = std::time::Duration::from_secs(10);
                            let reloaded = on_demand.wait_for_route_reload(reload_timeout).await;
                            if !reloaded {
                                warn!(
                                    environment_id = sleeping_env_id,
                                    "Route reload not observed within timeout after path-route wake"
                                );
                            }
                            // Re-resolve the path route after the target is awake.
                            let resolve_deadline =
                                std::time::Instant::now() + std::time::Duration::from_secs(5);
                            let rematch = |ctx: &ProxyContext| {
                                crate::handler::password_wall::resolve_request_path_route(
                                    &ctx.method,
                                    &ctx.path,
                                    ctx.query_string.as_deref(),
                                    |path| {
                                        self.project_context_resolver
                                            .match_path_route(&ctx.host, path)
                                    },
                                )
                            };
                            ctx.path_route = rematch(ctx);
                            while ctx.path_route.as_ref().is_none_or(|r| r.target.is_none())
                                && std::time::Instant::now() < resolve_deadline
                            {
                                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                                ctx.path_route = rematch(ctx);
                            }
                            if ctx.path_route.as_ref().is_none_or(|r| r.target.is_none()) {
                                error!(
                                    environment_id = sleeping_env_id,
                                    host = %ctx.host,
                                    "Path-route target woke but route did not resolve; returning retryable 503"
                                );
                                let mut response =
                                    ResponseHeader::build(StatusCode::SERVICE_UNAVAILABLE, None)?;
                                response.insert_header("Retry-After", "2")?;
                                response.insert_header("Cache-Control", "no-store")?;
                                response.insert_header("X-Request-ID", &ctx.request_id)?;
                                response.insert_header("Content-Type", "application/json")?;
                                let body_bytes = Bytes::from_static(
                                    br#"{"status":"wake_pending","message":"Environment is starting, please retry"}"#,
                                );
                                session
                                    .write_response_header(Box::new(response), false)
                                    .await?;
                                session.write_response_body(Some(body_bytes), true).await?;
                                ctx.routing_status = "wake_pending".to_string();
                                return Ok(Flow::Answered);
                            }
                            // Fall through: ctx.path_route now has a live target.
                        }
                        Err(e) => {
                            error!(
                                environment_id = sleeping_env_id,
                                error = %e,
                                "Failed to wake path-route target environment"
                            );
                            let mut response =
                                ResponseHeader::build(StatusCode::SERVICE_UNAVAILABLE, None)?;
                            response.insert_header("Retry-After", "5")?;
                            response.insert_header("Cache-Control", "no-store")?;
                            response.insert_header("X-Request-ID", &ctx.request_id)?;
                            response.insert_header("Content-Type", "application/json")?;
                            let body_bytes = Bytes::from_static(
                                br#"{"status":"wake_failed","message":"Failed to start environment, please retry"}"#,
                            );
                            session
                                .write_response_header(Box::new(response), false)
                                .await?;
                            session.write_response_body(Some(body_bytes), true).await?;
                            ctx.routing_status = "wake_failed".to_string();
                            return Ok(Flow::Answered);
                        }
                    }
                }
            }
        }

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
        let project_context = match path_target_context {
            Some(context) => Some(context),
            None => {
                self.project_context_resolver
                    .resolve_context(&ctx.host)
                    .await
            }
        };

        if let Some(project_ctx) = &project_context {
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
                    let mut response =
                        ResponseHeader::build(StatusCode::SERVICE_UNAVAILABLE, None)?;
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

            match access_outcome {
                crate::ingress::AccessOutcome::Allow
                | crate::ingress::AccessOutcome::PasswordWall => {}
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
                    let mut response =
                        ResponseHeader::build(StatusCode::SERVICE_UNAVAILABLE, None)?;
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

            // ADR 066 (I4): Shield, after the operator's IP gate and the
            // rate limits, so an operator deny rule or a spent rate budget
            // answers first and is not a Shield decision (D1). The verify
            // POST and the solver the page loads are answered here, whichever
            // environment this host or path resolved to: the clearance is for
            // the environment in the signed token, and a page served just
            // before Shield was turned off can still finish. Neither they nor
            // ACME HTTP-01 are ever challenged (D7).
            if ctx.path == crate::shield::VERIFY_PATH {
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
                        let html =
                            crate::handler::password_wall::generate_password_form_html_for_route(
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
                        crate::handler::password_wall::validate_cookie(
                            value,
                            env_id,
                            &password_hash,
                        )
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
        } else {
            ctx.routing_status = "no_project".to_string();
        }

        // The challenge solver, for hosts without a project (project hosts
        // were answered with Shield above).
        if self.serve_solver_asset(session, ctx).await? {
            return Ok(Flow::Answered);
        }

        // Handle ACME HTTP-01 challenges BEFORE redirects
        // This ensures domains configured as redirects can still complete certificate provisioning
        if let Some(key_authorization) = self
            .handle_acme_http_challenge(&ctx.host, &ctx.path)
            .await?
        {
            debug!(
                "Serving ACME HTTP-01 challenge response for {}{} (request_id={}) - before redirect check",
                ctx.host, ctx.path, ctx.request_id
            );

            let key_auth_bytes = Bytes::from(key_authorization.clone());
            let content_length = key_auth_bytes.len();

            let mut resp = ResponseHeader::build(200, None)?;
            resp.insert_header("Content-Type", "text/plain")?;
            resp.insert_header("Cache-Control", "no-cache")?;
            resp.insert_header("X-Request-ID", &ctx.request_id)?;
            resp.insert_header("Content-Length", content_length.to_string())?;
            resp.insert_header("Connection", "close")?;

            session.write_response_header(Box::new(resp), false).await?;
            session
                .write_response_body(Some(key_auth_bytes), true)
                .await?;

            info!(
                "ACME challenge completed (redirect domain): {} {} - 200 OK - {}ms",
                ctx.method,
                ctx.path,
                ctx.start_time.elapsed().as_millis()
            );

            ctx.routing_status = "acme_challenge".to_string();
            return Ok(Flow::Answered);
        }

        // On-demand HTTP-01 TLS UX (ADR-018 §5). Only engaged when the manager
        // is wired (on-demand TLS enabled) — otherwise zero overhead, no extra
        // settings fetch. MUST come after ACME challenge handling so a challenge
        // can always complete, and before the HTTPS redirect so a host whose
        // cert is still provisioning gets the 503 instead of a redirect to a
        // non-existent cert.
        // Gate on the cheap, in-memory checks FIRST so the common case (HTTPS
        // traffic, and HTTP hosts with no on-demand cert state) costs nothing.
        // `get_settings()` is TTL-cached (no Postgres round-trip), but the lazy
        // fetch is still kept inside `handle_on_demand_http` so it only runs for
        // the rare ephemeral `redirect_to_env` branch rather than for every
        // plain-HTTP request.
        if self.on_demand_cert_manager.is_some()
            && !self.is_tls_connection(session)
            && self.handle_on_demand_http(session, ctx).await?
        {
            return Ok(Flow::Answered);
        }

        // HTTP to HTTPS redirect for non-TLS connections.
        // This MUST come after ACME challenge handling to allow Let's Encrypt
        // HTTP-01 validation. `should_redirect_to_https` additionally exempts the
        // whole `/.well-known/acme-challenge/` prefix, so a validation request
        // that did NOT match a stored token (renewal in flight, token written to
        // a sibling domain row, wildcard parent) still falls through to normal
        // routing instead of being 301'd to a certificate that has expired or
        // does not exist yet.
        //
        // By default the redirect is per-domain: we only redirect when the
        // requesting host actually has an active TLS certificate in the database
        // (exact match or wildcard parent). This means HTTP-only installs
        // (sslip.io quick/local modes, no cert provisioned) never get redirected,
        // while hosts that have gone through SSL provisioning get automatic HTTPS
        // enforcement.
        //
        // The environment resolved above can override that default in either
        // direction (`force_https`): `Some(true)` for sites whose TLS is
        // terminated by an upstream CDN — no local cert exists, so the default
        // heuristic would leave plain HTTP serving a full 200 alongside HTTPS —
        // and `Some(false)` for environments that must stay reachable over HTTP.
        // Reading it off `ctx.environment` costs nothing extra: the environment
        // was already resolved and cloned into the context earlier in this
        // filter, so there is no additional lookup on the hot path.
        //
        // `disable_https_redirect` is a global escape hatch (set by the service
        // unit in local/testing mode) that bypasses the check entirely and
        // outranks the per-environment override.
        // WS3: cert-host check is now a lock-free ArcSwap snapshot read; the
        // background `CertHostCache::run_refresh_loop` keeps it current (±30 s).
        let env_force_https = ctx.environment.as_ref().and_then(|env| env.force_https);
        // An unresolved Host falls through to the console upstream. The console
        // has its own operator-set override rather than inheriting a project's,
        // and it is consulted only when no environment matched — `get_settings`
        // is TTL-cached, so this costs nothing on the project hot path.
        //
        // On a settings read failure this yields `None` (inherit the per-host
        // certificate heuristic), not `Some(true)`: guessing "redirect" here
        // would turn a transient database blip into a console that 301s to a
        // certificate it may not have.
        let console_force_https = if ctx.environment.is_none() {
            match self.config_service.get_settings().await {
                Ok(settings) => {
                    let request_host = ctx
                        .host
                        .split(':')
                        .next()
                        .unwrap_or(&ctx.host)
                        .trim_end_matches('.')
                        .to_ascii_lowercase();
                    if settings.console_hostname().as_deref() == Some(request_host.as_str()) {
                        settings.console_force_https
                    } else {
                        None
                    }
                }
                Err(error) => {
                    warn!(
                        error = %error,
                        "Failed to read app settings; falling back to the per-host certificate heuristic for HTTPS policy"
                    );
                    None
                }
            }
        } else {
            None
        };
        // Exactly one of these can be set: `console_force_https` is only
        // computed when no environment resolved.
        let force_https = env_force_https.or(console_force_https);
        let production_https = if should_apply_production_https_default(
            self.disable_https_redirect,
            self.is_tls_connection(session),
            &ctx.path,
            force_https,
            ctx.environment.is_some(),
        ) {
            match self.config_service.get_url_scheme().await {
                Ok(scheme) => scheme != "http",
                Err(error) => {
                    warn!(
                        error = %error,
                        "Failed to read external URL scheme; enforcing HTTPS"
                    );
                    true
                }
            }
        } else {
            false
        };
        let needs_redirect = should_redirect_to_https(
            self.disable_https_redirect,
            self.is_tls_connection(session),
            &ctx.path,
            force_https,
            // Lock-free ArcSwap snapshot read, and only reached when the
            // environment has no explicit override.
            || {
                inherited_https_policy(
                    production_https,
                    self.cert_host_cache.has_cert_for_host(&ctx.host),
                )
            },
        );
        if needs_redirect {
            // Build the HTTPS redirect URL preserving path and query string
            let redirect_url = if let Some(query) = &ctx.query_string {
                format!(
                    "https://{}{}{}",
                    ctx.host,
                    ctx.path,
                    if query.is_empty() {
                        String::new()
                    } else {
                        format!("?{}", query)
                    }
                )
            } else {
                format!("https://{}{}", ctx.host, ctx.path)
            };

            debug!(
                request_id = %ctx.request_id,
                host = %ctx.host,
                path = %ctx.path,
                redirect_url = %redirect_url,
                "Redirecting HTTP to HTTPS"
            );

            // Use 301 Permanent Redirect for HTTP→HTTPS
            let resp = https_redirect_response(&redirect_url, &ctx.request_id)?;
            // Managed monitors use this marker to distinguish the proxy's
            // pre-upstream protocol upgrade from an application's own 3xx.
            // It carries no trust decision by itself: monitors still require
            // a same-host HTTPS target and pin the follow-up to the configured
            // local TLS listener.
            ctx.routing_status = "http_to_https_redirect".to_string();

            session.write_response_header(Box::new(resp), true).await?;
            return Ok(Flow::Answered);
        }

        // Check if this host should redirect
        if let Some((redirect_target, status_code)) = self
            .project_context_resolver
            .get_redirect_info(&ctx.host)
            .await
        {
            let redirect_url =
                domain_redirect_location(&redirect_target, &ctx.path, ctx.query_string.as_deref());
            debug!(
                request_id = %ctx.request_id,
                host = %ctx.host,
                redirect_url = %redirect_url,
                status_code = status_code,
                "Redirecting request"
            );

            // Build redirect response
            let mut resp = ResponseHeader::build(status_code, None)?;
            resp.insert_header("Location", &redirect_url)?;
            resp.insert_header("Content-Length", "0")?;
            resp.insert_header("X-Temps-Proxy-Probe-Capable", "1")?;

            // Add CORS headers for redirect responses
            resp.insert_header("Access-Control-Allow-Origin", "*")?;

            // Update context for logging
            ctx.routing_status = "redirected".to_string();

            session.write_response_header(Box::new(resp), true).await?;
            return Ok(Flow::Answered); // Skip proxying
        }

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

        // Handle ACME HTTP-01 challenges
        if let Some(key_authorization) = self
            .handle_acme_http_challenge(&ctx.host, &ctx.path)
            .await?
        {
            debug!(
                "Serving ACME HTTP-01 challenge response for {}{} (request_id={})",
                ctx.host, ctx.path, ctx.request_id
            );

            let key_auth_bytes = Bytes::from(key_authorization.clone());
            let content_length = key_auth_bytes.len();

            let mut resp = ResponseHeader::build(200, None)?;
            resp.insert_header("Content-Type", "text/plain")?;
            resp.insert_header("Cache-Control", "no-cache")?;
            resp.insert_header("X-Request-ID", &ctx.request_id)?;
            resp.insert_header("Content-Length", content_length.to_string())?;
            resp.insert_header("Connection", "close")?;

            session.write_response_header(Box::new(resp), false).await?;
            session
                .write_response_body(Some(key_auth_bytes), true)
                .await?;

            // Log this ACME challenge response for debugging
            info!(
                "ACME challenge completed: {} {} - 200 OK - {}ms",
                ctx.method,
                ctx.path,
                ctx.start_time.elapsed().as_millis()
            );

            // Update routing status for potential logging
            ctx.routing_status = "acme_challenge".to_string();

            return Ok(Flow::Answered);
        }

        // Check for redirects or static file serving
        if let Some((redirect_target, status_code)) = self
            .project_context_resolver
            .get_redirect_info(&ctx.host)
            .await
        {
            let location =
                domain_redirect_location(&redirect_target, &ctx.path, ctx.query_string.as_deref());
            let mut resp = ResponseHeader::build(status_code, None)?;
            resp.insert_header(header::LOCATION, &location)?;
            session.write_response_header(Box::new(resp), true).await?;
            return Ok(Flow::Answered);
        }

        // Check if this is a static deployment using route table. A path
        // route serves from its target instead: its files if the target is
        // static, the upstream otherwise.
        let static_dir = match ctx.path_route.as_ref() {
            Some(route) => route
                .target
                .as_ref()
                .and_then(|target| target.static_dir())
                .map(str::to_string),
            None => {
                self.project_context_resolver
                    .get_static_path(&ctx.host)
                    .await
            }
        };
        if let Some(static_dir) = static_dir {
            debug!(
                "Static deployment detected for {}: {}",
                ctx.host, static_dir
            );

            // IMPORTANT: Skip static file serving for /api/_temps/* paths
            // These must ALWAYS be proxied to the console address (admin API)
            if !ctx.path.starts_with("/api/_temps/") {
                let served = self.serve_static_target(session, ctx, &static_dir).await;
                match served {
                    Ok(StaticFileServeOutcome::Served) => {
                        debug!("Served static file: {}", ctx.path);
                        ctx.routing_status = "static_file".to_string();
                        self.log_static_request(ctx, 200, "static_file", &static_dir, None, None);
                        return Ok(Flow::Answered);
                    }
                    Ok(StaticFileServeOutcome::ServedNotFoundPage) => {
                        debug!(
                            request_path = %bounded_log_value(&ctx.path),
                            stored_static_dir = %bounded_log_value(&static_dir),
                            "Static file request answered with the deployment's 404 page"
                        );
                        ctx.routing_status = "static_file_not_found_page".to_string();
                        self.log_static_request(
                            ctx,
                            404,
                            "static_file_not_found_page",
                            &static_dir,
                            Some("Static file not found".to_string()),
                            None,
                        );
                        return Ok(Flow::Answered);
                    }
                    Ok(StaticFileServeOutcome::Redirected) => {
                        debug!(
                            request_path = %bounded_log_value(&ctx.path),
                            "Static file request redirected to its slashless page"
                        );
                        ctx.routing_status = "static_file_redirect".to_string();
                        self.log_static_request(
                            ctx,
                            308,
                            "static_file_redirect",
                            &static_dir,
                            None,
                            None,
                        );
                        return Ok(Flow::Answered);
                    }
                    Ok(StaticFileServeOutcome::NotFound) => {
                        debug!(
                            request_path = %bounded_log_value(&ctx.path),
                            stored_static_dir = %bounded_log_value(&static_dir),
                            "Static file request returned the uniform not-found response"
                        );
                        let contract = static_not_found_contract(&ctx.method);
                        let mut resp = ResponseHeader::build(contract.status, None)?;
                        resp.insert_header(header::CONTENT_TYPE, contract.content_type)?;
                        resp.insert_header(
                            header::CONTENT_LENGTH,
                            contract.content_length.to_string(),
                        )?;
                        resp.insert_header(header::CACHE_CONTROL, contract.cache_control)?;
                        resp.insert_header("X-Request-ID", &ctx.request_id)?;

                        self.ensure_static_visitor_session(session, ctx, contract.content_type)
                            .await;
                        self.set_tracking_cookies(session, &mut resp, ctx).await?;
                        session.write_response_header(Box::new(resp), false).await?;
                        if contract.send_body {
                            session
                                .write_response_body(
                                    Some(Bytes::from_static(STATIC_NOT_FOUND_BODY)),
                                    true,
                                )
                                .await?;
                        } else {
                            session.write_response_body(None, true).await?;
                        }

                        self.log_static_request(
                            ctx,
                            404,
                            "static_file_not_found",
                            &static_dir,
                            Some("Static file not found".to_string()),
                            Some(contract.content_length as i64),
                        );
                        return Ok(Flow::Answered);
                    }
                    Err(error) => return Err(error),
                }
            }
            // If we reach here and path starts with /api/_temps/,
            // fall through to normal proxying logic (will be proxied to console)
        }

        // Serve persisted static assets via deployment-prefixed URLs.
        // /_temps/assets/{deployment_slug}/path → DB lookup → CAS blob
        if ctx.path.starts_with("/_temps/assets/") {
            let after_prefix = &ctx.path["/_temps/assets/".len()..];
            if let Some(slash_pos) = after_prefix.find('/') {
                let deployment_slug = &after_prefix[..slash_pos];
                let asset_path = after_prefix[slash_pos + 1..].to_string();
                let mut legacy_source = None;
                let mut legacy_current_slug = None;
                let mut asset_scope = ctx.deployment.as_ref().and_then(|deployment| {
                    let context = deployment.context_vars.as_ref();
                    let source_slug = context
                        .and_then(|value| value.get("source_deployment_slug"))
                        .and_then(serde_json::Value::as_str);
                    let source_deployment_id = context
                        .and_then(|value| value.get("source_deployment_id"))
                        .and_then(serde_json::Value::as_i64)
                        .and_then(|value| i32::try_from(value).ok());
                    if source_slug.is_none() {
                        legacy_source = source_deployment_id;
                        legacy_current_slug = Some(deployment.slug.clone());
                    }
                    deployment_asset_scope(
                        &deployment.slug,
                        deployment.environment_id,
                        deployment.id,
                        source_slug,
                        context
                            .and_then(|value| value.get("source_environment_id"))
                            .and_then(serde_json::Value::as_i64)
                            .and_then(|value| i32::try_from(value).ok()),
                        source_deployment_id,
                        deployment_slug,
                    )
                });
                if let (Some(project_id), Some(source_deployment_id)) = (
                    ctx.project.as_ref().map(|project| project.id),
                    legacy_source,
                ) {
                    if let Some(origin) = self
                        .static_asset_lookup
                        .resolve_legacy_asset_origin(project_id, source_deployment_id)
                        .await
                    {
                        asset_scope = legacy_deployment_asset_scope(
                            legacy_current_slug.as_deref().unwrap_or_default(),
                            &origin,
                            deployment_slug,
                        );
                    }
                }
                if Self::is_cacheable_static_asset(&asset_path) {
                    if let Some(asset_scope) = asset_scope {
                        if let Ok(true) = self
                            .serve_asset_from_store(session, ctx, &asset_path, Some(asset_scope))
                            .await
                        {
                            ctx.routing_status = "prefixed_asset".to_string();
                            return Ok(Flow::Answered);
                        }
                    }
                }
            }
        }

        // Fallback: serve immutable static assets from file store.
        // For container deployments where the upstream didn't have the asset,
        // check the path-keyed file store (stale-chunk fallback).
        if Self::is_cacheable_static_asset(&ctx.path)
            && ctx.path_route.is_none()
            && !self
                .project_context_resolver
                .is_static_deployment(&ctx.host)
                .await
        {
            let url_path = ctx.path.trim_start_matches('/').to_string();
            if let Ok(true) = self
                .serve_asset_from_store(session, ctx, &url_path, None)
                .await
            {
                ctx.routing_status = "stale_chunk_fallback".to_string();
                return Ok(Flow::Answered);
            }
        }

        // Admin gate: when a non-noop gate is wired and the request is
        // about to fall back to the console (no deployed app for this host,
        // not a public ingest path under /api/_temps/*, not a preview),
        // require the (IP, Host) tuple to pass the gate. If it doesn't, the
        // management surface must stay invisible from non-admin hosts.
        //
        // The public ingress hostname (ADR-051) is not gated at all: the
        // resolver pins it to the public-only listener for every client, so
        // there is no management surface behind it to protect.
        if let Some(gate) = self
            .admin_gate
            .as_ref()
            .filter(|_| !self.is_public_ingress_request(&ctx.host))
        {
            let config = gate.current();
            if Self::should_consult_admin_gate(&config, &ctx.path, ctx.preview_route.is_some()) {
                let host_has_route = self.upstream_resolver.has_route_for_host(&ctx.host).await;
                if !host_has_route {
                    let client_ip = ctx
                        .ip_address
                        .as_deref()
                        .and_then(|s| s.parse::<std::net::IpAddr>().ok())
                        .unwrap_or_else(|| std::net::IpAddr::from([127, 0, 0, 1]));
                    match Self::console_fall_through(
                        &config,
                        client_ip,
                        &ctx.host,
                        self.upstream_resolver.console_ingress_address().is_some(),
                    ) {
                        ConsoleFallThrough::Console => {}
                        // A denied client is still a legitimate caller of the
                        // public routes: a mail client fetching a tracking
                        // pixel, a git provider delivering a push. Hand it to
                        // the listener that mounts those and nothing else.
                        // That listener answers everything that is not a
                        // public route with a 404, so the console stays as
                        // invisible as when the proxy answered itself.
                        ConsoleFallThrough::PublicSurfaceOnly => {
                            debug!(
                                host = %ctx.host,
                                client_ip = %client_ip,
                                path = %ctx.path,
                                "admin gate denied request; serving the public-only surface"
                            );
                            ctx.public_surface_only = true;
                        }
                        ConsoleFallThrough::NotFound => {
                            warn!(
                                host = %ctx.host,
                                client_ip = %client_ip,
                                path = %ctx.path,
                                "admin gate denied request to non-admin host"
                            );
                            let mut response = ResponseHeader::build(StatusCode::NOT_FOUND, None)?;
                            response.insert_header("Cache-Control", "no-store")?;
                            response.insert_header("X-Request-ID", &ctx.request_id)?;
                            response.insert_header("Content-Type", "text/html; charset=utf-8")?;
                            let body =
                                Bytes::from(crate::branded_404::render(&ctx.host, &ctx.request_id));
                            response.insert_header("Content-Length", body.len().to_string())?;
                            session
                                .write_response_header(Box::new(response), false)
                                .await?;
                            session.write_response_body(Some(body), true).await?;
                            ctx.routing_status = "admin_gate_denied".to_string();
                            return Ok(Flow::Answered);
                        }
                    }
                }
            }
        }

        Ok(Flow::Continue)
    }
}
