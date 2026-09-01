// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Edge TLS: in-memory certificate store and Pingora TLS callback.
//!
//! Certificates are decrypted from ECIES bundles received during route sync
//! and stored in memory. The `EdgeTlsAccept` callback serves them to Pingora
//! during TLS handshakes based on SNI hostname (with wildcard fallback).

use async_trait::async_trait;
use pingora_core::listeners::TlsAccept;
use pingora_core::protocols::tls::TlsRef;
use pingora_openssl::pkey::PKey;
use pingora_openssl::ssl::NameType;
use pingora_openssl::x509::X509;
use rustls_pemfile;
use std::collections::HashMap;
use std::io::BufReader;
use std::sync::{Arc, RwLock};
use tracing::{debug, warn};

use crate::route_table::{EdgeCertBundle, EdgeCertificates};

/// Parsed certificate data ready for Pingora's OpenSSL callback.
struct CertEntry {
    /// DER-encoded certificate chain
    certs_der: Vec<Vec<u8>>,
    /// DER-encoded private key
    key_der: Vec<u8>,
    /// SHA-256 fingerprint for change detection
    fingerprint: String,
}

/// Thread-safe in-memory certificate store for the edge proxy.
///
/// Certificates live only in memory — never written to disk on the edge node.
/// Updated atomically during each route sync cycle.
pub struct EdgeCertStore {
    inner: RwLock<HashMap<String, CertEntry>>,
}

impl Default for EdgeCertStore {
    fn default() -> Self {
        Self::new()
    }
}

impl EdgeCertStore {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }

    /// Update certificates from decrypted ECIES bundles.
    ///
    /// Only re-parses certificates whose fingerprint has changed.
    /// `edge_private_key_b64` is the edge node's X25519 private key.
    pub fn update_from_sync(
        &self,
        certificates: &EdgeCertificates,
        edge_private_key_b64: &str,
    ) -> usize {
        let mut updated = 0;

        // Check which certs need updating based on fingerprint
        let needs_update: Vec<&EdgeCertBundle> = {
            let store = self.inner.read().unwrap();
            certificates
                .bundles
                .iter()
                .filter(|b| {
                    store
                        .get(&b.domain)
                        .map(|existing| existing.fingerprint != b.fingerprint)
                        .unwrap_or(true)
                })
                .collect()
        };

        if needs_update.is_empty() {
            return 0;
        }

        let mut store = self.inner.write().unwrap();

        for bundle in needs_update {
            let ecies_bundle = temps_core::ecies::EncryptedBundle {
                ciphertext: bundle.ciphertext.clone(),
                nonce: bundle.nonce.clone(),
            };

            let plaintext = match temps_core::ecies::decrypt_bundle(
                edge_private_key_b64,
                &certificates.ephemeral_public_key,
                &ecies_bundle,
            ) {
                Ok(p) => p,
                Err(e) => {
                    warn!("Failed to decrypt cert bundle for {}: {}", bundle.domain, e);
                    continue;
                }
            };

            let payload = match String::from_utf8(plaintext) {
                Ok(s) => s,
                Err(_) => {
                    warn!("Decrypted cert for {} is not valid UTF-8", bundle.domain);
                    continue;
                }
            };

            // Parse PEM cert chain and private key from the combined payload
            match parse_pem_payload(&payload) {
                Ok((certs_der, key_der)) => {
                    debug!(
                        "Updated certificate for {} ({} cert(s), fingerprint={})",
                        bundle.domain,
                        certs_der.len(),
                        &bundle.fingerprint[..12]
                    );
                    store.insert(
                        bundle.domain.clone(),
                        CertEntry {
                            certs_der,
                            key_der,
                            fingerprint: bundle.fingerprint.clone(),
                        },
                    );
                    updated += 1;
                }
                Err(e) => {
                    warn!("Failed to parse cert PEM for {}: {}", bundle.domain, e);
                }
            }
        }

        updated
    }

    /// Look up a certificate by SNI hostname (exact match, then wildcard fallback).
    fn lookup(&self, sni: &str) -> Option<(Vec<Vec<u8>>, Vec<u8>)> {
        let store = self.inner.read().unwrap();

        // Exact match
        if let Some(entry) = store.get(sni) {
            return Some((entry.certs_der.clone(), entry.key_der.clone()));
        }

        // Wildcard fallback: app.example.com -> *.example.com
        if let Some(dot_pos) = sni.find('.') {
            let wildcard = format!("*.{}", &sni[dot_pos + 1..]);
            if let Some(entry) = store.get(&wildcard) {
                return Some((entry.certs_der.clone(), entry.key_der.clone()));
            }
        }

        None
    }

    /// Number of certificates in the store.
    pub fn len(&self) -> usize {
        self.inner.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// List all domains with certificates (for debugging).
    pub fn domains(&self) -> Vec<String> {
        self.inner.read().unwrap().keys().cloned().collect()
    }
}

