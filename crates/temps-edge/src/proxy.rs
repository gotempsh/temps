//! Edge proxy — simplified Pingora `ProxyHttp` implementation.
//!
//! Handles three request types:
//! 1. **Cacheable static assets** — served from local `EdgeCache`, pull-through on miss
//! 2. **Dynamic requests** — proxied transparently to the origin
//! 3. **Unknown domains** — returns 502 Bad Gateway

use async_trait::async_trait;
use bytes::Bytes;
use flate2::write::GzEncoder;
use flate2::Compression;
use pingora::http::StatusCode;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_core::Result;
use pingora_http::ResponseHeader;
use pingora_proxy::{ProxyHttp, Session as PingoraSession};
use std::io::Write;
use std::sync::Arc;
use tracing::{debug, error, warn};

use crate::analytics::{self, EdgeAnalyticsHandle};
use crate::cache::EdgeCache;
use crate::route_table::EdgeRouteTable;

/// Per-request context for the edge proxy.
pub struct EdgeCtx {
    pub host: String,
    pub path: String,
    pub cache_status: &'static str,
    /// When the origin fetch started (for latency tracking).
    pub fetch_start: Option<std::time::Instant>,
    /// Bytes served in this response (for bandwidth tracking).
    pub bytes_served: u64,
}

/// The edge CDN proxy.
pub struct EdgeProxy {
    pub origin_url: String,
    pub origin_host: String,
    pub origin_port: u16,
    pub origin_tls: bool,
    pub token: String,
    pub route_table: Arc<EdgeRouteTable>,
    pub cache: Arc<EdgeCache>,
    pub analytics: EdgeAnalyticsHandle,
    pub region: Option<String>,
    pub origin_client: reqwest::Client,
}

impl EdgeProxy {
    pub fn new(
        origin_url: &str,
        token: &str,
        route_table: Arc<EdgeRouteTable>,
        cache: Arc<EdgeCache>,
        analytics: EdgeAnalyticsHandle,
        region: Option<String>,
    ) -> Self {
        let parsed = url::Url::parse(origin_url).unwrap_or_else(|e| {
            panic!(
                "EdgeProxy::new called with invalid origin URL '{}': {}",
                origin_url, e
            )
        });
        let origin_host = parsed.host_str().unwrap_or("localhost").to_string();
        let origin_tls = parsed.scheme() == "https";
        let origin_port = parsed.port().unwrap_or(if origin_tls { 443 } else { 80 });

        let origin_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|e| panic!("EdgeProxy::new failed to create HTTP client: {}", e));

