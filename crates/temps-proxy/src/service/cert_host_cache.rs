use arc_swap::ArcSwap;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use temps_database::DbConnection;
use temps_entities::domains;
use thiserror::Error;
use tracing::{debug, warn};

use crate::tls_cert_loader::wildcard_for;

/// How often the in-memory cert-host snapshot is reloaded from the database.
///
/// The HTTP→HTTPS redirect check (`host_has_active_cert`) previously issued 2
/// Postgres SELECTs per plain-HTTP request. This snapshot eliminates those queries;
/// the background refresh keeps it current within this window.
///
/// **Staleness note:** a freshly-provisioned certificate may take up to 30 s to
/// start triggering HTTP→HTTPS redirects. This is harmless: TLS itself becomes
/// available immediately via WS2's cert cache (which has a 30 s negative TTL), so
/// the HTTPS endpoint is already reachable — the redirect is just a nicety.
const CERT_HOST_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Error, Debug)]
pub enum CertHostCacheError {
    #[error("Database error querying cert-host snapshot: {0}")]
    Database(#[from] sea_orm::DbErr),
}

/// Snapshot of all domains that currently have both a certificate and a private
/// key stored in the `domains` table.
///
/// Domains whose `domain` field starts with `*.` are placed in `wildcard`; all
/// others go in `exact`. Lookup checks exact first, then derives the wildcard
/// parent (e.g. `"api.example.com"` → `"*.example.com"`) and checks `wildcard`.
#[derive(Debug, Default)]
pub struct CertHostSnapshot {
    /// Domains that have an exact certificate, e.g. `"api.example.com"`.
    pub exact: HashSet<String>,
    /// Wildcard parent domains that have a certificate, e.g. `"*.example.com"`.
    pub wildcard: HashSet<String>,
}

/// In-memory cache of cert-equipped hosts, refreshed on a background interval.
///
/// ## Purpose
///
/// `host_has_active_cert` in `proxy.rs` used to issue 2 Postgres SELECTs (exact +
/// wildcard) on every plain-HTTP request to decide whether to issue an HTTP→HTTPS
/// redirect. This component replaces those queries with an `ArcSwap` load — a
/// single atomic pointer dereference — on the hot path.
///
/// ## Refresh pattern
///
/// Mirrors `IpAccessControlService::run_refresh_loop`: one dedicated thread runs
/// `run_refresh_loop()`, which loads once immediately and then sleeps for
/// `CERT_HOST_REFRESH_INTERVAL` before each subsequent reload. All snapshot reads
/// on the proxy hot path are lock-free via `ArcSwap::load()`.
///
/// ## Consistency with the TLS cert loader (WS2)
///
/// No status filter is applied — any row with both `certificate` and `private_key`
/// populated can serve TLS (matching the `tls_cert_loader.rs` philosophy of not
/// filtering by status during re-issuance). This means the redirect check is
/// consistent with what the TLS layer will actually serve.
pub struct CertHostCache {
    db: Arc<DbConnection>,
    snapshot: Arc<ArcSwap<CertHostSnapshot>>,
}

impl CertHostCache {
    /// Create a new [`CertHostCache`]. The snapshot starts empty (no redirect for
    /// any host) until the first refresh completes.
    pub fn new(db: Arc<DbConnection>) -> Self {
        Self {
            db,
            snapshot: Arc::new(ArcSwap::from_pointee(CertHostSnapshot::default())),
        }
    }

