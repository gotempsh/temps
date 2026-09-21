// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::response::IntoResponse;
use tempfile::TempDir;
use temps_agent::internal_proxy::spawn_public_http;
use temps_agent::public_ingress::{self, PublicIngressConfig};
use temps_agent::route_store::{
    PublicIngressCertBundle, PublicIngressCertificates, PublicIngressSnapshot, RouteBackend,
    RouteEntry, RouteStore, SharedRouteStore,
};
use temps_agent::route_sync_client::RouteSyncClient;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Notify};
use tokio::time::timeout;

struct IngressHarness {
    address: std::net::SocketAddr,
    store: SharedRouteStore,
    shutdown: Arc<Notify>,
    _snapshot_dir: TempDir,
}

impl IngressHarness {
    async fn start(backend_address: std::net::SocketAddr, enabled: bool) -> Self {
        install_rustls_provider();
        let snapshot_dir = TempDir::new().expect("create isolated route snapshot directory");
        let store = Arc::new(RouteStore::new(snapshot_dir.path().join("routes.json")));
        store.apply_public_snapshot(PublicIngressSnapshot {
            enabled,
            routes: vec![RouteEntry {
                host: "app.example.test".to_string(),
                backends: vec![RouteBackend {
                    address: backend_address.to_string(),
                    container_id: Some("test-backend".to_string()),
                    container_name: Some("public-ingress-test-backend".to_string()),
                }],
                deployment_id: Some(41),
                project_id: Some(42),
                environment_id: Some(43),
            }],
            certificates: None,
            unsupported_route_count: 0,
            unsupported_reasons: Vec::new(),
        });

        let shutdown = Arc::new(Notify::new());
        let address = spawn_public_http(
            "127.0.0.1:0".parse().expect("parse loopback address"),
            Arc::clone(&store),
            Arc::clone(&shutdown),
        )
        .await
        .expect("start public ingress listener");

        Self {
            address,
            store,
            shutdown,
            _snapshot_dir: snapshot_dir,
        }
    }

    async fn request(&self, request: &str) -> String {
        let mut stream = TcpStream::connect(self.address)
            .await
            .expect("connect to public ingress");
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write request to public ingress");
        let mut response = Vec::new();
        timeout(Duration::from_secs(3), stream.read_to_end(&mut response))
            .await
            .expect("public ingress response timed out")
            .expect("read public ingress response");
        String::from_utf8(response).expect("public ingress response is UTF-8")
    }
}

fn install_rustls_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

async fn acme_request(ingress_address: std::net::SocketAddr, host: &str, token: &str) -> String {
    let mut client = TcpStream::connect(ingress_address)
        .await
        .expect("connect to ACME ingress");
    client
        .write_all(
            format!(
                "GET /.well-known/acme-challenge/{token} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .expect("write ACME request");
    let mut response = Vec::new();
    timeout(Duration::from_secs(7), client.read_to_end(&mut response))
        .await
        .expect("ACME response timed out")
        .expect("read ACME response");
    String::from_utf8(response).expect("ACME response is UTF-8")
}

async fn spawn_acme_test_ingress(
    store: SharedRouteStore,
    control_plane_address: std::net::SocketAddr,
) -> (public_ingress::PublicIngressHandle, Arc<Notify>) {
    install_rustls_provider();
    let shutdown = Arc::new(Notify::new());
    let handle = public_ingress::spawn(
        PublicIngressConfig {
            http_address: "127.0.0.1:0".parse().expect("parse HTTP listen address"),
            https_address: "127.0.0.1:0".parse().expect("parse HTTPS listen address"),
            private_key_b64: "dwdtCnMYpX08FsFyUbJmRd9ML4frwJkqsXf7pR25LCo=".to_string(),
            control_plane_url: format!("http://{control_plane_address}"),
            node_id: 7,
            node_token: "test-node-token".to_string(),
        },
        store,
        Arc::clone(&shutdown),
    )
    .await
    .expect("start ACME ingress fixture");
    (handle, shutdown)
}

fn enabled_public_store(snapshot_dir: &TempDir) -> SharedRouteStore {
    let store = Arc::new(RouteStore::new(snapshot_dir.path().join("routes.json")));
    store.apply_public_snapshot(PublicIngressSnapshot {
        enabled: true,
        routes: Vec::new(),
        certificates: None,
        unsupported_route_count: 0,
        unsupported_reasons: Vec::new(),
    });
    store
}

struct DockerContainerGuard {
    name: String,
}

impl Drop for DockerContainerGuard {
    fn drop(&mut self) {
        let _ = std::process::Command::new("docker")
            .args(["rm", "-f", &self.name])
            .output();
    }
}

impl Drop for IngressHarness {
    fn drop(&mut self) {
        self.shutdown.notify_waiters();
    }
}

async fn start_recording_backend() -> (std::net::SocketAddr, mpsc::Receiver<String>, Arc<Notify>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test backend");
    let address = listener.local_addr().expect("read test backend address");
    let (request_tx, request_rx) = mpsc::channel(4);
    let shutdown = Arc::new(Notify::new());
    let task_shutdown = Arc::clone(&shutdown);

    tokio::spawn(async move {
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((mut stream, _)) = accepted else { break };
                    let request_tx = request_tx.clone();
                    tokio::spawn(async move {
                        let mut request = vec![0; 8192];
                        let size = stream.read(&mut request).await.expect("read backend request");
                        let request = String::from_utf8_lossy(&request[..size]).into_owned();
                        request_tx.send(request).await.expect("record backend request");
                        stream
                            .write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\nConnection: close\r\n\r\nbackend-body",
                            )
                            .await
                            .expect("write backend response");
                    });
                }
                _ = task_shutdown.notified() => break,
            }
        }
    });

    (address, request_rx, shutdown)
}

