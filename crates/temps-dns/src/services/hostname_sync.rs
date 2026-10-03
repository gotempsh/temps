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
//! - **One zone operation at a time.** Everything that rewrites a zone's
//!   generated-hostname state — its DNS records, its record states and its
//!   hostname mode — holds the zone's [`ZoneOperationLock`] from planning to
//!   its last write, so two runs can never interleave on a zone.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;

use sha2::{Digest, Sha256};

use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DatabaseBackend, DatabaseConnection,
    DatabaseTransaction, DbErr, EntityTrait, QueryFilter, Set, Statement, TransactionTrait,
};
use temps_core::PublicHostnameStrategy;
use temps_entities::{
    dns_managed_domains, dns_managed_record_states, environments, preset::PresetConfig, projects,
};
use tracing::{debug, error, warn};

use crate::errors::{DnsError, HostnameModeIncomplete, HostnameModeSaved};
use crate::ownership::{
    check_proxy_allowed, parse_registry_record_name, record_fingerprint, registry_record_name,
    OwnershipMarker,
};
use crate::providers::{DnsProvider, DnsRecord, DnsRecordContent, DnsRecordRequest, DnsRecordType};
use crate::services::provider_service::GENERATED_RECORD_STATE_INSERT_BATCH;
use crate::services::{
    DnsProviderService, ManagedDnsRecordService, OwnershipScope, RecordOwnership,
};

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
    /// Generated hostnames the sync may not write without a decision. An
    /// apply that leaves any unresolved changes nothing, so this is only
    /// filled by a preview.
    pub conflicts: Vec<HostnameConflict>,
    /// Whether the provider token can manage this zone (None if not checked).
    pub zone_access_ok: Option<bool>,
}

/// A desired generated hostname whose record the sync may not write: the
/// record at its name is not one the generated-hostname sync manages.
///
/// The user resolves each one in the apply: adopt the record (when
/// `adoptable`), or skip the hostname and leave it untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostnameConflict {
    /// Fully-qualified generated hostname.
    pub name: String,
    /// Record type the sync publishes the hostname as.
    pub record_type: String,
    /// Value the sync would write.
    pub value: String,
    /// Whether the sync would write the record proxied.
    pub proxied: bool,
    /// Why the sync may not write the record, and what resolves it.
    pub reason: String,
    /// Whether adopting the record at this name and type resolves the
    /// conflict: the sync then stamps it as its own and points it at
    /// `value`. Only a record nothing else in Temps manages qualifies.
    pub adoptable: bool,
    /// Value of the record at this name and type, when there is exactly one.
    pub current_value: Option<String>,
    /// Whether that record is proxied.
    pub current_proxied: Option<bool>,
    /// Digest of everything above and of the zone state behind it: every
    /// routing record at the name and the marker that says who owns each
    /// type. A decision names the revision it was made on, and the apply
    /// refuses it when the conflict no longer has that revision, because the
    /// record, its owner, a record next to it, or what the sync would write
    /// changed after the preview.
    pub revision: String,
}

impl HostnameConflict {
    fn new(
        name: &str,
        record_type: &str,
        value: &str,
        proxied: bool,
        analysis: ConflictAnalysis,
        routing_state: &[RoutingRecords],
    ) -> Result<Self, DnsError> {
        let current_value = analysis
            .current
            .as_ref()
            .map(|record| record.content.canonical().to_value_string());
        let current_proxied = analysis.current.as_ref().map(|record| record.proxied);
        let encoded = serde_json::to_vec(&(
            name.to_ascii_lowercase(),
            record_type,
            value,
            proxied,
            &analysis.reason,
            analysis.adoptable,
            &current_value,
            current_proxied,
            routing_state,
        ))
        .map_err(DnsError::Serialization)?;
        Ok(Self {
            name: name.to_string(),
            record_type: record_type.to_string(),
            value: value.to_string(),
            proxied,
            reason: analysis.reason,
            adoptable: analysis.adoptable,
            current_value,
            current_proxied,
            revision: hex::encode(Sha256::digest(encoded)),
        })
    }

    /// What the conflict is now, for a decision made on another revision.
    fn describe(&self) -> String {
        match (&self.current_value, self.current_proxied) {
            (Some(value), proxied) => format!(
                "{} (current value: {})",
                self.reason,
                describe_value(value, proxied.unwrap_or(false))
            ),
            (None, _) => self.reason.clone(),
        }
    }
}

/// A conflicting record the user confirmed the sync may adopt. The apply
/// refuses it unless the conflict still has the `revision` the user saw.
#[derive(Debug, Clone)]
pub struct AdoptRecordDecision {
    /// Fully-qualified generated hostname of the conflict.
    pub name: String,
    pub record_type: String,
    /// [`HostnameConflict::revision`] of the conflict the user reviewed.
    pub revision: String,
}

/// A conflicting generated hostname the user chose to leave untouched. The
/// apply refuses it unless the conflict still has the `revision` the user
/// saw, so a skip never covers a record state nobody reviewed.
#[derive(Debug, Clone)]
pub struct SkipRecordDecision {
    /// Fully-qualified generated hostname of the conflict.
    pub name: String,
    pub record_type: String,
    /// [`HostnameConflict::revision`] of the conflict the user reviewed.
    pub revision: String,
}

/// The user's decisions for the conflicts a hostname-mode preview reported,
/// at most one per record. Each must resolve a conflict of the apply's plan.
#[derive(Debug, Clone, Default)]
pub struct ConflictDecisions {
    pub adopt: Vec<AdoptRecordDecision>,
    pub skip: Vec<SkipRecordDecision>,
}

impl ConflictDecisions {
    pub fn is_empty(&self) -> bool {
        self.adopt.is_empty() && self.skip.is_empty()
    }
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
///
/// The content is canonical ([`DnsRecordContent::canonical`]) — exactly what
/// the guarded write sends — so the plan shows the value that will be
/// written, and a live record that only differs in spelling (`Edge.Example.NET.`
/// for `edge.example.net`) reads as converged instead of being rewritten.
pub(crate) fn desired_content(edge_target: &str) -> (DnsRecordType, DnsRecordContent, String) {
    let content = match edge_target.trim().parse::<IpAddr>() {
        Ok(IpAddr::V4(address)) => DnsRecordContent::A {
            address: address.to_string(),
        },
        Ok(IpAddr::V6(address)) => DnsRecordContent::AAAA {
            address: address.to_string(),
        },
        Err(_) => DnsRecordContent::CNAME {
            target: edge_target.to_string(),
        },
    }
    .canonical();
    let record_type = content.record_type();
    (record_type, content, record_type.to_string())
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
        match OwnershipMarker::parse_at(content, &self.zone, name) {
            None => SnapshotRegistry::Occupied,
            Some(marker) if !marker.is_owned_by(instance) => SnapshotRegistry::Foreign(marker),
            Some(marker) if marker.covers(signing_key, instance, &self.zone, name, record_type) => {
                SnapshotRegistry::Owned(marker)
            }
            Some(_) => SnapshotRegistry::Occupied,
        }
    }

    /// Every routing record at `name` and the marker that says who owns
    /// each type, in a stable order: the zone state an adopt or skip
    /// decision for the name is made on.
    fn routing_state(
        &self,
        name: &str,
        instance: &str,
        signing_key: &[u8; 32],
    ) -> Vec<RoutingRecords> {
        ROUTING_TYPES
            .into_iter()
            .map(|record_type| {
                let mut records: Vec<(String, bool)> = self
                    .records_at(name, record_type)
                    .iter()
                    .map(|record| (record.content.canonical().to_value_string(), record.proxied))
                    .collect();
                records.sort();
                let owner = match self.registry_state(name, record_type, instance, signing_key) {
                    SnapshotRegistry::Absent => RegistryOwner::Absent,
                    SnapshotRegistry::Owned(marker) | SnapshotRegistry::Foreign(marker) => {
                        RegistryOwner::Marker {
                            instance: marker.instance,
                            controller: marker.controller,
                            project_id: marker.project_id,
                            environment_id: marker.environment_id,
                        }
                    }
                    SnapshotRegistry::Occupied => RegistryOwner::Occupied,
                };
                RoutingRecords {
                    record_type: record_type.to_string(),
                    records,
                    owner,
                }
            })
            .collect()
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
            if *type_key != txt {
                continue;
            }
            // A marker covers the record its registry name belongs to.
            let Some((name, record_type)) = parse_registry_record_name(registry_name) else {
                continue;
            };
            let [record] = records.as_slice() else {
                continue;
            };
            let DnsRecordContent::TXT { content } = &record.content else {
                continue;
            };
            let Some(marker) = OwnershipMarker::parse_at(content, &self.zone, &name) else {
                continue;
            };
            if !is_generated(&marker)
                || !marker.covers(signing_key, instance, &self.zone, &name, record_type)
            {
                continue;
            }
            locations.push((name, record_type));
        }
        locations.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.to_string().cmp(&b.1.to_string()))
        });
        locations
    }
}

/// Writes planned for one desired generated hostname, executed under that
/// name's locks. Each write carries the change row it reports.
#[derive(Debug, Clone)]
struct PlannedHost {
    name: String,
    environment_id: i32,
    /// Routing type the host is published as.
    record_type: DnsRecordType,
    /// The record at the host's name the user confirmed adopting, stamped
    /// as this sync's own before anything else is written there.
    adopt: Option<PlannedAdoption>,
    /// Removed before `set`: our previous record of a routing type that
    /// cannot stay next to the new one (a CNAME cannot be created next to
    /// the old A), and orphan markers. A record removed here is written back
    /// if `set` fails, so the name keeps resolving.
    remove_first: Vec<PlannedRemoval>,
    set: Option<(DnsRecordRequest, RecordChange)>,
    /// Removed after `set`: our previous record of a routing type that can
    /// stay next to the new one (an A next to the new AAAA), so the name
    /// resolves throughout.
    remove_after: Vec<(DnsRecordType, RecordChange)>,
}

/// Adoption of the record at a desired host's name, confirmed by the user.
#[derive(Debug, Clone)]
struct PlannedAdoption {
    /// [`record_fingerprint`] of the record the user reviewed; the record
    /// is only adopted while it still has this content.
    fingerprint: String,
    /// Whether that record is proxied.
    proxied: bool,
    change: RecordChange,
}

/// A record or orphan marker removed before a host's new record is written.
#[derive(Debug, Clone)]
struct PlannedRemoval {
    record_type: DnsRecordType,
    change: RecordChange,
    /// The routing record removed, written back if its replacement is not.
    /// `None` for an orphan marker, which routes nothing.
    restore: Option<RemovedRecord>,
}

impl PlannedRemoval {
    /// Removal of our orphan generated marker for (name, type).
    fn marker(name: &str, record_type: DnsRecordType, zone: &str) -> Self {
        Self {
            record_type,
            change: marker_removal_change(name, record_type, zone),
            restore: None,
        }
    }
}

/// A generated record as it was before a replacement removed it, with the
/// scope its marker was signed for.
#[derive(Debug, Clone)]
struct RemovedRecord {
    fqdn: String,
    content: DnsRecordContent,
    proxied: bool,
    project_id: Option<i32>,
    environment_id: Option<i32>,
}

/// Scope of the record the sync writes for a desired generated host.
fn generated_host_scope(environment_id: i32) -> OwnershipScope {
    OwnershipScope {
        project_id: None,
        environment_id: Some(environment_id),
        controller: Some(GENERATED_HOSTNAME_CONTROLLER),
    }
}

/// The result of planning a generated-hostname sync against one zone listing.
/// Apply it with [`apply_zone_plan`] to execute exactly what was planned
/// without listing the zone again.
#[derive(Debug, Clone)]
pub struct ZoneReconcilePlan {
    /// Every planned action in execution order, including `adopt` and
    /// `skip` rows for the conflicts the user resolved, and `conflict` rows
    /// for the ones nobody did.
    pub changes: Vec<RecordChange>,
    /// Conflicts no decision resolved. [`apply_zone_plan`] refuses a plan
    /// with any, before it changes anything.
    pub conflicts: Vec<HostnameConflict>,
    zone: String,
    hosts: Vec<PlannedHost>,
    stale: Vec<(String, DnsRecordType, RecordChange)>,
    /// Lowercase hostnames the user chose to skip: the plan leaves their
    /// records untouched.
    skipped: HashSet<String>,
}

impl ZoneReconcilePlan {
    /// Whether the user chose to leave `fqdn` untouched. A skipped host's
    /// record is not the sync's, so it gets no record state either.
    pub(crate) fn skips(&self, fqdn: &str) -> bool {
        self.skipped.contains(&fqdn.to_ascii_lowercase())
    }
}

/// Inputs for [`plan_zone_records`].
pub struct PlanOptions<'a> {
    pub proxied: bool,
    pub instance_id: &'a str,
    pub signing_key: &'a [u8; 32],
    /// The user's adopt and skip decisions; empty for a preview.
    pub decisions: &'a ConflictDecisions,
}

/// Why the sync may not write a desired host's record, from the snapshot.
struct ConflictAnalysis {
    reason: String,
    /// The record at the host's name and type, when there is exactly one.
    current: Option<DnsRecord>,
    adoptable: bool,
}

/// The records of one routing type at a name, and who owns them: part of
/// the zone state a conflict's revision covers.
#[derive(serde::Serialize)]
struct RoutingRecords {
    record_type: String,
    /// Canonical value and proxied flag of each record, sorted.
    records: Vec<(String, bool)>,
    owner: RegistryOwner,
}

/// What the registry name of a routing type says about who owns its records.
#[derive(serde::Serialize)]
enum RegistryOwner {
    /// No marker: nothing in Temps manages the records.
    Absent,
    /// A marker of this or another installation, by whom it was written for.
    Marker {
        instance: String,
        controller: Option<String>,
        project_id: Option<i32>,
        environment_id: Option<i32>,
    },
    /// Content that is not a marker covering the records.
    Occupied,
}

/// Who a reason names as the owner of a record carrying `marker`, which this
/// install signed for another controller.
fn marker_owner(marker: &OwnershipMarker) -> String {
    match marker.controller.as_deref() {
        None => "the DNS records API".to_string(),
        Some(crate::services::domain_delivery::DELIVERY_CONTROLLER) => {
            "custom domain delivery".to_string()
        }
        Some(controller) => format!("the '{controller}' workflow"),
    }
}

