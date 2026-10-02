// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;

use sea_orm::{DatabaseConnection, DatabaseTransaction, EntityTrait};
use temps_core::PublicHostnameStrategy;
use temps_entities::{environments, preset::PresetConfig, projects};

use crate::errors::DnsError;
use crate::ownership::{
    check_proxy_allowed, record_fingerprint, registry_record_name, OwnershipMarker,
    OWNERSHIP_REGISTRY_PREFIX,
};
use crate::providers::{DnsProvider, DnsRecord, DnsRecordContent, DnsRecordRequest, DnsRecordType};
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

    // project_id -> hostname labels of the public compose routes
    let public_services: HashMap<i32, Vec<String>> = projects::Entity::find()
        .all(db)
        .await?
        .into_iter()
        .map(|p| {
            let services = match p.preset_config {
                Some(PresetConfig::DockerCompose(cfg)) => {
                    temps_entities::preset::compose_public_route_labels(&cfg.public_ports)
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

/// Controller name stamped into — and required from — every ownership marker
/// this reconciler writes, so it never claims records of another workflow.
const GENERATED_HOSTNAME_CONTROLLER: &str = "generated-hostname";

/// Record types a generated hostname can be published as.
const ROUTING_TYPES: [DnsRecordType; 3] =
    [DnsRecordType::A, DnsRecordType::AAAA, DnsRecordType::CNAME];

fn is_generated(marker: &OwnershipMarker) -> bool {
    marker.controller.as_deref() == Some(GENERATED_HOSTNAME_CONTROLLER)
}

/// Whether records of these two routing types cannot share a name. A CNAME
/// cannot coexist with any other data at its name; A and AAAA can.
fn types_conflict(a: DnsRecordType, b: DnsRecordType) -> bool {
    a != b && (a == DnsRecordType::CNAME || b == DnsRecordType::CNAME)
}

/// Fully-qualified, lowercased name of a relative record name (`@` = apex).
fn fqdn_of(name: &str, zone: &str) -> String {
    let zone = zone.to_ascii_lowercase();
    if name == "@" || name.is_empty() {
        zone
    } else {
        format!("{}.{zone}", name.to_ascii_lowercase())
    }
}

/// Plan row for deleting an orphaned ownership marker (no target record).
fn marker_removal_change(name: &str, record_type: DnsRecordType, zone: &str) -> RecordChange {
    RecordChange {
        action: "delete".to_string(),
        name: fqdn_of(&registry_record_name(name, record_type), zone),
        record_type: DnsRecordType::TXT.to_string(),
        value: String::new(),
    }
}

/// State of an ownership registry TXT as seen in a [`ZoneSnapshot`]; mirrors
/// the registry classification `ManagedDnsRecordService` performs per record.
enum SnapshotRegistry {
    Absent,
    Owned(OwnershipMarker),
    Foreign(OwnershipMarker),
    Occupied,
}

/// One `list_records` result for a zone, indexed by (lowercased relative
/// name, record type).
///
/// Planning reads ownership from this index instead of issuing per-record
/// provider lookups, so a sync costs one listing plus guarded calls for the
/// records it actually changes — independent of how many unrelated records the
/// zone holds.
///
/// The snapshot only decides *what to try*. Every write still goes through
/// `guarded_set`/`guarded_remove`, which re-read the record and its signed
/// marker under the per-record lock, so a stale snapshot can make a change
/// fail but can never authorize touching a record temps does not own.
struct ZoneSnapshot {
    zone: String,
    index: HashMap<(String, String), Vec<DnsRecord>>,
}

impl ZoneSnapshot {
    async fn load(provider: &dyn DnsProvider, zone: &str) -> Result<Self, DnsError> {
        let records = provider.list_records(zone).await?;
        Ok(Self::from_records(zone, records))
    }

    fn from_records(zone: &str, records: Vec<DnsRecord>) -> Self {
        let zone = zone.to_ascii_lowercase();
        let suffix = format!(".{zone}");
        let mut index: HashMap<(String, String), Vec<DnsRecord>> = HashMap::new();
        for record in records {
            // A record reported outside this zone is never managed through it.
            let Ok(name) = relative_name(&record.fqdn, &suffix) else {
                continue;
            };
            index
                .entry((name, record.content.record_type().to_string()))
                .or_default()
                .push(record);
        }
        Self { zone, index }
    }

    fn records_at(&self, name: &str, record_type: DnsRecordType) -> &[DnsRecord] {
        self.index
            .get(&(name.to_ascii_lowercase(), record_type.to_string()))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn registry_state(
        &self,
        name: &str,
        record_type: DnsRecordType,
        instance: &str,
        signing_key: &[u8; 32],
    ) -> SnapshotRegistry {
        let registry =
            self.records_at(&registry_record_name(name, record_type), DnsRecordType::TXT);
        let [record] = registry else {
            return if registry.is_empty() {
                SnapshotRegistry::Absent
            } else {
                SnapshotRegistry::Occupied
            };
        };
        let DnsRecordContent::TXT { content } = &record.content else {
            return SnapshotRegistry::Occupied;
        };
        match OwnershipMarker::parse(content) {
            None => SnapshotRegistry::Occupied,
            Some(marker) if !marker.is_owned_by(instance) => SnapshotRegistry::Foreign(marker),
            Some(marker) if marker.covers(signing_key, instance, &self.zone, name, record_type) => {
                SnapshotRegistry::Owned(marker)
            }
            Some(_) => SnapshotRegistry::Occupied,
        }
    }

    /// Ownership of (name, type) from the snapshot, with the same semantics as
    /// `ManagedDnsRecordService::ownership_of` (signature, location and
    /// content fingerprint must all match for `Owned`).
    fn ownership(
        &self,
        name: &str,
        record_type: DnsRecordType,
        instance: &str,
        signing_key: &[u8; 32],
    ) -> Result<RecordOwnership, DnsError> {
        let registry = self.registry_state(name, record_type, instance, signing_key);
        let record = match self.records_at(name, record_type) {
            [] => {
                return Ok(match registry {
                    SnapshotRegistry::Absent => RecordOwnership::NotFound,
                    SnapshotRegistry::Owned(marker) => RecordOwnership::Orphaned(marker),
                    SnapshotRegistry::Foreign(marker) => RecordOwnership::BlockedByOther(marker),
                    SnapshotRegistry::Occupied => RecordOwnership::RegistryConflict,
                })
            }
            [record] => record.clone(),
            [first, ..] => return Ok(RecordOwnership::Unmanaged(first.clone())),
        };
        Ok(match registry {
            SnapshotRegistry::Owned(marker)
                if marker
                    .matches_fingerprint(&record_fingerprint(&record.content, record.proxied)?) =>
            {
                RecordOwnership::Owned(record, marker)
            }
            SnapshotRegistry::Foreign(marker) => RecordOwnership::OwnedByOther(record, marker),
            SnapshotRegistry::Owned(_) | SnapshotRegistry::Absent | SnapshotRegistry::Occupied => {
                RecordOwnership::Unmanaged(record)
            }
        })
    }

    /// Every (name, type) carrying this install's signed generated-hostname
    /// marker at its canonical registry name, sorted for deterministic plans.
    /// Cleanup walks these markers instead of every record in the zone.
    fn generated_marker_locations(
        &self,
        instance: &str,
        signing_key: &[u8; 32],
    ) -> Vec<(String, DnsRecordType)> {
        let txt = DnsRecordType::TXT.to_string();
        let mut locations = Vec::new();
        for ((registry_name, type_key), records) in &self.index {
            if *type_key != txt || !registry_name.starts_with(OWNERSHIP_REGISTRY_PREFIX) {
                continue;
            }
            let [record] = records.as_slice() else {
                continue;
            };
            let DnsRecordContent::TXT { content } = &record.content else {
                continue;
            };
            let Some(marker) = OwnershipMarker::parse(content) else {
                continue;
            };
            if !is_generated(&marker) {
                continue;
            }
            let Some(record_type) = ROUTING_TYPES
                .into_iter()
                .find(|candidate| candidate.to_string() == marker.record_type)
            else {
                continue;
            };
            if registry_record_name(&marker.name, record_type) != *registry_name
                || !marker.covers(signing_key, instance, &self.zone, &marker.name, record_type)
            {
                continue;
            }
            locations.push((marker.name, record_type));
        }
        locations.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.to_string().cmp(&b.1.to_string()))
        });
        locations
    }
}

/// Writes planned for one desired generated hostname, executed under that
/// name's locks.
#[derive(Debug, Clone)]
struct PlannedHost {
    name: String,
    environment_id: i32,
    /// Owned generated records (or orphan markers) removed before `set`: a
    /// previous record of another routing type — a CNAME cannot be created
    /// next to the old A — or an orphan marker for different content.
    remove_first: Vec<DnsRecordType>,
    set: Option<DnsRecordRequest>,
}

#[derive(Debug, Clone)]
struct PlannedConflict {
    name: String,
    record_type: DnsRecordType,
    reason: String,
}

/// The result of planning a generated-hostname sync against one zone listing.
/// Apply it with [`apply_zone_plan`] to execute exactly what was planned
/// without listing the zone again.
#[derive(Debug, Clone)]
pub struct ZoneReconcilePlan {
    /// Every planned action in execution order, including `conflict` rows,
    /// which make [`apply_zone_plan`] refuse the whole plan.
    pub changes: Vec<RecordChange>,
    zone: String,
    hosts: Vec<PlannedHost>,
    stale: Vec<(String, DnsRecordType)>,
    conflict: Option<PlannedConflict>,
}

impl ZoneReconcilePlan {
    fn push_conflict(&mut self, change: RecordChange, conflict: PlannedConflict) {
        self.changes.push(change);
        if self.conflict.is_none() {
            self.conflict = Some(conflict);
        }
    }
}

/// Inputs for [`plan_zone_records`].
pub struct PlanOptions<'a> {
    pub proxied: bool,
    pub instance_id: &'a str,
    pub signing_key: &'a [u8; 32],
}

/// Options for [`reconcile_zone_records`].
pub struct ReconcileOptions<'a> {
    pub proxied: bool,
    pub instance_id: &'a str,
    pub signing_key: &'a [u8; 32],
    pub dry_run: bool,
    /// Production reconciliation holds the same database lock as API writes.
    pub db: Option<&'a DatabaseConnection>,
}

