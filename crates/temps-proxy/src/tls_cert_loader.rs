use anyhow::Result;
use moka::future::Cache;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::io::BufReader;
use std::sync::Arc;
use std::time::Duration;
use temps_database::DbConnection;
use temps_entities::domains;
use tracing::{debug, warn};

/// Positive certificate cache TTL. Cert renewals happen weeks before expiry, so
/// 5 minutes of staleness is safe and eliminates up to 2 Postgres round-trips per
/// new TLS connection.
const CERT_CACHE_TTL: Duration = Duration::from_secs(5 * 60);

/// Negative certificate cache TTL. Kept short (30 s) so on-demand cert issuance
/// (ADR-018, `on_demand_cert.rs`) is picked up quickly after background provisioning
/// completes. A newly-issued cert becomes visible to clients within at most 30 s
/// after issuance, consistent with the on-demand "first request fails fast, retry
/// succeeds" contract in ADR-018 §1.
const CERT_NEGATIVE_CACHE_TTL: Duration = Duration::from_secs(30);

/// Upper bound on domains held in the positive cert cache. Keeps memory bounded
/// even on installs with many custom domains.
const CERT_CACHE_MAX_CAPACITY: u64 = 1_000;

/// Upper bound on SNIs held in the negative cert cache. Limits memory under
/// random-SNI scan conditions; the max is generous because entries are tiny
/// (one String key + unit value).
const CERT_NEGATIVE_CACHE_MAX_CAPACITY: u64 = 10_000;

/// TTL for the last-known-good fallback cache.
///
/// Entries in this cache are only consulted when `find_certificate_raw` returns
/// a DB *error* (not a clean "no rows" response). Cert renewals happen weeks
/// before expiry, so 24 hours is safely within any real-world Postgres outage
/// while still ensuring the entry will eventually age out if the domain is truly
/// decommissioned after a prolonged downtime.
///
/// A no-TTL (infinite) entry was considered but rejected: a 24-hour bound keeps
/// memory usage predictable and ensures entries don't survive far beyond the
/// cert's own validity window under catastrophic failure scenarios.
const CERT_LKG_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Which variant of [`PrivateKeyDer`] was parsed from the PEM. Stored alongside
/// the raw DER bytes so [`CachedCert::to_rustls`] can reconstruct the correct
/// typed key without re-parsing the PEM.
#[derive(Clone, Debug)]
enum CachedKeyType {
    Pkcs1,
    Sec1,
    Pkcs8,
}

/// In-memory representation of a cached TLS certificate entry.
///
/// Stores raw DER bytes rather than the parsed rustls types because
/// [`PrivateKeyDer`] is not [`Clone`] and therefore cannot be held directly in a
/// moka cache value. On a cache hit, the bytes are cheaply copied from the shared
/// [`Arc`] to construct fresh [`CertificateDer`] / [`PrivateKeyDer`] values — one
/// heap copy instead of a Postgres round-trip plus AES-GCM decryption plus PEM parse.
///
/// The [`Arc`] wrapper (used as the actual cache value type) means all concurrent
/// callers that hit the same cache entry share one allocation; cloning an
/// `Arc<CachedCert>` is a single atomic increment.
#[derive(Clone, Debug)]
pub struct CachedCert {
    /// DER bytes for each certificate in the chain, leaf certificate first.
    cert_ders: Vec<Vec<u8>>,
    /// Raw private key DER bytes.
    key_der: Vec<u8>,
    /// Which [`PrivateKeyDer`] sub-type to reconstruct from `key_der`.
    key_type: CachedKeyType,
}

impl CachedCert {
    /// Reconstruct owned rustls TLS types from the cached raw bytes.
    ///
    /// Each call copies the stored byte slices out of the `Arc`, so the returned
    /// values are fully owned and independent. This is the only allocation on a
    /// cache hit.
    fn to_rustls(&self) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
        let certs: Vec<CertificateDer<'static>> = self
            .cert_ders
            .iter()
            .map(|der| CertificateDer::from(der.clone()))
            .collect();

        let key_bytes = self.key_der.clone();
        let key: PrivateKeyDer<'static> = match self.key_type {
            CachedKeyType::Pkcs1 => rustls::pki_types::PrivatePkcs1KeyDer::from(key_bytes).into(),
            CachedKeyType::Sec1 => rustls::pki_types::PrivateSec1KeyDer::from(key_bytes).into(),
            CachedKeyType::Pkcs8 => rustls::pki_types::PrivatePkcs8KeyDer::from(key_bytes).into(),
        };

        Ok((certs, key))
    }
}

