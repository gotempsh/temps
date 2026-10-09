// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Long-poll client that mirrors the CP's internal-zone route table
//! into [`RouteStore`].
//!
//! Same shape as `temps-dns-resolver::sync_client`: one tokio task,
//! `since=current_generation`, the CP holds the request open until a
//! reload happens or its long-poll deadline fires, the client
//! `apply_snapshot`s the result and then ACKs. Restart-resilient via
//! the disk snapshot in [`RouteStore::load_from_disk`].
//!
//! ## Backoff
//!
//! On any error (network, 5xx, parse failure) we sleep with exponential
//! backoff capped at 30s before retrying. We don't ACK on error, so the
//! CP's view of `applied_generation` can lag during outages — that's
//! correct: it lets ops detect drift. ACK on success.
//!
//! ## CP restart
//!
//! When the CP restarts its in-memory generation resets (today). Our
//! `since` may be > server `current`, in which case the handler still
//! returns a snapshot at the new `current`. We accept whatever
//! generation the server reports and apply unconditionally — the
//! snapshot itself is the source of truth, the number is just a
//! wakeup hint.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{debug, info, warn};

use crate::route_store::{
    PublicIngressCertBundle, PublicIngressCertificates, PublicIngressSnapshot, RouteBackend,
    RouteEntry, SharedRouteStore,
};

