//! Internal DNS registry — authoritative store for `*.temps.local` records.
//!
//! See ADR-011. This service is the **only** writer to `service_endpoints`
//! and `node_dns_state`. Per-node Hickory resolvers consume it via the
//! sync HTTP endpoint (handlers/dns_sync.rs) — they never read the DB
//! directly.
//!
//! ## Generation semantics
//!
//! Every mutation bumps a cluster-wide monotonic counter. Agents long-poll
//! `WHERE generation > $applied_generation` and ACK back what they applied.
//! The counter is computed at write-time inside a transaction as
//! `MAX(generation) + 1`, with `FOR UPDATE` on the holding row to serialise
//! concurrent writers. This is intentional: we want a *strict* monotonic
//! sequence so the `since=N` long-poll diff is well-defined.
//!
//! ## Container restarts (the IP-churn problem)
//!
//! Docker assigns a fresh IP to a container on every `docker create` (and
//! after `rm` + `create`). The lifecycle hook therefore calls
//! [`replace_endpoints_for_owner`] on every container *start*, not just
//! first creation. That method does delete+insert atomically inside one
//! transaction so consumers never see a window with both old and new IPs
//! present. Old generation numbers are not reused — the new insert gets a
//! fresh, higher generation.
//!
//! ## Distinct from `DnsRecordService`
//!
//! `DnsRecordService` (in `record_service.rs`) manages DNS records at
//! **external providers** like Cloudflare and Route53 for user-facing
//! domains. `DnsRegistry` manages records in the **internal** zone served
//! by Temps' own per-node resolvers. They share zero code and serve
//! completely different traffic.

use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseConnection,
    DatabaseTransaction, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Statement,
    TransactionTrait,
};
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;
use temps_core::DBDateTime;
use temps_entities::{node_dns_state, service_endpoints};
use thiserror::Error;
use tracing::{debug, info, warn};

/// Allowed DNS record types in the internal zone.
///
/// Matches the CHECK constraint on `service_endpoints.record_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordType {
    A,
    Aaaa,
    Srv,
    Cname,
}

impl RecordType {
    pub fn as_str(&self) -> &'static str {
        match self {
            RecordType::A => "A",
            RecordType::Aaaa => "AAAA",
            RecordType::Srv => "SRV",
            RecordType::Cname => "CNAME",
        }
    }
}

impl FromStr for RecordType {
    type Err = DnsRegistryError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "A" => Ok(RecordType::A),
            "AAAA" => Ok(RecordType::Aaaa),
            "SRV" => Ok(RecordType::Srv),
            "CNAME" => Ok(RecordType::Cname),
            other => Err(DnsRegistryError::Validation {
                message: format!("unknown record_type {:?}", other),
            }),
        }
    }
}

/// Allowed owner kinds. Matches the CHECK constraint on
/// `service_endpoints.owner_kind`. Determines how `owner_id` is interpreted
/// for GC and lookups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerKind {
    /// `owner_id` is `service_members.id` — Tier 2 records (per-container).
    ServiceMember,
    /// `owner_id` is `external_services.id` — Tier 3 records (role aliases
    /// like `primary.<svc>` and the `<svc>.temps.local` VIP).
    ServiceRole,
    /// `owner_id` is `nodes.id` — node identity records.
    Node,
    /// Opaque static configuration (cluster-wide constants, hand-edited).
    Static,
    /// `owner_id` is `deployments.id` — the stable per-deployment FQDN
    /// (`<env-slug>.<project-slug>.temps.local`) that the edge proxy
    /// resolves to a live container set. Decouples client-side DNS from
    /// container churn: the record points at the proxy, the proxy
    /// fans out to whatever is currently running.
    Deployment,
}

impl OwnerKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            OwnerKind::ServiceMember => "service_member",
            OwnerKind::ServiceRole => "service_role",
            OwnerKind::Node => "node",
            OwnerKind::Static => "static",
            OwnerKind::Deployment => "deployment",
        }
    }
}