/// Certificate loader that fetches TLS certificates from the database with an
/// in-memory moka cache to eliminate per-connection Postgres round-trips on the
/// TLS SNI hot path.
///
/// ## Caching strategy
///
/// - **Positive results** (cert found): cached for 5 minutes keyed by the resolved
///   domain string (exact SNI, or `*.base.domain` for a wildcard match). Renewals
///   happen weeks before expiry, so 5-minute staleness is acceptable.
///
/// - **Negative results** (no cert): cached for 30 seconds keyed by the SNI. Short
///   TTL so on-demand issuance (ADR-018) is picked up quickly — a newly-provisioned
///   cert is visible within ~30 s of the background job completing.
///
/// - **Last-known-good fallback** (DB *error* only): every successful positive cache
///   population also writes to `last_known_good` with a 24-hour TTL. When the
///   5-minute `cert_cache` entry has expired *and* the DB call errors (e.g. Postgres
///   is down), `load_certificate` checks `last_known_good` for the same key and
///   serves the stale cert rather than failing the TLS handshake. This is the same
///   resilience pattern used by [`crate::service::cert_host_cache::CertHostCache`]'s
///   `ArcSwap` snapshot — we never go "blank" just because a refresh errored.
///
///   The fallback is only consulted on a DB *error*. A clean "no rows" response still
///   populates the `negative_cache` and returns `None` exactly as today.
///
/// ## Key separation
///
/// Exact-match hits are cached under the literal SNI key. Wildcard hits are cached
/// under the wildcard key (`*.base.domain`), so multiple SNIs that share the same
/// wildcard cert benefit from a single cache entry rather than duplicating it.
/// The two-step exact-first / wildcard-second resolution order is preserved.
pub struct CertificateLoader {
    db: Arc<DbConnection>,
    encryption_service: Arc<temps_core::EncryptionService>,
    /// Positive cache: resolved domain → Arc<cached cert+key bytes>. TTL 5 min.
    cert_cache: Cache<String, Arc<CachedCert>>,
    /// Negative cache: SNI → () (presence = no cert found). TTL 30 s.
    negative_cache: Cache<String, ()>,
    /// Last-known-good fallback: resolved domain → Arc<cached cert+key bytes>.
    /// TTL 24 h. Only consulted when `find_certificate_raw` returns a DB *error*
    /// after the primary `cert_cache` entry has expired. Populated in parallel with
    /// every `cert_cache` write so it always holds the most recently successful cert.
    last_known_good: Cache<String, Arc<CachedCert>>,
}

impl CertificateLoader {
    /// Create a new [`CertificateLoader`] with production cache TTLs
    /// (5 min positive, 30 s negative).
    pub fn new(
        db: Arc<DbConnection>,
        encryption_service: Arc<temps_core::EncryptionService>,
    ) -> Self {
        Self::new_with_ttls(
            db,
            encryption_service,
            CERT_CACHE_TTL,
            CERT_NEGATIVE_CACHE_TTL,
        )
    }

    /// Internal constructor that accepts explicit TTLs, used by tests to shorten
    /// the negative-cache or positive-cache TTL to observable durations.
    ///
    /// The `last_known_good` cache always uses [`CERT_LKG_TTL`] (24 h) — it is not
    /// configurable per-call because its whole purpose is a long-lived safety net.
    fn new_with_ttls(
        db: Arc<DbConnection>,
        encryption_service: Arc<temps_core::EncryptionService>,
        cert_ttl: Duration,
        negative_ttl: Duration,
    ) -> Self {
        let cert_cache = Cache::builder()
            .max_capacity(CERT_CACHE_MAX_CAPACITY)
            .time_to_live(cert_ttl)
            .build();
        let negative_cache = Cache::builder()
            .max_capacity(CERT_NEGATIVE_CACHE_MAX_CAPACITY)
            .time_to_live(negative_ttl)
            .build();
        let last_known_good = Cache::builder()
            .max_capacity(CERT_CACHE_MAX_CAPACITY)
            .time_to_live(CERT_LKG_TTL)
            .build();
        Self {
            db,
            encryption_service,
            cert_cache,
            negative_cache,
            last_known_good,
        }
    }

