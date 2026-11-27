// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Stored direct-ingress settings of a worker (ADR 058 D7).
//!
//! The console handlers validate a change against what the worker allows;
//! this service reads and writes `node_public_ingress` and flips the node's
//! enabled flag in the same transaction.

use std::sync::Arc;

use sea_orm::sea_query::OnConflict;
use sea_orm::{ActiveValue::Set, DatabaseConnection, EntityTrait, QuerySelect, TransactionTrait};
use temps_entities::{node_public_ingress, nodes};
use thiserror::Error;

use crate::services::node_service::{NodeError, NodeService};

#[derive(Debug, Error)]
pub enum NodePublicIngressError {
    #[error(transparent)]
    Node(#[from] NodeError),

    #[error("Could not load ingress settings for node {node_id}: {source}")]
    Load {
        node_id: i32,
        #[source]
        source: sea_orm::DbErr,
    },

    #[error("Could not save ingress settings for node {node_id}: {source}")]
    Save {
        node_id: i32,
        #[source]
        source: sea_orm::DbErr,
    },

    #[error(transparent)]
    StaleRevision(#[from] temps_core::revision::StaleRevision),
}

/// The HTTP port of a node with no stored settings.
pub const DEFAULT_HTTP_PORT: u16 = 80;
/// The HTTPS port of a node with no stored settings.
pub const DEFAULT_HTTPS_PORT: u16 = 443;

/// A stored port as the node uses it: `default` when unset (0) or invalid.
pub fn effective_port(value: i32, default: u16) -> u16 {
    u16::try_from(value)
        .ok()
        .filter(|p| *p != 0)
        .unwrap_or(default)
}

/// The HTTP and HTTPS ports in effect, the defaults when nothing is stored.
pub fn effective_ports(row: Option<&node_public_ingress::Model>) -> (u16, u16) {
    row.map_or((DEFAULT_HTTP_PORT, DEFAULT_HTTPS_PORT), |r| {
        (
            effective_port(r.http_port, DEFAULT_HTTP_PORT),
            effective_port(r.https_port, DEFAULT_HTTPS_PORT),
        )
    })
}

/// Revision of the settings the console writes (the enabled flag, listen
/// address, ports and public address), whichever path changed them last.
/// What the agent reports is left out: it changes on its own and is never
/// written back by a save. The ports are the ones in effect, so a check or
/// report that first creates the row with the defaults changes nothing.
pub fn settings_revision(node: &nodes::Model, row: Option<&node_public_ingress::Model>) -> String {
    let (http_port, https_port) = effective_ports(row);
    temps_core::revision::document_revision(&serde_json::json!({
        "enabled": node.public_ingress_enabled,
        "listen_address": row.and_then(|r| r.listen_address.as_deref()),
        "http_port": http_port,
        "https_port": https_port,
        "public_address": row.and_then(|r| r.public_address.as_deref()),
    }))
}

/// What the console sets; the agent reports the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicIngressChange {
    pub enabled: bool,
    pub listen_address: Option<String>,
    pub http_port: u16,
    pub https_port: u16,
    pub public_address: Option<String>,
}

pub struct NodePublicIngressService {
    db: Arc<DatabaseConnection>,
    node_service: Arc<NodeService>,
}

impl NodePublicIngressService {
    pub fn new(db: Arc<DatabaseConnection>, node_service: Arc<NodeService>) -> Self {
        Self { db, node_service }
    }

    /// The node and its stored settings, `None` when nothing was ever stored.
    pub async fn load(
        &self,
        node_id: i32,
    ) -> Result<(nodes::Model, Option<node_public_ingress::Model>), NodePublicIngressError> {
        let node = self.node_service.get_by_id(node_id).await?;
        let row = node_public_ingress::Entity::find_by_id(node_id)
            .one(self.db.as_ref())
            .await
            .map_err(|source| NodePublicIngressError::Load { node_id, source })?;
        Ok((node, row))
    }

