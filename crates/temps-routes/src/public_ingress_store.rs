// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a worker records about its direct ingress (ADR 058 D7): its ingress
//! key and its listener report. The node-facing handlers in
//! [`crate::public_ingress_sync`] authenticate and validate; this writes.

use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseConnection, DbErr, EntityTrait,
    QueryFilter,
};
use temps_core::tailnet::{ServeEntryReport, ServeOutcome, TailscaleReport};
use temps_core::worker_ingress::ListenerReport;
use temps_entities::{node_public_ingress, nodes, tailnet_serve_entries};

/// Most host addresses kept from one report; a host with more is unusual and
/// the console only needs the plausible candidates.
const MAX_INTERFACES: usize = 64;
/// Most inside checks kept from one report.
const MAX_PRIVATE_CHECKS: usize = 256;
/// Most Serve entry reports and tailnet checks kept from one report (ADR 065).
const MAX_SERVE_ENTRIES: usize = 256;
/// Most capabilities one report may name (ADR 065 D6).
const MAX_CAPABILITIES: usize = 16;

fn clip(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

/// The Tailscale facts as stored, clipped like the rest of the report and
/// stamped with when they arrived (ADR 065 D4's fingerprint, D5's checks).
fn stored_tailscale(
    mut report: TailscaleReport,
    now: chrono::DateTime<chrono::Utc>,
) -> serde_json::Value {
    let short = |v: Option<String>| v.map(|v| clip(&v, 253));
    report.user = short(report.user);
    report.error = report.error.map(|v| clip(&v, 1000));
    report.version = report.version.map(|v| clip(&v, 64));
    report.backend_state = report.backend_state.map(|v| clip(&v, 32));
    report.dns_name = short(report.dns_name);
    report.tailnet = short(report.tailnet);
    report.login = short(report.login);
    report.port_443_held_by = report.port_443_held_by.map(|v| clip(&v, 300));
    report.cert_domains.truncate(64);
    report.tags.truncate(64);
    report.addresses.truncate(16);
    report.entries.truncate(MAX_SERVE_ENTRIES);
    report.checks.truncate(MAX_SERVE_ENTRIES);
    for entry in &mut report.entries {
        entry.address_id = clip(&entry.address_id, 64);
        entry.etag = entry.etag.as_deref().map(|v| clip(v, 128));
        entry.detail = entry.detail.as_deref().map(|v| clip(v, 1000));
    }
    for check in &mut report.checks {
        check.address_id = clip(&check.address_id, 64);
        check.hostname = clip(&check.hostname, 253);
        check.message = clip(&check.message, 1000);
    }
    let mut value = serde_json::json!(report);
    if let Some(object) = value.as_object_mut() {
        object.insert("received_at".to_string(), serde_json::json!(now));
    }
    value
}

/// ADR 064 Security: a worker in `private` mode reporting a listener on an
/// address that is not private. The control plane never routes a private
/// address to it ([`crate::route_sync::private_listener_of`]) and says why.
pub fn refuse_public_private_listener(report: &ListenerReport) -> Option<String> {
    use temps_core::worker_ingress::{check_private_listen, parse_reported_ip, IngressMode};
    if report.mode != Some(IngressMode::Private) {
        return None;
    }
    let ip = report.https.as_deref().and_then(parse_reported_ip)?;
    check_private_listen(ip).err().map(|why| {
        format!(
            "The control plane refuses this worker's private listener: {why} Run `temps agent \
             ingress private --listen tailscale` on the worker."
        )
    })
}

/// Outcome of offering an ingress key.
#[derive(Debug, PartialEq, Eq)]
pub enum KeyRegistration {
    /// The node already has this key; nothing changed.
    AlreadyRegistered,
    /// The key was empty and is now stored.
    Stored,
    /// The node has a different key, which it may not replace on its own.
    Refused,
}

pub struct PublicIngressStore<'a> {
    db: &'a DatabaseConnection,
}

