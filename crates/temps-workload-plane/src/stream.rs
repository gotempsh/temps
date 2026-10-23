// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The server side of the per-node desired-state stream (ADR-055 §7).
//!
//! A read takes the visibility watermark `W =
//! pg_snapshot_xmin(pg_current_snapshot())`: every transaction with a lower
//! id has finished. It returns the markers of `node_state_changes` with
//! `since <= xid < W` for the node (and those for every node), resolved
//! level-triggered: each marked object's *current* state is read and sent
//! as `upsert` when it still exists and is in the node's view, `remove`
//! otherwise. A transaction still in flight has an id of at least `W`, so
//! its markers come with a later read, whatever order transactions commit
//! in.
//!
//! The answer is the node's whole view when `since` is 0, below the
//! retention horizon, or above the database's `xmax` (a restored
//! database), and when one transaction marked more objects than a delta
//! carries.
//!
//! Cost: one indexed range scan on `(node_id, xid)` per read, then one
//! query per object kind for the marked ids. Never a scan of the object
//! tables, except for a full snapshot, which reads the node's own
//! instances and the DNS claims.
//!
//! Secret values never appear here: workload specs hold references and
//! imports, and instance templates hold no environment.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, EntityTrait, QueryFilter, QueryOrder, Statement,
};
use temps_entities::{workload_instances, workloads};
use temps_workload::stream::{
    DnsClaimObject, FullReason, InstanceObject, ObjectKind, StateAck, StateChange, StateObject,
    StateResponse, WorkloadObject,
};
use temps_workload::{InstanceHandle, WorkloadSpec};

use crate::error::PlaneError;
use crate::plane::WorkloadPlane;

/// Most markers one delta answer carries. A window with more is cut at a
/// transaction boundary and the node asks again at once (`more`).
pub const MAX_MARKERS_PER_READ: u64 = 1_000;
/// Watermark lag above which a read logs at `warn`, with the oldest
/// in-flight transaction's age.
pub const LAG_WARN_SECS: f64 = 30.0;
/// Markers are never kept longer than this, whatever a node acked.
pub const MAX_MARKER_AGE_HOURS: i64 = 24;
/// Most markers one sweep statement deletes.
pub const SWEEP_BATCH: u64 = 10_000;

/// The stream's gauges and counters, read by the status endpoint.
#[derive(Debug, Default)]
pub struct StreamMetrics {
    /// The watermark's lag behind the oldest marker it holds back, in
    /// milliseconds; 0 when nothing is held back.
    pub watermark_lag_ms: AtomicU64,
    pub reads: AtomicU64,
    pub full_snapshots: AtomicU64,
    pub markers_swept: AtomicU64,
    /// Unix seconds of the last lag warning, to log it at most every 30 s.
    pub(crate) last_lag_warning: AtomicI64,
}

/// A copy of [`StreamMetrics`] at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamMetricsSnapshot {
    pub watermark_lag_ms: u64,
    pub reads: u64,
    pub full_snapshots: u64,
    pub markers_swept: u64,
}

impl StreamMetrics {
    pub fn snapshot(&self) -> StreamMetricsSnapshot {
        StreamMetricsSnapshot {
            watermark_lag_ms: self.watermark_lag_ms.load(Ordering::Relaxed),
            reads: self.reads.load(Ordering::Relaxed),
            full_snapshots: self.full_snapshots.load(Ordering::Relaxed),
            markers_swept: self.markers_swept.load(Ordering::Relaxed),
        }
    }
}

/// What one retention sweep did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepOutcome {
    pub deleted: u64,
    /// The new horizon (decimal `xid8`).
    pub horizon: String,
}

/// Parse a cursor. `None` and `""` are 0.
pub fn parse_cursor(since: Option<&str>) -> Result<u64, String> {
    match since.map(str::trim) {
        None | Some("") => Ok(0),
        Some(text) => text
            .parse::<u64>()
            .map_err(|_| format!("cursor '{text}' is not a transaction id watermark")),
    }
}

struct Window {
    watermark: u64,
    xmax: u64,
    horizon: u64,
}

fn text_u64(row: &sea_orm::QueryResult, column: &str) -> Result<u64, PlaneError> {
    let text: String = row
        .try_get("", column)
        .map_err(PlaneError::db("reading the stream window", "*"))?;
    text.parse::<u64>().map_err(|e| PlaneError::Database {
        operation: "parsing the stream window",
        workload: "*".to_string(),
        source: sea_orm::DbErr::Custom(format!("{column} '{text}': {e}")),
    })
}

fn node_filter(node_id: i32) -> sea_orm::Condition {
    // Node 0, the control plane, is `NULL` in `workload_instances`.
    if node_id == 0 {
        sea_orm::Condition::all().add(workload_instances::Column::NodeId.is_null())
    } else {
        sea_orm::Condition::all().add(workload_instances::Column::NodeId.eq(node_id))
    }
}

impl WorkloadPlane {
    /// The stream's metrics.
    pub fn stream_metrics(&self) -> &StreamMetrics {
        &self.stream_metrics
    }

