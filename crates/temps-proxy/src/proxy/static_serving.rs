// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Serving static deployments from disk or the object store, and their cache and validator policy.

use super::*;

impl LoadBalancer {
    /// Check if a request path should be logged (HTML pages only, skip static assets)
    fn should_log_static_request(path: &str) -> bool {
        path == "/" || path.ends_with(".html") || path.ends_with(".htm") || !path.contains('.')
        // SPA routes without extension
    }

    /// Create and spawn proxy log for static file serving
    pub(super) fn log_static_request(
        &self,
        ctx: &ProxyContext,
        status_code: i16,
        routing_status: &str,
        static_dir: &str,
        error_message: Option<String>,
        response_size: Option<i64>,
    ) {
        // Only log HTML pages (skip .js, .css, .svg, etc.)
        if !Self::should_log_static_request(&ctx.path) {
            return;
        }

        let (request_source, is_system_request) =
            Self::traffic_classification(&ctx.path, &ctx.user_agent);
        let proxy_log_request = CreateProxyLogRequest {
            method: ctx.method.clone(),
            path: ctx.path.clone(),
            query_string: ctx.query_string.clone(),
            host: ctx.host.clone(),
            status_code,
            response_time_ms: Some(ctx.start_time.elapsed().as_millis() as i32),
            request_source: request_source.to_string(),
            is_system_request,
            routing_status: routing_status.to_string(),
            project_id: ctx.project.as_ref().map(|p| p.id),
            environment_id: ctx.environment.as_ref().map(|e| e.id),
            deployment_id: ctx.deployment.as_ref().map(|d| d.id),
            session_id: None,
            visitor_id: None,
            visitor_uuid: ctx.visitor_id.clone(),
            session_uuid: ctx.session_id.clone(),
            container_id: None,
            upstream_host: Some(format!("static://{}", static_dir)),
            error_message,
            client_ip: ctx.ip_address.clone(),
            user_agent: Some(ctx.user_agent.clone()),
            referrer: ctx.referrer.clone(),
            request_id: ctx.request_id.clone(),
            ip_geolocation_id: None,
            browser: None,
            browser_version: None,
            operating_system: None,
            device_type: None,
            is_bot: None,
            bot_name: None,
            request_size_bytes: None,
            response_size_bytes: response_size,
            cache_status: None,
            request_headers: ctx
                .request_headers
                .as_ref()
                .and_then(|h| serde_json::to_value(h).ok()),
            response_headers: None,
            trace_id: Self::extract_traceparent_trace_id(ctx.request_headers.as_ref()),
            error_group_id: None,
            shield_verdict: ctx.shield_verdict(),
            shield_reasons: ctx.shield_reasons(),
            ja4: ctx.ja4(),
            ja4_source: ctx.ja4_source(),
        };

        // Non-blocking enqueue; shed with rate-limited accounting when full.
        self.proxy_log_handle.send_or_drop(proxy_log_request);
    }

    /// Serve the static target the request resolved to, honouring a path
    /// route.
    ///
    /// A stripping path route looks the file up without its prefix, so
    /// `ctx.path` holds the route-relative path while the file is served and
    /// the requested path again afterwards (the request log keeps it). Every
    /// absolute path the static layer writes back to the client is built from
    /// the route-relative `ctx.path` and must be re-mounted under the route
    /// prefix by the writer (see [`static_mount_prefix`]).
    pub(super) async fn serve_static_target(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
        static_dir: &str,
    ) -> Result<StaticFileServeOutcome> {
        let requested_path = ctx
            .path_route
            .as_ref()
            .filter(|route| route.strip_prefix)
            .map(|route| {
                let target_path = route.upstream_path(&ctx.path);
                std::mem::replace(&mut ctx.path, target_path)
            });
        let served = self.serve_static_file(session, ctx, static_dir).await;
        if let Some(requested_path) = requested_path {
            ctx.path = requested_path;
        }
        served
    }

