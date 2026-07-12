//! Generated-hostname enumeration, flatten preview/apply, and per-hostname DNS
//! zone reconciliation for managed domains.
//!
//! Only the per-service hostname layout differs between Standard and Flat, so a
//! flatten preview reports the service hostnames that change. The DNS sync
//! reconciles one proxied record per generated hostname against the provider's
//! live zone, pointing each at the configured `edge_target` (an `A`/`AAAA`
//! record for an IP, otherwise a `CNAME`).
//!
//! Safety rules (learned the hard way against a live zone):
//! - **Never delete pre-existing/user records.** Deletion is limited to records
//!   Temps itself tagged as managed. Providers whose API exposes no record
//!   comment (e.g. the cloudflare crate we use) can't carry that tag, so for
//!   them deletion is effectively a no-op — the sync only creates/updates.
//! - **Production environments are excluded** from the generated-DNS sync. The
//!   `edge_target` is the staging/preview edge; production hosts live elsewhere
//!   and must not be pointed at it.

use std::collections::HashMap;
use std::net::IpAddr;

use sea_orm::{DatabaseConnection, EntityTrait};
use temps_core::PublicHostnameStrategy;
use temps_entities::{environments, preset::PresetConfig, projects};

use crate::providers::{DnsProvider, DnsRecord, DnsRecordContent, DnsRecordRequest, DnsRecordType};

/// Record comment Temps stamps on records it manages, so the sync only ever
/// deletes its own records and never user-created ones. Surfaced via a record's
/// `metadata["comment"]` when the provider exposes it.
pub const MANAGED_TAG: &str = "temps:managed";

/// A generated public hostname under a managed domain.
#[derive(Debug, Clone)]
pub struct GeneratedHost {
    /// `"environment"` or `"service"`.
    pub kind: &'static str,
    /// Owning environment id (used as the change row id for display).
    pub owner_id: i32,
    /// Fully-qualified generated hostname.
    pub fqdn: String,
}

/// A generated-hostname change between two strategies.
#[derive(Debug, Clone)]
pub struct HostChange {
    pub kind: String,
    pub id: i32,
    pub old: String,
    pub new: String,
}

/// A DNS record action the sync would perform.
#[derive(Debug, Clone)]
pub struct RecordChange {
    pub action: String,
    pub name: String,
    pub record_type: String,
    pub value: String,
}

/// Combined result of a hostname-mode preview or apply.
#[derive(Debug, Clone, Default)]
pub struct HostnameModeResult {
    pub hostname_changes: Vec<HostChange>,
    pub dns_changes: Vec<RecordChange>,
    /// Whether the provider token can manage this zone (None if not checked).
    pub zone_access_ok: Option<bool>,
}

/// Whether an environment is a production environment (and so excluded from the
/// generated-DNS sync, which targets the staging/preview edge). Matches `prod`
/// or `production` on either the slug or the display name, case-insensitively.
pub fn is_production_env(slug: &str, name: &str) -> bool {
    let is_prod = |s: &str| {
        let s = s.trim().to_ascii_lowercase();
        s == "prod" || s == "production"
    };
    is_prod(slug) || is_prod(name)
}

/// Whether an environment's generated hostnames should be synced to the edge.
fn should_sync_environment(slug: &str, name: &str) -> bool {
    !is_production_env(slug, name)
}

