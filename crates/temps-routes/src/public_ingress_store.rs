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
use temps_core::worker_ingress::ListenerReport;
use temps_entities::{node_public_ingress, nodes};

/// Most host addresses kept from one report; a host with more is unusual and
/// the console only needs the plausible candidates.
const MAX_INTERFACES: usize = 64;

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
        let clip = |value: Option<String>, max: usize| {
            value.map(|v| v.chars().take(max).collect::<String>())
        };
        let listening = serde_json::json!({
            "http": clip(report.http, 64),
            "https": clip(report.https, 64),
            "error": clip(report.error, 1000),
        });
        let now = chrono::Utc::now();
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
                        node_public_ingress::Column::UpdatedAt,
                    ])
                    .to_owned(),
            )
            .exec(self.db)
            .await?;
        Ok(())
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
