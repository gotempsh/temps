// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Real TLS and HTTP across the guest Unix attachment. The upstream is a
//! controlled authenticated fixture, not a live third-party service.
use super::*;
use rustls::{
    pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName},
    ClientConfig, RootCertStore, ServerConfig,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokenless_control::service::policy::UpdateRule;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, UnixStream},
    task::{JoinHandle, JoinSet},
};
use tokio_rustls::{TlsAcceptor, TlsConnector};

struct Upstream {
    host: String,
    cert: CertificateDer<'static>,
    calls: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}
impl Drop for Upstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn head(stream: &mut (impl AsyncRead + Unpin)) -> String {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut bytes = Vec::new();
        while bytes.len() < 8192 && !bytes.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            match stream.read(&mut byte).await {
                Ok(1) => bytes.push(byte[0]),
                Ok(_) | Err(_) => break,
            }
        }
        String::from_utf8(bytes).unwrap()
    })
    .await
    .expect("HTTP header must complete within five seconds")
}

impl Upstream {
    async fn start() -> Self {
        Self::with_authorization("Bearer test-credential".into()).await
    }

    async fn with_authorization(authorization: String) -> Self {
        let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        let der = cert.cert.der().clone();
        let config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(
                    vec![der.clone()],
                    PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()).into(),
                )
                .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host = listener.local_addr().unwrap().to_string();
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        let task = tokio::spawn(async move {
            let mut requests = JoinSet::new();
            loop {
                tokio::select! {
                    _ = requests.join_next(), if !requests.is_empty() => {},
                    accepted = listener.accept() => {
                        let (tcp, _) = accepted.unwrap();
                        assert!(requests.len() < 8, "fixture admission is bounded");
                        let acceptor = acceptor.clone();
                        let seen = Arc::clone(&seen);
                        let expected = authorization.clone();
                        requests.spawn(async move {
                            let mut tls = tokio::time::timeout(Duration::from_secs(5), acceptor.accept(tcp)).await.unwrap().unwrap();
                            let request = head(&mut tls).await;
                            seen.fetch_add(1, Ordering::SeqCst);
                            let authenticated = request.lines().filter_map(|line| line.split_once(':'))
                                .any(|(name, value)| name.eq_ignore_ascii_case("authorization") && value.trim() == expected);
                            let response = if authenticated { "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok" }
                                else { "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n" };
                            tls.write_all(response.as_bytes()).await.unwrap();
                            let _ = tls.shutdown().await;
                        });
                    }
                }
            }
        });
        Self {
            host,
            cert: der,
            calls,
            task,
        }
    }
}

fn guest_tls(f: &Fixture) -> TlsConnector {
    let pem = f.control.nodes.interception_ca_pem();
    let mut roots = RootCertStore::empty();
    for cert in rustls_pemfile::certs(&mut pem.as_bytes()) {
        roots.add(cert.unwrap()).unwrap();
    }
    let client =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
    TlsConnector::from(Arc::new(client))
}