#[tokio::test]
async fn test_public_ingress_known_host_forwards_request_to_published_backend() {
    let (backend_address, mut requests, backend_shutdown) = start_recording_backend().await;
    let ingress = IngressHarness::start(backend_address, true).await;

    let response = ingress
        .request("GET /hello?source=test HTTP/1.1\r\nHost: app.example.test\r\nConnection: close\r\n\r\n")
        .await;

    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    assert!(response.ends_with("backend-body"), "{response}");
    let backend_request = timeout(Duration::from_secs(1), requests.recv())
        .await
        .expect("backend request timed out")
        .expect("backend request channel closed");
    assert!(backend_request.starts_with("GET /hello?source=test HTTP/1.1"));
    assert!(backend_request
        .to_ascii_lowercase()
        .contains("host: app.example.test"));
    backend_shutdown.notify_waiters();
}

#[tokio::test]
async fn test_public_ingress_unknown_host_returns_not_found_without_backend_access() {
    let (backend_address, mut requests, backend_shutdown) = start_recording_backend().await;
    let ingress = IngressHarness::start(backend_address, true).await;

    let response = ingress
        .request("GET / HTTP/1.1\r\nHost: unknown.example.test\r\nConnection: close\r\n\r\n")
        .await;

    assert!(response.starts_with("HTTP/1.1 404 Not Found"), "{response}");
    assert!(
        timeout(Duration::from_millis(150), requests.recv())
            .await
            .is_err(),
        "unknown public host unexpectedly reached a backend"
    );
    backend_shutdown.notify_waiters();
}

#[tokio::test]
async fn test_public_ingress_duplicate_host_headers_are_rejected_without_backend_access() {
    let (backend_address, mut requests, backend_shutdown) = start_recording_backend().await;
    let ingress = IngressHarness::start(backend_address, true).await;

    let response = ingress
        .request(
            "GET / HTTP/1.1\r\nHost: app.example.test\r\nHost: unknown.example.test\r\nConnection: close\r\n\r\n",
        )
        .await;

    assert!(
        response.starts_with("HTTP/1.1 400 Bad Request")
            || response.starts_with("HTTP/1.1 421 Misdirected Request"),
        "duplicate Host headers must fail closed: {response}"
    );
    assert!(
        timeout(Duration::from_millis(150), requests.recv())
            .await
            .is_err(),
        "duplicate Host headers unexpectedly reached a backend"
    );
    backend_shutdown.notify_waiters();
}

#[tokio::test]
async fn test_public_ingress_forged_forwarding_headers_are_replaced() {
    let (backend_address, mut requests, backend_shutdown) = start_recording_backend().await;
    let ingress = IngressHarness::start(backend_address, true).await;

    let response = ingress
        .request(
            "GET /headers HTTP/1.1\r\nHost: app.example.test:8443\r\nForwarded: for=203.0.113.99;proto=http\r\nX-Forwarded-For: 203.0.113.99\r\nX-Forwarded-Host: attacker.example\r\nX-Forwarded-Proto: http\r\nConnection: close\r\n\r\n",
        )
        .await;

    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    let request = timeout(Duration::from_secs(1), requests.recv())
        .await
        .expect("backend request timed out")
        .expect("backend request channel closed")
        .to_ascii_lowercase();
    assert!(!request.contains("203.0.113.99"), "{request}");
    assert!(!request.contains("attacker.example"), "{request}");
    assert!(request.contains("x-forwarded-for: 127.0.0.1"), "{request}");
    assert!(
        request.contains("x-forwarded-host: app.example.test:8443"),
        "{request}"
    );
    assert_eq!(
        request
            .lines()
            .filter(|line| line.starts_with("host:"))
            .collect::<Vec<_>>(),
        vec!["host: app.example.test:8443"]
    );
    backend_shutdown.notify_waiters();
}

#[tokio::test]
async fn test_public_ingress_disabled_snapshot_refuses_cached_route() {
    let (backend_address, mut requests, backend_shutdown) = start_recording_backend().await;
    let ingress = IngressHarness::start(backend_address, false).await;

    let response = ingress
        .request("GET / HTTP/1.1\r\nHost: app.example.test\r\nConnection: close\r\n\r\n")
        .await;

    assert!(
        response.starts_with("HTTP/1.1 404 Not Found")
            || response.starts_with("HTTP/1.1 503 Service Unavailable"),
        "disabled public ingress must fail closed: {response}"
    );
    assert!(
        timeout(Duration::from_millis(150), requests.recv())
            .await
            .is_err(),
        "disabled public ingress unexpectedly reached a backend"
    );
    backend_shutdown.notify_waiters();
}

