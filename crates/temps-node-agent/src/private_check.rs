// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The worker's inside check of its private addresses (ADR 064 D7).
//!
//! The control plane cannot reach a tailnet, so the worker checks for it,
//! and the check is bound to the listener recorded for the address, not to
//! whatever DNS says:
//!
//! - it works only from the private routes in this worker's own snapshot:
//!   the control plane names an address by id, never a host, address or URL;
//! - it resolves the hostname through this worker's resolver and compares
//!   the answer with the recorded listener; any other answer is reported
//!   and nothing is dialled;
//! - it connects only to the recorded listener, port 443, with the hostname
//!   as TLS SNI and `Host`, verifies the certificate for that hostname, and
//!   follows no redirects.
//!
//! The results go to the control plane in the listener report.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use async_trait::async_trait;
use temps_core::worker_ingress::{PrivateCheck, PrivateCheckOutcome, PRIVATE_HTTPS_PORT};

use crate::internal_proxy::PRIVATE_REFUSAL_HEADER;
use crate::route_store::PrivateRouteEntry;

/// Most addresses checked in one pass.
const MAX_CHECKS: usize = 64;
const CHECK_TIMEOUT: Duration = Duration::from_secs(10);

/// How the check resolves a hostname.
#[async_trait]
pub trait Resolve: Send + Sync {
    async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, String>;
}

/// This worker's own resolver.
pub struct SystemResolver;

#[async_trait]
impl Resolve for SystemResolver {
    async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, String> {
        let answers = tokio::time::timeout(
            CHECK_TIMEOUT,
            tokio::net::lookup_host((host, PRIVATE_HTTPS_PORT)),
        )
        .await
        .map_err(|_| "the lookup timed out".to_string())?
        .map_err(|error| error.to_string())?;
        let mut ips: Vec<IpAddr> = answers.map(|address| address.ip()).collect();
        ips.sort();
        ips.dedup();
        Ok(ips)
    }
}

/// Check every private address in the snapshot.
pub async fn check_all(
    routes: &[PrivateRouteEntry],
    root: Option<reqwest::Certificate>,
) -> Vec<PrivateCheck> {
    let mut out = Vec::with_capacity(routes.len().min(MAX_CHECKS));
    for entry in routes.iter().take(MAX_CHECKS) {
        out.push(check(entry, &SystemResolver, root.clone(), PRIVATE_HTTPS_PORT).await);
    }
    out
}