async fn request(f: &Fixture, attachment: &UnixAttachment, host: &str, path: &str) -> String {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut stream = UnixStream::connect(attachment.directory().join("proxy.sock")).await.unwrap();
        if stream.write_all(format!("CONNECT {host} HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes()).await.is_err() { return String::new(); }
        let response = head(&mut stream).await;
        if !response.starts_with("HTTP/1.1 200") { return response; }
        let Ok(mut tls) = guest_tls(f).connect(ServerName::try_from("127.0.0.1").unwrap(), stream).await else { return String::new(); };
        // No provider token or session is given to the guest. A forged client
        // authorization header must be replaced by the host's credential.
        if tls.write_all(format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer guest-forgery\r\nConnection: close\r\n\r\n").as_bytes()).await.is_err() { return String::new(); }
        head(&mut tls).await
    }).await.expect("guest HTTPS request must finish")
}

async fn fixture(upstream: &Upstream) -> Fixture {
    let f = Fixture::with_transport(
        30,
        &upstream.host,
        DataPlaneConfig {
            use_public_roots: false,
            extra_upstream_roots: vec![upstream.cert.clone()],
            ..Default::default()
        },
        true,
    )
    .await;
    f.control
        .policy
        .update_rule(
            &f.caller,
            &f.rule,
            UpdateRule {
                paths: Some(vec!["/allowed/*".into()]),
                methods: Some(vec!["GET".into()]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    f
}

async fn new_connection_is_denied(attachment: &UnixAttachment) {
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        UnixStream::connect(attachment.directory().join("proxy.sock")),
    )
    .await
    .expect("revoked endpoint must fail promptly");
    match result {
        Ok(guest) => assert_guest_closed(guest).await,
        // A stopped Unix listener can reject connect itself on Linux. That is
        // the same fail-closed contract as accepting and immediately closing.
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::ConnectionReset
            ) => {}
        Err(error) => panic!("unexpected attachment error: {error}"),
    }
}

#[tokio::test]
async fn https_grant_injects_a_host_credential_and_revoke_preserves_another_sandbox() {
    let upstream = Upstream::start().await;
    let f = fixture(&upstream).await;
    let token = f.mint(true).await;
    let version = f.install().await;
    let binding = f.proxy.bind(f.spec(token), version).await.unwrap();
    let root = tempfile::tempdir().unwrap();
    let a = UnixAttachment::reserve(root.path(), binding.id().into()).unwrap();
    // Before approval the endpoint accepts no traffic, even for a known host.
    new_connection_is_denied(&a).await;
    assert_eq!(upstream.calls.load(Ordering::SeqCst), 0);
    a.activate(binding).unwrap();
    assert!(request(&f, &a, &upstream.host, "/allowed/health")
        .await
        .starts_with("HTTP/1.1 200"));
    assert_eq!(upstream.calls.load(Ordering::SeqCst), 1);
    assert!(request(&f, &a, &upstream.host, "/forbidden")
        .await
        .starts_with("HTTP/1.1 403"));
    assert_eq!(upstream.calls.load(Ordering::SeqCst), 1);
    let other_id = "sandbox:independent-incarnation";
    let token = f
        .control
        .sessions
        .mint(
            &f.caller,
            &f.vault,
            MintSession {
                rule_ids: vec![f.rule],
                node_id: Some(f.node.id),
                binding: Some(SessionBinding {
                    id: other_id.into(),
                    generation: 1,
                }),
                strict: true,
                ttl_secs: Some(60),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .token;
    let mut spec = f.spec(token);
    spec.binding_id = other_id.into();
    let binding = f.proxy.bind(spec, f.install().await).await.unwrap();
    let b = UnixAttachment::bind(root.path(), binding).unwrap();
    a.revoke_and_wait(Duration::from_secs(2)).await.unwrap();
    new_connection_is_denied(&a).await;
    assert!(request(&f, &b, &upstream.host, "/allowed/health")
        .await
        .starts_with("HTTP/1.1 200"));
    assert_eq!(upstream.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn independent_service_deadlines_stop_only_the_expired_service() {
    let first = Upstream::start().await;
    let second = Upstream::start().await;
    let f = Fixture::with_transport(
        30,
        &first.host,
        DataPlaneConfig {
            use_public_roots: false,
            extra_upstream_roots: vec![first.cert.clone(), second.cert.clone()],
            ..Default::default()
        },
        true,
    )
    .await;
    let credential = f
        .control
        .policy
        .create_credential(
            &f.caller,
            &f.vault,
            CreateCredential {
                name: "second-service".into(),
                template_id: "generic-bearer".into(),
                template_version: None,
                host_override: vec![second.host.clone()],
                fields: [("token".into(), "test-credential".into())].into(),
                custom_headers: vec![],
                substitutions: vec![],
                not_after: None,
            },
        )
        .await
        .unwrap();
    let second_rule = f
        .control
        .policy
        .create_rule(
            &f.caller,
            &f.vault,
            CreateRule {
                name: "second-rule".into(),
                credential_id: credential.id,
                hosts: None,
                paths: None,
                methods: None,
                mcp_tools: None,
                git: None,
            },
        )
        .await
        .unwrap()
        .id;
    let token = f.mint(true).await;
    let binding = f
        .proxy
        .bind(f.spec(token), f.install().await)
        .await
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let attachment = UnixAttachment::bind(root.path(), Arc::clone(&binding)).unwrap();
    // Knowing the second service's address never grants access to it.
    assert!(request(&f, &attachment, &second.host, "/health")
        .await
        .starts_with("HTTP/1.1 403"));
    assert_eq!(second.calls.load(Ordering::SeqCst), 0);
    let deadline = Utc::now() + chrono::Duration::seconds(3);
    let token = f
        .control
        .sessions
        .mint(
            &f.caller,
            &f.vault,
            MintSession {
                rule_ids: vec![f.rule, second_rule],
                node_id: Some(f.node.id),
                binding: Some(SessionBinding {
                    id: binding.id().into(),
                    generation: 1,
                }),
                strict: true,
                ttl_secs: Some(60),
                rule_not_after: [(f.rule, deadline)].into(),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .token;
    f.install().await;
    f.proxy
        .update(
            &binding,
            BindingUpdate {
                version: 2,
                sessions: vec![token],
            },
        )
        .await
        .unwrap();
    assert!(request(&f, &attachment, &first.host, "/health")
        .await
        .starts_with("HTTP/1.1 200"));
    assert!(request(&f, &attachment, &second.host, "/health")
        .await
        .starts_with("HTTP/1.1 200"));
    tokio::time::sleep(Duration::from_secs(4)).await;
    let expired = request(&f, &attachment, &first.host, "/health").await;
    assert!(
        expired.is_empty() || expired.starts_with("HTTP/1.1 403"),
        "expired grant must be denied: {expired}"
    );
    assert_eq!(
        first.calls.load(Ordering::SeqCst),
        1,
        "expired service must not reach upstream"
    );
    assert!(request(&f, &attachment, &second.host, "/health")
        .await
        .starts_with("HTTP/1.1 200"));
    attachment
        .revoke_and_wait(Duration::from_secs(2))
        .await
        .unwrap();
    new_connection_is_denied(&attachment).await;
    assert_eq!(second.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn git_smart_http_injects_basic_auth_only_for_the_granted_repository_and_operation() {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use tokenless_core::git::GitScope;
    let upstream = Upstream::with_authorization(format!(
        "Basic {}",
        STANDARD.encode("x-access-token:test-credential")
    ))
    .await;
    let mut f = fixture(&upstream).await;
    // Controlled GitHub-style authentication fixture. The built-in GitHub
    // template intentionally cannot be redirected to an arbitrary host.
    let yaml = format!(
        r#"
id: fixture-git
version: 1
name: Fixture Git
hosts: ["{}"]
allow:
  paths: ["/*"]
  methods: ["GET", "POST"]
credential_fields:
  - name: token
    label: Token
injection:
  method: basic_token
  username: x-access-token
  password_field: token
"#,
        upstream.host
    );
    f.control
        .policy
        .import_template(&f.caller, &yaml)
        .await
        .unwrap();
    f.control
        .policy
        .approve_template(&f.caller, "fixture-git", 1)
        .await
        .unwrap();
    let credential = f
        .control
        .policy
        .create_credential(
            &f.caller,
            &f.vault,
            CreateCredential {
                name: "fixture-git".into(),
                template_id: "fixture-git".into(),
                template_version: None,
                host_override: vec![],
                fields: [("token".into(), "test-credential".into())].into(),
                custom_headers: vec![],
                substitutions: vec![],
                not_after: None,
            },
        )
        .await
        .unwrap();
    f.rule = f
        .control
        .policy
        .create_rule(
            &f.caller,
            &f.vault,
            CreateRule {
                name: "repository-read".into(),
                credential_id: credential.id,
                hosts: None,
                paths: None,
                methods: None,
                mcp_tools: None,
                git: Some(GitScope::new(vec!["owner/private-repo".into()], false).unwrap()),
            },
        )
        .await
        .unwrap()
        .id;
    let token = f.mint(true).await;
    let binding = f
        .proxy
        .bind(f.spec(token), f.install().await)
        .await
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let attachment = UnixAttachment::bind(root.path(), binding).unwrap();
    assert!(request(
        &f,
        &attachment,
        &upstream.host,
        "/owner/private-repo.git/info/refs?service=git-upload-pack"
    )
    .await
    .starts_with("HTTP/1.1 200"));
    for path in [
        "/owner/another-repo.git/info/refs?service=git-upload-pack",
        "/owner/private-repo.git/info/refs?service=git-receive-pack",
        "/owner/private-repo.git/info/refs?service=git-upload-pack&service=git-receive-pack",
        "/repos/owner/private-repo",
    ] {
        assert!(
            request(&f, &attachment, &upstream.host, path)
                .await
                .starts_with("HTTP/1.1 403"),
            "must deny {path}"
        );
    }
    assert_eq!(
        upstream.calls.load(Ordering::SeqCst),
        1,
        "denied Git operations must not reach upstream"
    );
    attachment
        .revoke_and_wait(Duration::from_secs(2))
        .await
        .unwrap();
    new_connection_is_denied(&attachment).await;
}