impl FromStr for OwnerKind {
    type Err = DnsRegistryError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "service_member" => Ok(OwnerKind::ServiceMember),
            "service_role" => Ok(OwnerKind::ServiceRole),
            "node" => Ok(OwnerKind::Node),
            "static" => Ok(OwnerKind::Static),
            "deployment" => Ok(OwnerKind::Deployment),
            other => Err(DnsRegistryError::Validation {
                message: format!("unknown owner_kind {:?}", other),
            }),
        }
    }
}

/// Resolver health states. Mirrors `node_dns_state.health` CHECK.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolverHealth {
    Healthy,
    Degraded,
    Stale,
    Unknown,
}

impl ResolverHealth {
    pub fn as_str(&self) -> &'static str {
        match self {
            ResolverHealth::Healthy => "healthy",
            ResolverHealth::Degraded => "degraded",
            ResolverHealth::Stale => "stale",
            ResolverHealth::Unknown => "unknown",
        }
    }
}

/// One record to insert. `generation` is assigned by the service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointDraft {
    pub fqdn: String,
    pub record_type: RecordType,
    /// Required for A/AAAA. For CNAME, the target hostname goes here.
    /// For SRV (which we don't write today but the schema supports), the
    /// resolved target IP.
    pub target_ip: Option<String>,
    pub target_port: Option<i32>,
    pub ttl: i32,
    pub owner_kind: OwnerKind,
    pub owner_id: i64,
    pub node_id: Option<i32>,
}

impl EndpointDraft {
    fn validate(&self) -> Result<(), DnsRegistryError> {
        if self.fqdn.trim().is_empty() {
            return Err(DnsRegistryError::Validation {
                message: "fqdn cannot be empty".into(),
            });
        }
        if self.fqdn.len() > 253 {
            return Err(DnsRegistryError::Validation {
                message: format!(
                    "fqdn {:?} exceeds 253 chars (RFC 1035)",
                    truncate(&self.fqdn, 80)
                ),
            });
        }
        if self.ttl <= 0 || self.ttl > 86_400 {
            return Err(DnsRegistryError::Validation {
                message: format!(
                    "ttl {} out of range (1..=86400) for fqdn {}",
                    self.ttl, self.fqdn
                ),
            });
        }
        match self.record_type {
            RecordType::A => {
                let ip = self
                    .target_ip
                    .as_deref()
                    .ok_or_else(|| DnsRegistryError::Validation {
                        message: format!("A record for {} requires target_ip", self.fqdn),
                    })?;
                let parsed = IpAddr::from_str(ip).map_err(|_| DnsRegistryError::InvalidIp {
                    fqdn: self.fqdn.clone(),
                    value: ip.to_string(),
                })?;
                if !parsed.is_ipv4() {
                    return Err(DnsRegistryError::Validation {
                        message: format!("A record for {} requires IPv4, got {}", self.fqdn, ip),
                    });
                }
            }
            RecordType::Aaaa => {
                let ip = self
                    .target_ip
                    .as_deref()
                    .ok_or_else(|| DnsRegistryError::Validation {
                        message: format!("AAAA record for {} requires target_ip", self.fqdn),
                    })?;
                let parsed = IpAddr::from_str(ip).map_err(|_| DnsRegistryError::InvalidIp {
                    fqdn: self.fqdn.clone(),
                    value: ip.to_string(),
                })?;
                if !parsed.is_ipv6() {
                    return Err(DnsRegistryError::Validation {
                        message: format!("AAAA record for {} requires IPv6, got {}", self.fqdn, ip),
                    });
                }
            }
            RecordType::Srv | RecordType::Cname => {
                if self.target_ip.as_deref().map(str::is_empty).unwrap_or(true) {
                    return Err(DnsRegistryError::Validation {
                        message: format!(
                            "{} record for {} requires non-empty target",
                            self.record_type.as_str(),
                            self.fqdn
                        ),
                    });
                }
            }
        }
        Ok(())
    }
}