/// Reconcile the provider's DNS zone so every desired generated hostname has a
/// record pointing at `edge_target`.
///
/// - **Creates** a record for a desired host that doesn't exist.
/// - **Updates** only a desired host carrying this installation's signed marker.
/// - **Replaces** this installation's generated record of another routing
///   type at a desired host (edge target switched between IP and hostname).
/// - **Deletes** only signed, owned records — and orphaned signed markers —
///   that are no longer desired.
///
/// When `dry_run` is true, nothing is written; the returned [`RecordChange`]
/// list is the plan. Callers that persist the plan before applying it should
/// use [`plan_zone_records`] + [`apply_zone_plan`] so the zone is listed once.
pub async fn reconcile_zone_records(
    provider: &dyn DnsProvider,
    base_domain: &str,
    desired_hosts: &[GeneratedHost],
    edge_target: &str,
    options: ReconcileOptions<'_>,
) -> Result<Vec<RecordChange>, DnsError> {
    let ReconcileOptions {
        proxied,
        instance_id,
        signing_key,
        dry_run,
        db,
    } = options;
    let plan = plan_zone_records(
        provider,
        base_domain,
        desired_hosts,
        edge_target,
        PlanOptions {
            proxied,
            instance_id,
            signing_key,
        },
    )
    .await?;
    if dry_run {
        return Ok(plan.changes);
    }
    apply_zone_plan(provider, plan, instance_id, signing_key, db).await
}