    /// Reload the cert-host snapshot from the database. Executes a single
    /// `SELECT domain FROM domains WHERE certificate IS NOT NULL AND private_key
    /// IS NOT NULL` and splits results into exact vs wildcard sets.
    pub async fn refresh(&self) -> Result<(), CertHostCacheError> {
        let rows = domains::Entity::find()
            .filter(domains::Column::Certificate.is_not_null())
            .filter(domains::Column::PrivateKey.is_not_null())
            .all(self.db.as_ref())
            .await?;

        let mut exact = HashSet::new();
        let mut wildcard = HashSet::new();

        for row in rows {
            if row.domain.starts_with("*.") {
                wildcard.insert(row.domain);
            } else {
                exact.insert(row.domain);
            }
        }

        let total = exact.len() + wildcard.len();
        self.snapshot
            .store(Arc::new(CertHostSnapshot { exact, wildcard }));
        debug!("Refreshed cert-host snapshot: {} domain(s)", total);
        Ok(())
    }

    /// Run the periodic cert-host refresh forever. Loads immediately, then every
    /// `CERT_HOST_REFRESH_INTERVAL`. Spawn once at startup alongside the IP
    /// block-list refresh loop; mirrors `IpAccessControlService::run_refresh_loop`.
    pub async fn run_refresh_loop(self: Arc<Self>) {
        loop {
            if let Err(e) = self.refresh().await {
                warn!("Failed to refresh cert-host snapshot: {}", e);
            }
            tokio::time::sleep(CERT_HOST_REFRESH_INTERVAL).await;
        }
    }

    /// Returns `true` when `host` (or its wildcard parent) has an active TLS cert
    /// in the current snapshot.
    ///
    /// Checks exact match first, then derives the wildcard parent domain using the
    /// same logic as `tls_cert_loader::wildcard_for` (e.g. `"api.example.com"` →
    /// `"*.example.com"`) and checks the `wildcard` set. Lock-free: uses a single
    /// `ArcSwap` pointer load with no blocking.
    pub fn has_cert_for_host(&self, host: &str) -> bool {
        let snap = self.snapshot.load();
        if snap.exact.contains(host) {
            return true;
        }
        if let Some(wc) = wildcard_for(host) {
            snap.wildcard.contains(&wc)
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use sea_orm::{DatabaseBackend, MockDatabase};

    /// Build a minimal domains::Model with the given domain name and both cert +
    /// key present. The actual cert/key content is irrelevant for these tests.
    fn cert_domain(domain: &str) -> domains::Model {
        let now = Utc::now();
        domains::Model {
            id: 1,
            domain: domain.to_string(),
            certificate: Some("CERT_PEM".to_string()),
            private_key: Some("KEY_PEM".to_string()),
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

    // -------------------------------------------------------------------------
    // Lookup tests (purely in-memory, no DB interaction needed)
    // -------------------------------------------------------------------------

    /// An exact-match domain in the snapshot is found by `has_cert_for_host`.
    #[tokio::test]
    async fn test_exact_match_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let cache = CertHostCache::new(Arc::new(db));

        // Populate the snapshot directly without going through the DB.
        let mut exact = HashSet::new();
        exact.insert("api.example.com".to_string());
        cache.snapshot.store(Arc::new(CertHostSnapshot {
            exact,
            wildcard: HashSet::new(),
        }));

        assert!(cache.has_cert_for_host("api.example.com"));
        assert!(!cache.has_cert_for_host("www.example.com")); // different subdomain, not in exact
    }

    /// A wildcard domain in the snapshot matches any subdomain with the same base.
    #[tokio::test]
    async fn test_wildcard_match_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let cache = CertHostCache::new(Arc::new(db));

        let mut wildcard = HashSet::new();
        wildcard.insert("*.example.com".to_string());
        cache.snapshot.store(Arc::new(CertHostSnapshot {
            exact: HashSet::new(),
            wildcard,
        }));

        assert!(cache.has_cert_for_host("api.example.com")); // derives *.example.com
        assert!(cache.has_cert_for_host("www.example.com")); // same wildcard
        assert!(!cache.has_cert_for_host("api.other.com")); // different base domain
    }

    /// Exact match takes precedence; an exact entry does not accidentally match
    /// a host whose base domain has a wildcard cert entry.
    #[tokio::test]
    async fn test_exact_and_wildcard_coexist() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let cache = CertHostCache::new(Arc::new(db));

        let mut exact = HashSet::new();
        exact.insert("api.example.com".to_string());
        let mut wildcard = HashSet::new();
        wildcard.insert("*.other.com".to_string());
        cache
            .snapshot
            .store(Arc::new(CertHostSnapshot { exact, wildcard }));

        assert!(cache.has_cert_for_host("api.example.com")); // exact
        assert!(cache.has_cert_for_host("foo.other.com")); // wildcard
        assert!(!cache.has_cert_for_host("www.example.com")); // neither exact nor wildcard
    }

    /// An empty snapshot returns false for every host — no redirect when no certs
    /// are provisioned (e.g. a fresh install or before the first refresh).
    #[tokio::test]
    async fn test_empty_snapshot_returns_false() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let cache = CertHostCache::new(Arc::new(db));
        // Snapshot is default (empty) — no explicit store needed.
        assert!(!cache.has_cert_for_host("example.com"));
        assert!(!cache.has_cert_for_host("api.example.com"));
        assert!(!cache.has_cert_for_host("localhost"));
    }