/// Classify a desired host's conflict: the reason, and whether adopting the
/// record at (name, type) would let the sync write it.
///
/// Only two records are adoptable: one with no ownership marker at all (for
/// example created by hand, or by a release whose DNS sync wrote no
/// markers), and this sync's own record for the same environment that was
/// changed at the provider after it was written. Anything another Temps
/// workflow or installation owns, and any ambiguous state, is not: adopting
/// it would take a record over from its owner.
fn analyze_conflict(
    snapshot: &ZoneSnapshot,
    name: &str,
    record_type: DnsRecordType,
    ownership: RecordOwnership,
    host_scope: &OwnershipScope,
    instance_id: &str,
    signing_key: &[u8; 32],
) -> ConflictAnalysis {
    let registry_name = registry_record_name(name, record_type);
    let occupied_registry = || {
        format!(
            "a TXT record that is not a Temps ownership marker occupies the registry name '{registry_name}' this record's marker needs; remove that TXT record at the provider"
        )
    };
    let blocked = |reason: String, current: Option<DnsRecord>| ConflictAnalysis {
        reason,
        current,
        adoptable: false,
    };
    match ownership {
        RecordOwnership::Unmanaged(record) => {
            let count = snapshot.records_at(name, record_type).len();
            if count > 1 {
                return blocked(
                    format!(
                        "{count} {record_type} records exist at this name, and Temps manages exactly one record per name and type; remove the extra records at the provider"
                    ),
                    None,
                );
            }
            match snapshot.registry_state(name, record_type, instance_id, signing_key) {
                SnapshotRegistry::Absent => ConflictAnalysis {
                    reason: format!(
                        "an existing {record_type} record at this name has no Temps ownership marker (for example one created by hand, or by an earlier Temps DNS sync that did not mark its records), and Temps never overwrites a record it does not manage"
                    ),
                    current: Some(record),
                    adoptable: true,
                },
                SnapshotRegistry::Owned(marker) if is_generated(&marker) && host_scope.permits(&marker) => {
                    ConflictAnalysis {
                        reason: "the record was changed at the provider after Temps wrote it, so its ownership marker no longer covers its value".to_string(),
                        current: Some(record),
                        adoptable: true,
                    }
                }
                SnapshotRegistry::Owned(marker) if is_generated(&marker) => blocked(
                    format!(
                        "Temps wrote this record for {} and it was changed at the provider since; remove it at the provider",
                        marker
                            .environment_id
                            .map(|environment_id| format!("environment {environment_id}"))
                            .unwrap_or_else(|| "no environment".to_string())
                    ),
                    Some(record),
                ),
                SnapshotRegistry::Owned(marker) => blocked(
                    format!(
                        "the record belongs to {}, which this sync never takes over, and it was changed at the provider after Temps wrote it",
                        marker_owner(&marker)
                    ),
                    Some(record),
                ),
                SnapshotRegistry::Foreign(marker) => blocked(
                    format!(
                        "the record is owned by another Temps installation ('{}')",
                        marker.instance
                    ),
                    Some(record),
                ),
                SnapshotRegistry::Occupied => blocked(occupied_registry(), Some(record)),
            }
        }
        RecordOwnership::Owned(record, marker) => blocked(
            format!(
                "the record is managed by {}, which this sync never takes over",
                marker_owner(&marker)
            ),
            Some(record),
        ),
        RecordOwnership::Orphaned(marker) => blocked(
            format!(
                "an ownership marker of {} is at this name without its record; remove it at the provider",
                marker_owner(&marker)
            ),
            None,
        ),
        RecordOwnership::OwnedByOther(record, marker) => blocked(
            format!(
                "the record is owned by another Temps installation ('{}')",
                marker.instance
            ),
            Some(record),
        ),
        RecordOwnership::BlockedByOther(marker) => blocked(
            format!(
                "another Temps installation ('{}') holds the ownership marker for this name",
                marker.instance
            ),
            None,
        ),
        RecordOwnership::RegistryConflict => blocked(occupied_registry(), None),
        // The caller only classifies states it cannot write.
        RecordOwnership::NotFound => blocked(
            "the record disappeared while the sync was planning".to_string(),
            None,
        ),
    }
}

/// `value`, plus ` (proxied)` when it is, as reasons show a record.
fn describe_value(value: &str, proxied: bool) -> String {
    if proxied {
        format!("{value} (proxied)")
    } else {
        value.to_string()
    }
}

/// A decision sent with an apply, waiting for the conflict it resolves.
#[derive(Clone, Copy)]
enum Decision<'a> {
    Adopt(&'a AdoptRecordDecision),
    Skip(&'a SkipRecordDecision),
}

impl Decision<'_> {
    fn kind(&self) -> &'static str {
        match self {
            Decision::Adopt(_) => "adopt",
            Decision::Skip(_) => "skip",
        }
    }

    /// Revision of the conflict the decision was made on.
    fn revision(&self) -> &str {
        match self {
            Decision::Adopt(adopt) => &adopt.revision,
            Decision::Skip(skip) => &skip.revision,
        }
    }
}

/// The decisions of one apply by (lowercase hostname, record type). Planning
/// takes each one with the conflict it resolves; one left over resolves
/// nothing, and refuses the apply.
struct DecisionIndex<'a> {
    pending: HashMap<(String, String), Decision<'a>>,
}

impl<'a> DecisionIndex<'a> {
    fn new(decisions: &'a ConflictDecisions, zone: &str) -> Result<Self, DnsError> {
        let mut pending = HashMap::new();
        let entries = decisions
            .adopt
            .iter()
            .map(|adopt| (&adopt.name, &adopt.record_type, Decision::Adopt(adopt)))
            .chain(
                decisions
                    .skip
                    .iter()
                    .map(|skip| (&skip.name, &skip.record_type, Decision::Skip(skip))),
            );
        for (name, record_type, decision) in entries {
            let record_type = record_type.trim().to_ascii_uppercase();
            if !ROUTING_TYPES
                .iter()
                .any(|routing| routing.to_string() == record_type)
            {
                return Err(DnsError::Validation(format!(
                    "The {} decision for '{name}' in zone '{zone}' names record type '{record_type}'; generated hostnames are only published as A, AAAA or CNAME records",
                    decision.kind()
                )));
            }
            let key = (
                name.trim().trim_end_matches('.').to_ascii_lowercase(),
                record_type,
            );
            if pending.insert(key.clone(), decision).is_some() {
                return Err(DnsError::Validation(format!(
                    "{} record '{}' in zone '{zone}' has more than one adopt or skip decision; send exactly one per conflicting record",
                    key.1, key.0
                )));
            }
        }
        Ok(Self { pending })
    }

    fn take(&mut self, fqdn: &str, record_type: &str) -> Option<Decision<'a>> {
        self.pending
            .remove(&(fqdn.to_ascii_lowercase(), record_type.to_string()))
    }

    /// The decision no conflict took, if any; the first by name, so the
    /// error is deterministic.
    fn into_unused(self) -> Option<(String, String, Decision<'a>)> {
        self.pending
            .into_iter()
            .min_by(|(a, _), (b, _)| a.cmp(b))
            .map(|((name, record_type), decision)| (name, record_type, decision))
    }
}

/// The database a production reconciliation runs against.
#[derive(Clone, Copy)]
pub struct ReconcileDatabase<'a> {
    pub db: &'a DatabaseConnection,
    /// DNS provider hosting the zone; part of the zone operation lock key.
    pub provider_id: i32,
}

/// Options for [`reconcile_zone_records`].
pub struct ReconcileOptions<'a> {
    pub proxied: bool,
    pub instance_id: &'a str,
    pub signing_key: &'a [u8; 32],
    pub dry_run: bool,
    /// Production reconciliation holds the zone operation lock for the whole
    /// run, plus the same per-record database locks as API writes. `None`
    /// only for database-less callers over an in-memory provider.
    pub database: Option<ReconcileDatabase<'a>>,
}

/// Advisory-lock namespace of whole-zone generated-hostname operations.
///
/// Distinct from the per-record `managed-dns:` and `domain-delivery:`
/// namespaces, so holding a zone operation never blocks — and is never
/// blocked by — a single-record write.
const ZONE_OPERATION_LOCK_NAMESPACE: &str = "managed-dns-zone-op";

/// Advisory-lock key of a zone operation: the DNS provider plus the zone in
/// the canonical form its record states are stored under (trimmed, no
/// wildcard prefix, no root dot, lowercase), so every spelling of a managed
/// zone contends on one lock.
fn zone_operation_lock_key(provider_id: i32, zone: &str) -> String {
    format!(
        "{ZONE_OPERATION_LOCK_NAMESPACE}:{provider_id}:{}",
        DnsProviderService::normalize_domain(zone)
    )
}

/// Take the operation lock of `zone` on `provider_id` for the rest of
/// `transaction`.
///
/// A try-lock: a zone that another operation is changing fails fast with
/// [`DnsError::ZoneOperationInProgress`] (409) instead of queueing behind a
/// run whose provider calls can take minutes, the same way the per-record
/// locks fail fast. Because it never waits, taking it adds no edge to the
/// row-lock order (custom domain → provider → managed zone) and cannot
/// deadlock.
pub(crate) async fn lock_zone_operation(
    transaction: &DatabaseTransaction,
    provider_id: i32,
    zone: &str,
) -> Result<(), DnsError> {
    let canonical_zone = DnsProviderService::normalize_domain(zone);
    let row = transaction
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT pg_try_advisory_xact_lock(hashtext($1)) AS acquired",
            [zone_operation_lock_key(provider_id, &canonical_zone).into()],
        ))
        .await?
        .ok_or_else(|| {
            DnsError::Database(DbErr::Custom(format!(
                "PostgreSQL zone operation lock query for zone '{canonical_zone}' on DNS provider {provider_id} returned no row"
            )))
        })?;
    let acquired: bool = row.try_get("", "acquired")?;
    if !acquired {
        warn!(
            "Refused a generated-hostname operation on zone {} (DNS provider {}): another one holds the zone operation lock",
            canonical_zone, provider_id
        );
        return Err(DnsError::ZoneOperationInProgress {
            provider_id,
            zone: canonical_zone,
        });
    }
    Ok(())
}

/// A held zone operation lock (see [`lock_zone_operation`]) and the open
/// transaction it lives on.
///
/// Every operation that rewrites a zone's generated-hostname state — its DNS
/// records, its `dns_managed_record_states` rows (the proxy's
/// origin-certificate allowlist) and its `generated_hostname_mode` (which
/// the route table derives generated hostnames from) — holds one from before
/// it plans until its last write. Two of them can therefore never interleave
/// on a zone, and the record states and the mode always come from the same
/// run.
///
/// The transaction only carries the lock: it takes no row locks and holds no
/// writes, so the guarded per-record writes (each on its own transaction and
/// record lock) and domain delivery's provider/zone row locks never wait on
/// it. The holder saves state in transactions of its own, so what it saved
/// stays saved if it fails later.
pub(crate) struct ZoneOperationLock<'a> {
    db: &'a DatabaseConnection,
    transaction: DatabaseTransaction,
    provider_id: i32,
    zone: String,
}

impl<'a> ZoneOperationLock<'a> {
    /// Begin a transaction on `db` and take the zone operation lock on it.
    pub(crate) async fn acquire(
        db: &'a DatabaseConnection,
        provider_id: i32,
        zone: &str,
    ) -> Result<Self, DnsError> {
        let transaction = db.begin().await?;
        lock_zone_operation(&transaction, provider_id, zone).await?;
        let zone = DnsProviderService::normalize_domain(zone);
        debug!(
            "Acquired the generated-hostname operation lock for zone {} on DNS provider {}",
            zone, provider_id
        );
        Ok(Self {
            db,
            transaction,
            provider_id,
            zone,
        })
    }

    /// Refuse to run work for a zone this lock does not cover.
    fn ensure_covers(&self, zone: &str) -> Result<(), DnsError> {
        if DnsProviderService::normalize_domain(zone) == self.zone {
            return Ok(());
        }
        Err(DnsError::Validation(format!(
            "A generated-hostname plan for zone '{zone}' cannot run under the operation lock of zone '{}' on DNS provider {}; it must hold its own zone's lock",
            self.zone, self.provider_id
        )))
    }

    /// Take the per-record database lock every guarded write of `name` takes,
    /// on its own transaction: dropping that transaction releases the record
    /// while this zone lock stays held.
    async fn lock_record(&self, zone: &str, name: &str) -> Result<DatabaseTransaction, DnsError> {
        ManagedDnsRecordService::lock_record_in_db(self.db, zone, name).await
    }

