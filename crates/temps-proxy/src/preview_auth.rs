//! Preview gateway authentication for Pingora.
//!
//! When a request hits a hostname matching
//! `ws-<sandbox_hex>-<port>.<preview_domain>`, the proxy looks up the sandbox,
//! checks for a valid preview cookie, and (on success) forwards the request
//! to the local preview gateway at `127.0.0.1:8090`.
//!
//! Unauthenticated requests are redirected to a form-based login page at
//! `/__temps/preview/login` (handled in [`crate::handler::preview_wall`] and
//! `proxy.rs`). HTTP Basic auth is **not** supported — browsers cache Basic
//! credentials unpredictably across subdomains and some clients refuse to
//! send them over plain HTTP. Cookie + form is the only supported flow.
//!
//! Design notes:
//! - The preview gateway itself is a dumb TCP-level reverse proxy bound to
//!   loopback. All authentication happens here in Pingora so the gateway never
//!   needs to talk to the database.
//! - Failures are rate-limited per (client_ip, sandbox_hex) using an in-memory
//!   sliding window. This is best-effort and resets on proxy restart.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use argon2::password_hash::PasswordHash;
use argon2::{Argon2, PasswordVerifier};
use dashmap::DashMap;
use moka::future::Cache;
use sea_orm::EntityTrait;
use temps_core::CookieCrypto;
use temps_database::DbConnection;
use tracing::{debug, warn};

/// Cookie name template for sandbox previews (`temps_preview_sbx_<hex>`).
pub const PREVIEW_SANDBOX_COOKIE_PREFIX: &str = "temps_preview_sbx_";

/// How long a preview session cookie is valid before the user is asked to
/// re-enter the password. Rotating the password invalidates cookies
/// immediately regardless of this TTL.
pub const PREVIEW_COOKIE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// The local TCP address where the preview gateway listens. Pingora forwards
/// authenticated preview requests to this peer.
pub const PREVIEW_GATEWAY_PEER: &str = "127.0.0.1:8090";

/// Maximum number of failed auth attempts allowed per (client_ip, sandbox_hex)
/// inside [`RATE_LIMIT_WINDOW`] before the proxy starts rejecting with 429.
const MAX_FAILURES: u32 = 10;

/// Sliding window for rate limiting failed auth attempts.
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);

/// Parsed preview hostname components.
///
/// `hex` is the 16-hex suffix of the sandbox `sbx_<hex>` public_id, stored
/// lowercase to avoid case-sensitivity bugs in cookie names and log output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewHost {
    pub hex: String,
    pub port: u16,
}

impl PreviewHost {
    /// Human-readable label for logs.
    pub fn label(&self) -> &str {
        &self.hex
    }
}

/// Parse a hostname against `ws-<hex>-<port>.<preview_domain>`.
///
/// `preview_domain` may start with `*.` (wildcard form) — the leading `*.` is
/// stripped before comparison. The label must be exactly 16 hex chars (the
/// suffix of a sandbox `sbx_<hex>` public_id). The port must be a non-zero
/// `u16`.
pub fn parse_preview_host(host: &str, preview_domain: &str) -> Option<PreviewHost> {
    let domain = preview_domain.trim_start_matches("*.");
    let host_no_port = host.split(':').next()?.to_ascii_lowercase();
    let suffix = format!(".{}", domain.to_ascii_lowercase());
    let label = host_no_port.strip_suffix(&suffix)?;

    // label must be `ws-<hex>-<port>`
    let rest = label.strip_prefix("ws-")?;
    let (sid_str, port_str) = rest.rsplit_once('-')?;

    let port: u16 = port_str.parse().ok()?;
    if port == 0 {
        return None;
    }

    if sid_str.len() != 16 || !sid_str.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }

    Some(PreviewHost {
        hex: sid_str.to_ascii_lowercase(),
        port,
    })
}