impl<'a> PublicIngressStore<'a> {
    pub fn new(db: &'a DatabaseConnection) -> Self {
        Self { db }
    }

    /// Store `offered` as the node's ingress key if it has none.
    ///
    /// A node token may register the ingress key once. Replacing it would let
    /// whoever holds the token alone redirect certificate bundles to a key
    /// they control, so a different key is refused: the operator re-enrolls
    /// instead.
    pub async fn register_edge_key(
        &self,
        node: &nodes::Model,
        offered: &str,
    ) -> Result<KeyRegistration, DbErr> {
        match node.edge_public_key.as_deref() {
            Some(current) if current == offered => return Ok(KeyRegistration::AlreadyRegistered),
            Some(_) => return Ok(KeyRegistration::Refused),
            None => {}
        }
        // Only fills an empty key: two racing registrations cannot both win.
        let stored = nodes::Entity::update_many()
            .col_expr(nodes::Column::EdgePublicKey, Expr::value(offered))
            .filter(nodes::Column::Id.eq(node.id))
            .filter(nodes::Column::EdgePublicKey.is_null())
            .exec(self.db)
            .await?;
        if stored.rows_affected == 0 {
            return Ok(KeyRegistration::Refused);
        }
        Ok(KeyRegistration::Stored)
    }

    /// Ask the route table to send a fresh snapshot to every agent.
    pub async fn announce_change(&self) -> Result<(), DbErr> {
        self.db
            .execute_unprepared("SELECT pg_notify('route_table_changes', '')")
            .await?;
        Ok(())
    }

    /// Keep what the agent can bind and what it bound, leaving the console's
    /// settings as they are.
    pub async fn record_listener_report(
        &self,
        node_id: i32,
        mut report: ListenerReport,
    ) -> Result<(), DbErr> {
        report.interfaces.truncate(MAX_INTERFACES);
        let tailscale = report.tailscale.take();
        let clip = |value: Option<String>, max: usize| {
            value.map(|v| v.chars().take(max).collect::<String>())
        };
        if let Some(refused) = refuse_public_private_listener(&report) {
            report.error = Some(refused);
        }
        let now = chrono::Utc::now();
        // Inside checks (ADR 064 D7), stamped with when they arrived: the
        // worker's clock never says when an address was last seen working.
        report.private_checks.truncate(MAX_PRIVATE_CHECKS);
        let private_checks =
            (report.mode == Some(temps_core::worker_ingress::IngressMode::Private)).then(|| {
                serde_json::json!(report
                    .private_checks
                    .iter()
                    .map(|check| serde_json::json!({
                        "address_id": check.address_id.chars().take(64).collect::<String>(),
                        "hostname": check.hostname.chars().take(253).collect::<String>(),
                        "listener": check.listener.chars().take(64).collect::<String>(),
                        "outcome": check.outcome,
                        "message": check.message.chars().take(1000).collect::<String>(),
                        "status": check.status,
                        "received_at": now,
                    }))
                    .collect::<Vec<_>>())
            });
        // ADR 065 D6: what the agent enforces decides which routes it gets,
        // so a change rebuilds its snapshot.
        let mut capabilities: Vec<String> = report
            .capabilities
            .iter()
            .take(MAX_CAPABILITIES)
            .map(|c| c.chars().take(64).collect())
            .collect();
        capabilities.sort();
        capabilities.dedup();
        let capabilities_before: Vec<String> = node_public_ingress::Entity::find_by_id(node_id)
            .one(self.db)
            .await?
            .and_then(|row| row.listening)
            .and_then(|l| l.get("capabilities").cloned())
            .and_then(|c| serde_json::from_value(c).ok())
            .unwrap_or_default();
        let listening = serde_json::json!({
            "http": clip(report.http, 64),
            "https": clip(report.https, 64),
            "error": clip(report.error, 1000),
            "capabilities": capabilities,
        });
        let row = node_public_ingress::ActiveModel {
            node_id: Set(node_id),
            listen_address: Set(None),
            http_port: Set(80),
            https_port: Set(443),
            public_address: Set(None),
            interfaces: Set(Some(serde_json::json!(report.interfaces))),
            listening: Set(Some(listening)),
            mode: Set(report.mode.map(|mode| mode.as_str().to_string())),
            last_check: Set(None),
            private_checks: Set(private_checks),
            tailscale: Set(tailscale.clone().map(|t| stored_tailscale(t, now))),
            checked_at: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        };
        node_public_ingress::Entity::insert(row)
            .on_conflict(
                OnConflict::column(node_public_ingress::Column::NodeId)
                    .update_columns([
                        node_public_ingress::Column::Interfaces,
                        node_public_ingress::Column::Listening,
                        node_public_ingress::Column::Mode,
                        node_public_ingress::Column::PrivateChecks,
                        node_public_ingress::Column::Tailscale,
                        node_public_ingress::Column::UpdatedAt,
                    ])
                    .to_owned(),
            )
            .exec(self.db)
            .await?;
        let mut changed = capabilities_before != capabilities;
        if let Some(tailscale) = tailscale {
            let mut entries = tailscale.entries;
            entries.truncate(MAX_SERVE_ENTRIES);
            changed |= self.record_serve_reports(node_id, &entries, now).await?;
        }
        if changed {
            self.announce_change().await?;
        }
        Ok(())
    }