    /// End the operation and return its `outcome`. The lock is released
    /// before this returns, so whoever sees the operation end can start the
    /// next one at once. The lock's transaction holds no writes, so failing
    /// to end it is logged and never changes the outcome: the connection
    /// closing releases the lock all the same.
    pub(crate) async fn finish<T>(self, outcome: Result<T, DnsError>) -> Result<T, DnsError> {
        let Self {
            transaction,
            provider_id,
            zone,
            ..
        } = self;
        let ended = if outcome.is_ok() {
            transaction.commit().await
        } else {
            transaction.rollback().await
        };
        if let Err(error) = ended {
            error!(
                "Failed to release the generated-hostname operation lock of zone {} (DNS provider {}): {}",
                zone, provider_id, error
            );
        }
        outcome
    }
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
///
/// A writing run against a database holds the zone's [`ZoneOperationLock`]
/// from planning to its last write, so it never interleaves with a
/// hostname-mode apply or another reconciliation of the zone; it fails fast
/// with [`DnsError::ZoneOperationInProgress`] while one runs. A dry run
/// writes nothing and takes no lock.
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
        database,
    } = options;
    let zone_lock = match database {
        Some(database) if !dry_run => {
            Some(ZoneOperationLock::acquire(database.db, database.provider_id, base_domain).await?)
        }
        Some(_) | None => None,
    };
    let outcome = async {
        let no_decisions = ConflictDecisions::default();
        let plan = plan_zone_records(
            provider,
            base_domain,
            desired_hosts,
            edge_target,
            PlanOptions {
                proxied,
                instance_id,
                signing_key,
                decisions: &no_decisions,
            },
        )
        .await?;
        if dry_run {
            return Ok(plan.changes);
        }
        apply_zone_plan(provider, plan, instance_id, signing_key, zone_lock.as_ref()).await
    }
    .await;
    match zone_lock {
        Some(zone_lock) => zone_lock.finish(outcome).await,
        None => outcome,
    }
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
        decisions,
    } = options;
    let mut decisions = DecisionIndex::new(decisions, base_domain)?;
    let suffix = format!(".{}", base_domain.to_ascii_lowercase());
    let (record_type, content, type_str) = desired_content(edge_target);
    // Plan rows show the canonical value that is actually written.
    let value = content.to_value_string();
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
        conflicts: Vec::new(),
        zone: base_domain.to_string(),
        hosts: Vec::new(),
        stale: Vec::new(),
        skipped: HashSet::new(),
    };
    let rejected = |name: &str, record_type: &str, decision: Decision<'_>, reason: String| {
        DnsError::HostnameDecisionRejected {
            zone: base_domain.to_string(),
            name: name.to_string(),
            record_type: record_type.to_string(),
            decision: decision.kind(),
            reason,
        }
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
            value: value.clone(),
        };
        let host_scope = generated_host_scope(host.owner_id);

        let mut remove_first = Vec::new();
        let mut remove_after = Vec::new();
        let mut conflict = None;
        let mut action = match snapshot.ownership(&name, record_type, instance_id, signing_key)? {
            RecordOwnership::NotFound => Some("create"),
            RecordOwnership::Orphaned(marker) if is_generated(&marker) => {
                // guarded_set refuses an orphan marker signed for different
                // content, so retire it before publishing the new record.
                if !marker.matches_fingerprint(&desired_fingerprint) {
                    remove_first.push(PlannedRemoval::marker(&name, record_type, base_domain));
                }
                Some("create")
            }
            // Fingerprints cover canonical content, so a live record that
            // only differs in spelling (`Edge.Example.NET.`) is converged. A
            // marker still carrying the value of an interrupted update is
            // rewritten too, so it stops covering the previous value.
            RecordOwnership::Owned(record, marker)
                if is_generated(&marker)
                    && (record_fingerprint(&record.content, record.proxied)?
                        != desired_fingerprint
                        || marker.has_pending_fingerprint()) =>
            {
                Some("update")
            }
            RecordOwnership::Owned(_, marker) if is_generated(&marker) => None,
            ownership @ (RecordOwnership::Orphaned(_)
            | RecordOwnership::Owned(_, _)
            | RecordOwnership::Unmanaged(_)
            | RecordOwnership::OwnedByOther(_, _)
            | RecordOwnership::BlockedByOther(_)
            | RecordOwnership::RegistryConflict) => {
                conflict = Some(analyze_conflict(
                    &snapshot,
                    &name,
                    record_type,
                    ownership,
                    &host_scope,
                    instance_id,
                    signing_key,
                ));
                None
            }
        };

        // Records of the other routing types at the same name: our previous
        // generated record is replaced; anyone else's blocks an incompatible
        // type (CNAME next to A/AAAA) instead of failing at the provider.
        //
        // Our record is removed after the new one is written when it can
        // stay next to it: A and AAAA can share a name, and the new record's
        // scope must permit it as a sibling. Otherwise it is removed just
        // before, and written back if the new record is not written.
        // A record the sync is about to publish here, created or adopted,
        // must be the only routing record of the name: one next to a record
        // the sync does not manage (an A next to someone's AAAA) would split
        // the hostname's traffic between two destinations, which is also why
        // `guarded_set` refuses such a create.
        let publishes_record = action == Some("create") || conflict.is_some();
        let mut blocking = None;
        for other in ROUTING_TYPES {
            if other == record_type {
                continue;
            }
            match snapshot.ownership(&name, other, instance_id, signing_key)? {
                RecordOwnership::Owned(record, marker) if is_generated(&marker) => {
                    let change = RecordChange {
                        action: "delete".to_string(),
                        name: record.fqdn.clone(),
                        record_type: other.to_string(),
                        value: String::new(),
                    };
                    if !types_conflict(record_type, other) && host_scope.permits(&marker) {
                        remove_after.push((other, change));
                    } else {
                        remove_first.push(PlannedRemoval {
                            record_type: other,
                            change,
                            restore: Some(RemovedRecord {
                                fqdn: record.fqdn,
                                content: record.content,
                                proxied: record.proxied,
                                project_id: marker.project_id,
                                environment_id: marker.environment_id,
                            }),
                        });
                    }
                }
                RecordOwnership::Orphaned(marker) if is_generated(&marker) => {
                    remove_first.push(PlannedRemoval::marker(&name, other, base_domain));
                }
                RecordOwnership::Owned(record, _)
                | RecordOwnership::Unmanaged(record)
                | RecordOwnership::OwnedByOther(record, _) => {
                    if (publishes_record || types_conflict(record_type, other))
                        && blocking.is_none()
                    {
                        blocking = Some((other, record.content.canonical().to_value_string()));
                    }
                }
                RecordOwnership::NotFound
                | RecordOwnership::Orphaned(_)
                | RecordOwnership::BlockedByOther(_)
                | RecordOwnership::RegistryConflict => {}
            }
        }
        // Adopting the record of the host's own type cannot resolve this:
        // the other record would still be in the way.
        if let Some((other, other_value)) = blocking {
            let effect = if types_conflict(record_type, other) {
                format!("cannot coexist with the {record_type} record the sync writes")
            } else {
                format!(
                    "would keep sending part of the hostname's traffic elsewhere next to the {record_type} record the sync writes"
                )
            };
            conflict = Some(ConflictAnalysis {
                reason: format!(
                    "an existing {other} record at this name ({other_value}) is not managed by the generated-hostname sync and {effect}; remove it at the provider"
                ),
                current: conflict.and_then(|analysis| analysis.current),
                adoptable: false,
            });
        }

        let mut adopt = None;
        if let Some(analysis) = conflict {
            let conflict = HostnameConflict::new(
                &host.fqdn,
                &type_str,
                &value,
                proxied,
                analysis,
                &snapshot.routing_state(&name, instance_id, signing_key),
            )?;
            let Some(decision) = decisions.take(&host.fqdn, &type_str) else {
                plan.changes.push(conflict_change());
                plan.conflicts.push(conflict);
                continue;
            };
            // A decision covers exactly what the user reviewed: the record,
            // its owner, the records next to it, and what the sync would
            // write.
            if decision.revision() != conflict.revision {
                return Err(rejected(
                    &host.fqdn,
                    &type_str,
                    decision,
                    format!(
                        "the conflict changed after the preview and now reads: {}",
                        conflict.describe()
                    ),
                ));
            }
            match decision {
                Decision::Skip(_) => {
                    plan.changes.push(RecordChange {
                        action: "skip".to_string(),
                        name: host.fqdn.clone(),
                        record_type: type_str.clone(),
                        value: value.clone(),
                    });
                    plan.skipped.insert(host.fqdn.to_ascii_lowercase());
                    continue;
                }
                Decision::Adopt(_) => {
                    let record = match snapshot.records_at(&name, record_type) {
                        [record] if conflict.adoptable => record,
                        _ => {
                            return Err(rejected(
                                &host.fqdn,
                                &type_str,
                                decision,
                                format!("the record cannot be adopted: {}", conflict.reason),
                            ))
                        }
                    };
                    let fingerprint = record_fingerprint(&record.content, record.proxied)?;
                    // An adopted record that already holds the desired value
                    // only needs its ownership marker.
                    action = (fingerprint != desired_fingerprint).then_some("update");
                    adopt = Some(PlannedAdoption {
                        fingerprint,
                        proxied: record.proxied,
                        change: RecordChange {
                            action: "adopt".to_string(),
                            name: host.fqdn.clone(),
                            record_type: type_str.clone(),
                            value: record.content.canonical().to_value_string(),
                        },
                    });
                }
            }
        }

        let set = action.map(|action| {
            (
                request,
                RecordChange {
                    action: action.to_string(),
                    name: host.fqdn.clone(),
                    record_type: type_str.clone(),
                    value: value.clone(),
                },
            )
        });
        plan.changes
            .extend(adopt.iter().map(|adoption| adoption.change.clone()));
        plan.changes
            .extend(remove_first.iter().map(|removal| removal.change.clone()));
        plan.changes
            .extend(set.iter().map(|(_, change)| change.clone()));
        plan.changes
            .extend(remove_after.iter().map(|(_, change)| change.clone()));
        if adopt.is_some() || set.is_some() || !remove_first.is_empty() || !remove_after.is_empty()
        {
            plan.hosts.push(PlannedHost {
                name,
                environment_id: host.owner_id,
                record_type,
                adopt,
                remove_first,
                set,
                remove_after,
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
                let change = RecordChange {
                    action: "delete".to_string(),
                    name: record.fqdn,
                    record_type: stale_type.to_string(),
                    value: String::new(),
                };
                plan.changes.push(change.clone());
                plan.stale.push((name, stale_type, change));
            }
            RecordOwnership::Orphaned(marker) if is_generated(&marker) => {
                let change = marker_removal_change(&name, stale_type, base_domain);
                plan.changes.push(change.clone());
                plan.stale.push((name, stale_type, change));
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

    // A decision no conflict took resolves nothing: the zone changed after
    // the preview it was made on, so the user has to review it again.
    if let Some((name, decision_type, decision)) = decisions.into_unused() {
        let reason = if !desired_fqdns.contains(&name) {
            "it is not one of the generated hostnames this sync publishes".to_string()
        } else if decision_type != type_str {
            format!(
                "this sync publishes the hostname's record as type {type_str}, not {decision_type}"
            )
        } else {
            "the record no longer conflicts, so the sync no longer needs a decision for it"
                .to_string()
        };
        return Err(rejected(&name, &decision_type, decision, reason));
    }

    Ok(plan)
}

/// Take the per-name database lock when reconciling against production.
async fn lock_in_db(
    zone_lock: Option<&ZoneOperationLock<'_>>,
    zone: &str,
    name: &str,
) -> Result<Option<DatabaseTransaction>, DnsError> {
    match zone_lock {
        Some(zone_lock) => Ok(Some(zone_lock.lock_record(zone, name).await?)),
        None => Ok(None),
    }
}

/// What a plan run changed before it returned, in execution order.
#[derive(Debug, Default)]
pub(crate) struct ZonePlanProgress {
    /// Change rows of the writes that completed.
    pub(crate) completed: Vec<RecordChange>,
    /// Records created or updated: name, type, and whether the provider
    /// stored them proxied.
    written: Vec<(String, DnsRecordType, bool)>,
    /// Records, or orphan markers, removed: name and type.
    removed: Vec<(String, DnsRecordType)>,
    /// Whether the step between the writes and the removals completed.
    pub(crate) switched: bool,
}

impl ZonePlanProgress {
    /// Record that (name, type) is now a managed record the provider
    /// stored with `proxied`. A host's later write replaces what an earlier
    /// step recorded for it (an adoption, then the update to the desired
    /// value), so each record is saved once.
    fn wrote(&mut self, name: &str, record_type: DnsRecordType, proxied: bool) {
        match self
            .written
            .iter_mut()
            .find(|(written, written_type, _)| written == name && *written_type == record_type)
        {
            Some(entry) => entry.2 = proxied,
            None => self.written.push((name.to_string(), record_type, proxied)),
        }
    }

    /// Record that (name, type) is no longer recognized as managed, after
    /// an earlier step of its host recorded it as written.
    fn unmanaged(&mut self, name: &str, record_type: DnsRecordType) {
        self.written
            .retain(|(written, written_type, _)| written != name || *written_type != record_type);
    }
}

/// Execute a plan from [`plan_zone_records`]. Refuses the whole plan if it
/// contains a conflict; every write re-verifies ownership under lock.
///
/// Against a database the caller must hold the plan zone's
/// [`ZoneOperationLock`], and every record is then also written under the
/// per-record lock API writes take. `None` is only for database-less callers
/// over an in-memory provider.
pub(crate) async fn apply_zone_plan(
    provider: &dyn DnsProvider,
    plan: ZoneReconcilePlan,
    instance_id: &str,
    signing_key: &[u8; 32],
    zone_lock: Option<&ZoneOperationLock<'_>>,
) -> Result<Vec<RecordChange>, DnsError> {
    let mut progress = ZonePlanProgress::default();
    run_zone_plan(
        provider,
        plan,
        instance_id,
        signing_key,
        zone_lock,
        &mut progress,
        || async { Ok(()) },
    )
    .await?;
    Ok(progress.completed)
}

/// [`apply_zone_plan`] in two halves with `between` in the middle: every
/// create and update (each with the removals it needs at its own name),
/// then `between`, then the removal of records no longer desired. `between`
/// runs only once every create and update succeeded, and the removals only
/// once it succeeded. Each completed write is recorded in `progress`, so a
/// caller can tell what changed when this fails.
///
/// A record removed because it cannot stay next to its replacement is
/// written back when the replacement is not written, so its name keeps
/// resolving (see [`restore_replaced`]).
async fn run_zone_plan<F, Fut>(
    provider: &dyn DnsProvider,
    plan: ZoneReconcilePlan,
    instance_id: &str,
    signing_key: &[u8; 32],
    zone_lock: Option<&ZoneOperationLock<'_>>,
    progress: &mut ZonePlanProgress,
    between: F,
) -> Result<(), DnsError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), DnsError>>,
{
    let ZoneReconcilePlan {
        changes: _,
        conflicts,
        zone,
        hosts,
        stale,
        skipped: _,
    } = plan;
    if let Some(zone_lock) = zone_lock {
        zone_lock.ensure_covers(&zone)?;
    }
    if !conflicts.is_empty() {
        return Err(DnsError::GeneratedHostnameConflicts {
            zone,
            conflicts: conflicts
                .into_iter()
                .map(|conflict| {
                    format!(
                        "{} '{}': {}",
                        conflict.record_type, conflict.name, conflict.reason
                    )
                })
                .collect(),
        });
    }

    for host in hosts {
        let _db_lock = lock_in_db(zone_lock, &zone, &host.name).await?;
        let _record_lock = ManagedDnsRecordService::lock_record(&zone, &host.name).await;
        // The record the user confirmed adopting becomes this sync's own
        // first, while it still holds the value they reviewed; the writes
        // below then treat it like any record the sync wrote.
        if let Some(adoption) = host.adopt {
            ManagedDnsRecordService::guarded_import(
                provider,
                &zone,
                &host.name,
                host.record_type,
                Some(&adoption.fingerprint),
                instance_id,
                signing_key,
                generated_host_scope(host.environment_id),
            )
            .await?;
            progress.wrote(&host.name, host.record_type, adoption.proxied);
            progress.completed.push(adoption.change);
        }
        // Routing records removed for the new one, written back if it is not.
        let mut replaced = Vec::new();
        for removal in host.remove_first {
            let removed = ManagedDnsRecordService::guarded_remove(
                provider,
                &zone,
                &host.name,
                removal.record_type,
                instance_id,
                signing_key,
                OwnershipScope::for_controller(GENERATED_HOSTNAME_CONTROLLER),
            )
            .await;
            // A record is deleted before its marker, so a failed removal may
            // have deleted it too.
            replaced.extend(removal.restore.map(|record| (removal.record_type, record)));
            if let Err(error) = removed {
                restore_replaced(
                    provider,
                    &zone,
                    &host.name,
                    host.record_type,
                    replaced,
                    instance_id,
                    signing_key,
                    progress,
                )
                .await;
                return Err(error);
            }
            progress
                .removed
                .push((host.name.clone(), removal.record_type));
            progress.completed.push(removal.change);
        }
        if let Some((request, change)) = host.set {
            let written = ManagedDnsRecordService::guarded_set(
                provider,
                &zone,
                request,
                instance_id,
                signing_key,
                generated_host_scope(host.environment_id),
            )
            .await;
            match written {
                Ok(record) => {
                    progress.wrote(&host.name, host.record_type, record.proxied);
                    progress.completed.push(change);
                }
                // The record changed before its marker write failed, so it is
                // part of what this run changed. Its state is saved only when
                // the record still counts as managed.
                Err(DnsError::ManagedRecordMarkerNotFinalized(details)) => {
                    if details.stays_managed {
                        progress.wrote(&host.name, host.record_type, details.proxied);
                    } else {
                        progress.unmanaged(&host.name, host.record_type);
                    }
                    progress.completed.push(change);
                    return Err(DnsError::ManagedRecordMarkerNotFinalized(details));
                }
                Err(error) => {
                    restore_replaced(
                        provider,
                        &zone,
                        &host.name,
                        host.record_type,
                        replaced,
                        instance_id,
                        signing_key,
                        progress,
                    )
                    .await;
                    return Err(error);
                }
            }
        }
        for (old_type, change) in host.remove_after {
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
            progress.removed.push((host.name.clone(), old_type));
            progress.completed.push(change);
        }
    }
    between().await?;
    progress.switched = true;
    for (name, stale_type, change) in stale {
        let _db_lock = lock_in_db(zone_lock, &zone, &name).await?;
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
        progress.removed.push((name, stale_type));
        progress.completed.push(change);
    }
    Ok(())
}

/// Write back the generated records removed at `name` for its new
/// `record_type` record, after that record was not written, so a name the
/// current routes use keeps resolving. Nothing is written back when the new
/// record is at the provider after all (a write can fail after the provider
/// applied it), and a removed record still in place is left as it is.
///
/// Each record written back is a `restore` change in `progress`. One that
/// cannot be is logged: its name may not resolve until a sync writes it.
#[allow(clippy::too_many_arguments)]
async fn restore_replaced(
    provider: &dyn DnsProvider,
    zone: &str,
    name: &str,
    record_type: DnsRecordType,
    replaced: Vec<(DnsRecordType, RemovedRecord)>,
    instance_id: &str,
    signing_key: &[u8; 32],
    progress: &mut ZonePlanProgress,
) {
    if replaced.is_empty() {
        return;
    }
    match provider.get_records(zone, name, record_type).await {
        Ok(records) if records.is_empty() => {}
        Ok(_) => {
            warn!(
                "{} record '{}' in zone {} exists although writing it failed, so the {} record(s) removed for it are not written back",
                record_type,
                name,
                zone,
                replaced.len()
            );
            return;
        }
        Err(error) => {
            error!(
                "Could not check whether {} record '{}' in zone {} exists after writing it failed, so the {} record(s) removed for it are not written back and the name may not resolve: {}",
                record_type,
                name,
                zone,
                replaced.len(),
                error
            );
            return;
        }
    }
    for (old_type, record) in replaced {
        match provider.get_records(zone, name, old_type).await {
            Ok(records) if records.is_empty() => {}
            // Its removal failed before deleting it.
            Ok(_) => continue,
            Err(error) => {
                error!(
                    "Could not check whether {} record '{}' in zone {} still exists, so it is not written back and the name may not resolve: {}",
                    old_type, name, zone, error
                );
                continue;
            }
        }
        let value = record.content.to_value_string();
        let restored = ManagedDnsRecordService::guarded_set(
            provider,
            zone,
            DnsRecordRequest {
                name: name.to_string(),
                content: record.content,
                ttl: None,
                proxied: record.proxied,
            },
            instance_id,
            signing_key,
            OwnershipScope {
                project_id: record.project_id,
                environment_id: record.environment_id,
                controller: Some(GENERATED_HOSTNAME_CONTROLLER),
            },
        )
        .await;
        let managed_proxied = match restored {
            Ok(written) => Some(written.proxied),
            // Written back; guarded_set logged why its marker is not final.
            Err(DnsError::ManagedRecordMarkerNotFinalized(details)) => {
                details.stays_managed.then_some(details.proxied)
            }
            Err(error) => {
                error!(
                    "Failed to write back {} record '{}' in zone {} after its replacement {} record could not be written; the name may not resolve until a sync writes it: {}",
                    old_type, name, zone, record_type, error
                );
                continue;
            }
        };
        warn!(
            "Wrote back {} record '{}' in zone {} after its replacement {} record could not be written",
            old_type, name, zone, record_type
        );
        if let Some(proxied) = managed_proxied {
            progress.wrote(name, old_type, proxied);
        }
        progress.completed.push(RecordChange {
            action: "restore".to_string(),
            name: record.fqdn,
            record_type: old_type.to_string(),
            value,
        });
    }
}

/// What a hostname-mode apply saves once every record it creates or
/// updates is in place.
pub(crate) struct HostnameModeSwitch<'a> {
    /// The managed zone, as read under the zone operation lock.
    pub(crate) managed: &'a dns_managed_domains::Model,
    pub(crate) target: PublicHostnameStrategy,
    /// The generated hostnames `target` uses. Those the plan skips get no
    /// record state.
    pub(crate) desired: &'a [GeneratedHost],
    pub(crate) edge_target: &'a str,
}

