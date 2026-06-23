//! Wiring for ADR-018 on-demand HTTP-01 TLS (Layer 5).
//!
//! Both proxy startup paths — the standalone `temps proxy`
//! ([`crate::commands::proxy`]) and the single-binary `temps serve`
//! ([`super::proxy`]) — call [`build_on_demand_cert_manager`] to construct the
//! [`OnDemandCertManager`] when `AppSettings.on_demand_tls.enabled` is true. When
//! the feature is disabled (the default) this returns `None`, so the TLS callback
//! keeps its existing fail-fast behavior with zero behavior change.
//!
//! Responsibilities of this layer (everything outside the manager's own gate):
//!   - read [`OnDemandTlsSettings`] at startup and bail to `None` when disabled;
//!   - derive the on-demand zone from `settings.on_demand_tls.zone` or, failing
//!     that, from the `external_url`'s `*.sslip.io` pattern;
//!   - refuse to enable when `external_url` resolves to loopback (local mode):
//!     Let's Encrypt can't reach `127.0.0.1` for the HTTP-01 challenge (ADR §6);
//!   - build the [`OnDemandCertProvisioner`] adapter over
//!     [`DomainService::provision_on_demand`] (the ACME flow stays in
//!     `temps-domains` — this crate only injects the trait);
//!   - rebuild the in-process state cache from the `domains` table at startup
//!     (rows in `on_demand_pending`/`on_demand_issuing`/`on_demand_failed` within
//!     the last 24h, ADR §3) so a fresh proxy doesn't re-enqueue everything.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use temps_core::{AppSettings, EncryptionService};
use temps_database::DbConnection;
use temps_domains::{DefaultCertificateRepository, DomainService, LetsEncryptProvider};
use temps_entities::domains;
use temps_proxy::on_demand_cert::{
    CertProvisionFailure, OnDemandCertConfig, OnDemandCertManager, OnDemandCertProvisioner,
    OnDemandCertState,
};
use temps_proxy::CachedPeerTable;
use tracing::{info, warn};

/// State-cache seed lookback window (ADR §3): rows older than this are treated as
/// stale and not mirrored into the in-process cache on startup.
const STATE_SEED_LOOKBACK_HOURS: i64 = 24;

/// Adapter that satisfies [`OnDemandCertProvisioner`] by driving
/// [`DomainService::provision_on_demand`]. Lives in the wiring layer so the proxy
/// crate stays free of any `temps-domains`/ACME-client dependency (mirrors the
/// `ContainerLifecycleAdapter` pattern for scale-to-zero).
struct DomainServiceProvisioner {
    domain_service: Arc<DomainService>,
    /// ACME contact email resolved at startup from `letsencrypt.email` settings
    /// (the manager re-passes it per call; resolving once at boot avoids a
    /// settings round-trip on every issuance). There is no fallback: if
    /// `letsencrypt.email` is unset, [`Self::resolve_email`] returns empty and
    /// issuance fails cleanly with "no ACME contact email configured" rather
    /// than substituting a system/placeholder address.
    configured_email: Option<String>,
}

impl DomainServiceProvisioner {
    /// Resolve the ACME account email. The configured `letsencrypt.email` is the
    /// single source of truth — there is NO fallback to the first user's email
    /// or a placeholder, because those produced invalid contacts (e.g. the
    /// `system@localhost` system user). Returns empty when unset; the caller
    /// (`provision_on_demand`) treats empty as a clean, logged failure.
    async fn resolve_email(&self) -> String {
        self.configured_email
            .as_ref()
            .map(|e| e.trim().to_string())
            .filter(|e| !e.is_empty())
            .unwrap_or_default()
    }
}

#[async_trait]
impl OnDemandCertProvisioner for DomainServiceProvisioner {
    async fn provision(&self, hostname: &str, _email: &str) -> Result<(), CertProvisionFailure> {
        // The manager hands us the boot-time email it was configured with, but we
        // re-resolve here so a settings change (or a user added after boot) is
        // honored without a proxy restart for the email specifically.
        let email = self.resolve_email().await;
        self.domain_service
            .provision_on_demand(hostname, &email)
            .await
            .map_err(map_provision_error)
    }
}