/// Outcome of preview auth processing.
#[derive(Debug)]
pub enum PreviewAuthOutcome {
    /// Auth succeeded — forward the request to [`PREVIEW_GATEWAY_PEER`].
    Allow { host: PreviewHost },
    /// No valid cookie — reply with 303 redirect to the login form.
    LoginRequired { host: PreviewHost },
    /// Too many failed attempts — reply with 429.
    RateLimited { host: PreviewHost },
    /// Target sandbox does not exist (or DB lookup failed).
    NotFound { host: PreviewHost },
}

/// SHA-256 of the full argon2 PHC hash, truncated to 16 hex chars. Folded
/// into the cookie payload so rotating the password (which changes the
/// argon2 hash) immediately invalidates every live cookie for that sandbox.
pub fn hash_fingerprint(hash: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(hash.as_bytes());
    let digest = hasher.finalize();
    hex::encode(&digest[..8]) // 16 hex chars
}

/// Encode a fresh preview cookie value: `subject|fingerprint|expires_unix`,
/// then encrypted+authenticated by `CookieCrypto` (AES-256-GCM). The subject
/// is the sandbox `sbx_<hex>` public_id and must not contain `|`.
pub fn encode_preview_cookie_subject(
    crypto: &CookieCrypto,
    subject: &str,
    password_hash: &str,
    now: SystemTime,
) -> Option<String> {
    if subject.contains('|') {
        return None;
    }
    let exp = now
        .checked_add(PREVIEW_COOKIE_TTL)?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs();
    let payload = format!("{}|{}|{}", subject, hash_fingerprint(password_hash), exp);
    crypto.encrypt(&payload).ok()
}

/// Validate a previously issued preview cookie. Returns true iff the cookie
/// decrypts cleanly, names the expected `subject`, was minted against the
/// current password hash (so rotation revokes), and has not expired.
pub fn verify_preview_cookie_subject(
    crypto: &CookieCrypto,
    cookie_value: &str,
    subject: &str,
    password_hash: &str,
    now: SystemTime,
) -> bool {
    let Ok(plain) = crypto.decrypt(cookie_value) else {
        return false;
    };
    let parts: Vec<&str> = plain.splitn(3, '|').collect();
    if parts.len() != 3 {
        return false;
    }
    if parts[0] != subject {
        return false;
    }
    if parts[1] != hash_fingerprint(password_hash) {
        return false;
    }
    let Ok(exp) = parts[2].parse::<u64>() else {
        return false;
    };
    let Ok(now_secs) = now.duration_since(UNIX_EPOCH) else {
        return false;
    };
    now_secs.as_secs() <= exp
}

/// Pull a single cookie value out of a `Cookie:` header by name.
pub fn extract_cookie<'a>(cookie_header: &'a str, name: &str) -> Option<&'a str> {
    for pair in cookie_header.split(';') {
        let pair = pair.trim();
        if let Some((k, v)) = pair.split_once('=') {
            if k == name {
                return Some(v);
            }
        }
    }
    None
}

/// Build the `Set-Cookie` header for a sandbox preview cookie.
pub fn build_set_cookie_sandbox(
    public_id_suffix: &str,
    cookie_value: &str,
    preview_domain: &str,
    secure: bool,
) -> String {
    // `Secure` is only emitted when the request came in over TLS. Browsers
    // silently drop `Secure` cookies sent over plain HTTP, which would
    // completely break preview auth for self-hosted setups running without
    // TLS (e.g. `http://host.docker.internal:8080`).
    let domain = preview_domain.trim_start_matches("*.");
    let secure_attr = if secure { "; Secure" } else { "" };
    let ttl = PREVIEW_COOKIE_TTL.as_secs();
    format!(
        "{PREVIEW_SANDBOX_COOKIE_PREFIX}{public_id_suffix}={cookie_value}; Domain=.{domain}; Path=/; HttpOnly{secure_attr}; SameSite=Lax; Max-Age={ttl}"
    )
}

