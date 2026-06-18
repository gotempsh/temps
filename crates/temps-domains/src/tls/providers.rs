use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType as AcmeChallengeType,
    HttpClient, Identifier, NewAccount, NewOrder, Order, OrderStatus,
};
use rcgen::{CertificateParams, DistinguishedName, KeyPair};
use serde_json;
use std::sync::Arc;
use temps_core::UtcDateTime;
use tracing::{debug, error, info};

use super::errors::ProviderError;
use super::models::*;
use super::repository::CertificateRepository;

/// Build an `instant-acme`-compatible hyper+hyper-rustls HTTP client that trusts
/// the given PEM CA bundle in addition to the system roots.
///
/// This is the injection point for test CAs (e.g. Pebble's self-signed cert) and
/// for corporate internal ACME servers. A future ACME request timeout would also
/// live here (e.g. `HyperClient::builder(...).connection_verbose(true).pool_idle_timeout(...)`).
///
/// The function returns `Err` only when the PEM data cannot be parsed or the
/// connector cannot be built — both are configuration errors, not network errors.
///
/// instant-acme 0.8.5's client injects a `User-Agent: instant-acme/<ver>` header
/// on every request it builds, so Pebble's RFC 8555 §6.1 User-Agent requirement
/// is satisfied without any extra wrapper here.
/// Test-only re-export of `build_http_client_with_ca` so integration tests in
/// `domain_service.rs` can verify the underlying hyper client directly.
#[cfg(test)]
pub fn build_http_client_for_test(ca_pem: &[u8]) -> Result<Box<dyn HttpClient>, ProviderError> {
    build_http_client_with_ca(ca_pem)
}

/// Ensure a process-level rustls `CryptoProvider` is installed before we build
/// any `rustls::ClientConfig`.
///
/// instant-acme 0.8.5 enables hyper-rustls's `aws-lc-rs` backend by default,
/// while the rest of this workspace standardises on `ring` (via
/// testcontainers/bollard and our own `hyper-rustls`/`rustls` feature flags).
/// With BOTH the `ring` and `aws-lc-rs` rustls features compiled in, rustls can
/// no longer auto-pick a process-level provider, and `ClientConfig::builder()`
/// panics with "Could not automatically determine the process-level
/// CryptoProvider". The server binary already installs `ring` at startup
/// (`temps-cli`), so this only bites in standalone test/library contexts.
///
/// We pin `ring` here to match the workspace and call this before constructing
/// any client config. `install_default` returns `Err` if a provider is already
/// installed; that is fine — we ignore it (idempotent), mirroring the lazy
/// install pattern in `temps-query-postgres` and `temps-cli`.
fn ensure_crypto_provider_installed() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn build_http_client_with_ca(ca_pem: &[u8]) -> Result<Box<dyn HttpClient>, ProviderError> {
    use hyper_rustls::HttpsConnectorBuilder;
    use hyper_util::{client::legacy::Client as HyperClient, rt::TokioExecutor};
    use rustls::RootCertStore;

    let mut roots = RootCertStore::empty();

    // Load system roots first so the client can still reach other HTTPS endpoints.
    // `load_native_certs` returns a `CertificateResult` (not `Result`): best-effort
    // — but surface any load/add errors so an environment with no/partial system
    // trust store (minimal containers) is diagnosable instead of silently leaving
    // only the custom CA trusted. (Security review LOW.)
    let native = rustls_native_certs::load_native_certs();
    if !native.errors.is_empty() {
        tracing::warn!(
            error_count = native.errors.len(),
            "loading system root certificates reported errors; ACME client will trust \
             only the certs that loaded plus the supplied custom CA"
        );
    }
    for cert in native.certs {
        if let Err(e) = roots.add(cert) {
            tracing::warn!(error = %e, "failed to add a system root certificate to the ACME trust store");
        }
    }

    // Parse and add the caller-supplied CA PEM bundle.
    let mut cursor = std::io::Cursor::new(ca_pem);
    for cert in rustls_pemfile::certs(&mut cursor) {
        let cert = cert.map_err(|e| {
            ProviderError::Configuration(format!("Failed to parse custom CA PEM: {}", e))
        })?;
        roots.add(cert).map_err(|e| {
            ProviderError::Configuration(format!("Failed to add custom CA cert: {}", e))
        })?;
    }

    // Pin the rustls crypto provider (ring) so `ClientConfig::builder()` does
    // not panic when both `ring` and `aws-lc-rs` are compiled in (the latter is
    // pulled in transitively by instant-acme 0.8.5). See the helper docs.
    ensure_crypto_provider_installed();

    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    // `https_only`: never silently fall back to plaintext, even if a
    // misconfigured `ACME_DIRECTORY_URL` uses `http://` — that would transmit
    // ACME account credentials/key material in cleartext. (Security review LOW.)
    let connector = HttpsConnectorBuilder::new()
        .with_tls_config(tls_config)
        .https_only()
        .enable_http1()
        .enable_http2()
        .build();

    // instant-acme 0.8.5 builds requests with a `BodyWrapper<Bytes>` body and
    // provides a blanket `impl HttpClient for HyperClient<C, BodyWrapper<Bytes>>`,
    // so the bare hyper client is directly usable as a `Box<dyn HttpClient>`.
    let client: HyperClient<_, instant_acme::BodyWrapper<bytes::Bytes>> =
        HyperClient::builder(TokioExecutor::new()).build(connector);

    Ok(Box::new(client))
}