#[tokio::test]
async fn test_public_ingress_unexpired_disk_lease_survives_restart() {
    let (backend_address, mut requests, backend_shutdown) = start_recording_backend().await;
    let first = IngressHarness::start(backend_address, true).await;
    let snapshot_path = first._snapshot_dir.path().join("routes.json");
    let reloaded_store = Arc::new(RouteStore::new(snapshot_path));
    reloaded_store.load_from_disk();
    assert!(reloaded_store.public_ingress_snapshot().enabled);

    let shutdown = Arc::new(Notify::new());
    let address = spawn_public_http(
        "127.0.0.1:0".parse().expect("parse loopback address"),
        reloaded_store,
        Arc::clone(&shutdown),
    )
    .await
    .expect("start public ingress from cached snapshot");
    let cached = IngressHarness {
        address,
        store: Arc::clone(&first.store),
        shutdown,
        _snapshot_dir: TempDir::new().expect("create ownership placeholder"),
    };

    let response = cached
        .request("GET /cached HTTP/1.1\r\nHost: app.example.test\r\nConnection: close\r\n\r\n")
        .await;

    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    assert!(timeout(Duration::from_secs(1), requests.recv())
        .await
        .expect("cached backend request timed out")
        .is_some());
    backend_shutdown.notify_waiters();
}

#[test]
fn test_public_ingress_invalid_disk_lease_fails_closed() {
    for invalid_expiry in [
        0,
        chrono::Utc::now().timestamp().saturating_sub(1),
        chrono::Utc::now().timestamp().saturating_add(600),
    ] {
        let snapshot_dir = TempDir::new().expect("create isolated route snapshot directory");
        let snapshot_path = snapshot_dir.path().join("routes.json");
        let store = RouteStore::new(snapshot_path.clone());
        store.apply_public_snapshot(PublicIngressSnapshot {
            enabled: true,
            routes: vec![RouteEntry {
                host: "app.example.test".to_string(),
                backends: vec![RouteBackend {
                    address: "127.0.0.1:9".to_string(),
                    container_id: None,
                    container_name: None,
                }],
                deployment_id: None,
                project_id: None,
                environment_id: None,
            }],
            certificates: None,
            unsupported_route_count: 0,
            unsupported_reasons: Vec::new(),
        });
        let mut persisted: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&snapshot_path).expect("read persisted ingress snapshot"),
        )
        .expect("parse persisted ingress snapshot");
        persisted["public_ingress_expires_at"] = serde_json::json!(invalid_expiry);
        std::fs::write(
            &snapshot_path,
            serde_json::to_vec(&persisted).expect("serialize modified ingress snapshot"),
        )
        .expect("write modified ingress snapshot");

        let reloaded = RouteStore::new(snapshot_path);
        reloaded.load_from_disk();
        assert!(
            !reloaded.public_ingress_snapshot().enabled,
            "invalid cached lease {invalid_expiry} remained enabled"
        );
        assert!(
            reloaded.lookup_public("app.example.test").is_none(),
            "invalid cached lease {invalid_expiry} retained routable public ingress"
        );
    }
}