    async fn stream_window(&self) -> Result<Window, PlaneError> {
        let row = self
            .db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT pg_snapshot_xmin(pg_current_snapshot())::text AS watermark, \
                        pg_snapshot_xmax(pg_current_snapshot())::text AS xmax, \
                        COALESCE((SELECT horizon::text FROM node_state_retention WHERE id = 1), '0') \
                            AS horizon"
                    .to_string(),
            ))
            .await
            .map_err(PlaneError::db("reading the stream window", "*"))?
            .ok_or_else(|| PlaneError::Database {
                operation: "reading the stream window",
                workload: "*".to_string(),
                source: sea_orm::DbErr::RecordNotFound("no window row".to_string()),
            })?;
        Ok(Window {
            watermark: text_u64(&row, "watermark")?,
            xmax: text_u64(&row, "xmax")?,
            horizon: text_u64(&row, "horizon")?,
        })
    }

    /// One read of node `node_id`'s stream from cursor `since` (0 for the
    /// whole view).
    pub async fn read_node_state(
        &self,
        node_id: i32,
        since: u64,
    ) -> Result<StateResponse, PlaneError> {
        self.stream_metrics.reads.fetch_add(1, Ordering::Relaxed);
        let window = self.stream_window().await?;
        self.observe_lag(window.watermark).await;
        let full = if since == 0 {
            Some(FullReason::Initial)
        } else if since < window.horizon {
            Some(FullReason::BelowHorizon)
        } else if since > window.xmax {
            Some(FullReason::AheadOfDatabase)
        } else {
            None
        };
        if let Some(reason) = full {
            return self.full_state(node_id, window.watermark, reason).await;
        }
        if since >= window.watermark {
            return Ok(StateResponse {
                node_id,
                watermark: since.to_string(),
                full: None,
                changes: Vec::new(),
                more: false,
            });
        }
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT object_kind, object_id, xid::text AS xid FROM ( \
                     SELECT object_kind, object_id, xid FROM node_state_changes \
                     WHERE node_id = $1 AND xid >= $2::text::xid8 AND xid < $3::text::xid8 \
                     UNION ALL \
                     SELECT object_kind, object_id, xid FROM node_state_changes \
                     WHERE node_id IS NULL AND xid >= $2::text::xid8 AND xid < $3::text::xid8 \
                 ) marked ORDER BY xid LIMIT $4",
                vec![
                    node_id.into(),
                    since.to_string().into(),
                    window.watermark.to_string().into(),
                    i64::try_from(MAX_MARKERS_PER_READ + 1)
                        .unwrap_or(i64::MAX)
                        .into(),
                ],
            ))
            .await
            .map_err(PlaneError::db(
                "reading the node's markers",
                format!("node {node_id}"),
            ))?;
        let mut markers: Vec<(String, i64, u64)> = Vec::with_capacity(rows.len());
        for row in &rows {
            let kind: String = row.try_get("", "object_kind").map_err(PlaneError::db(
                "reading a marker",
                format!("node {node_id}"),
            ))?;
            let id: i64 = row.try_get("", "object_id").map_err(PlaneError::db(
                "reading a marker",
                format!("node {node_id}"),
            ))?;
            markers.push((kind, id, text_u64(row, "xid")?));
        }
        let mut watermark = window.watermark;
        let mut more = false;
        if markers.len() as u64 > MAX_MARKERS_PER_READ {
            // Cut at a transaction boundary: everything below the first
            // transaction that does not fit is delivered whole.
            let cut = markers[MAX_MARKERS_PER_READ as usize].2;
            markers.retain(|(_, _, xid)| *xid < cut);
            if markers.is_empty() {
                return self
                    .full_state(node_id, window.watermark, FullReason::Oversized)
                    .await;
            }
            watermark = cut;
            more = true;
        }
        let mut marked: BTreeMap<ObjectKind, BTreeSet<i64>> = BTreeMap::new();
        for (kind, id, _) in markers {
            match ObjectKind::parse(&kind) {
                Some(kind) => {
                    marked.entry(kind).or_default().insert(id);
                }
                None => tracing::debug!(
                    node_id,
                    kind = %kind,
                    "marker of a kind this control plane does not stream; skipped"
                ),
            }
        }
        let changes = self.resolve(node_id, marked).await?;
        Ok(StateResponse {
            node_id,
            watermark: watermark.to_string(),
            full: None,
            changes,
            more,
        })
    }

    /// Resolve marked objects to their current state for `node_id`.
    async fn resolve(
        &self,
        node_id: i32,
        marked: BTreeMap<ObjectKind, BTreeSet<i64>>,
    ) -> Result<Vec<StateChange>, PlaneError> {
        let mut changes = Vec::new();
        let mut sent_workloads: BTreeSet<i64> = BTreeSet::new();
        if let Some(ids) = marked.get(&ObjectKind::Instance) {
            let rows = workload_instances::Entity::find()
                .filter(workload_instances::Column::Id.is_in(ids.iter().copied()))
                .find_also_related(workloads::Entity)
                .all(self.db.as_ref())
                .await
                .map_err(PlaneError::db(
                    "reading marked instances",
                    format!("node {node_id}"),
                ))?;
            let mut found = BTreeSet::new();
            for (instance, workload) in rows {
                let Some(workload) = workload else { continue };
                if instance.node_id.unwrap_or(0) != node_id {
                    continue;
                }
                found.insert(instance.id);
                if sent_workloads.insert(workload.id) {
                    changes.push(StateChange::Upsert {
                        object: StateObject::Workload(workload_object(&workload)?),
                    });
                }
                changes.push(StateChange::Upsert {
                    object: StateObject::Instance(self.instance_object(&instance, &workload)?),
                });
            }
            for id in ids.iter().filter(|id| !found.contains(id)) {
                changes.push(StateChange::Remove {
                    kind: ObjectKind::Instance,
                    id: *id,
                });
            }
        }
        if let Some(ids) = marked.get(&ObjectKind::Workload) {
            let pending: Vec<i64> = ids
                .iter()
                .copied()
                .filter(|id| !sent_workloads.contains(id))
                .collect();
            if !pending.is_empty() {
                // In view: the workload has an instance on this node.
                let hosted: BTreeSet<i64> = workload_instances::Entity::find()
                    .filter(node_filter(node_id))
                    .filter(workload_instances::Column::WorkloadId.is_in(pending.clone()))
                    .all(self.db.as_ref())
                    .await
                    .map_err(PlaneError::db(
                        "reading the node's instances of marked workloads",
                        format!("node {node_id}"),
                    ))?
                    .into_iter()
                    .map(|i| i.workload_id)
                    .collect();
                let rows = if hosted.is_empty() {
                    Vec::new()
                } else {
                    workloads::Entity::find()
                        .filter(workloads::Column::Id.is_in(hosted.iter().copied()))
                        .all(self.db.as_ref())
                        .await
                        .map_err(PlaneError::db(
                            "reading marked workloads",
                            format!("node {node_id}"),
                        ))?
                };
                let mut found = BTreeSet::new();
                for row in rows {
                    found.insert(row.id);
                    changes.push(StateChange::Upsert {
                        object: StateObject::Workload(workload_object(&row)?),
                    });
                }
                for id in pending.iter().filter(|id| !found.contains(id)) {
                    changes.push(StateChange::Remove {
                        kind: ObjectKind::Workload,
                        id: *id,
                    });
                }
            }
        }
        if let Some(ids) = marked.get(&ObjectKind::DnsClaim) {
            let claims = self.dns_claims(Some(ids.iter().copied().collect())).await?;
            let found: BTreeSet<i64> = claims.iter().map(|c| c.id).collect();
            changes.extend(claims.into_iter().map(|claim| StateChange::Upsert {
                object: StateObject::DnsClaim(claim),
            }));
            for id in ids.iter().filter(|id| !found.contains(id)) {
                changes.push(StateChange::Remove {
                    kind: ObjectKind::DnsClaim,
                    id: *id,
                });
            }
        }
        Ok(changes)
    }

    /// The node's whole view at `watermark`.
    async fn full_state(
        &self,
        node_id: i32,
        watermark: u64,
        reason: FullReason,
    ) -> Result<StateResponse, PlaneError> {
        self.stream_metrics
            .full_snapshots
            .fetch_add(1, Ordering::Relaxed);
        tracing::info!(node_id, ?reason, watermark, "sending a full state snapshot");
        let rows = workload_instances::Entity::find()
            .filter(node_filter(node_id))
            .order_by_asc(workload_instances::Column::Id)
            .find_also_related(workloads::Entity)
            .all(self.db.as_ref())
            .await
            .map_err(PlaneError::db(
                "reading the node's instances",
                format!("node {node_id}"),
            ))?;
        let mut changes = Vec::with_capacity(rows.len() * 2);
        let mut sent_workloads = BTreeSet::new();
        for (instance, workload) in rows {
            let Some(workload) = workload else { continue };
            if sent_workloads.insert(workload.id) {
                changes.push(StateChange::Upsert {
                    object: StateObject::Workload(workload_object(&workload)?),
                });
            }
            changes.push(StateChange::Upsert {
                object: StateObject::Instance(self.instance_object(&instance, &workload)?),
            });
        }
        changes.extend(
            self.dns_claims(None)
                .await?
                .into_iter()
                .map(|claim| StateChange::Upsert {
                    object: StateObject::DnsClaim(claim),
                }),
        );
        Ok(StateResponse {
            node_id,
            watermark: watermark.to_string(),
            full: Some(reason),
            changes,
            more: false,
        })
    }

    /// DNS claims by id (`hashtextextended(fqdn, 0)`), or all of them.
    async fn dns_claims(&self, ids: Option<Vec<i64>>) -> Result<Vec<DnsClaimObject>, PlaneError> {
        let select = "SELECT hashtextextended(fqdn, 0) AS id, fqdn, owner_kind, owner_id, \
                      target_workload_id FROM dns_name_claims";
        let statement = match ids {
            Some(ids) => Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!("{select} WHERE hashtextextended(fqdn, 0) = ANY($1) ORDER BY fqdn"),
                vec![ids.into()],
            ),
            None => {
                Statement::from_string(DatabaseBackend::Postgres, format!("{select} ORDER BY fqdn"))
            }
        };
        let rows = self
            .db
            .query_all(statement)
            .await
            .map_err(PlaneError::db("reading DNS name claims", "*"))?;
        rows.iter()
            .map(|row| {
                let get = PlaneError::db("reading a DNS name claim", "*");
                let claim = (|| -> Result<DnsClaimObject, sea_orm::DbErr> {
                    Ok(DnsClaimObject {
                        id: row.try_get("", "id")?,
                        fqdn: row.try_get("", "fqdn")?,
                        owner_kind: row.try_get("", "owner_kind")?,
                        owner_id: row.try_get("", "owner_id")?,
                        target_workload_id: row.try_get("", "target_workload_id")?,
                    })
                })();
                claim.map_err(get)
            })
            .collect()
    }

    pub(crate) fn instance_object(
        &self,
        instance: &workload_instances::Model,
        workload: &workloads::Model,
    ) -> Result<InstanceObject, PlaneError> {
        let spec: WorkloadSpec =
            serde_json::from_value(workload.spec.clone()).map_err(|e| PlaneError::Corrupt {
                workload_id: workload.id,
                field: "spec",
                reason: e.to_string(),
            })?;
        let handle: InstanceHandle =
            serde_json::from_value(instance.handle.clone()).map_err(|e| PlaneError::Corrupt {
                workload_id: workload.id,
                field: "instance handle",
                reason: format!("instance {}: {e}", instance.id),
            })?;
        let template = self.instance_template(
            &spec,
            workload.id,
            instance.revision,
            u32::try_from(instance.attempt).unwrap_or(0),
            &handle.name,
        );
        Ok(InstanceObject {
            id: instance.id,
            workload_id: instance.workload_id,
            attempt: instance.attempt,
            revision: instance.revision,
            node_id: instance.node_id.unwrap_or(0),
            phase: instance.phase.clone(),
            handle,
            template,
        })
    }

    /// Record what node `node_id` applied, per kind. A cursor never moves
    /// back.
    pub async fn ack_node_state(&self, node_id: i32, ack: &StateAck) -> Result<(), PlaneError> {
        for (kind, cursor) in &ack.acks {
            let cursor = parse_cursor(Some(cursor)).map_err(|reason| PlaneError::Database {
                operation: "recording a stream ack",
                workload: format!("node {node_id}"),
                source: sea_orm::DbErr::Custom(reason),
            })?;
            self.db
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "INSERT INTO node_state_acks (node_id, object_kind, acked_xid, updated_at) \
                     VALUES ($1, $2, $3::text::xid8, now()) \
                     ON CONFLICT (node_id, object_kind) DO UPDATE \
                     SET acked_xid = GREATEST(node_state_acks.acked_xid, EXCLUDED.acked_xid), \
                         updated_at = now()",
                    vec![
                        node_id.into(),
                        kind.as_str().into(),
                        cursor.to_string().into(),
                    ],
                ))
                .await
                .map_err(PlaneError::db(
                    "recording a stream ack",
                    format!("node {node_id}"),
                ))?;
        }
        Ok(())
    }

    /// Delete markers every active node has acked, and every marker older
    /// than 24 h, at most [`SWEEP_BATCH`] per statement. The horizon moves
    /// to the first id kept: a node behind it gets a full snapshot.
    /// `active_nodes` are the node ids whose acks hold markers back; a node
    /// with no ack yet holds nothing back (it starts from a full snapshot).
    pub async fn sweep_node_state(
        &self,
        active_nodes: &[i32],
        now: DateTime<Utc>,
    ) -> Result<SweepOutcome, PlaneError> {
        let cutoff = now - chrono::Duration::hours(MAX_MARKER_AGE_HOURS);
        let min_ack = if active_nodes.is_empty() {
            None
        } else {
            self.db
                .query_one(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "SELECT min(acked_xid)::text AS cursor FROM node_state_acks \
                     WHERE node_id = ANY($1)",
                    vec![active_nodes.to_vec().into()],
                ))
                .await
                .map_err(PlaneError::db("reading the lowest ack", "*"))?
                .and_then(|row| row.try_get::<Option<String>>("", "cursor").ok().flatten())
                .and_then(|text| text.parse::<u64>().ok())
        };
        // Without any active node's ack only age deletes.
        let min_ack = min_ack.unwrap_or(0);
        let mut deleted = 0u64;
        loop {
            let row = self
                .db
                .query_one(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "WITH doomed AS ( \
                         SELECT id, xid FROM node_state_changes \
                         WHERE xid < $1::text::xid8 OR created_at < $2 \
                         ORDER BY id LIMIT $3 \
                     ), gone AS ( \
                         DELETE FROM node_state_changes c USING doomed \
                         WHERE c.id = doomed.id RETURNING c.xid \
                     ) \
                     SELECT count(*) AS deleted, max(xid)::text AS top FROM gone",
                    vec![
                        min_ack.to_string().into(),
                        cutoff.into(),
                        i64::try_from(SWEEP_BATCH).unwrap_or(i64::MAX).into(),
                    ],
                ))
                .await
                .map_err(PlaneError::db("sweeping stream markers", "*"))?;
            let Some(row) = row else { break };
            let count: i64 = row
                .try_get("", "deleted")
                .map_err(PlaneError::db("sweeping stream markers", "*"))?;
            let top: Option<String> = row
                .try_get("", "top")
                .map_err(PlaneError::db("sweeping stream markers", "*"))?;
            let count = u64::try_from(count).unwrap_or(0);
            deleted += count;
            // The horizon covers everything deleted: the lowest ack, and
            // one past the newest marker removed by age.
            let top_plus_one = top
                .and_then(|t| t.parse::<u64>().ok())
                .map(|t| t.saturating_add(1))
                .unwrap_or(0);
            let horizon = min_ack.max(top_plus_one);
            self.db
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "UPDATE node_state_retention \
                     SET horizon = GREATEST(horizon, $1::text::xid8), swept_at = now() \
                     WHERE id = 1",
                    vec![horizon.to_string().into()],
                ))
                .await
                .map_err(PlaneError::db("moving the retention horizon", "*"))?;
            if count < SWEEP_BATCH {
                break;
            }
        }
        self.stream_metrics
            .markers_swept
            .fetch_add(deleted, Ordering::Relaxed);
        let horizon = self.stream_window().await?.horizon;
        if deleted > 0 {
            tracing::debug!(deleted, horizon, "swept stream markers");
        }
        Ok(SweepOutcome {
            deleted,
            horizon: horizon.to_string(),
        })
    }

    /// Update the lag gauge; warn when it is above 30 s, at most every 30 s.
    async fn observe_lag(&self, watermark: u64) {
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT COALESCE(EXTRACT(EPOCH FROM now() - min(created_at)), 0)::float8 AS lag \
                 FROM node_state_changes WHERE xid >= $1::text::xid8",
                vec![watermark.to_string().into()],
            ))
            .await;
        let lag = match row {
            Ok(Some(row)) => row.try_get::<f64>("", "lag").unwrap_or(0.0),
            Ok(None) => 0.0,
            Err(error) => {
                tracing::debug!("reading the watermark lag failed: {error}");
                return;
            }
        };
        let lag = lag.max(0.0);
        self.stream_metrics
            .watermark_lag_ms
            .store((lag * 1000.0) as u64, Ordering::Relaxed);
        if lag <= LAG_WARN_SECS {
            return;
        }
        let now = Utc::now().timestamp();
        let last = self.stream_metrics.last_lag_warning.load(Ordering::Relaxed);
        if now - last < LAG_WARN_SECS as i64 {
            return;
        }
        self.stream_metrics
            .last_lag_warning
            .store(now, Ordering::Relaxed);
        let oldest = self
            .db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT COALESCE(EXTRACT(EPOCH FROM now() - min(xact_start)), 0)::float8 AS age \
                 FROM pg_stat_activity WHERE backend_xid IS NOT NULL"
                    .to_string(),
            ))
            .await
            .ok()
            .flatten()
            .and_then(|row| row.try_get::<f64>("", "age").ok())
            .unwrap_or(0.0);
        tracing::warn!(
            watermark,
            lag_secs = lag,
            oldest_transaction_secs = oldest,
            "the node state stream's watermark lags {lag:.0}s behind its oldest held-back marker; \
             the oldest in-flight transaction is {oldest:.0}s old"
        );
    }
}