/// A `rustls` server-certificate verifier that accepts ANY certificate without
/// performing chain, hostname, expiry, or signature validation.
///
/// ############################  DANGER  ############################
/// This DISABLES ALL TLS VERIFICATION. It exists SOLELY so the integration
/// test suite can talk to Pebble's self-signed `https://localhost:14000/dir`
/// ACME directory, which presents a cert no system trust store contains. It
/// MUST NEVER be reachable in production: the only thing that constructs it is
/// `build_insecure_http_client`, which is itself gated STRICTLY on the
/// `ACME_INSECURE=1` environment variable (a TEST-ONLY switch). Do not relax
/// that gate, do not call this from any non-test code path, and never set
/// `ACME_INSECURE` in a production deployment — doing so makes ACME traffic
/// trivially MITM-able.
/// ##################################################################
#[derive(Debug)]
struct NoCertVerifier {
    /// The signature schemes the underlying crypto provider can verify. We still
    /// have to advertise a non-empty, correct list here even though we accept
    /// every signature, otherwise rustls rejects the handshake.
    supported_schemes: Vec<rustls::SignatureScheme>,
}

impl rustls::client::danger::ServerCertVerifier for NoCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls_pki_types::CertificateDer<'_>,
        _intermediates: &[rustls_pki_types::CertificateDer<'_>],
        _server_name: &rustls_pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls_pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        // DANGER: accept any certificate unconditionally (TEST-ONLY).
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls_pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        // DANGER: accept any signature unconditionally (TEST-ONLY).
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls_pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        // DANGER: accept any signature unconditionally (TEST-ONLY).
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.supported_schemes.clone()
    }
}

/// Build an `instant-acme`-compatible HTTP client that DISABLES TLS
/// verification entirely (accepts self-signed / invalid / expired certs).
///
/// ############################  DANGER  ############################
/// This client trusts every server certificate without validation. It is
/// TEST-ONLY and exists exclusively so the integration tests can reach
/// Pebble's self-signed ACME directory. The single caller
/// (`LetsEncryptProvider::acme_http_client`) invokes this ONLY when the
/// `ACME_INSECURE=1` environment variable is set. NEVER enable `ACME_INSECURE`
/// in production — it makes the ACME channel MITM-able and would let an
/// attacker issue/observe certificates for your domains.
/// ##################################################################
fn build_insecure_http_client() -> Result<Box<dyn HttpClient>, ProviderError> {
    use hyper_rustls::HttpsConnectorBuilder;
    use hyper_util::{client::legacy::Client as HyperClient, rt::TokioExecutor};

    // Pull the supported signature schemes from the default (ring) crypto
    // provider so the no-verify verifier still advertises a correct scheme
    // list during the handshake.
    let supported_schemes = rustls::crypto::ring::default_provider()
        .signature_verification_algorithms
        .supported_schemes();

    let verifier = std::sync::Arc::new(NoCertVerifier { supported_schemes });

    // Pin the rustls crypto provider (ring) so `ClientConfig::builder()` does
    // not panic when both `ring` and `aws-lc-rs` are compiled in (the latter is
    // pulled in transitively by instant-acme 0.8.5). See the helper docs.
    ensure_crypto_provider_installed();

    // DANGER: `.dangerous().with_custom_certificate_verifier(..)` installs the
    // no-verify verifier above, disabling all certificate validation.
    let tls_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();

    let connector = HttpsConnectorBuilder::new()
        .with_tls_config(tls_config)
        .https_only()
        .enable_http1()
        .enable_http2()
        .build();

    // instant-acme 0.8.5 builds requests with a `BodyWrapper<Bytes>` body and
    // provides a blanket `impl HttpClient for HyperClient<C, BodyWrapper<Bytes>>`,
    // so the bare hyper client is directly usable as a `Box<dyn HttpClient>`.
    // instant-acme injects its own User-Agent header (RFC 8555 §6.1) per request.
    let client: HyperClient<_, instant_acme::BodyWrapper<bytes::Bytes>> =
        HyperClient::builder(TokioExecutor::new()).build(connector);

    Ok(Box::new(client))
}

#[async_trait]
pub trait CertificateProvider: Send + Sync {
    async fn provision(
        &self,
        domain: &str,
        challenge: ChallengeType,
        email: &str,
    ) -> Result<ProvisioningResult, ProviderError>;

    async fn complete_challenge(
        &self,
        domain: &str,
        challenge_data: &ChallengeData,
        email: &str,
    ) -> Result<Certificate, ProviderError>;

