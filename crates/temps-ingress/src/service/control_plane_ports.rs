// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Which loopback port a tunnel on the control plane sends visitors to
//! (ADR-107 D3). A worker agent on the same machine holds the default
//! `edge_port`, so the control plane's proxy may listen on a later one, and
//! a tunnel placed only on the control plane follows it.

use std::collections::HashSet;

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use temps_core::CONTROL_PLANE_NODE_ID;
use temps_entities::{ingress_connector_nodes, ingress_connectors};
use tracing::{info, warn};

use super::{live_connectors, IngressConnectorService, DEFAULT_EDGE_PORT};
use crate::error::IngressError;

impl IngressConnectorService {
    /// The loopback port the control plane's proxy takes tunnel traffic
    /// on, when this process runs control-plane connectors and bound one.
    pub(super) fn control_plane_port(&self) -> Option<u16> {
        self.control_plane.get()?.bound_port()
    }

    /// The `edge_port` of a new tunnel placed on `node_ids`: the control
    /// plane's own port when it runs only there (a worker agent on the same
    /// machine may hold the default), the default otherwise. Every worker
    /// binds a connector's port itself, so they all keep the default.
    pub(super) fn edge_port_for(&self, node_ids: &[i32]) -> i32 {
        match self.control_plane_port() {
            Some(port)
                if !node_ids.is_empty()
                    && node_ids.iter().all(|id| *id == CONTROL_PLANE_NODE_ID) =>
            {
                i32::from(port)
            }
            _ => DEFAULT_EDGE_PORT,
        }
    }

    /// Point every tunnel placed only on the control plane at `port`, the
    /// loopback port its proxy actually listens on (ADR-107 D3): the default
    /// is taken when a worker agent runs on the same machine, and the port is
    /// part of the provider's tunnel configuration (`http://127.0.0.1:{port}`).
    ///
    /// Each one is re-ensured with the new port and keeps it only once the
    /// provider accepted it, so a failure leaves the tunnel on its old port
    /// with the error on the row, and is not retried every pass: the next
    /// successful sync clears the error and the pass after moves it. A tunnel
    /// also placed on workers keeps its port; its control-plane replica says
    /// why it cannot run.
    pub async fn align_control_plane_edge_ports(&self, port: u16) -> Result<(), IngressError> {
        let on_control_plane: Vec<i32> = ingress_connector_nodes::Entity::find()
            .filter(ingress_connector_nodes::Column::NodeId.eq(CONTROL_PLANE_NODE_ID))
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(|row| row.connector_id)
            .collect();
        if on_control_plane.is_empty() {
            return Ok(());
        }
        let also_on_workers: HashSet<i32> = ingress_connector_nodes::Entity::find()
            .filter(ingress_connector_nodes::Column::ConnectorId.is_in(on_control_plane.clone()))
            .filter(ingress_connector_nodes::Column::NodeId.ne(CONTROL_PLANE_NODE_ID))
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .map(|row| row.connector_id)
            .collect();
        let only_on_control_plane = on_control_plane
            .into_iter()
            .filter(|id| !also_on_workers.contains(id));
        let stale = live_connectors(
            ingress_connectors::Entity::find()
                .filter(ingress_connectors::Column::Id.is_in(only_on_control_plane))
                .filter(ingress_connectors::Column::EdgePort.ne(i32::from(port)))
                .filter(ingress_connectors::Column::Status.ne("error"))
                .all(self.db.as_ref())
                .await?,
        );
        for mut connector in stale {
            let (id, from) = (connector.id, connector.edge_port);
            connector.edge_port = i32::from(port);
            match self.ensure(connector).await {
                Ok(()) => info!(
                    connector_id = id,
                    from,
                    to = port,
                    "moved a control-plane tunnel to the port the proxy listens on"
                ),
                Err(error) => warn!(
                    connector_id = id,
                    from,
                    to = port,
                    %error,
                    "could not move a control-plane tunnel to the port the proxy listens on"
                ),
            }
        }
        Ok(())
    }
}