/// Map a `DomainServiceError` from the on-demand flow into the proxy-side
/// [`CertProvisionFailure`]. `temps-domains` already persisted the authoritative
/// `on_demand_backoff_until` on the `domains` row; here we only mirror the coarse
/// category and the rate-limit `Retry-After` deadline (the one deadline the
/// orchestration layer surfaces to us). When no precise deadline is available we
/// return `None`, and the manager's negative cache falls back to its short first
/// rung — the authoritative DB ladder reasserts itself on the next request.
fn map_provision_error(err: temps_domains::DomainServiceError) -> CertProvisionFailure {
    use temps_domains::DomainServiceError as E;
    let error_chain = err.to_string();
    match &err {
        E::OnDemandRateLimited { retry_after, .. } => CertProvisionFailure {
            error_chain,
            category: "rate_limited".to_string(),
            backoff_until_epoch: retry_after.map(|d| d.timestamp().max(0) as u64),
        },
        E::OnDemandIssuanceFailed { category, .. } => CertProvisionFailure {
            error_chain,
            category: category.clone(),
            backoff_until_epoch: None,
        },
        _ => CertProvisionFailure {
            error_chain,
            category: "internal".to_string(),
            backoff_until_epoch: None,
        },
    }
}

/// Returns true if `external_url` resolves to a loopback host (local mode).
/// On-demand TLS is purposeless there — Let's Encrypt cannot reach `127.0.0.1`
/// for the HTTP-01 challenge (ADR §6).
fn external_url_is_loopback(external_url: &str) -> bool {
    let trimmed = external_url.trim();
    if trimmed.is_empty() {
        return false;
    }

    // Prefer the typed `url::Host` so a bracketed IPv6 literal (`[::1]`) parses as
    // an address rather than a string with brackets. Fall back to a best-effort
    // substring check on the raw input if URL parsing fails entirely.
    if let Ok(parsed) = url::Url::parse(trimmed) {
        match parsed.host() {
            Some(url::Host::Ipv4(addr)) => return addr.is_loopback(),
            Some(url::Host::Ipv6(addr)) => return addr.is_loopback(),
            Some(url::Host::Domain(host)) => {
                let host_lc = host.to_ascii_lowercase();
                if host_lc == "localhost" || host_lc.ends_with(".localhost") {
                    return true;
                }
                // 127.0.0.1.sslip.io and friends — local-mode sslip.io maps to
                // loopback (both dotted and dashed sslip.io encodings).
                return host_lc.starts_with("127.0.0.1") || host_lc.contains("127-0-0-1");
            }
            None => {}
        }
    }

    let host_lc = trimmed.to_ascii_lowercase();
    if host_lc == "localhost" || host_lc.ends_with(".localhost") {
        return true;
    }
    if host_lc.starts_with("127.0.0.1") || host_lc.contains("127-0-0-1") {
        return true;
    }
    host_lc
        .parse::<std::net::IpAddr>()
        .map(|addr| addr.is_loopback())
        .unwrap_or(false)
}

/// Derive the on-demand zone (the allowlist suffix the gate matches direct
/// subdomains against). Resolution order (ADR §2 / §6):
///   1. explicit `settings.on_demand_tls.zone` (operator opt-in for custom
///      domains, or a precomputed sslip.io zone written by `temps setup`);
///   2. auto-derived `<ip>.sslip.io` suffix from a `*.sslip.io` `external_url`.
///
/// Returns `None` when no zone can be derived — the caller then disables the
/// feature (an empty zone makes the gate reject all SNI anyway).
fn derive_zone(settings: &OnDemandZoneInputs) -> Option<String> {
    if let Some(zone) = settings
        .configured_zone
        .as_ref()
        .map(|z| z.trim().trim_end_matches('.'))
        .filter(|z| !z.is_empty())
    {
        return Some(zone.to_ascii_lowercase());
    }

    // Auto-derive from external_url when it is a sslip.io URL.
    let external = settings.external_url.as_ref()?.trim();
    if external.is_empty() {
        return None;
    }
    let host = match url::Url::parse(external) {
        Ok(u) => u.host_str().map(|h| h.to_string()),
        Err(_) => Some(external.to_string()),
    }?;
    let host_lc = host.trim_end_matches('.').to_ascii_lowercase();
    if !host_lc.ends_with("sslip.io") {
        return None;
    }
    // The console/base host on a sslip.io install IS the zone itself
    // (`<ip>.sslip.io`), e.g. external_url `https://1.2.3.4.sslip.io` →
    // zone `1.2.3.4.sslip.io`. App hostnames are `<app>.1.2.3.4.sslip.io`.
    Some(host_lc)
}