    fn supported_challenges(&self) -> Vec<ChallengeType>;

    async fn validate_prerequisites(
        &self,
        domain: &str,
        email: &str,
    ) -> Result<ValidationResult, ProviderError>;

    /// Cancel an existing ACME order for a domain
    /// This allows you to abandon a failed order and create a new one
    async fn cancel_order(&self, domain: &str) -> Result<(), ProviderError>;
}

pub struct LetsEncryptProvider {
    repository: Arc<dyn CertificateRepository>,
    environment: String,
    /// Optional PEM-encoded CA bundle to trust when connecting to the ACME
    /// directory. When `None` the default `hyper-rustls` client (native roots)
    /// is used. When `Some`, a custom hyper+hyper-rustls client is built with
    /// this CA added to the trust store — see `build_http_client_with_ca`.
    ///
    /// Use this for Pebble (test ACME CA) or internal corporate ACME servers
    /// whose CA is not in the system trust store. Set via `with_custom_ca_pem`
    /// or the `ACME_CA_CERT_PEM` env var (path to a PEM file). Do NOT use this
    /// to disable TLS verification — the custom-CA path still performs full
    /// chain validation against the explicitly-supplied CA.
    custom_ca_pem: Option<Vec<u8>>,
}

impl LetsEncryptProvider {
    pub fn new(repository: Arc<dyn CertificateRepository>) -> Self {
        // Read environment from LETSENCRYPT_MODE env var, default to "production"
        let environment =
            std::env::var("LETSENCRYPT_MODE").unwrap_or_else(|_| "production".to_string());

        // Allow a custom CA PEM file path via env var (e.g. for Pebble in CI).
        let custom_ca_pem = std::env::var("ACME_CA_CERT_PEM")
            .ok()
            .and_then(|path| std::fs::read(&path).ok());

        Self {
            repository,
            environment,
            custom_ca_pem,
        }
    }

    /// Inject a custom CA PEM bundle into this provider. The provider will
    /// build a hyper+hyper-rustls client that trusts this CA in addition to
    /// the system roots. Intended for Pebble and corporate internal CAs.
    pub fn with_custom_ca_pem(mut self, ca_pem: Vec<u8>) -> Self {
        self.custom_ca_pem = Some(ca_pem);
        self
    }

    fn get_acme_url(&self) -> String {
        // Allow custom ACME directory URL for testing (e.g., Pebble)
        if let Ok(custom_url) = std::env::var("ACME_DIRECTORY_URL") {
            return custom_url;
        }

        if self.environment == "production" {
            instant_acme::LetsEncrypt::Production.url().to_string()
        } else {
            instant_acme::LetsEncrypt::Staging.url().to_string()
        }
    }

    /// Decide which HTTP client `instant-acme` should use for this provider.
    ///
    /// Returns `Ok(None)` for the DEFAULT path (no custom client) so that
    /// `Account::create` / `Account::from_credentials` use `instant-acme`'s
    /// built-in client unchanged. Returns `Ok(Some(client))` only when a custom
    /// client is required:
    ///
    /// 1. `ACME_INSECURE=1` (TEST-ONLY) — builds a client that DISABLES TLS
    ///    verification so the integration tests can reach Pebble's self-signed
    ///    ACME directory. See the DANGER notice on `build_insecure_http_client`.
    ///    This takes precedence over the custom-CA path. It MUST NEVER be set in
    ///    production: it makes the ACME channel MITM-able.
    /// 2. A `custom_ca_pem` is configured (e.g. Pebble's CA, or a corporate
    ///    internal ACME CA) — builds a client that trusts that CA in addition
    ///    to the system roots while still performing FULL chain validation.
    ///
    /// When neither applies, behaviour is byte-for-byte identical to before
    /// (default `instant-acme` client, full system-root TLS validation).
    fn acme_http_client(&self) -> Result<Option<Box<dyn HttpClient>>, ProviderError> {
        // Whenever we build a custom client, the directory URL must be https://.
        // The custom connectors are https_only, but fail loudly here rather than
        // let a misconfigured http:// URL produce a confusing connect error.
        // (Security review LOW.)
        let needs_custom_client =
            std::env::var("ACME_INSECURE").as_deref() == Ok("1") || self.custom_ca_pem.is_some();
        if needs_custom_client {
            let url = self.get_acme_url();
            if !url.starts_with("https://") {
                return Err(ProviderError::Configuration(format!(
                    "ACME directory URL must use https:// when a custom CA / insecure \
                     client is configured, got: {url}"
                )));
            }
        }

        // TEST-ONLY insecure path, gated STRICTLY on the env var being exactly "1".
        // SECURITY: this disables TLS verification — never enable in production.
        if std::env::var("ACME_INSECURE").as_deref() == Ok("1") {
            tracing::warn!(
                "ACME_INSECURE=1 is set: TLS verification for the ACME directory is DISABLED. \
                 This is TEST-ONLY (Pebble) and MUST NEVER be used in production."
            );
            return Ok(Some(build_insecure_http_client()?));
        }

        if let Some(ca_pem) = &self.custom_ca_pem {
            return Ok(Some(build_http_client_with_ca(ca_pem)?));
        }

        Ok(None)
    }