/// What an agent receives in response to `GET .../dns/changes?since=N`.
#[derive(Debug, Clone)]
pub struct ChangeSet {
    /// The highest generation included in this response — the agent ACKs
    /// this back after applying.
    pub generation: i64,
    /// True when `since=0` or the diff is too large; client should replace
    /// its full zone instead of merging.
    pub full_snapshot: bool,
    /// Records to add (or, in snapshot mode, the entire zone).
    pub records: Vec<service_endpoints::Model>,
    /// Records the agent should remove from its zone. Empty in snapshot mode.
    pub removed_ids: Vec<i64>,
}

#[derive(Error, Debug)]
pub enum DnsRegistryError {
    #[error("DNS endpoint {endpoint_id} not found")]
    NotFound { endpoint_id: i64 },

    #[error("Node DNS state not found for node {node_id}")]
    NodeStateNotFound { node_id: i32 },

    #[error("Validation error: {message}")]
    Validation { message: String },

    #[error("Invalid IP literal {value:?} for {fqdn}")]
    InvalidIp { fqdn: String, value: String },

    #[error("ACK rejected: node {node_id} tried to ACK generation {acked} but only {current} has been issued")]
    AckTooHigh {
        node_id: i32,
        acked: i64,
        current: i64,
    },

    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
}

// Note: `#[from]` on the `Database` variant generates `From<sea_orm::DbErr>`.
// We deliberately do NOT map `RecordNotFound` to `NotFound` at this layer
// because `NotFound` requires an `endpoint_id`. Callers that have the ID
// in scope produce the typed `NotFound` themselves.

/// Snapshot returned by `get_full_zone`. Equivalent to a `since=0` ChangeSet
/// but distinguished at the type level so call sites can't accidentally
/// treat it as a diff.
#[derive(Debug, Clone)]
pub struct ZoneSnapshot {
    pub generation: i64,
    pub records: Vec<service_endpoints::Model>,
}

/// If the diff would include more records than this, we fall back to
/// returning a full snapshot instead. Picked to keep a single response
/// well under typical body limits while still letting agents catch up
/// from short outages without re-snapshotting.
const SNAPSHOT_THRESHOLD: usize = 1_000;

#[derive(Clone)]
pub struct DnsRegistry {
    db: Arc<DatabaseConnection>,
}