/// Inputs to [`derive_zone`], extracted so zone derivation is unit-testable
/// without an `AppSettings` value.
struct OnDemandZoneInputs {
    configured_zone: Option<String>,
    external_url: Option<String>,
}

/// Derive the conventional console host (`console.<zone>`) for the gate's
/// console exemption.
///
/// The installer's quick/sslip.io flow serves the console at `console.<zone>`
/// (e.g. `console.1.2.3.4.sslip.io`) as a fall-through, so it has no
/// `CachedPeerTable` route. The gate exempts exactly this host from the
/// cert-eligible-route check (it is still subject to the in-zone check) so the
/// console gets HTTPS on demand. Returns `None` for an empty zone.
///
/// Note: this only covers the `console.<zone>` convention. A custom-domain
/// (advanced) install serves the console at the bare base domain and ships a
/// wildcard cert that already covers it, so the console there is served from
/// that cert and never reaches the on-demand path — the `console.<zone>`
/// exemption is simply unused in that case (harmless).
fn derive_console_host(zone: &str) -> Option<String> {
    let zone = zone.trim().trim_end_matches('.').to_ascii_lowercase();
    if zone.is_empty() {
        return None;
    }
    Some(format!("console.{zone}"))
}

/// Build the [`OnDemandCertManager`] for a proxy process when on-demand TLS is
/// enabled, or `None` when the feature is off / cannot be safely enabled.
///
/// This runs inside an existing Tokio runtime context (both callers invoke it
/// from `rt.block_on(...)`), so it can spawn the manager's background consumer
/// and perform the startup DB read for the state-cache seed.
///
/// `settings` is the already-fetched `AppSettings` (both startup paths fetch it
/// for `preview_domain`); we reuse it rather than issuing a second query.
pub async fn build_on_demand_cert_manager(
    settings: &AppSettings,
    db: Arc<DbConnection>,
    encryption_service: Arc<EncryptionService>,
    route_table: Arc<CachedPeerTable>,
) -> Option<Arc<OnDemandCertManager>> {
    let cfg = &settings.on_demand_tls;
    if !cfg.enabled {
        return None;
    }

    // Local-mode guard (ADR §6): refuse to enable on loopback and warn.
    if let Some(external) = settings.external_url.as_deref() {
        if external_url_is_loopback(external) {
            warn!(
                external_url = %external,
                "on-demand TLS is enabled but external_url is loopback (local mode); \
                 Let's Encrypt cannot reach 127.0.0.1 for the HTTP-01 challenge. \
                 Disabling on-demand TLS for this process."
            );
            return None;
        }
    }

    let zone = match derive_zone(&OnDemandZoneInputs {
        configured_zone: cfg.zone.clone(),
        external_url: settings.external_url.clone(),
    }) {
        Some(zone) => zone,
        None => {
            warn!(
                "on-demand TLS is enabled but no zone could be derived \
                 (set on_demand_tls.zone, or use a *.sslip.io external_url). \
                 Disabling on-demand TLS for this process."
            );
            return None;
        }
    };

    let email = settings
        .letsencrypt
        .email
        .as_ref()
        .map(|e| e.trim().to_string())
        .filter(|e| !e.is_empty());

    // Warn loudly at boot if on-demand TLS is enabled but has no ACME contact:
    // every issuance will fail cleanly ("no ACME contact email configured"), so
    // the console/app HTTPS will never come up. There is no fallback by design.
    if email.is_none() {
        warn!(
            "on-demand TLS is enabled but no Let's Encrypt contact email is set \
             (settings.letsencrypt.email is empty). Certificate issuance cannot \
             proceed — set a contact email (e.g. re-run `temps setup \
             --letsencrypt-email you@example.com`) to enable HTTPS."
        );
    }

    // Build the DomainService that drives the ACME flow. Reuses the same
    // repository + LetsEncryptProvider construction as the domains plugin so
    // on-demand issuance is byte-for-byte the same code path as manual
    // provisioning (LETSENCRYPT_MODE env still selects staging vs production).
    let repository = Arc::new(DefaultCertificateRepository::new(
        db.clone(),
        encryption_service.clone(),
    ));
    let cert_provider = Arc::new(LetsEncryptProvider::new(repository.clone()));
    let domain_service = Arc::new(DomainService::new(
        db.clone(),
        cert_provider,
        repository,
        encryption_service,
    ));

    let provisioner: Arc<dyn OnDemandCertProvisioner> = Arc::new(DomainServiceProvisioner {
        domain_service,
        configured_email: email.clone(),
    });

    // Derive the console host so the gate exempts it from the cert-eligible
    // route check. On a sslip.io install the console is served at
    // `console.<zone>` (the deploy-script convention; the proxy itself serves
    // the console as a fall-through for any unmatched host, so it has no route
    // table entry). The console is in-zone, stable, and single — exactly the
    // shape on-demand TLS should cover — so `quick` mode gets console HTTPS
    // without the eager HTTP-01 step the old `testing` mode performed.
    let console_host = derive_console_host(&zone);

    let manager = OnDemandCertManager::new(
        OnDemandCertConfig {
            zone: zone.clone(),
            // The provisioner re-resolves the email per call; this is the boot
            // value passed through to it (and surfaced in logs/tests).
            email: email.unwrap_or_default(),
            max_concurrent: cfg.max_concurrent.max(1),
            hourly_cap: cfg.hourly_cap.max(1),
            console_host,
        },
        route_table,
        provisioner,
    );

    // Rebuild the in-process state cache from the domains table (ADR §3).
    match seed_state_from_db(db.as_ref()).await {
        Ok(seed) => {
            let count = seed.len();
            manager.seed_state(seed);
            info!(
                zone = %zone,
                max_concurrent = cfg.max_concurrent.max(1),
                hourly_cap = cfg.hourly_cap.max(1),
                seeded_hosts = count,
                "on-demand TLS enabled: certificate manager started"
            );
        }
        Err(e) => {
            // Non-fatal: a cold cache only risks duplicate no-op jobs that the
            // DB-level WHERE-NOT-EXISTS guard in provision_on_demand absorbs.
            warn!(
                zone = %zone,
                "on-demand TLS enabled but state-cache seed from DB failed: {}. \
                 Starting with a cold cache.",
                e
            );
        }
    }

    Some(manager)
}