    async fn get_or_create_acme_account(
        &self,
        email: &str,
    ) -> Result<(Account, AccountCredentials), ProviderError> {
        info!(
            "Getting or creating ACME account for email: {} environment: {}",
            email, self.environment
        );

        if let Some(account) = self
            .repository
            .find_acme_account(email, &self.environment)
            .await?
        {
            let account_creds: AccountCredentials = serde_json::from_str(&account.credentials)
                .map_err(|e| {
                    ProviderError::Configuration(format!("Failed to deserialize account: {}", e))
                })?;

            let account_creds_clone = serde_json::from_str(&account.credentials).map_err(|e| {
                ProviderError::Configuration(format!("Failed to deserialize account: {}", e))
            })?;

            // 0.8.5 uses a builder: a custom HTTP client (Pebble CA / insecure)
            // selects `builder_with_http`; otherwise the default client builder.
            let builder = match self.acme_http_client()? {
                Some(http) => Account::builder_with_http(http),
                None => Account::builder().map_err(|e| {
                    ProviderError::Acme(format!("Failed to build ACME account client: {}", e))
                })?,
            };
            let acme_account = builder
                .from_credentials(account_creds)
                .await
                .map_err(|e| ProviderError::Acme(format!("Failed to load account: {}", e)))?;

            Ok((acme_account, account_creds_clone))
        } else {
            let acme_url = self.get_acme_url();
            let contact = format!("mailto:{}", email);
            let new_account = NewAccount {
                contact: &[contact.as_str()],
                terms_of_service_agreed: true,
                only_return_existing: false,
            };
            // 0.8.5 uses a builder + owned directory URL; `external_account` is None.
            let builder = match self.acme_http_client()? {
                Some(http) => Account::builder_with_http(http),
                None => Account::builder().map_err(|e| {
                    ProviderError::Acme(format!("Failed to build ACME account client: {}", e))
                })?,
            };
            let (acme_account, credentials) = builder.create(&new_account, acme_url, None).await?;

            let account_creds_str = serde_json::to_string(&credentials).map_err(|e| {
                ProviderError::Configuration(format!("Failed to serialize account: {}", e))
            })?;

            let acme_account_data = AcmeAccount {
                email: email.to_string(),
                environment: self.environment.clone(),
                credentials: account_creds_str,
                created_at: Utc::now(),
            };

            self.repository.save_acme_account(acme_account_data).await?;

            Ok((acme_account, credentials))
        }
    }

    async fn generate_certificate_from_order(
        &self,
        domain: &str,
        order: &mut Order,
    ) -> Result<Certificate, ProviderError> {
        // Generate CSR
        // For wildcard domains, include both wildcard and base domain
        let names = if let Some(base_domain) = domain.strip_prefix("*.") {
            vec![domain.to_string(), base_domain.to_string()]
        } else {
            vec![domain.to_string()]
        };
        let mut params = CertificateParams::new(names)?;
        params.distinguished_name = DistinguishedName::new();

        let private_key = KeyPair::generate()?;
        let csr = params.serialize_request(&private_key)?;

        // Finalize order. In instant-acme 0.8.5 the CSR-based finalize is
        // `finalize_csr` (`finalize` is the rcgen-convenience variant that
        // generates its own key+CSR — we build our own above, so use `finalize_csr`).
        order.finalize_csr(csr.der()).await?;

        // Wait for certificate
        let cert_chain_pem = loop {
            match order.certificate().await? {
                Some(cert) => break cert,
                None => tokio::time::sleep(std::time::Duration::from_secs(1)).await,
            }
        };

        // Extract expiration time
        let expiration_time = self.extract_expiration_time(&cert_chain_pem)?;

        Ok(Certificate {
            id: 1,
            domain: domain.to_string(),
            certificate_pem: cert_chain_pem,
            private_key_pem: private_key.serialize_pem(),
            expiration_time,
            last_renewed: Some(Utc::now()),
            is_wildcard: domain.starts_with("*."),
            verification_method: "acme".to_string(),
            status: CertificateStatus::Active,
        })
    }

    fn extract_expiration_time(&self, cert_pem: &str) -> Result<UtcDateTime, ProviderError> {
        let (_, pem) = x509_parser::pem::parse_x509_pem(cert_pem.as_bytes()).map_err(|e| {
            ProviderError::CertificateGeneration(format!("Failed to parse PEM: {}", e))
        })?;

        let x509 = pem.parse_x509().map_err(|e| {
            ProviderError::CertificateGeneration(format!("Failed to parse X509: {}", e))
        })?;

        let not_after = x509.validity().not_after;

        let expiration_time = chrono::Utc
            .timestamp_opt(not_after.timestamp(), 0)
            .single()
            .ok_or_else(|| {
                ProviderError::CertificateGeneration("Invalid expiration timestamp".to_string())
            })?;

        Ok(expiration_time)
    }