        Self {
            origin_url: origin_url.trim_end_matches('/').to_string(),
            origin_host,
            origin_port,
            origin_tls,
            token: token.to_string(),
            route_table,
            cache,
            analytics,
            region,
            origin_client,
        }
    }

    /// Check if a URL path is a cacheable static asset.
    fn is_cacheable_static_asset(path: &str) -> bool {
        // Path patterns — any path containing these segments is cacheable
        let path_patterns = [
            "/assets/",
            "/static/",
            "/_next/static/",
            "/_next/image",
            "/_next/data/",
            ".chunk.",
            ".hash.",
        ];
        if path_patterns.iter().any(|p| path.contains(p)) {
            return true;
        }

        // Extension-based — common static file types are always cacheable
        let cacheable_extensions = [
            ".js", ".css", ".woff", ".woff2", ".ttf", ".eot", ".png", ".jpg", ".jpeg", ".gif",
            ".svg", ".ico", ".webp", ".avif", ".wasm", ".map", ".json",
        ];
        cacheable_extensions.iter().any(|ext| path.ends_with(ext))
    }

    /// Infer content type from file extension.
    fn infer_content_type(path: &str) -> &'static str {
        let ext = path.rsplit('.').next().unwrap_or("");
        match ext {
            "js" | "mjs" => "application/javascript; charset=utf-8",
            "css" => "text/css; charset=utf-8",
            "html" | "htm" => "text/html; charset=utf-8",
            "json" => "application/json; charset=utf-8",
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "svg" => "image/svg+xml",
            "ico" => "image/x-icon",
            "webp" => "image/webp",
            "avif" => "image/avif",
            "woff" => "font/woff",
            "woff2" => "font/woff2",
            "ttf" => "font/ttf",
            "eot" => "application/vnd.ms-fontobject",
            "map" => "application/json",
            "txt" => "text/plain; charset=utf-8",
            "xml" => "application/xml; charset=utf-8",
            "wasm" => "application/wasm",
            _ => "application/octet-stream",
        }
    }

    /// Generate ETag from content bytes.
    fn generate_etag(content: &[u8]) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        content.hash(&mut hasher);
        format!("W/\"{:x}\"", hasher.finish())
    }

    /// Compress data with gzip.
    fn compress_gzip(data: &[u8]) -> std::io::Result<Vec<u8>> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(data)?;
        encoder.finish()
    }

    /// Check if the client accepts gzip.
    fn accepts_gzip(session: &PingoraSession) -> bool {
        session
            .req_header()
            .headers
            .get("accept-encoding")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.contains("gzip"))
            .unwrap_or(false)
    }

    /// Should we compress this content type?
    fn should_compress(content_type: &str, size: usize) -> bool {
        if size < 256 {
            return false;
        }
        content_type.contains("javascript")
            || content_type.contains("css")
            || content_type.contains("html")
            || content_type.contains("json")
            || content_type.contains("xml")
            || content_type.contains("svg")
            || content_type.contains("text/")
    }

    /// Serve a cached asset with proper headers.
    async fn serve_cached(
        &self,
        session: &mut PingoraSession,
        data: &Bytes,
        path: &str,
        cache_status: &str,
    ) -> Result<bool> {
        let content_type = Self::infer_content_type(path);
        let etag = Self::generate_etag(data);

        // ETag conditional check
        if let Some(if_none_match) = session
            .req_header()
            .headers
            .get("if-none-match")
            .and_then(|v| v.to_str().ok())
        {
            if if_none_match == etag {
                let mut resp = ResponseHeader::build(StatusCode::NOT_MODIFIED, None)?;
                resp.insert_header("ETag", &etag)?;
                resp.insert_header("Cache-Control", "public, max-age=31536000, immutable")?;
                resp.insert_header("X-Edge-Cache", cache_status)?;
                if let Some(ref region) = self.region {
                    resp.insert_header("X-Edge-Region", region.as_str())?;
                }
                session.write_response_header(Box::new(resp), false).await?;
                session.write_response_body(None, true).await?;
                return Ok(true);
            }
        }

        // Compression
        let client_accepts_gzip = Self::accepts_gzip(session);
        let should_compress =
            client_accepts_gzip && Self::should_compress(content_type, data.len());

        let (final_content, is_compressed) = if should_compress {
            match Self::compress_gzip(data) {
                Ok(compressed) if compressed.len() < data.len() => (compressed, true),
                _ => (data.to_vec(), false),
            }
        } else {
            (data.to_vec(), false)
        };

        let mut resp = ResponseHeader::build(200, None)?;
        resp.insert_header("Content-Type", content_type)?;
        resp.insert_header("Content-Length", final_content.len().to_string())?;
        resp.insert_header("ETag", &etag)?;
        resp.insert_header("Cache-Control", "public, max-age=31536000, immutable")?;
        resp.insert_header("X-Edge-Cache", cache_status)?;
        if let Some(ref region) = self.region {
            resp.insert_header("X-Edge-Region", region.as_str())?;
        }
        if is_compressed {
            resp.insert_header("Content-Encoding", "gzip")?;
            resp.insert_header("Vary", "Accept-Encoding")?;
        }

        session.write_response_header(Box::new(resp), false).await?;
        session
            .write_response_body(Some(Bytes::from(final_content)), true)
            .await?;
        Ok(true)
    }

    /// Build the pull-through origin request for a cache miss.
    ///
    /// Cacheable asset misses are routed by the public tenant `Host` header, so
    /// the request may terminate in untrusted application code. Do not attach
    /// the edge control-plane bearer token here; that token is only for direct
    /// control-plane APIs such as registration and route sync.
    fn origin_asset_request(&self, ctx: &EdgeCtx) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.origin_url, ctx.path);

        self.origin_client
            .get(&url)
            .header("Host", &ctx.host)
            .header("X-Edge-Fetch", "true")
    }

    /// Fetch an asset from the origin and cache it.
    async fn fetch_and_cache(
        &self,
        session: &mut PingoraSession,
        ctx: &mut EdgeCtx,
    ) -> Result<bool> {
        let response = self.origin_asset_request(ctx).send().await;

        match response {
            Ok(resp) => {
                let status = resp.status();
                if !status.is_success() {
                    // Forward the error status from origin
                    let mut header = ResponseHeader::build(status.as_u16(), None)?;
                    header.insert_header("X-Edge-Cache", "MISS")?;
                    if let Some(ref region) = self.region {
                        header.insert_header("X-Edge-Region", region.as_str())?;
                    }
                    session
                        .write_response_header(Box::new(header), false)
                        .await?;
                    if let Ok(body) = resp.bytes().await {
                        session.write_response_body(Some(body), true).await?;
                    } else {
                        session.write_response_body(None, true).await?;
                    }
                    return Ok(true);
                }

                let body = resp.bytes().await.map_err(|e| {
                    pingora::Error::because(
                        pingora::ErrorType::ReadError,
                        "Failed to read origin response body",
                        e,
                    )
                })?;

                // Record analytics for cache miss
                let bytes_fetched = body.len() as u64;
                let latency_ms = ctx
                    .fetch_start
                    .map(|s| s.elapsed().as_secs_f64() * 1000.0)
                    .unwrap_or(0.0);
                self.analytics.record(analytics::make_event(
                    &ctx.host,
                    &ctx.path,
                    "GET",
                    200,
                    "MISS",
                    bytes_fetched,
                    latency_ms,
                    self.region.as_deref(),
                    Self::is_cacheable_static_asset(&ctx.path),
                ));

                // Cache the asset
                let is_immutable = Self::is_cacheable_static_asset(&ctx.path);
                if let Err(e) = self
                    .cache
                    .put(&ctx.host, &ctx.path, body.clone(), is_immutable)
                    .await
                {
                    warn!("Failed to cache {}{}: {}", ctx.host, ctx.path, e);
                }

                ctx.cache_status = "MISS";
                self.serve_cached(session, &body, &ctx.path, "MISS").await
            }
            Err(e) => {
                error!("Origin fetch failed for {}{}: {}", ctx.host, ctx.path, e);
                let mut resp = ResponseHeader::build(StatusCode::BAD_GATEWAY, None)?;
                resp.insert_header("X-Edge-Cache", "ERROR")?;
                if let Some(ref region) = self.region {
                    resp.insert_header("X-Edge-Region", region.as_str())?;
                }
                session.write_response_header(Box::new(resp), false).await?;
                session
                    .write_response_body(
                        Some(Bytes::from(
                            "<html><body><h1>502 Bad Gateway</h1><p>Edge node could not reach origin.</p></body></html>",
                        )),
                        true,
                    )
                    .await?;
                Ok(true)
            }
        }
    }
}