/// Execute a hostname-mode apply's `plan` under `zone_lock`, in an order
/// that leaves a usable saved state wherever it stops:
///
/// 1. Create and update the records the new mode needs. The only records
///    removed here are ones of another routing type at those same names:
///    one that cannot sit next to its replacement (a CNAME next to an A)
///    just before the replacement is written, and written back if the
///    replacement is not; any other just after.
/// 2. Save the new mode together with the record states of the records it
///    uses, in one transaction ([`save_hostname_mode_switch`]).
/// 3. Remove the records the new mode no longer uses.
///
/// Every other record the current mode uses stays in place until step 3. A
/// failure in step 1 or 2 keeps the current mode and saves the record
/// states of the records already written, so origin certificates follow
/// what is at the provider. A failure in step 3 keeps the saved mode, and
/// some unused records remain. Unless nothing had changed, the error is a
/// [`DnsError::HostnameModeIncomplete`] listing the completed changes and
/// what was saved; applying the mode again finishes the work.
pub(crate) async fn apply_hostname_mode_plan(
    db: &DatabaseConnection,
    provider: &dyn DnsProvider,
    zone_lock: &ZoneOperationLock<'_>,
    plan: ZoneReconcilePlan,
    switch: HostnameModeSwitch<'_>,
    instance_id: &str,
    signing_key: &[u8; 32],
) -> Result<Vec<RecordChange>, DnsError> {
    // A skipped hostname keeps a record the sync does not manage, so it gets
    // no record state: the proxy only serves its origin certificate for
    // records Temps wrote.
    let synced: Vec<GeneratedHost> = switch
        .desired
        .iter()
        .filter(|host| !plan.skips(&host.fqdn))
        .cloned()
        .collect();
    let switch = HostnameModeSwitch {
        desired: &synced,
        ..switch
    };
    let mut progress = ZonePlanProgress::default();
    let outcome = run_zone_plan(
        provider,
        plan,
        instance_id,
        signing_key,
        Some(zone_lock),
        &mut progress,
        || save_hostname_mode_switch(db, &switch),
    )
    .await;
    let source = match outcome {
        Ok(()) => return Ok(progress.completed),
        Err(source) => source,
    };
    let managed = switch.managed;
    let saved = if progress.switched {
        HostnameModeSaved::Mode
    } else if progress.completed.is_empty() {
        // Nothing changed, so there is nothing to save or to report.
        return Err(source);
    } else {
        match save_progress_record_states(db, managed.provider_id, &managed.domain, &progress).await
        {
            Ok(()) => HostnameModeSaved::RecordStates,
            Err(error) => {
                error!(
                    "Failed to save the record states of {} DNS change(s) a failed hostname-mode apply made in zone {} (DNS provider {}): {}",
                    progress.completed.len(),
                    managed.domain,
                    managed.provider_id,
                    error
                );
                HostnameModeSaved::Nothing
            }
        }
    };
    Err(DnsError::HostnameModeIncomplete(Box::new(
        HostnameModeIncomplete {
            provider_id: managed.provider_id,
            zone: managed.domain.clone(),
            mode: switch.target.as_db_str().to_string(),
            completed: progress.completed,
            saved,
            source,
        },
    )))
}

/// Save `switch.target` as the zone's hostname mode together with the record
/// states of the records it uses, in one transaction, so the route table and
/// the origin-certificate allowlist change together.
async fn save_hostname_mode_switch(
    db: &DatabaseConnection,
    switch: &HostnameModeSwitch<'_>,
) -> Result<(), DnsError> {
    let managed = switch.managed;
    let transaction = db.begin().await?;
    let saved = async {
        DnsProviderService::replace_generated_record_states(
            &transaction,
            managed.provider_id,
            &managed.domain,
            switch.desired,
            switch.edge_target,
            managed.proxied_by_default,
        )
        .await?;
        let mut active: dns_managed_domains::ActiveModel = managed.clone().into();
        active.generated_hostname_mode = Set(switch.target.as_db_str().to_string());
        active.sync_generated_records = Set(true);
        active.update(&transaction).await?;
        Ok(())
    }
    .await;
    end_transaction(
        transaction,
        saved,
        &format!(
            "saving hostname mode '{}' for zone {} (DNS provider {})",
            switch.target.as_db_str(),
            managed.domain,
            managed.provider_id
        ),
    )
    .await
}

/// Save the record states of the writes in `progress`, made by an apply
/// that stopped before its switch: each written record gets a state with
/// the proxied flag the provider stored, and each removed record loses its
/// state. Removals are saved first, so a record removed and then written
/// back (see [`restore_replaced`]) keeps its state; no record is removed
/// after it was written.
async fn save_progress_record_states(
    db: &DatabaseConnection,
    provider_id: i32,
    zone: &str,
    progress: &ZonePlanProgress,
) -> Result<(), DnsError> {
    if progress.written.is_empty() && progress.removed.is_empty() {
        return Ok(());
    }
    let zone = DnsProviderService::normalize_domain(zone);
    let transaction = db.begin().await?;
    let saved = async {
        if !progress.removed.is_empty() {
            let removed =
                progress
                    .removed
                    .iter()
                    .fold(Condition::any(), |removed, (name, record_type)| {
                        removed.add(
                            Condition::all()
                                .add(dns_managed_record_states::Column::Name.eq(name.as_str()))
                                .add(
                                    dns_managed_record_states::Column::RecordType
                                        .eq(record_type.to_string()),
                                ),
                        )
                    });
            dns_managed_record_states::Entity::delete_many()
                .filter(dns_managed_record_states::Column::ProviderId.eq(provider_id))
                .filter(dns_managed_record_states::Column::Zone.eq(zone.as_str()))
                .filter(
                    dns_managed_record_states::Column::Controller.eq(GENERATED_HOSTNAME_CONTROLLER),
                )
                .filter(removed)
                .exec(&transaction)
                .await?;
        }
        let rows: Vec<dns_managed_record_states::ActiveModel> = progress
            .written
            .iter()
            .map(
                |(name, record_type, proxied)| dns_managed_record_states::ActiveModel {
                    provider_id: Set(provider_id),
                    zone: Set(zone.clone()),
                    name: Set(name.clone()),
                    fqdn: Set(fqdn_of(name, &zone)),
                    record_type: Set(record_type.to_string()),
                    controller: Set(GENERATED_HOSTNAME_CONTROLLER.to_string()),
                    proxied: Set(*proxied),
                    updated_at: Set(chrono::Utc::now()),
                    ..Default::default()
                },
            )
            .collect();
        for chunk in rows.chunks(GENERATED_RECORD_STATE_INSERT_BATCH) {
            dns_managed_record_states::Entity::insert_many(chunk.to_vec())
                .on_conflict(
                    OnConflict::columns([
                        dns_managed_record_states::Column::ProviderId,
                        dns_managed_record_states::Column::Zone,
                        dns_managed_record_states::Column::Name,
                        dns_managed_record_states::Column::RecordType,
                        dns_managed_record_states::Column::Controller,
                    ])
                    .update_columns([
                        dns_managed_record_states::Column::Fqdn,
                        dns_managed_record_states::Column::Proxied,
                        dns_managed_record_states::Column::UpdatedAt,
                    ])
                    .to_owned(),
                )
                .exec_without_returning(&transaction)
                .await?;
        }
        Ok(())
    }
    .await;
    end_transaction(
        transaction,
        saved,
        &format!(
            "saving the record states of a stopped hostname-mode apply in zone {zone} (DNS provider {provider_id})"
        ),
    )
    .await
}