    /// Explicitly invalidate cached data for `domain`. Call this at cert-provisioning
    /// and renewal sites to make newly-issued certs visible before the TTL expires.
    ///
    /// Removes `domain` from all caches (primary, negative, and last-known-good) and
    /// also removes the wildcard form of `domain` from the positive and
    /// last-known-good caches. Silently no-ops for entries not present.
    pub async fn invalidate(&self, domain: &str) {
        self.cert_cache.invalidate(domain).await;
        self.negative_cache.invalidate(domain).await;
        self.last_known_good.invalidate(domain).await;
        if let Some(wildcard) = self.get_wildcard_domain(domain) {
            self.cert_cache.invalidate(&wildcard).await;
            self.last_known_good.invalidate(&wildcard).await;
        }
    }

    /// Load certificate for a given SNI hostname.
    ///
    /// Supports both exact matches and wildcard certificates (exact checked first).
    /// Results are served from the in-memory cache when available:
    ///
    /// - Positive hits (cert found): 5-minute TTL, keyed by resolved domain.
    /// - Negative hits (no cert): 30-second TTL, keyed by the SNI.
    ///
    /// ## DB-error resilience
    ///
    /// If the primary `cert_cache` entry has expired *and* the DB call errors (e.g.
    /// Postgres is down or the connection pool times out), this method checks
    /// `last_known_good` for the same key. If a previously-successful cert exists
    /// there, it is returned with a `warn!` log — the TLS handshake succeeds at the
    /// cost of serving a potentially-stale cert (still valid for weeks; renewals are
    /// automatic). If no last-known-good entry exists for this SNI (it has never been
    /// successfully cached before), the DB error is propagated unchanged.
    ///
    /// This behaviour mirrors [`crate::service::cert_host_cache::CertHostCache`]'s
    /// `ArcSwap` snapshot: we never go dark just because a background refresh failed.
    pub async fn load_certificate(
        &self,
        sni: &str,
    ) -> Result<Option<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)>> {
        debug!("Loading certificate for SNI: {}", sni);

        // Fast path 1: SNI is in the negative cache — no cert exists for this host.
        // Checking this first avoids two cache lookups on the common miss path.
        if self.negative_cache.get(sni).await.is_some() {
            debug!("Certificate negative-cache hit for SNI: {}", sni);
            return Ok(None);
        }

        // Fast path 2: exact domain is in the positive cache.
        if let Some(cached) = self.cert_cache.get(sni).await {
            debug!("Certificate cache hit (exact) for SNI: {}", sni);
            return cached.to_rustls().map(Some);
        }

        // Fast path 3: wildcard form is in the positive cache. This benefits all
        // SNIs that share the same wildcard cert without duplicating the entry.
        if let Some(wildcard) = self.get_wildcard_domain(sni) {
            if let Some(cached) = self.cert_cache.get(&wildcard).await {
                debug!(
                    "Certificate cache hit (wildcard {}) for SNI: {}",
                    wildcard, sni
                );
                return cached.to_rustls().map(Some);
            }
        }

        // Cache miss: fall through to database. Exact match first (DB query 1).
        let exact_raw = match self.find_certificate_raw(sni).await {
            Ok(opt) => opt,
            Err(e) => {
                // DB error (e.g. connection-pool timeout while Postgres is down).
                // Check last_known_good before propagating so an existing cert can
                // keep serving the TLS handshake even if Postgres is temporarily
                // unavailable.
                warn!(
                    sni = sni,
                    error = %e,
                    "DB error during exact certificate lookup; checking last-known-good cache"
                );
                if let Some(lkg) = self.last_known_good.get(sni).await {
                    warn!(
                        sni = sni,
                        "Serving stale last-known-good certificate for SNI due to DB error"
                    );
                    return lkg.to_rustls().map(Some);
                }
                // Nothing to fall back to — propagate the DB error.
                return Err(e);
            }
        };
        if let Some((key_type, cert_ders, key_der)) = exact_raw {
            let cached = Arc::new(CachedCert {
                cert_ders,
                key_der,
                key_type,
            });
            self.cert_cache
                .insert(sni.to_string(), Arc::clone(&cached))
                .await;
            self.last_known_good
                .insert(sni.to_string(), Arc::clone(&cached))
                .await;
            return cached.to_rustls().map(Some);
        }

        // Wildcard match (DB query 2). Cache under the wildcard key so future
        // lookups for any SNI with the same base domain benefit.
        if let Some(wildcard_domain) = self.get_wildcard_domain(sni) {
            debug!("Trying wildcard certificate for: {}", wildcard_domain);
            let wildcard_raw = match self.find_certificate_raw(&wildcard_domain).await {
                Ok(opt) => opt,
                Err(e) => {
                    // DB error on wildcard lookup — same resilience pattern as the
                    // exact-match path above: fall back to last_known_good if
                    // available, otherwise propagate.
                    warn!(
                        sni = sni,
                        wildcard_key = %wildcard_domain,
                        error = %e,
                        "DB error during wildcard certificate lookup; checking last-known-good cache"
                    );
                    if let Some(lkg) = self.last_known_good.get(&wildcard_domain).await {
                        warn!(
                            sni = sni,
                            wildcard_key = %wildcard_domain,
                            "Serving stale last-known-good wildcard certificate for SNI due to DB error"
                        );
                        return lkg.to_rustls().map(Some);
                    }
                    // Nothing to fall back to — propagate the DB error.
                    return Err(e);
                }
            };
            if let Some((key_type, cert_ders, key_der)) = wildcard_raw {
                let cached = Arc::new(CachedCert {
                    cert_ders,
                    key_der,
                    key_type,
                });
                self.cert_cache
                    .insert(wildcard_domain.clone(), Arc::clone(&cached))
                    .await;
                self.last_known_good
                    .insert(wildcard_domain, Arc::clone(&cached))
                    .await;
                return cached.to_rustls().map(Some);
            }
        }

        warn!("No certificate found for SNI: {}", sni);
        // Cache the negative outcome so repeated TLS handshakes for unknown SNIs
        // (e.g. bots scanning by IP, or on-demand cert provisioning in progress)
        // do not pile up into Postgres round-trips.
        self.negative_cache.insert(sni.to_string(), ()).await;
        Ok(None)
    }

    /// Query the database for a domain row, returning raw DER bytes suitable for
    /// caching when both `certificate` and `private_key` are present.
    ///
    /// Returns `None` if no matching row exists or the row lacks either field.
    ///
    /// Status is intentionally NOT filtered — any row with both fields populated
    /// can serve TLS regardless of ACME lifecycle state. See the original
    /// `find_certificate` rationale:
    /// 1. A cert must not become unserveable while re-issuance is in progress
    ///    (status transitions through non-serving states during the ACME flow).
    /// 2. An `on_demand_failed` row that still holds a valid unexpired cert must
    ///    keep serving rather than going dark.
    async fn find_certificate_raw(
        &self,
        domain: &str,
    ) -> Result<Option<(CachedKeyType, Vec<Vec<u8>>, Vec<u8>)>> {
        let domain_entity = domains::Entity::find()
            .filter(domains::Column::Domain.eq(domain))
            .one(self.db.as_ref())
            .await?;

        if let Some(domain_row) = domain_entity {
            if let (Some(cert_pem), Some(encrypted_key_pem)) =
                (domain_row.certificate, domain_row.private_key)
            {
                debug!("Found certificate for domain: {}", domain_row.domain);

                let key_pem = self
                    .encryption_service
                    .decrypt_string(&encrypted_key_pem)
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "Failed to decrypt private key for domain {}: {}",
                            domain_row.domain,
                            e
                        )
                    })?;

                let cert_ders = self.extract_cert_ders(cert_pem.as_bytes())?;
                let (key_type, key_der) = self.extract_key_der(key_pem.as_bytes())?;

                return Ok(Some((key_type, cert_ders, key_der)));
            } else {
                warn!(
                    "Domain {} found but missing certificate or key",
                    domain_row.domain
                );
            }
        }

        Ok(None)
    }

    /// Get wildcard domain from a subdomain.
    /// e.g. `"api.example.com"` → `"*.example.com"`.
    /// Returns `None` for single-label hostnames like `"localhost"`.
    fn get_wildcard_domain(&self, domain: &str) -> Option<String> {
        wildcard_for(domain)
    }

    /// Parse PEM-encoded certificates, returning raw DER bytes for each cert in
    /// the chain (leaf first).
    fn extract_cert_ders(&self, pem_bytes: &[u8]) -> Result<Vec<Vec<u8>>> {
        let mut reader = BufReader::new(pem_bytes);
        let ders: Vec<Vec<u8>> = rustls_pemfile::certs(&mut reader)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("Failed to parse certificates: {}", e))?
            .into_iter()
            .map(|cert| cert.as_ref().to_vec())
            .collect();

        if ders.is_empty() {
            return Err(anyhow::anyhow!("No certificates found in PEM"));
        }

        Ok(ders)
    }

    /// Parse a PEM-encoded private key, returning the raw DER bytes and variant tag.
    fn extract_key_der(&self, pem_bytes: &[u8]) -> Result<(CachedKeyType, Vec<u8>)> {
        let mut reader = BufReader::new(pem_bytes);

        loop {
            match rustls_pemfile::read_one(&mut reader)
                .map_err(|e| anyhow::anyhow!("Failed to parse private key: {}", e))?
            {
                Some(rustls_pemfile::Item::Pkcs1Key(key)) => {
                    return Ok((CachedKeyType::Pkcs1, key.secret_pkcs1_der().to_vec()));
                }
                Some(rustls_pemfile::Item::Pkcs8Key(key)) => {
                    return Ok((CachedKeyType::Pkcs8, key.secret_pkcs8_der().to_vec()));
                }
                Some(rustls_pemfile::Item::Sec1Key(key)) => {
                    return Ok((CachedKeyType::Sec1, key.secret_sec1_der().to_vec()));
                }
                None => break,
                _ => {}
            }
        }

        Err(anyhow::anyhow!("No valid private key found in PEM"))
    }
}