/// Plan a generated-hostname sync from a single `list_records` call. Writes
/// nothing.
pub async fn plan_zone_records(
    provider: &dyn DnsProvider,
    base_domain: &str,
    desired_hosts: &[GeneratedHost],
    edge_target: &str,
    options: PlanOptions<'_>,
) -> Result<ZoneReconcilePlan, DnsError> {
    let PlanOptions {
        proxied,
        instance_id,
        signing_key,
    } = options;
    let suffix = format!(".{}", base_domain.to_ascii_lowercase());
    let (record_type, content, type_str) = desired_content(edge_target);
    let provider_name = provider.provider_type().to_string();
    let capabilities = provider.capabilities();
    ManagedDnsRecordService::validate_provider_capabilities(
        &capabilities,
        &provider_name,
        record_type,
    )?;
    let desired_fingerprint = record_fingerprint(&content, proxied)?;
    let snapshot = ZoneSnapshot::load(provider, base_domain).await?;

    let mut plan = ZoneReconcilePlan {
        changes: Vec::new(),
        zone: base_domain.to_string(),
        hosts: Vec::new(),
        stale: Vec::new(),
        conflict: None,
    };
    let mut desired_fqdns: HashSet<String> = HashSet::new();

    for host in desired_hosts {
        if !desired_fqdns.insert(host.fqdn.to_ascii_lowercase()) {
            continue;
        }
        let name = relative_name(&host.fqdn, &suffix)?;
        let request = DnsRecordRequest {
            name: name.clone(),
            content: content.clone(),
            ttl: None,
            proxied,
        };
        ManagedDnsRecordService::validate_record_request(base_domain, &request)?;
        if proxied {
            check_proxy_allowed(&capabilities, &provider_name, base_domain, &name)?;
        }
        let conflict_change = || RecordChange {
            action: "conflict".to_string(),
            name: host.fqdn.clone(),
            record_type: type_str.clone(),
            value: edge_target.to_string(),
        };

        let mut remove_first = Vec::new();
        let mut removal_changes = Vec::new();
        let action = match snapshot.ownership(&name, record_type, instance_id, signing_key)? {
            RecordOwnership::NotFound => Some("create"),
            RecordOwnership::Orphaned(marker) if is_generated(&marker) => {
                // guarded_set refuses an orphan marker signed for different
                // content, so retire it before publishing the new record.
                if !marker.matches_fingerprint(&desired_fingerprint) {
                    remove_first.push(record_type);
                    removal_changes.push(marker_removal_change(&name, record_type, base_domain));
                }
                Some("create")
            }
            RecordOwnership::Owned(record, marker)
                if is_generated(&marker)
                    && record_fingerprint(&record.content, record.proxied)?
                        != desired_fingerprint =>
            {
                Some("update")
            }
            RecordOwnership::Owned(_, marker) if is_generated(&marker) => None,
            RecordOwnership::Orphaned(_)
            | RecordOwnership::Owned(_, _)
            | RecordOwnership::Unmanaged(_)
            | RecordOwnership::OwnedByOther(_, _)
            | RecordOwnership::BlockedByOther(_)
            | RecordOwnership::RegistryConflict => {
                plan.push_conflict(
                    conflict_change(),
                    PlannedConflict {
                        name,
                        record_type,
                        reason: "generated hostname is already managed by another owner or has no valid generated-hostname marker".to_string(),
                    },
                );
                continue;
            }
        };

        // Records of the other routing types at the same name: our previous
        // generated record is replaced; anyone else's blocks an incompatible
        // type (CNAME next to A/AAAA) instead of failing at the provider.
        let mut blocking = None;
        for other in ROUTING_TYPES {
            if other == record_type {
                continue;
            }
            match snapshot.ownership(&name, other, instance_id, signing_key)? {
                RecordOwnership::Owned(record, marker) if is_generated(&marker) => {
                    remove_first.push(other);
                    removal_changes.push(RecordChange {
                        action: "delete".to_string(),
                        name: record.fqdn,
                        record_type: other.to_string(),
                        value: String::new(),
                    });
                }
                RecordOwnership::Orphaned(marker) if is_generated(&marker) => {
                    remove_first.push(other);
                    removal_changes.push(marker_removal_change(&name, other, base_domain));
                }
                RecordOwnership::Owned(_, _)
                | RecordOwnership::Unmanaged(_)
                | RecordOwnership::OwnedByOther(_, _) => {
                    if types_conflict(record_type, other) && blocking.is_none() {
                        blocking = Some(other);
                    }
                }
                RecordOwnership::NotFound
                | RecordOwnership::Orphaned(_)
                | RecordOwnership::BlockedByOther(_)
                | RecordOwnership::RegistryConflict => {}
            }
        }
        if let Some(other) = blocking {
            plan.push_conflict(
                conflict_change(),
                PlannedConflict {
                    name,
                    record_type,
                    reason: format!(
                        "an existing {other} record at this name is not managed by the generated-hostname sync and cannot coexist with a {record_type} record"
                    ),
                },
            );
            continue;
        }

        plan.changes.extend(removal_changes);
        if let Some(action) = action {
            plan.changes.push(RecordChange {
                action: action.to_string(),
                name: host.fqdn.clone(),
                record_type: type_str.clone(),
                value: edge_target.to_string(),
            });
        }
        if action.is_some() || !remove_first.is_empty() {
            plan.hosts.push(PlannedHost {
                name,
                environment_id: host.owner_id,
                remove_first,
                set: action.map(|_| request),
            });
        }
    }

    // Retire generated records and orphan markers for hosts that are no
    // longer desired. Walks our signed markers, not every zone record.
    for (name, stale_type) in snapshot.generated_marker_locations(instance_id, signing_key) {
        if desired_fqdns.contains(&fqdn_of(&name, base_domain)) {
            continue;
        }
        match snapshot.ownership(&name, stale_type, instance_id, signing_key)? {
            RecordOwnership::Owned(record, marker) if is_generated(&marker) => {
                plan.changes.push(RecordChange {
                    action: "delete".to_string(),
                    name: record.fqdn,
                    record_type: stale_type.to_string(),
                    value: String::new(),
                });
                plan.stale.push((name, stale_type));
            }
            RecordOwnership::Orphaned(marker) if is_generated(&marker) => {
                plan.changes
                    .push(marker_removal_change(&name, stale_type, base_domain));
                plan.stale.push((name, stale_type));
            }
            // Content drifted from the signed fingerprint (now unmanaged) or
            // the registry is ambiguous: hands off.
            RecordOwnership::Owned(_, _)
            | RecordOwnership::Orphaned(_)
            | RecordOwnership::NotFound
            | RecordOwnership::Unmanaged(_)
            | RecordOwnership::OwnedByOther(_, _)
            | RecordOwnership::BlockedByOther(_)
            | RecordOwnership::RegistryConflict => {}
        }
    }

    Ok(plan)
}