impl DnsRegistry {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Insert or replace exactly the records belonging to one owner,
    /// atomically. **This is the lifecycle hook's primary entry point.**
    ///
    /// Behaviour:
    /// 1. Open a transaction.
    /// 2. Compute the next generation (`MAX(generation) + 1` over the
    ///    whole table — cluster-wide monotonic).
    /// 3. Delete every existing row with `(owner_kind, owner_id) = (k, id)`.
    /// 4. Insert each new draft with the new generation.
    /// 5. Commit.
    ///
    /// A container restart that produces a different IP becomes a
    /// delete-old + insert-new in one generation bump. Resolvers see
    /// the change as a single atomic update.
    pub async fn replace_endpoints_for_owner(
        &self,
        owner_kind: OwnerKind,
        owner_id: i64,
        drafts: &[EndpointDraft],
    ) -> Result<i64, DnsRegistryError> {
        for d in drafts {
            d.validate()?;
            if d.owner_kind != owner_kind || d.owner_id != owner_id {
                return Err(DnsRegistryError::Validation {
                    message: format!(
                        "draft owner ({}, {}) does not match call args ({}, {})",
                        d.owner_kind.as_str(),
                        d.owner_id,
                        owner_kind.as_str(),
                        owner_id
                    ),
                });
            }
        }

        let txn = self.db.begin().await?;
        let generation = next_generation(&txn).await?;

        let deleted = service_endpoints::Entity::delete_many()
            .filter(service_endpoints::Column::OwnerKind.eq(owner_kind.as_str()))
            .filter(service_endpoints::Column::OwnerId.eq(owner_id))
            .exec(&txn)
            .await?;
        debug!(
            owner_kind = owner_kind.as_str(),
            owner_id,
            deleted = deleted.rows_affected,
            new_generation = generation,
            new_records = drafts.len(),
            "replacing DNS endpoints for owner"
        );

        for d in drafts {
            let now = chrono::Utc::now();
            let am = service_endpoints::ActiveModel {
                fqdn: Set(d.fqdn.clone()),
                record_type: Set(d.record_type.as_str().to_string()),
                target_ip: Set(d.target_ip.clone()),
                target_port: Set(d.target_port),
                ttl: Set(d.ttl),
                owner_kind: Set(d.owner_kind.as_str().to_string()),
                owner_id: Set(d.owner_id),
                node_id: Set(d.node_id),
                generation: Set(generation),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            };
            // Idempotent publish: the delete above clears our own prior records,
            // but a re-deploy can reuse a node's deterministic container IP, so the
            // (fqdn, record_type, target_ip) tuple may still collide with a row left
            // by an earlier deployment generation (different owner_id). Upsert so the
            // newest generation/owner wins instead of aborting the whole publish on a
            // unique-constraint violation (which previously stalled DNS on re-deploy,
            // leaving the zone pinned to dead container IPs).
            service_endpoints::Entity::insert(am)
                .on_conflict(
                    sea_orm::sea_query::OnConflict::columns([
                        service_endpoints::Column::Fqdn,
                        service_endpoints::Column::RecordType,
                        service_endpoints::Column::TargetIp,
                    ])
                    .update_columns([
                        service_endpoints::Column::TargetPort,
                        service_endpoints::Column::Ttl,
                        service_endpoints::Column::OwnerKind,
                        service_endpoints::Column::OwnerId,
                        service_endpoints::Column::NodeId,
                        service_endpoints::Column::Generation,
                        service_endpoints::Column::UpdatedAt,
                    ])
                    .to_owned(),
                )
                .exec(&txn)
                .await?;
        }

        txn.commit().await?;
        info!(
            owner_kind = owner_kind.as_str(),
            owner_id,
            generation,
            count = drafts.len(),
            "DNS endpoints replaced"
        );
        Ok(generation)
    }

    /// Delete every record for an owner. Used by the container-stop hook
    /// and by the GC reconciler. No-op if no records exist (returns 0).
    pub async fn delete_by_owner(
        &self,
        owner_kind: OwnerKind,
        owner_id: i64,
    ) -> Result<u64, DnsRegistryError> {
        let txn = self.db.begin().await?;
        let res = service_endpoints::Entity::delete_many()
            .filter(service_endpoints::Column::OwnerKind.eq(owner_kind.as_str()))
            .filter(service_endpoints::Column::OwnerId.eq(owner_id))
            .exec(&txn)
            .await?;

        // Bump the generation iff something actually changed; otherwise
        // we'd churn the long-poll cursor for nothing.
        if res.rows_affected > 0 {
            let _ = next_generation(&txn).await?;
        }
        txn.commit().await?;
        Ok(res.rows_affected)
    }

    /// Hourly janitor: delete `service_endpoints` rows whose owner has
    /// vanished from `service_members` (Tier 2 GC) or `external_services`
    /// (Tier 3 GC). Returns the number of orphan records deleted.
    ///
    /// Catches edge cases the live lifecycle hooks miss:
    /// - A `delete_service` whose `delete_by_owner` call failed mid-flight
    ///   (control-plane crash between DB tx commit and DNS cleanup).
    /// - Manual DB surgery that removes members without going through the
    ///   manager.
    /// - Records left over from earlier code revisions.
    ///
    /// One transaction wraps the deletion + generation bump, so resolvers
    /// observe orphan removal as a single atomic update.
    pub async fn gc_orphan_records(&self) -> Result<u64, DnsRegistryError> {
        let txn = self.db.begin().await?;

        // Tier 2: orphan service_member records. NOT EXISTS is faster than
        // a LEFT JOIN here because owner_id is BIGINT and service_members.id
        // is INT — the planner uses the PK index either way, but NOT EXISTS
        // lets it short-circuit on the first match.
        let tier2 = txn
            .execute(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "DELETE FROM service_endpoints \
                 WHERE owner_kind = 'service_member' \
                 AND NOT EXISTS ( \
                     SELECT 1 FROM service_members WHERE id = service_endpoints.owner_id \
                 )"
                .to_string(),
            ))
            .await?;