    /// Store the console's settings and turn ingress on or off.
    ///
    /// With `expected_revision` (the [`settings_revision`] the caller read),
    /// the save happens only while the stored settings still have it,
    /// checked under the node row's lock; otherwise it is refused with
    /// [`NodePublicIngressError::StaleRevision`] and nothing changes.
    pub async fn save(
        &self,
        node_id: i32,
        change: &PublicIngressChange,
        expected_revision: Option<&str>,
    ) -> Result<(), NodePublicIngressError> {
        let now = chrono::Utc::now();
        let row = node_public_ingress::ActiveModel {
            node_id: Set(node_id),
            listen_address: Set(change.listen_address.clone()),
            http_port: Set(i32::from(change.http_port)),
            https_port: Set(i32::from(change.https_port)),
            public_address: Set(change.public_address.clone()),
            interfaces: Set(None),
            listening: Set(None),
            mode: Set(None),
            last_check: Set(None),
            private_checks: Set(None),
            tailscale: Set(None),
            checked_at: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        };
        // One transaction, the node row locked first: snapshots never see
        // the flag without its settings, and two overlapping saves apply one
        // after the other instead of mixing one's flag with the other's ports.
        let txn = self
            .db
            .begin()
            .await
            .map_err(|source| NodePublicIngressError::Save { node_id, source })?;
        if let Some(expected) = expected_revision {
            // Locked before reading, so no save lands between this check and
            // the write below. Dropping `txn` on an error rolls it back.
            let node = nodes::Entity::find_by_id(node_id)
                .lock_exclusive()
                .one(&txn)
                .await
                .map_err(|source| NodePublicIngressError::Load { node_id, source })?
                .ok_or(NodeError::NotFoundById { node_id })?;
            let row = node_public_ingress::Entity::find_by_id(node_id)
                .one(&txn)
                .await
                .map_err(|source| NodePublicIngressError::Load { node_id, source })?;
            temps_core::revision::check_expected_revision(
                || {
                    format!(
                        "Public ingress settings of node '{}' (id {node_id})",
                        node.name
                    )
                },
                Some(expected),
                &settings_revision(&node, row.as_ref()),
            )?;
        }
        NodeService::set_public_ingress_enabled_in(&txn, node_id, change.enabled).await?;
        node_public_ingress::Entity::insert(row)
            .on_conflict(
                OnConflict::column(node_public_ingress::Column::NodeId)
                    .update_columns([
                        node_public_ingress::Column::ListenAddress,
                        node_public_ingress::Column::HttpPort,
                        node_public_ingress::Column::HttpsPort,
                        node_public_ingress::Column::PublicAddress,
                        node_public_ingress::Column::UpdatedAt,
                    ])
                    .to_owned(),
            )
            .exec(&txn)
            .await
            .map_err(|source| NodePublicIngressError::Save { node_id, source })?;
        // Agents are notified on commit, so the new settings go out at once.
        txn.commit()
            .await
            .map_err(|source| NodePublicIngressError::Save { node_id, source })?;
        Ok(())
    }

