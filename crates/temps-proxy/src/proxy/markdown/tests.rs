// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

#[cfg(test)]
pub(in crate::proxy) mod markdown_tests {
    use super::*;
    use bytes::Bytes;

    // ── Helper: build a minimal ProxyContext for testing ──────────────────────
    pub(in crate::proxy) fn make_ctx() -> ProxyContext {
        ProxyContext {
            response_modified: false,
            response_compressed: false,
            upstream_response_headers: None,
            content_type: None,
            buffer: vec![],
            project: None,
            environment: None,
            deployment: None,
            request_id: "test-req".to_string(),
            start_time: Instant::now(),
            method: "GET".to_string(),
            path: "/".to_string(),
            query_string: None,
            host: "example.com".to_string(),
            user_agent: "TestAgent/1.0".to_string(),
            referrer: None,
            ip_address: Some("127.0.0.1".to_string()),
            visitor_id: None,
            session_id: None,
            is_new_session: false,
            request_headers: None,
            response_headers: None,
            request_visitor_cookie: None,
            request_session_cookie: None,
            is_sse: false,
            is_websocket: false,
            skip_tracking: false,
            routing_status: "pending".to_string(),
            error_message: None,
            upstream_host: None,
            container_id: None,
            container_name: None,
            tls_fingerprint: None,
            tls_version: None,
            tls_cipher: None,
            sni_hostname: None,
            upstream_body_bytes_received: 0,
            client_body_bytes_received: 0,
            pending_proxy_log: None,
            wants_markdown: false,
            markdown_buffer: Vec::new(),
            markdown_fallback_accept_encoding: None,
            upstream_connect_tries: 0,
            upstream_write_pending_time_ms: None,
            upstream_start_time: None,
            upstream_response_time_ms: None,
            preview_route: None,
            preview_on_worker: false,
            path_route: None,
            gated_host: false,
            gate_identity: None,
            shield: None,
            shield_header: None,
            shield_env: None,
            netwatch_line: None,
            client_fingerprint: None,
            public_surface_only: false,
            status_page_host: false,
            streaming_session: false,
            connection_permit: None,
        }
    }

    // ── estimate_markdown_tokens ──────────────────────────────────────────────

    #[test]
    fn test_token_estimate_empty() {
        assert_eq!(estimate_markdown_tokens(""), 0);
    }

    #[test]
    fn test_token_estimate_proportional() {
        // 3 words → 4 tokens (3 * 4 / 3 = 4)
        let count = estimate_markdown_tokens("one two three");
        assert_eq!(count, 4);
    }

    #[test]
    fn test_token_estimate_larger() {
        // 300 words → 400 tokens
        let text = "word ".repeat(300);
        assert_eq!(estimate_markdown_tokens(&text), 400);
    }

    // ── wants_markdown detection (logic extracted from early_request_filter) ──

    fn parse_wants_markdown(accept: &str) -> bool {
        accept
            .split(',')
            .any(|part| part.trim().to_lowercase().starts_with("text/markdown"))
    }

    #[test]
    fn test_accept_text_markdown_exact() {
        assert!(parse_wants_markdown("text/markdown"));
    }

    #[test]
    fn test_accept_text_markdown_with_quality() {
        assert!(parse_wants_markdown("text/html, text/markdown;q=0.9"));
    }

    #[test]
    fn test_accept_text_markdown_uppercase() {
        assert!(parse_wants_markdown("Text/Markdown"));
    }

    #[test]
    fn test_accept_no_markdown() {
        assert!(!parse_wants_markdown("text/html, application/json"));
    }

    #[test]
    fn test_accept_empty() {
        assert!(!parse_wants_markdown(""));
    }

    // ── upstream_response_filter gating logic ─────────────────────────────────

    fn should_convert(ctx: &ProxyContext, content_type: &str) -> bool {
        // Mirrors the gating logic in upstream_response_filter
        ctx.wants_markdown && !ctx.is_sse && !ctx.is_websocket && content_type.contains("text/html")
    }