        // Tier 3: orphan service_role records (owner_id is external_services.id).
        let tier3 = txn
            .execute(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "DELETE FROM service_endpoints \
                 WHERE owner_kind = 'service_role' \
                 AND NOT EXISTS ( \
                     SELECT 1 FROM external_services WHERE id = service_endpoints.owner_id \
                 )"
                .to_string(),
            ))
            .await?;

        let total = tier2.rows_affected() + tier3.rows_affected();
        if total > 0 {
            let _ = next_generation(&txn).await?;
        }
        txn.commit().await?;
        Ok(total)
    }

    /// Drift-detection query: list nodes whose resolver hasn't ACK'd any
    /// generation in the last `stale_after_seconds` seconds. Used by the
    /// janitor to log warnings (and eventually surface in the ops UI).
    ///
    /// Returns `(node_id, applied_generation, server_generation)` tuples.
    /// `node_id` is from `node_dns_state`; `server_generation` is the
    /// current cluster-wide generation. The gap between the two is how
    /// far behind the resolver is.
    pub async fn list_stale_resolvers(
        &self,
        stale_after_seconds: i64,
    ) -> Result<Vec<StaleResolver>, DnsRegistryError> {
        if stale_after_seconds <= 0 {
            return Err(DnsRegistryError::Validation {
                message: format!("stale_after_seconds must be positive, got {stale_after_seconds}"),
            });
        }
        let current = current_generation(self.db.as_ref()).await?;
        let rows = self
            .db
            .as_ref()
            .query_all(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT node_id, applied_generation \
                 FROM node_dns_state \
                 WHERE last_sync_at IS NULL \
                    OR last_sync_at < (now() - ($1::bigint || ' seconds')::interval) \
                 ORDER BY node_id",
                [stale_after_seconds.into()],
            ))
            .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let node_id: i32 = row
                .try_get("", "node_id")
                .map_err(DnsRegistryError::Database)?;
            let applied: i64 = row
                .try_get("", "applied_generation")
                .map_err(DnsRegistryError::Database)?;
            out.push(StaleResolver {
                node_id,
                applied_generation: applied,
                server_generation: current,
            });
        }
        Ok(out)
    }

    /// Long-poll diff. Returns records with `generation > since` plus
    /// (separately) the IDs of any records the agent should drop.
    ///
    /// Note: with the current "delete-old, insert-new" replace semantics,
    /// removed records leave no row behind we can hand back as a tombstone.
    /// Step 2 (the resolver crate) handles this by replacing the entire
    /// zone whenever it observes a generation jump it can't account for —
    /// for now we always return `removed_ids = []` and let the resolver
    /// reconcile by name. If the diff is large or `since=0`, the response
    /// is a full snapshot instead.
    pub async fn get_changes_since(&self, since: i64) -> Result<ChangeSet, DnsRegistryError> {
        let current = current_generation(self.db.as_ref()).await?;
        if since <= 0 || current <= since {
            // since=0 → snapshot. current<=since → no change (snapshot=false,
            // empty records, generation=current).
            if since <= 0 {
                let snap = self.get_full_zone().await?;
                return Ok(ChangeSet {
                    generation: snap.generation,
                    full_snapshot: true,
                    records: snap.records,
                    removed_ids: vec![],
                });
            }
            return Ok(ChangeSet {
                generation: current,
                full_snapshot: false,
                records: vec![],
                removed_ids: vec![],
            });
        }

        let records = service_endpoints::Entity::find()
            .filter(service_endpoints::Column::Generation.gt(since))
            .order_by_asc(service_endpoints::Column::Generation)
            .order_by_asc(service_endpoints::Column::Id)
            .limit((SNAPSHOT_THRESHOLD as u64) + 1)
            .all(self.db.as_ref())
            .await?;

        if records.len() > SNAPSHOT_THRESHOLD {
            warn!(
                since,
                current, "diff exceeds snapshot threshold; returning full zone instead"
            );
            let snap = self.get_full_zone().await?;
            return Ok(ChangeSet {
                generation: snap.generation,
                full_snapshot: true,
                records: snap.records,
                removed_ids: vec![],
            });
        }

        Ok(ChangeSet {
            generation: current,
            full_snapshot: false,
            records,
            removed_ids: vec![],
        })
    }

    /// Return the entire zone plus the current generation. Used when an
    /// agent first connects, recovers from a long outage, or after a
    /// reconciliation that detected drift.
    pub async fn get_full_zone(&self) -> Result<ZoneSnapshot, DnsRegistryError> {
        let generation = current_generation(self.db.as_ref()).await?;
        let records = service_endpoints::Entity::find()
            .order_by_asc(service_endpoints::Column::Fqdn)
            .order_by_asc(service_endpoints::Column::RecordType)
            .order_by_asc(service_endpoints::Column::Id)
            .all(self.db.as_ref())
            .await?;
        Ok(ZoneSnapshot {
            generation,
            records,
        })
    }

    /// Agent ACK after applying a generation. Idempotent: if `applied` is
    /// less than the stored value, the row stays as-is (no rollback) and
    /// `last_sync_at` is still bumped so we know the agent is alive.
    /// `applied > current_generation()` is rejected — that would mean the
    /// agent claims state we never produced.
    pub async fn ack_applied(
        &self,
        node_id: i32,
        applied: i64,
    ) -> Result<node_dns_state::Model, DnsRegistryError> {
        let current = current_generation(self.db.as_ref()).await?;
        if applied > current {
            return Err(DnsRegistryError::AckTooHigh {
                node_id,
                acked: applied,
                current,
            });
        }

        let txn = self.db.begin().await?;
        let existing = node_dns_state::Entity::find_by_id(node_id)
            .one(&txn)
            .await?;
        let now = chrono::Utc::now();

        let model = match existing {
            Some(row) => {
                let new_applied = std::cmp::max(row.applied_generation, applied);
                let mut am: node_dns_state::ActiveModel = row.into();
                am.applied_generation = Set(new_applied);
                am.last_sync_at = Set(Some(now));
                am.health = Set(ResolverHealth::Healthy.as_str().to_string());
                am.update(&txn).await?
            }
            None => {
                let am = node_dns_state::ActiveModel {
                    node_id: Set(node_id),
                    applied_generation: Set(applied),
                    last_sync_at: Set(Some(now)),
                    health: Set(ResolverHealth::Healthy.as_str().to_string()),
                };
                am.insert(&txn).await?
            }
        };
        txn.commit().await?;
        Ok(model)
    }

    /// Read-only: applied state for a node. Used by ops dashboards and the
    /// drift detector.
    pub async fn get_node_state(
        &self,
        node_id: i32,
    ) -> Result<Option<node_dns_state::Model>, DnsRegistryError> {
        let row = node_dns_state::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await?;
        Ok(row)
    }
}