    /// Serve a static file from the filesystem using fixed-size response chunks.
    ///
    /// Filesystem/path failures are deliberately returned as a uniform not-found
    /// outcome. Only response-protocol and mid-stream IO failures become Pingora
    /// errors after a response has started.
    async fn serve_static_file(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
        static_dir: &str,
    ) -> Result<StaticFileServeOutcome> {
        // `static_object_store` is only `Some` when an operator has explicitly
        // set `TEMPS_STATIC_STORAGE_BACKEND=s3` — every existing self-hosted
        // install (the field defaults to `None`) falls through to the
        // disk-based path below completely unchanged.
        if let Some(store) = self.static_object_store.clone() {
            return self
                .serve_static_file_from_store(session, ctx, static_dir, &store)
                .await;
        }

        let mut opened = match open_static_file(
            &self.config_service.static_dir(),
            static_dir,
            &ctx.path,
        )
        .await
        {
            Ok(opened) => opened,
            Err(error) => {
                debug!(
                    request_path = %bounded_log_value(&ctx.path),
                    stored_static_dir = %bounded_log_value(static_dir),
                    failure = error.category(),
                    "Static file request resolved to the uniform not-found response"
                );
                return Ok(unavailable_outcome(&error));
            }
        };

        // Resolve the actual response MIME before creating analytics state.
        // Static SPA fallbacks and extensionless paths otherwise look like
        // pages from the request path alone, including /api-style requests.
        let content_type = opened
            .canonical_path
            .to_str()
            .map(Self::infer_content_type)
            .unwrap_or("application/octet-stream");
        if opened.matched == StaticFileMatch::CanonicalRedirect {
            return self
                .write_static_canonical_redirect(session, ctx, content_type)
                .await;
        }
        self.ensure_static_visitor_session(session, ctx, content_type)
            .await;

        // Metadata + immutable deployment identity produce the validator before
        // body IO. Conditional requests therefore never read the file body.
        let etag = metadata_etag(&opened.canonical_path, &opened.metadata);
        let is_not_found_page = opened.matched == StaticFileMatch::NotFoundPage;

        // Check If-None-Match header for 304 Not Modified response. A 404 has
        // no validator, so the deployment's 404 page is always sent in full.
        if let Some(if_none_match) = session
            .req_header()
            .headers
            .get("if-none-match")
            .and_then(|v| v.to_str().ok())
            .filter(|_| !is_not_found_page)
        {
            if if_none_match_matches(if_none_match, &etag) {
                debug!("ETag match - returning 304 Not Modified for: {}", ctx.path);
                let mut resp = ResponseHeader::build(StatusCode::NOT_MODIFIED, None)?;
                resp.insert_header("ETag", &etag)?;
                resp.insert_header("X-Request-ID", &ctx.request_id)?;

                resp.insert_header(
                    header::CACHE_CONTROL,
                    Self::static_cache_control(&ctx.path, opened.matched),
                )?;

                // CRITICAL: Set tracking cookies even for 304 responses to keep sessions alive
                // Without this, visitors won't get cookies on cached root URLs (/) and events will fail
                self.set_tracking_cookies(session, &mut resp, ctx).await?;

                session.write_response_header(Box::new(resp), false).await?;
                session.write_response_body(None, true).await?;
                return Ok(StaticFileServeOutcome::Served);
            }
        }

        // Build response
        let mut resp = ResponseHeader::build(opened.matched.status(), None)?;
        resp.insert_header(header::CONTENT_TYPE, content_type)?;
        resp.insert_header(header::CONTENT_LENGTH, opened.metadata.len().to_string())?;
        resp.insert_header("X-Request-ID", &ctx.request_id)?;
        Self::insert_static_validators(&mut resp, &ctx.path, &etag, opened.matched)?;

        // Set visitor and session tracking cookies for static file responses
        self.set_tracking_cookies(session, &mut resp, ctx).await?;

        // HEAD has the same metadata as GET and intentionally never reads a body.
        let outcome = Self::static_served_outcome(opened.matched);
        session.write_response_header(Box::new(resp), false).await?;
        if ctx.method == "HEAD" {
            session.write_response_body(None, true).await?;
            return Ok(outcome);
        }

        let mut remaining = opened.metadata.len();
        while remaining > 0 {
            let mut chunk = read_static_chunk(&mut opened.file).await.map_err(|error| {
                Error::because(
                    pingora::ErrorType::FileOpenError,
                    format!(
                        "Failed to stream static file '{}' for request '{}'",
                        opened.canonical_path.display(),
                        ctx.path
                    ),
                    error,
                )
            })?;
            if chunk.is_empty() {
                return Err(Error::because(
                    pingora::ErrorType::FileOpenError,
                    format!(
                        "Static file '{}' ended before its opened length for request '{}'",
                        opened.canonical_path.display(),
                        bounded_log_value(&ctx.path)
                    ),
                    std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "static file shrank while streaming",
                    ),
                ));
            }
            remaining -= cap_static_chunk(&mut chunk, remaining);
            session.write_response_body(Some(chunk), false).await?;
        }
        session.write_response_body(None, true).await?;