/// Read the `domains` rows currently in an on-demand state (within the lookback
/// window) and translate them into in-process [`OnDemandCertState`] seeds.
async fn seed_state_from_db(
    db: &DbConnection,
) -> Result<Vec<(String, OnDemandCertState)>, sea_orm::DbErr> {
    let cutoff = Utc::now() - ChronoDuration::hours(STATE_SEED_LOOKBACK_HOURS);

    let rows = domains::Entity::find()
        .filter(domains::Column::Status.is_in([
            "on_demand_pending",
            "on_demand_issuing",
            "on_demand_failed",
        ]))
        .filter(domains::Column::UpdatedAt.gte(cutoff))
        .all(db)
        .await?;

    let mut seeds = Vec::with_capacity(rows.len());
    for row in rows {
        let state = match row.status.as_str() {
            "on_demand_pending" => OnDemandCertState::Pending,
            "on_demand_issuing" => OnDemandCertState::Issuing,
            "on_demand_failed" => {
                // Mirror the persisted backoff; a failed row with no backoff (or
                // one already elapsed) seeds epoch 0 so the gate treats it as
                // "retry allowed" rather than blocking forever.
                let backoff_until_epoch = row
                    .on_demand_backoff_until
                    .map(|d| d.timestamp().max(0) as u64)
                    .unwrap_or(0);
                OnDemandCertState::Failed {
                    backoff_until_epoch,
                }
            }
            // Filtered out above; defensively skip anything else.
            _ => continue,
        };
        seeds.push((row.domain, state));
    }
    Ok(seeds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_detection() {
        assert!(external_url_is_loopback("http://127.0.0.1.sslip.io"));
        assert!(external_url_is_loopback("https://127.0.0.1.sslip.io:8080"));
        assert!(external_url_is_loopback("http://localhost"));
        assert!(external_url_is_loopback("http://localhost:3000"));
        assert!(external_url_is_loopback("http://127.0.0.1"));
        assert!(external_url_is_loopback("http://127.0.0.1:8080"));
        assert!(external_url_is_loopback("http://[::1]:8080"));

        assert!(!external_url_is_loopback("https://1.2.3.4.sslip.io"));
        assert!(!external_url_is_loopback("https://paas.example.com"));
        assert!(!external_url_is_loopback(""));
    }

    #[test]
    fn zone_from_explicit_setting_wins() {
        let zone = derive_zone(&OnDemandZoneInputs {
            configured_zone: Some("  Apps.Example.COM. ".to_string()),
            external_url: Some("https://1.2.3.4.sslip.io".to_string()),
        });
        // Trimmed, trailing-dot-stripped, lowercased.
        assert_eq!(zone.as_deref(), Some("apps.example.com"));
    }

    #[test]
    fn zone_auto_derived_from_sslip_external_url() {
        let zone = derive_zone(&OnDemandZoneInputs {
            configured_zone: None,
            external_url: Some("https://1.2.3.4.sslip.io".to_string()),
        });
        assert_eq!(zone.as_deref(), Some("1.2.3.4.sslip.io"));
    }

    #[test]
    fn zone_auto_derived_strips_port_and_lowercases() {
        let zone = derive_zone(&OnDemandZoneInputs {
            configured_zone: None,
            external_url: Some("http://1.2.3.4.SSLIP.IO:8080".to_string()),
        });
        assert_eq!(zone.as_deref(), Some("1.2.3.4.sslip.io"));
    }

    #[test]
    fn zone_none_for_custom_domain_without_explicit_setting() {
        // A custom domain with no explicit zone cannot be auto-derived — the
        // operator must opt in. The caller disables the feature in this case.
        let zone = derive_zone(&OnDemandZoneInputs {
            configured_zone: None,
            external_url: Some("https://paas.example.com".to_string()),
        });
        assert_eq!(zone, None);
    }

    #[test]
    fn console_host_derived_from_zone() {
        assert_eq!(
            derive_console_host("1.2.3.4.sslip.io").as_deref(),
            Some("console.1.2.3.4.sslip.io")
        );
    }

    #[test]
    fn console_host_trims_and_lowercases_zone() {
        assert_eq!(
            derive_console_host("  5.6.7.8.SSLIP.IO. ").as_deref(),
            Some("console.5.6.7.8.sslip.io")
        );
    }

    #[test]
    fn console_host_none_for_empty_zone() {
        assert_eq!(derive_console_host("   "), None);
        assert_eq!(derive_console_host(""), None);
    }

    #[test]
    fn zone_none_when_nothing_configured() {
        let zone = derive_zone(&OnDemandZoneInputs {
            configured_zone: None,
            external_url: None,
        });
        assert_eq!(zone, None);
    }

    #[test]
    fn zone_empty_explicit_falls_through_to_external_url() {
        // An empty/whitespace explicit zone is treated as "not set", so a
        // sslip.io external_url still auto-derives.
        let zone = derive_zone(&OnDemandZoneInputs {
            configured_zone: Some("   ".to_string()),
            external_url: Some("https://5.6.7.8.sslip.io".to_string()),
        });
        assert_eq!(zone.as_deref(), Some("5.6.7.8.sslip.io"));
    }

    #[test]
    fn map_rate_limited_error_carries_retry_after_epoch() {
        let deadline = Utc::now() + ChronoDuration::hours(2);
        let err = temps_domains::DomainServiceError::OnDemandRateLimited {
            hostname: "app.1.2.3.4.sslip.io".to_string(),
            detail: "too many certs".to_string(),
            retry_after: Some(deadline),
        };
        let failure = map_provision_error(err);
        assert_eq!(failure.category, "rate_limited");
        assert_eq!(
            failure.backoff_until_epoch,
            Some(deadline.timestamp().max(0) as u64)
        );
    }

    #[test]
    fn map_issuance_failed_error_preserves_category_no_deadline() {
        let err = temps_domains::DomainServiceError::OnDemandIssuanceFailed {
            hostname: "app.1.2.3.4.sslip.io".to_string(),
            category: "challenge_mismatch".to_string(),
            error_chain: "challenge token not served".to_string(),
        };
        let failure = map_provision_error(err);
        assert_eq!(failure.category, "challenge_mismatch");
        assert_eq!(failure.backoff_until_epoch, None);
    }
}