/// Drift-detector row returned by [`DnsRegistry::list_stale_resolvers`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleResolver {
    pub node_id: i32,
    pub applied_generation: i64,
    pub server_generation: i64,
}

impl StaleResolver {
    /// How many generations behind the cluster this node is.
    pub fn lag(&self) -> i64 {
        self.server_generation - self.applied_generation
    }
}

/// Bump the cluster-wide monotonic generation counter and return the new
/// value. Atomic via `UPDATE ... RETURNING` against the `dns_generation`
/// singleton row. Concurrent writers serialise on Postgres' row lock.
///
/// The counter lives in its own table (not derived from
/// `MAX(service_endpoints.generation)`) so that:
///
/// - deleting all rows from `service_endpoints` does NOT reset the counter
///   (which would break the long-poll "since=N" invariant for any agent
///   that already saw a higher value);
/// - the counter survives full table rebuilds, restores, and migrations.
///
/// Sequence-style allocators (e.g. Postgres SEQUENCE) would hand out gaps
/// on rolled-back transactions; for our long-poll diff cursor, gap-free
/// monotonicity is much easier to reason about.
async fn next_generation(txn: &DatabaseTransaction) -> Result<i64, DnsRegistryError> {
    let row = txn
        .query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "UPDATE dns_generation SET current = current + 1, updated_at = now() \
             WHERE id = 1 RETURNING current"
                .to_string(),
        ))
        .await?
        .ok_or_else(|| DnsRegistryError::Validation {
            message: "dns_generation singleton row missing — migration not applied?".into(),
        })?;
    let g: i64 = row
        .try_get("", "current")
        .map_err(DnsRegistryError::Database)?;
    Ok(g)
}

