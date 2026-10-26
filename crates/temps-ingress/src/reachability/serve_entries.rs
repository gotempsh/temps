// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The control plane's half of ADR 065 D4's Serve entries: one
//! `tailnet_serve_entries` row per (worker, address) says what that worker's
//! tailscaled must serve, and runs reference it through `tailnet_serve_refs`
//! rather than owning it.
//!
//! Every change to an entry's references, and the last-reference check that
//! retires the entry, runs in one transaction that first takes a
//! transaction-scoped advisory lock on `(node_id, address_id)`, taken even
//! when no row exists yet, so a run adding a reference and an undo removing
//! the last one never interleave. An entry naming a Service takes that
//! Service's lock first, always in that order.
//!
//! The worker's agent applies the rows the route snapshot carries to it and
//! reports per entry (`temps_routes::public_ingress_store`). What happens to
//! an entry left with no reference depends on that report:
//!
//! | Last report | The row |
//! |---|---|
//! | `applied` | `removing`, with its `applied_etag`; deleted on `removed` |
//! | `conflict` (the agent wrote nothing) | deleted, the agent told nothing |
//! | `error`, or none | `removing` without an etag; deleted on `removed` or `absent` |

use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, Set, Statement, TransactionTrait,
};
use temps_core::tailnet::TailscaleReport;
use temps_entities::{node_public_ingress, nodes, tailnet_serve_entries, tailnet_serve_refs};
use thiserror::Error;

use super::run::{ServeClaim, ServeEntryStatus, ServeRelease, ServeWish, ServiceEntryHost};

#[derive(Debug, Error)]
pub enum ServeEntryError {
    #[error("Database error while {operation} the Serve entry for {address_id} on node {node_id}: {source}")]
    Database {
        operation: &'static str,
        node_id: i32,
        address_id: String,
        source: DbErr,
    },
    #[error("Database error while forgetting Serve entry {entry_id}: {source}")]
    Forget { entry_id: i32, source: DbErr },
}

/// Take the `(node, address)` lock for the rest of `txn`.
async fn lock_entry<C: ConnectionTrait>(
    conn: &C,
    node_id: i32,
    address_id: &str,
) -> Result<(), DbErr> {
    conn.execute(Statement::from_sql_and_values(
        sea_orm::DatabaseBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended('tailnet-serve:' || $1, 0))",
        [format!("{node_id}:{address_id}").into()],
    ))
    .await?;
    Ok(())
}

/// Take the `(tailnet, Service)` lock for the rest of `txn`. The Service
/// deletion of ADR 065 D4 takes the same one.
pub async fn lock_service<C: ConnectionTrait>(
    conn: &C,
    tailnet: &str,
    service: &str,
) -> Result<(), DbErr> {
    conn.execute(Statement::from_sql_and_values(
        sea_orm::DatabaseBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended('tailnet-service:' || $1, 0))",
        [format!("{tailnet}:{service}").into()],
    ))
    .await?;
    Ok(())
}

/// Tell agents the snapshot changed; delivered when `conn`'s transaction
/// commits.
async fn notify<C: ConnectionTrait>(conn: &C) -> Result<(), DbErr> {
    conn.execute_unprepared("SELECT pg_notify('route_table_changes', '')")
        .await?;
    Ok(())
}

/// What a row asks of its worker, in words.
fn describe(row: &tailnet_serve_entries::Model) -> String {
    match &row.service_name {
        Some(service) => format!("`{}` as Tailscale Service `{service}`", row.hostname),
        None => format!("`{}`, its machine name, with Tailscale Serve", row.hostname),
    }
}

fn same_wish(row: &tailnet_serve_entries::Model, wish: &ServeWish) -> bool {
    row.private_via == wish.target.via
        && row.service_name == wish.target.service
        && row.hostname.eq_ignore_ascii_case(&wish.hostname)
}

