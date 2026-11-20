// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Running one uptime check from this worker (ADR 108 D5), so a monitor can
//! tell a target that is down from a path that is.
//!
//! - Only the control plane can ask: the node token, at the handler too.
//! - The request is validated exactly as the control plane validates it on
//!   save, and probed through the same public-address policy
//!   ([`temps_uptime_probe`]), resolved through this worker's resolver: a
//!   target this node sees as private, loopback or link-local is not dialled.
//! - Headers and a body arrive only sealed to this node's key; the shared
//!   middleware opens them after authentication.
//! - At most [`MAX_CONCURRENT_PROBES`] run at once; more are refused rather
//!   than queued, and the control plane counts this node as not answering.
//! - Nothing is stored or logged: not the target, the outcome nor the
//!   request.

use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    routing::post,
    Extension, Json, Router,
};
use temps_authz::permission_guard;
use temps_core::problemdetails::Problem;
use temps_uptime_probe::{
    NetworkProber, ProbeOutcome, RemoteProbeRequest, TargetProber, REMOTE_PROBE_PATH,
};
use tokio::sync::Semaphore;
use utoipa::OpenApi;

use crate::auth::{require_agent_auth, AgentAuth, RequireAgentAuth};
use crate::handlers::{error_response, AgentResponse};
use crate::sealed_requests::{open_sealed_requests, SealedRequestState};

/// Checks this agent runs at once. Each is bounded by its own timeout
/// (30 seconds at most) and a response body read of 256 KiB.
pub const MAX_CONCURRENT_PROBES: usize = 16;

struct ProbeState {
    prober: Arc<dyn TargetProber>,
    slots: Arc<Semaphore>,
}

pub fn router(auth: Arc<AgentAuth>, sealed: Arc<SealedRequestState>) -> Router {
    routes(Arc::new(NetworkProber), auth, sealed)
}

fn routes(
    prober: Arc<dyn TargetProber>,
    auth: Arc<AgentAuth>,
    sealed: Arc<SealedRequestState>,
) -> Router {
    Router::new()
        .route(REMOTE_PROBE_PATH, post(probe))
        // Outermost last: authentication runs before anything is opened.
        .layer(middleware::from_fn(open_sealed_requests))
        .layer(middleware::from_fn(require_agent_auth))
        .layer(Extension(auth))
        .layer(Extension(sealed))
        .with_state(Arc::new(ProbeState {
            prober,
            slots: Arc::new(Semaphore::new(MAX_CONCURRENT_PROBES)),
        }))
}

/// Run one uptime check from this node
#[utoipa::path(
    post,
    path = "/agent/uptime/probe",
    tag = "Uptime",
    request_body = RemoteProbeRequest,
    responses(
        (status = 200, description = "What this node saw", body = AgentResponse<ProbeOutcome>),
        (status = 400, description = "The check or target is invalid"),
        (status = 401, description = "Node authentication required"),
        (status = 429, description = "This node is already running its maximum of checks")
    ),
    security(("bearer_auth" = []))
)]
async fn probe(
    RequireAgentAuth(auth): RequireAgentAuth,
    State(state): State<Arc<ProbeState>>,
    Json(request): Json<RemoteProbeRequest>,
) -> Result<Response, Problem> {
    permission_guard!(auth, DeploymentsRead);
    let Ok(_slot) = state.slots.clone().try_acquire_owned() else {
        return Ok(error_response(
            StatusCode::TOO_MANY_REQUESTS,
            format!("this node is already running {MAX_CONCURRENT_PROBES} uptime checks"),
        )
        .into_response());
    };
    Ok(match request.run(state.prober.as_ref()).await {
        Ok(outcome) => AgentResponse::ok(outcome).into_response(),
        Err(error) => error_response(StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    })
}

#[derive(OpenApi)]
#[openapi(paths(probe), components(schemas(RemoteProbeRequest, ProbeOutcome)))]
pub struct UptimeProbeApiDoc;

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use axum::body::Body;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use temps_uptime_probe::spec::{CheckRequest, CheckSpec, Target, TcpCheck};
    use temps_uptime_probe::ProbeStatus;
    use tower::ServiceExt;

    const TOKEN: &str = "uptime-probe-test-token";

    struct Counting(AtomicUsize);

    #[async_trait]
    impl TargetProber for Counting {
        async fn probe(&self, _: &CheckSpec, target: &Target, _: &CheckRequest) -> ProbeOutcome {
            self.0.fetch_add(1, Ordering::SeqCst);
            ProbeOutcome {
                status: ProbeStatus::Operational,
                response_time_ms: Some(12),
                message: Some(target.stored()),
            }
        }
    }

    fn app(prober: Arc<Counting>) -> Router {
        routes(
            prober,
            Arc::new(AgentAuth::new(TOKEN)),
            Arc::new(SealedRequestState {
                key: crate::sealed_requests::NodeRequestKey::fixed(None),
                node_name: "worker-test".to_string(),
            }),
        )
    }

    fn body(target: &str) -> String {
        serde_json::to_string(&RemoteProbeRequest {
            check: CheckSpec::Tcp(TcpCheck::default()),
            target: target.to_string(),
            request: CheckRequest::default(),
        })
        .unwrap()
    }

    async fn post(
        app: &Router,
        token: Option<&str>,
        body: String,
    ) -> (StatusCode, serde_json::Value) {
        let mut request =
            axum::http::Request::post(REMOTE_PROBE_PATH).header("content-type", "application/json");
        if let Some(token) = token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    #[tokio::test]
    async fn needs_the_node_token() {
        let prober = Arc::new(Counting(AtomicUsize::new(0)));
        let app = app(prober.clone());
        let (status, _) = post(&app, None, body("db.example.com:5432")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = post(&app, Some("wrong"), body("db.example.com:5432")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(prober.0.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn runs_a_valid_check_and_refuses_an_invalid_one() {
        let prober = Arc::new(Counting(AtomicUsize::new(0)));
        let app = app(prober.clone());
        let (status, json) = post(&app, Some(TOKEN), body("db.example.com:5432")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["success"], true);
        assert_eq!(json["data"]["status"], "operational");
        assert_eq!(json["data"]["message"], "db.example.com:5432");

        let (status, json) = post(&app, Some(TOKEN), body("db.example.com")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(json["error"].as_str().unwrap().contains("port"), "{json}");
        assert_eq!(prober.0.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn the_real_prober_does_not_dial_private_targets() {
        let app = routes(
            Arc::new(NetworkProber),
            Arc::new(AgentAuth::new(TOKEN)),
            Arc::new(SealedRequestState {
                key: crate::sealed_requests::NodeRequestKey::fixed(None),
                node_name: "worker-test".to_string(),
            }),
        );
        let (status, json) = post(&app, Some(TOKEN), body("127.0.0.1:22")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["data"]["status"], "major_outage");
        assert!(
            json["data"]["message"]
                .as_str()
                .unwrap()
                .contains("not public"),
            "{json}"
        );
    }
}