async fn current_generation<C: ConnectionTrait>(db: &C) -> Result<i64, DnsRegistryError> {
    let row = db
        .query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT current FROM dns_generation WHERE id = 1".to_string(),
        ))
        .await?
        .ok_or_else(|| DnsRegistryError::Validation {
            message: "dns_generation singleton row missing — migration not applied?".into(),
        })?;
    let g: i64 = row
        .try_get("", "current")
        .map_err(DnsRegistryError::Database)?;
    Ok(g)
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}

// Compile-time check that the `DBDateTime` import is exercised — keeps the
// `use` statement honest if we ever stop touching the column directly.
const _: fn() -> Option<DBDateTime> = || None;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_type_round_trip() {
        for s in ["A", "AAAA", "SRV", "CNAME"] {
            assert_eq!(RecordType::from_str(s).unwrap().as_str(), s);
        }
        assert!(RecordType::from_str("TXT").is_err());
    }

    #[test]
    fn owner_kind_round_trip() {
        for s in ["service_member", "service_role", "node", "static"] {
            assert_eq!(OwnerKind::from_str(s).unwrap().as_str(), s);
        }
        assert!(OwnerKind::from_str("hammer").is_err());
    }

    #[test]
    fn validates_a_record_requires_ipv4() {
        let bad = EndpointDraft {
            fqdn: "x.temps.local".into(),
            record_type: RecordType::A,
            target_ip: Some("fd00::1".into()),
            target_port: None,
            ttl: 30,
            owner_kind: OwnerKind::Static,
            owner_id: 1,
            node_id: None,
        };
        let err = bad.validate().unwrap_err();
        assert!(
            matches!(err, DnsRegistryError::Validation { .. }),
            "expected Validation error, got {err:?}"
        );

        let ok = EndpointDraft {
            target_ip: Some("172.20.5.10".into()),
            ..bad
        };
        ok.validate().unwrap();
    }

    #[test]
    fn validates_aaaa_record_requires_ipv6() {
        let bad = EndpointDraft {
            fqdn: "y.temps.local".into(),
            record_type: RecordType::Aaaa,
            target_ip: Some("172.20.5.10".into()),
            target_port: None,
            ttl: 30,
            owner_kind: OwnerKind::Static,
            owner_id: 1,
            node_id: None,
        };
        assert!(matches!(
            bad.validate().unwrap_err(),
            DnsRegistryError::Validation { .. }
        ));
    }

    #[test]
    fn validates_invalid_ip_literal() {
        let bad = EndpointDraft {
            fqdn: "z.temps.local".into(),
            record_type: RecordType::A,
            target_ip: Some("not.an.ip".into()),
            target_port: None,
            ttl: 30,
            owner_kind: OwnerKind::Static,
            owner_id: 1,
            node_id: None,
        };
        assert!(matches!(
            bad.validate().unwrap_err(),
            DnsRegistryError::InvalidIp { .. }
        ));
    }

    #[test]
    fn validates_ttl_range() {
        let mut d = EndpointDraft {
            fqdn: "t.temps.local".into(),
            record_type: RecordType::A,
            target_ip: Some("1.2.3.4".into()),
            target_port: None,
            ttl: 0,
            owner_kind: OwnerKind::Static,
            owner_id: 1,
            node_id: None,
        };
        assert!(matches!(
            d.validate().unwrap_err(),
            DnsRegistryError::Validation { .. }
        ));
        d.ttl = 100_000;
        assert!(matches!(
            d.validate().unwrap_err(),
            DnsRegistryError::Validation { .. }
        ));
        d.ttl = 30;
        d.validate().unwrap();
    }

    #[test]
    fn stale_resolver_lag_is_signed_difference() {
        let r = StaleResolver {
            node_id: 1,
            applied_generation: 5,
            server_generation: 12,
        };
        assert_eq!(r.lag(), 7);

        // If a resolver somehow ACK'd ahead of the server (only possible
        // mid-rollback), lag goes negative — surface it as-is rather than
        // clamp, so ops can see something weird is happening.
        let r2 = StaleResolver {
            node_id: 1,
            applied_generation: 10,
            server_generation: 8,
        };
        assert_eq!(r2.lag(), -2);
    }

    #[test]
    fn validates_empty_fqdn() {
        let bad = EndpointDraft {
            fqdn: "".into(),
            record_type: RecordType::A,
            target_ip: Some("1.2.3.4".into()),
            target_port: None,
            ttl: 30,
            owner_kind: OwnerKind::Static,
            owner_id: 1,
            node_id: None,
        };
        assert!(matches!(
            bad.validate().unwrap_err(),
            DnsRegistryError::Validation { .. }
        ));
    }

    #[tokio::test]
    async fn test_replace_endpoints_idempotent_upsert() {
        // A re-deploy reuses a node's deterministic container IP, so a new
        // deployment (a different owner_id) re-publishes the same
        // (fqdn, record_type, target_ip) tuple that is still owned by the
        // previous deployment. delete-by-owner clears only the new owner's rows
        // (none yet), so the insert collides with the old row on
        // service_endpoints_uniq. Before the ON CONFLICT upsert this aborted the
        // whole publish and stalled DNS on dead container IPs. Verify the publish
        // now succeeds and the newest owner/generation wins.
        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(_) => {
                println!("Docker/DB not available, skipping test");
                return;
            }
        };
        let db = test_db.connection_arc();
        let registry = DnsRegistry::new(db.clone());

        let draft = |owner_id: i64| EndpointDraft {
            fqdn: "production.echo.temps.local".into(),
            record_type: RecordType::A,
            target_ip: Some("172.20.0.5".into()),
            target_port: Some(80),
            ttl: 10,
            owner_kind: OwnerKind::Deployment,
            owner_id,
            // node_id left NULL to avoid the FK to `nodes` (the conflict under
            // test is on (fqdn, record_type, target_ip), independent of node_id).
            node_id: None,
        };

        // Deployment 100 publishes its endpoint.
        let g1 = registry
            .replace_endpoints_for_owner(OwnerKind::Deployment, 100, &[draft(100)])
            .await
            .expect("first publish should succeed");

        // Deployment 200 re-deploys onto the same node, reusing the same IP.
        let g2 = registry
            .replace_endpoints_for_owner(OwnerKind::Deployment, 200, &[draft(200)])
            .await
            .expect("re-deploy reusing the IP must upsert, not violate the unique index");

        assert!(g2 > g1, "each publish bumps the generation");

        // Exactly one row for the tuple, now owned by deployment 200 at gen g2.
        let rows = service_endpoints::Entity::find()
            .filter(service_endpoints::Column::Fqdn.eq("production.echo.temps.local"))
            .all(db.as_ref())
            .await
            .expect("query endpoints");
        assert_eq!(rows.len(), 1, "upsert keeps one row per (fqdn, type, ip)");
        assert_eq!(rows[0].owner_id, 200, "newest owner wins");
        assert_eq!(rows[0].generation, g2, "row carries the newest generation");
    }
}
