// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Static content the proxy serves itself instead of forwarding.

use super::super::*;
use super::Flow;

/// Where a request's static files come from, if anywhere.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum StaticSource {
    /// Serve the files under this directory.
    Dir(String),
    /// Not a static deployment: the request goes on to its upstream.
    NotStatic,
    /// A private static route no project admitted: the project stage, which
    /// applies the IP policy and the password wall, never ran for it (the
    /// host was withdrawn by a reload, or belongs to no project). Never
    /// served.
    Unadmitted,
}

impl LoadBalancer {
    /// The directory a request's static files come from. A path route
    /// serves from its target: its files if the target is static, the
    /// upstream otherwise. On node 0's private path (ADR-115 D3) the route
    /// admitted there is the only one, since the public resolver no longer
    /// knows the host, and it is served only once the project stage admitted
    /// the request for that same project and environment.
    pub(super) async fn static_source(&self, ctx: &ProxyContext) -> StaticSource {
        if let Some(route) = ctx.path_route.as_ref() {
            return route
                .target
                .as_ref()
                .and_then(|target| target.static_dir())
                .map_or(StaticSource::NotStatic, |dir| {
                    StaticSource::Dir(dir.to_string())
                });
        }
        if let Some(route) = ctx.private_route.as_ref() {
            let Some(dir) = route.static_dir() else {
                return StaticSource::NotStatic;
            };
            // The project stage polices per environment (IP policy,
            // password wall), so the request must have been admitted for
            // this route's own environment, not a sibling the host moved to
            // between the private lookup and project resolution.
            let admitted = match (
                ctx.project.as_ref(),
                ctx.environment.as_ref(),
                route.project.as_ref(),
                route.environment.as_ref(),
            ) {
                (Some(project), Some(environment), Some(owner), Some(owner_environment)) => {
                    project.id == owner.id && environment.id == owner_environment.id
                }
                _ => false,
            };
            return if admitted {
                StaticSource::Dir(dir.to_string())
            } else {
                StaticSource::Unadmitted
            };
        }
        self.project_context_resolver
            .get_static_path(&ctx.host)
            .await
            .map_or(StaticSource::NotStatic, StaticSource::Dir)
    }

    /// A static deployment's files. A path route serves from its target instead: its files if the target is static, the upstream otherwise.
    pub(super) async fn static_files_stage(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<Flow> {
        let static_dir = match self.static_source(ctx).await {
            StaticSource::Dir(dir) => Some(dir),
            StaticSource::NotStatic => None,
            StaticSource::Unadmitted => {
                warn!(
                    host = %bounded_log_value(&ctx.host),
                    "Private static route reached the file stage without an admitted project; refused"
                );
                ctx.routing_status = "private_route_unadmitted".to_string();
                session.respond_error(404).await?;
                return Ok(Flow::Answered);
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

        Ok(Flow::Continue)
    }

    /// Persisted static assets, addressed by deployment.
    pub(super) async fn persisted_asset_stage(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<Flow> {
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

        Ok(Flow::Continue)
    }

    /// An immutable asset the upstream's current build no longer has.
    pub(super) async fn stale_chunk_stage(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<Flow> {
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

        Ok(Flow::Continue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::markdown::tests::markdown_tests::make_ctx;
    use crate::proxy::static_serving::static_path_route_redirect_tests::load_balancer;
    use std::sync::Arc;

    fn project(id: i32) -> Arc<temps_entities::projects::Model> {
        Arc::new(super::super::order_tests::project(id))
    }

    fn environment(id: i32, project_id: i32) -> Arc<temps_entities::environments::Model> {
        Arc::new(super::super::order_tests::environment(id, project_id))
    }

    /// A private static route owned by `owner` (project, environment).
    fn private_static(owner: Option<(i32, i32)>) -> temps_routes::RouteInfo {
        temps_routes::RouteInfo {
            backend: temps_routes::BackendType::StaticDir {
                path: "/srv/app".to_string(),
            },
            redirect_to: None,
            status_code: None,
            project: owner.map(|(project_id, _)| project(project_id)),
            environment: owner.map(|(project_id, id)| environment(id, project_id)),
            deployment: None,
            cert_eligible: false,
        }
    }

    /// ADR-115 D3: a static deployment on node 0's private address is served
    /// from its own route's directory, which the public resolver (it knows
    /// no such host here) cannot give, but only after the project stage
    /// admitted the request for the route's own project and environment.
    /// One admitted for a sibling environment passed that environment's IP
    /// policy and password wall, not this one's; one no project admitted
    /// (a reload withdrew the host, or it belongs to none) passed neither.
    #[tokio::test]
    async fn a_private_static_route_is_served_only_once_its_environment_admitted_it() {
        let data_dir = temp_dir::TempDir::new().expect("temp dir");
        let lb = load_balancer(data_dir.path()).expect("load balancer");
        let mut ctx = make_ctx();
        ctx.host = "app.internal.example.com".to_string();

        // Public: the resolver does not know the host.
        assert_eq!(lb.static_source(&ctx).await, StaticSource::NotStatic);

        // Private, admitted by the project stage for its own environment.
        ctx.private_route = Some(private_static(Some((7, 70))));
        ctx.project = Some(project(7));
        ctx.environment = Some(environment(70, 7));
        assert_eq!(
            lb.static_source(&ctx).await,
            StaticSource::Dir("/srv/app".to_string())
        );

        // A sibling environment of the same project, another project, no
        // admission at all, or a route without an owner: never served.
        ctx.environment = Some(environment(71, 7));
        assert_eq!(lb.static_source(&ctx).await, StaticSource::Unadmitted);
        ctx.project = Some(project(8));
        ctx.environment = Some(environment(70, 8));
        assert_eq!(lb.static_source(&ctx).await, StaticSource::Unadmitted);
        ctx.project = None;
        ctx.environment = None;
        assert_eq!(lb.static_source(&ctx).await, StaticSource::Unadmitted);
        ctx.private_route = Some(private_static(None));
        ctx.project = Some(project(7));
        ctx.environment = Some(environment(70, 7));
        assert_eq!(lb.static_source(&ctx).await, StaticSource::Unadmitted);
    }
}
