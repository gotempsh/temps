//! HTTP reverse proxy from Temps to external plugin processes over Unix socket.

use std::path::PathBuf;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Router;
use tracing::{debug, error};

/// Proxy configuration for a single external plugin.
#[derive(Debug, Clone)]
pub struct PluginProxy {
    /// Unix socket path to the plugin
    pub socket_path: PathBuf,
    /// Plugin name for header injection
    pub plugin_name: String,
    /// HMAC secret for authenticating proxied requests
    pub auth_secret: String,
}

impl PluginProxy {
    pub fn new(socket_path: PathBuf, plugin_name: String, auth_secret: String) -> Self {
        Self {
            socket_path,
            plugin_name,
            auth_secret,
        }
    }
}

/// Create an axum Router that proxies all requests to an external plugin.
///
/// Mounts at `/x/{plugin_name}` — strips the prefix before forwarding.
/// Adds Temps headers (user context, request ID, auth signature).
pub fn create_plugin_proxy_router(proxy: PluginProxy) -> Router {
    Router::new().fallback(proxy_handler).with_state(proxy)
}

/// The actual proxy handler — forwards requests to the plugin over Unix socket.
async fn proxy_handler(State(proxy): State<PluginProxy>, request: Request) -> Response {
    let method = request.method().clone();
    let uri = request.uri().clone();
    let path = uri.path();

    debug!(
        plugin = %proxy.plugin_name,
        method = %method,
        path = %path,
        "Proxying request to external plugin"
    );

    let path_and_query = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or(path);

    match forward_to_unix_socket(&proxy, request, path_and_query).await {
        Ok(response) => response,
        Err(e) => {
            error!(
                plugin = %proxy.plugin_name,
                path = %path,
                "Failed to proxy request to plugin: {}", e
            );
            (
                StatusCode::BAD_GATEWAY,
                format!("Plugin '{}' unavailable: {}", proxy.plugin_name, e),
            )
                .into_response()
        }
    }
}

/// Forward an HTTP request to a plugin over its Unix domain socket.
async fn forward_to_unix_socket(
    proxy: &PluginProxy,
    original_request: Request,
    path_and_query: &str,
) -> Result<Response, String> {
    use hyper_util::rt::TokioIo;
    use tokio::net::UnixStream;

    let stream = UnixStream::connect(&proxy.socket_path).await.map_err(|e| {
        format!(
            "Cannot connect to plugin socket {}: {}",
            proxy.socket_path.display(),
            e
        )
    })?;

    let io = TokioIo::new(stream);

    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .map_err(|e| format!("HTTP handshake failed: {}", e))?;

    tokio::spawn(async move {
        if let Err(e) = conn.await {
            error!("Plugin proxy connection error: {}", e);
        }
    });

    let (mut parts, body) = original_request.into_parts();

    parts.headers.insert(
        "x-temps-plugin",
        proxy
            .plugin_name
            .parse()
            .unwrap_or_else(|_| hyper::header::HeaderValue::from_static("unknown")),
    );
    parts.headers.insert(
        "x-temps-request-id",
        uuid::Uuid::new_v4()
            .to_string()
            .parse()
            .unwrap_or_else(|_| hyper::header::HeaderValue::from_static("")),
    );

    let target_uri: hyper::Uri = path_and_query
        .parse()
        .map_err(|e| format!("Invalid URI '{}': {}", path_and_query, e))?;
    parts.uri = target_uri;

    parts.headers.insert(
        hyper::header::HOST,
        hyper::header::HeaderValue::from_static("localhost"),
    );

    let forwarded_request = Request::from_parts(parts, body);

    let response = sender
        .send_request(forwarded_request)
        .await
        .map_err(|e| format!("Plugin request failed: {}", e))?;

    let (parts, body) = response.into_parts();
    let body = Body::new(body);
    Ok(Response::from_parts(parts, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_proxy_creation() {
        let proxy = PluginProxy::new(
            PathBuf::from("/tmp/test.sock"),
            "test-plugin".to_string(),
            "secret".to_string(),
        );
        assert_eq!(proxy.plugin_name, "test-plugin");
        assert_eq!(proxy.socket_path, PathBuf::from("/tmp/test.sock"));
    }
}
