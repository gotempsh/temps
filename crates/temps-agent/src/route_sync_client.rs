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
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self {
            control_plane_url,
            node_id,
            node_token,
            store,
            shutdown,
            http,
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
        let since = self.store.current_generation();
        let url = format!(
            "{}/api/internal/nodes/{}/routes/snapshot?since={}",
            self.control_plane_url, self.node_id, since
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
            self.ack(applied).await.ok();
        } else {
            debug!(generation = body.generation, "route snapshot unchanged");
        }

        Ok(())
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
        Ok(())
    }
}

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
        let app = axum::Router::new().route(
            "/api/internal/nodes/{node_id}/routes/snapshot",
            axum::routing::get(move || {
                let body = body.clone();
                async move { (status, axum::Json(body)) }
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
}
