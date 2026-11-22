// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Opens requests the control plane sealed to this node (see
//! [`temps_core::sealed_request`]), so handlers keep receiving plain JSON.
//!
//! Plain requests pass through: a control plane from before sealing still
//! works against an upgraded worker, and which requests carry values is the
//! sender's decision.

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Extension, Request},
    http::{header, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use temps_core::sealed_request::{
    open_request, OpenRequestError, MAX_SEALED_REQUEST_BYTES, MAX_SEALED_UPLOAD_BYTES,
    SEALED_REQUEST_CONTENT_TYPE,
};

use crate::handlers::error_response;
use crate::AgentConfig;

/// This node's private key: from `temps join`, else the one the ingress
/// manager provisioned. Read per sealed request, since that file can appear
/// while the agent runs.
#[derive(Debug, Clone)]
pub struct NodeRequestKey {
    configured: Option<String>,
    key_file: Option<PathBuf>,
}

impl NodeRequestKey {
    pub fn from_config(config: &AgentConfig) -> Self {
        Self {
            configured: config.public_ingress_private_key.clone(),
            key_file: Some(config.provisioned_key_file()),
        }
    }

    #[cfg(test)]
    pub(crate) fn fixed(private_key: Option<String>) -> Self {
        Self {
            configured: private_key,
            key_file: None,
        }
    }

    async fn private_key(&self) -> Option<String> {
        if let Some(key) = &self.configured {
            return Some(key.clone());
        }
        let saved = tokio::fs::read_to_string(self.key_file.as_ref()?)
            .await
            .ok()?;
        let saved = saved.trim();
        (!saved.is_empty()).then(|| saved.to_string())
    }

    /// Open `sealed` for `path`, as what the refusal should tell the sender.
    pub(crate) async fn open(
        &self,
        node: &str,
        path: &str,
        sealed: &[u8],
    ) -> Result<Vec<u8>, SealedRequestRefusal> {
        let Some(private_key) = self.private_key().await else {
            return Err(SealedRequestRefusal::NoKey {
                node: node.to_string(),
            });
        };
        open_request(&private_key, path, sealed).map_err(SealedRequestRefusal::Open)
    }
}

/// Why a sealed request was not handled. Never holds a request's content.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SealedRequestRefusal {
    #[error(
        "Node '{node}' has no key to open sealed requests with. Restart its worker agent so it \
         provisions one, or re-run `temps join` on it"
    )]
    NoKey { node: String },
    #[error(transparent)]
    Open(OpenRequestError),
}

impl SealedRequestRefusal {
    pub(crate) fn status(&self) -> StatusCode {
        match self {
            Self::NoKey { .. } => StatusCode::CONFLICT,
            Self::Open(OpenRequestError::Malformed { .. })
            | Self::Open(OpenRequestError::Decrypt { .. }) => StatusCode::BAD_REQUEST,
        }
    }
}

/// The node name for refusals, kept next to its key.
#[derive(Debug, Clone)]
pub struct SealedRequestState {
    pub key: NodeRequestKey,
    pub node_name: String,
}

impl SealedRequestState {
    pub fn from_config(config: &AgentConfig) -> Arc<Self> {
        Arc::new(Self {
            key: NodeRequestKey::from_config(config),
            node_name: config.node_name.clone(),
        })
    }
}

/// Routes opened by [`open_sealed_uploads`], from inside their upload slot.
const SANDBOX_UPLOAD_PATHS: [&str; 2] = [
    "/agent/sandboxes/write-file",
    "/agent/sandboxes/write-directory",
];

/// Replace a sealed body with the JSON it holds. Runs after authentication, so
/// only an authenticated caller can make the agent decrypt anything.
pub(crate) async fn open_sealed_requests(
    Extension(state): Extension<Arc<SealedRequestState>>,
    request: Request,
    next: Next,
) -> Response {
    if SANDBOX_UPLOAD_PATHS.contains(&request.uri().path()) {
        return next.run(request).await;
    }
    let limit = if request.uri().path() == crate::sandbox_handlers::service_access::PATH {
        // Eight MiB of JSON plus the base64 envelope, bounded before decoding.
        12 * 1024 * 1024
    } else {
        MAX_SEALED_REQUEST_BYTES
    };
    open_sealed(&state, limit, request, next).await
}

/// [`open_sealed_requests`] for the sandbox upload routes: a route layer inside
/// [`crate::sandbox_handlers::limit_uploads`], because opening one of these holds
/// far more memory than any other request and the upload slots are what bound
/// how many run at once.
pub(crate) async fn open_sealed_uploads(
    Extension(state): Extension<Arc<SealedRequestState>>,
    request: Request,
    next: Next,
) -> Response {
    open_sealed(&state, MAX_SEALED_UPLOAD_BYTES, request, next).await
}