    /// Apply what the agent did with each Serve entry to its row (ADR 065
    /// D4). A report names the row it is for: one about a row that was since
    /// replaced, or for another node, changes nothing. Returns whether a row
    /// was deleted, so the snapshot drops it.
    pub async fn record_serve_reports(
        &self,
        node_id: i32,
        reports: &[ServeEntryReport],
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, DbErr> {
        use tailnet_serve_entries::{
            Column, Entity, REPORT_APPLIED, REPORT_CONFLICT, REPORT_ERROR,
        };
        use tailnet_serve_entries::{STATE_ACTIVE, STATE_REMOVING};
        let mut deleted = false;
        for report in reports {
            let detail = report.detail.as_deref().map(|v| clip(v, 1000));
            match serve_report_effect(report.outcome) {
                ServeReportEffect::Delete => {
                    // Only a row being removed is deleted on `removed` or
                    // `absent`; an active one keeps serving.
                    let result = Entity::delete_many()
                        .filter(Column::Id.eq(report.entry_id))
                        .filter(Column::NodeId.eq(node_id))
                        .filter(Column::AddressId.eq(report.address_id.as_str()))
                        .filter(Column::State.eq(STATE_REMOVING))
                        .exec(self.db)
                        .await?;
                    deleted |= result.rows_affected > 0;
                }
                ServeReportEffect::Record(outcome) => {
                    let report_value = match outcome {
                        ServeOutcome::Applied => REPORT_APPLIED,
                        ServeOutcome::Conflict => REPORT_CONFLICT,
                        ServeOutcome::Error | ServeOutcome::Removed | ServeOutcome::Absent => {
                            REPORT_ERROR
                        }
                    };
                    let detail = match (outcome, report.funnel) {
                        (ServeOutcome::Applied, true) => Some(
                            "Funnel is on for this name: the agent refuses its requests"
                                .to_string(),
                        ),
                        _ => detail,
                    };
                    let mut update = Entity::update_many()
                        .col_expr(Column::Report, Expr::value(report_value))
                        .col_expr(Column::ReportDetail, Expr::value(detail))
                        .col_expr(Column::ReportedAt, Expr::value(now))
                        .col_expr(Column::UpdatedAt, Expr::value(now));
                    update = match outcome {
                        // An applied report on a row being removed is the
                        // agent's state before it saw the removal: ignored.
                        ServeOutcome::Applied => update
                            .col_expr(
                                Column::AppliedEtag,
                                Expr::value(report.etag.as_deref().map(|v| clip(v, 128))),
                            )
                            .filter(Column::State.eq(STATE_ACTIVE)),
                        // A conflict on a row being removed: the slot holds
                        // something the agent did not write. The row stays
                        // until the user decides.
                        ServeOutcome::Conflict
                        | ServeOutcome::Error
                        | ServeOutcome::Removed
                        | ServeOutcome::Absent => update,
                    };
                    update
                        .filter(Column::Id.eq(report.entry_id))
                        .filter(Column::NodeId.eq(node_id))
                        .filter(Column::AddressId.eq(report.address_id.as_str()))
                        .exec(self.db)
                        .await?;
                }
            }
        }
        Ok(deleted)
    }
}

/// What one entry report does to its row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServeReportEffect {
    /// Record the outcome on the row.
    Record(ServeOutcome),
    /// The agent took the entry off (or it was never there): the row goes,
    /// if it is being removed.
    Delete,
}