        Ok(outcome)
    }

    /// `Cache-Control` for a static-site response. The deployment's 404 page
    /// gets no cache lifetime: it answers a path that may exist in the next
    /// deployment, and a hashed-asset path (`/assets/app-1a2b.js`) must never
    /// pin a 404 page as `immutable`.
    fn static_cache_control(request_path: &str, matched: StaticFileMatch) -> &'static str {
        match matched {
            StaticFileMatch::NotFoundPage => "no-store",
            // The next deployment may add `<path>/index.html`, which must win.
            StaticFileMatch::CanonicalRedirect => "public, max-age=0, must-revalidate",
            StaticFileMatch::Requested | StaticFileMatch::SpaShell => {
                if Self::is_cacheable_static_asset(request_path) {
                    "public, max-age=31536000, immutable"
                } else {
                    "public, max-age=0, must-revalidate"
                }
            }
        }
    }

    /// `ETag` and `Cache-Control` for a full static response. The 404 page has
    /// no validator, so a conditional request can never turn it into a 304.
    fn insert_static_validators(
        resp: &mut ResponseHeader,
        request_path: &str,
        etag: &str,
        matched: StaticFileMatch,
    ) -> Result<()> {
        if matched != StaticFileMatch::NotFoundPage {
            resp.insert_header("ETag", etag)?;
        }
        resp.insert_header(
            header::CACHE_CONTROL,
            Self::static_cache_control(request_path, matched),
        )?;
        Ok(())
    }

    fn static_served_outcome(matched: StaticFileMatch) -> StaticFileServeOutcome {
        match matched {
            StaticFileMatch::Requested | StaticFileMatch::SpaShell => {
                StaticFileServeOutcome::Served
            }
            StaticFileMatch::NotFoundPage => StaticFileServeOutcome::ServedNotFoundPage,
            StaticFileMatch::CanonicalRedirect => StaticFileServeOutcome::Redirected,
        }
    }

    /// Answer a trailing-slash request whose page is `<path>.html` with a 308
    /// to the slashless URL, keeping the query string.
    ///
    /// `page_content_type` is the MIME of the page being redirected to. The
    /// redirect is attributed like that page: it is often a visitor's first
    /// request, and the one that still carries the original `Referer` and UTM
    /// query, so the visitor and session are created here and their cookies
    /// set on the 308. The followed request then reuses them instead of
    /// starting a second, referrer-less visit.
    async fn write_static_canonical_redirect(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
        page_content_type: &str,
    ) -> Result<StaticFileServeOutcome> {
        // `ctx.path` is the route-relative path while a stripping path route
        // is being served; the client must be sent back under the mount.
        let mount_prefix = static_mount_prefix(ctx.path_route.as_ref());
        let Some(location) =
            canonical_redirect_location(mount_prefix, &ctx.path, ctx.query_string.as_deref())
        else {
            warn!(
                request_path = %bounded_log_value(&ctx.path),
                "Static canonical redirect rejected an unsafe location"
            );
            return Ok(StaticFileServeOutcome::NotFound);
        };
        let matched = StaticFileMatch::CanonicalRedirect;
        let mut resp = ResponseHeader::build(matched.status(), None)?;
        resp.insert_header(header::LOCATION, &location)?;
        resp.insert_header(header::CONTENT_LENGTH, "0")?;
        resp.insert_header(
            header::CACHE_CONTROL,
            Self::static_cache_control(&ctx.path, matched),
        )?;
        resp.insert_header("X-Request-ID", &ctx.request_id)?;
        self.ensure_static_visitor_session(session, ctx, page_content_type)
            .await;
        self.set_tracking_cookies(session, &mut resp, ctx).await?;
        session.write_response_header(Box::new(resp), true).await?;
        Ok(Self::static_served_outcome(matched))
    }

    /// Serve a static file from an object-store-backed deployment
    /// (`TEMPS_STATIC_STORAGE_BACKEND=s3`), through the same byte-level cache
    /// as CAS blobs so a warm request never touches the backend.
    ///
    /// Mirrors `serve_static_file`'s disk-based ETag/304/HEAD/streaming
    /// contract exactly — only key resolution differs (no filesystem
    /// canonicalization or symlink defense, since neither concept exists for
    /// an object store; path-traversal and sensitive-path protection is
    /// identical, applied by `resolve_static_object_request` before any
    /// candidate key is built). Every resolution failure — not found, or a
    /// genuine backend error/timeout — maps to the same uniform not-found
    /// response as the disk path, so the two backends are indistinguishable
    /// to a client and neither leaks backend-specific error detail.
    async fn serve_static_file_from_store(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
        static_dir: &str,
        store: &Arc<dyn temps_file_store::FileStore>,
    ) -> Result<StaticFileServeOutcome> {
        let request = match resolve_static_object_request(static_dir, &ctx.path) {
            Ok(request) => request,
            Err(error) => {
                debug!(
                    request_path = %bounded_log_value(&ctx.path),
                    stored_static_dir = %bounded_log_value(static_dir),
                    failure = error.category(),
                    "Static object-store request resolved to the uniform not-found response"
                );
                return Ok(unavailable_outcome(&error));
            }
        };

        // HEAD only ever needs `Content-Length`/`ETag`, never the body (see
        // the `ctx.method == "HEAD"` short-circuit below, which returns
        // before `opened.reader` is ever touched). Looking up size via
        // `stat_raw` instead of `open_raw` matters specifically for a
        // caching decorator: `open_raw` on a cacheable, not-yet-warm key
        // downloads and buffers the *entire* body just to answer a request
        // that sends no body at all, while `stat_raw` costs nothing extra
        // for an already-cached key and a metadata-only backend call
        // (e.g. S3 `HeadObject`) otherwise.
        let is_head = ctx.method == "HEAD";
        let mut resolved: Option<(String, StaticFileMatch, temps_file_store::OpenedBlob)> = None;
        for candidate in &request.candidates {
            let key = static_object_key(&request.relative_static_dir, &candidate.path);
            // A redirect never sends the page body, so it only needs to know
            // the key exists.
            let lookup = if is_head || candidate.matched == StaticFileMatch::CanonicalRedirect {
                store
                    .stat_raw(&key)
                    .await
                    .map(|size_bytes| temps_file_store::OpenedBlob {
                        reader: Box::new(tokio::io::empty()),
                        size_bytes,
                    })
            } else {
                store.open_raw(&key).await
            };
            match lookup {
                Ok(opened) => {
                    resolved = Some((key, candidate.matched, opened));
                    break;
                }
                Err(temps_file_store::FileStoreError::NotFound { .. }) => continue,
                Err(error) => {
                    // A real backend problem (timeout, S3 error) — trying the
                    // remaining candidates against the same struggling
                    // backend is unlikely to help, so stop here rather than
                    // pile on more latency. Still folds into the same
                    // uniform not-found response as every other resolution
                    // failure.
                    warn!(
                        key = %bounded_log_value(&key),
                        error = %error,
                        "Static object-store lookup failed"
                    );
                    break;
                }
            }
        }
        let Some((resolved_key, matched, mut opened)) = resolved else {
            debug!(
                request_path = %bounded_log_value(&ctx.path),
                stored_static_dir = %bounded_log_value(static_dir),
                "Static object-store request found no matching candidate"
            );
            return Ok(StaticFileServeOutcome::NotFound);
        };

        // Resolve the actual response MIME before creating analytics state,
        // from the resolved key (e.g. an SPA fallback's `index.html`) rather
        // than the original request path — identical to the disk-backed path.
        let content_type = Self::infer_content_type(&resolved_key);
        if matched == StaticFileMatch::CanonicalRedirect {
            return self
                .write_static_canonical_redirect(session, ctx, content_type)
                .await;
        }
        self.ensure_static_visitor_session(session, ctx, content_type)
            .await;

        let etag = object_etag(&resolved_key, opened.size_bytes);

        if let Some(if_none_match) = session
            .req_header()
            .headers
            .get("if-none-match")
            .and_then(|v| v.to_str().ok())
            .filter(|_| matched != StaticFileMatch::NotFoundPage)
        {
            if if_none_match_matches(if_none_match, &etag) {
                let mut resp = ResponseHeader::build(StatusCode::NOT_MODIFIED, None)?;
                resp.insert_header("ETag", &etag)?;
                resp.insert_header("X-Request-ID", &ctx.request_id)?;
                resp.insert_header(
                    header::CACHE_CONTROL,
                    Self::static_cache_control(&ctx.path, matched),
                )?;
                self.set_tracking_cookies(session, &mut resp, ctx).await?;
                session.write_response_header(Box::new(resp), false).await?;
                session.write_response_body(None, true).await?;
                return Ok(StaticFileServeOutcome::Served);
            }
        }

        let mut resp = ResponseHeader::build(matched.status(), None)?;
        resp.insert_header(header::CONTENT_TYPE, content_type)?;
        resp.insert_header(header::CONTENT_LENGTH, opened.size_bytes.to_string())?;
        resp.insert_header("X-Request-ID", &ctx.request_id)?;
        Self::insert_static_validators(&mut resp, &ctx.path, &etag, matched)?;
        self.set_tracking_cookies(session, &mut resp, ctx).await?;

        let outcome = Self::static_served_outcome(matched);
        session.write_response_header(Box::new(resp), false).await?;
        if ctx.method == "HEAD" {
            session.write_response_body(None, true).await?;
            return Ok(outcome);
        }

        let mut remaining = opened.size_bytes;
        while remaining > 0 {
            let mut chunk = read_static_chunk(opened.reader.as_mut())
                .await
                .map_err(|error| {
                    Error::because(
                        pingora::ErrorType::FileOpenError,
                        format!(
                            "Failed to stream static object '{}' for request '{}'",
                            bounded_log_value(&resolved_key),
                            bounded_log_value(&ctx.path)
                        ),
                        error,
                    )
                })?;
            if chunk.is_empty() {
                return Err(Error::because(
                    pingora::ErrorType::FileOpenError,
                    format!(
                        "Static object '{}' ended before its opened length for request '{}'",
                        bounded_log_value(&resolved_key),
                        bounded_log_value(&ctx.path)
                    ),
                    std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "static object shrank while streaming",
                    ),
                ));
            }
            remaining -= cap_static_chunk(&mut chunk, remaining);
            session.write_response_body(Some(chunk), false).await?;
        }
        session.write_response_body(None, true).await?;

        Ok(outcome)
    }

    /// Infer content type from file extension
    pub fn infer_content_type(file_path: &str) -> &'static str {
        let extension = std::path::Path::new(file_path)
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("");

        match extension.to_lowercase().as_str() {
            "html" => "text/html; charset=utf-8",
            "css" => "text/css; charset=utf-8",
            "js" | "mjs" | "cjs" => "application/javascript; charset=utf-8",
            "json" => "application/json; charset=utf-8",
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "svg" => "image/svg+xml",
            "webp" => "image/webp",
            "ico" => "image/x-icon",
            "woff" => "font/woff",
            "woff2" => "font/woff2",
            "ttf" => "font/ttf",
            "eot" => "application/vnd.ms-fontobject",
            "pdf" => "application/pdf",
            "txt" | "log" => "text/plain; charset=utf-8",
            "xml" => "application/xml; charset=utf-8",
            "zip" => "application/zip",
            _ => "application/octet-stream",
        }
    }

    /// Serve a static asset from CAS via the in-memory lookup cache.
    ///
    /// `static_asset_lookup` resolves the exact routed project/environment/
    /// deployment and URL path using a moka TTL cache (60 s), so the table is not
    /// queried on every cacheable-asset request. Both hits and misses are cached;
    /// the miss case (no fallback row — the common path for container deployments)
    /// is the most important one to protect. See WS4 / `static_asset_lookup.rs`.
    ///
    /// Returns `Ok(true)` if the asset was served, `Ok(false)` if not found.
    pub(super) async fn serve_asset_from_store(
        &self,
        session: &mut PingoraSession,
        ctx: &mut ProxyContext,
        url_path: &str,
        source_scope: Option<(i32, i32)>,
    ) -> Result<bool> {
        // Apply the static-file publication policy only to CAS serving. Invalid
        // container routes still fall through to their upstream unchanged.
        if normalize_static_request_path(url_path).is_err() {
            return Ok(false);
        }

        let file_store = match &self.file_store {
            Some(fs) => fs,
            None => return Ok(false),
        };

        let scope = match (&ctx.project, &ctx.environment, &ctx.deployment) {
            (Some(project), Some(environment), Some(deployment)) => {
                let (environment_id, deployment_id) =
                    source_scope.unwrap_or((environment.id, deployment.id));
                (project.id, environment_id, deployment_id)
            }
            _ => return Ok(false),
        };
        let asset = match self
            .static_asset_lookup
            .get_asset_metadata(scope.0, scope.1, scope.2, url_path)
            .await
        {
            Some(asset) => asset,
            None => return Ok(false),
        };

        let Some(etag) = bounded_cas_etag(&asset.content_hash, asset.size_bytes) else {
            warn!(
                path = %url_path,
                declared_size_bytes = asset.size_bytes,
                maximum_size_bytes = MAX_PUBLIC_STATIC_ASSET_BYTES,
                "CAS static asset has invalid or oversized metadata"
            );
            return Ok(false);
        };

        // Open first so 304/HEAD validate the actual blob size without reading
        // body bytes or trusting stale/corrupt database metadata.
        let mut opened = match file_store.open_blob(&asset.content_hash).await {
            Ok(opened) => opened,
            Err(temps_file_store::FileStoreError::NotFound { .. }) => {
                debug!(
                    hash_prefix = asset.content_hash.get(..8).unwrap_or("<invalid>"),
                    path = %bounded_log_value(url_path),
                    "CAS blob is missing"
                );
                return Ok(false);
            }
            Err(error) => {
                debug!(
                    hash_prefix = asset.content_hash.get(..8).unwrap_or("<invalid>"),
                    path = %bounded_log_value(url_path),
                    reason = %error,
                    "CAS blob open failed"
                );
                return Ok(false);
            }
        };
        if !opened_cas_size_matches(asset.size_bytes, opened.size_bytes) {
            warn!(
                path = %bounded_log_value(url_path),
                declared_size_bytes = asset.size_bytes,
                actual_size_bytes = opened.size_bytes,
                maximum_size_bytes = MAX_PUBLIC_STATIC_ASSET_BYTES,
                "CAS static asset opened size violates bounded metadata"
            );
            return Ok(false);
        }

        let content_type = Self::infer_content_type(url_path);
        if let Some(if_none_match) = session
            .req_header()
            .headers
            .get("if-none-match")
            .and_then(|value| value.to_str().ok())
        {
            if if_none_match_matches(if_none_match, &etag) {
                let mut resp = ResponseHeader::build(StatusCode::NOT_MODIFIED, None)?;
                resp.insert_header("ETag", &etag)?;
                resp.insert_header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")?;
                resp.insert_header("X-Request-ID", &ctx.request_id)?;
                self.set_tracking_cookies(session, &mut resp, ctx).await?;
                session.write_response_header(Box::new(resp), false).await?;
                session.write_response_body(None, true).await?;
                return Ok(true);
            }
        }

        // HEAD uses opened-file metadata but performs no body read.
        if ctx.method == "HEAD" {
            let mut resp = ResponseHeader::build(StatusCode::OK, None)?;
            resp.insert_header(header::CONTENT_TYPE, content_type)?;
            resp.insert_header(header::CONTENT_LENGTH, asset.size_bytes.to_string())?;
            resp.insert_header("ETag", &etag)?;
            resp.insert_header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")?;
            resp.insert_header("X-Request-ID", &ctx.request_id)?;
            self.set_tracking_cookies(session, &mut resp, ctx).await?;
            session.write_response_header(Box::new(resp), false).await?;
            session.write_response_body(None, true).await?;
            return Ok(true);
        }

        let mut resp = ResponseHeader::build(200, None)?;
        resp.insert_header(header::CONTENT_TYPE, content_type)?;
        resp.insert_header(header::CONTENT_LENGTH, opened.size_bytes.to_string())?;
        resp.insert_header("ETag", &etag)?;
        resp.insert_header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")?;
        resp.insert_header("X-Request-ID", &ctx.request_id)?;
        self.set_tracking_cookies(session, &mut resp, ctx).await?;

        session.write_response_header(Box::new(resp), false).await?;
        let mut remaining = opened.size_bytes;
        while remaining > 0 {
            let mut chunk = read_static_chunk(opened.reader.as_mut())
                .await
                .map_err(|error| {
                    Error::because(
                        pingora::ErrorType::FileOpenError,
                        format!(
                            "Failed to stream CAS static asset for request '{}'",
                            bounded_log_value(url_path)
                        ),
                        error,
                    )
                })?;
            if chunk.is_empty() {
                return Err(Error::because(
                    pingora::ErrorType::FileOpenError,
                    format!(
                        "CAS static asset ended before its opened length for request '{}'",
                        bounded_log_value(url_path)
                    ),
                    std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "CAS static asset shrank while streaming",
                    ),
                ));
            }
            remaining -= cap_static_chunk(&mut chunk, remaining);
            session.write_response_body(Some(chunk), false).await?;
        }
        session.write_response_body(None, true).await?;

        Ok(true)
    }

    /// Check if a file should have long-term caching headers
    pub fn is_cacheable_static_asset(path: &str) -> bool {
        let cacheable_patterns = [
            "/assets/",
            "/static/",
            "/_next/static/",
            ".chunk.",
            ".hash.",
        ];

        cacheable_patterns
            .iter()
            .any(|pattern| path.contains(pattern))
    }
}