async fn open_sealed(
    state: &SealedRequestState,
    limit: usize,
    request: Request,
    next: Next,
) -> Response {
    let sealed = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case(SEALED_REQUEST_CONTENT_TYPE));
    if !sealed {
        return next.run(request).await;
    }

    let path = request.uri().path().to_string();
    let (mut parts, body) = request.into_parts();
    let envelope = match axum::body::to_bytes(body, limit).await {
        Ok(envelope) => envelope,
        Err(error) => {
            tracing::warn!(%path, %error, "refused a sealed request: unreadable or too large");
            return error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!(
                    "The sealed request to {path} is unreadable or larger than {limit} bytes: \
                     {error}"
                ),
            )
            .into_response();
        }
    };
    let opened = match state.key.open(&state.node_name, &path, &envelope).await {
        Ok(opened) => opened,
        Err(refusal) => {
            tracing::warn!(%path, error = %refusal, "refused a sealed request");
            return error_response(refusal.status(), refusal.to_string()).into_response();
        }
    };
    parts.headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    parts.headers.remove(header::CONTENT_LENGTH);
    next.run(Request::from_parts(parts, Body::from(opened)))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{middleware, routing::post, Json, Router};
    use temps_core::sealed_request::NodeSealing;
    use tower::ServiceExt;

    fn keypair() -> (String, String) {
        temps_core::worker_ingress::generate_ingress_key().unwrap()
    }

    fn app(private_key: Option<String>) -> Router {
        Router::new()
            .route(
                "/agent/echo",
                post(|Json(body): Json<serde_json::Value>| async move { Json(body) }),
            )
            .layer(middleware::from_fn(open_sealed_requests))
            .layer(Extension(Arc::new(SealedRequestState {
                key: NodeRequestKey::fixed(private_key),
                node_name: "worker-a".to_string(),
            })))
    }

    fn request(path: &str, content_type: &str, body: Vec<u8>) -> Request {
        Request::builder()
            .method("POST")
            .uri(path)
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from(body))
            .unwrap()
    }

    async fn read(response: Response) -> (StatusCode, String) {
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    fn sealed(public: String, path: &str) -> Vec<u8> {
        NodeSealing::Key(public)
            .seal("worker-a", path, br#"{"API_KEY":"value"}"#)
            .unwrap()
    }

    #[tokio::test]
    async fn a_sealed_request_reaches_the_handler_as_plain_json() {
        let (private, public) = keypair();
        let response = app(Some(private))
            .oneshot(request(
                "/agent/echo",
                SEALED_REQUEST_CONTENT_TYPE,
                sealed(public, "/agent/echo"),
            ))
            .await
            .unwrap();
        let (status, body) = read(response).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, r#"{"API_KEY":"value"}"#);
    }

    /// An agent from before sealing is this: a `Json` handler with no
    /// middleware. It must refuse outright, which is what lets the sender tell
    /// the operator to upgrade instead of reporting a confusing parse error.
    #[tokio::test]
    async fn an_agent_from_before_sealing_refuses_with_415() {
        let (_, public) = keypair();
        let older_agent = Router::new().route(
            "/agent/echo",
            post(|Json(body): Json<serde_json::Value>| async move { Json(body) }),
        );
        let response = older_agent
            .oneshot(request(
                "/agent/echo",
                SEALED_REQUEST_CONTENT_TYPE,
                sealed(public, "/agent/echo"),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn a_plain_request_from_an_older_control_plane_still_works() {
        let (private, _) = keypair();
        let response = app(Some(private))
            .oneshot(request(
                "/agent/echo",
                "application/json",
                br#"{"plain":true}"#.to_vec(),
            ))
            .await
            .unwrap();
        let (status, body) = read(response).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, r#"{"plain":true}"#);
    }

    #[tokio::test]
    async fn a_request_sealed_for_another_route_is_refused_without_reaching_the_handler() {
        let (private, public) = keypair();
        let response = app(Some(private))
            .oneshot(request(
                "/agent/echo",
                SEALED_REQUEST_CONTENT_TYPE,
                sealed(public, "/agent/other"),
            ))
            .await
            .unwrap();
        let (status, body) = read(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("temps join"), "{body}");
        assert!(!body.contains("API_KEY"), "{body}");
    }

    #[tokio::test]
    async fn a_request_sealed_to_another_key_is_refused() {
        let (private, _) = keypair();
        let (_, other_public) = keypair();
        let response = app(Some(private))
            .oneshot(request(
                "/agent/echo",
                SEALED_REQUEST_CONTENT_TYPE,
                sealed(other_public, "/agent/echo"),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_worker_without_a_key_says_so_and_names_itself() {
        let (_, public) = keypair();
        let response = app(None)
            .oneshot(request(
                "/agent/echo",
                SEALED_REQUEST_CONTENT_TYPE,
                sealed(public, "/agent/echo"),
            ))
            .await
            .unwrap();
        let (status, body) = read(response).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("worker-a"), "{body}");
    }

    #[tokio::test]
    async fn an_oversized_envelope_is_refused_before_it_is_buffered_whole() {
        let (private, _) = keypair();
        let response = app(Some(private))
            .oneshot(request(
                "/agent/echo",
                SEALED_REQUEST_CONTENT_TYPE,
                vec![b'x'; MAX_SEALED_REQUEST_BYTES + 1],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn the_key_saved_by_the_ingress_manager_is_used_when_join_gave_none() {
        let (private, public) = keypair();
        let dir = tempfile::tempdir().unwrap();
        let key_file = dir.path().join("public-ingress.key");
        let key = NodeRequestKey {
            configured: None,
            key_file: Some(key_file.clone()),
        };
        let sealed = sealed(public, "/agent/echo");
        // The file appears while the agent runs.
        assert!(matches!(
            key.open("worker-a", "/agent/echo", &sealed).await,
            Err(SealedRequestRefusal::NoKey { .. })
        ));
        std::fs::write(&key_file, format!("{private}\n")).unwrap();
        assert!(key.open("worker-a", "/agent/echo", &sealed).await.is_ok());
    }
}