#[async_trait]
impl ProxyHttp for EdgeProxy {
    type CTX = EdgeCtx;

    fn new_ctx(&self) -> Self::CTX {
        EdgeCtx {
            host: String::new(),
            path: String::new(),
            cache_status: "BYPASS",
            fetch_start: None,
            bytes_served: 0,
        }
    }

    async fn early_request_filter(
        &self,
        session: &mut PingoraSession,
        ctx: &mut Self::CTX,
    ) -> Result<()>
    where
        Self::CTX: Send + Sync,
    {
        // Extract host from Host header or HTTP/2 :authority pseudo-header
        let host = session
            .req_header()
            .headers
            .get("host")
            .and_then(|v| v.to_str().ok())
            .or_else(|| session.req_header().uri.authority().map(|a| a.as_str()))
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("")
            .to_string();
        let path = session.req_header().uri.path().to_string();

        ctx.host = host;
        ctx.path = path;

        Ok(())
    }

    async fn request_filter(
        &self,
        session: &mut PingoraSession,
        ctx: &mut Self::CTX,
    ) -> Result<bool>
    where
        Self::CTX: Send + Sync,
    {
        // Check if we have a route for this domain
        if !self.route_table.contains(&ctx.host) {
            debug!("No route for domain: {}", ctx.host);
            let mut resp = ResponseHeader::build(StatusCode::BAD_GATEWAY, None)?;
            resp.insert_header("X-Edge-Cache", "NOROUTE")?;
            session.write_response_header(Box::new(resp), false).await?;
            session
                .write_response_body(
                    Some(Bytes::from(
                        "<html><body><h1>502 Bad Gateway</h1><p>Domain not configured on this edge node.</p></body></html>",
                    )),
                    true,
                )
                .await?;
            return Ok(true);
        }

        // Only cache GET requests for static assets
        let is_get = session.req_header().method == http::Method::GET;
        let is_cacheable = is_get && Self::is_cacheable_static_asset(&ctx.path);

        if !is_cacheable {
            // Dynamic request — will be proxied to origin via upstream_peer
            ctx.cache_status = "BYPASS";
            self.analytics.record(analytics::make_event(
                &ctx.host,
                &ctx.path,
                "GET",
                0,
                "BYPASS",
                0,
                0.0,
                self.region.as_deref(),
                false,
            ));
            return Ok(false);
        }

        // Try to serve from cache
        if let Some(data) = self.cache.get(&ctx.host, &ctx.path).await {
            ctx.cache_status = "HIT";
            ctx.bytes_served = data.len() as u64;
            self.analytics.record(analytics::make_event(
                &ctx.host,
                &ctx.path,
                "GET",
                200,
                "HIT",
                data.len() as u64,
                0.0,
                self.region.as_deref(),
                Self::is_cacheable_static_asset(&ctx.path),
            ));
            debug!("Cache HIT: {}{}", ctx.host, ctx.path);
            return self.serve_cached(session, &data, &ctx.path, "HIT").await;
        }

        // Cache miss — fetch from origin and cache
        ctx.fetch_start = Some(std::time::Instant::now());
        debug!("Cache MISS: {}{}", ctx.host, ctx.path);
        self.fetch_and_cache(session, ctx).await
    }

