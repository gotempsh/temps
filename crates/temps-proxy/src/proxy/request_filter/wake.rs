// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Waking a sleeping environment inline, for a host or a path route target.

use super::super::*;
use super::Flow;

impl LoadBalancer {
    /// Wake the sleeping environment behind this host, holding the request
    /// until it can be served.
    pub(super) async fn wake_sleeping_host_stage(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<Flow> {
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
        Ok(Flow::Continue)
    }

    /// Wake the path route's target when it is sleeping.
    pub(super) async fn wake_path_target_stage(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<Flow> {
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
        Ok(Flow::Continue)
    }
}
