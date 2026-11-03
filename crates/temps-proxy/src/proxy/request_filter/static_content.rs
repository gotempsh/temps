// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Static content the proxy serves itself instead of forwarding.

use super::super::*;
use super::Flow;

impl LoadBalancer {
    /// A static deployment's files. A path route serves from its target instead: its files if the target is static, the upstream otherwise.
    pub(super) async fn static_files_stage(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
    ) -> Result<Flow> {
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
}