/// Create the entry with this run's `created` reference, or reference the
/// same one as `found` (ADR 065 D4). A row asking for something else, or
/// one being removed, is left exactly as it is.
pub async fn claim(
    db: &DatabaseConnection,
    run_id: i32,
    wish: &ServeWish,
) -> Result<ServeClaim, ServeEntryError> {
    let node_id = wish.target.node_id;
    let address_id = wish.address_id.clone();
    let wrap = |operation: &'static str| {
        let address_id = address_id.clone();
        move |source: DbErr| ServeEntryError::Database {
            operation,
            node_id,
            address_id,
            source,
        }
    };
    let txn = db.begin().await.map_err(wrap("claiming"))?;
    if let (Some(tailnet), Some(service)) = (&wish.target.tailnet, &wish.target.service) {
        lock_service(&txn, tailnet, service)
            .await
            .map_err(wrap("locking the Service of"))?;
    }
    lock_entry(&txn, node_id, &address_id)
        .await
        .map_err(wrap("locking"))?;
    let row = tailnet_serve_entries::Entity::find()
        .filter(tailnet_serve_entries::Column::NodeId.eq(node_id))
        .filter(tailnet_serve_entries::Column::AddressId.eq(address_id.as_str()))
        .one(&txn)
        .await
        .map_err(wrap("reading"))?;
    let now = chrono::Utc::now();
    let claim = match row {
        Some(row) if row.state == tailnet_serve_entries::STATE_REMOVING => ServeClaim::Removing,
        Some(row) if !same_wish(&row, wish) => ServeClaim::Differs(describe(&row)),
        Some(row) => {
            let existing = tailnet_serve_refs::Entity::find()
                .filter(tailnet_serve_refs::Column::EntryId.eq(row.id))
                .filter(tailnet_serve_refs::Column::RunId.eq(run_id))
                .one(&txn)
                .await
                .map_err(wrap("reading the references of"))?;
            match existing {
                // A retry of this run's own claim.
                Some(reference) if reference.kind == tailnet_serve_refs::KIND_CREATED => {
                    ServeClaim::Created { entry_id: row.id }
                }
                Some(_) => ServeClaim::Found { entry_id: row.id },
                None => {
                    tailnet_serve_refs::ActiveModel {
                        entry_id: Set(row.id),
                        run_id: Set(run_id),
                        kind: Set(tailnet_serve_refs::KIND_FOUND.to_string()),
                        created_at: Set(now),
                        ..Default::default()
                    }
                    .insert(&txn)
                    .await
                    .map_err(wrap("referencing"))?;
                    ServeClaim::Found { entry_id: row.id }
                }
            }
        }
        None => {
            let row = tailnet_serve_entries::ActiveModel {
                node_id: Set(node_id),
                node_name: Set(wish.target.node_name.clone()),
                address_id: Set(address_id.clone()),
                hostname: Set(wish.hostname.clone()),
                private_via: Set(wish.target.via.clone()),
                service_name: Set(wish.target.service.clone()),
                tailnet: Set(wish.target.tailnet.clone()),
                service_addrs: Set(
                    (!wish.service_addrs.is_empty()).then(|| serde_json::json!(wish.service_addrs))
                ),
                state: Set(tailnet_serve_entries::STATE_ACTIVE.to_string()),
                report: Set(None),
                report_detail: Set(None),
                applied_etag: Set(None),
                reported_at: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            }
            .insert(&txn)
            .await
            .map_err(wrap("creating"))?;
            tailnet_serve_refs::ActiveModel {
                entry_id: Set(row.id),
                run_id: Set(run_id),
                kind: Set(tailnet_serve_refs::KIND_CREATED.to_string()),
                created_at: Set(now),
                ..Default::default()
            }
            .insert(&txn)
            .await
            .map_err(wrap("referencing"))?;
            notify(&txn).await.map_err(wrap("announcing"))?;
            ServeClaim::Created { entry_id: row.id }
        }
    };
    txn.commit().await.map_err(wrap("claiming"))?;
    Ok(claim)
}