    /// A single-label hostname (no dot) never matches a wildcard (wildcard_for
    /// returns None for single-label names).
    #[tokio::test]
    async fn test_single_label_hostname_no_wildcard_match() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let cache = CertHostCache::new(Arc::new(db));

        let mut wildcard = HashSet::new();
        wildcard.insert("*.com".to_string()); // unreachable from "localhost"
        cache.snapshot.store(Arc::new(CertHostSnapshot {
            exact: HashSet::new(),
            wildcard,
        }));

        // "localhost" → wildcard_for("localhost") = None → no wildcard lookup
        assert!(!cache.has_cert_for_host("localhost"));
    }

    // -------------------------------------------------------------------------
    // Refresh tests (uses MockDatabase to simulate DB results)
    // -------------------------------------------------------------------------

    /// After a refresh that returns cert rows, the snapshot reflects those domains.
    #[tokio::test]
    async fn test_refresh_populates_snapshot() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![
                cert_domain("example.com"),
                cert_domain("*.example.com"),
            ]])
            .into_connection();

        let cache = CertHostCache::new(Arc::new(db));
        cache.refresh().await.expect("refresh should succeed");

        // Exact entry is queryable.
        assert!(cache.has_cert_for_host("example.com"));
        // Wildcard entry matches subdomain.
        assert!(cache.has_cert_for_host("api.example.com"));
    }

    /// After a second refresh with updated data, the old snapshot is fully
    /// replaced by the new one.
    #[tokio::test]
    async fn test_refresh_replaces_snapshot() {
        // First refresh: two domains.
        // Second refresh: only one domain remains (simulates cert deletion).
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![
                // First refresh result
                vec![
                    cert_domain("old.example.com"),
                    cert_domain("keep.example.com"),
                ],
                // Second refresh result
                vec![cert_domain("keep.example.com")],
            ])
            .into_connection();

        let cache = CertHostCache::new(Arc::new(db));

        // First refresh.
        cache.refresh().await.expect("first refresh should succeed");
        assert!(cache.has_cert_for_host("old.example.com"));
        assert!(cache.has_cert_for_host("keep.example.com"));

        // Second refresh — old.example.com is gone.
        cache
            .refresh()
            .await
            .expect("second refresh should succeed");
        assert!(!cache.has_cert_for_host("old.example.com"));
        assert!(cache.has_cert_for_host("keep.example.com"));
    }

    /// An empty DB result (no certs) produces an empty snapshot → no redirects.
    #[tokio::test]
    async fn test_refresh_with_empty_db_result() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<domains::Model>::new()])
            .into_connection();

        let cache = CertHostCache::new(Arc::new(db));
        cache.refresh().await.expect("refresh should succeed");

        assert!(!cache.has_cert_for_host("example.com"));
    }
}