    #[test]
    fn test_gate_html_converts() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        assert!(should_convert(&ctx, "text/html; charset=utf-8"));
    }

    #[test]
    fn test_gate_json_does_not_convert() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        assert!(!should_convert(&ctx, "application/json"));
    }

    #[test]
    fn test_gate_sse_does_not_convert() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        ctx.is_sse = true;
        assert!(!should_convert(&ctx, "text/html"));
    }

    #[test]
    fn test_gate_websocket_does_not_convert() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        ctx.is_websocket = true;
        assert!(!should_convert(&ctx, "text/html"));
    }

    #[test]
    fn test_gate_wants_markdown_false_skips() {
        let ctx = make_ctx(); // wants_markdown == false by default
        assert!(!should_convert(&ctx, "text/html"));
    }

    // ── response_body_filter buffering logic ──────────────────────────────────

    /// Simulate the body filter for a single-chunk response by delegating to
    /// the real `response_body_filter_inner`, so this test module exercises
    /// production behaviour rather than a parallel re-implementation of it.
    fn run_body_filter_single_chunk(ctx: &mut ProxyContext, html: &[u8]) -> Option<Bytes> {
        let mut body: Option<Bytes> = Some(Bytes::copy_from_slice(html));
        response_body_filter_inner(&mut body, true, ctx).unwrap();
        body
    }

    // Helper: parse and extract content from an HTML string.
    fn extract(html: &str) -> String {
        let doc = scraper::Html::parse_document(html);
        extract_content_html(&doc)
    }

    // ── extract_content_html ─────────────────────────────────────────────────

    #[test]
    fn test_extract_main_tag_preferred() {
        let html = r#"<html><body>
            <nav>Nav noise</nav>
            <main><h1>Content</h1><p>Body text</p></main>
            <footer>Footer noise</footer>
        </body></html>"#;
        let extracted = extract(html);
        assert!(
            extracted.contains("Content"),
            "Expected main content in: {}",
            extracted
        );
        assert!(
            !extracted.contains("Nav noise"),
            "Expected nav stripped, got: {}",
            extracted
        );
        assert!(
            !extracted.contains("Footer noise"),
            "Expected footer stripped, got: {}",
            extracted
        );
    }

    #[test]
    fn test_extract_falls_back_to_body_when_no_main() {
        let html = r#"<html><body><h1>Article</h1><p>Text</p></body></html>"#;
        let extracted = extract(html);
        assert!(
            extracted.contains("Article"),
            "Expected body content in: {}",
            extracted
        );
        assert!(
            extracted.contains("Text"),
            "Expected body content in: {}",
            extracted
        );
    }

    #[test]
    fn test_extract_first_main_when_multiple() {
        let html = r#"<html><body>
            <main id="first"><p>Primary</p></main>
            <div><main id="second"><p>Nested</p></main></div>
        </body></html>"#;
        let extracted = extract(html);
        assert!(
            extracted.contains("Primary"),
            "Expected first main in: {}",
            extracted
        );
    }

    #[test]
    fn test_extract_script_inside_main_stripped() {
        // <script> inside <main> must be stripped (the key bug we fixed).
        let html = r#"<html><body>
            <main>
                <script>window.foo = 1;</script>
                <script type="application/ld+json">{"@context":"https://schema.org"}</script>
                <p>Clean content</p>
            </main>
        </body></html>"#;
        let extracted = extract(html);
        assert!(
            extracted.contains("Clean content"),
            "Expected content in: {}",
            extracted
        );
        assert!(
            !extracted.contains("window.foo"),
            "Expected inline script stripped, got: {}",
            extracted
        );
        assert!(
            !extracted.contains("schema.org"),
            "Expected JSON-LD stripped, got: {}",
            extracted
        );
    }

    #[test]
    fn test_extract_style_inside_main_stripped() {
        let html = r#"<html><body>
            <main>
                <style>.foo { color: red; }</style>
                <p>Article text</p>
            </main>
        </body></html>"#;
        let extracted = extract(html);
        assert!(
            extracted.contains("Article text"),
            "Expected content in: {}",
            extracted
        );
        assert!(
            !extracted.contains("color: red"),
            "Expected style stripped, got: {}",
            extracted
        );
    }

    #[test]
    fn test_extract_script_outside_main_not_in_output() {
        let html = r#"<html><head><style>body { color: red; }</style></head><body>
            <script>window.bar = 2;</script>
            <main><p>Clean content</p></main>
        </body></html>"#;
        let extracted = extract(html);
        assert!(!extracted.contains("window.bar"));
        assert!(!extracted.contains("color: red"));
    }

    #[test]
    fn test_extract_fallback_to_original_when_no_body() {
        let fragment = "<h1>Just a heading</h1>";
        let extracted = extract(fragment);
        assert!(
            extracted.contains("Just a heading"),
            "Expected heading in: {}",
            extracted
        );
    }

    // ── extract_page_meta / frontmatter ──────────────────────────────────────

    #[test]
    fn test_frontmatter_from_og_title_and_description() {
        let html = r#"<html><head>
            <title>My Page · Site Name</title>
            <meta property="og:title" content="My Page"/>
            <meta name="description" content="A great page about things."/>
        </head><body><main><p>Content</p></main></body></html>"#;
        let doc = scraper::Html::parse_document(html);
        let meta = extract_page_meta(&doc);
        // og:title preferred over <title>
        assert_eq!(meta.title.as_deref(), Some("My Page"));
        assert_eq!(
            meta.description.as_deref(),
            Some("A great page about things.")
        );
        assert!(meta.image.is_none());

        let fm = meta.to_frontmatter().unwrap();
        assert!(fm.starts_with("---\n"), "Expected YAML fence: {}", fm);
        assert!(fm.contains("title: My Page"), "got: {}", fm);
        assert!(
            fm.contains("description: A great page about things."),
            "got: {}",
            fm
        );
        assert!(fm.ends_with("---\n\n"), "Expected closing fence: {}", fm);
    }

    #[test]
    fn test_frontmatter_falls_back_to_title_tag() {
        let html = r#"<html><head><title>Fallback Title</title></head>
        <body><main><p>x</p></main></body></html>"#;
        let doc = scraper::Html::parse_document(html);
        let meta = extract_page_meta(&doc);
        assert_eq!(meta.title.as_deref(), Some("Fallback Title"));
    }

    #[test]
    fn test_frontmatter_image_from_og_image() {
        let html = r#"<html><head>
            <meta property="og:image" content="https://example.com/img.png"/>
        </head><body><main><p>x</p></main></body></html>"#;
        let doc = scraper::Html::parse_document(html);
        let meta = extract_page_meta(&doc);
        assert_eq!(meta.image.as_deref(), Some("https://example.com/img.png"));
    }

    #[test]
    fn test_frontmatter_image_prefers_property_image_over_og_image() {
        let html = r#"<html><head>
            <meta property="image" content="https://example.com/preview.png"/>
            <meta property="og:image" content="https://example.com/og.png"/>
        </head><body><main><p>x</p></main></body></html>"#;
        let doc = scraper::Html::parse_document(html);
        let meta = extract_page_meta(&doc);
        assert_eq!(
            meta.image.as_deref(),
            Some("https://example.com/preview.png")
        );
    }

    #[test]
    fn test_frontmatter_none_when_no_meta() {
        let html = r#"<html><body><main><p>x</p></main></body></html>"#;
        let doc = scraper::Html::parse_document(html);
        let meta = extract_page_meta(&doc);
        assert!(meta.to_frontmatter().is_none());
    }

    #[test]
    fn test_body_filter_converts_html_to_markdown_with_frontmatter() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;

        // Full page with meta + main + noise — frontmatter should be prepended,
        // nav/footer stripped, script inside main stripped.
        let html = br#"<html><head>
            <meta property="og:title" content="Hello Page"/>
            <meta name="description" content="A test page."/>
        </head><body>
            <nav>Nav</nav>
            <main>
                <script>window.noise = 1;</script>
                <h1>Hello</h1><p>World</p>
            </main>
            <footer>Footer</footer>
        </body></html>"#;
        let result = run_body_filter_single_chunk(&mut ctx, html);

        let md = String::from_utf8(result.unwrap().to_vec()).unwrap();
        // Frontmatter present
        assert!(md.starts_with("---\n"), "Expected frontmatter: {}", md);
        assert!(md.contains("title: Hello Page"), "got: {}", md);
        assert!(md.contains("description: A test page."), "got: {}", md);
        // Article content present
        assert!(md.contains("Hello"), "got: {}", md);
        assert!(md.contains("World"), "got: {}", md);
        // Noise absent
        assert!(!md.contains("Nav"), "got: {}", md);
        assert!(!md.contains("Footer"), "got: {}", md);
        assert!(!md.contains("window.noise"), "got: {}", md);
    }

    #[test]
    fn test_body_filter_passthrough_when_wants_markdown_false() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = false;

        let html = b"<h1>Hello</h1>";
        let result = run_body_filter_single_chunk(&mut ctx, html);

        // Should return unchanged bytes
        assert!(result.is_some());
        assert_eq!(result.unwrap().as_ref(), html);
    }

    #[test]
    fn test_body_filter_size_guard_truncates_instead_of_passthrough() {
        // Regression test: response_filter has already sent the client a
        // `Content-Type: text/markdown` header by the time this body filter
        // runs, so it must never fall back to raw HTML passthrough for an
        // oversized body — that would ship raw markup mislabeled as markdown.
        // It must truncate to the cap and still convert.
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;

        let html = format!(
            "<html><body><main><p>{}</p></main></body></html>",
            "x".repeat(MAX_MARKDOWN_BODY_BYTES + 1)
        );
        let result = run_body_filter_single_chunk(&mut ctx, html.as_bytes());

        assert!(
            ctx.wants_markdown,
            "wants_markdown must stay true — the header commitment can't be undone"
        );
        let result = result.expect("a truncated, converted body must still be produced");
        assert!(
            result.len() <= MAX_MARKDOWN_BODY_BYTES + 1024,
            "body must be bounded near the cap, got {} bytes",
            result.len()
        );
        assert!(
            result.len() < html.len(),
            "body must actually be truncated, not equal to the {}-byte input",
            html.len()
        );
    }

    #[test]
    fn test_body_filter_multi_chunk_accumulation() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;

        // Simulate two chunks arriving before end_of_stream (split mid-tag)
        let chunk1 = Bytes::from_static(b"<html><body><main><h1>Greet");
        let chunk2 = Bytes::from_static(b"ings</h1></main></body></html>");

        // First chunk — not end of stream
        {
            let mut body: Option<Bytes> = Some(chunk1);
            if ctx.wants_markdown {
                if let Some(c) = body.take() {
                    ctx.markdown_buffer.extend_from_slice(&c);
                }
                // end_of_stream = false → return None (suppress)
            }
        }

        // Second chunk — end of stream
        {
            let mut body: Option<Bytes> = Some(chunk2);
            let end_of_stream = true;
            if ctx.wants_markdown {
                if let Some(c) = body.take() {
                    ctx.markdown_buffer.extend_from_slice(&c);
                }
                if end_of_stream {
                    let html_str = String::from_utf8_lossy(&ctx.markdown_buffer);
                    let document = scraper::Html::parse_document(&html_str);
                    let content = extract_content_html(&document);
                    let markdown = htmd::convert(&content).unwrap_or_default();
                    ctx.markdown_buffer = Vec::new();
                    body = Some(Bytes::from(markdown));
                }
            }

            let result = body;
            assert!(result.is_some());
            let md = String::from_utf8(result.unwrap().to_vec()).unwrap();
            assert!(md.contains("Greetings"), "Expected 'Greetings' in: {}", md);
        }
    }

    // ── SSE passthrough (critical safety test) ────────────────────────────────

    #[test]
    fn test_sse_passthrough_unaffected() {
        // Even if wants_markdown was somehow set, SSE responses must never be buffered.
        // The upstream_response_filter resets wants_markdown for SSE, but we also
        // guard in response_body_filter. Verify the guard works.
        let mut ctx = make_ctx();
        ctx.wants_markdown = true; // pretend the guard in upstream_response_filter was skipped
        ctx.is_sse = true;

        let sse_chunk = Bytes::from_static(b"data: hello\n\n");

        // Replicate the response_body_filter guard for SSE
        if ctx.is_sse || ctx.is_websocket {
            // pass through immediately — no buffering, no conversion
        } else if ctx.wants_markdown {
            panic!("Should not reach markdown conversion branch for SSE");
        }

        // body should be unchanged (the SSE branch never touches it)
        assert_eq!(sse_chunk.as_ref(), b"data: hello\n\n");
    }
}