/// Derive the wildcard parent domain from a subdomain.
///
/// Examples:
/// - `"api.example.com"` → `Some("*.example.com")`
/// - `"www.sub.example.com"` → `Some("*.sub.example.com")`
/// - `"localhost"` → `None` (single-label; no parent zone)
///
/// Used by both [`CertificateLoader`] (TLS SNI resolution) and
/// [`crate::service::cert_host_cache::CertHostCache`] (HTTP→HTTPS redirect check)
/// so that both are consistent about what constitutes a wildcard match.
pub(crate) fn wildcard_for(domain: &str) -> Option<String> {
    let parts: Vec<&str> = domain.split('.').collect();
    if parts.len() >= 2 {
        let base_domain = parts[1..].join(".");
        Some(format!("*.{}", base_domain))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_database::DbConnection;

    const TEST_ENCRYPTION_KEY: &str =
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn test_enc() -> Arc<temps_core::EncryptionService> {
        Arc::new(
            temps_core::EncryptionService::new(TEST_ENCRYPTION_KEY)
                .expect("test encryption service"),
        )
    }

    /// Generate a real self-signed certificate and encrypted private key for
    /// `domain` using rcgen + the test EncryptionService. Returns
    /// `(cert_pem, encrypted_key_pem)` ready to embed in a mock [`domains::Model`].
    fn make_test_cert(domain: &str, enc: &temps_core::EncryptionService) -> (String, String) {
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec![domain.to_string()])
                .expect("rcgen should generate a test certificate");
        let cert_pem = cert.pem();
        let key_pem = signing_key.serialize_pem();
        let encrypted_key = enc.encrypt_string(&key_pem).expect("encrypt test key");
        (cert_pem, encrypted_key)
    }

    /// Build a minimal [`domains::Model`] with the given cert data.
    fn domain_model(domain: &str, cert_pem: &str, enc_key: &str) -> domains::Model {
        let now = Utc::now();
        domains::Model {
            id: 1,
            domain: domain.to_string(),
            certificate: Some(cert_pem.to_string()),
            private_key: Some(enc_key.to_string()),
            expiration_time: None,
            last_renewed: None,
            status: "active".to_string(),
            dns_challenge_token: None,
            dns_challenge_value: None,
            http_challenge_token: None,
            http_challenge_key_authorization: None,
            last_error: None,
            last_error_type: None,
            is_wildcard: false,
            verification_method: "http".to_string(),
            on_demand_backoff_until: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn test_wildcard_domain_extraction() {
        let enc = test_enc();
        let loader = CertificateLoader::new(Arc::new(DbConnection::default()), enc);

        assert_eq!(
            loader.get_wildcard_domain("api.example.com"),
            Some("*.example.com".to_string())
        );
        assert_eq!(
            loader.get_wildcard_domain("www.sub.example.com"),
            Some("*.sub.example.com".to_string())
        );
        assert_eq!(
            loader.get_wildcard_domain("example.com"),
            Some("*.com".to_string())
        );
        // Single-label hostnames have no parent zone.
        assert_eq!(loader.get_wildcard_domain("localhost"), None);
    }

    /// First call hits the database once (exact match); second call is served
    /// entirely from the positive cache with zero DB queries.
    ///
    /// The MockDatabase queue has exactly ONE result. If the second call somehow
    /// goes to the DB, MockDatabase panics ("transaction log is empty") and the
    /// test fails.
    #[tokio::test]
    async fn test_cache_hit_avoids_second_db_call() {
        let enc = test_enc();
        let (cert_pem, enc_key) = make_test_cert("example.com", &enc);
        let model = domain_model("example.com", &cert_pem, &enc_key);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![model]])
            .into_connection();

        let loader = CertificateLoader::new(Arc::new(db), enc);

        // First load: cache miss → 1 DB query → cert cached under "example.com".
        let first = loader
            .load_certificate("example.com")
            .await
            .expect("first load should succeed");
        assert!(first.is_some(), "first call should return a cert");

        // Second load: positive cache hit → 0 DB queries. If this panics, the
        // cache is not working (MockDatabase empty queue → panic).
        let second = loader
            .load_certificate("example.com")
            .await
            .expect("second load should succeed");
        assert!(
            second.is_some(),
            "second call from cache should return a cert"
        );
    }

    /// A full miss (no cert in DB for either exact or wildcard) is cached in the
    /// negative cache. The second call returns None immediately without touching
    /// the DB.
    ///
    /// The MockDatabase queue has exactly TWO empty results (one for exact lookup,
    /// one for wildcard lookup on the first call). If the second call goes to DB,
    /// the empty queue causes a panic and the test fails.
    #[tokio::test]
    async fn test_negative_cache_prevents_repeated_db_call() {
        let enc = test_enc();

        // Two empty results: exact "unknown.example.com" and wildcard "*.example.com".
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![
                Vec::<domains::Model>::new(), // exact lookup → no row
                Vec::<domains::Model>::new(), // wildcard lookup → no row
            ])
            .into_connection();

        let loader = CertificateLoader::new(Arc::new(db), enc);

        // First load: both DB lookups miss → SNI entered into negative cache.
        let first = loader
            .load_certificate("unknown.example.com")
            .await
            .expect("first load should succeed");
        assert!(first.is_none(), "first call should return None (no cert)");

        // Second load: negative cache hit → 0 DB queries.
        let second = loader
            .load_certificate("unknown.example.com")
            .await
            .expect("second load should succeed");
        assert!(
            second.is_none(),
            "second call from negative cache should return None"
        );
    }

    /// After the negative cache entry expires, the loader retries the DB.
    /// Verified by using a 1 ms negative TTL and sleeping past it.
    #[tokio::test]
    async fn test_negative_cache_expiry_retries_db() {
        let enc = test_enc();

        // First call: 2 empty results (exact + wildcard) → negative cache.
        // After expiry, second call: 2 more empty results → new negative cache.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![
                Vec::<domains::Model>::new(), // call 1 exact
                Vec::<domains::Model>::new(), // call 1 wildcard
                Vec::<domains::Model>::new(), // call 2 exact (after expiry)
                Vec::<domains::Model>::new(), // call 2 wildcard (after expiry)
            ])
            .into_connection();

        // Negative cache TTL of 1 ms so we can expire it with a short sleep.
        let loader = CertificateLoader::new_with_ttls(
            Arc::new(db),
            enc,
            Duration::from_secs(300), // positive TTL irrelevant here
            Duration::from_millis(1), // negative TTL: expires after 1 ms
        );

        // First call: DB queries → negative cache.
        let first = loader
            .load_certificate("gone.example.com")
            .await
            .expect("first load should succeed");
        assert!(first.is_none());

        // Wait for the negative cache entry to expire.
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Second call: negative cache expired → DB queried again (uses the 3rd
        // and 4th queued results above).
        let second = loader
            .load_certificate("gone.example.com")
            .await
            .expect("second load after expiry should succeed");
        assert!(second.is_none());
    }

    /// A wildcard cert is cached under the wildcard key (`*.example.com`), not
    /// under the individual SNI. A second lookup from a *different* SNI that
    /// shares the same wildcard base domain finds the cached entry without
    /// touching the DB.
    ///
    /// MockDatabase queue:
    ///  Call 1 ("api.example.com"): exact miss → wildcard hit → cached under "*.example.com"
    ///  Call 2 ("www.example.com"): fast path 3 → wildcard cache hit → 0 DB queries
    #[tokio::test]
    async fn test_wildcard_cert_cached_under_wildcard_key() {
        let enc = test_enc();
        let (cert_pem, enc_key) = make_test_cert("*.example.com", &enc);
        let wildcard_model = domain_model("*.example.com", &cert_pem, &enc_key);

        // Call 1 uses 2 queries: exact miss + wildcard hit.
        // Call 2 should use 0 queries (wildcard cache hit).
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![
                Vec::<domains::Model>::new(), // "api.example.com" exact → miss
                vec![wildcard_model],         // "*.example.com" wildcard → hit
            ])
            .into_connection();

        let loader = CertificateLoader::new(Arc::new(db), enc);

        // Call 1: "api.example.com" → exact miss → wildcard hit → cached.
        let first = loader
            .load_certificate("api.example.com")
            .await
            .expect("first load should succeed");
        assert!(
            first.is_some(),
            "api.example.com should get the wildcard cert"
        );

        // Call 2: "www.example.com" → exact cache miss → wildcard cache HIT
        // (key "*.example.com" is already present). Zero DB queries.
        let second = loader
            .load_certificate("www.example.com")
            .await
            .expect("second load should succeed");
        assert!(
            second.is_some(),
            "www.example.com should get the cached wildcard cert"
        );
    }

    // -------------------------------------------------------------------------
    // Last-known-good (LKG) fallback tests
    // -------------------------------------------------------------------------

    /// After a successful load, once the 5-minute `cert_cache` TTL expires, a
    /// subsequent DB *error* must return the stale cert from `last_known_good`
    /// rather than failing the TLS handshake.
    ///
    /// This is the primary regression test for the fix introduced to guard against
    /// Postgres-outage-induced TLS handshake failures.
    ///
    /// Queue layout:
    ///  - Result 1: valid cert row (primes the cache on the first call)
    ///  - Error  2: simulated DB error (returned when cert_cache has expired)
    #[tokio::test]
    async fn test_stale_cert_served_on_db_error_after_cache_expiry() {
        let enc = test_enc();
        let (cert_pem, enc_key) = make_test_cert("example.com", &enc);
        let model = domain_model("example.com", &cert_pem, &enc_key);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![model]]) // call 1: cert found, cached
            .append_query_errors(vec![sea_orm::DbErr::Custom(
                "simulated postgres outage".to_string(),
            )]) // call 2: DB error after cert_cache expiry
            .into_connection();

        // Short cert TTL (1 ms) so we can expire it in the test. Negative TTL is
        // irrelevant here (we never go negative), but keep it reasonable.
        let loader = CertificateLoader::new_with_ttls(
            Arc::new(db),
            enc,
            Duration::from_millis(1), // cert_ttl: expires quickly
            Duration::from_secs(30),  // negative_ttl: irrelevant
        );

        // First call: cache miss → DB returns cert → populate cert_cache + last_known_good.
        let first = loader
            .load_certificate("example.com")
            .await
            .expect("first load should succeed");
        assert!(first.is_some(), "first call must return a cert");

        // Let the cert_cache entry expire (cert_ttl = 1 ms).
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Second call: cert_cache miss (expired) → DB errors → last_known_good hit →
        // must return the stale cert rather than propagating the DB error.
        let second = loader
            .load_certificate("example.com")
            .await
            .expect("second call must return Ok even though DB errored");
        assert!(
            second.is_some(),
            "stale last-known-good cert must be served when DB errors after cert_cache expiry"
        );
    }

    /// A brand-new SNI that has never been successfully cached before must still
    /// propagate a DB error (nothing to fall back to).
    ///
    /// Confirming that the LKG fallback does not silently swallow errors for
    /// domains that have never been seen.
    #[tokio::test]
    async fn test_db_error_with_no_lkg_propagates_error() {
        let enc = test_enc();

        // Two DB errors: one for the exact lookup, one for the wildcard lookup.
        // (The exact-match call will error first and propagate immediately.)
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors(vec![sea_orm::DbErr::Custom(
                "simulated postgres outage on brand-new SNI".to_string(),
            )])
            .into_connection();

        let loader = CertificateLoader::new(Arc::new(db), enc);

        // No prior successful load → no last_known_good entry → error propagates.
        let result = loader.load_certificate("brandnew.example.com").await;
        assert!(
            result.is_err(),
            "DB error for an uncached SNI must propagate as an error (no LKG to fall back to)"
        );
    }

    /// A genuine "no matching domain" DB response (zero rows, no error) must still
    /// populate the negative cache and return `Ok(None)`.
    ///
    /// This is a regression guard: the LKG error-handling path must not interfere
    /// with the normal zero-row (success) path.
    #[tokio::test]
    async fn test_genuine_no_cert_row_still_negative_caches_and_returns_none() {
        let enc = test_enc();

        // Two empty results: exact + wildcard → genuine "no cert" response.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![
                Vec::<domains::Model>::new(), // exact lookup → no row
                Vec::<domains::Model>::new(), // wildcard lookup → no row
            ])
            .into_connection();

        let loader = CertificateLoader::new(Arc::new(db), enc);

        // Must return Ok(None), not Err, when DB succeeds with zero rows.
        let result = loader
            .load_certificate("missing.example.com")
            .await
            .expect("zero-row DB response must return Ok, not Err");
        assert!(
            result.is_none(),
            "zero-row DB response must return None (negative cache path)"
        );

        // A second call must hit the negative cache (no further DB queries).
        // MockDatabase queue is empty — any DB access would panic here.
        let result2 = loader
            .load_certificate("missing.example.com")
            .await
            .expect("negative cache hit must return Ok");
        assert!(
            result2.is_none(),
            "second call must return None from negative cache"
        );
    }

    /// Exact-match cache entries and wildcard-match cache entries use distinct keys
    /// and do not interfere with each other.
    ///
    /// Scenario:
    ///   - "api.example.com" has its own cert (exact match, cached under "api.example.com").
    ///   - "*.example.com" has a different cert (cached under "*.example.com").
    ///   - Looking up "api.example.com" returns the exact cert, not the wildcard.
    ///   - Looking up "www.example.com" returns the wildcard cert.
    #[tokio::test]
    async fn test_exact_and_wildcard_keys_do_not_interfere() {
        let enc = test_enc();
        let (exact_cert_pem, exact_enc_key) = make_test_cert("api.example.com", &enc);
        let (wildcard_cert_pem, wildcard_enc_key) = make_test_cert("*.example.com", &enc);

        let exact_model = domain_model("api.example.com", &exact_cert_pem, &exact_enc_key);
        let wildcard_model = domain_model("*.example.com", &wildcard_cert_pem, &wildcard_enc_key);

        // Lookups:
        //  "api.example.com" (call 1): exact hit → 1 query.
        //  "www.example.com" (call 2): exact miss + wildcard hit → 2 queries.
        //  "api.example.com" (call 3): exact cache hit → 0 queries.
        //  "www.example.com" (call 4): wildcard cache hit → 0 queries.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![
                vec![exact_model],            // call 1: "api.example.com" exact → hit
                Vec::<domains::Model>::new(), // call 2: "www.example.com" exact → miss
                vec![wildcard_model],         // call 2: "*.example.com" wildcard → hit
            ])
            .into_connection();

        let loader = CertificateLoader::new(Arc::new(db), enc);

        // Call 1: exact hit for "api.example.com".
        let res1 = loader.load_certificate("api.example.com").await.unwrap();
        assert!(res1.is_some(), "api.example.com should get its exact cert");
        let (exact_certs, _) = res1.unwrap();

        // Call 2: exact miss, wildcard hit for "www.example.com".
        let res2 = loader.load_certificate("www.example.com").await.unwrap();
        assert!(
            res2.is_some(),
            "www.example.com should get the wildcard cert"
        );
        let (wildcard_certs, _) = res2.unwrap();

        // The two certs must be distinct (different keys were generated).
        assert_ne!(
            exact_certs[0].as_ref(),
            wildcard_certs[0].as_ref(),
            "exact and wildcard certs must be different"
        );

        // Call 3: exact cache hit for "api.example.com" (no DB).
        let res3 = loader.load_certificate("api.example.com").await.unwrap();
        assert!(res3.is_some());

        // Call 4: wildcard cache hit for "www.example.com" (no DB).
        let res4 = loader.load_certificate("www.example.com").await.unwrap();
        assert!(res4.is_some());
    }
}