/// Check one private address. `port` is always 443 outside tests; `root`
/// replaces the public roots in tests only.
pub async fn check(
    entry: &PrivateRouteEntry,
    resolver: &dyn Resolve,
    root: Option<reqwest::Certificate>,
    port: u16,
) -> PrivateCheck {
    let host = entry.route.host.trim_end_matches('.').to_ascii_lowercase();
    let result = |outcome, message: String, status| PrivateCheck {
        address_id: entry.address_id.clone(),
        hostname: host.clone(),
        listener: entry.listener.clone(),
        outcome,
        message,
        status,
    };
    let Ok(listener) = entry.listener.parse::<IpAddr>() else {
        return result(
            PrivateCheckOutcome::NotListening,
            format!(
                "The listener recorded for {host} ({}) is not an address. Make it reachable \
                 again.",
                entry.listener
            ),
            None,
        );
    };
    let answers = match resolver.resolve(&host).await {
        Ok(answers) if !answers.is_empty() => answers,
        Ok(_) => {
            return result(
                PrivateCheckOutcome::NotResolved,
                format!(
                    "{host} does not resolve from this worker. Check its A record points at \
                     {listener}, or wait for DNS to update."
                ),
                None,
            )
        }
        Err(error) => {
            return result(
                PrivateCheckOutcome::NotResolved,
                format!(
                    "{host} does not resolve from this worker ({error}). Check its A record \
                     points at {listener}, or wait for DNS to update."
                ),
                None,
            )
        }
    };
    if answers.iter().any(|ip| *ip != listener) {
        let shown: Vec<String> = answers.iter().map(ToString::to_string).collect();
        return result(
            PrivateCheckOutcome::ResolvesElsewhere,
            format!(
                "{host} resolves to {}, not this worker's private listener {listener}. Nothing \
                 was dialled. Point its DNS record only at {listener}.",
                shown.join(", ")
            ),
            None,
        );
    }

    let mut client = reqwest::Client::builder()
        // The recorded listener only, whatever DNS says next time.
        .resolve(&host, SocketAddr::new(listener, port))
        .redirect(reqwest::redirect::Policy::none())
        .https_only(true)
        .timeout(CHECK_TIMEOUT)
        .no_proxy();
    if let Some(root) = root {
        client = client
            .tls_built_in_root_certs(false)
            .add_root_certificate(root);
    }
    let client = match client.build() {
        Ok(client) => client,
        Err(error) => {
            return result(
                PrivateCheckOutcome::NotListening,
                format!("The check could not start: {error}"),
                None,
            )
        }
    };
    let url = if port == PRIVATE_HTTPS_PORT {
        format!("https://{host}/")
    } else {
        format!("https://{host}:{port}/")
    };
    match client.get(&url).send().await {
        Ok(response) => {
            let status = response.status().as_u16();
            let refusal = response
                .headers()
                .get(PRIVATE_REFUSAL_HEADER)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            match refusal.as_deref() {
                Some("no-route") => result(
                    PrivateCheckOutcome::NoRoute,
                    format!(
                        "This worker's private listener has no route for {host} yet. It gets \
                         one with the next route sync; if this lasts, make it reachable again."
                    ),
                    Some(status),
                ),
                Some(other) => result(
                    PrivateCheckOutcome::NoRoute,
                    format!(
                        "This worker's private listener refused {host} ({other}): its range \
                         does not include {listener}."
                    ),
                    Some(status),
                ),
                None => result(
                    PrivateCheckOutcome::Reachable,
                    format!(
                        "{host} answers on {listener} port {port} with a valid certificate (HTTP \
                         {status})."
                    ),
                    Some(status),
                ),
            }
        }
        Err(error) => {
            let detail = error_chain(&error);
            let lower = detail.to_ascii_lowercase();
            // Nothing took the connection: refused, unreachable or timed
            // out. Anything else failing while connecting is the TLS
            // handshake, whose wording differs per TLS library.
            let unanswered = error.is_timeout() || refused(&error);
            if !unanswered
                && (lower.contains("certificate")
                    || lower.contains("unknownissuer")
                    || lower.contains("notvalidforname")
                    || lower.contains("mismatch")
                    || lower.contains("verif")
                    || lower.contains("trust")
                    || lower.contains("handshake")
                    || lower.contains("tls")
                    || lower.contains("ssl"))
            {
                result(
                    PrivateCheckOutcome::Certificate,
                    format!(
                        "{listener} did not present a valid certificate for {host} ({detail}). \
                         If it was just issued, it reaches this worker with the next route \
                         sync."
                    ),
                    None,
                )
            } else {
                result(
                    PrivateCheckOutcome::NotListening,
                    format!(
                        "Nothing answers on {listener} port {port} ({detail}). Run `temps agent \
                         ingress status` on this worker."
                    ),
                    None,
                )
            }
        }
    }
}

/// Whether the connection itself was refused or could not be made.
fn refused(error: &(dyn std::error::Error + 'static)) -> bool {
    use std::io::ErrorKind;
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(e) = current {
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            if matches!(
                io.kind(),
                ErrorKind::ConnectionRefused
                    | ErrorKind::TimedOut
                    | ErrorKind::HostUnreachable
                    | ErrorKind::NetworkUnreachable
                    | ErrorKind::AddrNotAvailable
            ) {
                return true;
            }
        }
        current = e.source();
    }
    false
}