/// This run's reference to the `(node, address)` entry: `(entry id,
/// created)`.
pub async fn reference(
    db: &DatabaseConnection,
    run_id: i32,
    node_id: i32,
    address_id: &str,
) -> Result<Option<(i32, bool)>, ServeEntryError> {
    let wrap = |source| ServeEntryError::Database {
        operation: "reading this run's reference to",
        node_id,
        address_id: address_id.to_string(),
        source,
    };
    let Some(row) = tailnet_serve_entries::Entity::find()
        .filter(tailnet_serve_entries::Column::NodeId.eq(node_id))
        .filter(tailnet_serve_entries::Column::AddressId.eq(address_id))
        .one(db)
        .await
        .map_err(wrap)?
    else {
        return Ok(None);
    };
    Ok(tailnet_serve_refs::Entity::find()
        .filter(tailnet_serve_refs::Column::EntryId.eq(row.id))
        .filter(tailnet_serve_refs::Column::RunId.eq(run_id))
        .one(db)
        .await
        .map_err(wrap)?
        .map(|r| (row.id, r.kind == tailnet_serve_refs::KIND_CREATED)))
}

/// The entry and what its worker last said about it.
pub async fn status(
    db: &DatabaseConnection,
    node_id: i32,
    address_id: &str,
) -> Result<Option<ServeEntryStatus>, ServeEntryError> {
    let wrap = |source| ServeEntryError::Database {
        operation: "reading",
        node_id,
        address_id: address_id.to_string(),
        source,
    };
    let Some(row) = tailnet_serve_entries::Entity::find()
        .filter(tailnet_serve_entries::Column::NodeId.eq(node_id))
        .filter(tailnet_serve_entries::Column::AddressId.eq(address_id))
        .one(db)
        .await
        .map_err(wrap)?
    else {
        return Ok(None);
    };
    let node = nodes::Entity::find_by_id(node_id)
        .one(db)
        .await
        .map_err(wrap)?;
    let report: Option<TailscaleReport> = node_public_ingress::Entity::find_by_id(node_id)
        .one(db)
        .await
        .map_err(wrap)?
        .and_then(|r| r.tailscale)
        .and_then(|v| serde_json::from_value(v).ok());
    Ok(Some(status_of(&row, node.is_some(), report.as_ref())))
}

/// Every entry naming the Service `service`, on any node, with what
/// identifies its worker in the tailnet (ADR 065 D4): the only hosts the
/// Service may have. Read on `conn`, so a caller holding the Service's lock
/// sees what that lock guards.
pub async fn for_service<C: ConnectionTrait>(
    conn: &C,
    service: &str,
) -> Result<Vec<ServiceEntryHost>, DbErr> {
    let rows = tailnet_serve_entries::Entity::find()
        .filter(tailnet_serve_entries::Column::ServiceName.eq(service))
        .order_by_asc(tailnet_serve_entries::Column::Id)
        .all(conn)
        .await?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let reports: std::collections::HashMap<i32, TailscaleReport> =
        node_public_ingress::Entity::find()
            .filter(node_public_ingress::Column::NodeId.is_in(rows.iter().map(|r| r.node_id)))
            .all(conn)
            .await?
            .into_iter()
            .filter_map(|row| {
                let report = serde_json::from_value(row.tailscale?).ok()?;
                Some((row.node_id, report))
            })
            .collect();
    Ok(rows
        .into_iter()
        .map(|row| {
            let report = reports.get(&row.node_id);
            ServiceEntryHost {
                node_id: row.node_id,
                node_name: row.node_name,
                address_id: row.address_id,
                hostname: row.hostname,
                tailscale_node_id: report.and_then(|r| r.node_id.clone()),
                dns_name: report.and_then(|r| r.dns_name.clone()),
                removing: row.state == tailnet_serve_entries::STATE_REMOVING,
            }
        })
        .collect())
}