#[cfg(test)]
mod static_response_policy_tests {
    use super::*;

    fn header_value<'a>(resp: &'a ResponseHeader, name: &str) -> Option<&'a str> {
        resp.headers.get(name).and_then(|value| value.to_str().ok())
    }

    #[test]
    fn not_found_page_has_no_etag_and_is_never_cached() {
        for path in ["/does-not-exist", "/assets/app-1a2b.js"] {
            let mut resp = ResponseHeader::build(StaticFileMatch::NotFoundPage.status(), None)
                .expect("build response");

            LoadBalancer::insert_static_validators(
                &mut resp,
                path,
                "W/\"etag\"",
                StaticFileMatch::NotFoundPage,
            )
            .expect("insert validators");

            assert_eq!(resp.status.as_u16(), 404, "{path}");
            assert_eq!(header_value(&resp, "etag"), None, "{path}");
            assert_eq!(
                header_value(&resp, "cache-control"),
                Some("no-store"),
                "{path}"
            );
        }
    }

    #[test]
    fn served_files_keep_their_etag_and_path_based_caching() {
        for (path, matched, cache_control) in [
            (
                "/assets/app.js",
                StaticFileMatch::Requested,
                "public, max-age=31536000, immutable",
            ),
            (
                "/about",
                StaticFileMatch::Requested,
                "public, max-age=0, must-revalidate",
            ),
            (
                "/dashboard",
                StaticFileMatch::SpaShell,
                "public, max-age=0, must-revalidate",
            ),
        ] {
            let mut resp = ResponseHeader::build(matched.status(), None).expect("build response");

            LoadBalancer::insert_static_validators(&mut resp, path, "W/\"etag\"", matched)
                .expect("insert validators");

            assert_eq!(resp.status.as_u16(), 200, "{path}");
            assert_eq!(header_value(&resp, "etag"), Some("W/\"etag\""), "{path}");
            assert_eq!(
                header_value(&resp, "cache-control"),
                Some(cache_control),
                "{path}"
            );
        }
    }

    #[test]
    fn only_the_not_found_page_reports_a_not_found_outcome() {
        assert_eq!(
            LoadBalancer::static_served_outcome(StaticFileMatch::Requested),
            StaticFileServeOutcome::Served
        );
        assert_eq!(
            LoadBalancer::static_served_outcome(StaticFileMatch::SpaShell),
            StaticFileServeOutcome::Served
        );
        assert_eq!(
            LoadBalancer::static_served_outcome(StaticFileMatch::NotFoundPage),
            StaticFileServeOutcome::ServedNotFoundPage
        );
        assert_eq!(
            LoadBalancer::static_served_outcome(StaticFileMatch::CanonicalRedirect),
            StaticFileServeOutcome::Redirected
        );
    }

    #[test]
    fn canonical_redirect_is_tracked_like_the_page_it_redirects_to() {
        // The 308 for `/about/` is attributed with the MIME of `about.html`,
        // so a browser's first navigation creates the visit on the redirect.
        let page_content_type = LoadBalancer::infer_content_type("about.html");
        let browser_navigation = LoadBalancer::should_track_page(
            "/about/",
            Some(page_content_type),
            "GET",
            Some("text/html,application/xhtml+xml"),
            Some("document"),
            Some("1"),
        );
        assert!(browser_navigation);

        let plain_http_client = LoadBalancer::should_track_page(
            "/about/",
            Some(page_content_type),
            "GET",
            Some("*/*"),
            None,
            None,
        );
        assert!(!plain_http_client);
    }

    #[test]
    fn canonical_redirect_is_revalidated_even_on_an_asset_like_path() {
        assert_eq!(
            LoadBalancer::static_cache_control(
                "/assets/guide/",
                StaticFileMatch::CanonicalRedirect
            ),
            "public, max-age=0, must-revalidate"
        );
    }
}

