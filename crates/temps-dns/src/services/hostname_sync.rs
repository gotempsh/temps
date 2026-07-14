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
//! - **Never update or delete pre-existing/user records.** Every mutation uses
//!   ADR-031's signed TXT ownership registry; an unowned name is a conflict.
//! - **Only explicitly classified preview environments are included.** The
//!   `edge_target` is the preview edge; inferring production from a slug or
//!   display name is not safe enough for public DNS automation.

use std::collections::HashMap;
use std::net::IpAddr;

use sea_orm::{DatabaseConnection, EntityTrait};
use temps_core::PublicHostnameStrategy;
use temps_entities::{environments, preset::PresetConfig, projects};

use crate::errors::DnsError;
use crate::ownership::{check_proxy_allowed, record_fingerprint};
use crate::providers::{DnsProvider, DnsRecordContent, DnsRecordRequest, DnsRecordType};
use crate::services::{ManagedDnsRecordService, OwnershipScope, RecordOwnership};

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
#[derive(Debug, Clone, serde::Serialize)]
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

/// Whether an environment's generated hostnames should be synced to the
/// preview edge. This uses the persisted classification, never a name guess.
fn should_sync_environment(is_preview: bool) -> bool {
    is_preview
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
) -> Result<Vec<GeneratedHost>, DnsError> {
    let envs = environments::Entity::find().all(db).await?;

    // project_id -> public compose service names
    let public_services: HashMap<i32, Vec<String>> = projects::Entity::find()
        .all(db)
        .await?
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
        if !should_sync_environment(env.is_preview) {
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

    Ok(hosts)
}

/// Compute the generated-hostname changes between the current `Standard` layout
/// and `target`. Only service hostnames differ, so environment hosts never
/// appear here.
pub async fn compute_hostname_changes(
    db: &DatabaseConnection,
    preview_domain: &str,
    target: PublicHostnameStrategy,
) -> Result<Vec<HostChange>, DnsError> {
    if target == PublicHostnameStrategy::Standard {
        return Ok(Vec::new());
    }
    let before =
        enumerate_generated_hosts(db, preview_domain, PublicHostnameStrategy::Standard).await?;
    let after = enumerate_generated_hosts(db, preview_domain, target).await?;

    Ok(before
        .into_iter()
        .zip(after)
        .filter(|(b, a)| b.fqdn != a.fqdn)
        .map(|(b, a)| HostChange {
            kind: b.kind.to_string(),
            id: b.owner_id,
            old: b.fqdn,
            new: a.fqdn,
        })
        .collect())
}

/// Build the desired DNS record content for a generated hostname, choosing the
/// record type from the shape of `edge_target`.
pub(crate) fn desired_content(edge_target: &str) -> (DnsRecordType, DnsRecordContent, String) {
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

/// Reconcile the provider's DNS zone so every desired generated hostname has a
/// record pointing at `edge_target`.
///
/// - **Creates** a record for a desired host that doesn't exist.
/// - **Updates** only a desired host carrying this installation's signed marker.
/// - **Deletes** only signed, owned records that are no longer desired.
///
/// When `dry_run` is true, nothing is written; the returned [`RecordChange`]
/// list is the plan.
pub struct ReconcileOptions<'a> {
    pub proxied: bool,
    pub instance_id: &'a str,
    pub signing_key: &'a [u8; 32],
    pub dry_run: bool,
}

pub async fn reconcile_zone_records(
    provider: &dyn DnsProvider,
    base_domain: &str,
    desired_hosts: &[GeneratedHost],
    edge_target: &str,
    options: ReconcileOptions<'_>,
) -> Result<Vec<RecordChange>, crate::errors::DnsError> {
    let ReconcileOptions {
        proxied,
        instance_id,
        signing_key,
        dry_run,
    } = options;
    let suffix = format!(".{}", base_domain.to_ascii_lowercase());
    let desired_fqdns: std::collections::HashSet<String> = desired_hosts
        .iter()
        .map(|h| h.fqdn.to_ascii_lowercase())
        .collect();

    let existing = provider.list_records(base_domain).await?;
    let (record_type, content, type_str) = desired_content(edge_target);
    let provider_name = provider.provider_type().to_string();
    ManagedDnsRecordService::validate_provider_capabilities(
        &provider.capabilities(),
        &provider_name,
        record_type,
    )?;
    let desired_fingerprint = record_fingerprint(&content, proxied)?;
    let mut changes = Vec::new();
    let mut planned_sets = Vec::new();
    let mut planned_removals = Vec::new();

    // Create or update desired hosts through the signed ownership guard.
    for host in desired_hosts {
        let name = relative_name(&host.fqdn, &suffix)?;
        let request = DnsRecordRequest {
            name: name.clone(),
            content: content.clone(),
            ttl: None,
            proxied,
        };
        ManagedDnsRecordService::validate_record_request(base_domain, &request)?;
        if proxied {
            check_proxy_allowed(&provider.capabilities(), &provider_name, base_domain, &name)?;
        }
        let ownership = ManagedDnsRecordService::ownership_of(
            provider,
            base_domain,
            &name,
            record_type,
            instance_id,
            signing_key,
        )
        .await?;
        let action = match &ownership {
            RecordOwnership::NotFound => Some("create"),
            RecordOwnership::Orphaned(marker)
                if marker.controller.as_deref() == Some("generated-hostname") =>
            {
                Some("create")
            }
            RecordOwnership::Owned(record, marker)
                if marker.controller.as_deref() == Some("generated-hostname")
                    && record_fingerprint(&record.content, record.proxied)?
                        != desired_fingerprint =>
            {
                Some("update")
            }
            RecordOwnership::Owned(_, marker)
                if marker.controller.as_deref() == Some("generated-hostname") =>
            {
                None
            }
            RecordOwnership::Orphaned(_)
            | RecordOwnership::Owned(_, _)
            | RecordOwnership::Unmanaged(_)
            | RecordOwnership::OwnedByOther(_, _)
            | RecordOwnership::BlockedByOther(_)
            | RecordOwnership::RegistryConflict => Some("conflict"),
        };
        let Some(action) = action else { continue };

        changes.push(RecordChange {
            action: action.to_string(),
            name: host.fqdn.clone(),
            record_type: type_str.clone(),
            value: edge_target.to_string(),
        });
        if action == "conflict" {
            if !dry_run {
                return Err(DnsError::RecordConflict {
                    domain: base_domain.to_string(),
                    name,
                    record_type: record_type.to_string(),
                    reason: "generated hostname is already managed by another owner or has no valid generated-hostname marker".to_string(),
                });
            }
            continue;
        }
        planned_sets.push((request, host.owner_id));
    }

    // Delete only records whose signed marker proves this install owns them.
    for record in &existing {
        let fqdn = record.fqdn.to_ascii_lowercase();
        let stale_type = record.content.record_type();
        if desired_fqdns.contains(&fqdn)
            || !matches!(
                stale_type,
                DnsRecordType::A | DnsRecordType::AAAA | DnsRecordType::CNAME
            )
        {
            continue;
        }
        let name = relative_name(&record.fqdn, &suffix)?;
        let ownership = ManagedDnsRecordService::ownership_of(
            provider,
            base_domain,
            &name,
            stale_type,
            instance_id,
            signing_key,
        )
        .await?;
        if !matches!(
            ownership,
            RecordOwnership::Owned(
                _,
                ref marker
            ) if marker.controller.as_deref() == Some("generated-hostname")
        ) {
            continue;
        }
        changes.push(RecordChange {
            action: "delete".to_string(),
            name: record.fqdn.clone(),
            record_type: stale_type.to_string(),
            value: String::new(),
        });
        planned_removals.push((name, stale_type));
    }

    if !dry_run {
        for (request, environment_id) in planned_sets {
            let _record_lock =
                ManagedDnsRecordService::lock_record(base_domain, &request.name).await;
            ManagedDnsRecordService::guarded_set(
                provider,
                base_domain,
                request,
                instance_id,
                signing_key,
                OwnershipScope {
                    project_id: None,
                    environment_id: Some(environment_id),
                    controller: Some("generated-hostname"),
                },
            )
            .await?;
        }
        for (name, stale_type) in planned_removals {
            let _record_lock = ManagedDnsRecordService::lock_record(base_domain, &name).await;
            ManagedDnsRecordService::guarded_remove(
                provider,
                base_domain,
                &name,
                stale_type,
                instance_id,
                signing_key,
            )
            .await?;
        }
    }

    Ok(changes)
}

/// Strip the zone suffix to get the relative record name (`@` for the apex).
pub(crate) fn relative_name(fqdn: &str, suffix: &str) -> Result<String, DnsError> {
    let fqdn = fqdn.to_ascii_lowercase();
    let base = suffix.trim_start_matches('.');
    if fqdn == base {
        Ok("@".to_string())
    } else if let Some(stripped) = fqdn.strip_suffix(suffix) {
        Ok(stripped.to_string())
    } else {
        Err(DnsError::Validation(format!(
            "Generated hostname '{fqdn}' is outside managed zone '{base}'"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::DnsError;
    use crate::ownership::{registry_record_name, OwnershipMarker};
    use crate::providers::{
        DnsProvider, DnsProviderCapabilities, DnsProviderType, DnsRecord, DnsRecordContent,
        DnsRecordRequest, DnsRecordType, DnsZone,
    };
    use async_trait::async_trait;
    use sea_orm::{DatabaseBackend, DbErr, MockDatabase};
    use std::sync::Mutex;

    const INSTANCE: &str = "test-install";
    const SIGNING_KEY: [u8; 32] = [29; 32];

    fn host(fqdn: &str) -> GeneratedHost {
        GeneratedHost {
            kind: "environment",
            owner_id: 1,
            fqdn: fqdn.to_string(),
        }
    }

    fn record(name: &str, base: &str, ip: &str) -> DnsRecord {
        let fqdn = if name == "@" {
            base.to_string()
        } else {
            format!("{name}.{base}")
        };
        DnsRecord {
            id: Some(format!("id-{name}")),
            zone: base.to_string(),
            name: name.to_string(),
            fqdn,
            content: DnsRecordContent::A {
                address: ip.to_string(),
            },
            ttl: 1,
            proxied: false,
            metadata: HashMap::new(),
        }
    }

    fn owned_records(name: &str, base: &str, ip: &str) -> Vec<DnsRecord> {
        owned_records_for_controller(name, base, ip, Some("generated-hostname"))
    }

    fn owned_records_for_controller(
        name: &str,
        base: &str,
        ip: &str,
        controller: Option<&str>,
    ) -> Vec<DnsRecord> {
        let target = record(name, base, ip);
        let fingerprint = record_fingerprint(&target.content, target.proxied).unwrap();
        let marker = OwnershipMarker::new_signed(
            &SIGNING_KEY,
            INSTANCE,
            base,
            name,
            DnsRecordType::A,
            &fingerprint,
            None,
            Some(1),
            controller,
        )
        .unwrap();
        let marker_name = registry_record_name(name, DnsRecordType::A);
        let marker_record = DnsRecord {
            id: Some(format!("id-{marker_name}")),
            zone: base.to_string(),
            name: marker_name.clone(),
            fqdn: format!("{marker_name}.{base}"),
            content: DnsRecordContent::TXT {
                content: marker.to_txt_content().unwrap(),
            },
            ttl: 1,
            proxied: false,
            metadata: HashMap::new(),
        };
        vec![target, marker_record]
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
                .and_then(|record| match &record.content {
                    DnsRecordContent::A { address } | DnsRecordContent::AAAA { address } => {
                        Some(address.clone())
                    }
                    DnsRecordContent::CNAME { target } => Some(target.clone()),
                    _ => None,
                })
        }
    }

    #[async_trait]
    impl DnsProvider for MockProvider {
        fn provider_type(&self) -> DnsProviderType {
            DnsProviderType::Cloudflare
        }
        fn capabilities(&self) -> DnsProviderCapabilities {
            DnsProviderCapabilities {
                a_record: true,
                aaaa_record: true,
                cname_record: true,
                txt_record: true,
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
        async fn delete_record(&self, _domain: &str, record_id: &str) -> Result<(), DnsError> {
            self.records
                .lock()
                .unwrap()
                .retain(|record| record.id.as_deref() != Some(record_id));
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
    fn only_explicit_preview_environments_are_included() {
        assert!(should_sync_environment(true));
        assert!(!should_sync_environment(false));
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

    #[tokio::test]
    async fn generated_host_enumeration_propagates_database_errors() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([DbErr::Custom("environment query unavailable".to_string())])
            .into_connection();

        let error =
            enumerate_generated_hosts(&db, "preview.example.com", PublicHostnameStrategy::Standard)
                .await
                .unwrap_err();

        assert!(matches!(error, DnsError::Database(_)));
    }

    #[test]
    fn relative_name_strips_suffix() {
        assert_eq!(
            relative_name("careowner-staging.cp.careowner.com", ".careowner.com").unwrap(),
            "careowner-staging.cp"
        );
        assert_eq!(
            relative_name("careowner.com", ".careowner.com").unwrap(),
            "@"
        );
        assert!(relative_name("outside.example.net", ".careowner.com").is_err());
    }

    // The bug we discovered against careowner.com: a domain-wide sync must NEVER
    // delete pre-existing single-label records like app.careowner.com.
    #[tokio::test]
    async fn reconcile_refuses_to_update_untagged_records() {
        let base = "careowner.com";
        let provider = MockProvider::new(vec![
            record("app", base, "10.0.0.1"),
            record("www", base, "10.0.0.2"),
            record("sentry", base, "10.0.0.3"),
            record("careowner-staging.cp", base, "9.9.9.9"),
        ]);
        let desired = vec![host("careowner-staging.cp.careowner.com")];

        let error = reconcile_zone_records(
            &provider,
            base,
            &desired,
            "35.163.83.53",
            ReconcileOptions {
                proxied: false,
                instance_id: INSTANCE,
                signing_key: &SIGNING_KEY,
                dry_run: false,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, DnsError::RecordConflict { .. }));

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
        assert_eq!(
            provider
                .value_of("careowner-staging.cp.careowner.com")
                .as_deref(),
            Some("9.9.9.9")
        );
    }

    #[tokio::test]
    async fn reconcile_creates_missing_and_skips_correct() {
        let base = "careowner.com";
        let mut records = vec![record("app", base, "10.0.0.1")];
        records.extend(owned_records("careowner-staging.cp", base, "35.163.83.53"));
        let provider = MockProvider::new(records);
        let desired = vec![
            host("careowner-staging.cp.careowner.com"), // unchanged
            host("careowner-preview.cp.careowner.com"), // new → create
        ];

        let changes = reconcile_zone_records(
            &provider,
            base,
            &desired,
            "35.163.83.53",
            ReconcileOptions {
                proxied: false,
                instance_id: INSTANCE,
                signing_key: &SIGNING_KEY,
                dry_run: false,
            },
        )
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
        let provider = MockProvider::new(vec![record("app", base, "10.0.0.1")]);
        let before = provider.fqdns();
        let desired = vec![host("careowner-staging.cp.careowner.com")];

        let changes = reconcile_zone_records(
            &provider,
            base,
            &desired,
            "35.163.83.53",
            ReconcileOptions {
                proxied: false,
                instance_id: INSTANCE,
                signing_key: &SIGNING_KEY,
                dry_run: true,
            },
        )
        .await
        .unwrap();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].action, "create");
        // dry run: zone unchanged.
        assert_eq!(provider.fqdns(), before);
    }

    #[tokio::test]
    async fn reconcile_deletes_only_signed_owned_stale_records() {
        let base = "careowner.com";
        let mut records = vec![record("app", base, "10.0.0.1")];
        records.extend(owned_records("old-preview.cp", base, "9.9.9.9"));
        records.extend(owned_records_for_controller(
            "manual-owned",
            base,
            "9.9.9.8",
            None,
        ));
        let provider = MockProvider::new(records);
        let desired = vec![host("careowner-staging.cp.careowner.com")];

        let changes = reconcile_zone_records(
            &provider,
            base,
            &desired,
            "35.163.83.53",
            ReconcileOptions {
                proxied: false,
                instance_id: INSTANCE,
                signing_key: &SIGNING_KEY,
                dry_run: false,
            },
        )
        .await
        .unwrap();

        assert!(changes
            .iter()
            .any(|c| c.action == "delete" && c.name == "old-preview.cp.careowner.com"));
        let fqdns = provider.fqdns();
        assert!(fqdns.contains(&"app.careowner.com".to_string()));
        assert!(fqdns.contains(&"manual-owned.careowner.com".to_string()));
        assert!(!fqdns.contains(&"old-preview.cp.careowner.com".to_string()));
    }

    #[tokio::test]
    async fn reconcile_refuses_to_claim_a_manual_owned_desired_record() {
        let base = "careowner.com";
        let mut records =
            owned_records_for_controller("careowner-staging.cp", base, "9.9.9.9", None);
        records.extend(owned_records("unrelated-preview.cp", base, "35.163.83.53"));
        let provider = MockProvider::new(records);
        let before = provider.fqdns();

        let error = reconcile_zone_records(
            &provider,
            base,
            &[host("careowner-staging.cp.careowner.com")],
            "35.163.83.53",
            ReconcileOptions {
                proxied: false,
                instance_id: INSTANCE,
                signing_key: &SIGNING_KEY,
                dry_run: false,
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(error, DnsError::RecordConflict { .. }));
        assert_eq!(provider.fqdns(), before);
        assert_eq!(
            provider
                .value_of("careowner-staging.cp.careowner.com")
                .as_deref(),
            Some("9.9.9.9")
        );
    }
}