    /// Keep the latest reachability check, leaving the settings as they are.
    pub async fn record_check(
        &self,
        node_id: i32,
        check: serde_json::Value,
    ) -> Result<(), NodePublicIngressError> {
        let now = chrono::Utc::now();
        let stored = node_public_ingress::ActiveModel {
            node_id: Set(node_id),
            listen_address: Set(None),
            http_port: Set(i32::from(DEFAULT_HTTP_PORT)),
            https_port: Set(i32::from(DEFAULT_HTTPS_PORT)),
            public_address: Set(None),
            interfaces: Set(None),
            listening: Set(None),
            mode: Set(None),
            last_check: Set(Some(check)),
            private_checks: Set(None),
            tailscale: Set(None),
            checked_at: Set(Some(now)),
            created_at: Set(now),
            updated_at: Set(now),
        };
        node_public_ingress::Entity::insert(stored)
            .on_conflict(
                OnConflict::column(node_public_ingress::Column::NodeId)
                    .update_columns([
                        node_public_ingress::Column::LastCheck,
                        node_public_ingress::Column::CheckedAt,
                    ])
                    .to_owned(),
            )
            .exec(self.db.as_ref())
            .await
            .map_err(|source| NodePublicIngressError::Save { node_id, source })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::ActiveModelTrait;

    /// #2081: admin A reads a worker's ingress settings, admin B saves, then
    /// A saves what it read with one field changed. A's save must be refused
    /// instead of reverting B's.
    #[tokio::test]
    async fn an_ingress_save_from_a_stale_read_is_refused() {
        if !temps_test_utils::docker_available().await {
            eprintln!("Skipping stale ingress settings test: Docker unavailable");
            return;
        }
        let test_db = temps_test_utils::TestDatabase::with_migrations()
            .await
            .expect("test database");
        let db = test_db.connection_arc();
        let node = nodes::ActiveModel {
            name: Set("ingress-worker".to_string()),
            token_hash: Set("unused".to_string()),
            address: Set("https://10.100.0.9:3100".to_string()),
            private_address: Set("10.100.0.9".to_string()),
            role: Set("worker".to_string()),
            status: Set("active".to_string()),
            labels: Set(serde_json::json!({})),
            capacity: Set(serde_json::json!({})),
            dns_resolver_consecutive_failures: Set(0),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await
        .expect("worker row");
        let service =
            NodePublicIngressService::new(db.clone(), Arc::new(NodeService::new(db.clone())));
        async fn revision(service: &NodePublicIngressService, node_id: i32) -> String {
            let (node, row) = service.load(node_id).await.expect("load");
            settings_revision(&node, row.as_ref())
        }

        let read_by_a = revision(&service, node.id).await;
        let read_by_b = revision(&service, node.id).await;
        let change = |https_port: u16, public_address: Option<&str>| PublicIngressChange {
            enabled: true,
            listen_address: None,
            http_port: 80,
            https_port,
            public_address: public_address.map(str::to_string),
        };
        service
            .save(node.id, &change(443, Some("203.0.113.7")), Some(&read_by_b))
            .await
            .expect("B saves first");
        let after_b = revision(&service, node.id).await;
        assert_ne!(after_b, read_by_b, "a save changes the revision");

        match service
            .save(node.id, &change(8443, None), Some(&read_by_a))
            .await
        {
            Err(NodePublicIngressError::StaleRevision(stale)) => {
                assert_eq!(stale.expected, read_by_a);
                assert_eq!(stale.current, after_b);
                assert!(stale.resource.contains("ingress-worker"), "{stale}");
            }
            other => panic!("expected a stale revision, got {other:?}"),
        }
        let (_, row) = service.load(node.id).await.expect("load");
        let row = row.expect("settings row");
        assert_eq!(
            row.public_address.as_deref(),
            Some("203.0.113.7"),
            "B's save is kept"
        );
        assert_eq!(row.https_port, 443);
        assert_eq!(
            revision(&service, node.id).await,
            after_b,
            "the refused save wrote nothing"
        );

        // Re-read, A's change applies; without a revision a save still goes through.
        service
            .save(node.id, &change(8443, Some("203.0.113.7")), Some(&after_b))
            .await
            .expect("A saves after re-reading");
        service
            .save(node.id, &change(443, None), None)
            .await
            .expect("an older client without a revision");
    }

    #[test]
    fn a_check_that_creates_the_settings_row_keeps_the_revision() {
        let node = nodes::Model {
            public_ingress_enabled: false,
            ..test_node()
        };
        let now = chrono::Utc::now();
        // What `record_check` stores for a node that had no settings row.
        let created_by_a_check = node_public_ingress::Model {
            node_id: node.id,
            listen_address: None,
            http_port: i32::from(DEFAULT_HTTP_PORT),
            https_port: i32::from(DEFAULT_HTTPS_PORT),
            public_address: None,
            interfaces: None,
            listening: None,
            mode: None,
            last_check: Some(serde_json::json!({"reachable": false})),
            private_checks: None,
            tailscale: None,
            checked_at: Some(now),
            created_at: now,
            updated_at: now,
        };
        assert_eq!(
            settings_revision(&node, None),
            settings_revision(&node, Some(&created_by_a_check))
        );
        let moved = node_public_ingress::Model {
            https_port: 8443,
            ..created_by_a_check
        };
        assert_ne!(
            settings_revision(&node, None),
            settings_revision(&node, Some(&moved))
        );
    }

    fn test_node() -> nodes::Model {
        serde_json::from_value(serde_json::json!({
            "id": 7,
            "name": "ingress-worker",
            "token_hash": "unused",
            "address": "https://10.100.0.9:3100",
            "private_address": "10.100.0.9",
            "role": "worker",
            "status": "active",
            "labels": {},
            "capacity": {},
            "dns_resolver_consecutive_failures": 0,
            "public_ingress_enabled": false,
            "public_ingress_unsupported_reasons": [],
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z"
        }))
        .expect("a node row")
    }
}
