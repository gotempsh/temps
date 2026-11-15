// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pull connector specs from the control plane and report how they run.
//!
//! The last specs are kept on disk (0600: they hold connector secrets), so a
//! node that restarts while the control plane is unreachable brings its
//! tunnels back anyway: fail-static, like the route snapshot.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;
use tracing::{debug, warn};

use temps_core::worker_ingress::IngressMode;

use super::wire::{
    ConnectorStatus, NodeConnectorSpec, NodeConnectorStatusReport, NodeConnectorsResponse,
};
use super::ConnectorSupervisor;
use crate::ControlPlaneCa;

const POLL_INTERVAL: Duration = Duration::from_secs(15);

pub struct ConnectorSync {
    http: reqwest::Client,
    control_plane_url: String,
    node_id: i32,
    node_token: String,
    supervisor: Arc<ConnectorSupervisor>,
    state_path: PathBuf,
    shutdown: Arc<Notify>,
}

impl ConnectorSync {
    pub fn new(
        control_plane_url: String,
        node_id: i32,
        node_token: String,
        control_plane_ca: Option<ControlPlaneCa>,
        supervisor: Arc<ConnectorSupervisor>,
        data_dir: PathBuf,
        shutdown: Arc<Notify>,
    ) -> Result<Self, String> {
        let http = crate::with_control_plane_trust(
            reqwest::Client::builder().timeout(Duration::from_secs(20)),
            control_plane_ca,
        )
        .build()
        .map_err(|e| e.to_string())?;
        Ok(Self {
            http,
            control_plane_url: control_plane_url.trim_end_matches('/').to_string(),
            node_id,
            node_token,
            supervisor,
            state_path: data_dir.join("connectors").join("specs.json"),
            shutdown,
        })
    }

    pub async fn run(self) {
        let mut assigned: Vec<NodeConnectorSpec> = self
            .load()
            .map(|saved| saved.connectors)
            .unwrap_or_default();
        let mut mode = crate::public_ingress_manager::watch_mode();
        let mut last_error: Option<String> = None;
        let mut refused_logged = false;
        loop {
            match self.fetch().await {
                Ok(specs) => {
                    if last_error.take().is_some() {
                        warn!("ingress connector specs reachable again");
                    }
                    self.save(&specs);
                    assigned = specs.connectors;
                }
                Err(error) => {
                    if last_error.as_deref() != Some(error.as_str()) {
                        warn!(%error, "could not fetch ingress connector specs; keeping the current ones");
                    }
                    last_error = Some(error);
                }
            }
            // A tunnel puts this worker on the internet as much as an open
            // port does, so it runs only where the worker delegated public
            // ingress (ADR 058 D7). The control plane cannot override this.
            let allowed = crate::public_ingress_manager::current_mode()
                .is_none_or(|mode| mode == IngressMode::Delegated);
            let refused = if allowed || assigned.is_empty() {
                refused_logged = false;
                self.supervisor.apply(assigned.clone()).await;
                Vec::new()
            } else {
                if !refused_logged {
                    warn!(
                        count = assigned.len(),
                        "not running ingress connectors: this worker does not delegate public ingress (`temps agent ingress delegated`)"
                    );
                    refused_logged = true;
                }
                self.supervisor.apply(Vec::new()).await;
                assigned.iter().map(refused_status).collect()
            };
            if let Err(error) = self.report(refused).await {
                debug!(%error, "could not report ingress connector status");
            }
            let mode_changed = async {
                match mode.as_mut() {
                    Some(receiver) => {
                        if receiver.changed().await.is_err() {
                            std::future::pending::<()>().await;
                        }
                    }
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                _ = self.shutdown.notified() => return,
                _ = mode_changed => {}
                _ = tokio::time::sleep(POLL_INTERVAL) => {}
            }
        }
    }

    async fn fetch(&self) -> Result<NodeConnectorsResponse, String> {
        let url = format!(
            "{}/api/internal/nodes/{}/ingress/connectors",
            self.control_plane_url, self.node_id
        );
        let response = self
            .http
            .get(&url)
            .bearer_auth(&self.node_token)
            .send()
            .await
            .map_err(|e| format!("GET {url}: {e}"))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            // A control plane without connector support: nothing to run.
            return Ok(NodeConnectorsResponse::default());
        }
        let response = response
            .error_for_status()
            .map_err(|e| format!("GET {url}: {e}"))?;
        response
            .json()
            .await
            .map_err(|e| format!("GET {url}: unreadable body: {e}"))
    }

    async fn report(&self, refused: Vec<ConnectorStatus>) -> Result<(), String> {
        let mut reports = self.supervisor.statuses().await;
        reports.extend(refused);
        if reports.is_empty() {
            return Ok(());
        }
        let url = format!(
            "{}/api/internal/nodes/{}/ingress/status",
            self.control_plane_url, self.node_id
        );
        self.http
            .post(&url)
            .bearer_auth(&self.node_token)
            .json(&NodeConnectorStatusReport { reports })
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| format!("POST {url}: {e}"))?;
        Ok(())
    }

    fn load(&self) -> Option<NodeConnectorsResponse> {
        let bytes = std::fs::read(&self.state_path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn save(&self, specs: &NodeConnectorsResponse) {
        let write = || -> std::io::Result<()> {
            if let Some(dir) = self.state_path.parent() {
                std::fs::create_dir_all(dir)?;
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            }
            let tmp = self.state_path.with_extension("json.part");
            std::fs::write(&tmp, serde_json::to_vec(specs).unwrap_or_default())?;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
            std::fs::rename(&tmp, &self.state_path)
        };
        if let Err(error) = write() {
            warn!(%error, path = %self.state_path.display(), "could not save ingress connector specs");
        }
    }
}

/// What the console shows for a connector this worker will not run.
pub const REFUSED_STATE: &str = "refused";

fn refused_status(spec: &NodeConnectorSpec) -> ConnectorStatus {
    ConnectorStatus {
        connector_id: spec.id,
        driver: None,
        state: REFUSED_STATE.to_string(),
        ready_connections: None,
        connector_ref: None,
        last_error: Some(
            "This worker does not let Temps expose it. On the worker, run `temps agent ingress delegated` as root.".to_string(),
        ),
        restarts: 0,
    }
}