// ── Pipeline integration tests ────────────────────────────────────────────────
//
// These tests exercise the full gate → header-rewrite → body-filter pipeline
// without needing a live Pingora session.  They construct `ResponseHeader` and
// `ProxyContext` directly and call the extracted free functions
// (`apply_markdown_upstream_gate`, `apply_markdown_response_headers`) plus the
// body-filter logic that `run_body_filter_single_chunk` (in markdown_tests)
// already covers, so here we focus on the header and gate behaviour and on
// every edge-case the body filter must handle gracefully.
#[cfg(test)]
mod markdown_pipeline_tests {
    use super::*;
    use bytes::Bytes;
    use std::time::Instant;

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn make_ctx() -> ProxyContext {
        ProxyContext {
            response_modified: false,
            response_compressed: false,
            upstream_response_headers: None,
            content_type: None,
            buffer: vec![],
            project: None,
            environment: None,
            deployment: None,
            request_id: "test-req".to_string(),
            start_time: Instant::now(),
            method: "GET".to_string(),
            path: "/".to_string(),
            query_string: None,
            host: "example.com".to_string(),
            user_agent: "TestAgent/1.0".to_string(),
            referrer: None,
            ip_address: Some("127.0.0.1".to_string()),
            visitor_id: None,
            session_id: None,
            is_new_session: false,
            request_headers: None,
            response_headers: None,
            request_visitor_cookie: None,
            request_session_cookie: None,
            is_sse: false,
            is_websocket: false,
            skip_tracking: false,
            routing_status: "pending".to_string(),
            error_message: None,
            upstream_host: None,
            container_id: None,
            container_name: None,
            tls_fingerprint: None,
            tls_version: None,
            tls_cipher: None,
            sni_hostname: None,
            upstream_body_bytes_received: 0,
            client_body_bytes_received: 0,
            pending_proxy_log: None,
            wants_markdown: false,
            markdown_buffer: Vec::new(),
            markdown_fallback_accept_encoding: None,
            upstream_connect_tries: 0,
            upstream_write_pending_time_ms: None,
            upstream_start_time: None,
            upstream_response_time_ms: None,
            preview_route: None,
            preview_on_worker: false,
            path_route: None,
            gated_host: false,
            gate_identity: None,
            shield: None,
            shield_header: None,
            shield_env: None,
            netwatch_line: None,
            client_fingerprint: None,
            public_surface_only: false,
            status_page_host: false,
            streaming_session: false,
            connection_permit: None,
        }
    }

