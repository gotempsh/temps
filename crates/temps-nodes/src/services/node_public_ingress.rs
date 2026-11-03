// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Stored direct-ingress settings of a worker (ADR 058 D7).
//!
//! The console handlers validate a change against what the worker allows;
//! this service reads and writes `node_public_ingress` and flips the node's
//! enabled flag in the same transaction.

use std::sync::Arc;

use sea_orm::sea_query::OnConflict;
use sea_orm::{ActiveValue::Set, DatabaseConnection, EntityTrait, TransactionTrait};
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
    pub async fn save(
        &self,
        node_id: i32,
        change: &PublicIngressChange,
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
            http_port: Set(80),
            https_port: Set(443),
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
