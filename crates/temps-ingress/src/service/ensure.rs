// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Configuring a tunnel at its provider and saving the outcome.
//!
//! Two runs for one connector can overlap: the console's sync and the
//! control-plane loop's port move (ADR-107 D3) run on different service
//! instances. The provider call is slow, so no lock or pooled connection is
//! held across it. Instead each run claims a generation on the row before
//! calling the provider and saves only if it is still the newest; a run a
//! newer one superseded applies the row's settings again. The port is
//! settled at the claim, so every run started after a move configures the
//! moved port, and the last configuration the provider receives is the one
//! the row holds.

use chrono::Utc;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set, TransactionTrait};
use temps_core::DBDateTime;
use temps_entities::{ingress_connectors, ingress_publications};
use tracing::{debug, info, warn};

use super::{
    is_deleting, keep_setup, tunnel_replaced, IngressConnectorService, ENSURE_GENERATION_KEY, STALE,
};
use crate::error::IngressError;
use crate::provider::TunnelContext;

/// Runs before giving up on a connector whose configuration keeps being
/// superseded: the newest run then saves the outcome.
const ENSURE_ATTEMPTS: usize = 3;

/// What one run left the row with.
enum Finished {
    Saved,
    /// A newer run started while this one called the provider.
    Superseded,
}

/// The row as a run claimed it.
struct Claim {
    connector: ingress_connectors::Model,
    /// `None` only when the stored settings are not an object, so no
    /// generation can be kept; such a row is not ordered.
    generation: Option<u64>,
    /// The port before the claim: restored if the provider refused a move.
    previous_port: i32,
}

impl IngressConnectorService {
    /// Create or adopt the provider-side tunnel and store the outcome on the
    /// row, so a failure is visible on the connector rather than lost.
    /// `edge_port`: the port to configure instead of the stored one
    /// (`align_control_plane_edge_ports`), kept only when the provider
    /// accepted it.
    pub(super) async fn ensure(&self, id: i32, edge_port: Option<i32>) -> Result<(), IngressError> {
        let mut edge_port = edge_port;
        for attempt in 1..=ENSURE_ATTEMPTS {
            match self.ensure_once(id, edge_port.take()).await? {
                Finished::Saved => return Ok(()),
                Finished::Superseded => debug!(
                    connector_id = id,
                    attempt,
                    "a newer configuration of this tunnel started meanwhile; applying the stored one again"
                ),
            }
        }
        warn!(
            connector_id = id,
            attempts = ENSURE_ATTEMPTS,
            "this tunnel's configuration kept being superseded; the newest run saves the outcome"
        );
        Ok(())
    }

    async fn ensure_once(&self, id: i32, edge_port: Option<i32>) -> Result<Finished, IngressError> {
        let Some(claim) = self.claim_ensure(id, edge_port).await? else {
            return Ok(Finished::Saved);
        };
        let connector = &claim.connector;
        let outcome = async {
            let provider = self.provider_for(connector).await?;
            let existing = self.handle_of(connector)?;
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
                if let Ok(provider) = self.provider_for(connector).await {
                    let _ = provider.delete(handle).await;
                }
            }
            return Ok(Finished::Saved);
        }
        if ensure_generation(&current.settings) != claim.generation {
            // A newer run claimed the row after this one: its outcome is the
            // one to keep, and this run's call may have reached the provider
            // after it, so the stored configuration is applied again.
            return Ok(Finished::Superseded);
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
                Ok(Finished::Saved)
            }
            Err(error) => {
                // The provider kept its previous configuration: so does the
                // row, and the error says why.
                row.edge_port = Set(claim.previous_port);
                row.status = Set("error".into());
                row.last_error = Set(Some(error.to_string()));
                row.update(&txn).await?;
                txn.commit().await?;
                Err(error)
            }
        }
    }

    /// Claim the next generation and settle the port, in one short
    /// transaction. `None` when the connector is being removed.
    async fn claim_ensure(
        &self,
        id: i32,
        edge_port: Option<i32>,
    ) -> Result<Option<Claim>, IngressError> {
        let _guard = self.settings_lock.lock().await;
        let txn = self.db.begin().await?;
        let mut connector = Self::find_for_update(&txn, id).await?;
        if is_deleting(&connector) {
            return Ok(None);
        }
        let previous_port = connector.edge_port;
        let generation = match &mut connector.settings {
            serde_json::Value::Object(map) => {
                let next = map
                    .get(ENSURE_GENERATION_KEY)
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0)
                    .saturating_add(1);
                map.insert(ENSURE_GENERATION_KEY.into(), serde_json::json!(next));
                Some(next)
            }
            _ => None,
        };
        if let Some(port) = edge_port {
            connector.edge_port = port;
        }
        let mut row: ingress_connectors::ActiveModel = connector.clone().into();
        row.settings = Set(connector.settings.clone());
        row.edge_port = Set(connector.edge_port);
        row.update(&txn).await?;
        txn.commit().await?;
        Ok(Some(Claim {
            connector,
            generation,
            previous_port,
        }))
    }
}

/// The generation of the newest configuration started for a connector.
fn ensure_generation(settings: &serde_json::Value) -> Option<u64> {
    settings
        .get(ENSURE_GENERATION_KEY)
        .and_then(serde_json::Value::as_u64)
}