#[derive(Debug, Clone, Deserialize)]
struct SnapshotResponse {
    generation: u64,
    routes: Vec<SnapshotRoute>,
    #[serde(default)]
    public_ingress: SnapshotPublicIngress,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct SnapshotPublicIngress {
    enabled: bool,
    routes: Vec<SnapshotRoute>,
    #[serde(default)]
    unsupported_route_count: usize,
    #[serde(default)]
    unsupported_reasons: Vec<String>,
    #[serde(default)]
    certificates: Option<SnapshotCertificates>,
}

#[derive(Debug, Clone, Deserialize)]
struct SnapshotCertificates {
    ephemeral_public_key: String,
    bundles: Vec<SnapshotCertBundle>,
}

#[derive(Debug, Clone, Deserialize)]
struct SnapshotCertBundle {
    domain: String,
    ciphertext: String,
    nonce: String,
    fingerprint: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SnapshotRoute {
    host: String,
    backends: Vec<SnapshotBackend>,
    #[serde(default)]
    deployment_id: Option<i32>,
    #[serde(default)]
    project_id: Option<i32>,
    #[serde(default)]
    environment_id: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
struct SnapshotBackend {
    address: String,
    #[serde(default)]
    container_id: Option<String>,
    #[serde(default)]
    container_name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct AckRequest {
    applied_generation: u64,
}

pub struct RouteSyncClient {
    /// Base URL of the control plane, no trailing slash.
    pub control_plane_url: String,
    /// This node's id (matches the bearer token's owner).
    pub node_id: i32,
    /// Bearer token for `/internal/...` endpoints.
    pub node_token: String,
    pub store: SharedRouteStore,
    pub shutdown: Arc<Notify>,
    /// HTTP client. Long-poll requests can take up to ~25s on the CP
    /// side, so the client timeout must be a comfortable margin
    /// above that.
    pub http: reqwest::Client,
    /// Generation the control plane last accepted an ACK for from this
    /// process, or [`NOTHING_ACKED`]. A fresh process has ACKed nothing: the
    /// generation it loaded from disk may have been applied by a previous
    /// run that died before its ACK got through, so it is sent again.
    last_acked: std::sync::atomic::AtomicU64,
}

impl RouteSyncClient {
    /// Create a sync client. The reqwest client is built with a
    /// 60s request timeout — long-poll on the CP is 25s, plus
    /// transport, plus a margin.
    pub fn new(
        control_plane_url: String,
        node_id: i32,
        node_token: String,
        store: SharedRouteStore,
        shutdown: Arc<Notify>,
    ) -> Result<Self, reqwest::Error> {
        Self::new_with_ca(
            control_plane_url,
            node_id,
            node_token,
            store,
            shutdown,
            None,
        )
    }

    /// [`Self::new`], verifying the control plane against `control_plane_ca`
    /// (the cluster CA, for a node whose join pinned it) instead of the public
    /// roots when given. See [`crate::control_plane_ca`].
    pub fn new_with_ca(
        control_plane_url: String,
        node_id: i32,
        node_token: String,
        store: SharedRouteStore,
        shutdown: Arc<Notify>,
        control_plane_ca: Option<crate::ControlPlaneCa>,
    ) -> Result<Self, reqwest::Error> {
        let http = crate::with_control_plane_trust(
            reqwest::Client::builder().timeout(Duration::from_secs(60)),
            control_plane_ca,
        )
        .build()?;
        let last_acked = std::sync::atomic::AtomicU64::new(NOTHING_ACKED);
        Ok(Self {
            control_plane_url,
            node_id,
            node_token,
            store,
            shutdown,
            http,
            last_acked,
        })
    }

    /// Run forever. Returns when `shutdown` is notified.
    pub async fn run(self) {
        // Backoff state for error recovery.
        let mut backoff = Duration::from_secs(1);
        let max_backoff = Duration::from_secs(30);

        loop {
            // Cooperative shutdown check before each round.
            tokio::select! {
                biased;
                _ = self.shutdown.notified() => {
                    info!("route sync client shutting down");
                    return;
                }
                res = self.tick_once() => {
                    match res {
                        Ok(()) => {
                            // Reset backoff after any successful round.
                            backoff = Duration::from_secs(1);
                        }
                        Err(e) => {
                            warn!(error = %e, ?backoff, "route sync tick failed");
                            tokio::select! {
                                _ = self.shutdown.notified() => return,
                                _ = tokio::time::sleep(backoff) => {}
                            }
                            backoff = (backoff * 2).min(max_backoff);
                        }
                    }
                }
            }
        }
    }

    async fn tick_once(&self) -> Result<(), String> {
        // Resend an ACK that failed (or that a previous run never got
        // through) before polling. While the generation is unchanged nothing
        // else would resend it, and the deployment completion gate waits on
        // exactly this ACK. If it fails again, routes must still arrive: skip
        // the long-poll and fetch the current snapshot at once (`since=0`
        // always answers immediately), then fail the round so the loop backs
        // off and resends the ACK. A refused node is cleared by that same
        // snapshot request.
        let pending_ack = self.ack_if_pending().await.err();
        if let Some(error) = &pending_ack {
            warn!(error = %error, "route ACK not accepted; fetching routes without waiting");
        }
        let since = self.store.current_generation();
        let poll_since = if pending_ack.is_some() { 0 } else { since };
        let url = format!(
            "{}/api/internal/nodes/{}/routes/snapshot?since={}",
            self.control_plane_url, self.node_id, poll_since
        );

        let resp = self
            .http
            .get(&url)
            .bearer_auth(&self.node_token)
            .send()
            .await
            .map_err(|e| format!("GET {url}: {e}"))?;

        if !resp.status().is_success() {
            if matches!(resp.status().as_u16(), 401 | 403 | 404) {
                self.store
                    .apply_public_snapshot(PublicIngressSnapshot::default());
            }
            return Err(format!("CP returned {} for {url}", resp.status()));
        }

        let body: SnapshotResponse = resp
            .json()
            .await
            .map_err(|e| format!("parse snapshot: {e}"))?;

        let public_ingress = PublicIngressSnapshot {
            enabled: body.public_ingress.enabled,
            routes: body
                .public_ingress
                .routes
                .into_iter()
                .map(snapshot_route_into_entry)
                .collect(),
            unsupported_route_count: body.public_ingress.unsupported_route_count,
            unsupported_reasons: body.public_ingress.unsupported_reasons,
            certificates: body.public_ingress.certificates.map(|certificates| {
                PublicIngressCertificates {
                    ephemeral_public_key: certificates.ephemeral_public_key,
                    bundles: certificates
                        .bundles
                        .into_iter()
                        .map(|bundle| PublicIngressCertBundle {
                            domain: bundle.domain,
                            ciphertext: bundle.ciphertext,
                            nonce: bundle.nonce,
                            fingerprint: bundle.fingerprint,
                        })
                        .collect(),
                }
            }),
        };
        self.store.apply_public_snapshot(public_ingress);
        crate::public_ingress::update_snapshot_health(&self.store);

        // Apply unconditionally — even when generation is unchanged
        // (long-poll timeout), this no-ops past the equality check on
        // CP, and we don't have to special-case anything here. The
        // store's apply is idempotent for an unchanged set.
        if body.generation != since || self.store.is_empty() {
            let routes: Vec<RouteEntry> = body
                .routes
                .into_iter()
                .map(snapshot_route_into_entry)
                .collect();
            let applied = self.store.apply_snapshot(body.generation, routes);
            // A failed ACK fails the round: the loop backs off briefly and
            // the next round resends it before polling again.
            self.ack(applied).await?;
        } else {
            debug!(generation = body.generation, "route snapshot unchanged");
        }
        // The outstanding ACK still fails the round unless an ACK for what
        // is applied now got through above.
        if let Some(error) = pending_ack {
            if self.last_acked.load(std::sync::atomic::Ordering::Acquire)
                != self.store.current_generation()
            {
                return Err(error);
            }
        }

        Ok(())
    }

    async fn ack_if_pending(&self) -> Result<(), String> {
        let applied = self.store.current_generation();
        // Generation 0 is an empty store: nothing has been applied yet.
        if applied == 0 || applied == self.last_acked.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(());
        }
        self.ack(applied).await
    }

    async fn ack(&self, applied_generation: u64) -> Result<(), String> {
        let url = format!(
            "{}/api/internal/nodes/{}/routes/ack",
            self.control_plane_url, self.node_id
        );
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.node_token)
            .json(&AckRequest { applied_generation })
            .send()
            .await
            .map_err(|e| format!("POST {url}: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("CP returned {} for {url}", resp.status()));
        }
        self.last_acked
            .store(applied_generation, std::sync::atomic::Ordering::Release);
        Ok(())
    }
}

/// `last_acked` before this process has had any ACK accepted.
const NOTHING_ACKED: u64 = u64::MAX;

fn snapshot_route_into_entry(route: SnapshotRoute) -> RouteEntry {
    RouteEntry {
        host: route.host,
        backends: route
            .backends
            .into_iter()
            .map(|backend| RouteBackend {
                address: backend.address,
                container_id: backend.container_id,
                container_name: backend.container_name,
            })
            .collect(),
        deployment_id: route.deployment_id,
        project_id: route.project_id,
        environment_id: route.environment_id,
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::net::TcpListener;

    use super::*;
    use crate::route_store::RouteStore;

    const PRIVATE_KEY_B64: &str = "dwdtCnMYpX08FsFyUbJmRd9ML4frwJkqsXf7pR25LCo=";
    const PUBLIC_KEY_B64: &str = "hSDwCYkwp1R0i33ctD73Wg2/Og0mOBr066SpjqqbTmo=";

    fn health_test_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    async fn spawn_snapshot_server(
        status: StatusCode,
        body: serde_json::Value,
    ) -> (String, Arc<Notify>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind route-sync control-plane fixture");
        let address = listener
            .local_addr()
            .expect("read route-sync fixture address");
        let app = axum::Router::new()
            .route(
                "/api/internal/nodes/{node_id}/routes/snapshot",
                axum::routing::get(move || {
                    let body = body.clone();
                    async move { (status, axum::Json(body)) }
                }),
            )
            // The ACK answers like the snapshot: a revoked node is refused
            // on both.
            .route(
                "/api/internal/nodes/{node_id}/routes/ack",
                axum::routing::post(move || async move {
                    if status.is_success() {
                        StatusCode::OK
                    } else {
                        status
                    }
                }),
            );
        let shutdown = Arc::new(Notify::new());
        let task_shutdown = Arc::clone(&shutdown);
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move { task_shutdown.notified().await })
                .await
                .expect("serve route-sync control-plane fixture");
        });
        (format!("http://{address}"), shutdown)
    }

