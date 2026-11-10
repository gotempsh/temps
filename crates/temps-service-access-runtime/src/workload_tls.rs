// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Host-owned workload credentials. Each binding has an exclusive trust root;
//! guest input cannot select an identity or obtain its private key.
use crate::RuntimeError;
use rustls::{
    pki_types::{PrivatePkcs8KeyDer, ServerName},
    ClientConfig, RootCertStore, ServerConfig,
};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::{TlsAcceptor, TlsConnector};

pub(crate) struct WorkloadTls {
    pub(crate) connector: TlsConnector,
    acceptor: TlsAcceptor,
    #[cfg(test)]
    client_config: ClientConfig,
    client_certificate: rustls::pki_types::CertificateDer<'static>,
    identity: String,
    renew_at: tokio::time::Instant,
}

impl WorkloadTls {
    pub(crate) fn new(identity: &str) -> Result<Self, RuntimeError> {
        let certificate_error = |source| RuntimeError::WorkloadCertificate {
            binding: identity.into(),
            source,
        };
        let tls_error = |source| RuntimeError::WorkloadTls {
            binding: identity.into(),
            source,
        };
        let mut ca_params =
            rcgen::CertificateParams::new(Vec::<String>::new()).map_err(certificate_error)?;
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
        ca_params.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign];
        let now = time::OffsetDateTime::now_utc();
        ca_params.not_before = now - time::Duration::minutes(1);
        ca_params.not_after = now + time::Duration::days(2);
        let ca_key = rcgen::KeyPair::generate().map_err(certificate_error)?;
        let ca = ca_params.self_signed(&ca_key).map_err(certificate_error)?;
        let issuer = rcgen::Issuer::new(ca_params, ca_key);
        let leaf = |name: &str, purpose| -> Result<_, RuntimeError> {
            let mut params = rcgen::CertificateParams::new(vec!["127.0.0.1".into()])
                .map_err(certificate_error)?;
            params
                .distinguished_name
                .push(rcgen::DnType::CommonName, name);
            params.not_before = now - time::Duration::minutes(1);
            params.not_after = now + time::Duration::days(1);
            params.extended_key_usages = vec![purpose];
            params.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
            let key = rcgen::KeyPair::generate().map_err(certificate_error)?;
            let cert = params.signed_by(&key, &issuer).map_err(certificate_error)?;
            Ok((
                cert.der().clone(),
                PrivatePkcs8KeyDer::from(key.serialize_der()),
            ))
        };
        let (client_certificate, client_key) =
            leaf(identity, rcgen::ExtendedKeyUsagePurpose::ClientAuth)?;
        let (server_certificate, server_key) = leaf(
            "Temps private proxy",
            rcgen::ExtendedKeyUsagePurpose::ServerAuth,
        )?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut roots = RootCertStore::empty();
        roots.add(ca.der().clone()).map_err(tls_error)?;
        let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
            Arc::new(roots.clone()),
            provider.clone(),
        )
        .build()
        .map_err(|_| RuntimeError::Invalid {
            operation: "configure workload verifier",
            binding: identity.into(),
            reason: "invalid workload trust root",
        })?;
        let server = ServerConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(tls_error)?
            .with_client_cert_verifier(verifier)
            .with_single_cert(vec![server_certificate], server_key.into())
            .map_err(tls_error)?;
        let client = ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(tls_error)?
            .with_root_certificates(roots)
            .with_client_auth_cert(vec![client_certificate.clone()], client_key.into())
            .map_err(tls_error)?;
        Ok(Self {
            #[cfg(test)]
            client_config: client.clone(),
            connector: TlsConnector::from(Arc::new(client)),
            acceptor: TlsAcceptor::from(Arc::new(server)),
            client_certificate,
            identity: identity.into(),
            renew_at: tokio::time::Instant::now() + std::time::Duration::from_secs(23 * 3600),
        })
    }

    pub(crate) async fn accept<S>(
        &self,
        stream: S,
    ) -> Result<tokio_rustls::server::TlsStream<S>, RuntimeError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let invalid = |reason| RuntimeError::Invalid {
            operation: "authenticate workload",
            binding: self.identity.clone(),
            reason,
        };
        let tls = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.acceptor.accept(stream),
        )
        .await
        .map_err(|_| invalid("TLS handshake timed out"))?
        .map_err(|source| RuntimeError::Attachment {
            operation: "authenticate workload",
            binding: self.identity.clone(),
            source,
        })?;
        if tls
            .get_ref()
            .1
            .peer_certificates()
            .and_then(|chain| chain.first())
            != Some(&self.client_certificate)
        {
            return Err(invalid("certificate does not match the bound workload key"));
        }
        Ok(tls)
    }

    pub(crate) fn needs_renewal(&self) -> bool {
        tokio::time::Instant::now() >= self.renew_at
    }

    pub(crate) fn server_name() -> ServerName<'static> {
        ServerName::IpAddress(std::net::Ipv4Addr::LOCALHOST.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn exact_workload_key_can_send_data() {
        let identity = WorkloadTls::new("sandbox:one-incarnation").unwrap();
        let (client, server) = tokio::io::duplex(16384);
        let (client, server) = tokio::join!(
            identity
                .connector
                .connect(WorkloadTls::server_name(), client),
            identity.accept(server)
        );
        let mut client = client.unwrap();
        let mut server = server.unwrap();
        client.write_all(b"allowed").await.unwrap();
        let mut bytes = [0; 7];
        server.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"allowed");
    }

    #[tokio::test]
    async fn same_identity_name_with_another_key_is_rejected() {
        let identity = WorkloadTls::new("sandbox:same-name").unwrap();
        let impostor = WorkloadTls::new("sandbox:same-name").unwrap();
        let mut config = identity.client_config.clone();
        config.client_auth_cert_resolver = impostor.client_config.client_auth_cert_resolver.clone();
        let impostor_connector = TlsConnector::from(Arc::new(config));
        let (client, server) = tokio::io::duplex(16384);
        let (_, result) = tokio::join!(
            impostor_connector.connect(WorkloadTls::server_name(), client),
            identity.accept(server)
        );
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn plaintext_session_identifier_is_not_authentication() {
        let identity = WorkloadTls::new("sandbox:one").unwrap();
        let (mut client, server) = tokio::io::duplex(1024);
        client
            .write_all(b"GET /session-known HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        client.shutdown().await.unwrap();
        assert!(identity.accept(server).await.is_err());
    }
}
