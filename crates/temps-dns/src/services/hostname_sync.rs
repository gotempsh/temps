//! Generated-hostname enumeration, flatten preview/apply, and per-hostname DNS
//! zone reconciliation for managed domains.
//!
//! Only the per-service hostname layout differs between Standard and Flat, so a
//! flatten preview reports the service hostnames that change. The DNS sync
//! reconciles one proxied record per generated hostname against the provider's
//! live zone, pointing each at the configured `edge_target` (an `A`/`AAAA`
//! record for an IP, otherwise a `CNAME`).

use std::collections::HashMap;
use std::net::IpAddr;

use sea_orm::{DatabaseConnection, EntityTrait};
use temps_core::PublicHostnameStrategy;
use temps_entities::{environment_domains, environments, preset::PresetConfig, projects};

use crate::providers::{DnsProvider, DnsRecordContent, DnsRecordRequest, DnsRecordType};

/// Cloudflare/record comment used to tag records Temps manages, so the sync only
/// ever deletes its own records and never user-created ones.
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

/// Enumerate every generated public hostname under `preview_domain` for the
/// given strategy. Returns environment hostnames and per-public-service
/// hostnames; the latter are the only ones whose layout depends on `strategy`.
pub async fn enumerate_generated_hosts(
    db: &DatabaseConnection,
    preview_domain: &str,
    strategy: PublicHostnameStrategy,
) -> Vec<GeneratedHost> {
    let envs = environments::Entity::find()
        .all(db)
        .await
        .unwrap_or_default();

    // environment_id -> main_url (stable per-env label, e.g. "project-staging")
    let main_urls: HashMap<i32, String> = environment_domains::Entity::find()
        .all(db)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|d| (d.environment_id, d.domain))
        .collect();

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
        let main_url = match main_urls.get(&env.id) {
            Some(u) => u.as_str(),
            None => continue,
        };

        // Environment host (strategy-independent, included for DNS sync coverage).
        hosts.push(GeneratedHost {
            kind: "environment",
            owner_id: env.id,
            fqdn: PublicHostnameStrategy::Standard.environment_hostname(preview_domain, main_url),
        });

        if let Some(services) = public_services.get(&env.project_id) {
            for service in services {
                hosts.push(GeneratedHost {
                    kind: "service",
                    owner_id: env.id,
                    fqdn: strategy.service_hostname(preview_domain, main_url, service),
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
    let before = enumerate_generated_hosts(db, preview_domain, PublicHostnameStrategy::Standard).await;
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

/// Reconcile the provider's DNS zone so every generated hostname under
/// `base_domain` has one proxied record pointing at `edge_target`.
///
/// Returns the set of changes. When `dry_run` is true, nothing is written.
/// Only records that match a generated hostname under this domain are ever
/// deleted, so user-created records are never touched.
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

    // Index existing records by fqdn (only those under this base domain).
    let existing = provider.list_records(base_domain).await?;
    let existing_fqdns: std::collections::HashSet<String> = existing
        .iter()
        .map(|r| r.fqdn.to_ascii_lowercase())
        .collect();

    let (record_type, _content, type_str) = desired_content(edge_target);
    let mut changes = Vec::new();

    // Create records for desired hostnames that don't yet exist.
    for host in desired_hosts {
        let fqdn = host.fqdn.to_ascii_lowercase();
        if existing_fqdns.contains(&fqdn) {
            continue;
        }
        changes.push(RecordChange {
            action: "create".to_string(),
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
                        proxied: provider.capabilities().proxy,
                    },
                )
                .await?;
        }
    }

    // Delete stale records that look like Temps-generated hosts under this base
    // domain but are no longer desired. We scope deletion to records whose name
    // matches the generated single-label pattern, never user records.
    for record in &existing {
        let fqdn = record.fqdn.to_ascii_lowercase();
        if desired_fqdns.contains(&fqdn) {
            continue;
        }
        if !is_generated_candidate(&record.fqdn, &suffix) {
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
                .remove_record(base_domain, &name, record_type.clone())
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

/// Whether a record fqdn looks like a Temps-generated host (exactly one label
/// below the base domain). This intentionally excludes deeper user records and
/// the apex from deletion candidates.
fn is_generated_candidate(fqdn: &str, suffix: &str) -> bool {
    let name = relative_name(fqdn, suffix);
    !name.is_empty() && name != "@" && !name.contains('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desired_content_picks_record_type() {
        assert!(matches!(desired_content("192.0.2.1").0, DnsRecordType::A));
        assert!(matches!(desired_content("2001:db8::1").0, DnsRecordType::AAAA));
        assert!(matches!(
            desired_content("edge.temps.sh").0,
            DnsRecordType::CNAME
        ));
    }

    #[test]
    fn relative_name_strips_suffix() {
        assert_eq!(relative_name("api-staging.example.com", ".example.com"), "api-staging");
        assert_eq!(relative_name("example.com", ".example.com"), "@");
    }

    #[test]
    fn generated_candidate_excludes_apex_and_deep_names() {
        assert!(is_generated_candidate("api-staging.example.com", ".example.com"));
        assert!(!is_generated_candidate("example.com", ".example.com"));
        // Deep (nested) names are not single-label generated candidates.
        assert!(!is_generated_candidate("api.staging.example.com", ".example.com"));
    }
}