    async fn handle_http_challenge(
        &self,
        domain: &str,
        order: &mut Order,
    ) -> Result<ChallengeData, ProviderError> {
        // Capture the order URL before borrowing `order` mutably for the
        // authorization stream (0.8.5: `authorizations()` takes `&mut self`).
        let order_url = order.url().to_string();

        // 0.8.5: drive the `Authorizations` stream. HTTP-01 single-domain orders
        // have exactly one authorization; take the first one.
        let mut authzs = order.authorizations();
        let mut handle = authzs
            .next()
            .await
            .ok_or_else(|| ProviderError::ValidationFailed("No authorizations found".to_string()))?
            .map_err(|e| ProviderError::Acme(format!("Failed to fetch authorization: {}", e)))?;

        // Check authorization status (read via Deref to AuthorizationState).
        match handle.status {
            AuthorizationStatus::Valid => {
                // This shouldn't happen since we check all_valid before calling this,
                // but handle it gracefully.
                return Err(ProviderError::ValidationFailed(
                    "Authorization is already valid, no challenge needed".to_string(),
                ));
            }
            AuthorizationStatus::Pending => {
                // Need to complete challenge.
            }
            other => {
                return Err(ProviderError::ValidationFailed(format!(
                    "Authorization has unexpected status: {:?}",
                    other
                )));
            }
        }

        let challenge = handle.challenge(AcmeChallengeType::Http01).ok_or_else(|| {
            ProviderError::UnsupportedChallenge("No HTTP-01 challenge found".to_string())
        })?;

        let key_auth = challenge.key_authorization();

        Ok(ChallengeData {
            challenge_type: ChallengeType::Http01,
            domain: domain.to_string(),
            token: challenge.token.clone(),
            key_authorization: key_auth.as_str().to_string(),
            validation_url: Some(challenge.url.clone()),
            dns_txt_records: vec![], // No DNS records for HTTP-01
            order_url: Some(order_url),
        })
    }

    async fn handle_dns_challenge(
        &self,
        domain: &str,
        order: &mut Order,
    ) -> Result<ChallengeData, ProviderError> {
        // Capture the order URL before the mutable authorization-stream borrow.
        let order_url = order.url().to_string();

        // Extract base domain for DNS record name
        let dns_record_domain = domain.strip_prefix("*.").unwrap_or(domain);

        // For wildcard domains with base domain, we'll have multiple authorizations.
        // Collect ALL DNS TXT records that need to be added.
        let mut dns_txt_records = Vec::new();
        let mut first_challenge_url: Option<String> = None;
        let mut first_token = String::new();
        let mut first_key_auth = String::new();
        let mut saw_any = false;

        // 0.8.5: drive the `Authorizations` stream off `&mut order`.
        let mut authzs = order.authorizations();
        while let Some(result) = authzs.next().await {
            let mut handle = result.map_err(|e| {
                ProviderError::Acme(format!("Failed to fetch authorization: {}", e))
            })?;
            saw_any = true;

            // The identifier is borrowed from the handle; format it to an owned
            // string for diagnostics so it does not outlive the handle reborrow.
            let identifier = handle.identifier().to_string();

            // Skip authorizations that are already valid (cached from previous validation).
            // ACME servers cache successful validations for ~30 days, so we may encounter
            // authorizations that don't need new challenges.
            match handle.status {
                AuthorizationStatus::Valid => {
                    debug!(
                        "Authorization for {} is already valid, skipping challenge",
                        identifier
                    );
                    continue;
                }
                AuthorizationStatus::Pending => {
                    // Need to complete challenge.
                }
                other => {
                    return Err(ProviderError::ValidationFailed(format!(
                        "Authorization for {} has unexpected status: {:?}",
                        identifier, other
                    )));
                }
            }

            let challenge = handle.challenge(AcmeChallengeType::Dns01).ok_or_else(|| {
                ProviderError::UnsupportedChallenge(format!(
                    "No DNS-01 challenge found for {}",
                    identifier
                ))
            })?;

            let key_auth = challenge.key_authorization();
            // 0.8.5: `KeyAuthorization::dns_value()` computes base64url(SHA256(..))
            // for us, replacing the previous manual Sha256 + URL_SAFE_NO_PAD block.
            let txt_value = key_auth.dns_value();
            let challenge_url = challenge.url.clone();
            let token = challenge.token.clone();
            let key_auth_str = key_auth.as_str().to_string();

            // Add DNS TXT record with its validation URL.
            dns_txt_records.push(DnsTxtRecord {
                name: format!("_acme-challenge.{}", dns_record_domain),
                value: txt_value,
                validation_url: challenge_url.clone(),
            });

            // Store first challenge details for backward compatibility.
            if first_challenge_url.is_none() {
                first_challenge_url = Some(challenge_url);
                first_token = token;
                first_key_auth = key_auth_str;
            }
        }

        if !saw_any {
            return Err(ProviderError::ValidationFailed(
                "No authorizations found".to_string(),
            ));
        }

        info!(
            "DNS-01 challenge for {}: {} TXT record(s) to add to _acme-challenge.{}",
            domain,
            dns_txt_records.len(),
            dns_record_domain
        );

        Ok(ChallengeData {
            challenge_type: ChallengeType::Dns01,
            domain: domain.to_string(),
            token: first_token,
            key_authorization: first_key_auth,
            validation_url: first_challenge_url,
            dns_txt_records,
            order_url: Some(order_url),
        })
    }