/// A path route mounts a static target under a prefix (ADR 050). The static
/// layer resolves files against the route-relative path, so the redirects it
/// writes must be re-mounted under the prefix or the client leaves the route.
#[cfg(test)]
mod static_path_route_redirect_tests {
    use super::{LoadBalancer, StaticFileServeOutcome};
    use crate::config::ProxyConfig;
    use crate::services::{ProjectContextResolverImpl, UpstreamResolverImpl};
    use crate::traits::{ProjectContextResolver, UpstreamResolver};
    use anyhow::Result;
    use pingora_proxy::{ProxyHttp, Session};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::sync::Arc;
    use temp_dir::TempDir;
    use temps_routes::{CachedPeerTable, PathRoute};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const STORED_DIR: &str = "projects/site/production/deploy-1";

    /// Load balancer over an in-memory database: serving a redirect never
    /// queries it, so these tests need no Docker.
    fn load_balancer(data_dir: &std::path::Path) -> Result<LoadBalancer> {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let mut config = temps_config::ServerConfig::new(
            "127.0.0.1:3000".to_string(),
            "postgresql://test@localhost/test".to_string(),
            None,
            None,
        )?;
        config.data_dir = data_dir.to_path_buf();
        let config_service = Arc::new(temps_config::ConfigService::new(
            Arc::new(config),
            db.clone(),
        ));

        std::env::set_var("TEMPS_GEO_MOCK", "true");
        let geoip_service = Arc::new(temps_geo::GeoIpService::new()?);
        let ip_service = Arc::new(temps_geo::IpAddressService::new(db.clone(), geoip_service));
        let proxy_log_storage: Arc<dyn crate::storage::ProxyLogStorage> = Arc::new(
            crate::storage::TimescaleDbProxyLogStore::new(db.clone(), ip_service.clone()),
        );
        let (proxy_log_handle, tracking_handle, _proxy_log_writer) =
            crate::service::proxy_log_batch_writer::ProxyLogBatchWriter::new(
                db.clone(),
                ip_service,
                proxy_log_storage,
            );

        let route_table = Arc::new(CachedPeerTable::new(db.clone()));
        let upstream_resolver = Arc::new(UpstreamResolverImpl::new(
            Arc::new(ProxyConfig::default()),
            Arc::new(crate::service::lb_service::LbService::new(db.clone())),
            route_table.clone(),
        )) as Arc<dyn UpstreamResolver>;
        let project_context_resolver = Arc::new(ProjectContextResolverImpl::new(route_table))
            as Arc<dyn ProjectContextResolver>;

        Ok(LoadBalancer::new(
            upstream_resolver,
            proxy_log_handle,
            tracking_handle,
            project_context_resolver,
            Arc::new(temps_core::CookieCrypto::new(
                "default-32-byte-key-for-testing!",
            )?),
            db.clone(),
            config_service,
            Arc::new(
                crate::service::ip_access_control_service::IpAccessControlService::new(db.clone()),
            ),
            Arc::new(temps_core::OpenIpGate),
            Arc::new(crate::shield::test_gate()),
            Arc::new(crate::service::cert_host_cache::CertHostCache::new(
                db.clone(),
            )),
            false,
        ))
    }