/// Enumerate every generated public hostname under `preview_domain` for the
/// given strategy, **excluding production environments**. Returns environment
/// hostnames and per-public-service hostnames; the latter are the only ones
/// whose layout depends on `strategy`.
///
/// Uses `environments.subdomain` as the canonical per-environment label (not
/// `environment_domains`, which can also hold user-supplied custom FQDNs).
pub async fn enumerate_generated_hosts(
    db: &DatabaseConnection,
    preview_domain: &str,
    strategy: PublicHostnameStrategy,
) -> Vec<GeneratedHost> {
    let envs = environments::Entity::find()
        .all(db)
        .await
        .unwrap_or_default();

    // project_id -> public compose service names
    let public_services: HashMap<i32, Vec<String>> = projects::Entity::find()
        .all(db)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|p| {
            let services = match p.preset_config {
                Some(PresetConfig::DockerCompose(cfg)) => {
                    cfg.public_ports.into_iter().map(|pp| pp.service).collect()
                }
                _ => Vec::new(),
            };
            (p.id, services)
        })
        .collect();

    let mut hosts = Vec::new();
    for env in envs {
        if env.deleted_at.is_some() {
            continue;
        }
        if !should_sync_environment(&env.slug, &env.name) {
            continue;
        }
        let label = env.subdomain.as_str();

        // Environment host (strategy-independent, included for DNS sync coverage).
        hosts.push(GeneratedHost {
            kind: "environment",
            owner_id: env.id,
            fqdn: PublicHostnameStrategy::Standard.environment_hostname(preview_domain, label),
        });

        if let Some(services) = public_services.get(&env.project_id) {
            for service in services {
                hosts.push(GeneratedHost {
                    kind: "service",
                    owner_id: env.id,
                    fqdn: strategy.service_hostname(preview_domain, label, service),
                });
            }
        }
    }

    hosts
}

/// Compute the generated-hostname changes between the current `Standard` layout
/// and `target`. Only service hostnames differ, so environment hosts never
/// appear here.
pub async fn compute_hostname_changes(
    db: &DatabaseConnection,
    preview_domain: &str,
    target: PublicHostnameStrategy,
) -> Vec<HostChange> {
    if target == PublicHostnameStrategy::Standard {
        return Vec::new();
    }
    let before =
        enumerate_generated_hosts(db, preview_domain, PublicHostnameStrategy::Standard).await;
    let after = enumerate_generated_hosts(db, preview_domain, target).await;

    before
        .into_iter()
        .zip(after)
        .filter(|(b, a)| b.fqdn != a.fqdn)
        .map(|(b, a)| HostChange {
            kind: b.kind.to_string(),
            id: b.owner_id,
            old: b.fqdn,
            new: a.fqdn,
        })
        .collect()
}

/// Build the desired DNS record content for a generated hostname, choosing the
/// record type from the shape of `edge_target`.
fn desired_content(edge_target: &str) -> (DnsRecordType, DnsRecordContent, String) {
    if let Ok(ip) = edge_target.parse::<IpAddr>() {
        match ip {
            IpAddr::V4(_) => (
                DnsRecordType::A,
                DnsRecordContent::A {
                    address: edge_target.to_string(),
                },
                "A".to_string(),
            ),
            IpAddr::V6(_) => (
                DnsRecordType::AAAA,
                DnsRecordContent::AAAA {
                    address: edge_target.to_string(),
                },
                "AAAA".to_string(),
            ),
        }
    } else {
        (
            DnsRecordType::CNAME,
            DnsRecordContent::CNAME {
                target: edge_target.to_string(),
            },
            "CNAME".to_string(),
        )
    }
}

/// Extract the comparable value (address/target) from a record's content.
fn record_value(content: &DnsRecordContent) -> Option<String> {
    match content {
        DnsRecordContent::A { address } => Some(address.clone()),
        DnsRecordContent::AAAA { address } => Some(address.clone()),
        DnsRecordContent::CNAME { target } => Some(target.clone()),
        _ => None,
    }
}

/// Whether a record was tagged by Temps as managed (and so is eligible for
/// deletion when no longer desired). Pre-existing/user records are never tagged
/// and are therefore never deleted.
fn record_is_managed(record: &DnsRecord) -> bool {
    record
        .metadata
        .get("comment")
        .map(|c| c == MANAGED_TAG)
        .unwrap_or(false)
}