    async fn wait_for_order_ready(&self, order: &mut Order) -> Result<(), ProviderError> {
        const MAX_ATTEMPTS: u8 = 6;
        const BASE_DELAY_SECS: u64 = 1;
        const MAX_DELAY_SECS: u64 = 30;

        for attempt in 1..=MAX_ATTEMPTS {
            // Exponential backoff: 1s, 2s, 4s, 8s, 16s, 30s (capped)
            let delay_secs = std::cmp::min(
                BASE_DELAY_SECS * 2u64.pow((attempt - 1) as u32),
                MAX_DELAY_SECS,
            );
            tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
            let state = order.refresh().await?;

            match state.status {
                OrderStatus::Ready => {
                    info!("Order is ready after {} attempt(s)", attempt);
                    return Ok(());
                }
                OrderStatus::Invalid => {
                    // Pull the per-challenge error detail off the order's
                    // authorizations so the failure is actionable (e.g. the ACME
                    // server's "Connection refused" / "incorrect key
                    // authorization" message) instead of a bare "validation
                    // failed". Best-effort: if we cannot fetch the detail we
                    // still return a typed error.
                    let detail = self.collect_authorization_errors(order).await;
                    let error_msg = if detail.is_empty() {
                        format!("Order validation failed after {} attempt(s)", attempt)
                    } else {
                        format!(
                            "Order validation failed after {} attempt(s): {}",
                            attempt, detail
                        )
                    };
                    error!("{}", error_msg);
                    return Err(ProviderError::ChallengeFailed(error_msg));
                }
                _ => {
                    if attempt < MAX_ATTEMPTS {
                        let next_delay = std::cmp::min(
                            BASE_DELAY_SECS * 2u64.pow(attempt as u32),
                            MAX_DELAY_SECS,
                        );
                        info!(
                            "Order not ready yet (attempt {}/{}), retrying in {}s",
                            attempt, MAX_ATTEMPTS, next_delay
                        );
                    } else {
                        let error_msg =
                            format!("Order validation timed out after {} attempts", MAX_ATTEMPTS);
                        error!("{}", error_msg);
                        return Err(ProviderError::ChallengeFailed(error_msg));
                    }
                }
            }
        }

        // This should never be reached due to the loop logic, but added for completeness
        Err(ProviderError::ChallengeFailed(format!(
            "Order validation timed out after {} attempts",
            MAX_ATTEMPTS
        )))
    }

    /// Walk the order's authorizations and collect any per-challenge ACME error
    /// details (type + detail) into a single human-readable string. Used to turn
    /// a bare `OrderStatus::Invalid` into an actionable message. Best-effort: a
    /// fetch failure simply contributes its own message rather than aborting.
    async fn collect_authorization_errors(&self, order: &mut Order) -> String {
        let mut parts: Vec<String> = Vec::new();
        let mut authzs = order.authorizations();
        while let Some(result) = authzs.next().await {
            match result {
                Ok(handle) => {
                    let identifier = handle.identifier().to_string();
                    for challenge in handle.challenges.iter() {
                        if let Some(err) = challenge.error.as_ref() {
                            parts.push(format!(
                                "[{} {:?}] {}: {}",
                                identifier,
                                challenge.r#type,
                                err.r#type.as_deref().unwrap_or("(no type)"),
                                err.detail.as_deref().unwrap_or("(no detail)")
                            ));
                        }
                    }
                }
                Err(e) => parts.push(format!("failed to fetch authorization: {}", e)),
            }
        }
        parts.join("; ")
    }
}