fn serve_report_effect(outcome: ServeOutcome) -> ServeReportEffect {
    match outcome {
        ServeOutcome::Removed | ServeOutcome::Absent => ServeReportEffect::Delete,
        ServeOutcome::Applied | ServeOutcome::Conflict | ServeOutcome::Error => {
            ServeReportEffect::Record(outcome)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    fn node(key: Option<&str>) -> nodes::Model {
        let mut node = crate::route_sync::tests::test_node(true);
        node.edge_public_key = key.map(str::to_string);
        node
    }

    #[test]
    fn a_private_listener_on_a_public_address_is_refused() {
        use temps_core::worker_ingress::IngressMode;
        let report = |mode, https: &str| ListenerReport {
            mode: Some(mode),
            https: Some(https.to_string()),
            ..ListenerReport::default()
        };
        let refused =
            refuse_public_private_listener(&report(IngressMode::Private, "203.0.113.7:443"))
                .expect("refused");
        assert!(refused.contains("not a private address"), "{refused}");
        assert_eq!(
            refuse_public_private_listener(&report(IngressMode::Private, "100.101.102.103:443")),
            None
        );
        // Other modes listen publicly on purpose.
        assert_eq!(
            refuse_public_private_listener(&report(IngressMode::Fixed, "203.0.113.7:443")),
            None
        );

        let row = |mode: &str, https: &str| node_public_ingress::Model {
            node_id: 1,
            listen_address: None,
            http_port: 80,
            https_port: 443,
            public_address: None,
            interfaces: None,
            listening: Some(serde_json::json!({ "https": https })),
            mode: Some(mode.to_string()),
            last_check: None,
            private_checks: None,
            tailscale: None,
            checked_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        use crate::route_sync::private_listener_of;
        assert_eq!(
            private_listener_of(Some(&row("private", "100.101.102.103:443"))),
            "100.101.102.103".parse().ok()
        );
        assert_eq!(
            private_listener_of(Some(&row("private", "203.0.113.7:443"))),
            None
        );
        assert_eq!(
            private_listener_of(Some(&row("private", "0.0.0.0:443"))),
            None
        );
        assert_eq!(
            private_listener_of(Some(&row("fixed", "10.0.0.2:443"))),
            None
        );
        assert_eq!(private_listener_of(None), None);
    }

    #[tokio::test]
    async fn a_node_registers_its_ingress_key_once() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let store = PublicIngressStore::new(&db);
        assert_eq!(
            store
                .register_edge_key(&node(Some("k1")), "k1")
                .await
                .unwrap(),
            KeyRegistration::AlreadyRegistered
        );
        assert_eq!(
            store
                .register_edge_key(&node(Some("k1")), "k2")
                .await
                .unwrap(),
            KeyRegistration::Refused
        );

        // A racing registration filled the key first: refused, not replaced.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();
        let store = PublicIngressStore::new(&db);
        assert_eq!(
            store.register_edge_key(&node(None), "k1").await.unwrap(),
            KeyRegistration::Refused
        );

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let store = PublicIngressStore::new(&db);
        assert_eq!(
            store.register_edge_key(&node(None), "k1").await.unwrap(),
            KeyRegistration::Stored
        );
    }
}