    async fn upstream_peer(
        &self,
        _session: &mut PingoraSession,
        ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        // Use IP address directly to avoid IPv6/IPv4 resolution issues.
        // Pingora may resolve "localhost" to [::1] which often isn't listening.
        let connect_host = if self.origin_host == "localhost" {
            "127.0.0.1"
        } else {
            &self.origin_host
        };

        let peer = HttpPeer::new(
            (connect_host, self.origin_port),
            self.origin_tls,
            self.origin_host.clone(),
        );
        debug!(
            "Proxying dynamic request to origin: {} -> {}:{}",
            ctx.path, connect_host, self.origin_port
        );
        Ok(Box::new(peer))
    }

    async fn upstream_request_filter(
        &self,
        session: &mut PingoraSession,
        upstream_request: &mut pingora_http::RequestHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()>
    where
        Self::CTX: Send + Sync,
    {
        // Preserve the original Host header so the origin routes correctly
        upstream_request.insert_header("Host", &ctx.host)?;

        // Add edge identification headers
        upstream_request.insert_header("X-Edge-Proxy", "true")?;
        if let Some(ref region) = self.region {
            upstream_request.insert_header("X-Edge-Region", region.as_str())?;
        }

        // Forward client IP
        if let Some(client_addr) = session.client_addr() {
            let ip = client_addr.to_string();
            upstream_request.insert_header("X-Forwarded-For", &ip)?;
            upstream_request.insert_header("X-Real-IP", &ip)?;
        }

        Ok(())
    }

    async fn response_filter(
        &self,
        _session: &mut PingoraSession,
        upstream_response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()>
    where
        Self::CTX: Send + Sync,
    {
        // Add edge headers to proxied responses
        upstream_response.insert_header("X-Edge-Cache", ctx.cache_status)?;
        if let Some(ref region) = self.region {
            upstream_response.insert_header("X-Edge-Region", region.as_str())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn test_proxy() -> EdgeProxy {
        let route_table = Arc::new(EdgeRouteTable::new());
        let unique = format!(
            "temps-edge-proxy-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let cache_dir = std::env::temp_dir().join(unique);
        let cache = Arc::new(EdgeCache::new(&cache_dir, 1024 * 1024));
        let (tx, _rx) = mpsc::channel(1);
        let analytics = EdgeAnalyticsHandle { tx };

        EdgeProxy::new(
            "https://origin.example",
            "edge-control-plane-secret",
            route_table,
            cache,
            analytics,
            None,
        )
    }

    #[test]
    fn origin_asset_request_preserves_tenant_routing_without_edge_token() {
        let proxy = test_proxy();
        let ctx = EdgeCtx {
            host: "malicious-app.example".to_string(),
            path: "/assets/steal.js".to_string(),
            cache_status: "MISS",
            fetch_start: None,
            bytes_served: 0,
        };

        let request = proxy
            .origin_asset_request(&ctx)
            .build()
            .expect("test request should build");

        assert_eq!(
            request.url().as_str(),
            "https://origin.example/assets/steal.js"
        );
        assert_eq!(
            request.headers().get("Host").and_then(|h| h.to_str().ok()),
            Some("malicious-app.example")
        );
        assert_eq!(
            request
                .headers()
                .get("X-Edge-Fetch")
                .and_then(|h| h.to_str().ok()),
            Some("true")
        );
        assert!(
            request.headers().get("Authorization").is_none(),
            "origin asset fetches must not expose the edge control-plane token to tenant-routed applications"
        );
    }
}