/// Reconcile the provider's DNS zone so every desired generated hostname has a
/// proxied record pointing at `edge_target`.
///
/// - **Creates** a record for a desired host that doesn't exist.
/// - **Updates** a desired host whose record points somewhere else.
/// - **Deletes** ONLY records Temps tagged as managed that are no longer desired
///   — never pre-existing/user records.
///
/// When `dry_run` is true, nothing is written; the returned [`RecordChange`]
/// list is the plan.
pub async fn reconcile_zone_records(
    provider: &dyn DnsProvider,
    base_domain: &str,
    desired_hosts: &[GeneratedHost],
    edge_target: &str,
    dry_run: bool,
) -> Result<Vec<RecordChange>, crate::errors::DnsError> {
    let suffix = format!(".{}", base_domain.to_ascii_lowercase());
    let desired_fqdns: std::collections::HashSet<String> = desired_hosts
        .iter()
        .map(|h| h.fqdn.to_ascii_lowercase())
        .collect();

    let existing = provider.list_records(base_domain).await?;
    let existing_by_fqdn: HashMap<String, &DnsRecord> = existing
        .iter()
        .map(|r| (r.fqdn.to_ascii_lowercase(), r))
        .collect();

    let (record_type, _content, type_str) = desired_content(edge_target);
    let proxied = provider.capabilities().proxy;
    let mut changes = Vec::new();

    // Create or update desired hosts. Use the upsert `set_record`.
    for host in desired_hosts {
        let fqdn = host.fqdn.to_ascii_lowercase();
        let action = match existing_by_fqdn.get(&fqdn) {
            None => Some("create"),
            Some(rec) => {
                if record_value(&rec.content).as_deref() != Some(edge_target) {
                    Some("update")
                } else {
                    None // already correct
                }
            }
        };
        let Some(action) = action else { continue };

        changes.push(RecordChange {
            action: action.to_string(),
            name: host.fqdn.clone(),
            record_type: type_str.clone(),
            value: edge_target.to_string(),
        });
        if !dry_run {
            let (_t, content, _s) = desired_content(edge_target);
            let name = relative_name(&host.fqdn, &suffix);
            provider
                .set_record(
                    base_domain,
                    DnsRecordRequest {
                        name,
                        content,
                        ttl: None,
                        proxied,
                    },
                )
                .await?;
        }
    }

    // Delete ONLY Temps-tagged records that are no longer desired. Untagged
    // (pre-existing/user) records are never deleted.
    for record in &existing {
        let fqdn = record.fqdn.to_ascii_lowercase();
        if desired_fqdns.contains(&fqdn) || !record_is_managed(record) {
            continue;
        }
        changes.push(RecordChange {
            action: "delete".to_string(),
            name: record.fqdn.clone(),
            record_type: format!("{:?}", record_type),
            value: String::new(),
        });
        if !dry_run {
            let name = relative_name(&record.fqdn, &suffix);
            provider
                .remove_record(base_domain, &name, record_type)
                .await?;
        }
    }

    Ok(changes)
}