#[derive(Debug, Default)]
struct FailureState {
    count: u32,
    window_start: Option<Instant>,
}

/// Hard cap on distinct (ip, sandbox_hex) pairs tracked concurrently.
/// An attacker spraying unique IPs/hex labels can no longer grow this map
/// without bound — at the cap we sweep expired entries, and if that fails
/// to free space we drop the oldest entry.
const MAX_TRACKED_ENTRIES: usize = 65_536;

/// In-memory rate limiter for preview auth failures.
#[derive(Debug, Default)]
pub struct PreviewAuthLimiter {
    failures: DashMap<(IpAddr, String), FailureState>,
}

impl PreviewAuthLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` if the (ip, sandbox_hex) pair is currently rate-limited.
    pub fn is_blocked(&self, ip: IpAddr, hex: &str) -> bool {
        let entry = self.failures.get(&(ip, hex.to_string()));
        let Some(state) = entry else { return false };
        let Some(start) = state.window_start else {
            return false;
        };
        if start.elapsed() > RATE_LIMIT_WINDOW {
            return false;
        }
        state.count >= MAX_FAILURES
    }

    pub fn record_failure(&self, ip: IpAddr, hex: &str) {
        let key = (ip, hex.to_string());
        // Cap enforcement: opportunistically evict before insert so the map
        // cannot be weaponized as an unbounded memory sink.
        if !self.failures.contains_key(&key) && self.failures.len() >= MAX_TRACKED_ENTRIES {
            self.evict_expired();
            if self.failures.len() >= MAX_TRACKED_ENTRIES {
                if let Some(victim) = self
                    .failures
                    .iter()
                    .min_by_key(|e| e.value().window_start)
                    .map(|e| e.key().clone())
                {
                    self.failures.remove(&victim);
                }
            }
        }

        let mut entry = self.failures.entry(key).or_default();
        let now = Instant::now();
        match entry.window_start {
            Some(start) if start.elapsed() <= RATE_LIMIT_WINDOW => {
                entry.count = entry.count.saturating_add(1);
            }
            _ => {
                entry.window_start = Some(now);
                entry.count = 1;
            }
        }
    }

    pub fn record_success(&self, ip: IpAddr, hex: &str) {
        self.failures.remove(&(ip, hex.to_string()));
    }

    /// Drop all entries whose window has expired. O(n), but only called when
    /// we hit the cap — amortized cost is negligible under normal load.
    fn evict_expired(&self) {
        self.failures.retain(|_, state| match state.window_start {
            Some(start) => start.elapsed() <= RATE_LIMIT_WINDOW,
            None => false,
        });
    }
}

/// Verify a plaintext password against an argon2 PHC hash.
pub fn verify_argon2(plaintext: &str, hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        warn!("preview-auth: stored password hash is malformed");
        return false;
    };
    Argon2::default()
        .verify_password(plaintext.as_bytes(), &parsed)
        .is_ok()
}

/// Outcome of looking up a sandbox for preview auth. Distinguishes the
/// three states that drive routing: doesn't exist, exists with no
/// password (URL-only), exists with a configured password.
#[derive(Debug, Clone)]
pub enum PreviewSandboxLookup {
    /// Sandbox exists and is live, with a password configured. The
    /// gateway should require a valid cookie or redirect to login.
    Protected { password_hash: String },
    /// Sandbox exists and is live but has no password — the unguessable
    /// hex public_id is the only gate. Forward traffic directly.
    Open,
    /// Sandbox does not exist (or is destroyed).
    NotFound,
}

/// Load a sandbox's existence and preview password hash. The result drives
/// three-way routing in `check_preview_auth`: missing → 404, unprotected →
/// Allow, protected → require cookie/login.
///
/// `"stopped"` (paused) sandboxes still resolve so the gateway can surface
/// a 502 from the dev-server side rather than a 404 from the auth side.
pub async fn lookup_sandbox(
    db: &Arc<DbConnection>,
    public_id_suffix: &str,
) -> PreviewSandboxLookup {
    use sea_orm::{ColumnTrait, QueryFilter};
    use temps_entities::sandboxes;

    // The DB stores the full `sbx_<hex>` public_id; rebuild it here.
    let full_public_id = format!("sbx_{}", public_id_suffix);
    match sandboxes::Entity::find()
        .filter(sandboxes::Column::PublicId.eq(full_public_id.clone()))
        .one(db.as_ref())
        .await
    {
        Ok(Some(row)) if row.status == "destroyed" => PreviewSandboxLookup::NotFound,
        Ok(Some(row)) => match row.preview_password_hash {
            Some(hash) => PreviewSandboxLookup::Protected {
                password_hash: hash,
            },
            None => PreviewSandboxLookup::Open,
        },
        Ok(None) => {
            debug!(
                public_id = %full_public_id,
                "preview-auth: sandbox not found"
            );
            PreviewSandboxLookup::NotFound
        }
        Err(e) => {
            warn!(
                public_id = %full_public_id,
                error = %e,
                "preview-auth: failed to load sandbox"
            );
            PreviewSandboxLookup::NotFound
        }
    }
}

/// TTL for sandbox lookup cache entries (both `Protected` and `NotFound`).
///
/// A 30-second window is acceptable because password rotation already
/// invalidates cookies cryptographically (the cookie payload contains a
/// SHA-256 fingerprint of the argon2 PHC hash — see `verify_preview_cookie_subject`).
/// The only effect of a stale cache entry is that a brand-new login attempt
/// for a sandbox whose password was just changed may verify against the old
/// hash for up to 30 s; existing cookies are unaffected.
const SANDBOX_LOOKUP_CACHE_TTL: Duration = Duration::from_secs(30);

/// Maximum number of sandbox lookup results to cache concurrently.
const SANDBOX_LOOKUP_CACHE_MAX_CAPACITY: u64 = 10_000;

/// In-memory cache for `lookup_sandbox` results on the proxy hot path.
///
/// Every request to a `ws-<hex>-<port>.<preview_domain>` host previously
/// issued a `SELECT` against the `sandboxes` table. This cache eliminates
/// that query for the common case of a sandbox the proxy has seen recently.
///
/// Both `Protected { password_hash }` and `NotFound` are cached. Caching
/// `NotFound` is important for sandboxes that no longer exist — without it,
/// repeated requests keep hitting Postgres even for destroyed sandboxes.
///
/// # Password rotation
///
/// Password rotation invalidates existing preview cookies immediately and
/// cryptographically: each cookie payload encodes a SHA-256 fingerprint of
/// the argon2 PHC hash (see [`verify_preview_cookie_subject`]). A stale
/// cache entry (holding the old hash) causes at most a 30-second window where
/// new login POSTs verify against the old hash — all existing cookies are
/// unaffected because the fingerprint in the cookie no longer matches.
pub struct SandboxLookupCache {
    db: Arc<DbConnection>,
    /// `sandbox_hex → PreviewSandboxLookup`. TTL 30 s, cap 10 k.
    cache: Cache<String, PreviewSandboxLookup>,
}

impl SandboxLookupCache {
    /// Create a new [`SandboxLookupCache`] with the production 30-second TTL.
    pub fn new(db: Arc<DbConnection>) -> Self {
        let cache = Cache::builder()
            .max_capacity(SANDBOX_LOOKUP_CACHE_MAX_CAPACITY)
            .time_to_live(SANDBOX_LOOKUP_CACHE_TTL)
            .build();
        Self { db, cache }
    }

    /// Internal constructor for tests — accepts an explicit TTL.
    #[cfg(test)]
    fn new_with_ttl(db: Arc<DbConnection>, ttl: Duration) -> Self {
        let cache = Cache::builder()
            .max_capacity(SANDBOX_LOOKUP_CACHE_MAX_CAPACITY)
            .time_to_live(ttl)
            .build();
        Self { db, cache }
    }

    /// Look up a sandbox by its hex suffix, using the in-memory cache.
    ///
    /// Both hit and miss results are cached so repeated requests for the same
    /// sandbox — including non-existent ones — never amplify into DB load.
    pub async fn lookup(&self, hex: &str) -> PreviewSandboxLookup {
        let key = hex.to_string();

        // Fast path: previous lookup already resolved this hex.
        if let Some(cached) = self.cache.get(&key).await {
            debug!(hex, "sandbox-lookup cache hit (skipping DB)");
            return cached;
        }

        // Cache miss: query the database.
        let result = lookup_sandbox(&self.db, hex).await;
        debug!(
            hex,
            found = !matches!(result, PreviewSandboxLookup::NotFound),
            "sandbox DB lookup; caching result"
        );
        self.cache.insert(key, result.clone()).await;
        result
    }
}

/// Run the preview auth check for a parsed preview host (cookie-only).
///
/// Order of operations:
/// 1. Look up the sandbox row.
/// 2. If missing → NotFound; if unprotected → Allow.
/// 3. Rate-limit gate.
/// 4. If a valid `temps_preview_sbx_<hex>` cookie is present → Allow.
/// 5. Otherwise → LoginRequired (caller issues a 303 to the login form).
///
/// Note: this does NOT record a rate-limit failure for missing cookies —
/// only the login POST records failures (via [`PreviewAuthLimiter::record_failure`])
/// so GETs without a cookie don't lock users out after a browser refresh.
pub async fn check_preview_auth(
    cache: &SandboxLookupCache,
    crypto: &CookieCrypto,
    limiter: &PreviewAuthLimiter,
    host: PreviewHost,
    client_ip: IpAddr,
    cookie_header: Option<&str>,
) -> PreviewAuthOutcome {
    let stored_hash = match cache.lookup(&host.hex).await {
        PreviewSandboxLookup::NotFound => {
            return PreviewAuthOutcome::NotFound { host };
        }
        PreviewSandboxLookup::Open => {
            return PreviewAuthOutcome::Allow { host };
        }
        PreviewSandboxLookup::Protected { password_hash } => {
            if limiter.is_blocked(client_ip, &host.hex) {
                return PreviewAuthOutcome::RateLimited { host };
            }
            password_hash
        }
    };

    let subject = format!("sbx_{}", host.hex);
    let cookie_name = format!("{}{}", PREVIEW_SANDBOX_COOKIE_PREFIX, host.hex);

    if let Some(header) = cookie_header {
        if let Some(value) = extract_cookie(header, &cookie_name) {
            if verify_preview_cookie_subject(
                crypto,
                value,
                &subject,
                &stored_hash,
                SystemTime::now(),
            ) {
                limiter.record_success(client_ip, &host.hex);
                return PreviewAuthOutcome::Allow { host };
            }
        }
    }

    PreviewAuthOutcome::LoginRequired { host }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_fingerprint_differs_for_different_hashes() {
        let hash_a = "$argon2id$v=19$m=19456,t=2,p=1$AAAAAAAAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let hash_b = "$argon2id$v=19$m=19456,t=2,p=1$BBBBBBBBBBBBBBBBBBBBBB$BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
        assert_ne!(hash_fingerprint(hash_a), hash_fingerprint(hash_b));
        assert_eq!(hash_fingerprint(hash_a), hash_fingerprint(hash_a));
        assert_eq!(hash_fingerprint(hash_a).len(), 16);
    }

    #[test]
    fn parse_preview_host_accepts_sandbox_hex() {
        let h = parse_preview_host("ws-7702c56bfb804b49-3000.localho.st", "localho.st").unwrap();
        assert_eq!(h.hex, "7702c56bfb804b49");
        assert_eq!(h.port, 3000);
    }

    #[test]
    fn parse_preview_host_lowercases_sandbox_hex() {
        let h = parse_preview_host("ws-7702C56BFB804B49-3000.localho.st", "localho.st").unwrap();
        assert_eq!(h.hex, "7702c56bfb804b49");
    }

    #[test]
    fn parse_preview_host_strips_wildcard_prefix() {
        let h = parse_preview_host(
            "ws-7702c56bfb804b49-8080.preview.example.com",
            "*.preview.example.com",
        )
        .unwrap();
        assert_eq!(h.hex, "7702c56bfb804b49");
        assert_eq!(h.port, 8080);
    }

    #[test]
    fn parse_preview_host_strips_request_port() {
        let h =
            parse_preview_host("ws-7702c56bfb804b49-3000.localho.st:8443", "localho.st").unwrap();
        assert_eq!(h.hex, "7702c56bfb804b49");
        assert_eq!(h.port, 3000);
    }

    #[test]
    fn parse_preview_host_rejects_wrong_hex_length() {
        // 15 chars, 17 chars: must be exactly 16.
        assert!(parse_preview_host("ws-7702c56bfb804b4-3000.localho.st", "localho.st").is_none());
        assert!(parse_preview_host("ws-7702c56bfb804b49a-3000.localho.st", "localho.st").is_none());
    }

    #[test]
    fn parse_preview_host_rejects_non_hex_mixed_label() {
        assert!(parse_preview_host("ws-gggggggggggggggg-3000.localho.st", "localho.st").is_none());
    }

    #[test]
    fn parse_preview_host_rejects_digit_only_label() {
        // Legacy workspace URLs used pure-digit labels; the sandbox-only
        // parser must reject them. (16 digits would also fail the hex check
        // — digits are valid hex — so test with non-16-length to be explicit.)
        assert!(parse_preview_host("ws-14-3000.localho.st", "localho.st").is_none());
    }

    #[test]
    fn parse_preview_host_rejects_wrong_domain() {
        assert!(parse_preview_host("ws-7702c56bfb804b49-3000.example.org", "localho.st").is_none());
    }

    #[test]
    fn parse_preview_host_rejects_missing_prefix() {
        assert!(parse_preview_host("foo-7702c56bfb804b49-3000.localho.st", "localho.st").is_none());
    }

    #[test]
    fn parse_preview_host_rejects_zero_port() {
        assert!(parse_preview_host("ws-7702c56bfb804b49-0.localho.st", "localho.st").is_none());
    }

    #[test]
    fn rate_limiter_trips_after_max_failures() {
        let limiter = PreviewAuthLimiter::new();
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        for _ in 0..MAX_FAILURES {
            assert!(!limiter.is_blocked(ip, "abc"));
            limiter.record_failure(ip, "abc");
        }
        assert!(limiter.is_blocked(ip, "abc"));
    }

    #[test]
    fn rate_limiter_resets_on_success() {
        let limiter = PreviewAuthLimiter::new();
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        for _ in 0..MAX_FAILURES {
            limiter.record_failure(ip, "abc");
        }
        assert!(limiter.is_blocked(ip, "abc"));
        limiter.record_success(ip, "abc");
        assert!(!limiter.is_blocked(ip, "abc"));
    }

    #[test]
    fn rate_limiter_is_bounded_under_flood() {
        // Spray far more unique (ip, hex) pairs than the cap allows.
        let limiter = PreviewAuthLimiter::new();
        let attacker_count = MAX_TRACKED_ENTRIES + 5_000;
        for i in 0..attacker_count {
            let octet_a = ((i >> 16) & 0xff) as u8;
            let octet_b = ((i >> 8) & 0xff) as u8;
            let octet_c = (i & 0xff) as u8;
            let ip: IpAddr = format!("10.{}.{}.{}", octet_a, octet_b, octet_c)
                .parse()
                .unwrap();
            limiter.record_failure(ip, &format!("hex{:08x}", i % 1024));
        }
        assert!(
            limiter.failures.len() <= MAX_TRACKED_ENTRIES,
            "limiter grew beyond cap: {}",
            limiter.failures.len()
        );
    }

    // -----------------------------------------------------------------------
    // SandboxLookupCache tests
    // -----------------------------------------------------------------------

    use chrono::Utc;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_entities::sandboxes;

    /// Build a minimal sandboxes::Model for use in MockDatabase results.
    fn sandbox_model(
        public_id: &str,
        status: &str,
        password_hash: Option<&str>,
    ) -> sandboxes::Model {
        let now = Utc::now();
        sandboxes::Model {
            id: 1,
            public_id: public_id.to_string(),
            user_id: 1,
            name: "test-sandbox".to_string(),
            status: status.to_string(),
            image: Some("ubuntu:22.04".to_string()),
            work_dir: "/workspace".to_string(),
            timeout_secs: 3600,
            metadata: None,
            created_at: now,
            last_activity_at: now,
            expires_at: now,
            preview_password_hash: password_hash.map(|s| s.to_string()),
            preview_password_hint: None,
        }
    }

    /// A protected sandbox is cached after the first lookup; the second call
    /// returns the cached result without a second DB query.
    #[tokio::test]
    async fn sandbox_cache_hit_avoids_second_db_call() {
        let hash = "$argon2id$v=19$m=19456,t=2,p=1$AAAAAAAAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let model = sandbox_model("sbx_7702c56bfb804b49", "running", Some(hash));

        // Exactly ONE result queued — a second DB call would panic.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![model]])
            .into_connection();

        let cache = SandboxLookupCache::new(Arc::new(db));

        // First call: cache miss → 1 DB query.
        let first = cache.lookup("7702c56bfb804b49").await;
        assert!(
            matches!(first, PreviewSandboxLookup::Protected { .. }),
            "expected Protected, got {:?}",
            first
        );

        // Second call: cache hit → 0 DB queries (MockDatabase would panic if queried).
        let second = cache.lookup("7702c56bfb804b49").await;
        assert!(
            matches!(second, PreviewSandboxLookup::Protected { .. }),
            "expected cached Protected, got {:?}",
            second
        );
    }

    /// A missing sandbox resolves to NotFound which is also cached so repeated
    /// requests for non-existent sandboxes don't amplify into DB load.
    #[tokio::test]
    async fn sandbox_cache_caches_not_found() {
        // ONE empty result — second query would panic.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<sandboxes::Model>::new()])
            .into_connection();

        let cache = SandboxLookupCache::new(Arc::new(db));

        let first = cache.lookup("deadbeef01234567").await;
        assert!(
            matches!(first, PreviewSandboxLookup::NotFound),
            "expected NotFound, got {:?}",
            first
        );

        // Second call served from cache, no DB query.
        let second = cache.lookup("deadbeef01234567").await;
        assert!(matches!(second, PreviewSandboxLookup::NotFound));
    }

    /// After the TTL expires the cache retries the DB.
    #[tokio::test]
    async fn sandbox_cache_retries_after_ttl_expiry() {
        let hash = "$argon2id$v=19$m=19456,t=2,p=1$AAAAAAAAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        // Two results queued: first=NotFound, second=Protected.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![
                Vec::<sandboxes::Model>::new(), // first call: not found
                vec![sandbox_model("sbx_aabbccdd11223344", "running", Some(hash))], // second call after TTL
            ])
            .into_connection();

        // Very short TTL so we can expire it in the test.
        let cache = SandboxLookupCache::new_with_ttl(Arc::new(db), Duration::from_millis(1));

        let first = cache.lookup("aabbccdd11223344").await;
        assert!(matches!(first, PreviewSandboxLookup::NotFound));

        // Wait for TTL to expire.
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Second call after expiry — DB queried again.
        let second = cache.lookup("aabbccdd11223344").await;
        assert!(
            matches!(second, PreviewSandboxLookup::Protected { .. }),
            "expected Protected after TTL expiry, got {:?}",
            second
        );
    }
}