#[async_trait]
impl CertificateProvider for LetsEncryptProvider {
    async fn provision(
        &self,
        domain: &str,
        challenge: ChallengeType,
        email: &str,
    ) -> Result<ProvisioningResult, ProviderError> {
        info!(
            "Provisioning certificate for domain: {} using {:?} with email: {}",
            domain, challenge, email
        );

        // Wildcard domains MUST use DNS-01 challenge
        if domain.starts_with("*.") && challenge != ChallengeType::Dns01 {
            return Err(ProviderError::UnsupportedChallenge(
                format!("Wildcard domain '{}' requires DNS-01 challenge. HTTP-01 is not supported for wildcards.", domain)
            ));
        }

        // For wildcard domains, also request the base domain in the same certificate
        // e.g., if domain is "*.example.com", request both "*.example.com" and "example.com"
        let identifiers = if let Some(base_domain) = domain.strip_prefix("*.") {
            // Remove "*." prefix
            info!(
                "Requesting wildcard certificate for {} - including base domain {}",
                domain, base_domain
            );
            vec![
                Identifier::Dns(domain.to_string()),
                Identifier::Dns(base_domain.to_string()),
            ]
        } else {
            vec![Identifier::Dns(domain.to_string())]
        };

        let (acme_account, _) = self.get_or_create_acme_account(email).await?;

        // 0.8.5: `NewOrder` fields are private — use the constructor.
        let mut order = acme_account.new_order(&NewOrder::new(&identifiers)).await?;

        // Check if order is already ready (renewal case)
        if order.state().status == OrderStatus::Ready {
            info!("Order is already ready, generating certificate");
            let cert = self
                .generate_certificate_from_order(domain, &mut order)
                .await?;
            return Ok(ProvisioningResult::Certificate(cert));
        }

        // Check if all authorizations are already valid (cached from previous validation).
        // This can happen when Let's Encrypt has cached the authorization (~30 days).
        // 0.8.5: drive the `Authorizations` stream to inspect each status. Fetched
        // state is cached on the order, so the handlers below re-iterate without
        // additional network round-trips.
        let mut authz_count: usize = 0;
        let mut all_valid = true;
        {
            let mut authzs = order.authorizations();
            while let Some(result) = authzs.next().await {
                let handle = result.map_err(|e| {
                    ProviderError::Acme(format!("Failed to fetch authorization: {}", e))
                })?;
                authz_count += 1;
                if handle.status != AuthorizationStatus::Valid {
                    all_valid = false;
                }
            }
        }

        if all_valid && authz_count > 0 {
            info!(
                "All {} authorizations are already valid (cached), generating certificate directly",
                authz_count
            );
            let cert = self
                .generate_certificate_from_order(domain, &mut order)
                .await?;
            return Ok(ProvisioningResult::Certificate(cert));
        }

        match challenge {
            ChallengeType::Http01 => {
                let challenge_data = self.handle_http_challenge(domain, &mut order).await?;
                Ok(ProvisioningResult::Challenge(challenge_data))
            }
            ChallengeType::Dns01 => {
                let challenge_data = self.handle_dns_challenge(domain, &mut order).await?;
                Ok(ProvisioningResult::Challenge(challenge_data))
            }
        }
    }

    async fn complete_challenge(
        &self,
        domain: &str,
        challenge_data: &ChallengeData,
        email: &str,
    ) -> Result<Certificate, ProviderError> {
        debug!(
            "Completing {:?} challenge for domain: {} with email: {}",
            challenge_data.challenge_type, domain, email
        );

        let (acme_account, _) = self.get_or_create_acme_account(email).await?;

        // Load the existing order using the stored order URL
        let order_url = challenge_data.order_url.as_ref().ok_or_else(|| {
            ProviderError::Configuration("Order URL not found in challenge data".to_string())
        })?;

        debug!("Loading existing ACME order from URL: {}", order_url);
        let mut order = acme_account.order(order_url.clone()).await?;

        // 0.8.5: `Order::set_challenge_ready(url)` was removed. Instead, drive the
        // `Authorizations` stream, obtain the `ChallengeHandle` for the requested
        // type on each (still-pending) authorization, and call `set_ready()` on it
        // (the handle carries its own challenge URL). This preserves the previous
        // behaviour: HTTP-01 marks the single challenge ready; DNS-01 marks every
        // authorization's challenge ready (important for wildcards with multiple
        // authorizations).
        let acme_challenge_type = match challenge_data.challenge_type {
            ChallengeType::Http01 => AcmeChallengeType::Http01,
            ChallengeType::Dns01 => {
                if challenge_data.dns_txt_records.is_empty() {
                    return Err(ProviderError::Configuration(
                        "No DNS TXT records found for DNS-01 challenge".to_string(),
                    ));
                }
                AcmeChallengeType::Dns01
            }
        };

        let mut marked_ready: usize = 0;
        {
            let mut authzs = order.authorizations();
            while let Some(result) = authzs.next().await {
                let mut handle = result.map_err(|e| {
                    ProviderError::Acme(format!("Failed to fetch authorization: {}", e))
                })?;

                // Skip authorizations that are already valid (cached validations).
                if handle.status == AuthorizationStatus::Valid {
                    continue;
                }

                let mut challenge =
                    handle
                        .challenge(acme_challenge_type.clone())
                        .ok_or_else(|| {
                            ProviderError::UnsupportedChallenge(format!(
                            "No {:?} challenge found for authorization while completing challenge",
                            acme_challenge_type
                        ))
                        })?;

                debug!(
                    "Setting {:?} challenge ready for domain {} (URL: {})",
                    challenge_data.challenge_type, domain, challenge.url
                );
                challenge.set_ready().await?;
                marked_ready += 1;
            }
        }

        if marked_ready == 0 {
            return Err(ProviderError::Configuration(format!(
                "No pending {:?} challenge found for domain {} when completing challenge",
                challenge_data.challenge_type, domain
            )));
        }

        // Wait for validation
        self.wait_for_order_ready(&mut order).await?;

        // Generate certificate
        self.generate_certificate_from_order(domain, &mut order)
            .await
    }