/// Parse a combined PEM payload (cert chain + private key) into DER bytes.
fn parse_pem_payload(payload: &str) -> Result<(Vec<Vec<u8>>, Vec<u8>), String> {
    let mut reader = BufReader::new(payload.as_bytes());
    let mut certs_der = Vec::new();
    let mut key_der: Option<Vec<u8>> = None;

    loop {
        match rustls_pemfile::read_one(&mut reader) {
            Ok(Some(rustls_pemfile::Item::X509Certificate(cert))) => {
                certs_der.push(cert.to_vec());
            }
            Ok(Some(rustls_pemfile::Item::Pkcs1Key(key))) => {
                key_der = Some(key.secret_pkcs1_der().to_vec());
            }
            Ok(Some(rustls_pemfile::Item::Pkcs8Key(key))) => {
                key_der = Some(key.secret_pkcs8_der().to_vec());
            }
            Ok(Some(rustls_pemfile::Item::Sec1Key(key))) => {
                key_der = Some(key.secret_sec1_der().to_vec());
            }
            Ok(Some(_)) => continue,
            Ok(None) => break,
            Err(e) => return Err(format!("PEM parse error: {}", e)),
        }
    }

    if certs_der.is_empty() {
        return Err("No certificates found in PEM".to_string());
    }
    let key = key_der.ok_or("No private key found in PEM")?;

    Ok((certs_der, key))
}

/// Pingora TLS callback that serves certificates from the `EdgeCertStore`.
///
/// Mirrors `DynamicCertLoader` in `temps-proxy/src/server.rs`.
pub struct EdgeTlsAccept {
    cert_store: Arc<EdgeCertStore>,
}

impl EdgeTlsAccept {
    pub fn new(cert_store: Arc<EdgeCertStore>) -> Self {
        Self { cert_store }
    }
}

#[async_trait]
impl TlsAccept for EdgeTlsAccept {
    async fn certificate_callback(&self, ssl_ref: &mut TlsRef) -> () {
        use pingora_openssl::ext;
        use pingora_openssl::ssl::SslRef;

        let ssl: &mut SslRef = unsafe { std::mem::transmute(ssl_ref) };

        let sni = ssl
            .servername(NameType::HOST_NAME)
            .unwrap_or("default")
            .to_string();

        debug!("Edge TLS callback for SNI: {}", sni);

        let (certs_der, key_der) = match self.cert_store.lookup(&sni) {
            Some(data) => data,
            None => {
                debug!("No certificate for SNI: {}", sni);
                return;
            }
        };

        // Load certificate chain
        for (i, cert_bytes) in certs_der.iter().enumerate() {
            match X509::from_der(cert_bytes) {
                Ok(cert) => {
                    if i == 0 {
                        if let Err(e) = ext::ssl_use_certificate(ssl, &cert) {
                            debug!("Failed to set leaf cert for {}: {}", sni, e);
                            return;
                        }
                    } else if let Err(e) = ext::ssl_add_chain_cert(ssl, &cert) {
                        debug!("Failed to add chain cert {} for {}: {}", i, sni, e);
                        return;
                    }
                }
                Err(e) => {
                    debug!("Failed to parse cert {} for {}: {}", i, sni, e);
                    return;
                }
            }
        }

        // Load private key
        match PKey::private_key_from_der(&key_der) {
            Ok(pkey) => {
                if let Err(e) = ext::ssl_use_private_key(ssl, &pkey) {
                    debug!("Failed to set private key for {}: {}", sni, e);
                }
            }
            Err(e) => {
                debug!("Failed to parse private key for {}: {}", sni, e);
            }
        }

        debug!("Edge TLS configured for {}", sni);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cert_store_empty() {
        let store = EdgeCertStore::new();
        assert!(store.is_empty());
        assert!(store.lookup("example.com").is_none());
    }

    #[test]
    fn test_cert_store_len() {
        let store = EdgeCertStore::new();
        assert_eq!(store.len(), 0);
    }
}
