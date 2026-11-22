// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The agent calls behind a worker service's runtime, stats and WAL panels,
//! against fake agents: current ones, and ones from before these routes.

use super::*;
use crate::externalsvc::{ServiceConfig, ServiceType};
use axum::{
    extract::Json,
    http::{header::AUTHORIZATION, HeaderMap},
    routing::{delete, get, post},
    Router,
};
use temps_core::sealed_request::{open_request, NodeSealing, SEALED_REQUEST_CONTENT_TYPE};

fn keys() -> &'static (String, String) {
    static KEYS: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();
    KEYS.get_or_init(|| temps_core::worker_ingress::generate_ingress_key().unwrap())
}

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{address}")
}

fn client(url: String) -> RemoteServiceClient {
    RemoteServiceClient::new(url, "node-token".into(), "worker-3".into())
        .unwrap()
        .with_sealing(NodeSealing::Key(keys().1.clone()))
}

fn postgres_config() -> ServiceConfig {
    ServiceConfig {
        name: "orders".into(),
        service_type: ServiceType::Postgres,
        version: Some("18".into()),
        parameters: serde_json::json!({
            "host": "localhost",
            "port": "5561",
            "username": "app",
            "password": "s3cret",
            "database": "orders",
        }),
    }
}

/// The probe request holds the database password: it reaches the agent
/// sealed and with the node token, and the snapshot comes back.
#[tokio::test]
async fn the_wal_probe_is_sealed_and_returns_the_snapshot() {
    async fn handler(headers: HeaderMap, sealed: axum::body::Bytes) -> Json<serde_json::Value> {
        assert_eq!(
            headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()),
            Some("Bearer node-token")
        );
        assert_eq!(
            headers
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some(SEALED_REQUEST_CONTENT_TYPE)
        );
        assert!(
            !String::from_utf8_lossy(&sealed).contains("s3cret"),
            "the password crossed the wire in clear"
        );
        let opened = open_request(&keys().0, "/agent/services/wal-health", &sealed).unwrap();
        let request: RemoteWalHealthRequest = serde_json::from_slice(&opened).unwrap();
        assert_eq!(request.service_config.name, "orders");
        Json(serde_json::json!({
            "success": true,
            "data": {"snapshot": {"pg_wal_bytes": 1024}, "error": null},
            "error": null,
        }))
    }
    let url = serve(Router::new().route("/agent/services/wal-health", post(handler))).await;

    let response = client(url)
        .probe_wal_health(RemoteWalHealthRequest {
            service_config: postgres_config(),
        })
        .await
        .expect("probe");

    match response {
        AgentSupport::Supported(response) => {
            assert_eq!(response.snapshot.unwrap()["pg_wal_bytes"], 1024);
        }
        AgentSupport::Unsupported => panic!("the agent has the route"),
    }
}

/// An agent from before the probe has `DELETE /agent/services/{name}`, which
/// the path matches, so it answers 405: that is "update the agent", not an
/// error.
#[tokio::test]
async fn an_agent_without_the_wal_probe_is_unsupported() {
    let url = serve(Router::new().route(
        "/agent/services/{name}",
        delete(|| async { Json(serde_json::json!({"success": true})) }),
    ))
    .await;

    let response = client(url)
        .probe_wal_health(RemoteWalHealthRequest {
            service_config: postgres_config(),
        })
        .await
        .expect("an old agent is not an error");

    assert!(matches!(response, AgentSupport::Unsupported));
}

/// A plain-HTTP agent is never sent the password.
#[tokio::test]
async fn the_wal_probe_is_not_sent_to_a_node_without_a_key() {
    let url = serve(Router::new()).await;
    let client = RemoteServiceClient::new(url, "node-token".into(), "worker-3".into())
        .unwrap()
        .with_sealing(NodeSealing::PlainHttp);

    let error = client
        .probe_wal_health(RemoteWalHealthRequest {
            service_config: postgres_config(),
        })
        .await
        .expect_err("refused before sending");

    assert!(error.to_string().contains("worker-3"), "{error}");
}

/// A probe that ran but found nothing says why; a very long reason from the
/// agent is bounded.
#[tokio::test]
async fn a_probe_without_a_snapshot_carries_a_bounded_reason() {
    let url = serve(Router::new().route(
        "/agent/services/wal-health",
        post(|| async {
            Json(serde_json::json!({
                "success": true,
                "data": {"snapshot": null, "error": "x".repeat(10_000)},
                "error": null,
            }))
        }),
    ))
    .await;

    let response = client(url)
        .probe_wal_health(RemoteWalHealthRequest {
            service_config: postgres_config(),
        })
        .await
        .expect("probe");

    let AgentSupport::Supported(response) = response else {
        panic!("the agent has the route");
    };
    assert!(response.snapshot.is_none());
    assert!(response.error.unwrap().len() <= MAX_HEALTH_ERROR_BYTES + '…'.len_utf8());
}

/// Runtime and stats are plain GETs with the node token; the stats sample
/// count is bounded to what the agent takes.
#[tokio::test]
async fn runtime_and_stats_reach_the_agent_with_its_token() {
    let url = serve(
        Router::new()
            .route(
                "/agent/services/{name}/runtime",
                get(|headers: HeaderMap| async move {
                    assert_eq!(
                        headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()),
                        Some("Bearer node-token")
                    );
                    Json(serde_json::json!({
                        "success": true,
                        "data": {"found": true, "status": "exited", "exit_code": 137},
                        "error": null,
                    }))
                }),
            )
            .route(
                "/agent/services/{name}/stats",
                get(
                    |axum::extract::Query(query): axum::extract::Query<
                        std::collections::HashMap<String, String>,
                    >| async move {
                        assert_eq!(query.get("samples").map(String::as_str), Some("2"));
                        Json(serde_json::json!({
                            "success": true,
                            "data": {"found": false, "samples": []},
                            "error": null,
                        }))
                    },
                ),
            ),
    )
    .await;
    let client = client(url);

    let AgentSupport::Supported(runtime) = client.service_runtime("postgres-orders").await.unwrap()
    else {
        panic!("the agent has the route");
    };
    assert_eq!(runtime.exit_code, Some(137));

    let AgentSupport::Supported(stats) = client.service_stats("postgres-orders", 9).await.unwrap()
    else {
        panic!("the agent has the route");
    };
    assert!(!stats.found);
}

/// An agent error is an error, not "unsupported": only a missing route is.
#[tokio::test]
async fn an_agent_error_is_not_mistaken_for_an_old_agent() {
    let url = serve(Router::new().route(
        "/agent/services/{name}/runtime",
        get(|| async {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "success": false,
                    "data": null,
                    "error": "Docker daemon is restarting",
                })),
            )
        }),
    ))
    .await;

    let error = client(url)
        .service_runtime("postgres-orders")
        .await
        .expect_err("a failed inspection is an error");

    assert!(
        error.to_string().contains("Docker daemon is restarting"),
        "{error}"
    );
}