/// What the run reads from an entry and its worker's last report.
pub fn status_of(
    row: &tailnet_serve_entries::Model,
    node_exists: bool,
    report: Option<&TailscaleReport>,
) -> ServeEntryStatus {
    let worker_error = report.and_then(|r| (!r.serve_ready()).then(|| r.error.clone()).flatten());
    let funnel = report.is_some_and(|r| {
        r.entries
            .iter()
            .any(|e| e.entry_id == row.id && e.address_id == row.address_id && e.funnel)
    });
    ServeEntryStatus {
        entry_id: row.id,
        removing: row.state == tailnet_serve_entries::STATE_REMOVING,
        report: row.report.clone(),
        detail: row.report_detail.clone(),
        applied_etag: row.applied_etag.clone(),
        worker_error,
        worker_gone: !node_exists,
        funnel,
    }
}

/// What an entry left with no reference becomes (ADR 065 D4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LastReference {
    /// The agent wrote nothing: delete the row, tell the agent nothing.
    Delete,
    /// Have the agent take its handler off; keep the etag it applied.
    RemoveApplied,
    /// The agent may have written it: remove by ownership, no etag.
    RemoveUnknown,
}

pub fn last_reference(report: Option<&str>) -> LastReference {
    match report {
        Some(tailnet_serve_entries::REPORT_CONFLICT) => LastReference::Delete,
        Some(tailnet_serve_entries::REPORT_APPLIED) => LastReference::RemoveApplied,
        // `error`: the write may have landed before the error.
        _ => LastReference::RemoveUnknown,
    }
}

/// Drop this run's reference under the entry's lock; the last one retires
/// the entry by its last report.
pub async fn release(
    db: &DatabaseConnection,
    run_id: i32,
    node_id: i32,
    address_id: &str,
) -> Result<ServeRelease, ServeEntryError> {
    let wrap = |operation: &'static str| {
        move |source: DbErr| ServeEntryError::Database {
            operation,
            node_id,
            address_id: address_id.to_string(),
            source,
        }
    };
    let txn = db.begin().await.map_err(wrap("releasing"))?;
    let row = tailnet_serve_entries::Entity::find()
        .filter(tailnet_serve_entries::Column::NodeId.eq(node_id))
        .filter(tailnet_serve_entries::Column::AddressId.eq(address_id))
        .one(&txn)
        .await
        .map_err(wrap("reading"))?;
    if let Some((tailnet, service)) = row
        .as_ref()
        .and_then(|r| Some((r.tailnet.clone()?, r.service_name.clone()?)))
    {
        lock_service(&txn, &tailnet, &service)
            .await
            .map_err(wrap("locking the Service of"))?;
    }
    lock_entry(&txn, node_id, address_id)
        .await
        .map_err(wrap("locking"))?;
    // Read again under the lock: what was read before it may be stale.
    let Some(row) = tailnet_serve_entries::Entity::find()
        .filter(tailnet_serve_entries::Column::NodeId.eq(node_id))
        .filter(tailnet_serve_entries::Column::AddressId.eq(address_id))
        .one(&txn)
        .await
        .map_err(wrap("reading"))?
    else {
        txn.commit().await.map_err(wrap("releasing"))?;
        return Ok(ServeRelease::Gone);
    };
    let dropped = tailnet_serve_refs::Entity::delete_many()
        .filter(tailnet_serve_refs::Column::EntryId.eq(row.id))
        .filter(tailnet_serve_refs::Column::RunId.eq(run_id))
        .exec(&txn)
        .await
        .map_err(wrap("dropping this run's reference to"))?;
    if dropped.rows_affected == 0 {
        txn.commit().await.map_err(wrap("releasing"))?;
        return Ok(ServeRelease::Gone);
    }
    let others = tailnet_serve_refs::Entity::find()
        .filter(tailnet_serve_refs::Column::EntryId.eq(row.id))
        .order_by_asc(tailnet_serve_refs::Column::CreatedAt)
        .all(&txn)
        .await
        .map_err(wrap("reading the references of"))?;
    let outcome = if let Some(oldest) = others.first() {
        ServeRelease::Kept(format!(
            "{} still serves it for another setup (set up on {})",
            row.node_name,
            oldest.created_at.format("%-d %B")
        ))
    } else {
        match last_reference(row.report.as_deref()) {
            LastReference::Delete => {
                tailnet_serve_entries::Entity::delete_by_id(row.id)
                    .exec(&txn)
                    .await
                    .map_err(wrap("deleting"))?;
                ServeRelease::Deleted
            }
            how @ (LastReference::RemoveApplied | LastReference::RemoveUnknown) => {
                let mut update = tailnet_serve_entries::Entity::update_many()
                    .col_expr(
                        tailnet_serve_entries::Column::State,
                        Expr::value(tailnet_serve_entries::STATE_REMOVING),
                    )
                    .col_expr(
                        tailnet_serve_entries::Column::UpdatedAt,
                        Expr::value(chrono::Utc::now()),
                    );
                if how == LastReference::RemoveUnknown {
                    update = update.col_expr(
                        tailnet_serve_entries::Column::AppliedEtag,
                        Expr::value(Option::<String>::None),
                    );
                }
                update
                    .filter(tailnet_serve_entries::Column::Id.eq(row.id))
                    .exec(&txn)
                    .await
                    .map_err(wrap("marking for removal"))?;
                notify(&txn).await.map_err(wrap("announcing"))?;
                ServeRelease::Removing
            }
        }
    };
    txn.commit().await.map_err(wrap("releasing"))?;
    Ok(outcome)
}