#[tokio::test]
async fn test_public_ingress_authenticated_memory_snapshot_survives_control_plane_loss() {
    let (backend_address, mut requests, backend_shutdown) = start_recording_backend().await;
    let cp_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock control plane");
    let cp_address = cp_listener
        .local_addr()
        .expect("read mock control-plane address");
    let snapshot = serde_json::json!({
        "generation": 1,
        "routes": [],
        "public_ingress": {
            "enabled": true,
            "routes": [{
                "host": "app.example.test",
                "backends": [{
                    "address": backend_address.to_string(),
                    "container_id": "cp-loss-backend",
                    "container_name": "public-ingress-cp-loss-backend"
                }],
                "deployment_id": 61,
                "project_id": 62,
                "environment_id": 63
            }]
        }
    });
    let cp_router = axum::Router::new()
        .route(
            "/api/internal/nodes/{node_id}/routes/snapshot",
            axum::routing::get({
                let snapshot = snapshot.clone();
                move || {
                    let snapshot = snapshot.clone();
                    async move { axum::Json(snapshot) }
                }
            }),
        )
        .route(
            "/api/internal/nodes/{node_id}/routes/ack",
            axum::routing::post(|| async { axum::http::StatusCode::NO_CONTENT }),
        );
    let cp_shutdown = Arc::new(Notify::new());
    let cp_shutdown_task = Arc::clone(&cp_shutdown);
    tokio::spawn(async move {
        axum::serve(cp_listener, cp_router)
            .with_graceful_shutdown(async move { cp_shutdown_task.notified().await })
            .await
            .expect("serve mock control plane");
    });

    let snapshot_dir = TempDir::new().expect("create isolated route snapshot directory");
    let store = Arc::new(RouteStore::new(snapshot_dir.path().join("routes.json")));
    let sync_shutdown = Arc::new(Notify::new());
    let sync = RouteSyncClient::new(
        format!("http://{cp_address}"),
        7,
        "test-node-token".to_string(),
        Arc::clone(&store),
        Arc::clone(&sync_shutdown),
    )
    .expect("create route sync client");
    tokio::spawn(sync.run());
    timeout(Duration::from_secs(3), async {
        while !store.public_ingress_snapshot().enabled {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("agent did not apply authenticated public snapshot");

    cp_shutdown.notify_waiters();
    tokio::time::sleep(Duration::from_millis(25)).await;
    let ingress_shutdown = Arc::new(Notify::new());
    let ingress_address = spawn_public_http(
        "127.0.0.1:0".parse().expect("parse loopback address"),
        store,
        Arc::clone(&ingress_shutdown),
    )
    .await
    .expect("start public ingress after control-plane loss");
    let mut client = TcpStream::connect(ingress_address)
        .await
        .expect("connect to public ingress");
    client
        .write_all(
            b"GET /during-outage HTTP/1.1\r\nHost: app.example.test\r\nConnection: close\r\n\r\n",
        )
        .await
        .expect("write request during control-plane outage");
    let mut response = Vec::new();
    timeout(Duration::from_secs(3), client.read_to_end(&mut response))
        .await
        .expect("outage response timed out")
        .expect("read outage response");

    assert!(
        String::from_utf8_lossy(&response).starts_with("HTTP/1.1 200 OK"),
        "{}",
        String::from_utf8_lossy(&response)
    );
    assert!(timeout(Duration::from_secs(1), requests.recv())
        .await
        .expect("backend request during CP outage timed out")
        .is_some());
    sync_shutdown.notify_waiters();
    ingress_shutdown.notify_waiters();
    backend_shutdown.notify_waiters();
}

#[tokio::test]
async fn test_public_ingress_https_uses_synced_certificate_and_forwards_request() {
    const PRIVATE_KEY_B64: &str = "dwdtCnMYpX08FsFyUbJmRd9ML4frwJkqsXf7pR25LCo=";
    const PUBLIC_KEY_B64: &str = "hSDwCYkwp1R0i33ctD73Wg2/Og0mOBr066SpjqqbTmo=";

    install_rustls_provider();
    let (backend_address, mut requests, backend_shutdown) = start_recording_backend().await;
    let snapshot_dir = TempDir::new().expect("create isolated route snapshot directory");
    let store = Arc::new(RouteStore::new(snapshot_dir.path().join("routes.json")));

    let ca = temps_core::node_pki::generate_cluster_ca().expect("generate test CA");
    let leaf = temps_core::node_pki::generate_node_keypair_csr(
        "app.example.test",
        &["app.example.test".to_string()],
    )
    .expect("generate test leaf key and CSR");
    let signed = temps_core::node_pki::sign_node_csr(
        &ca.cert_pem,
        &ca.key_pem,
        &leaf.csr_pem,
        &["app.example.test".to_string()],
    )
    .expect("sign test ingress certificate");
    let pem_bundle = format!("{}\n{}", signed.cert_pem, leaf.key_pem);
    let encryption = temps_core::ecies::EncryptionSession::new(PUBLIC_KEY_B64)
        .expect("create certificate encryption session");
    let encrypted = encryption
        .encrypt(pem_bundle.as_bytes())
        .expect("encrypt certificate bundle");

    store.apply_public_snapshot(PublicIngressSnapshot {
        enabled: true,
        routes: vec![RouteEntry {
            host: "app.example.test".to_string(),
            backends: vec![RouteBackend {
                address: backend_address.to_string(),
                container_id: Some("tls-test-backend".to_string()),
                container_name: Some("public-ingress-tls-test-backend".to_string()),
            }],
            deployment_id: Some(51),
            project_id: Some(52),
            environment_id: Some(53),
        }],
        certificates: Some(PublicIngressCertificates {
            ephemeral_public_key: encryption.ephemeral_public_key().to_string(),
            bundles: vec![PublicIngressCertBundle {
                domain: "app.example.test".to_string(),
                ciphertext: encrypted.ciphertext,
                nonce: encrypted.nonce,
                fingerprint: temps_core::ecies::cert_fingerprint(signed.cert_pem.trim()),
            }],
        }),
        unsupported_route_count: 0,
        unsupported_reasons: Vec::new(),
    });

    let shutdown = Arc::new(Notify::new());
    let ingress = public_ingress::spawn(
        PublicIngressConfig {
            http_address: "127.0.0.1:0".parse().expect("parse HTTP listen address"),
            https_address: "127.0.0.1:0".parse().expect("parse HTTPS listen address"),
            private_key_b64: PRIVATE_KEY_B64.to_string(),
            control_plane_url: "http://127.0.0.1:9".to_string(),
            node_id: 7,
            node_token: "test-node-token".to_string(),
        },
        store,
        Arc::clone(&shutdown),
    )
    .await
    .expect("start TLS public ingress");

    let mut roots = rustls::RootCertStore::empty();
    let mut ca_reader = std::io::BufReader::new(ca.cert_pem.as_bytes());
    let ca_der = rustls_pemfile::certs(&mut ca_reader)
        .next()
        .expect("test CA PEM contains a certificate")
        .expect("parse test CA certificate");
    roots.add(ca_der).expect("trust test CA");
    let client_config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(client_config));
    let tcp = TcpStream::connect(ingress.https_address())
        .await
        .expect("connect to HTTPS ingress");
    let server_name =
        rustls::pki_types::ServerName::try_from("app.example.test").expect("valid test DNS name");
    let mut tls = connector
        .connect(server_name, tcp)
        .await
        .expect("complete TLS handshake with synced certificate");
    tls.write_all(b"GET /secure HTTP/1.1\r\nHost: app.example.test\r\nConnection: close\r\n\r\n")
        .await
        .expect("write HTTPS request");
    let mut response = Vec::new();
    timeout(Duration::from_secs(3), tls.read_to_end(&mut response))
        .await
        .expect("HTTPS response timed out")
        .expect("read HTTPS response");
    let response = String::from_utf8(response).expect("HTTPS response is UTF-8");

    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    assert!(response.ends_with("backend-body"), "{response}");
    assert!(timeout(Duration::from_secs(1), requests.recv())
        .await
        .expect("HTTPS backend request timed out")
        .is_some());

    let tcp = TcpStream::connect(ingress.https_address())
        .await
        .expect("connect for mismatched SNI/Host request");
    let server_name =
        rustls::pki_types::ServerName::try_from("app.example.test").expect("valid test DNS name");
    let mut mismatched = connector
        .connect(server_name, tcp)
        .await
        .expect("complete second TLS handshake");
    mismatched
        .write_all(b"GET / HTTP/1.1\r\nHost: unknown.example.test\r\nConnection: close\r\n\r\n")
        .await
        .expect("write mismatched SNI/Host request");
    let mut mismatch_response = Vec::new();
    timeout(
        Duration::from_secs(3),
        mismatched.read_to_end(&mut mismatch_response),
    )
    .await
    .expect("mismatched SNI/Host response timed out")
    .expect("read mismatched SNI/Host response");
    assert!(
        String::from_utf8_lossy(&mismatch_response).starts_with("HTTP/1.1 421 Misdirected Request"),
        "{}",
        String::from_utf8_lossy(&mismatch_response)
    );
    shutdown.notify_waiters();
    backend_shutdown.notify_waiters();
}

#[tokio::test]
async fn test_public_ingress_acme_relay_returns_plaintext_for_pending_host_only() {
    install_rustls_provider();
    let cp_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ACME control-plane fixture");
    let cp_address = cp_listener.local_addr().expect("read ACME fixture address");
    let cp_hits = Arc::new(AtomicUsize::new(0));
    let handler_hits = Arc::clone(&cp_hits);
    let cp_router = axum::Router::new().route(
        "/api/internal/nodes/{node_id}/acme-challenge",
        axum::routing::get(move |request: axum::extract::Request| {
            let handler_hits = Arc::clone(&handler_hits);
            async move {
                handler_hits.fetch_add(1, Ordering::SeqCst);
                let authorized = request
                    .headers()
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    == Some("Bearer test-node-token");
                let query = request.uri().query().unwrap_or_default();
                let expected_host = query
                    .split('&')
                    .any(|part| part == "host=pending.example.test");
                let expected_token = query.split('&').any(|part| part == "token=test-token");
                if authorized && expected_host && expected_token {
                    (
                        axum::http::StatusCode::OK,
                        axum::Json(serde_json::json!({
                            "key_authorization": "test-token.test-authorization"
                        })),
                    )
                        .into_response()
                } else {
                    axum::http::StatusCode::NOT_FOUND.into_response()
                }
            }
        }),
    );
    let cp_shutdown = Arc::new(Notify::new());
    let cp_shutdown_task = Arc::clone(&cp_shutdown);
    tokio::spawn(async move {
        axum::serve(cp_listener, cp_router)
            .with_graceful_shutdown(async move { cp_shutdown_task.notified().await })
            .await
            .expect("serve ACME control-plane fixture");
    });

    let snapshot_dir = TempDir::new().expect("create isolated route snapshot directory");
    let store = Arc::new(RouteStore::new(snapshot_dir.path().join("routes.json")));
    store.apply_public_snapshot(PublicIngressSnapshot {
        enabled: true,
        routes: Vec::new(),
        certificates: None,
        unsupported_route_count: 0,
        unsupported_reasons: Vec::new(),
    });
    let ingress_shutdown = Arc::new(Notify::new());
    let ingress = public_ingress::spawn(
        PublicIngressConfig {
            http_address: "127.0.0.1:0".parse().expect("parse HTTP listen address"),
            https_address: "127.0.0.1:0".parse().expect("parse HTTPS listen address"),
            private_key_b64: "dwdtCnMYpX08FsFyUbJmRd9ML4frwJkqsXf7pR25LCo=".to_string(),
            control_plane_url: format!("http://{cp_address}"),
            node_id: 7,
            node_token: "test-node-token".to_string(),
        },
        Arc::clone(&store),
        Arc::clone(&ingress_shutdown),
    )
    .await
    .expect("start public ingress with ACME relay");

    let ingress_address = ingress.http_address();
    let success = acme_request(ingress_address, "pending.example.test", "test-token").await;
    assert!(success.starts_with("HTTP/1.1 200 OK"), "{success}");
    assert!(
        success.ends_with("test-token.test-authorization"),
        "ACME key authorization must be returned as plaintext: {success}"
    );
    let hits_after_success = cp_hits.load(Ordering::SeqCst);
    let cached_success = acme_request(ingress_address, "pending.example.test", "test-token").await;
    assert!(cached_success.starts_with("HTTP/1.1 200 OK"));
    assert!(cached_success.ends_with("test-token.test-authorization"));
    assert_eq!(cp_hits.load(Ordering::SeqCst), hits_after_success);

    let unknown_token =
        acme_request(ingress_address, "pending.example.test", "unknown-token").await;
    assert!(
        unknown_token.starts_with("HTTP/1.1 404 Not Found"),
        "{unknown_token}"
    );
    let hits_after_negative = cp_hits.load(Ordering::SeqCst);
    let cached_negative =
        acme_request(ingress_address, "pending.example.test", "unknown-token").await;
    assert!(cached_negative.starts_with("HTTP/1.1 404 Not Found"));
    assert_eq!(cp_hits.load(Ordering::SeqCst), hits_after_negative);

    let unknown_host = acme_request(ingress_address, "unknown.example.test", "test-token").await;
    assert!(
        unknown_host.starts_with("HTTP/1.1 404 Not Found"),
        "{unknown_host}"
    );

    store.apply_public_snapshot(PublicIngressSnapshot::default());
    let hits_before_disabled_request = cp_hits.load(Ordering::SeqCst);
    let disabled = acme_request(ingress_address, "pending.example.test", "test-token").await;
    assert!(disabled.starts_with("HTTP/1.1 404 Not Found"), "{disabled}");
    assert_eq!(
        cp_hits.load(Ordering::SeqCst),
        hits_before_disabled_request,
        "disabled ingress must not relay an ACME request to the control plane"
    );
    ingress_shutdown.notify_waiters();
    cp_shutdown.notify_waiters();
}

#[tokio::test]
async fn test_public_ingress_acme_global_burst_limits_fifth_uncached_lookup() {
    let cp_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ACME control-plane fixture");
    let cp_address = cp_listener.local_addr().expect("read ACME fixture address");
    let cp_hits = Arc::new(AtomicUsize::new(0));
    let handler_hits = Arc::clone(&cp_hits);
    let cp_router = axum::Router::new().route(
        "/api/internal/nodes/{node_id}/acme-challenge",
        axum::routing::get(move || {
            let handler_hits = Arc::clone(&handler_hits);
            async move {
                handler_hits.fetch_add(1, Ordering::SeqCst);
                axum::http::StatusCode::NOT_FOUND
            }
        }),
    );
    let cp_shutdown = Arc::new(Notify::new());
    let task_shutdown = Arc::clone(&cp_shutdown);
    tokio::spawn(async move {
        axum::serve(cp_listener, cp_router)
            .with_graceful_shutdown(async move { task_shutdown.notified().await })
            .await
            .expect("serve ACME control-plane fixture");
    });
    let snapshot_dir = TempDir::new().expect("create route snapshot directory");
    let (ingress, ingress_shutdown) =
        spawn_acme_test_ingress(enabled_public_store(&snapshot_dir), cp_address).await;

    for index in 0..4 {
        let response = acme_request(
            ingress.http_address(),
            &format!("pending-{index}.example.test"),
            &format!("invented-token-{index}"),
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 404 Not Found"), "{response}");
    }
    let limited = acme_request(
        ingress.http_address(),
        "pending-4.example.test",
        "invented-token-4",
    )
    .await;

    assert!(
        limited.starts_with("HTTP/1.1 429 Too Many Requests"),
        "{limited}"
    );
    assert!(limited.to_ascii_lowercase().contains("retry-after: 1"));
    assert_eq!(cp_hits.load(Ordering::SeqCst), 4);
    ingress_shutdown.notify_waiters();
    cp_shutdown.notify_waiters();
}

#[tokio::test]
async fn test_public_ingress_acme_allows_only_two_concurrent_control_plane_lookups() {
    let cp_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ACME control-plane fixture");
    let cp_address = cp_listener.local_addr().expect("read ACME fixture address");
    let cp_hits = Arc::new(AtomicUsize::new(0));
    let handler_hits = Arc::clone(&cp_hits);
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let handler_release = Arc::clone(&release);
    let cp_router = axum::Router::new().route(
        "/api/internal/nodes/{node_id}/acme-challenge",
        axum::routing::get(move || {
            let handler_hits = Arc::clone(&handler_hits);
            let handler_release = Arc::clone(&handler_release);
            async move {
                handler_hits.fetch_add(1, Ordering::SeqCst);
                let _permit = handler_release
                    .acquire()
                    .await
                    .expect("release ACME lookup");
                axum::http::StatusCode::NOT_FOUND
            }
        }),
    );
    let cp_shutdown = Arc::new(Notify::new());
    let task_shutdown = Arc::clone(&cp_shutdown);
    tokio::spawn(async move {
        axum::serve(cp_listener, cp_router)
            .with_graceful_shutdown(async move { task_shutdown.notified().await })
            .await
            .expect("serve ACME control-plane fixture");
    });
    let snapshot_dir = TempDir::new().expect("create route snapshot directory");
    let (ingress, ingress_shutdown) =
        spawn_acme_test_ingress(enabled_public_store(&snapshot_dir), cp_address).await;
    let ingress_address = ingress.http_address();
    let first = tokio::spawn(acme_request(
        ingress_address,
        "first.example.test",
        "first-token",
    ));
    let second = tokio::spawn(acme_request(
        ingress_address,
        "second.example.test",
        "second-token",
    ));
    timeout(Duration::from_secs(2), async {
        while cp_hits.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("two ACME lookups did not reach the control plane");

    let saturated = acme_request(ingress_address, "third.example.test", "third-token").await;
    assert!(
        saturated.starts_with("HTTP/1.1 429 Too Many Requests"),
        "{saturated}"
    );
    assert!(saturated.to_ascii_lowercase().contains("retry-after: 1"));
    assert_eq!(cp_hits.load(Ordering::SeqCst), 2);
    release.add_permits(2);
    assert!(first.await.unwrap().starts_with("HTTP/1.1 404 Not Found"));
    assert!(second.await.unwrap().starts_with("HTTP/1.1 404 Not Found"));
    ingress_shutdown.notify_waiters();
    cp_shutdown.notify_waiters();
}

#[tokio::test]
async fn test_public_ingress_acme_rejects_control_plane_body_over_16_kib() {
    let cp_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ACME control-plane fixture");
    let cp_address = cp_listener.local_addr().expect("read ACME fixture address");
    let oversized = "x".repeat(17 * 1024);
    let cp_router = axum::Router::new().route(
        "/api/internal/nodes/{node_id}/acme-challenge",
        axum::routing::get(move || {
            let oversized = oversized.clone();
            async move { (axum::http::StatusCode::OK, oversized) }
        }),
    );
    let cp_shutdown = Arc::new(Notify::new());
    let task_shutdown = Arc::clone(&cp_shutdown);
    tokio::spawn(async move {
        axum::serve(cp_listener, cp_router)
            .with_graceful_shutdown(async move { task_shutdown.notified().await })
            .await
            .expect("serve ACME control-plane fixture");
    });
    let snapshot_dir = TempDir::new().expect("create route snapshot directory");
    let (ingress, ingress_shutdown) =
        spawn_acme_test_ingress(enabled_public_store(&snapshot_dir), cp_address).await;

    let response = acme_request(
        ingress.http_address(),
        "oversized.example.test",
        "oversized-token",
    )
    .await;

    assert!(
        response.starts_with("HTTP/1.1 502 Bad Gateway"),
        "{response}"
    );
    ingress_shutdown.notify_waiters();
    cp_shutdown.notify_waiters();
}

#[tokio::test]
async fn test_public_ingress_routes_to_docker_published_container_port() {
    let image = "nginx:alpine";
    let image_available = std::process::Command::new("docker")
        .args(["image", "inspect", image])
        .output()
        .is_ok_and(|output| output.status.success());
    if !image_available {
        eprintln!("skipping Docker ingress test: cached nginx:alpine image is unavailable");
        return;
    }

    let container_name = format!("temps-public-ingress-test-{}", uuid::Uuid::new_v4());
    let started = std::process::Command::new("docker")
        .args([
            "run",
            "-d",
            "--name",
            &container_name,
            "-p",
            "127.0.0.1::80",
            image,
        ])
        .output()
        .expect("run isolated Docker backend");
    assert!(
        started.status.success(),
        "start isolated Docker backend: {}",
        String::from_utf8_lossy(&started.stderr)
    );
    let _guard = DockerContainerGuard {
        name: container_name.clone(),
    };
    let port_output = std::process::Command::new("docker")
        .args([
            "inspect",
            "--format",
            "{{(index (index .NetworkSettings.Ports \"80/tcp\") 0).HostPort}}",
            &container_name,
        ])
        .output()
        .expect("inspect isolated Docker backend port");
    assert!(
        port_output.status.success(),
        "inspect Docker published port"
    );
    let port: u16 = String::from_utf8(port_output.stdout)
        .expect("Docker port is UTF-8")
        .trim()
        .parse()
        .expect("Docker published port is numeric");
    let backend_address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(mut backend) = TcpStream::connect(backend_address).await {
                let request = b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
                if backend.write_all(request).await.is_ok() {
                    let mut status = [0; 15];
                    if backend.read_exact(&mut status).await.is_ok()
                        && status.starts_with(b"HTTP/1.1 200")
                    {
                        break;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("Docker backend did not accept its published port");

    let ingress = IngressHarness::start(backend_address, true).await;
    let response = ingress
        .request("GET / HTTP/1.1\r\nHost: app.example.test\r\nConnection: close\r\n\r\n")
        .await;

    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    assert!(response.contains("Welcome to nginx!"), "{response}");
}

#[tokio::test]
async fn test_public_ingress_streams_response_before_backend_finishes() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind streaming backend");
    let backend_address = listener
        .local_addr()
        .expect("read streaming backend address");
    let release_tail = Arc::new(Notify::new());
    let release_tail_task = Arc::clone(&release_tail);
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept ingress request");
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while request.len() < 8192 && !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let count = stream
                .read(&mut buffer)
                .await
                .expect("read ingress request headers");
            assert!(count > 0, "ingress request ended before headers completed");
            request.extend_from_slice(&buffer[..count]);
        }
        assert!(
            request.windows(4).any(|window| window == b"\r\n\r\n"),
            "ingress request headers exceeded the test backend limit"
        );
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\nfirst-")
            .await
            .expect("write first response segment");
        release_tail_task.notified().await;
        stream
            .write_all(b"second")
            .await
            .expect("write final response segment");
    });
    let ingress = IngressHarness::start(backend_address, true).await;
    let mut client = TcpStream::connect(ingress.address)
        .await
        .expect("connect to public ingress");
    client
        .write_all(b"GET /stream HTTP/1.1\r\nHost: app.example.test\r\nConnection: close\r\n\r\n")
        .await
        .expect("write streaming request");

    let mut received = Vec::new();
    let mut buffer = [0; 1024];
    timeout(Duration::from_secs(1), async {
        while !String::from_utf8_lossy(&received).contains("first-") {
            let count = client
                .read(&mut buffer)
                .await
                .expect("read streamed segment");
            assert!(
                count > 0,
                "ingress closed before streaming the first segment"
            );
            received.extend_from_slice(&buffer[..count]);
        }
    })
    .await
    .expect("ingress buffered the response until the backend completed");
    assert!(!String::from_utf8_lossy(&received).contains("second"));

    release_tail.notify_waiters();
    timeout(Duration::from_secs(1), client.read_to_end(&mut received))
        .await
        .expect("stream tail timed out")
        .expect("read stream tail");
    assert!(String::from_utf8_lossy(&received).contains("first-second"));
}

#[tokio::test]
async fn test_public_ingress_websocket_upgrade_tunnels_bidirectionally() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind upgrade backend");
    let backend_address = listener.local_addr().expect("read upgrade backend address");
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept upgrade request");
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let count = stream
                .read(&mut buffer)
                .await
                .expect("read upgrade request");
            assert!(count > 0, "upgrade request closed before headers completed");
            request.extend_from_slice(&buffer[..count]);
        }
        let request = String::from_utf8(request)
            .expect("upgrade request is UTF-8")
            .to_ascii_lowercase();
        assert_eq!(
            request
                .lines()
                .filter(|line| line.starts_with("host:"))
                .collect::<Vec<_>>(),
            vec!["host: app.example.test:8443"]
        );
        assert!(
            request.contains("x-forwarded-host: app.example.test:8443"),
            "{request}"
        );
        assert!(request.contains("x-forwarded-for: 127.0.0.1"), "{request}");
        assert!(!request.contains("203.0.113.99"), "{request}");
        stream
            .write_all(
                b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n",
            )
            .await
            .expect("accept websocket upgrade");
        let mut payload = [0; 4];
        stream
            .read_exact(&mut payload)
            .await
            .expect("read tunneled client payload");
        stream
            .write_all(&payload)
            .await
            .expect("echo tunneled backend payload");
    });
    let ingress = IngressHarness::start(backend_address, true).await;
    let mut client = TcpStream::connect(ingress.address)
        .await
        .expect("connect to public ingress");
    client
        .write_all(
            b"GET /socket HTTP/1.1\r\nHost: app.example.test:8443\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGVzdC1vbmx5LWtleQ==\r\nX-Forwarded-For: 203.0.113.99\r\nX-Real-IP: 203.0.113.99\r\n\r\n",
        )
        .await
        .expect("write websocket upgrade request");

    let mut response = Vec::new();
    let mut byte = [0; 1];
    timeout(Duration::from_secs(2), async {
        while !response.ends_with(b"\r\n\r\n") {
            client
                .read_exact(&mut byte)
                .await
                .expect("read websocket upgrade response");
            response.push(byte[0]);
        }
    })
    .await
    .expect("websocket upgrade response timed out");
    assert!(
        String::from_utf8_lossy(&response).starts_with("HTTP/1.1 101 Switching Protocols"),
        "{}",
        String::from_utf8_lossy(&response)
    );

    client
        .write_all(b"ping")
        .await
        .expect("write tunneled payload");
    let mut echoed = [0; 4];
    timeout(Duration::from_secs(1), client.read_exact(&mut echoed))
        .await
        .expect("websocket echo timed out")
        .expect("read tunneled echo");
    assert_eq!(&echoed, b"ping");
}