fn error_chain(error: &dyn std::error::Error) -> String {
    let mut parts = vec![error.to_string()];
    let mut source = error.source();
    while let Some(next) = source {
        let text = next.to_string();
        if !parts.contains(&text) {
            parts.push(text);
        }
        source = next.source();
    }
    parts.join(": ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct Fixed(Vec<IpAddr>);

    #[async_trait]
    impl Resolve for Fixed {
        async fn resolve(&self, _host: &str) -> Result<Vec<IpAddr>, String> {
            Ok(self.0.clone())
        }
    }

    fn entry(host: &str, listener: &str) -> PrivateRouteEntry {
        PrivateRouteEntry {
            route: crate::route_store::RouteEntry {
                host: host.to_string(),
                backends: Vec::new(),
                deployment_id: None,
                project_id: None,
                environment_id: None,
                gate: None,
                shield: None,
            },
            address_id: "custom:7".into(),
            range: "127.0.0.0/8".into(),
            listener: listener.into(),
            principals: None,
        }
    }

    /// What the TLS server saw: connections, SNI and the request head.
    #[derive(Default)]
    struct Seen {
        connections: AtomicUsize,
        sni: Mutex<Option<String>>,
        request: Mutex<String>,
    }

    /// A TLS listener on loopback with a certificate for `cert_for`,
    /// answering every request with a redirect elsewhere.
    async fn tls_server(cert_for: &str) -> (u16, reqwest::Certificate, Arc<Seen>) {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec![cert_for.to_string()]).unwrap();
        let root = reqwest::Certificate::from_pem(cert.pem().as_bytes()).unwrap();
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                rustls::pki_types::PrivateKeyDer::Pkcs8(signing_key.serialize_der().into()),
            )
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Seen::default());
        let state = Arc::clone(&seen);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                state.connections.fetch_add(1, Ordering::SeqCst);
                let Ok(mut tls) = acceptor.accept(stream).await else {
                    continue;
                };
                *state.sni.lock().unwrap() = tls.get_ref().1.server_name().map(str::to_string);
                let mut buffer = vec![0u8; 4096];
                let read = tls.read(&mut buffer).await.unwrap_or(0);
                *state.request.lock().unwrap() =
                    String::from_utf8_lossy(&buffer[..read]).to_string();
                let _ = tls
                    .write_all(
                        b"HTTP/1.1 301 Moved Permanently\r\nLocation: https://elsewhere.test/\r\n\
                          Content-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await;
                let _ = tls.shutdown().await;
            }
        });
        (port, root, seen)
    }

    #[tokio::test]
    async fn a_hostname_resolving_elsewhere_is_reported_and_nothing_is_dialled() {
        let (port, root, seen) = tls_server("myapp.internal.test").await;
        let checked = check(
            &entry("myapp.internal.test", "127.0.0.1"),
            &Fixed(vec!["10.0.0.9".parse().unwrap()]),
            Some(root.clone()),
            port,
        )
        .await;
        assert_eq!(checked.outcome, PrivateCheckOutcome::ResolvesElsewhere);
        assert!(checked.message.contains("10.0.0.9"), "{}", checked.message);
        assert!(checked.message.contains("Nothing was dialled"));
        // An extra answer next to the listener is a mismatch too.
        let checked = check(
            &entry("myapp.internal.test", "127.0.0.1"),
            &Fixed(vec![
                "127.0.0.1".parse().unwrap(),
                "10.0.0.9".parse().unwrap(),
            ]),
            Some(root),
            port,
        )
        .await;
        assert_eq!(checked.outcome, PrivateCheckOutcome::ResolvesElsewhere);
        assert_eq!(seen.connections.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn the_check_dials_the_listener_with_sni_and_host_and_follows_no_redirect() {
        let (port, root, seen) = tls_server("myapp.internal.test").await;
        let checked = check(
            &entry("myapp.internal.test", "127.0.0.1"),
            &Fixed(vec!["127.0.0.1".parse().unwrap()]),
            Some(root),
            port,
        )
        .await;
        assert_eq!(
            checked.outcome,
            PrivateCheckOutcome::Reachable,
            "{}",
            checked.message
        );
        assert_eq!(checked.status, Some(301), "the redirect is not followed");
        assert_eq!(seen.connections.load(Ordering::SeqCst), 1);
        assert_eq!(
            seen.sni.lock().unwrap().as_deref(),
            Some("myapp.internal.test")
        );
        let request = seen.request.lock().unwrap().to_ascii_lowercase();
        assert!(
            request.contains("\r\nhost: myapp.internal.test"),
            "{request}"
        );
    }

    #[tokio::test]
    async fn a_certificate_for_another_name_fails_the_check() {
        let (port, root, _) = tls_server("other.internal.test").await;
        let checked = check(
            &entry("myapp.internal.test", "127.0.0.1"),
            &Fixed(vec!["127.0.0.1".parse().unwrap()]),
            Some(root),
            port,
        )
        .await;
        assert_eq!(
            checked.outcome,
            PrivateCheckOutcome::Certificate,
            "{}",
            checked.message
        );
    }

    #[tokio::test]
    async fn an_unresolved_hostname_says_what_to_check() {
        let checked = check(
            &entry("myapp.internal.test", "100.101.102.103"),
            &Fixed(Vec::new()),
            None,
            PRIVATE_HTTPS_PORT,
        )
        .await;
        assert_eq!(checked.outcome, PrivateCheckOutcome::NotResolved);
        assert!(checked.message.contains("100.101.102.103"));
    }

    #[tokio::test]
    async fn a_closed_port_is_not_listening() {
        // Bind and drop: nothing listens there any more.
        let port = {
            let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            socket.local_addr().unwrap().port()
        };
        let checked = check(
            &entry("myapp.internal.test", "127.0.0.1"),
            &Fixed(vec!["127.0.0.1".parse().unwrap()]),
            None,
            port,
        )
        .await;
        assert_eq!(
            checked.outcome,
            PrivateCheckOutcome::NotListening,
            "{}",
            checked.message
        );
    }
}