    /// `<data_dir>/static/<STORED_DIR>` holding a root page, `guide.html` and
    /// `docs/other.html` (a deployment that itself ships a `docs/` folder).
    fn write_deployment(data_dir: &std::path::Path) -> Result<()> {
        let deployment = data_dir.join("static").join(STORED_DIR);
        std::fs::create_dir_all(deployment.join("docs"))?;
        std::fs::write(deployment.join("index.html"), b"root")?;
        std::fs::write(deployment.join("guide.html"), b"guide")?;
        std::fs::write(deployment.join("docs/other.html"), b"other")?;
        Ok(())
    }

    fn route(prefix: &str, strip_prefix: bool) -> PathRoute {
        PathRoute {
            prefix: prefix.to_string(),
            strip_prefix,
            target_service: "docs".to_string(),
            target: None,
            sleeping_environment_id: None,
        }
    }

    /// Serve `path_and_query` through the same entry point `request_filter`
    /// uses for a static target and return the raw response head.
    async fn serve(
        lb: &LoadBalancer,
        path_route: Option<PathRoute>,
        path_and_query: &str,
    ) -> Result<(StaticFileServeOutcome, String, String)> {
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        client
            .write_all(
                format!("GET {path_and_query} HTTP/1.1\r\nHost: site.example.com\r\n\r\n")
                    .as_bytes(),
            )
            .await?;
        let mut session = Session::new_h1(Box::new(server) as pingora_core::protocols::Stream);
        assert!(session.read_request().await?, "request must parse");

        let mut ctx = lb.new_ctx();
        let (path, query) = match path_and_query.split_once('?') {
            Some((path, query)) => (path, Some(query.to_string())),
            None => (path_and_query, None),
        };
        ctx.method = "GET".to_string();
        ctx.host = "site.example.com".to_string();
        ctx.path = path.to_string();
        ctx.query_string = query;
        ctx.path_route = path_route;

        let outcome = lb
            .serve_static_target(&mut session, &mut ctx, STORED_DIR)
            .await?;
        assert_eq!(
            ctx.path, path,
            "the requested path is restored after serving"
        );
        drop(session);

        let mut raw = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.read_to_end(&mut raw),
        )
        .await??;
        let response = String::from_utf8_lossy(&raw).to_string();
        let head = response
            .split_once("\r\n\r\n")
            .map(|(head, _)| head.to_string())
            .unwrap_or(response.clone());
        let location = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("location")
                    .then(|| value.trim().to_string())
            })
            .unwrap_or_default();
        Ok((outcome, head, location))
    }

    #[tokio::test]
    async fn stripped_route_redirect_keeps_the_mount_prefix_and_query() -> Result<()> {
        let data_dir = TempDir::new()?;
        write_deployment(data_dir.path())?;
        let lb = load_balancer(data_dir.path())?;

        let (outcome, head, location) =
            serve(&lb, Some(route("/docs", true)), "/docs/guide/?ref=home&x=1").await?;
        assert_eq!(outcome, StaticFileServeOutcome::Redirected, "{head}");
        assert!(head.starts_with("HTTP/1.1 308"), "{head}");
        assert_eq!(location, "/docs/guide?ref=home&x=1");

        let (_, head, location) = serve(&lb, Some(route("/docs", true)), "/docs/guide/").await?;
        assert!(head.starts_with("HTTP/1.1 308"), "{head}");
        assert_eq!(location, "/docs/guide");
        Ok(())
    }

    #[tokio::test]
    async fn stripped_route_root_is_served_not_redirected() -> Result<()> {
        let data_dir = TempDir::new()?;
        write_deployment(data_dir.path())?;
        let lb = load_balancer(data_dir.path())?;

        for requested in ["/docs", "/docs/"] {
            let (outcome, head, location) =
                serve(&lb, Some(route("/docs", true)), requested).await?;
            assert_eq!(
                outcome,
                StaticFileServeOutcome::Served,
                "{requested}: {head}"
            );
            assert!(head.starts_with("HTTP/1.1 200"), "{requested}: {head}");
            assert_eq!(location, "", "{requested}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn redirect_is_unchanged_without_a_stripping_route() -> Result<()> {
        let data_dir = TempDir::new()?;
        write_deployment(data_dir.path())?;
        let lb = load_balancer(data_dir.path())?;

        // Not path-routed.
        let (outcome, head, location) = serve(&lb, None, "/guide/?ref=home").await?;
        assert_eq!(outcome, StaticFileServeOutcome::Redirected, "{head}");
        assert_eq!(location, "/guide?ref=home");

        // A route that keeps its prefix: the lookup path already carries it.
        let (outcome, head, location) =
            serve(&lb, Some(route("/docs", false)), "/docs/other/").await?;
        assert_eq!(outcome, StaticFileServeOutcome::Redirected, "{head}");
        assert_eq!(location, "/docs/other");
        Ok(())
    }
}