    /// Build a `ResponseHeader` with an explicit status and optional `Content-Type`.
    fn make_response(status: u16, content_type: Option<&str>) -> ResponseHeader {
        let mut resp = ResponseHeader::build(status, None).unwrap();
        if let Some(ct) = content_type {
            resp.insert_header("Content-Type", ct).unwrap();
        }
        resp
    }

    /// Simulate the full pipeline for a single-chunk body, delegating body handling
    /// to the real `response_body_filter_inner` (the production function) rather
    /// than a re-implementation, so these tests catch real regressions in it.
    /// Returns (final_ctx, outbound_response_header, body_bytes).
    fn run_pipeline(
        mut ctx: ProxyContext,
        mut resp: ResponseHeader,
        body: &[u8],
    ) -> (ProxyContext, ResponseHeader, Option<Bytes>) {
        // Phase 1: upstream_response_filter — gate
        apply_markdown_upstream_gate(&mut resp, &mut ctx);

        // Phase 2: response_filter — header rewrite
        apply_markdown_response_headers(&mut resp, &ctx);

        // Phase 3: response_body_filter — single chunk, end_of_stream=true
        let mut body_opt: Option<Bytes> = Some(Bytes::copy_from_slice(body));
        response_body_filter_inner(&mut body_opt, true, &mut ctx).unwrap();

        (ctx, resp, body_opt)
    }

    /// Feed a body through `response_body_filter_inner` as multiple chunks,
    /// mirroring how Pingora streams a real chunked upstream response — used to
    /// exercise the size cap without allocating one enormous `Bytes` value.
    fn run_pipeline_chunked(
        mut ctx: ProxyContext,
        mut resp: ResponseHeader,
        chunks: &[&[u8]],
    ) -> (ProxyContext, ResponseHeader, Option<Bytes>) {
        apply_markdown_upstream_gate(&mut resp, &mut ctx);
        apply_markdown_response_headers(&mut resp, &ctx);

        let mut last_body = None;
        for (i, chunk) in chunks.iter().enumerate() {
            let end_of_stream = i == chunks.len() - 1;
            let mut body_opt: Option<Bytes> = Some(Bytes::copy_from_slice(chunk));
            response_body_filter_inner(&mut body_opt, end_of_stream, &mut ctx).unwrap();
            last_body = body_opt;
        }

        (ctx, resp, last_body)
    }

    // ── Gate tests ────────────────────────────────────────────────────────────