/// Commit `transaction` when `outcome` succeeded and roll it back when it
/// failed, before returning, so its locks are released either way. A failed
/// rollback is logged and never replaces the outcome's own error.
async fn end_transaction<T>(
    transaction: DatabaseTransaction,
    outcome: Result<T, DnsError>,
    operation: &str,
) -> Result<T, DnsError> {
    match outcome {
        Ok(value) => {
            transaction.commit().await?;
            Ok(value)
        }
        Err(error) => {
            if let Err(rollback_error) = transaction.rollback().await {
                error!(
                    "Failed to roll back {} after it failed with '{}': {}",
                    operation, error, rollback_error
                );
            }
            Err(error)
        }
    }
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
    static NO_DECISIONS: ConflictDecisions = ConflictDecisions {
        adopt: Vec::new(),
        skip: Vec::new(),
    };

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
        signed_records_for_environment(name, base, ip, controller, instance, Some(1))
    }

    /// [`signed_records`] with the marker written for `environment_id`.
    fn signed_records_for_environment(
        name: &str,
        base: &str,
        ip: &str,
        controller: Option<&str>,
        instance: &str,
        environment_id: Option<i32>,
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
            environment_id,
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

    /// In-memory DnsProvider for CF-free reconciliation tests. Records are
    /// keyed by (name, type), and like a real provider it refuses a CNAME
    /// next to any other record at its name.
    struct MockProvider {
        records: Mutex<Vec<DnsRecord>>,
        /// Number of zone listings (`get_records` defaults to a listing too).
        list_calls: AtomicUsize,
        /// Records created so far; numbers their IDs.
        created: AtomicUsize,
        /// Record names whose non-TXT writes fail: of every type (`None`)
        /// or of one.
        failing_writes: Mutex<Vec<(String, Option<DnsRecordType>)>>,
        /// Record IDs whose deletes fail.
        failing_deletes: Mutex<HashSet<String>>,
    }

    impl MockProvider {
        fn new(records: Vec<DnsRecord>) -> Self {
            Self {
                records: Mutex::new(records),
                list_calls: AtomicUsize::new(0),
                created: AtomicUsize::new(0),
                failing_writes: Mutex::new(Vec::new()),
                failing_deletes: Mutex::new(HashSet::new()),
            }
        }
        fn fail_writes_of(&self, name: &str) {
            self.failing_writes
                .lock()
                .unwrap()
                .push((name.to_string(), None));
        }
        fn fail_writes_of_type(&self, name: &str, record_type: DnsRecordType) {
            self.failing_writes
                .lock()
                .unwrap()
                .push((name.to_string(), Some(record_type)));
        }
        fn fail_deletes_of(&self, record_id: &str) {
            self.failing_deletes
                .lock()
                .unwrap()
                .insert(record_id.to_string());
        }
        fn heal(&self) {
            self.failing_writes.lock().unwrap().clear();
            self.failing_deletes.lock().unwrap().clear();
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
        /// `"TYPE value"` of every record at `fqdn`, sorted.
        fn routing_at(&self, fqdn: &str) -> Vec<String> {
            let mut routing: Vec<String> = self
                .records
                .lock()
                .unwrap()
                .iter()
                .filter(|record| record.fqdn == fqdn)
                .map(|record| {
                    format!(
                        "{} {}",
                        record.content.record_type(),
                        record.content.to_value_string()
                    )
                })
                .collect();
            routing.sort();
            routing
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
            if self.failing_deletes.lock().unwrap().contains(record_id) {
                return Err(DnsError::ApiError(format!(
                    "simulated delete failure for record {record_id}"
                )));
            }
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
            let record_type = request.content.record_type();
            if record_type != DnsRecordType::TXT
                && self
                    .failing_writes
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(name, failing)| {
                        *name == request.name
                            && failing.is_none_or(|failing| failing == record_type)
                    })
            {
                return Err(DnsError::ApiError(format!(
                    "simulated write failure for {record_type} {}",
                    request.name
                )));
            }
            let fqdn = if request.name == "@" {
                domain.to_string()
            } else {
                format!("{}.{}", request.name, domain)
            };
            let mut recs = self.records.lock().unwrap();
            if let Some(r) = recs
                .iter_mut()
                .find(|r| r.fqdn == fqdn && r.content.record_type() == record_type)
            {
                r.content = request.content.clone();
                r.proxied = request.proxied;
                return Ok(r.clone());
            }
            if let Some(other) = recs
                .iter()
                .find(|r| r.fqdn == fqdn && types_conflict(r.content.record_type(), record_type))
            {
                return Err(DnsError::ApiError(format!(
                    "simulated provider refusal: a {record_type} record cannot share {fqdn} with its {} record",
                    other.content.record_type()
                )));
            }
            let created = self.created.fetch_add(1, Ordering::SeqCst);
            let new = DnsRecord {
                id: Some(format!("created-{created}-{}", request.name)),
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
            record_type: DnsRecordType,
        ) -> Result<(), DnsError> {
            let fqdn = if name == "@" {
                domain.to_string()
            } else {
                format!("{}.{}", name, domain)
            };
            self.records
                .lock()
                .unwrap()
                .retain(|r| r.fqdn != fqdn || r.content.record_type() != record_type);
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

    #[test]
    fn desired_content_is_canonical() {
        assert_eq!(
            desired_content(" Edge.Example.NET. "),
            (
                DnsRecordType::CNAME,
                DnsRecordContent::CNAME {
                    target: "edge.example.net".to_string()
                },
                "CNAME".to_string()
            )
        );
        assert_eq!(
            desired_content("2001:DB8:0:0:0:0:0:1"),
            (
                DnsRecordType::AAAA,
                DnsRecordContent::AAAA {
                    address: "2001:db8::1".to_string()
                },
                "AAAA".to_string()
            )
        );
        assert_eq!(
            desired_content(" 203.0.113.10 "),
            (
                DnsRecordType::A,
                DnsRecordContent::A {
                    address: "203.0.113.10".to_string()
                },
                "A".to_string()
            )
        );
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
                database: None,
            },
        )
        .await
        .unwrap_err();
        match &error {
            DnsError::GeneratedHostnameConflicts { zone, conflicts } => {
                assert_eq!(zone, "example.com");
                assert_eq!(conflicts.len(), 1, "{error}");
                assert!(
                    conflicts[0].starts_with("A 'app-staging.cp.example.com': "),
                    "{error}"
                );
                assert!(
                    conflicts[0].contains("no Temps ownership marker"),
                    "{error}"
                );
            }
            other => panic!("expected GeneratedHostnameConflicts, got {other:?}"),
        }

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
                database: None,
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
                database: None,
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
                database: None,
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
                database: None,
            },
        )
        .await
        .unwrap_err();

        match &error {
            DnsError::GeneratedHostnameConflicts { conflicts, .. } => {
                assert!(
                    conflicts[0].contains("managed by the DNS records API"),
                    "{error}"
                );
            }
            other => panic!("expected GeneratedHostnameConflicts, got {other:?}"),
        }
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
            database: None,
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
                decisions: &NO_DECISIONS,
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

    fn plan_options() -> PlanOptions<'static> {
        PlanOptions {
            proxied: false,
            instance_id: INSTANCE,
            signing_key: &SIGNING_KEY,
            decisions: &NO_DECISIONS,
        }
    }

    /// Plan `desired` against `edge` in `example.com` and run the plan, with
    /// what the run changed before it returned.
    async fn run_plan(
        provider: &MockProvider,
        desired: &[GeneratedHost],
        edge: &str,
    ) -> (Result<(), DnsError>, ZonePlanProgress) {
        let plan = plan_zone_records(provider, "example.com", desired, edge, plan_options())
            .await
            .expect("plan the sync");
        let mut progress = ZonePlanProgress::default();
        let outcome = run_zone_plan(
            provider,
            plan,
            INSTANCE,
            &SIGNING_KEY,
            None,
            &mut progress,
            || async { Ok(()) },
        )
        .await;
        (outcome, progress)
    }

    /// A CNAME cannot share its name, so the previous record of the other
    /// type is removed before it is written. When the new record cannot be
    /// written, the previous one is written back: the name keeps resolving
    /// as before and stays managed, and a later run replaces it.
    #[tokio::test]
    async fn failed_cname_replacement_writes_the_removed_record_back() {
        let base = "example.com";
        let name = "app-staging.cp";
        let fqdn = "app-staging.cp.example.com";
        let address = ("A", "203.0.113.10");
        let alias = ("CNAME", "edge.example.net");
        for (records, (old_type, old_edge), (new_type, new_edge)) in [
            (owned_records(name, base, address.1), address, alias),
            (
                generated_cname_records(name, base, alias.1, alias.1),
                alias,
                address,
            ),
        ] {
            let provider = MockProvider::new(records);
            let failing_type = if new_type == "A" {
                DnsRecordType::A
            } else {
                DnsRecordType::CNAME
            };
            provider.fail_writes_of_type(name, failing_type);

            let (outcome, progress) = run_plan(&provider, &[host(fqdn)], new_edge).await;
            assert!(
                matches!(outcome, Err(DnsError::ApiError(_))),
                "{old_type} -> {new_type}: {outcome:?}"
            );
            assert_eq!(
                change_names(&progress.completed),
                [
                    format!("delete {old_type} {fqdn}"),
                    format!("restore {old_type} {fqdn}")
                ]
            );
            assert_eq!(
                provider.routing_at(fqdn),
                [format!("{old_type} {old_edge}")]
            );
            // Written back under a valid generated-hostname marker: planning
            // its own edge again changes nothing.
            let unchanged =
                plan_zone_records(&provider, base, &[host(fqdn)], old_edge, plan_options())
                    .await
                    .unwrap();
            assert!(unchanged.changes.is_empty(), "{:?}", unchanged.changes);

            provider.heal();
            let changes =
                reconcile_zone_records(&provider, base, &[host(fqdn)], new_edge, options(false))
                    .await
                    .unwrap();
            assert_eq!(
                actions(&changes),
                [
                    change("delete", old_type, fqdn),
                    change("create", new_type, fqdn)
                ]
            );
            assert_eq!(
                provider.routing_at(fqdn),
                [format!("{new_type} {new_edge}")]
            );
        }
    }

    /// A removal deletes the record before its marker. When the marker
    /// cannot be deleted the record is already gone, so it is written back
    /// as well, under the marker that remained.
    #[tokio::test]
    async fn failed_removal_writes_the_deleted_record_back() {
        let base = "example.com";
        let fqdn = "app-staging.cp.example.com";
        let provider = MockProvider::new(owned_records("app-staging.cp", base, "203.0.113.10"));
        provider.fail_deletes_of("id-_temps-owned-a.app-staging.cp");

        let (outcome, progress) = run_plan(&provider, &[host(fqdn)], "edge.example.net").await;
        assert!(matches!(outcome, Err(DnsError::ApiError(_))), "{outcome:?}");
        assert_eq!(
            change_names(&progress.completed),
            [format!("restore A {fqdn}")]
        );
        assert_eq!(provider.routing_at(fqdn), ["A 203.0.113.10"]);
        let unchanged = plan_zone_records(
            &provider,
            base,
            &[host(fqdn)],
            "203.0.113.10",
            plan_options(),
        )
        .await
        .unwrap();
        assert!(unchanged.changes.is_empty(), "{:?}", unchanged.changes);

        provider.heal();
        let changes = reconcile_zone_records(
            &provider,
            base,
            &[host(fqdn)],
            "edge.example.net",
            options(false),
        )
        .await
        .unwrap();
        assert_eq!(
            actions(&changes),
            [change("delete", "A", fqdn), change("create", "CNAME", fqdn)]
        );
        assert_eq!(provider.routing_at(fqdn), ["CNAME edge.example.net"]);
    }

    /// A and AAAA can share a name, so the previous address record is
    /// removed only once the new one is written: a failed write leaves the
    /// name resolving as before, with nothing removed.
    #[tokio::test]
    async fn address_family_switch_removes_the_previous_record_last() {
        let base = "example.com";
        let fqdn = "app-staging.cp.example.com";
        let provider = MockProvider::new(owned_records("app-staging.cp", base, "203.0.113.10"));
        provider.fail_writes_of_type("app-staging.cp", DnsRecordType::AAAA);

        let (outcome, progress) = run_plan(&provider, &[host(fqdn)], "2001:db8::10").await;
        assert!(matches!(outcome, Err(DnsError::ApiError(_))), "{outcome:?}");
        assert!(progress.completed.is_empty(), "{:?}", progress.completed);
        assert_eq!(provider.routing_at(fqdn), ["A 203.0.113.10"]);

        provider.heal();
        let changes = reconcile_zone_records(
            &provider,
            base,
            &[host(fqdn)],
            "2001:db8::10",
            options(false),
        )
        .await
        .unwrap();
        assert_eq!(
            actions(&changes),
            [change("create", "AAAA", fqdn), change("delete", "A", fqdn)]
        );
        assert_eq!(provider.routing_at(fqdn), ["AAAA 2001:db8::10"]);
        assert!(!provider
            .fqdns()
            .contains(&"_temps-owned-a.app-staging.cp.example.com".to_string()));
    }

    /// The previous record is only left in place while its replacement is
    /// written when the replacement may be created next to it: a record of
    /// another environment is removed first, as before, and written back if
    /// the replacement fails.
    #[tokio::test]
    async fn address_record_of_another_environment_is_removed_first() {
        let base = "example.com";
        let fqdn = "app-staging.cp.example.com";
        let provider = MockProvider::new(owned_records("app-staging.cp", base, "203.0.113.10"));
        let other_environment = [GeneratedHost {
            owner_id: 2,
            ..host(fqdn)
        }];

        let plan = plan_zone_records(
            &provider,
            base,
            &other_environment,
            "2001:db8::10",
            plan_options(),
        )
        .await
        .unwrap();
        assert_eq!(
            actions(&plan.changes),
            [change("delete", "A", fqdn), change("create", "AAAA", fqdn)]
        );

        provider.fail_writes_of_type("app-staging.cp", DnsRecordType::AAAA);
        let (outcome, progress) = run_plan(&provider, &other_environment, "2001:db8::10").await;
        assert!(matches!(outcome, Err(DnsError::ApiError(_))), "{outcome:?}");
        assert_eq!(
            change_names(&progress.completed),
            [format!("delete A {fqdn}"), format!("restore A {fqdn}")]
        );
        assert_eq!(provider.routing_at(fqdn), ["A 203.0.113.10"]);
    }

    /// Adopt `conflict`, naming the revision the preview showed, as the
    /// console does.
    fn adopt(conflict: &HostnameConflict) -> AdoptRecordDecision {
        AdoptRecordDecision {
            name: conflict.name.clone(),
            record_type: conflict.record_type.clone(),
            revision: conflict.revision.clone(),
        }
    }

    /// Skip `conflict`, naming the revision the preview showed.
    fn skip(conflict: &HostnameConflict) -> SkipRecordDecision {
        SkipRecordDecision {
            name: conflict.name.clone(),
            record_type: conflict.record_type.clone(),
            revision: conflict.revision.clone(),
        }
    }

    /// A skip decision written by hand, for a conflict no preview reported.
    fn skip_named(name: &str, record_type: &str) -> SkipRecordDecision {
        SkipRecordDecision {
            name: name.to_string(),
            record_type: record_type.to_string(),
            revision: "not-from-a-preview".to_string(),
        }
    }

    fn deciding(
        adopt: Vec<AdoptRecordDecision>,
        skip: Vec<SkipRecordDecision>,
    ) -> ConflictDecisions {
        ConflictDecisions { adopt, skip }
    }

    /// An `AAAA` record at the name [`record`] would use.
    fn aaaa_record(name: &str, base: &str, ip: &str) -> DnsRecord {
        DnsRecord {
            id: Some(format!("id-aaaa-{name}")),
            content: DnsRecordContent::AAAA {
                address: ip.to_string(),
            },
            ..record(name, base, "192.0.2.1")
        }
    }

    /// Change the `A` record at `fqdn` in place, as an edit at the provider.
    fn edit_record(provider: &MockProvider, fqdn: &str, edit: impl FnOnce(&mut DnsRecord)) {
        let mut records = provider.records.lock().unwrap();
        let record = records
            .iter_mut()
            .find(|record| record.fqdn == fqdn && record.content.record_type() == DnsRecordType::A)
            .expect("the A record to edit exists");
        edit(record);
    }

    /// Plan `desired` in `example.com` against [`EDGE`] with `decisions`.
    async fn plan_deciding(
        provider: &MockProvider,
        desired: &[GeneratedHost],
        decisions: &ConflictDecisions,
    ) -> Result<ZoneReconcilePlan, DnsError> {
        plan_zone_records(
            provider,
            "example.com",
            desired,
            EDGE,
            PlanOptions {
                decisions,
                ..plan_options()
            },
        )
        .await
    }

    /// The conflicts a preview of `desired` reports.
    async fn preview_conflicts(
        provider: &MockProvider,
        desired: &[GeneratedHost],
    ) -> Vec<HostnameConflict> {
        plan_deciding(provider, desired, &NO_DECISIONS)
            .await
            .expect("preview the sync")
            .conflicts
    }

    /// The decision kind and reason of a refused decision.
    fn rejection(planned: Result<ZoneReconcilePlan, DnsError>) -> (&'static str, String) {
        match planned {
            Err(DnsError::HostnameDecisionRejected {
                decision, reason, ..
            }) => (decision, reason),
            other => panic!("expected HostnameDecisionRejected, got {other:?}"),
        }
    }

    /// Run `plan`, with what the run changed before it returned.
    async fn run_planned(
        provider: &MockProvider,
        plan: ZoneReconcilePlan,
    ) -> (Result<(), DnsError>, ZonePlanProgress) {
        let mut progress = ZonePlanProgress::default();
        let outcome = run_zone_plan(
            provider,
            plan,
            INSTANCE,
            &SIGNING_KEY,
            None,
            &mut progress,
            || async { Ok(()) },
        )
        .await;
        (outcome, progress)
    }

    /// The upgrade case: an earlier DNS sync published a generated hostname
    /// without an ownership marker. The preview reports it as a conflict the
    /// user can adopt, with the record's current value next to the new one.
    #[tokio::test]
    async fn preview_reports_an_unmarked_record_as_an_adoptable_conflict() {
        let provider = MockProvider::new(vec![record("pr-1", "example.com", "198.51.100.7")]);
        let desired = [host("pr-1.example.com")];

        let plan = plan_deciding(&provider, &desired, &NO_DECISIONS)
            .await
            .expect("a preview plans around conflicts");

        assert_eq!(
            actions(&plan.changes),
            vec![change("conflict", "A", "pr-1.example.com")]
        );
        let conflict = &plan.conflicts[0];
        assert_eq!(
            plan.conflicts,
            [HostnameConflict {
                name: "pr-1.example.com".to_string(),
                record_type: "A".to_string(),
                value: EDGE.to_string(),
                proxied: false,
                reason: conflict.reason.clone(),
                adoptable: true,
                current_value: Some("198.51.100.7".to_string()),
                current_proxied: Some(false),
                revision: conflict.revision.clone(),
            }]
        );
        assert!(
            conflict.reason.contains("no Temps ownership marker"),
            "{}",
            conflict.reason
        );
        assert!(
            conflict
                .reason
                .contains("never overwrites a record it does not manage"),
            "{}",
            conflict.reason
        );
        // The revision identifies what the preview showed, so an unchanged
        // zone previews the same revision again.
        assert_eq!(conflict.revision.len(), 64, "{}", conflict.revision);
        assert_eq!(
            preview_conflicts(&provider, &desired).await[0].revision,
            conflict.revision
        );
    }

    /// Only records nothing else in Temps owns can be adopted: another
    /// workflow's record, another installation's, and ambiguous states are
    /// reported with what to do instead.
    #[tokio::test]
    async fn conflicts_owned_elsewhere_or_ambiguous_are_not_adoptable() {
        let base = "example.com";
        let mut records = owned_records_for_controller("api-owned", base, "198.51.100.1", None);
        records.extend(owned_records_for_controller(
            "delivery-owned",
            base,
            "198.51.100.2",
            Some("domain-delivery"),
        ));
        records.extend(signed_records(
            "other-install",
            base,
            "198.51.100.3",
            Some("generated-hostname"),
            "other-install",
        ));
        records.push(record("doubled", base, "198.51.100.4"));
        records.push(record("doubled", base, "198.51.100.5"));
        let provider = MockProvider::new(records);
        let desired = [
            host("api-owned.example.com"),
            host("delivery-owned.example.com"),
            host("other-install.example.com"),
            host("doubled.example.com"),
        ];

        let conflicts = preview_conflicts(&provider, &desired).await;

        let summary: Vec<(&str, bool, &str)> = conflicts
            .iter()
            .map(|c| (c.name.as_str(), c.adoptable, c.reason.as_str()))
            .collect();
        assert_eq!(summary.len(), 4, "{summary:?}");
        assert!(
            summary.iter().all(|(_, adoptable, _)| !adoptable),
            "{summary:?}"
        );
        assert!(
            summary[0].2.contains("managed by the DNS records API"),
            "{summary:?}"
        );
        assert!(
            summary[1].2.contains("managed by custom domain delivery"),
            "{summary:?}"
        );
        assert!(
            summary[2]
                .2
                .contains("owned by another Temps installation ('other-install')"),
            "{summary:?}"
        );
        assert!(
            summary[3].2.contains("2 A records exist at this name"),
            "{summary:?}"
        );
        assert_eq!(conflicts[3].current_value, None);
    }

    /// Adopting an unmarked record that already holds the desired value
    /// only stamps its marker; the next sync finds it converged.
    #[tokio::test]
    async fn adopting_a_record_with_the_desired_value_only_stamps_its_marker() {
        let fqdn = "pr-1.example.com";
        let provider = MockProvider::new(vec![record("pr-1", "example.com", EDGE)]);
        let desired = [host(fqdn)];
        let reviewed = preview_conflicts(&provider, &desired).await;

        let plan = plan_deciding(
            &provider,
            &desired,
            &deciding(vec![adopt(&reviewed[0])], Vec::new()),
        )
        .await
        .expect("plan the adoption");
        assert!(plan.conflicts.is_empty(), "{:?}", plan.conflicts);
        assert_eq!(actions(&plan.changes), vec![change("adopt", "A", fqdn)]);

        let (outcome, progress) = run_planned(&provider, plan).await;
        outcome.expect("adopt the record");
        assert_eq!(
            change_names(&progress.completed),
            [format!("adopt A {fqdn}")]
        );
        assert_eq!(
            progress.written,
            [("pr-1".to_string(), DnsRecordType::A, false)]
        );
        assert_eq!(provider.routing_at(fqdn), [format!("A {EDGE}")]);

        let next = plan_deciding(&provider, &desired, &NO_DECISIONS)
            .await
            .expect("plan again");
        assert!(next.changes.is_empty(), "{:?}", next.changes);
        assert!(next.conflicts.is_empty(), "{:?}", next.conflicts);
    }

    /// An adopted record holding another value is then pointed at the edge,
    /// and recorded once with the flag the provider stored.
    #[tokio::test]
    async fn adopting_a_record_points_it_at_the_edge() {
        let fqdn = "pr-1.example.com";
        let provider = MockProvider::new(vec![record("pr-1", "example.com", "198.51.100.7")]);
        let desired = [host(fqdn)];
        let reviewed = preview_conflicts(&provider, &desired).await;

        let plan = plan_deciding(
            &provider,
            &desired,
            &deciding(vec![adopt(&reviewed[0])], Vec::new()),
        )
        .await
        .expect("plan the adoption");
        assert_eq!(
            actions(&plan.changes),
            vec![change("adopt", "A", fqdn), change("update", "A", fqdn)]
        );
        assert_eq!(plan.changes[0].value, "198.51.100.7");
        assert_eq!(plan.changes[1].value, EDGE);

        let (outcome, progress) = run_planned(&provider, plan).await;
        outcome.expect("adopt and update the record");
        assert_eq!(
            change_names(&progress.completed),
            [format!("adopt A {fqdn}"), format!("update A {fqdn}")]
        );
        assert_eq!(
            progress.written,
            [("pr-1".to_string(), DnsRecordType::A, false)]
        );
        assert_eq!(provider.value_of(fqdn).as_deref(), Some(EDGE));

        let next = plan_deciding(&provider, &desired, &NO_DECISIONS)
            .await
            .expect("plan again");
        assert!(next.changes.is_empty(), "{:?}", next.changes);
    }

    /// This sync's own record, edited at the provider since, no longer
    /// matches its marker. The user can adopt it back.
    #[tokio::test]
    async fn a_generated_record_changed_at_the_provider_can_be_adopted_back() {
        let fqdn = "pr-1.example.com";
        let mut records = owned_records("pr-1", "example.com", EDGE);
        records[0].content = DnsRecordContent::A {
            address: "198.51.100.7".to_string(),
        };
        let provider = MockProvider::new(records);
        let desired = [host(fqdn)];

        let reviewed = preview_conflicts(&provider, &desired).await;
        assert_eq!(reviewed.len(), 1);
        assert!(reviewed[0].adoptable, "{reviewed:?}");
        assert!(
            reviewed[0]
                .reason
                .contains("changed at the provider after Temps wrote it"),
            "{reviewed:?}"
        );

        let plan = plan_deciding(
            &provider,
            &desired,
            &deciding(vec![adopt(&reviewed[0])], Vec::new()),
        )
        .await
        .expect("plan the adoption");
        let (outcome, _) = run_planned(&provider, plan).await;
        outcome.expect("adopt the record back");
        assert_eq!(provider.value_of(fqdn).as_deref(), Some(EDGE));
        let next = plan_deciding(&provider, &desired, &NO_DECISIONS)
            .await
            .expect("plan again");
        assert!(next.changes.is_empty(), "{:?}", next.changes);
    }

    /// A record that is not the one the user reviewed — another value, or
    /// another proxied flag — is never adopted.
    #[tokio::test]
    async fn adopt_refuses_a_record_that_changed_after_the_preview() {
        let fqdn = "pr-1.example.com";
        let provider = MockProvider::new(vec![record("pr-1", "example.com", "198.51.100.7")]);
        let desired = [host(fqdn)];
        let reviewed = preview_conflicts(&provider, &desired).await;
        let decisions = deciding(vec![adopt(&reviewed[0])], Vec::new());

        edit_record(&provider, fqdn, |record| {
            record.content = DnsRecordContent::A {
                address: "198.51.100.8".to_string(),
            }
        });
        let (decision, reason) = rejection(plan_deciding(&provider, &desired, &decisions).await);
        assert_eq!(decision, "adopt");
        assert!(reason.contains("changed after the preview"), "{reason}");
        assert!(reason.contains("current value: 198.51.100.8)"), "{reason}");

        edit_record(&provider, fqdn, |record| {
            record.content = DnsRecordContent::A {
                address: "198.51.100.7".to_string(),
            };
            record.proxied = true;
        });
        let (_, reason) = rejection(plan_deciding(&provider, &desired, &decisions).await);
        assert!(
            reason.contains("current value: 198.51.100.7 (proxied))"),
            "{reason}"
        );

        assert_eq!(provider.fqdns(), [fqdn.to_string()]);
        assert_eq!(provider.value_of(fqdn).as_deref(), Some("198.51.100.7"));
    }

    /// The adoption checks the record again under its locks, so one that
    /// changed after planning is left alone.
    #[tokio::test]
    async fn adoption_rechecks_the_record_when_it_runs() {
        let fqdn = "pr-1.example.com";
        let provider = MockProvider::new(vec![record("pr-1", "example.com", "198.51.100.7")]);
        let desired = [host(fqdn)];
        let reviewed = preview_conflicts(&provider, &desired).await;
        let plan = plan_deciding(
            &provider,
            &desired,
            &deciding(vec![adopt(&reviewed[0])], Vec::new()),
        )
        .await
        .expect("plan the adoption");
        edit_record(&provider, fqdn, |record| {
            record.content = DnsRecordContent::A {
                address: "198.51.100.9".to_string(),
            }
        });

        let (outcome, progress) = run_planned(&provider, plan).await;

        match outcome {
            Err(DnsError::RecordConflict { reason, .. }) => {
                assert!(reason.contains("changed after it was reviewed"), "{reason}");
                assert!(reason.contains("now 198.51.100.9"), "{reason}");
            }
            other => panic!("expected RecordConflict, got {other:?}"),
        }
        assert!(progress.completed.is_empty(), "{:?}", progress.completed);
        assert_eq!(provider.fqdns(), [fqdn.to_string()]);
        assert_eq!(provider.value_of(fqdn).as_deref(), Some("198.51.100.9"));
    }

    #[tokio::test]
    async fn adopt_refuses_a_conflict_that_cannot_be_adopted() {
        let fqdn = "pr-1.example.com";
        let provider = MockProvider::new(owned_records_for_controller(
            "pr-1",
            "example.com",
            "198.51.100.7",
            None,
        ));
        let before = provider.fqdns();
        let desired = [host(fqdn)];
        let reviewed = preview_conflicts(&provider, &desired).await;
        assert!(!reviewed[0].adoptable, "{reviewed:?}");

        let (decision, reason) = rejection(
            plan_deciding(
                &provider,
                &desired,
                &deciding(vec![adopt(&reviewed[0])], Vec::new()),
            )
            .await,
        );

        assert_eq!(decision, "adopt");
        assert!(
            reason.contains("cannot be adopted: the record is managed by the DNS records API"),
            "{reason}"
        );
        assert_eq!(provider.fqdns(), before);
    }

    /// Skipping a conflict leaves that hostname's record untouched and syncs
    /// every other hostname.
    #[tokio::test]
    async fn skipping_a_conflict_syncs_every_other_hostname() {
        let provider = MockProvider::new(vec![record("pr-1", "example.com", "198.51.100.7")]);
        let desired = [host("pr-1.example.com"), host("pr-2.example.com")];
        let reviewed = preview_conflicts(&provider, &desired).await;
        // Names match whatever their spelling.
        let decisions = deciding(
            Vec::new(),
            vec![SkipRecordDecision {
                name: "PR-1.example.com.".to_string(),
                ..skip(&reviewed[0])
            }],
        );

        let plan = plan_deciding(&provider, &desired, &decisions)
            .await
            .expect("plan around the skipped hostname");
        assert!(plan.conflicts.is_empty(), "{:?}", plan.conflicts);
        assert_eq!(
            actions(&plan.changes),
            vec![
                change("skip", "A", "pr-1.example.com"),
                change("create", "A", "pr-2.example.com")
            ]
        );
        assert!(plan.skips("pr-1.example.com"));
        assert!(!plan.skips("pr-2.example.com"));

        let (outcome, progress) = run_planned(&provider, plan).await;
        outcome.expect("sync the other hostname");
        assert_eq!(
            change_names(&progress.completed),
            ["create A pr-2.example.com"]
        );
        assert_eq!(
            provider.value_of("pr-1.example.com").as_deref(),
            Some("198.51.100.7")
        );
        assert_eq!(
            provider.fqdns(),
            [
                "_temps-owned-a.pr-2.example.com",
                "pr-1.example.com",
                "pr-2.example.com"
            ]
        );
    }

    /// A skip covers the conflict the user reviewed. If its record changes
    /// value or owner after the preview but still conflicts, the apply
    /// refuses instead of saving a mode around a state nobody reviewed.
    #[tokio::test]
    async fn skip_refuses_a_conflict_that_changed_after_the_preview() {
        let base = "example.com";
        let fqdn = "pr-1.example.com";
        let desired = [host(fqdn), host("pr-2.example.com")];
        let provider = MockProvider::new(vec![record("pr-1", base, "198.51.100.7")]);
        let reviewed = preview_conflicts(&provider, &desired).await;
        let decisions = deciding(Vec::new(), vec![skip(&reviewed[0])]);

        // Its value changes, and it still conflicts.
        edit_record(&provider, fqdn, |record| {
            record.content = DnsRecordContent::A {
                address: "198.51.100.9".to_string(),
            }
        });
        let (decision, reason) = rejection(plan_deciding(&provider, &desired, &decisions).await);
        assert_eq!(decision, "skip");
        assert!(reason.contains("changed after the preview"), "{reason}");
        assert!(reason.contains("current value: 198.51.100.9)"), "{reason}");

        // Same value, but another workflow took the record over.
        let taken_over = MockProvider::new(owned_records_for_controller(
            "pr-1",
            base,
            "198.51.100.7",
            None,
        ));
        let before = taken_over.fqdns();
        let (_, reason) = rejection(plan_deciding(&taken_over, &desired, &decisions).await);
        assert!(
            reason.contains("managed by the DNS records API"),
            "{reason}"
        );
        assert_eq!(taken_over.fqdns(), before);
    }

    /// A change the reason does not spell out refuses the skip too: the
    /// environment the record's owner wrote it for, or the proxy status of
    /// the record next to it.
    #[tokio::test]
    async fn skip_refuses_a_conflict_whose_owner_or_neighbour_changed() {
        let base = "example.com";
        let desired = &[host("pr-1.example.com")];
        let refused_after = |provider: MockProvider, change: fn(&mut Vec<DnsRecord>)| async move {
            let reviewed = preview_conflicts(&provider, desired).await;
            let decisions = deciding(Vec::new(), vec![skip(&reviewed[0])]);
            change(&mut provider.records.lock().unwrap());
            let now = preview_conflicts(&provider, desired).await;
            assert_eq!(now[0].reason, reviewed[0].reason);
            assert_ne!(now[0].revision, reviewed[0].revision);
            rejection(plan_deciding(&provider, desired, &decisions).await)
        };

        let (decision, reason) = refused_after(
            MockProvider::new(owned_records_for_controller(
                "pr-1",
                base,
                "198.51.100.7",
                None,
            )),
            |records| {
                *records = signed_records_for_environment(
                    "pr-1",
                    "example.com",
                    "198.51.100.7",
                    None,
                    INSTANCE,
                    Some(2),
                )
            },
        )
        .await;
        assert_eq!(decision, "skip");
        assert!(reason.contains("changed after the preview"), "{reason}");

        let (_, reason) = refused_after(
            MockProvider::new(vec![
                record("pr-1", base, "198.51.100.7"),
                aaaa_record("pr-1", base, "2001:db8::7"),
            ]),
            |records| {
                for record in records.iter_mut() {
                    if record.content.record_type() == DnsRecordType::AAAA {
                        record.proxied = true;
                    }
                }
            },
        )
        .await;
        assert!(reason.contains("changed after the preview"), "{reason}");
    }

    /// A conflict nobody decided on refuses the whole plan before anything
    /// is written, hostnames without conflicts included, and names it.
    #[tokio::test]
    async fn an_unresolved_conflict_refuses_the_whole_plan() {
        let provider = MockProvider::new(vec![
            record("pr-1", "example.com", "198.51.100.7"),
            record("pr-2", "example.com", "198.51.100.8"),
        ]);
        let before = provider.fqdns();
        let desired = [
            host("pr-1.example.com"),
            host("pr-2.example.com"),
            host("pr-3.example.com"),
        ];
        let reviewed = preview_conflicts(&provider, &desired).await;
        assert_eq!(reviewed.len(), 2);

        let plan = plan_deciding(
            &provider,
            &desired,
            &deciding(Vec::new(), vec![skip(&reviewed[0])]),
        )
        .await
        .expect("plan");
        assert_eq!(plan.conflicts.len(), 1);
        let (outcome, progress) = run_planned(&provider, plan).await;

        match &outcome {
            Err(error @ DnsError::GeneratedHostnameConflicts { conflicts, .. }) => {
                assert_eq!(conflicts.len(), 1, "{error}");
                assert!(
                    conflicts[0].starts_with("A 'pr-2.example.com': "),
                    "{error}"
                );
                assert!(error.to_string().contains("adopt or skip"), "{error}");
            }
            other => panic!("expected GeneratedHostnameConflicts, got {other:?}"),
        }
        assert!(progress.completed.is_empty(), "{:?}", progress.completed);
        assert_eq!(provider.fqdns(), before);
    }

    /// A decision that matches no conflict means the zone changed after the
    /// preview it was made on, so the apply refuses instead of guessing.
    #[tokio::test]
    async fn a_decision_that_resolves_no_conflict_is_rejected() {
        // pr-1 has no record, so the sync creates it without a decision.
        let provider = &MockProvider::new(Vec::new());
        let desired = &[host("pr-1.example.com")];
        let refused = |decisions: ConflictDecisions| async move {
            rejection(plan_deciding(provider, desired, &decisions).await)
        };

        let (decision, no_longer) = refused(deciding(
            Vec::new(),
            vec![skip_named("pr-1.example.com", "A")],
        ))
        .await;
        assert_eq!(decision, "skip");
        assert!(no_longer.contains("no longer conflicts"), "{no_longer}");

        let (decision, not_generated) = refused(deciding(
            vec![AdoptRecordDecision {
                name: "pr-9.example.com".to_string(),
                record_type: "A".to_string(),
                revision: "not-from-a-preview".to_string(),
            }],
            Vec::new(),
        ))
        .await;
        assert_eq!(decision, "adopt");
        assert!(
            not_generated.contains("not one of the generated hostnames"),
            "{not_generated}"
        );

        let (_, other_type) = refused(deciding(
            Vec::new(),
            vec![skip_named("pr-1.example.com", "CNAME")],
        ))
        .await;
        assert!(other_type.contains("as type A, not CNAME"), "{other_type}");
        assert!(provider.fqdns().is_empty());
    }

    #[tokio::test]
    async fn duplicated_or_malformed_decisions_are_validation_errors() {
        let provider = MockProvider::new(vec![record("pr-1", "example.com", "198.51.100.7")]);
        let desired = [host("pr-1.example.com")];
        let reviewed = preview_conflicts(&provider, &desired).await;

        let twice = deciding(vec![adopt(&reviewed[0])], vec![skip(&reviewed[0])]);
        match plan_deciding(&provider, &desired, &twice).await {
            Err(DnsError::Validation(message)) => {
                assert!(
                    message.contains("more than one adopt or skip decision"),
                    "{message}"
                )
            }
            other => panic!("expected a validation error, got {other:?}"),
        }

        let txt = deciding(Vec::new(), vec![skip_named("pr-1.example.com", "TXT")]);
        match plan_deciding(&provider, &desired, &txt).await {
            Err(DnsError::Validation(message)) => {
                assert!(
                    message.contains("only published as A, AAAA or CNAME"),
                    "{message}"
                )
            }
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    /// Adopting the A record would leave someone's AAAA record at the same
    /// name sending IPv6 clients elsewhere, so the conflict cannot be
    /// adopted. It can still be skipped, which leaves both records alone.
    #[tokio::test]
    async fn a_record_is_not_adopted_next_to_a_foreign_routing_record() {
        let base = "example.com";
        let fqdn = "pr-1.example.com";
        let desired = [host(fqdn)];
        let provider = MockProvider::new(vec![
            record("pr-1", base, "198.51.100.7"),
            aaaa_record("pr-1", base, "2001:db8::7"),
        ]);

        let reviewed = preview_conflicts(&provider, &desired).await;
        assert_eq!(reviewed.len(), 1, "{reviewed:?}");
        assert!(!reviewed[0].adoptable, "{reviewed:?}");
        assert!(
            reviewed[0]
                .reason
                .contains("existing AAAA record at this name (2001:db8::7)"),
            "{reviewed:?}"
        );
        assert!(
            reviewed[0].reason.contains("traffic elsewhere"),
            "{reviewed:?}"
        );
        assert_eq!(reviewed[0].current_value.as_deref(), Some("198.51.100.7"));

        let (decision, reason) = rejection(
            plan_deciding(
                &provider,
                &desired,
                &deciding(vec![adopt(&reviewed[0])], Vec::new()),
            )
            .await,
        );
        assert_eq!(decision, "adopt");
        assert!(reason.contains("cannot be adopted"), "{reason}");

        let plan = plan_deciding(
            &provider,
            &desired,
            &deciding(Vec::new(), vec![skip(&reviewed[0])]),
        )
        .await
        .expect("skip the hostname");
        let (outcome, progress) = run_planned(&provider, plan).await;
        outcome.expect("nothing else to do");
        assert!(progress.completed.is_empty(), "{:?}", progress.completed);
        assert_eq!(
            provider.routing_at(fqdn),
            ["A 198.51.100.7", "AAAA 2001:db8::7"]
        );
    }

    /// The same holds for a record the sync would create: it is a conflict
    /// in the preview, not a write the provider-side guard refuses halfway
    /// through the apply.
    #[tokio::test]
    async fn a_record_is_not_created_next_to_a_foreign_routing_record() {
        let fqdn = "pr-1.example.com";
        let provider = MockProvider::new(vec![aaaa_record("pr-1", "example.com", "2001:db8::7")]);

        let plan = plan_deciding(&provider, &[host(fqdn)], &NO_DECISIONS)
            .await
            .expect("preview");
        assert_eq!(actions(&plan.changes), vec![change("conflict", "A", fqdn)]);
        assert!(!plan.conflicts[0].adoptable, "{:?}", plan.conflicts);
        assert_eq!(plan.conflicts[0].current_value, None);

        let (outcome, _) = run_planned(&provider, plan).await;
        assert!(
            matches!(outcome, Err(DnsError::GeneratedHostnameConflicts { .. })),
            "{outcome:?}"
        );
        assert_eq!(provider.routing_at(fqdn), ["AAAA 2001:db8::7"]);
    }

    /// A record the sync already manages keeps following the edge target
    /// when someone adds a record of another type next to it: skipping it
    /// would drop the state its origin certificate depends on.
    #[tokio::test]
    async fn an_owned_record_stays_synced_next_to_a_foreign_routing_record() {
        let fqdn = "pr-1.example.com";
        let mut records = owned_records("pr-1", "example.com", "198.51.100.7");
        records.push(aaaa_record("pr-1", "example.com", "2001:db8::7"));
        let provider = MockProvider::new(records);

        let plan = plan_deciding(&provider, &[host(fqdn)], &NO_DECISIONS)
            .await
            .expect("plan");

        assert!(plan.conflicts.is_empty(), "{:?}", plan.conflicts);
        assert_eq!(actions(&plan.changes), vec![change("update", "A", fqdn)]);
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
        // Adopting a CNAME cannot help while the A record is in the way.
        let preview = plan_zone_records(
            &provider,
            base,
            &desired,
            "edge.example.net",
            plan_options(),
        )
        .await
        .unwrap();
        assert_eq!(preview.conflicts.len(), 1);
        assert!(!preview.conflicts[0].adoptable, "{:?}", preview.conflicts);
        assert!(
            preview.conflicts[0]
                .reason
                .contains("remove it at the provider"),
            "{:?}",
            preview.conflicts
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
        match &error {
            DnsError::GeneratedHostnameConflicts { conflicts, .. } => {
                assert_eq!(conflicts.len(), 1, "{error}");
                assert!(
                    conflicts[0].starts_with("CNAME 'app-staging.cp.example.com': "),
                    "{error}"
                );
                assert!(
                    conflicts[0].contains("existing A record at this name (9.9.9.9)"),
                    "{error}"
                );
            }
            other => panic!("expected GeneratedHostnameConflicts, got {other:?}"),
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

    /// `[generated CNAME as the provider lists it, its signed registry
    /// marker]`, the marker signed over `signed_target` (what temps wrote).
    fn generated_cname_records(
        name: &str,
        base: &str,
        listed_target: &str,
        signed_target: &str,
    ) -> Vec<DnsRecord> {
        let target = DnsRecord {
            id: Some(format!("id-{name}-cname")),
            zone: base.to_string(),
            name: name.to_string(),
            fqdn: format!("{name}.{base}"),
            content: DnsRecordContent::CNAME {
                target: listed_target.to_string(),
            },
            ttl: 1,
            proxied: false,
            metadata: HashMap::new(),
        };
        let fingerprint = record_fingerprint(
            &DnsRecordContent::CNAME {
                target: signed_target.to_string(),
            },
            false,
        )
        .unwrap();
        let marker = OwnershipMarker::new_signed(
            &SIGNING_KEY,
            INSTANCE,
            base,
            name,
            DnsRecordType::CNAME,
            &fingerprint,
            None,
            Some(1),
            Some("generated-hostname"),
        )
        .unwrap();
        let marker_name = registry_record_name(name, DnsRecordType::CNAME);
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

    #[tokio::test]
    async fn reconcile_treats_a_respelled_generated_cname_as_converged() {
        let base = "example.com";
        let fqdn = "app-staging.cp.example.com";
        // Temps wrote `edge.example.net`; the provider lists it upper-cased
        // with a root dot.
        let provider = MockProvider::new(generated_cname_records(
            "app-staging.cp",
            base,
            "Edge.Example.NET.",
            "edge.example.net",
        ));
        let before = provider.fqdns();

        // Neither the listed spelling nor the configured one forces a write.
        for configured_edge in ["edge.example.net", " EDGE.example.net. "] {
            let changes = reconcile_zone_records(
                &provider,
                base,
                &[host(fqdn)],
                configured_edge,
                options(false),
            )
            .await
            .unwrap();
            assert!(changes.is_empty(), "{configured_edge:?}: {changes:?}");
        }

        assert_eq!(provider.fqdns(), before);
        assert_eq!(
            provider.value_of(fqdn).as_deref(),
            Some("Edge.Example.NET."),
            "a converged record must not be rewritten"
        );
    }

    #[tokio::test]
    async fn reconcile_writes_and_reports_the_canonical_edge_target() {
        let base = "example.com";
        let fqdn = "app-staging.cp.example.com";
        let provider = MockProvider::new(vec![]);

        let changes = reconcile_zone_records(
            &provider,
            base,
            &[host(fqdn)],
            " Edge.Example.NET. ",
            options(false),
        )
        .await
        .unwrap();

        assert_eq!(actions(&changes), vec![change("create", "CNAME", fqdn)]);
        assert_eq!(changes[0].value, "edge.example.net");
        assert_eq!(provider.value_of(fqdn).as_deref(), Some("edge.example.net"));

        // The record it wrote is owned and converged on the next run.
        let again = reconcile_zone_records(
            &provider,
            base,
            &[host(fqdn)],
            "edge.example.net",
            options(false),
        )
        .await
        .unwrap();
        assert!(again.is_empty(), "{again:?}");
    }

    #[test]
    fn zone_operation_lock_key_uses_the_stored_zone_form() {
        assert_eq!(
            zone_operation_lock_key(7, " *.Example.COM. "),
            "managed-dns-zone-op:7:example.com"
        );
        assert_ne!(
            zone_operation_lock_key(7, "example.com"),
            zone_operation_lock_key(8, "example.com"),
            "zones of different providers are separate locks"
        );
    }

    #[tokio::test]
    async fn zone_operation_lock_admits_one_operation_per_zone() {
        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) => {
                eprintln!(
                    "Docker/Postgres unavailable; skipping zone operation lock test: {error}"
                );
                return;
            }
        };
        let db = test_db.connection_arc();

        let first = ZoneOperationLock::acquire(db.as_ref(), 7, "Example.COM.")
            .await
            .expect("first operation takes the zone lock");
        let refused = ZoneOperationLock::acquire(db.as_ref(), 7, "example.com")
            .await
            .err()
            .expect("a second operation on the same zone must fail fast");
        assert!(
            matches!(
                &refused,
                DnsError::ZoneOperationInProgress { provider_id: 7, zone } if zone == "example.com"
            ),
            "{refused:?}"
        );

        // Other zones, other providers' zones and single-record writes in the
        // zone are separate locks.
        let other_zone = ZoneOperationLock::acquire(db.as_ref(), 7, "example.net")
            .await
            .expect("another zone is not blocked");
        let other_provider = ZoneOperationLock::acquire(db.as_ref(), 8, "example.com")
            .await
            .expect("the same zone name on another provider is not blocked");
        let record = ManagedDnsRecordService::lock_record_in_db(db.as_ref(), "example.com", "app")
            .await
            .expect("a record write in the zone does not wait on the zone lock");
        record.rollback().await.expect("release the record lock");
        other_zone
            .finish(Ok(()))
            .await
            .expect("release the other zone");
        other_provider
            .finish(Ok(()))
            .await
            .expect("release the other provider's zone");

        // A plan for another zone never runs under this zone's lock.
        let mismatch = first
            .ensure_covers("example.net")
            .expect_err("a lock covers only its own zone");
        assert!(matches!(mismatch, DnsError::Validation(_)), "{mismatch:?}");

        // A failed operation still releases the lock, and keeps its own error.
        let failed = first
            .finish::<()>(Err(DnsError::Validation("boom".into())))
            .await
            .expect_err("the operation's error is returned");
        assert!(matches!(failed, DnsError::Validation(ref message) if message == "boom"));
        let next = ZoneOperationLock::acquire(db.as_ref(), 7, "example.com")
            .await
            .expect("the zone is free once the first operation ends");
        next.finish(Ok(())).await.expect("release the zone");
    }

    const EDGE: &str = "203.0.113.10";

    /// A managed `example.com` zone in standard mode, proxied by default,
    /// on a new DNS provider row.
    async fn managed_zone_row(db: &DatabaseConnection) -> dns_managed_domains::Model {
        let now = chrono::Utc::now();
        let provider = temps_entities::dns_providers::ActiveModel {
            name: Set("hostname-mode-test".into()),
            provider_type: Set("cloudflare".into()),
            credentials: Set("{}".into()),
            is_active: Set(true),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert provider");
        dns_managed_domains::ActiveModel {
            provider_id: Set(provider.id),
            domain: Set("example.com".into()),
            auto_manage: Set(true),
            proxied_by_default: Set(true),
            verified: Set(true),
            generated_hostname_mode: Set("standard".into()),
            sync_generated_records: Set(false),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert managed domain")
    }

    async fn stored_mode(db: &DatabaseConnection, managed: &dns_managed_domains::Model) -> String {
        dns_managed_domains::Entity::find_by_id(managed.id)
            .one(db)
            .await
            .expect("read managed domain")
            .expect("managed domain exists")
            .generated_hostname_mode
    }

    /// `(fqdn, proxied)` of the zone's saved generated-hostname states.
    async fn stored_states(db: &DatabaseConnection) -> Vec<(String, bool)> {
        let mut states: Vec<(String, bool)> = dns_managed_record_states::Entity::find()
            .all(db)
            .await
            .expect("read record states")
            .into_iter()
            .map(|state| (state.fqdn, state.proxied))
            .collect();
        states.sort();
        states
    }

    /// Plan and apply `mode` for `desired` under the zone lock, as a
    /// hostname-mode apply does.
    async fn apply_mode(
        db: &DatabaseConnection,
        provider: &MockProvider,
        managed: &dns_managed_domains::Model,
        desired: &[GeneratedHost],
    ) -> Result<Vec<RecordChange>, DnsError> {
        apply_mode_at_edge(db, provider, managed, desired, EDGE).await
    }

    /// [`apply_mode`] with the edge at `edge`.
    async fn apply_mode_at_edge(
        db: &DatabaseConnection,
        provider: &MockProvider,
        managed: &dns_managed_domains::Model,
        desired: &[GeneratedHost],
        edge: &str,
    ) -> Result<Vec<RecordChange>, DnsError> {
        apply_mode_deciding(db, provider, managed, desired, edge, &NO_DECISIONS).await
    }

    /// [`apply_mode_at_edge`] with the user's adopt and skip `decisions`.
    async fn apply_mode_deciding(
        db: &DatabaseConnection,
        provider: &MockProvider,
        managed: &dns_managed_domains::Model,
        desired: &[GeneratedHost],
        edge: &str,
        decisions: &ConflictDecisions,
    ) -> Result<Vec<RecordChange>, DnsError> {
        let zone_lock = ZoneOperationLock::acquire(db, managed.provider_id, &managed.domain)
            .await
            .expect("take the zone lock");
        let outcome = async {
            let plan = plan_zone_records(
                provider,
                &managed.domain,
                desired,
                edge,
                PlanOptions {
                    proxied: managed.proxied_by_default,
                    instance_id: INSTANCE,
                    signing_key: &SIGNING_KEY,
                    decisions,
                },
            )
            .await?;
            apply_hostname_mode_plan(
                db,
                provider,
                &zone_lock,
                plan,
                HostnameModeSwitch {
                    managed,
                    target: PublicHostnameStrategy::Flat,
                    desired,
                    edge_target: edge,
                },
                INSTANCE,
                &SIGNING_KEY,
            )
            .await
        }
        .await;
        zone_lock.finish(outcome).await
    }

    fn change_names(changes: &[RecordChange]) -> Vec<String> {
        changes
            .iter()
            .map(|change| format!("{} {} {}", change.action, change.record_type, change.name))
            .collect()
    }

    /// An apply that fails while writing the new mode's records keeps the
    /// current mode, so routes keep using hostnames whose records still
    /// exist. The records it did write are reported, and their record
    /// states are saved so Cloudflare-proxied ones get origin certificates.
    /// Applying again finishes the work.
    #[tokio::test]
    async fn hostname_mode_apply_stopped_while_writing_keeps_the_mode_and_saves_written_states() {
        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) => {
                eprintln!(
                    "Docker/Postgres unavailable; skipping hostname-mode apply test: {error}"
                );
                return;
            }
        };
        let db = test_db.connection_arc();
        let managed = managed_zone_row(db.as_ref()).await;
        let desired = [
            host("one.example.com"),
            host("two.example.com"),
            host("three.example.com"),
        ];
        let provider = MockProvider::new(Vec::new());
        provider.fail_writes_of("three");

        let error = apply_mode(db.as_ref(), &provider, &managed, &desired)
            .await
            .expect_err("the third record write fails");
        let DnsError::HostnameModeIncomplete(incomplete) = &error else {
            panic!("expected an incomplete apply, got {error:?}");
        };
        assert_eq!(incomplete.saved, HostnameModeSaved::RecordStates, "{error}");
        assert_eq!(
            change_names(&incomplete.completed),
            ["create A one.example.com", "create A two.example.com"]
        );
        assert!(
            matches!(incomplete.source, DnsError::ApiError(_)),
            "{error}"
        );
        assert_eq!(stored_mode(db.as_ref(), &managed).await, "standard");
        assert_eq!(
            stored_states(db.as_ref()).await,
            [
                ("one.example.com".to_string(), true),
                ("two.example.com".to_string(), true)
            ]
        );

        provider.heal();
        let finished = apply_mode(db.as_ref(), &provider, &managed, &desired)
            .await
            .expect("applying again finishes the work");
        assert_eq!(change_names(&finished), ["create A three.example.com"]);
        assert_eq!(stored_mode(db.as_ref(), &managed).await, "flat");
        assert_eq!(
            stored_states(db.as_ref()).await,
            [
                ("one.example.com".to_string(), true),
                ("three.example.com".to_string(), true),
                ("two.example.com".to_string(), true)
            ]
        );
    }

    /// The user resolves an upgraded zone's conflicts: one unmarked record is
    /// adopted, another hostname skipped. The apply saves the mode with
    /// record states for the adopted and created records only: the skipped
    /// record is not Temps', so the proxy must not serve its origin
    /// certificate for it.
    #[tokio::test]
    async fn hostname_mode_apply_saves_states_for_adopted_hostnames_but_not_skipped_ones() {
        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) => {
                eprintln!(
                    "Docker/Postgres unavailable; skipping hostname-mode apply test: {error}"
                );
                return;
            }
        };
        let db = test_db.connection_arc();
        let managed = managed_zone_row(db.as_ref()).await;
        let provider = MockProvider::new(vec![
            record("adopted", "example.com", "198.51.100.7"),
            record("skipped", "example.com", "198.51.100.8"),
        ]);
        let desired = [
            host("adopted.example.com"),
            host("skipped.example.com"),
            host("created.example.com"),
        ];

        let refused = apply_mode(db.as_ref(), &provider, &managed, &desired)
            .await
            .expect_err("unresolved conflicts refuse the apply");
        assert!(
            matches!(refused, DnsError::GeneratedHostnameConflicts { ref conflicts, .. } if conflicts.len() == 2),
            "{refused}"
        );
        assert_eq!(stored_mode(db.as_ref(), &managed).await, "standard");
        assert!(stored_states(db.as_ref()).await.is_empty());

        let reviewed = plan_zone_records(
            &provider,
            &managed.domain,
            &desired,
            EDGE,
            PlanOptions {
                proxied: managed.proxied_by_default,
                ..plan_options()
            },
        )
        .await
        .expect("preview the sync")
        .conflicts;
        let decisions = deciding(vec![adopt(&reviewed[0])], vec![skip(&reviewed[1])]);
        let changes =
            apply_mode_deciding(db.as_ref(), &provider, &managed, &desired, EDGE, &decisions)
                .await
                .expect("apply with every conflict resolved");

        assert_eq!(
            change_names(&changes),
            [
                "adopt A adopted.example.com",
                "update A adopted.example.com",
                "create A created.example.com"
            ]
        );
        assert_eq!(stored_mode(db.as_ref(), &managed).await, "flat");
        assert_eq!(
            stored_states(db.as_ref()).await,
            [
                ("adopted.example.com".to_string(), true),
                ("created.example.com".to_string(), true)
            ]
        );
        assert_eq!(
            provider.value_of("skipped.example.com").as_deref(),
            Some("198.51.100.8")
        );
        assert_eq!(
            provider.value_of("adopted.example.com").as_deref(),
            Some(EDGE)
        );
    }

    /// Records the new mode no longer uses are removed only after the mode
    /// and its record states are saved. A failure there keeps the saved
    /// mode, so routes follow the records that were written, and says so;
    /// applying again removes the rest.
    #[tokio::test]
    async fn hostname_mode_apply_stopped_while_removing_keeps_the_saved_mode() {
        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) => {
                eprintln!(
                    "Docker/Postgres unavailable; skipping hostname-mode apply test: {error}"
                );
                return;
            }
        };
        let db = test_db.connection_arc();
        let managed = managed_zone_row(db.as_ref()).await;
        let desired = [host("one.example.com")];
        let provider = MockProvider::new(owned_records("old", "example.com", EDGE));
        provider.fail_deletes_of("id-old");

        let error = apply_mode(db.as_ref(), &provider, &managed, &desired)
            .await
            .expect_err("removing the unused record fails");
        let DnsError::HostnameModeIncomplete(incomplete) = &error else {
            panic!("expected an incomplete apply, got {error:?}");
        };
        assert_eq!(incomplete.saved, HostnameModeSaved::Mode, "{error}");
        assert_eq!(
            change_names(&incomplete.completed),
            ["create A one.example.com"]
        );
        assert!(
            error.to_string().contains("record states were saved"),
            "{error}"
        );
        assert_eq!(stored_mode(db.as_ref(), &managed).await, "flat");
        assert_eq!(
            stored_states(db.as_ref()).await,
            [("one.example.com".to_string(), true)]
        );
        assert!(provider.fqdns().contains(&"old.example.com".to_string()));

        provider.heal();
        let finished = apply_mode(db.as_ref(), &provider, &managed, &desired)
            .await
            .expect("applying again removes the unused record");
        assert_eq!(change_names(&finished), ["delete A old.example.com"]);
        assert!(!provider.fqdns().contains(&"old.example.com".to_string()));
        assert_eq!(stored_mode(db.as_ref(), &managed).await, "flat");
    }

    /// An apply whose replacement of a record the current mode uses fails
    /// writes the previous record back: the mode is unchanged, the name
    /// resolves as before, and its record state is kept.
    #[tokio::test]
    async fn hostname_mode_apply_writes_back_a_record_whose_replacement_failed() {
        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) => {
                eprintln!(
                    "Docker/Postgres unavailable; skipping hostname-mode apply test: {error}"
                );
                return;
            }
        };
        let db = test_db.connection_arc();
        let managed = managed_zone_row(db.as_ref()).await;
        let desired = [host("one.example.com")];
        // Published as an A for the edge address; the edge is now a hostname.
        let provider = MockProvider::new(owned_records("one", "example.com", EDGE));
        provider.fail_writes_of_type("one", DnsRecordType::CNAME);

        let error = apply_mode_at_edge(
            db.as_ref(),
            &provider,
            &managed,
            &desired,
            "edge.example.net",
        )
        .await
        .expect_err("the CNAME write fails");
        let DnsError::HostnameModeIncomplete(incomplete) = &error else {
            panic!("expected an incomplete apply, got {error:?}");
        };
        assert_eq!(incomplete.saved, HostnameModeSaved::RecordStates, "{error}");
        assert_eq!(
            change_names(&incomplete.completed),
            ["delete A one.example.com", "restore A one.example.com"]
        );
        assert_eq!(stored_mode(db.as_ref(), &managed).await, "standard");
        assert_eq!(
            stored_states(db.as_ref()).await,
            [("one.example.com".to_string(), false)]
        );
        assert_eq!(
            provider.routing_at("one.example.com"),
            [format!("A {EDGE}")]
        );
    }
}