/// Take the per-name database lock when reconciling against production.
async fn lock_in_db(
    db: Option<&DatabaseConnection>,
    zone: &str,
    name: &str,
) -> Result<Option<DatabaseTransaction>, DnsError> {
    match db {
        Some(db) => Ok(Some(
            ManagedDnsRecordService::lock_record_in_db(db, zone, name).await?,
        )),
        None => Ok(None),
    }
}

/// Execute a plan from [`plan_zone_records`]. Refuses the whole plan if it
/// contains a conflict; every write re-verifies ownership under lock.
pub async fn apply_zone_plan(
    provider: &dyn DnsProvider,
    plan: ZoneReconcilePlan,
    instance_id: &str,
    signing_key: &[u8; 32],
    db: Option<&DatabaseConnection>,
) -> Result<Vec<RecordChange>, DnsError> {
    let ZoneReconcilePlan {
        changes,
        zone,
        hosts,
        stale,
        conflict,
    } = plan;
    if let Some(conflict) = conflict {
        return Err(DnsError::RecordConflict {
            domain: zone,
            name: conflict.name,
            record_type: conflict.record_type.to_string(),
            reason: conflict.reason,
        });
    }

    for host in hosts {
        let _db_lock = lock_in_db(db, &zone, &host.name).await?;
        let _record_lock = ManagedDnsRecordService::lock_record(&zone, &host.name).await;
        for old_type in host.remove_first {
            ManagedDnsRecordService::guarded_remove(
                provider,
                &zone,
                &host.name,
                old_type,
                instance_id,
                signing_key,
                OwnershipScope::for_controller(GENERATED_HOSTNAME_CONTROLLER),
            )
            .await?;
        }
        if let Some(request) = host.set {
            ManagedDnsRecordService::guarded_set(
                provider,
                &zone,
                request,
                instance_id,
                signing_key,
                OwnershipScope {
                    project_id: None,
                    environment_id: Some(host.environment_id),
                    controller: Some(GENERATED_HOSTNAME_CONTROLLER),
                },
            )
            .await?;
        }
    }
    for (name, stale_type) in stale {
        let _db_lock = lock_in_db(db, &zone, &name).await?;
        let _record_lock = ManagedDnsRecordService::lock_record(&zone, &name).await;
        ManagedDnsRecordService::guarded_remove(
            provider,
            &zone,
            &name,
            stale_type,
            instance_id,
            signing_key,
            OwnershipScope::for_controller(GENERATED_HOSTNAME_CONTROLLER),
        )
        .await?;
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
    use std::sync::atomic::{AtomicUsize, Ordering};
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
        signed_records(name, base, ip, controller, INSTANCE)
    }

    /// `[target A record, its signed registry marker]` for `instance`.
    fn signed_records(
        name: &str,
        base: &str,
        ip: &str,
        controller: Option<&str>,
        instance: &str,
    ) -> Vec<DnsRecord> {
        let target = record(name, base, ip);
        let fingerprint = record_fingerprint(&target.content, target.proxied).unwrap();
        let marker = OwnershipMarker::new_signed(
            &SIGNING_KEY,
            instance,
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
        /// Number of zone listings (`get_records` defaults to a listing too).
        list_calls: AtomicUsize,
    }

    impl MockProvider {
        fn new(records: Vec<DnsRecord>) -> Self {
            Self {
                records: Mutex::new(records),
                list_calls: AtomicUsize::new(0),
            }
        }
        fn list_calls(&self) -> usize {
            self.list_calls.load(Ordering::SeqCst)
        }
        fn type_of(&self, fqdn: &str) -> Option<DnsRecordType> {
            self.records
                .lock()
                .unwrap()
                .iter()
                .find(|r| r.fqdn == fqdn)
                .map(|r| r.content.record_type())
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
            self.list_calls.fetch_add(1, Ordering::SeqCst);
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
            desired_content("203.0.113.10").0,
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
            relative_name("app-staging.cp.example.com", ".example.com").unwrap(),
            "app-staging.cp"
        );
        assert_eq!(relative_name("example.com", ".example.com").unwrap(), "@");
        assert!(relative_name("outside.example.net", ".example.com").is_err());
    }

    // Regression for a reported generated-hostname reconciliation bug: a
    // domain-wide sync must NEVER delete pre-existing single-label records like
    // app.example.com.
    #[tokio::test]
    async fn reconcile_refuses_to_update_untagged_records() {
        let base = "example.com";
        let provider = MockProvider::new(vec![
            record("app", base, "10.0.0.1"),
            record("www", base, "10.0.0.2"),
            record("sentry", base, "10.0.0.3"),
            record("app-staging.cp", base, "9.9.9.9"),
        ]);
        let desired = vec![host("app-staging.cp.example.com")];

        let error = reconcile_zone_records(
            &provider,
            base,
            &desired,
            "203.0.113.10",
            ReconcileOptions {
                proxied: false,
                instance_id: INSTANCE,
                signing_key: &SIGNING_KEY,
                dry_run: false,
                db: None,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, DnsError::RecordConflict { .. }));

        // app / www / sentry survive untouched.
        let fqdns = provider.fqdns();
        for keep in ["app.example.com", "www.example.com", "sentry.example.com"] {
            assert!(fqdns.contains(&keep.to_string()), "{keep} was removed!");
        }
        assert_eq!(
            provider.value_of("app.example.com").as_deref(),
            Some("10.0.0.1")
        );
        assert_eq!(
            provider.value_of("app-staging.cp.example.com").as_deref(),
            Some("9.9.9.9")
        );
    }

    #[tokio::test]
    async fn reconcile_creates_missing_and_skips_correct() {
        let base = "example.com";
        let mut records = vec![record("app", base, "10.0.0.1")];
        records.extend(owned_records("app-staging.cp", base, "203.0.113.10"));
        let provider = MockProvider::new(records);
        let desired = vec![
            host("app-staging.cp.example.com"), // unchanged
            host("app-preview.cp.example.com"), // new → create
        ];

        let changes = reconcile_zone_records(
            &provider,
            base,
            &desired,
            "203.0.113.10",
            ReconcileOptions {
                proxied: false,
                instance_id: INSTANCE,
                signing_key: &SIGNING_KEY,
                dry_run: false,
                db: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].action, "create");
        assert_eq!(changes[0].name, "app-preview.cp.example.com");
        assert!(provider
            .fqdns()
            .contains(&"app-preview.cp.example.com".to_string()));
    }

    #[tokio::test]
    async fn reconcile_dry_run_writes_nothing() {
        let base = "example.com";
        let provider = MockProvider::new(vec![record("app", base, "10.0.0.1")]);
        let before = provider.fqdns();
        let desired = vec![host("app-staging.cp.example.com")];

        let changes = reconcile_zone_records(
            &provider,
            base,
            &desired,
            "203.0.113.10",
            ReconcileOptions {
                proxied: false,
                instance_id: INSTANCE,
                signing_key: &SIGNING_KEY,
                dry_run: true,
                db: None,
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
        let base = "example.com";
        let mut records = vec![record("app", base, "10.0.0.1")];
        records.extend(owned_records("old-preview.cp", base, "9.9.9.9"));
        records.extend(owned_records_for_controller(
            "manual-owned",
            base,
            "9.9.9.8",
            None,
        ));
        let provider = MockProvider::new(records);
        let desired = vec![host("app-staging.cp.example.com")];

        let changes = reconcile_zone_records(
            &provider,
            base,
            &desired,
            "203.0.113.10",
            ReconcileOptions {
                proxied: false,
                instance_id: INSTANCE,
                signing_key: &SIGNING_KEY,
                dry_run: false,
                db: None,
            },
        )
        .await
        .unwrap();

        assert!(changes
            .iter()
            .any(|c| c.action == "delete" && c.name == "old-preview.cp.example.com"));
        let fqdns = provider.fqdns();
        assert!(fqdns.contains(&"app.example.com".to_string()));
        assert!(fqdns.contains(&"manual-owned.example.com".to_string()));
        assert!(!fqdns.contains(&"old-preview.cp.example.com".to_string()));
    }

    #[tokio::test]
    async fn reconcile_refuses_to_claim_a_manual_owned_desired_record() {
        let base = "example.com";
        let mut records = owned_records_for_controller("app-staging.cp", base, "9.9.9.9", None);
        records.extend(owned_records("unrelated-preview.cp", base, "203.0.113.10"));
        let provider = MockProvider::new(records);
        let before = provider.fqdns();

        let error = reconcile_zone_records(
            &provider,
            base,
            &[host("app-staging.cp.example.com")],
            "203.0.113.10",
            ReconcileOptions {
                proxied: false,
                instance_id: INSTANCE,
                signing_key: &SIGNING_KEY,
                dry_run: false,
                db: None,
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(error, DnsError::RecordConflict { .. }));
        assert_eq!(provider.fqdns(), before);
        assert_eq!(
            provider.value_of("app-staging.cp.example.com").as_deref(),
            Some("9.9.9.9")
        );
    }

    fn options(dry_run: bool) -> ReconcileOptions<'static> {
        ReconcileOptions {
            proxied: false,
            instance_id: INSTANCE,
            signing_key: &SIGNING_KEY,
            dry_run,
            db: None,
        }
    }

    fn actions(changes: &[RecordChange]) -> Vec<(String, String, String)> {
        changes
            .iter()
            .map(|c| (c.action.clone(), c.record_type.clone(), c.name.clone()))
            .collect()
    }

    fn change(action: &str, record_type: &str, name: &str) -> (String, String, String) {
        (
            action.to_string(),
            record_type.to_string(),
            name.to_string(),
        )
    }

    #[tokio::test]
    async fn reconcile_lists_zone_once_regardless_of_unrelated_records() {
        let base = "example.com";
        let mut records: Vec<DnsRecord> = (0..200)
            .map(|i| record(&format!("user-{i}"), base, "10.0.0.1"))
            .collect();
        records.extend(owned_records("app-staging.cp", base, "203.0.113.10"));
        let provider = MockProvider::new(records);
        let desired = vec![host("app-staging.cp.example.com")];

        // Planning reads every ownership decision from one listing.
        let plan = plan_zone_records(
            &provider,
            base,
            &desired,
            "203.0.113.10",
            PlanOptions {
                proxied: false,
                instance_id: INSTANCE,
                signing_key: &SIGNING_KEY,
            },
        )
        .await
        .unwrap();
        assert!(plan.changes.is_empty());
        assert_eq!(provider.list_calls(), 1);

        // Applying the shared plan does not list the zone again when there is
        // nothing to change.
        let applied = apply_zone_plan(&provider, plan, INSTANCE, &SIGNING_KEY, None)
            .await
            .unwrap();
        assert!(applied.is_empty());
        assert_eq!(provider.list_calls(), 1);

        // The one-shot entry point costs a single listing as well.
        let changes =
            reconcile_zone_records(&provider, base, &desired, "203.0.113.10", options(false))
                .await
                .unwrap();
        assert!(changes.is_empty());
        assert_eq!(provider.list_calls(), 2);
    }

    #[tokio::test]
    async fn reconcile_replaces_owned_record_when_edge_switches_between_ip_and_hostname() {
        let base = "example.com";
        let fqdn = "app-staging.cp.example.com";
        let mut records = vec![record("app", base, "10.0.0.1")];
        records.extend(owned_records("app-staging.cp", base, "203.0.113.10"));
        let provider = MockProvider::new(records);
        let desired = vec![host(fqdn)];

        // IP -> hostname: the owned A is removed before the CNAME is created.
        let changes = reconcile_zone_records(
            &provider,
            base,
            &desired,
            "edge.example.net",
            options(false),
        )
        .await
        .unwrap();
        assert_eq!(
            actions(&changes),
            vec![change("delete", "A", fqdn), change("create", "CNAME", fqdn)]
        );
        assert_eq!(provider.type_of(fqdn), Some(DnsRecordType::CNAME));
        assert_eq!(provider.value_of(fqdn).as_deref(), Some("edge.example.net"));
        let fqdns = provider.fqdns();
        assert!(fqdns.contains(&"_temps-owned-cname.app-staging.cp.example.com".to_string()));
        assert!(!fqdns.contains(&"_temps-owned-a.app-staging.cp.example.com".to_string()));
        assert_eq!(
            provider.value_of("app.example.com").as_deref(),
            Some("10.0.0.1")
        );

        // Converged: a second run is a no-op.
        let again = reconcile_zone_records(
            &provider,
            base,
            &desired,
            "edge.example.net",
            options(false),
        )
        .await
        .unwrap();
        assert!(again.is_empty());

        // hostname -> IP: the owned CNAME is removed before the A is created.
        let back =
            reconcile_zone_records(&provider, base, &desired, "203.0.113.10", options(false))
                .await
                .unwrap();
        assert_eq!(
            actions(&back),
            vec![change("delete", "CNAME", fqdn), change("create", "A", fqdn)]
        );
        assert_eq!(provider.type_of(fqdn), Some(DnsRecordType::A));
        assert_eq!(provider.value_of(fqdn).as_deref(), Some("203.0.113.10"));
    }

    #[tokio::test]
    async fn reconcile_refuses_cname_next_to_an_unmanaged_address_record() {
        let base = "example.com";
        let provider = MockProvider::new(vec![record("app-staging.cp", base, "9.9.9.9")]);
        let before = provider.fqdns();
        let desired = vec![host("app-staging.cp.example.com")];

        let plan =
            reconcile_zone_records(&provider, base, &desired, "edge.example.net", options(true))
                .await
                .unwrap();
        assert_eq!(
            actions(&plan),
            vec![change("conflict", "CNAME", "app-staging.cp.example.com")]
        );

        let error = reconcile_zone_records(
            &provider,
            base,
            &desired,
            "edge.example.net",
            options(false),
        )
        .await
        .unwrap_err();
        match error {
            DnsError::RecordConflict {
                name, record_type, ..
            } => {
                assert_eq!(name, "app-staging.cp");
                assert_eq!(record_type, "CNAME");
            }
            other => panic!("expected RecordConflict, got {other:?}"),
        }
        assert_eq!(provider.fqdns(), before);
        assert_eq!(
            provider.value_of("app-staging.cp.example.com").as_deref(),
            Some("9.9.9.9")
        );
    }

    #[tokio::test]
    async fn reconcile_removes_only_our_generated_orphan_markers() {
        let base = "example.com";
        let generated_orphan = owned_records("gone.cp", base, "203.0.113.10").remove(1);
        let manual_orphan =
            owned_records_for_controller("manual-gone", base, "203.0.113.11", None).remove(1);
        let foreign_orphan = signed_records(
            "other-gone.cp",
            base,
            "203.0.113.12",
            Some("generated-hostname"),
            "other-install",
        )
        .remove(1);
        let provider = MockProvider::new(vec![generated_orphan, manual_orphan, foreign_orphan]);

        let plan = reconcile_zone_records(&provider, base, &[], "203.0.113.10", options(true))
            .await
            .unwrap();
        assert_eq!(
            actions(&plan),
            vec![change(
                "delete",
                "TXT",
                "_temps-owned-a.gone.cp.example.com"
            )]
        );

        reconcile_zone_records(&provider, base, &[], "203.0.113.10", options(false))
            .await
            .unwrap();
        let fqdns = provider.fqdns();
        assert!(!fqdns.contains(&"_temps-owned-a.gone.cp.example.com".to_string()));
        assert!(fqdns.contains(&"_temps-owned-a.manual-gone.example.com".to_string()));
        assert!(fqdns.contains(&"_temps-owned-a.other-gone.cp.example.com".to_string()));
    }

    #[tokio::test]
    async fn reconcile_replaces_stale_orphan_marker_for_a_desired_host() {
        let base = "example.com";
        let fqdn = "app-staging.cp.example.com";
        // Our marker survives for an older edge IP, but its record is gone.
        let orphan = owned_records("app-staging.cp", base, "198.51.100.7").remove(1);
        let provider = MockProvider::new(vec![orphan]);

        let changes = reconcile_zone_records(
            &provider,
            base,
            &[host(fqdn)],
            "203.0.113.10",
            options(false),
        )
        .await
        .unwrap();
        assert_eq!(
            actions(&changes),
            vec![
                change("delete", "TXT", "_temps-owned-a.app-staging.cp.example.com"),
                change("create", "A", fqdn),
            ]
        );
        assert_eq!(provider.value_of(fqdn).as_deref(), Some("203.0.113.10"));
        assert!(provider
            .fqdns()
            .contains(&"_temps-owned-a.app-staging.cp.example.com".to_string()));
    }
}