/// Delete a `removing` entry the user chose to leave on the worker as it
/// is: nothing more is asked of its agent.
pub async fn forget(db: &DatabaseConnection, entry_id: i32) -> Result<(), ServeEntryError> {
    let wrap = |source| ServeEntryError::Forget { entry_id, source };
    let txn = db.begin().await.map_err(wrap)?;
    let Some(row) = tailnet_serve_entries::Entity::find_by_id(entry_id)
        .one(&txn)
        .await
        .map_err(wrap)?
    else {
        return Ok(());
    };
    lock_entry(&txn, row.node_id, &row.address_id)
        .await
        .map_err(wrap)?;
    let references = tailnet_serve_refs::Entity::find()
        .filter(tailnet_serve_refs::Column::EntryId.eq(entry_id))
        .count(&txn)
        .await
        .map_err(wrap)?;
    // Only an entry nothing references and that is being removed.
    if references == 0 {
        tailnet_serve_entries::Entity::delete_many()
            .filter(tailnet_serve_entries::Column::Id.eq(entry_id))
            .filter(tailnet_serve_entries::Column::State.eq(tailnet_serve_entries::STATE_REMOVING))
            .exec(&txn)
            .await
            .map_err(wrap)?;
        notify(&txn).await.map_err(wrap)?;
    }
    txn.commit().await.map_err(wrap)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reachability::run::ServeTarget;
    use temps_core::tailnet::{ServeEntryReport, ServeOutcome, TailscaleState};

    /// ADR 065 D4's table: what an entry with no reference left becomes.
    #[test]
    fn the_last_report_decides_what_an_unreferenced_entry_becomes() {
        assert_eq!(last_reference(Some("conflict")), LastReference::Delete);
        assert_eq!(
            last_reference(Some("applied")),
            LastReference::RemoveApplied
        );
        // The write may have landed before the error: never "wrote nothing".
        assert_eq!(last_reference(Some("error")), LastReference::RemoveUnknown);
        assert_eq!(last_reference(None), LastReference::RemoveUnknown);
    }

    fn row(state: &str, report: Option<&str>) -> tailnet_serve_entries::Model {
        let now = chrono::Utc::now();
        tailnet_serve_entries::Model {
            id: 7,
            node_id: 2,
            node_name: "master".into(),
            address_id: "custom:12".into(),
            hostname: "master.example.ts.net".into(),
            private_via: "tailscale_serve".into(),
            service_name: None,
            tailnet: None,
            service_addrs: None,
            state: state.into(),
            report: report.map(str::to_string),
            report_detail: None,
            applied_etag: None,
            reported_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn a_status_carries_funnel_and_the_workers_problem_for_this_entry_only() {
        let mut report = TailscaleReport {
            consent: true,
            root: true,
            state: TailscaleState::NoSocket,
            error: Some("Tailscale is not running".into()),
            entries: vec![
                ServeEntryReport {
                    entry_id: 7,
                    address_id: "custom:12".into(),
                    outcome: ServeOutcome::Applied,
                    etag: Some("\"v2\"".into()),
                    detail: None,
                    funnel: true,
                },
                ServeEntryReport {
                    entry_id: 8,
                    address_id: "custom:13".into(),
                    outcome: ServeOutcome::Applied,
                    etag: None,
                    detail: None,
                    funnel: false,
                },
            ],
            ..TailscaleReport::default()
        };
        let status = status_of(&row("active", Some("applied")), true, Some(&report));
        assert!(status.funnel);
        assert_eq!(
            status.worker_error.as_deref(),
            Some("Tailscale is not running")
        );
        assert!(!status.removing && !status.worker_gone);

        // Another entry's Funnel is not this one's.
        report.entries[0].funnel = false;
        report.entries[1].funnel = true;
        assert!(!status_of(&row("active", None), true, Some(&report)).funnel);
        assert!(status_of(&row("removing", None), false, None).worker_gone);
    }

    fn wish(via: &str, service: Option<&str>) -> ServeWish {
        ServeWish {
            target: ServeTarget {
                node_id: 2,
                node_name: "master".into(),
                via: via.into(),
                service: service.map(str::to_string),
                tailnet: service.map(|_| "example.ts.net".to_string()),
            },
            address_id: "custom:12".into(),
            hostname: "master.example.ts.net".into(),
            service_addrs: Vec::new(),
        }
    }

    #[test]
    fn only_the_same_way_in_and_name_is_the_same_wish() {
        let row = row("active", None);
        assert!(same_wish(&row, &wish("tailscale_serve", None)));
        assert!(!same_wish(
            &row,
            &wish("tailscale_service", Some("svc:myapp"))
        ));
        assert!(describe(&row).contains("machine name"));
    }

    async fn database() -> Option<(temps_database::test_utils::TestDatabase, i32, i32)> {
        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) => {
                eprintln!("Docker/Postgres unavailable; skipping Serve entry test: {error}");
                return None;
            }
        };
        let db = test_db.connection_arc();
        let now = chrono::Utc::now();
        let mut runs = Vec::new();
        for _ in 0..2 {
            let run = temps_entities::reachability_runs::ActiveModel {
                kind: Set("apply".into()),
                audience: Set("private".into()),
                option: Set("tailscale_serve".into()),
                state: Set("running".into()),
                steps: Set(serde_json::json!([])),
                journal: Set(serde_json::json!([])),
                context: Set(serde_json::Value::Null),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            }
            .insert(db.as_ref())
            .await
            .expect("run");
            runs.push(run.id);
        }
        Some((test_db, runs[0], runs[1]))
    }

    /// ADR 065 D4: runs reference one shared entry; the entry goes only with
    /// the last reference, and by what the agent last reported.
    #[tokio::test]
    async fn references_share_one_entry_and_the_last_one_retires_it() {
        let Some((test_db, first, second)) = database().await else {
            return;
        };
        let db = test_db.connection_arc();
        let wish = wish("tailscale_serve", None);

        let created = claim(&db, first, &wish).await.expect("claim");
        let ServeClaim::Created { entry_id } = created else {
            panic!("expected created, got {created:?}");
        };
        // A retry of the same run's claim changes nothing.
        assert_eq!(
            claim(&db, first, &wish).await.expect("retry"),
            ServeClaim::Created { entry_id }
        );
        assert_eq!(
            claim(&db, second, &wish).await.expect("found"),
            ServeClaim::Found { entry_id }
        );
        // Another wish for the slot is refused, naming what is there.
        let other = claim(
            &db,
            second,
            &self::wish("tailscale_service", Some("svc:myapp")),
        )
        .await
        .expect("differs");
        assert!(matches!(other, ServeClaim::Differs(ref what) if what.contains("machine name")));
        assert_eq!(
            reference(&db, first, 2, "custom:12").await.expect("ref"),
            Some((entry_id, true))
        );
        assert_eq!(
            reference(&db, second, 2, "custom:12").await.expect("ref"),
            Some((entry_id, false))
        );

        // The first run's undo leaves the entry to the second.
        assert!(matches!(
            release(&db, first, 2, "custom:12").await.expect("release"),
            ServeRelease::Kept(_)
        ));
        assert_eq!(
            release(&db, first, 2, "custom:12").await.expect("again"),
            ServeRelease::Gone
        );

        // The agent applied it: the last undo has the agent remove it.
        tailnet_serve_entries::Entity::update_many()
            .col_expr(
                tailnet_serve_entries::Column::Report,
                Expr::value("applied"),
            )
            .col_expr(
                tailnet_serve_entries::Column::AppliedEtag,
                Expr::value("\"v3\""),
            )
            .filter(tailnet_serve_entries::Column::Id.eq(entry_id))
            .exec(db.as_ref())
            .await
            .expect("report");
        assert_eq!(
            release(&db, second, 2, "custom:12").await.expect("last"),
            ServeRelease::Removing
        );
        let removing = tailnet_serve_entries::Entity::find_by_id(entry_id)
            .one(db.as_ref())
            .await
            .expect("read")
            .expect("row");
        assert_eq!(removing.state, "removing");
        assert_eq!(removing.applied_etag.as_deref(), Some("\"v3\""));
        // Never revived while it is being removed.
        assert_eq!(
            claim(&db, first, &wish).await.expect("claim"),
            ServeClaim::Removing
        );
        forget(&db, entry_id).await.expect("forget");
        assert!(tailnet_serve_entries::Entity::find_by_id(entry_id)
            .one(db.as_ref())
            .await
            .expect("read")
            .is_none());

        // The agent wrote nothing (conflict): the row just goes.
        let ServeClaim::Created { entry_id } = claim(&db, first, &wish).await.expect("claim")
        else {
            panic!("expected a new entry");
        };
        tailnet_serve_entries::Entity::update_many()
            .col_expr(
                tailnet_serve_entries::Column::Report,
                Expr::value("conflict"),
            )
            .filter(tailnet_serve_entries::Column::Id.eq(entry_id))
            .exec(db.as_ref())
            .await
            .expect("report");
        assert_eq!(
            release(&db, first, 2, "custom:12").await.expect("release"),
            ServeRelease::Deleted
        );

        // No report, or an error: removed by ownership, without an etag.
        let ServeClaim::Created { entry_id } = claim(&db, first, &wish).await.expect("claim")
        else {
            panic!("expected a new entry");
        };
        assert_eq!(
            release(&db, first, 2, "custom:12").await.expect("release"),
            ServeRelease::Removing
        );
        let removing = tailnet_serve_entries::Entity::find_by_id(entry_id)
            .one(db.as_ref())
            .await
            .expect("read")
            .expect("row");
        assert_eq!(removing.applied_etag, None);
    }
}