    fn route(host: &str) -> RouteEntry {
        RouteEntry {
            host: host.to_string(),
            backends: vec![RouteBackend {
                address: "192.0.2.10:32000".to_string(),
                container_id: Some("test-container".to_string()),
                container_name: Some("test-container-name".to_string()),
            }],
            deployment_id: Some(1),
            project_id: Some(2),
            environment_id: Some(3),
        }
    }

    fn valid_tls_snapshot(host: &str) -> PublicIngressSnapshot {
        let ca = temps_core::node_pki::generate_cluster_ca().expect("generate test CA");
        let leaf = temps_core::node_pki::generate_node_keypair_csr(host, &[host.to_string()])
            .expect("generate test certificate request");
        let signed = temps_core::node_pki::sign_node_csr(
            &ca.cert_pem,
            &ca.key_pem,
            &leaf.csr_pem,
            &[host.to_string()],
        )
        .expect("sign test certificate");
        let certificate = signed.cert_pem.trim().to_string();
        let plaintext = format!("{certificate}\n{}", leaf.key_pem);
        let encryption = temps_core::ecies::EncryptionSession::new(PUBLIC_KEY_B64)
            .expect("create certificate encryption session");
        let encrypted = encryption
            .encrypt(plaintext.as_bytes())
            .expect("encrypt test certificate");
        PublicIngressSnapshot {
            enabled: true,
            routes: vec![route(host)],
            certificates: Some(PublicIngressCertificates {
                ephemeral_public_key: encryption.ephemeral_public_key().to_string(),
                bundles: vec![PublicIngressCertBundle {
                    domain: host.to_string(),
                    ciphertext: encrypted.ciphertext,
                    nonce: encrypted.nonce,
                    fingerprint: temps_core::ecies::cert_fingerprint(&certificate),
                }],
            }),
            unsupported_route_count: 0,
            unsupported_reasons: Vec::new(),
        }
    }