/// Strip the zone suffix to get the relative record name (`@` for the apex).
fn relative_name(fqdn: &str, suffix: &str) -> String {
    let fqdn = fqdn.to_ascii_lowercase();
    let base = suffix.trim_start_matches('.');
    if fqdn == base {
        "@".to_string()
    } else if let Some(stripped) = fqdn.strip_suffix(suffix) {
        stripped.to_string()
    } else {
        fqdn
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::DnsError;
    use crate::providers::{
        DnsProvider, DnsProviderCapabilities, DnsProviderType, DnsRecord, DnsRecordContent,
        DnsRecordRequest, DnsRecordType, DnsZone,
    };
    use async_trait::async_trait;
    use std::sync::Mutex;

    fn host(fqdn: &str) -> GeneratedHost {
        GeneratedHost {
            kind: "environment",
            owner_id: 1,
            fqdn: fqdn.to_string(),
        }
    }

    fn record(name: &str, base: &str, ip: &str, managed: bool) -> DnsRecord {
        let fqdn = if name == "@" {
            base.to_string()
        } else {
            format!("{name}.{base}")
        };
        let mut metadata = HashMap::new();
        if managed {
            metadata.insert("comment".to_string(), MANAGED_TAG.to_string());
        }
        DnsRecord {
            id: Some(format!("id-{name}")),
            zone: base.to_string(),
            name: name.to_string(),
            fqdn,
            content: DnsRecordContent::A {
                address: ip.to_string(),
            },
            ttl: 1,
            proxied: true,
            metadata,
        }
    }

    /// In-memory DnsProvider for CF-free reconciliation tests.
    struct MockProvider {
        records: Mutex<Vec<DnsRecord>>,
    }

    impl MockProvider {
        fn new(records: Vec<DnsRecord>) -> Self {
            Self {
                records: Mutex::new(records),
            }
        }
        fn fqdns(&self) -> Vec<String> {
            let mut v: Vec<String> = self
                .records
                .lock()
                .unwrap()
                .iter()
                .map(|r| r.fqdn.clone())
                .collect();
            v.sort();
            v
        }
        fn value_of(&self, fqdn: &str) -> Option<String> {
            self.records
                .lock()
                .unwrap()
                .iter()
                .find(|r| r.fqdn == fqdn)
                .and_then(|r| record_value(&r.content))
        }
    }

    #[async_trait]
    impl DnsProvider for MockProvider {
        fn provider_type(&self) -> DnsProviderType {
            DnsProviderType::Cloudflare
        }
        fn capabilities(&self) -> DnsProviderCapabilities {
            DnsProviderCapabilities {
                proxy: true,
                ..Default::default()
            }
        }
        async fn test_connection(&self) -> Result<bool, DnsError> {
            Ok(true)
        }
        async fn list_zones(&self) -> Result<Vec<DnsZone>, DnsError> {
            Ok(vec![])
        }
        async fn get_zone(&self, _domain: &str) -> Result<Option<DnsZone>, DnsError> {
            Ok(None)
        }
        async fn list_records(&self, _domain: &str) -> Result<Vec<DnsRecord>, DnsError> {
            Ok(self.records.lock().unwrap().clone())
        }
        async fn get_record(
            &self,
            _domain: &str,
            _name: &str,
            _record_type: DnsRecordType,
        ) -> Result<Option<DnsRecord>, DnsError> {
            Ok(None)
        }
        async fn create_record(
            &self,
            domain: &str,
            request: DnsRecordRequest,
        ) -> Result<DnsRecord, DnsError> {
            self.set_record(domain, request).await
        }
        async fn update_record(
            &self,
            domain: &str,
            _record_id: &str,
            request: DnsRecordRequest,
        ) -> Result<DnsRecord, DnsError> {
            self.set_record(domain, request).await
        }
        async fn delete_record(&self, _domain: &str, _record_id: &str) -> Result<(), DnsError> {
            Ok(())
        }
        async fn set_record(
            &self,
            domain: &str,
            request: DnsRecordRequest,
        ) -> Result<DnsRecord, DnsError> {
            let fqdn = if request.name == "@" {
                domain.to_string()
            } else {
                format!("{}.{}", request.name, domain)
            };
            let mut recs = self.records.lock().unwrap();
            if let Some(r) = recs.iter_mut().find(|r| r.fqdn == fqdn) {
                r.content = request.content.clone();
                r.proxied = request.proxied;
                return Ok(r.clone());
            }
            let new = DnsRecord {
                id: Some(format!("id-{}", request.name)),
                zone: domain.to_string(),
                name: request.name.clone(),
                fqdn: fqdn.clone(),
                content: request.content.clone(),
                ttl: request.ttl.unwrap_or(1),
                proxied: request.proxied,
                metadata: HashMap::new(),
            };
            recs.push(new.clone());
            Ok(new)
        }
        async fn remove_record(
            &self,
            domain: &str,
            name: &str,
            _record_type: DnsRecordType,
        ) -> Result<(), DnsError> {
            let fqdn = if name == "@" {
                domain.to_string()
            } else {
                format!("{}.{}", name, domain)
            };
            self.records.lock().unwrap().retain(|r| r.fqdn != fqdn);
            Ok(())
        }
    }

    #[test]
    fn production_environments_are_excluded() {
        assert!(is_production_env("prod", "prod"));
        assert!(is_production_env("production", "Production"));
        assert!(is_production_env("staging", "Production")); // name matches
        assert!(!is_production_env("staging", "staging"));
        assert!(!is_production_env("preview", "preview"));
        assert!(!is_production_env("staging-t", "staging-t"));
    }

    #[test]
    fn desired_content_picks_record_type() {
        assert!(matches!(
            desired_content("35.163.83.53").0,
            DnsRecordType::A
        ));
        assert!(matches!(
            desired_content("2001:db8::1").0,
            DnsRecordType::AAAA
        ));
        assert!(matches!(
            desired_content("edge.temps.sh").0,
            DnsRecordType::CNAME
        ));
    }

    #[test]
    fn relative_name_strips_suffix() {
        assert_eq!(
            relative_name("careowner-staging.cp.careowner.com", ".careowner.com"),
            "careowner-staging.cp"
        );
        assert_eq!(relative_name("careowner.com", ".careowner.com"), "@");
    }

    // The bug we discovered against careowner.com: a domain-wide sync must NEVER
    // delete pre-existing single-label records like app.careowner.com.
    #[tokio::test]
    async fn reconcile_never_deletes_untagged_records() {
        let base = "careowner.com";
        let provider = MockProvider::new(vec![
            record("app", base, "10.0.0.1", false),    // prod, other VM
            record("www", base, "10.0.0.2", false),    // user record
            record("sentry", base, "10.0.0.3", false), // user record
            record("careowner-staging.cp", base, "9.9.9.9", false), // stale generated, untagged
        ]);
        let desired = vec![host("careowner-staging.cp.careowner.com")];

        let changes = reconcile_zone_records(&provider, base, &desired, "35.163.83.53", false)
            .await
            .unwrap();

        // Only the staging record is updated; nothing is deleted.
        assert!(changes.iter().all(|c| c.action != "delete"), "{changes:?}");
        assert!(changes
            .iter()
            .any(|c| c.action == "update" && c.name == "careowner-staging.cp.careowner.com"));

        // app / www / sentry survive untouched.
        let fqdns = provider.fqdns();
        for keep in [
            "app.careowner.com",
            "www.careowner.com",
            "sentry.careowner.com",
        ] {
            assert!(fqdns.contains(&keep.to_string()), "{keep} was removed!");
        }
        assert_eq!(
            provider.value_of("app.careowner.com").as_deref(),
            Some("10.0.0.1")
        );
        // staging now points at the edge.
        assert_eq!(
            provider
                .value_of("careowner-staging.cp.careowner.com")
                .as_deref(),
            Some("35.163.83.53")
        );
    }

    #[tokio::test]
    async fn reconcile_creates_missing_and_skips_correct() {
        let base = "careowner.com";
        let provider = MockProvider::new(vec![
            record("app", base, "10.0.0.1", false),
            // already-correct staging record → no change
            record("careowner-staging.cp", base, "35.163.83.53", false),
        ]);
        let desired = vec![
            host("careowner-staging.cp.careowner.com"), // unchanged
            host("careowner-preview.cp.careowner.com"), // new → create
        ];

        let changes = reconcile_zone_records(&provider, base, &desired, "35.163.83.53", false)
            .await
            .unwrap();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].action, "create");
        assert_eq!(changes[0].name, "careowner-preview.cp.careowner.com");
        assert!(provider
            .fqdns()
            .contains(&"careowner-preview.cp.careowner.com".to_string()));
    }

    #[tokio::test]
    async fn reconcile_dry_run_writes_nothing() {
        let base = "careowner.com";
        let provider = MockProvider::new(vec![record("app", base, "10.0.0.1", false)]);
        let before = provider.fqdns();
        let desired = vec![host("careowner-staging.cp.careowner.com")];

        let changes = reconcile_zone_records(&provider, base, &desired, "35.163.83.53", true)
            .await
            .unwrap();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].action, "create");
        // dry run: zone unchanged.
        assert_eq!(provider.fqdns(), before);
    }

    #[tokio::test]
    async fn reconcile_deletes_only_tagged_stale_records() {
        let base = "careowner.com";
        let provider = MockProvider::new(vec![
            record("app", base, "10.0.0.1", false), // untagged → keep
            record("old-preview.cp", base, "9.9.9.9", true), // tagged + not desired → delete
        ]);
        let desired = vec![host("careowner-staging.cp.careowner.com")];

        let changes = reconcile_zone_records(&provider, base, &desired, "35.163.83.53", false)
            .await
            .unwrap();

        assert!(changes
            .iter()
            .any(|c| c.action == "delete" && c.name == "old-preview.cp.careowner.com"));
        let fqdns = provider.fqdns();
        assert!(fqdns.contains(&"app.careowner.com".to_string()));
        assert!(!fqdns.contains(&"old-preview.cp.careowner.com".to_string()));
    }
}