/// A workload as the stream sends it. `env.vars` is the non-secret part of
/// a spec by contract; a variable whose name still classifies as secret is
/// sent without its value all the same, so a product that put one there by
/// mistake does not leak it into every node's snapshot. A node never needs
/// it: the create call's environment comes from `instance_env`.
fn workload_object(row: &workloads::Model) -> Result<WorkloadObject, PlaneError> {
    let mut spec: WorkloadSpec =
        serde_json::from_value(row.spec.clone()).map_err(|e| PlaneError::Corrupt {
            workload_id: row.id,
            field: "spec",
            reason: e.to_string(),
        })?;
    for (name, value) in spec.env.vars.iter_mut() {
        if temps_workload_env::is_secret_variable(name) {
            value.clear();
        }
    }
    Ok(WorkloadObject {
        id: row.id,
        name: row.name.clone(),
        generation: row.generation,
        phase: row.phase.clone(),
        deleted: row.deleted_at.is_some(),
        spec,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{job_spec, plane_with, test_db};
    use sea_orm::TransactionTrait;
    use std::time::Duration;

    async fn seed<C: ConnectionTrait>(conn: &C, node_id: Option<i32>, name: &str) -> (i64, i64) {
        let spec = serde_json::to_value(job_spec("registry.example.test/app:1")).expect("spec");
        let workload = conn
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO workloads (owner_kind, owner_id, name, spec) \
                 VALUES ('cron', 7, $1, $2) RETURNING id",
                vec![name.into(), spec.into()],
            ))
            .await
            .expect("workload inserted")
            .expect("a row")
            .try_get::<i64>("", "id")
            .expect("an id");
        let handle = serde_json::json!({
            "driver": "fake",
            "id": format!("t-cron-{workload}-a1"),
            "name": format!("t-cron-{workload}-a1"),
        });
        let instance = conn
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO workload_instances (workload_id, revision, node_id, driver, handle) \
                 VALUES ($1, 1, $2, 'fake', $3) RETURNING id",
                vec![workload.into(), node_id.into(), handle.into()],
            ))
            .await
            .expect("instance inserted")
            .expect("a row")
            .try_get::<i64>("", "id")
            .expect("an id");
        (workload, instance)
    }

    async fn exec<C: ConnectionTrait>(conn: &C, sql: &str, values: Vec<sea_orm::Value>) {
        conn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            values,
        ))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }

    async fn count<C: ConnectionTrait>(conn: &C, sql: &str, values: Vec<sea_orm::Value>) -> i64 {
        conn.query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            values,
        ))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
        .expect("a row")
        .try_get::<i64>("", "n")
        .expect("a count")
    }

    /// A cursor every marker written from now on is at or above.
    async fn cursor(plane: &WorkloadPlane, node_id: i32) -> u64 {
        let answer = plane.read_node_state(node_id, 0).await.expect("read");
        answer.watermark.parse().expect("a cursor")
    }

    fn has_upsert(changes: &[StateChange], kind: ObjectKind, id: i64) -> bool {
        changes.iter().any(|change| match change {
            StateChange::Upsert { object } => match (kind, object) {
                (ObjectKind::Workload, StateObject::Workload(w)) => w.id == id,
                (ObjectKind::Instance, StateObject::Instance(i)) => i.id == id,
                (ObjectKind::DnsClaim, StateObject::DnsClaim(c)) => c.id == id,
                _ => false,
            },
            StateChange::Remove { .. } => false,
        })
    }

    fn has_remove(changes: &[StateChange], kind: ObjectKind, id: i64) -> bool {
        changes.iter().any(|change| {
            matches!(change, StateChange::Remove { kind: k, id: i } if *k == kind && *i == id)
        })
    }

    /// Read deltas from `since` until `done` holds for everything received.
    /// Other tests share the database server, so a watermark can be held
    /// back for a moment by their transactions.
    async fn read_until(
        plane: &WorkloadPlane,
        node_id: i32,
        since: u64,
        done: impl Fn(&[StateChange]) -> bool,
    ) -> (Vec<StateChange>, u64) {
        let mut received = Vec::new();
        let mut cursor = since;
        for _ in 0..300 {
            let answer = plane.read_node_state(node_id, cursor).await.expect("read");
            assert_eq!(
                answer.full, None,
                "a delta read from {cursor} answered in full"
            );
            received.extend(answer.changes);
            cursor = answer.watermark.parse().expect("a cursor");
            if done(&received) {
                return (received, cursor);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("node {node_id}: the stream never delivered what was expected: {received:?}");
    }

    #[tokio::test]
    async fn markers_are_written_in_the_objects_transaction_and_a_rollback_leaves_none() {
        let Some(db) = test_db().await else { return };

        let txn = db.db.begin().await.expect("begin");
        let (_, instance) = seed(&txn, Some(4), "rolled-back").await;
        let in_txn = count(
            &txn,
            "SELECT count(*) AS n FROM node_state_changes \
             WHERE object_kind = 'instance' AND object_id = $1 AND node_id = 4 \
               AND xid = pg_current_xact_id()",
            vec![instance.into()],
        )
        .await;
        assert_eq!(in_txn, 1, "the marker carries the writing transaction's id");
        txn.rollback().await.expect("rollback");

        let after = count(
            db.db.as_ref(),
            "SELECT count(*) AS n FROM node_state_changes",
            vec![],
        )
        .await;
        assert_eq!(after, 0, "a rolled-back change leaves no marker");

        let (_, kept) = seed(db.db.as_ref(), Some(4), "committed").await;
        let committed = count(
            db.db.as_ref(),
            "SELECT count(*) AS n FROM node_state_changes WHERE object_id = $1",
            vec![kept.into()],
        )
        .await;
        assert_eq!(committed, 1);

        // A status-only update (what a node's report writes) adds none.
        exec(
            db.db.as_ref(),
            "UPDATE workload_instances SET ready = true WHERE id = $1",
            vec![kept.into()],
        )
        .await;
        let still = count(
            db.db.as_ref(),
            "SELECT count(*) AS n FROM node_state_changes WHERE object_id = $1",
            vec![kept.into()],
        )
        .await;
        assert_eq!(still, 1);
    }

    #[tokio::test]
    async fn a_deleted_instance_and_workload_resolve_to_remove() {
        let Some(db) = test_db().await else { return };
        let (plane, _, _) = plane_with(&db);
        let (workload, instance) = seed(db.db.as_ref(), Some(3), "tombstone").await;

        let full = plane.read_node_state(3, 0).await.expect("read");
        assert_eq!(full.full, Some(FullReason::Initial));
        assert!(has_upsert(&full.changes, ObjectKind::Workload, workload));
        assert!(has_upsert(&full.changes, ObjectKind::Instance, instance));
        let since: u64 = full.watermark.parse().expect("a cursor");

        exec(
            db.db.as_ref(),
            "DELETE FROM workloads WHERE id = $1",
            vec![workload.into()],
        )
        .await;
        let (changes, _) = read_until(&plane, 3, since, |c| {
            has_remove(c, ObjectKind::Instance, instance)
                && has_remove(c, ObjectKind::Workload, workload)
        })
        .await;
        assert!(!has_upsert(&changes, ObjectKind::Instance, instance));
    }

    #[tokio::test]
    async fn reassignment_removes_on_the_old_node_and_upserts_on_the_new() {
        let Some(db) = test_db().await else { return };
        let (plane, _, _) = plane_with(&db);
        let (workload, instance) = seed(db.db.as_ref(), Some(1), "moving").await;
        let on_a = cursor(&plane, 1).await;
        let on_b = cursor(&plane, 2).await;

        exec(
            db.db.as_ref(),
            "UPDATE workload_instances SET node_id = 2 WHERE id = $1",
            vec![instance.into()],
        )
        .await;

        let (a, _) = read_until(&plane, 1, on_a, |c| {
            has_remove(c, ObjectKind::Instance, instance)
        })
        .await;
        assert!(!has_upsert(&a, ObjectKind::Instance, instance));
        let (b, _) = read_until(&plane, 2, on_b, |c| {
            has_upsert(c, ObjectKind::Instance, instance)
        })
        .await;
        assert!(
            has_upsert(&b, ObjectKind::Workload, workload),
            "an instance upsert brings its workload"
        );
        assert!(!has_remove(&b, ObjectKind::Instance, instance));
    }

    #[tokio::test]
    async fn the_watermark_holds_back_a_later_commit_until_an_earlier_transaction_ends() {
        let Some(db) = test_db().await else { return };
        let (plane, _, _) = plane_with(&db);
        let start = cursor(&plane, 6).await;

        // Two connections: the first takes the lower transaction id and
        // commits last.
        let first = db.db.begin().await.expect("begin first");
        let (_, early) = seed(&first, Some(6), "early").await;
        let second = db.db.begin().await.expect("begin second");
        let (_, late) = seed(&second, Some(6), "late").await;
        second.commit().await.expect("commit second");

        let held = plane.read_node_state(6, start).await.expect("read");
        assert_eq!(held.full, None);
        assert!(
            !has_upsert(&held.changes, ObjectKind::Instance, late),
            "a commit above an in-flight transaction is not delivered past it"
        );
        assert!(!has_upsert(&held.changes, ObjectKind::Instance, early));
        let held_at: u64 = held.watermark.parse().expect("a cursor");

        first.commit().await.expect("commit first");
        let (changes, _) = read_until(&plane, 6, held_at, |c| {
            has_upsert(c, ObjectKind::Instance, early) && has_upsert(c, ObjectKind::Instance, late)
        })
        .await;
        assert!(has_upsert(&changes, ObjectKind::Instance, late));
    }

    #[tokio::test]
    async fn a_cursor_of_zero_gets_a_full_snapshot() {
        let Some(db) = test_db().await else { return };
        let (plane, _, _) = plane_with(&db);
        let (workload, instance) = seed(db.db.as_ref(), Some(8), "initial").await;
        let (_, elsewhere) = seed(db.db.as_ref(), Some(9), "elsewhere").await;
        let answer = plane.read_node_state(8, 0).await.expect("read");
        assert_eq!(answer.full, Some(FullReason::Initial));
        assert!(has_upsert(&answer.changes, ObjectKind::Workload, workload));
        assert!(has_upsert(&answer.changes, ObjectKind::Instance, instance));
        assert!(
            !has_upsert(&answer.changes, ObjectKind::Instance, elsewhere),
            "another node's instance is not in this node's view"
        );
    }

    #[tokio::test]
    async fn a_cursor_below_the_retention_horizon_gets_a_full_snapshot() {
        let Some(db) = test_db().await else { return };
        let (plane, _, _) = plane_with(&db);
        let stale = cursor(&plane, 8).await;
        let (_, instance) = seed(db.db.as_ref(), Some(8), "swept").await;
        // Node 8 acknowledges past the marker; the sweep deletes it and
        // moves the horizon above the stale cursor.
        let (_, acked) = read_until(&plane, 8, stale, |c| {
            has_upsert(c, ObjectKind::Instance, instance)
        })
        .await;
        let ack = StateAck {
            acks: ObjectKind::ALL
                .iter()
                .map(|kind| (*kind, acked.to_string()))
                .collect(),
        };
        plane.ack_node_state(8, &ack).await.expect("acked");
        let swept = plane
            .sweep_node_state(&[8], Utc::now())
            .await
            .expect("swept");
        assert!(swept.deleted >= 1, "the acknowledged marker is deleted");
        assert!(swept.horizon.parse::<u64>().expect("a horizon") > stale);

        let answer = plane.read_node_state(8, stale).await.expect("read");
        assert_eq!(answer.full, Some(FullReason::BelowHorizon));
        assert!(has_upsert(&answer.changes, ObjectKind::Instance, instance));
    }

    #[tokio::test]
    async fn a_cursor_above_the_database_xmax_gets_a_full_snapshot() {
        let Some(db) = test_db().await else { return };
        let (plane, _, _) = plane_with(&db);
        let (_, instance) = seed(db.db.as_ref(), Some(8), "restored").await;
        // A cursor from before a restore: ahead of every id this database
        // has handed out.
        let answer = plane.read_node_state(8, u64::MAX / 2).await.expect("read");
        assert_eq!(answer.full, Some(FullReason::AheadOfDatabase));
        assert!(has_upsert(&answer.changes, ObjectKind::Instance, instance));
        assert!(answer.watermark.parse::<u64>().expect("a cursor") < u64::MAX / 2);
    }

    #[tokio::test]
    async fn dns_claims_reach_every_node() {
        let Some(db) = test_db().await else { return };
        let (plane, _, _) = plane_with(&db);
        let since = cursor(&plane, 11).await;
        exec(
            db.db.as_ref(),
            "INSERT INTO dns_name_claims (fqdn, owner_kind, owner_id) \
             VALUES ('app.example.test', 'environment', 3)",
            vec![],
        )
        .await;
        let id = db
            .db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT hashtextextended('app.example.test', 0) AS id".to_string(),
            ))
            .await
            .expect("hashed")
            .expect("a row")
            .try_get::<i64>("", "id")
            .expect("an id");
        let (_, after) = read_until(&plane, 11, since, |c| {
            has_upsert(c, ObjectKind::DnsClaim, id)
        })
        .await;
        exec(
            db.db.as_ref(),
            "DELETE FROM dns_name_claims WHERE fqdn = 'app.example.test'",
            vec![],
        )
        .await;
        read_until(&plane, 11, after, |c| {
            has_remove(c, ObjectKind::DnsClaim, id)
        })
        .await;
    }

    #[tokio::test]
    async fn the_stream_never_carries_a_secret_classified_value() {
        let Some(db) = test_db().await else { return };
        let (plane, _, _) = plane_with(&db);
        let mut spec = job_spec("registry.example.test/app:1");
        spec.env
            .vars
            .insert("API_TOKEN".into(), "stream-seeded-secret-41c9".into());
        spec.env.vars.insert("MODE".into(), "batch".into());
        let spec = serde_json::to_value(spec).expect("spec");
        let workload = db
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO workloads (owner_kind, owner_id, name, spec) \
                 VALUES ('cron', 7, 'secretive', $1) RETURNING id",
                vec![spec.into()],
            ))
            .await
            .expect("workload inserted")
            .expect("a row")
            .try_get::<i64>("", "id")
            .expect("an id");
        exec(
            db.db.as_ref(),
            "INSERT INTO workload_instances (workload_id, revision, node_id, driver, handle) \
             VALUES ($1, 1, 14, 'node-stream', $2)",
            vec![
                workload.into(),
                serde_json::json!({"driver": "node-stream", "id": "t-cron-1-a1", "name": "t-cron-1-a1"})
                    .into(),
            ],
        )
        .await;
        let answer = plane.read_node_state(14, 0).await.expect("read");
        let text = serde_json::to_string(&answer).expect("encoded");
        assert!(!text.contains("stream-seeded-secret-41c9"), "{text}");
        assert!(
            text.contains("API_TOKEN") && text.contains("batch"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn acks_never_move_back_and_old_markers_are_swept_by_age() {
        let Some(db) = test_db().await else { return };
        let (plane, _, _) = plane_with(&db);
        let ack = |cursor: &str| StateAck {
            acks: [(ObjectKind::Instance, cursor.to_string())]
                .into_iter()
                .collect(),
        };
        plane.ack_node_state(5, &ack("900")).await.expect("acked");
        plane.ack_node_state(5, &ack("100")).await.expect("acked");
        let stored = db
            .db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT acked_xid::text AS c FROM node_state_acks \
                 WHERE node_id = 5 AND object_kind = 'instance'"
                    .to_string(),
            ))
            .await
            .expect("read")
            .expect("a row")
            .try_get::<String>("", "c")
            .expect("a cursor");
        assert_eq!(stored, "900");

        // A node that never acknowledges does not keep a marker past 24 h.
        seed(db.db.as_ref(), Some(12), "aged").await;
        exec(
            db.db.as_ref(),
            "UPDATE node_state_changes SET created_at = now() - interval '25 hours'",
            vec![],
        )
        .await;
        let swept = plane
            .sweep_node_state(&[], Utc::now())
            .await
            .expect("swept");
        assert_eq!(swept.deleted, 1);
        assert_ne!(swept.horizon, "0");
    }
}