    fn persisted_expiry(path: &std::path::Path) -> i64 {
        let snapshot: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).expect("read persisted route snapshot"))
                .expect("parse persisted route snapshot");
        snapshot["public_ingress_expires_at"]
            .as_i64()
            .expect("snapshot contains public ingress expiry")
    }

    #[tokio::test]
    async fn test_tick_once_unchanged_generation_applies_public_snapshot_and_renews_lease() {
        let _health_guard = health_test_lock().lock().await;
        let _ = rustls::crypto::ring::default_provider().install_default();
        let replacement_snapshot = valid_tls_snapshot("app.example.test");
        let body = serde_json::json!({
            "generation": 7,
            "routes": [],
            "public_ingress": replacement_snapshot
        });
        let (control_plane_url, server_shutdown) =
            spawn_snapshot_server(StatusCode::OK, body).await;
        let snapshot_dir = TempDir::new().expect("create route snapshot directory");
        let snapshot_path = snapshot_dir.path().join("routes.json");
        let store = Arc::new(RouteStore::new(snapshot_path.clone()));
        store.apply_snapshot(7, vec![route("internal.temps.local")]);
        store.configure_public_tls(PRIVATE_KEY_B64.to_string());
        store.apply_public_snapshot(valid_tls_snapshot("app.example.test"));
        let original_key = store
            .lookup_public_tls_key("app.example.test")
            .expect("prepare original TLS certificate");
        let first_expiry = persisted_expiry(&snapshot_path);
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let client = RouteSyncClient::new(
            control_plane_url,
            7,
            "test-token".to_string(),
            Arc::clone(&store),
            Arc::new(Notify::new()),
        )
        .expect("create route sync client");

        client
            .tick_once()
            .await
            .expect("apply unchanged generation");

        let replacement_key = store
            .lookup_public_tls_key("app.example.test")
            .expect("prepare replacement TLS certificate");
        assert!(!Arc::ptr_eq(&original_key, &replacement_key));
        assert!(store.lookup_public("app.example.test").is_some());
        assert_eq!(store.current_generation(), 7);
        assert!(persisted_expiry(&snapshot_path) > first_expiry);
        server_shutdown.notify_waiters();
    }

    #[tokio::test]
    async fn test_tick_once_revocation_status_clears_routes_and_prepared_certificates() {
        let _health_guard = health_test_lock().lock().await;
        let _ = rustls::crypto::ring::default_provider().install_default();
        for status in [
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::NOT_FOUND,
        ] {
            let (control_plane_url, server_shutdown) =
                spawn_snapshot_server(status, serde_json::json!({})).await;
            let snapshot_dir = TempDir::new().expect("create route snapshot directory");
            let store = Arc::new(RouteStore::new(snapshot_dir.path().join("routes.json")));
            store.configure_public_tls(PRIVATE_KEY_B64.to_string());
            store.apply_public_snapshot(valid_tls_snapshot("app.example.test"));
            assert!(store.lookup_public("app.example.test").is_some());
            assert!(store.lookup_public_tls_key("app.example.test").is_some());
            assert_eq!(store.public_ingress_runtime_status().2, 1);
            let client = RouteSyncClient::new(
                control_plane_url,
                7,
                "test-token".to_string(),
                Arc::clone(&store),
                Arc::new(Notify::new()),
            )
            .expect("create route sync client");

            let error = client.tick_once().await.unwrap_err();

            assert!(error.contains(&status.as_u16().to_string()));
            assert!(store.lookup_public("app.example.test").is_none());
            assert!(store.lookup_public_tls_key("app.example.test").is_none());
            assert_eq!(store.public_ingress_runtime_status().2, 0);
            server_shutdown.notify_waiters();
        }
    }

    #[tokio::test]
    async fn test_invalid_certificate_snapshot_health_reports_zero_prepared_certificates() {
        let _health_guard = health_test_lock().lock().await;
        let store = Arc::new(RouteStore::new(
            TempDir::new()
                .expect("create route snapshot directory")
                .path()
                .join("routes.json"),
        ));
        store.configure_public_tls("invalid private key".to_string());
        store.apply_public_snapshot(PublicIngressSnapshot {
            enabled: true,
            routes: vec![route("invalid.example.test")],
            certificates: Some(PublicIngressCertificates {
                ephemeral_public_key: "invalid ephemeral key".to_string(),
                bundles: vec![PublicIngressCertBundle {
                    domain: "invalid.example.test".to_string(),
                    ciphertext: "invalid ciphertext".to_string(),
                    nonce: "invalid nonce".to_string(),
                    fingerprint: "invalid fingerprint".to_string(),
                }],
            }),
            unsupported_route_count: 0,
            unsupported_reasons: Vec::new(),
        });

        crate::public_ingress::update_snapshot_health(&store);

        let health = crate::public_ingress::health().expect("public ingress health initialized");
        assert_eq!(health.route_count, 1);
        assert_eq!(health.certificate_count, 0);
    }

    /// A failed ACK is resent before the next long-poll instead of waiting
    /// for another route change, and an accepted ACK is not repeated.
    #[tokio::test]
    async fn test_failed_ack_is_resent_before_the_next_poll() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let acks = Arc::new(AtomicUsize::new(0));
        let acked_generations = Arc::new(std::sync::Mutex::new(Vec::new()));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind route-sync control-plane fixture");
        let address = listener.local_addr().expect("read fixture address");
        let app = {
            let acks = Arc::clone(&acks);
            let acked_generations = Arc::clone(&acked_generations);
            axum::Router::new()
                .route(
                    "/api/internal/nodes/{node_id}/routes/snapshot",
                    axum::routing::get(|| async {
                        axum::Json(serde_json::json!({
                            "generation": 8,
                            "routes": [{
                                "host": "app.temps.local",
                                "backends": [{"address": "10.0.0.2:8080"}]
                            }]
                        }))
                    }),
                )
                .route(
                    "/api/internal/nodes/{node_id}/routes/ack",
                    axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                        let acks = Arc::clone(&acks);
                        let acked_generations = Arc::clone(&acked_generations);
                        async move {
                            acked_generations
                                .lock()
                                .expect("ack fixture lock")
                                .push(body["applied_generation"].as_u64());
                            if acks.fetch_add(1, Ordering::SeqCst) == 0 {
                                StatusCode::SERVICE_UNAVAILABLE
                            } else {
                                StatusCode::OK
                            }
                        }
                    }),
                )
        };
        let server_shutdown = Arc::new(Notify::new());
        let task_shutdown = Arc::clone(&server_shutdown);
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move { task_shutdown.notified().await })
                .await
                .expect("serve route-sync fixture");
        });
        let snapshot_dir = TempDir::new().expect("create route snapshot directory");
        let store = Arc::new(RouteStore::new(snapshot_dir.path().join("routes.json")));
        let client = RouteSyncClient::new(
            format!("http://{address}"),
            7,
            "test-token".to_string(),
            Arc::clone(&store),
            Arc::new(Notify::new()),
        )
        .expect("create route sync client");

        // Generation 8 is applied, but its ACK is refused: the round fails.
        assert!(client.tick_once().await.is_err());
        assert_eq!(store.current_generation(), 8);
        // The next round resends it first, then polls an unchanged generation.
        client.tick_once().await.expect("resend ACK and poll");
        // Nothing is pending any more.
        client.tick_once().await.expect("poll unchanged generation");
        assert_eq!(acks.load(Ordering::SeqCst), 2);
        assert_eq!(
            *acked_generations.lock().expect("ack fixture lock"),
            vec![Some(8), Some(8)]
        );
        server_shutdown.notify_waiters();
    }

    /// An ACK that keeps failing never stops route updates: the agent
    /// fetches the snapshot without long-polling, applies new routes, and
    /// keeps failing the round so the ACK is retried after backoff.
    #[tokio::test]
    async fn test_failing_acks_do_not_block_route_updates() {
        use std::sync::atomic::{AtomicU64, Ordering};
        let generation = Arc::new(AtomicU64::new(8));
        let polled_since = Arc::new(std::sync::Mutex::new(Vec::new()));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind route-sync control-plane fixture");
        let address = listener.local_addr().expect("read fixture address");
        let app = {
            let generation = Arc::clone(&generation);
            let polled_since = Arc::clone(&polled_since);
            axum::Router::new()
                .route(
                    "/api/internal/nodes/{node_id}/routes/snapshot",
                    axum::routing::get(
                        move |axum::extract::Query(query): axum::extract::Query<
                            std::collections::HashMap<String, String>,
                        >| {
                            let generation = Arc::clone(&generation);
                            let polled_since = Arc::clone(&polled_since);
                            async move {
                                polled_since
                                    .lock()
                                    .expect("snapshot fixture lock")
                                    .push(query.get("since").cloned());
                                let current = generation.fetch_add(1, Ordering::SeqCst);
                                axum::Json(serde_json::json!({
                                    "generation": current,
                                    "routes": [{
                                        "host": format!("gen-{current}.temps.local"),
                                        "backends": [{"address": "10.0.0.2:8080"}]
                                    }]
                                }))
                            }
                        },
                    ),
                )
                .route(
                    "/api/internal/nodes/{node_id}/routes/ack",
                    axum::routing::post(|| async { StatusCode::SERVICE_UNAVAILABLE }),
                )
        };
        let server_shutdown = Arc::new(Notify::new());
        let task_shutdown = Arc::clone(&server_shutdown);
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move { task_shutdown.notified().await })
                .await
                .expect("serve route-sync fixture");
        });
        let snapshot_dir = TempDir::new().expect("create route snapshot directory");
        let store = Arc::new(RouteStore::new(snapshot_dir.path().join("routes.json")));
        let client = RouteSyncClient::new(
            format!("http://{address}"),
            7,
            "test-token".to_string(),
            Arc::clone(&store),
            Arc::new(Notify::new()),
        )
        .expect("create route sync client");

        assert!(client.tick_once().await.is_err());
        assert_eq!(store.current_generation(), 8);
        // The ACK for 8 fails again, yet generation 9 is fetched and applied.
        assert!(client.tick_once().await.is_err());
        assert_eq!(store.current_generation(), 9);
        assert_eq!(
            *polled_since.lock().expect("snapshot fixture lock"),
            vec![Some("0".to_string()), Some("0".to_string())],
            "with an ACK outstanding the agent does not long-poll"
        );
        server_shutdown.notify_waiters();
    }

    /// A restarted agent ACKs the generation it loaded from disk before it
    /// parks on the long-poll: the run that applied it may have died before
    /// its ACK got through, and the generation not changing again would
    /// otherwise leave the node behind for good.
    #[tokio::test]
    async fn test_restart_acks_the_generation_loaded_from_disk() {
        let acked_generations = Arc::new(std::sync::Mutex::new(Vec::new()));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind route-sync control-plane fixture");
        let address = listener.local_addr().expect("read fixture address");
        let app = {
            let acked_generations = Arc::clone(&acked_generations);
            axum::Router::new()
                .route(
                    "/api/internal/nodes/{node_id}/routes/snapshot",
                    axum::routing::get(|| async {
                        axum::Json(serde_json::json!({
                            "generation": 8,
                            "routes": [{
                                "host": "app.temps.local",
                                "backends": [{"address": "10.0.0.2:8080"}]
                            }]
                        }))
                    }),
                )
                .route(
                    "/api/internal/nodes/{node_id}/routes/ack",
                    axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                        let acked_generations = Arc::clone(&acked_generations);
                        async move {
                            acked_generations
                                .lock()
                                .expect("ack fixture lock")
                                .push(body["applied_generation"].as_u64());
                            StatusCode::OK
                        }
                    }),
                )
        };
        let server_shutdown = Arc::new(Notify::new());
        let task_shutdown = Arc::clone(&server_shutdown);
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move { task_shutdown.notified().await })
                .await
                .expect("serve route-sync fixture");
        });
        let snapshot_dir = TempDir::new().expect("create route snapshot directory");
        let snapshot_path = snapshot_dir.path().join("routes.json");
        // The previous run applied and persisted generation 8.
        RouteStore::new(snapshot_path.clone()).apply_snapshot(8, vec![route("app.temps.local")]);
        let store = Arc::new(RouteStore::new(snapshot_path));
        store.load_from_disk();
        assert_eq!(store.current_generation(), 8);
        let client = RouteSyncClient::new(
            format!("http://{address}"),
            7,
            "test-token".to_string(),
            Arc::clone(&store),
            Arc::new(Notify::new()),
        )
        .expect("create route sync client");

        client.tick_once().await.expect("ACK and poll");
        client.tick_once().await.expect("poll unchanged generation");

        assert_eq!(
            *acked_generations.lock().expect("ack fixture lock"),
            vec![Some(8)],
            "the loaded generation is ACKed once, before the first poll"
        );
        server_shutdown.notify_waiters();
    }
}
