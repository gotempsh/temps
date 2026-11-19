// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Configuring a tunnel at its provider and saving the outcome.
//!
//! Two service instances configure tunnels in one process: the console's
//! (create, sync) and the control-plane loop's (moving a tunnel to the port
//! the proxy listens on, ADR-107 D3). Only one control-plane process owns a
//! database at a time (`temps serve` takes an owner lock at startup), so a
//! per-connector lock in that process orders every configuration of a
//! tunnel: a run reads the row, calls the provider and saves the outcome
//! before the next one reads anything. The last configuration the provider
//! receives is therefore always the one saved.
//!
//! The lock is a tokio mutex per connector, not a database lock: waiting on
//! it holds no pooled connection, and it orders only runs for the same
//! tunnel, which must not overlap anyway.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set, TransactionTrait};
use temps_core::DBDateTime;
use temps_entities::{ingress_connectors, ingress_publications};
use tokio::sync::{Mutex, OwnedMutexGuard};
use tracing::info;

use super::{is_deleting, keep_setup, tunnel_replaced, IngressConnectorService, STALE};
use crate::error::IngressError;
use crate::provider::TunnelContext;

/// One lock per connector, shared by every service instance of the
/// process that configures tunnels (through the control-plane handle).
#[derive(Default)]
pub struct ConnectorLocks {
    locks: Mutex<HashMap<i32, Arc<Mutex<()>>>>,
}

impl ConnectorLocks {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Wait until no other run holds connector `id`, then hold it until the
    /// guard is dropped. Entries nobody holds or waits for are dropped, so
    /// the map stays as small as the tunnels in use.
    pub(crate) async fn lock(&self, id: i32) -> OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.locks.lock().await;
            locks.retain(|_, lock| Arc::strong_count(lock) > 1);
            Arc::clone(locks.entry(id).or_default())
        };
        lock.lock_owned().await
    }
}

impl IngressConnectorService {
    /// Create or adopt the provider-side tunnel and store the outcome on the
    /// row, so a failure is visible on the connector rather than lost.
    ///
    /// `edge_port`: move the tunnel to this port
    /// (`align_control_plane_edge_ports`). Applied only while the tunnel
    /// still runs on the control plane alone, checked under the lock that
    /// placement changes take too, and saved only once the provider
    /// accepted it: an interrupted or refused move leaves the stored port,
    /// so the next pass moves it again.
    pub(super) async fn ensure(&self, id: i32, edge_port: Option<i32>) -> Result<(), IngressError> {
        let _serialized = self.connector_locks().lock(id).await;
        let mut connector = self.find(id).await?;
        if is_deleting(&connector) {
            return Ok(());
        }
        if let Some(port) = edge_port {
            if connector.edge_port == port || !self.runs_only_on_control_plane(id).await? {
                return Ok(());
            }
            connector.edge_port = port;
        }
        let outcome = async {
            let provider = self.provider_for(&connector).await?;
            let existing = self.handle_of(&connector)?;
            let instance_id = self.records.instance_id().await?;
            provider
                .ensure(&TunnelContext {
                    connector_id: connector.id,
                    name: &connector.name,
                    domain: &connector.domain,
                    edge_port: connector.edge_port as u16,
                    instance_id: &instance_id,
                    existing,
                })
                .await
        }
        .await;
        // Settings the service owns may have changed while the provider was
        // called (token holders); keep the stored ones.
        let _guard = self.settings_lock.lock().await;
        let txn = self.db.begin().await?;
        let current = Self::find_for_update(&txn, connector.id)
            .await
            .unwrap_or_else(|_| connector.clone());
        if is_deleting(&current) {
            drop(txn);
            // Removed meanwhile: nothing it made is kept.
            if let Ok(handle) = &outcome {
                if let Ok(provider) = self.provider_for(&connector).await {
                    let _ = provider.delete(handle).await;
                }
            }
            return Ok(());
        }
        let mut row: ingress_connectors::ActiveModel = connector.clone().into();
        row.updated_at = Set(Utc::now());
        match outcome {
            Ok(handle) => {
                let secret = self
                    .encryption
                    .encrypt_string(&handle.secret)
                    .map_err(|e| IngressError::Encryption(e.to_string()))?;
                let replaced = tunnel_replaced(current.external_id.as_deref(), &handle.external_id);
                row.external_id = Set(Some(handle.external_id));
                // The port the provider now sends visitors to.
                row.edge_port = Set(connector.edge_port);
                row.settings = Set(keep_setup(&current.settings, handle.settings));
                row.secret_encrypted = Set(Some(secret));
                row.status = Set("ready".into());
                row.last_error = Set(None);
                row.update(&txn).await?;
                if replaced {
                    // Every record points at the tunnel that is gone. Marked
                    // stale, they are published again through the new one (or
                    // removed) by the reconcile that follows.
                    let stale = ingress_publications::Entity::update_many()
                        .col_expr(ingress_publications::Column::State, Expr::value(STALE))
                        .col_expr(
                            ingress_publications::Column::PublishedAt,
                            Expr::value(Option::<DBDateTime>::None),
                        )
                        .col_expr(
                            ingress_publications::Column::UpdatedAt,
                            Expr::value(Utc::now()),
                        )
                        .filter(ingress_publications::Column::ConnectorId.eq(connector.id))
                        .exec(&txn)
                        .await?;
                    info!(
                        connector_id = connector.id,
                        publications = stale.rows_affected,
                        "tunnel was replaced; republishing its hostnames"
                    );
                }
                txn.commit().await?;
                Ok(())
            }
            Err(error) => {
                // The stored port stays: the provider did not take the new one.
                row.status = Set("error".into());
                row.last_error = Set(Some(error.to_string()));
                row.update(&txn).await?;
                txn.commit().await?;
                Err(error)
            }
        }
    }
}