    fn supported_challenges(&self) -> Vec<ChallengeType> {
        vec![ChallengeType::Http01, ChallengeType::Dns01]
    }

    async fn validate_prerequisites(
        &self,
        domain: &str,
        email: &str,
    ) -> Result<ValidationResult, ProviderError> {
        let mut result = ValidationResult {
            is_valid: true,
            errors: Vec::new(),
            warnings: Vec::new(),
        };

        // Check if email is configured
        if email.is_empty() {
            result.is_valid = false;
            result.errors.push("ACME email not configured".to_string());
        }

        // Check domain format
        if domain.is_empty() {
            result.is_valid = false;
            result.errors.push("Domain cannot be empty".to_string());
        }

        // Warn about staging environment
        if self.environment != "production" {
            result.warnings.push(format!(
                "Using {} environment - certificates will not be trusted",
                self.environment
            ));
        }

        Ok(result)
    }

    async fn cancel_order(&self, domain: &str) -> Result<(), ProviderError> {
        info!("Canceling ACME order for domain: {}", domain);

        // Note: We can't directly access the DB from the provider, so we'll just
        // return success. The actual cancellation happens when creating a new order.
        // ACME doesn't require explicit order cancellation - orders expire after some time.

        info!(
            "ACME order cancellation requested for domain: {}. New order can be created.",
            domain
        );
        Ok(())
    }
}

// Additional methods specific to LetsEncryptProvider
impl LetsEncryptProvider {
    /// Fetch live challenge validation status from Let's Encrypt
    /// This fetches the current state of the challenge directly from the ACME server
    pub async fn get_challenge_status(
        &self,
        order_url: &str,
        email: &str,
    ) -> Result<Option<serde_json::Value>, ProviderError> {
        debug!("Fetching live challenge status for order: {}", order_url);

        let (acme_account, _) = self.get_or_create_acme_account(email).await?;

        // Load the order from Let's Encrypt
        let mut order = acme_account.order(order_url.to_string()).await?;

        // 0.8.5: drive the `Authorizations` stream. We only inspect the first
        // authorization's challenges (typically one per single-domain order).
        let mut authzs = order.authorizations();
        let handle = match authzs.next().await {
            Some(result) => result.map_err(|e| {
                ProviderError::Acme(format!("Failed to fetch authorization: {}", e))
            })?,
            None => {
                debug!("No authorizations found for order");
                return Ok(None);
            }
        };

        // `challenges` is reachable on `AuthorizationState` via `Deref`.
        let challenge = handle.challenges.iter().find(|c| {
            matches!(
                c.r#type,
                AcmeChallengeType::Http01 | AcmeChallengeType::Dns01
            )
        });

        if let Some(challenge) = challenge {
            // Convert challenge to JSON format matching Let's Encrypt response
            let challenge_json = serde_json::json!({
                "type": match challenge.r#type {
                    AcmeChallengeType::Http01 => "http-01",
                    AcmeChallengeType::Dns01 => "dns-01",
                    _ => "unknown"
                },
                "url": challenge.url,
                "status": format!("{:?}", challenge.status).to_lowercase(),
                "error": challenge.error.as_ref().map(|e| serde_json::json!({
                    "type": e.r#type,
                    "detail": e.detail,
                    "status": e.status
                })),
                "token": challenge.token
            });

            Ok(Some(challenge_json))
        } else {
            debug!("No HTTP-01 or DNS-01 challenge found");
            Ok(None)
        }
    }
}

impl From<super::errors::RepositoryError> for ProviderError {
    fn from(err: super::errors::RepositoryError) -> Self {
        use super::errors::RepositoryError;
        match err {
            RepositoryError::NotFound(msg) => ProviderError::Internal(msg),
            RepositoryError::Database(msg) => ProviderError::Internal(msg),
            _ => ProviderError::Internal(err.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tls::repository::test_utils::MockCertificateRepository;

    #[tokio::test]
    async fn test_letsencrypt_provider_validation() {
        std::env::set_var("LETSENCRYPT_MODE", "staging");
        let repo = Arc::new(MockCertificateRepository::new());
        let provider = LetsEncryptProvider::new(repo);

        let result = provider
            .validate_prerequisites("example.com", "test@example.com")
            .await
            .unwrap();
        assert!(result.is_valid);
        assert_eq!(result.warnings.len(), 1); // Staging environment warning
    }

    #[tokio::test]
    async fn test_supported_challenges() {
        let repo = Arc::new(MockCertificateRepository::new());
        let provider = LetsEncryptProvider::new(repo);

        let challenges = provider.supported_challenges();
        assert_eq!(challenges.len(), 2);
        assert!(challenges.contains(&ChallengeType::Http01));
    }
}