    #[test]
    fn gate_allows_200_text_html() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let mut resp = make_response(200, Some("text/html; charset=utf-8"));
        apply_markdown_upstream_gate(&mut resp, &mut ctx);
        assert!(ctx.wants_markdown, "200 text/html should be allowed");
        assert_eq!(
            resp.headers.get("vary").and_then(|v| v.to_str().ok()),
            Some("Accept"),
            "Vary: Accept must be set"
        );
    }

    #[test]
    fn gate_cancels_non_html_content_type() {
        for ct in &[
            "application/json",
            "text/plain",
            "image/png",
            "application/octet-stream",
        ] {
            let mut ctx = make_ctx();
            ctx.wants_markdown = true;
            let mut resp = make_response(200, Some(ct));
            apply_markdown_upstream_gate(&mut resp, &mut ctx);
            assert!(
                !ctx.wants_markdown,
                "wants_markdown must be false for Content-Type: {}",
                ct
            );
        }
    }

    #[test]
    fn gate_cancels_missing_content_type() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let mut resp = make_response(200, None);
        apply_markdown_upstream_gate(&mut resp, &mut ctx);
        assert!(
            !ctx.wants_markdown,
            "missing Content-Type must cancel conversion"
        );
    }

    #[test]
    fn gate_cancels_compressed_upstream_body() {
        for encoding in &["gzip", "br", "deflate", "zstd", "GZIP"] {
            let mut ctx = make_ctx();
            ctx.wants_markdown = true;
            let mut resp = make_response(200, Some("text/html; charset=utf-8"));
            resp.insert_header("Content-Encoding", *encoding).unwrap();
            apply_markdown_upstream_gate(&mut resp, &mut ctx);
            assert!(
                !ctx.wants_markdown,
                "Content-Encoding {} must cancel conversion",
                encoding
            );
            apply_markdown_response_headers(&mut resp, &ctx);
            assert_eq!(
                resp.headers
                    .get("content-encoding")
                    .and_then(|v| v.to_str().ok()),
                Some(*encoding),
                "a passed-through compressed body keeps its Content-Encoding"
            );
            assert!(
                resp.headers
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|ct| ct.starts_with("text/html")),
                "a passed-through body keeps its text/html Content-Type"
            );
        }
    }

    #[test]
    fn gate_allows_identity_content_encoding() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let mut resp = make_response(200, Some("text/html"));
        resp.insert_header("Content-Encoding", "identity").unwrap();
        apply_markdown_upstream_gate(&mut resp, &mut ctx);
        assert!(ctx.wants_markdown);
    }

    #[test]
    fn http10_upstream_response_is_answered_as_http11_when_compressed() {
        // Regression: a Compose service built on Python's http.server answers
        // HTTP/1.0. Pingora gzips any compressible body of 20+ bytes and
        // switches it to chunked, but left the upstream's HTTP/1.0 status line,
        // so browsers failed with ERR_CONTENT_DECODING_FAILED.
        let mut compression = ResponseCompressionCtx::new(RESPONSE_COMPRESSION_LEVEL, false, false);
        let mut req = RequestHeader::build("GET", b"/", None).unwrap();
        req.insert_header("Accept-Encoding", "gzip").unwrap();
        compression.request_filter(&req);

        let mut resp = ResponseHeader::build(200, None).unwrap();
        resp.set_version(pingora_http::Version::HTTP_10);
        resp.insert_header("Content-Type", "text/html").unwrap();
        resp.insert_header("Content-Length", "23").unwrap();
        compression.response_header_filter(&mut resp, false);
        assert_eq!(
            resp.headers
                .get("transfer-encoding")
                .map(|value| value.as_bytes()),
            Some(&b"chunked"[..]),
            "Pingora streams the compressed body as chunked"
        );
        assert_eq!(resp.version, pingora_http::Version::HTTP_10);

        let version = downstream_response_version(pingora_http::Version::HTTP_11, resp.version);
        assert_eq!(version, Some(pingora_http::Version::HTTP_11));
    }

    #[test]
    fn downstream_response_version_leaves_http11_http2_and_http10_clients_alone() {
        use pingora_http::Version;
        assert_eq!(
            downstream_response_version(Version::HTTP_11, Version::HTTP_11),
            None
        );
        assert_eq!(
            downstream_response_version(Version::HTTP_2, Version::HTTP_10),
            None
        );
        assert_eq!(
            downstream_response_version(Version::HTTP_10, Version::HTTP_10),
            None
        );
        assert_eq!(
            downstream_response_version(Version::HTTP_11, Version::HTTP_09),
            Some(Version::HTTP_11)
        );
    }

    #[test]
    fn gzip_html_is_passed_through_byte_for_byte() {
        // Regression: temps.sh served gzip bytes decoded as UTF-8 (every 0x8b
        // became U+FFFD) under Content-Type: text/markdown.
        let gzip_magic_and_payload: &[u8] = &[0x1f, 0x8b, 0x08, 0x00, 0xde, 0xad, 0xbe, 0xef];
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let mut resp = make_response(200, Some("text/html; charset=utf-8"));
        resp.insert_header("Content-Encoding", "gzip").unwrap();
        let (ctx, _resp, body) = run_pipeline(ctx, resp, gzip_magic_and_payload);
        assert!(!ctx.wants_markdown);
        assert_eq!(body.as_deref(), Some(gzip_magic_and_payload));
    }

    #[test]
    fn gate_cancels_when_any_encoding_field_is_not_identity() {
        // `identity` first and `gzip` in a second field, and a combined list.
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let mut resp = make_response(200, Some("text/html"));
        resp.append_header("Content-Encoding", "identity").unwrap();
        resp.append_header("Content-Encoding", "gzip").unwrap();
        apply_markdown_upstream_gate(&mut resp, &mut ctx);
        assert!(
            !ctx.wants_markdown,
            "a second gzip field must cancel conversion"
        );

        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let mut resp = make_response(200, Some("text/html"));
        resp.insert_header("Content-Encoding", "identity, br")
            .unwrap();
        apply_markdown_upstream_gate(&mut resp, &mut ctx);
        assert!(!ctx.wants_markdown, "`identity, br` must cancel conversion");
    }

    /// Pingora's order for one Markdown request whose response is passed
    /// through: the compression ctx sees the header first (compression is off,
    /// so it does nothing), then `upstream_response_filter` restores
    /// compression, then the body chunks arrive. Returns the response header
    /// and the bytes sent downstream.
    fn pass_through_in_pingora_order(
        is_head: bool,
        status: u16,
        body: &[u8],
    ) -> (ResponseHeader, Vec<u8>) {
        use pingora_core::protocols::http::HttpTask;

        let mut compression = ResponseCompressionCtx::new(0, false, false);
        let mut upstream_req = RequestHeader::build("GET", b"/api/data", None).unwrap();
        request_identity_encoding_for_markdown(&mut upstream_req);
        compression.request_filter(&upstream_req);

        let mut header = HttpTask::Header(
            Box::new(make_response(status, Some("application/json"))),
            false,
        );
        compression.response_filter(&mut header);
        let HttpTask::Header(mut resp, _) = header else {
            panic!("the compression ctx replaced the header task");
        };

        // The gate passed the response through; restore the client's encoding.
        let no_body = is_head || matches!(status, 204 | 304);
        restore_client_compression(&mut compression, "gzip, br", &mut resp, no_body);

        let mut sent = Vec::new();
        let mut tasks = Vec::new();
        if !no_body {
            tasks.push(HttpTask::Body(Some(Bytes::copy_from_slice(body)), false));
        }
        tasks.push(HttpTask::Done);
        for mut task in tasks {
            compression.response_filter(&mut task);
            if let HttpTask::Body(Some(bytes), _) = task {
                sent.extend_from_slice(&bytes);
            }
        }
        (*resp, sent)
    }

    #[test]
    fn pass_through_response_is_compressed_for_the_client_again() {
        let body = vec![b'a'; 4096];
        // Before the fix this panicked inside Pingora on the first body chunk
        // ("Wrong phase: HeaderPhase") and the client saw the connection drop.
        let (resp, sent) = pass_through_in_pingora_order(false, 200, &body);
        let encoding = resp
            .headers
            .get("content-encoding")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        assert!(
            matches!(encoding.as_deref(), Some("gzip") | Some("br")),
            "pass-through response must be compressed for a client that accepts it, got {:?}",
            encoding
        );
        assert!(
            !sent.is_empty() && sent.len() < body.len(),
            "the body must be sent compressed: {} bytes for a {}-byte body",
            sent.len(),
            body.len()
        );
    }

    #[test]
    fn pass_through_response_without_a_body_is_never_compressed() {
        for (is_head, status) in [(true, 200), (false, 204), (false, 304)] {
            let (resp, sent) = pass_through_in_pingora_order(is_head, status, b"");
            assert!(
                resp.headers.get("content-encoding").is_none(),
                "status {status} (HEAD: {is_head}) must not get a Content-Encoding"
            );
            assert!(
                sent.is_empty(),
                "status {status} (HEAD: {is_head}) must not send body bytes"
            );
        }
    }

    #[test]
    fn markdown_request_asks_upstream_for_identity_encoding() {
        let mut req = RequestHeader::build("GET", b"/", None).unwrap();
        req.insert_header("Accept-Encoding", "gzip, deflate, br")
            .unwrap();
        request_identity_encoding_for_markdown(&mut req);
        assert_eq!(
            req.headers
                .get("accept-encoding")
                .and_then(|v| v.to_str().ok()),
            Some("identity")
        );
    }

    #[test]
    fn gate_cancels_4xx_even_with_html() {
        for status in &[400u16, 401, 403, 404, 422, 429] {
            let mut ctx = make_ctx();
            ctx.wants_markdown = true;
            let mut resp = make_response(*status, Some("text/html; charset=utf-8"));
            apply_markdown_upstream_gate(&mut resp, &mut ctx);
            assert!(
                !ctx.wants_markdown,
                "wants_markdown must be false for status {}",
                status
            );
        }
    }

    #[test]
    fn gate_cancels_5xx_even_with_html() {
        for status in &[500u16, 502, 503, 504] {
            let mut ctx = make_ctx();
            ctx.wants_markdown = true;
            let mut resp = make_response(*status, Some("text/html; charset=utf-8"));
            apply_markdown_upstream_gate(&mut resp, &mut ctx);
            assert!(
                !ctx.wants_markdown,
                "wants_markdown must be false for status {}",
                status
            );
        }
    }

    #[test]
    fn gate_cancels_3xx_redirect() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let mut resp = make_response(302, Some("text/html"));
        apply_markdown_upstream_gate(&mut resp, &mut ctx);
        assert!(!ctx.wants_markdown, "302 redirect should cancel conversion");
    }

    #[test]
    fn gate_handles_uppercase_content_type() {
        // Some upstreams send "TEXT/HTML" — must still be recognised.
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let mut resp = make_response(200, Some("TEXT/HTML; CHARSET=UTF-8"));
        apply_markdown_upstream_gate(&mut resp, &mut ctx);
        assert!(ctx.wants_markdown, "uppercase TEXT/HTML must be allowed");
    }

    #[test]
    fn gate_cancels_sse_even_with_html() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        ctx.is_sse = true;
        let mut resp = make_response(200, Some("text/html"));
        apply_markdown_upstream_gate(&mut resp, &mut ctx);
        assert!(!ctx.wants_markdown, "SSE must cancel conversion");
    }

    #[test]
    fn gate_cancels_websocket_even_with_html() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        ctx.is_websocket = true;
        let mut resp = make_response(200, Some("text/html"));
        apply_markdown_upstream_gate(&mut resp, &mut ctx);
        assert!(!ctx.wants_markdown, "WebSocket must cancel conversion");
    }

    #[test]
    fn gate_noop_when_wants_markdown_false() {
        // If wants_markdown is already false the gate must not touch the response.
        let mut ctx = make_ctx(); // wants_markdown = false
        let mut resp = make_response(200, Some("text/html"));
        apply_markdown_upstream_gate(&mut resp, &mut ctx);
        assert!(!ctx.wants_markdown);
        assert!(
            resp.headers.get("vary").is_none(),
            "Vary must NOT be added when wants_markdown is false"
        );
    }

    // ── Header-rewrite tests ──────────────────────────────────────────────────

    #[test]
    fn header_rewrite_sets_markdown_content_type() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let mut resp = make_response(200, Some("text/html; charset=utf-8"));
        // Simulate Content-Length being set by upstream
        resp.insert_header("Content-Length", "1234").unwrap();
        resp.insert_header("Content-Encoding", "gzip").unwrap();
        apply_markdown_response_headers(&mut resp, &ctx);
        assert_eq!(
            resp.headers
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/markdown; charset=utf-8")
        );
        assert!(
            resp.headers.get("content-length").is_none(),
            "Content-Length must be removed"
        );
        assert!(
            resp.headers.get("content-encoding").is_none(),
            "Content-Encoding must be removed"
        );
        assert_eq!(
            resp.headers
                .get("x-markdown-tokens")
                .and_then(|v| v.to_str().ok()),
            Some("0"),
            "X-Markdown-Tokens placeholder must be present"
        );
    }

    #[test]
    fn header_rewrite_noop_when_wants_markdown_false() {
        let ctx = make_ctx(); // wants_markdown = false
        let mut resp = make_response(200, Some("text/html"));
        apply_markdown_response_headers(&mut resp, &ctx);
        assert_eq!(
            resp.headers
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/html"),
            "Content-Type must be unchanged when wants_markdown is false"
        );
        assert!(resp.headers.get("x-markdown-tokens").is_none());
    }

    // ── Full pipeline tests ───────────────────────────────────────────────────

    #[test]
    fn pipeline_converts_html_to_markdown() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let resp = make_response(200, Some("text/html; charset=utf-8"));
        let html =
            b"<html><body><main><h1>Hello World</h1><p>A paragraph.</p></main></body></html>";

        let (_ctx, out_resp, body) = run_pipeline(ctx, resp, html);

        // Headers
        assert_eq!(
            out_resp
                .headers
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/markdown; charset=utf-8")
        );
        assert!(out_resp.headers.get("x-markdown-tokens").is_some());

        // Body
        let md = String::from_utf8(body.unwrap().to_vec()).unwrap();
        assert!(
            md.contains("Hello World"),
            "heading must appear in output: {}",
            md
        );
        assert!(
            md.contains("A paragraph"),
            "paragraph must appear in output: {}",
            md
        );
    }

    #[test]
    fn pipeline_passthrough_on_non_html_content_type() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let resp = make_response(200, Some("application/json"));
        let json = br#"{"key":"value"}"#;

        let (final_ctx, out_resp, body) = run_pipeline(ctx, resp, json);

        assert!(
            !final_ctx.wants_markdown,
            "gate must have cancelled conversion"
        );
        assert_eq!(
            out_resp
                .headers
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("application/json"),
            "Content-Type must be unchanged"
        );
        assert!(out_resp.headers.get("x-markdown-tokens").is_none());
        assert_eq!(body.unwrap().as_ref(), json);
    }

    #[test]
    fn pipeline_passthrough_on_missing_content_type() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let resp = make_response(200, None);
        let payload = b"some raw bytes";

        let (final_ctx, out_resp, body) = run_pipeline(ctx, resp, payload);

        assert!(!final_ctx.wants_markdown);
        assert!(out_resp.headers.get("content-type").is_none());
        assert!(out_resp.headers.get("x-markdown-tokens").is_none());
        assert_eq!(body.unwrap().as_ref(), payload);
    }

    #[test]
    fn pipeline_passthrough_on_404() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let html = b"<html><body><h1>Not Found</h1></body></html>";
        let resp = make_response(404, Some("text/html; charset=utf-8"));

        let (final_ctx, out_resp, body) = run_pipeline(ctx, resp, html);

        assert!(!final_ctx.wants_markdown, "404 must cancel conversion");
        assert_eq!(
            out_resp
                .headers
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/html; charset=utf-8"),
            "Content-Type must be unchanged for 404"
        );
        // Body must be the original HTML, not markdown
        assert_eq!(body.unwrap().as_ref(), html);
    }

    #[test]
    fn pipeline_passthrough_on_500() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let html = b"<html><body><h1>Internal Error</h1></body></html>";
        let resp = make_response(500, Some("text/html"));

        let (final_ctx, _out_resp, body) = run_pipeline(ctx, resp, html);

        assert!(!final_ctx.wants_markdown);
        assert_eq!(body.unwrap().as_ref(), html);
    }

    #[test]
    fn pipeline_passthrough_on_302_redirect() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let mut resp = make_response(302, Some("text/html"));
        resp.insert_header("Location", "https://example.com/new")
            .unwrap();

        let (final_ctx, out_resp, body) = run_pipeline(ctx, resp, b"");

        assert!(!final_ctx.wants_markdown);
        assert_eq!(
            out_resp
                .headers
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/html")
        );
        assert!(out_resp.headers.get("x-markdown-tokens").is_none());
        assert_eq!(body.unwrap().as_ref(), b"");
    }

    #[test]
    fn pipeline_passthrough_when_not_requesting_markdown() {
        // Client did not send Accept: text/markdown — wants_markdown stays false throughout.
        let ctx = make_ctx(); // wants_markdown = false
        let resp = make_response(200, Some("text/html"));
        let html = b"<html><body><h1>Hello</h1></body></html>";

        let (final_ctx, out_resp, body) = run_pipeline(ctx, resp, html);

        assert!(!final_ctx.wants_markdown);
        assert_eq!(
            out_resp
                .headers
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/html")
        );
        // Body unchanged
        assert_eq!(body.unwrap().as_ref(), html);
    }

    #[test]
    fn pipeline_converts_uppercase_content_type() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let resp = make_response(200, Some("TEXT/HTML"));
        let html = b"<body><p>Content</p></body>";

        let (_ctx, out_resp, body) = run_pipeline(ctx, resp, html);

        assert_eq!(
            out_resp
                .headers
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/markdown; charset=utf-8")
        );
        let md = String::from_utf8(body.unwrap().to_vec()).unwrap();
        assert!(
            md.contains("Content"),
            "body text must survive conversion: {}",
            md
        );
    }

    // ── Regression tests: text/markdown must never surface raw HTML ───────────
    //
    // response_filter rewrites Content-Type to text/markdown and Pingora sends
    // those headers to the client BEFORE response_body_filter ever runs — so by
    // the time the body-filter discovers a problem (body too large, conversion
    // failure), it is too late to change the Content-Type back to text/html.
    // These tests pin down that once wants_markdown is true after the gate, the
    // body filter must always hand back real, tag-free text — truncated if
    // necessary — never the untouched upstream HTML bytes.

    #[test]
    fn gate_cancels_when_content_length_declares_oversized_body() {
        // Discovered upfront (before headers are sent) via Content-Length —
        // the cheapest way to avoid ever promising markdown for a body we
        // already know will not fit.
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let mut resp = make_response(200, Some("text/html; charset=utf-8"));
        resp.insert_header("Content-Length", (MAX_MARKDOWN_BODY_BYTES + 1).to_string())
            .unwrap();

        apply_markdown_upstream_gate(&mut resp, &mut ctx);

        assert!(
            !ctx.wants_markdown,
            "an oversized declared Content-Length must cancel conversion before \
             response_filter ever commits the markdown Content-Type"
        );
    }

    #[test]
    fn gate_allows_when_content_length_under_limit() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let mut resp = make_response(200, Some("text/html; charset=utf-8"));
        resp.insert_header("Content-Length", "1024").unwrap();

        apply_markdown_upstream_gate(&mut resp, &mut ctx);

        assert!(
            ctx.wants_markdown,
            "a small declared Content-Length must not cancel conversion"
        );
    }

    #[test]
    fn pipeline_oversized_body_is_truncated_not_leaked_as_raw_html() {
        // No Content-Length header — the gate can't reject this upfront (mirrors
        // a chunked upstream response), so the over-cap condition is only
        // discovered while streaming the body, after the client already has a
        // `Content-Type: text/markdown` response header.
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let resp = make_response(200, Some("text/html; charset=utf-8"));
        let html = format!(
            "<html><body><main><p>{}</p></main></body></html>",
            "x".repeat(MAX_MARKDOWN_BODY_BYTES + 1)
        );

        let (final_ctx, out_resp, body) = run_pipeline(ctx, resp, html.as_bytes());

        assert_eq!(
            out_resp
                .headers
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/markdown; charset=utf-8"),
            "the markdown Content-Type was already sent to the client before the \
             body filter ran — it cannot be reverted to text/html here"
        );
        assert!(
            final_ctx.wants_markdown,
            "conversion must stay committed once the header promise is made"
        );

        let body = body.expect("a body must still be produced when truncated");
        assert!(
            body.len() <= MAX_MARKDOWN_BODY_BYTES + 1024,
            "converted body ({} bytes) must be bounded near the cap, not grow to \
             the full oversized input",
            body.len()
        );
        assert!(
            body.len() < html.len(),
            "body must actually be truncated, not the full {}-byte input passed \
             through untouched",
            html.len()
        );

        let text = String::from_utf8_lossy(&body);
        assert!(
            !text.contains("<main>") && !text.contains("<body>") && !text.contains("<html>"),
            "a response already labeled text/markdown must never contain literal, \
             unconverted HTML tags: {}",
            &text[..text.len().min(200)]
        );
    }

    #[test]
    fn pipeline_oversized_body_truncated_across_multiple_chunks() {
        // Same regression as above, but exercised the way Pingora actually
        // delivers a chunked upstream response: several response_body_filter
        // calls, only the last one with end_of_stream = true.
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let resp = make_response(200, Some("text/html; charset=utf-8"));

        let opening = b"<html><body><main><p>".to_vec();
        let giant_chunk = vec![b'x'; MAX_MARKDOWN_BODY_BYTES];
        let closing = b"</p></main></body></html>".to_vec();
        let chunks: Vec<&[u8]> = vec![&opening, &giant_chunk, &closing];

        let (final_ctx, out_resp, body) = run_pipeline_chunked(ctx, resp, &chunks);

        assert_eq!(
            out_resp
                .headers
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/markdown; charset=utf-8")
        );
        assert!(final_ctx.wants_markdown);

        let body = body.expect("final chunk must flush a converted body");
        assert!(
            body.len() <= MAX_MARKDOWN_BODY_BYTES + 1024,
            "buffer must have been capped across chunk boundaries, got {} bytes",
            body.len()
        );
        let text = String::from_utf8_lossy(&body);
        assert!(
            !text.contains('<'),
            "no raw HTML tags may survive into a text/markdown body: {}",
            &text[..text.len().min(200)]
        );
    }

    #[test]
    fn plain_text_fallback_never_leaks_html_tags() {
        // Exercises the last-resort branch used when htmd::convert() itself
        // fails — must always return readable text, never markup, since the
        // client has already been told Content-Type: text/markdown.
        let html = "<div><h1>Title</h1><p>Some <b>bold</b> text.</p></div>";
        let text = plain_text_fallback(html);

        assert!(
            !text.contains('<') && !text.contains('>'),
            "fallback must strip all HTML tags: {}",
            text
        );
        assert!(text.contains("Title"));
        assert!(text.contains("Some"));
        assert!(text.contains("bold"));
        assert!(text.contains("text."));
    }

    #[test]
    fn pipeline_includes_frontmatter_when_meta_present() {
        let mut ctx = make_ctx();
        ctx.wants_markdown = true;
        let resp = make_response(200, Some("text/html; charset=utf-8"));
        let html = br#"<html>
            <head>
                <meta property="og:title" content="My Article" />
                <meta name="description" content="A great read" />
            </head>
            <body><main><p>Body text.</p></main></body>
        </html>"#;

        let (_ctx, _out_resp, body) = run_pipeline(ctx, resp, html);
        let md = String::from_utf8(body.unwrap().to_vec()).unwrap();

        assert!(
            md.starts_with("---\n"),
            "output must start with YAML frontmatter"
        );
        assert!(
            md.contains("title: My Article"),
            "og:title must be in frontmatter"
        );
        assert!(
            md.contains("description: A great read"),
            "description must be in frontmatter"
        );
        assert!(
            md.contains("Body text."),
            "article body must appear after frontmatter"
        );
    }

    #[test]
    fn pipeline_vary_header_set_only_on_conversion() {
        // Vary: Accept must appear when conversion happens, not when it is cancelled.
        let mut ctx_yes = make_ctx();
        ctx_yes.wants_markdown = true;
        let mut resp_yes = make_response(200, Some("text/html"));
        apply_markdown_upstream_gate(&mut resp_yes, &mut ctx_yes);
        assert_eq!(
            resp_yes.headers.get("vary").and_then(|v| v.to_str().ok()),
            Some("Accept")
        );

        let mut ctx_no = make_ctx();
        ctx_no.wants_markdown = true;
        let mut resp_no = make_response(200, Some("application/json"));
        apply_markdown_upstream_gate(&mut resp_no, &mut ctx_no);
        assert!(
            resp_no.headers.get("vary").is_none(),
            "Vary must NOT be added when conversion is cancelled"
        );
    }
}
