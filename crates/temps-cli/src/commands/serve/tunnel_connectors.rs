// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Step 6b: tunnel connectors placed on the control plane (ADR-107 D2).
//!
//! `temps-ingress` decides what node `0` runs and records how it runs;
//! `temps-node-agent`'s supervisor runs it, exactly as on a worker. The two
//! crates do not depend on each other, so this binary joins them: an adapter
//! that converts between their wire types (the agent keeps a copy of the
//! control plane's, like the route snapshot) and runs the agent's supervisor
//! without its own listener, because the proxy in this process already
//! serves the tunnel origin.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use temps_core::tunnel_origin::ControlPlaneTunnelOrigin;
use temps_ingress::wire::{ConnectorStatus, NodeConnectorSpec};
use temps_ingress::{ControlPlaneConnectors, IngressConnectorService, LocalConnectorRuntime};
use temps_node_agent::connectors::{wire as agent_wire, ConnectorSupervisor};
use tokio::sync::Notify;
use tracing::{debug, info, warn};

/// The node agent's supervisor, speaking `temps-ingress`'s wire types.
struct AgentSupervisor {
    supervisor: Arc<ConnectorSupervisor>,
}

#[async_trait]
impl LocalConnectorRuntime for AgentSupervisor {
    async fn apply(&self, specs: Vec<NodeConnectorSpec>) -> Vec<ConnectorStatus> {
        let (specs, refused) = to_agent_specs(specs);
        self.supervisor.apply(specs).await;
        refused
    }

    async fn statuses(&self) -> Vec<ConnectorStatus> {
        self.supervisor
            .statuses()
            .await
            .into_iter()
            .map(from_agent_status)
            .collect()
    }
}

/// The specs in the agent's types, and a `down` status for each one the
/// agent's copy of the types cannot represent. Both copies are built from
/// this repository at once, so that only happens if they drift.
fn to_agent_specs(
    specs: Vec<NodeConnectorSpec>,
) -> (Vec<agent_wire::NodeConnectorSpec>, Vec<ConnectorStatus>) {
    let mut converted = Vec::with_capacity(specs.len());
    let mut refused = Vec::new();
    for spec in specs {
        let id = spec.id;
        match serde_json::to_value(spec).and_then(serde_json::from_value) {
            Ok(spec) => converted.push(spec),
            Err(error) => {
                warn!(connector_id = id, %error, "control-plane connector spec does not convert to the agent's");
                refused.push(ConnectorStatus {
                    connector_id: id,
                    driver: None,
                    state: "down".into(),
                    ready_connections: None,
                    connector_ref: None,
                    last_error: Some(format!(
                        "This Temps build could not hand connector {id} to its supervisor \
                         ({error}). This is a bug; please report it with the Temps version."
                    )),
                    restarts: 0,
                });
            }
        }
    }
    (converted, refused)
}

fn from_agent_status(status: agent_wire::ConnectorStatus) -> ConnectorStatus {
    ConnectorStatus {
        connector_id: status.connector_id,
        driver: status.driver,
        state: status.state,
        ready_connections: status.ready_connections,
        connector_ref: status.connector_ref,
        last_error: status.last_error,
        restarts: status.restarts,
    }
}

/// Run node `0`'s connectors in this process when the proxy shares it
/// (`origin` is `Some`). Without it the service keeps refusing node `0`
/// with the reason.
pub(super) fn start_control_plane_connectors(
    service_context: &temps_core::plugin::ServiceRegistrationContext,
    origin: Option<Arc<ControlPlaneTunnelOrigin>>,
    data_dir: PathBuf,
) {
    let Some(origin) = origin else {
        debug!("the proxy runs in another process; the control plane runs no tunnel connectors");
        return;
    };
    let Some(service) = service_context.get_service::<IngressConnectorService>() else {
        debug!("ingress connectors are not registered; the control plane runs none");
        return;
    };
    let runtime = Arc::new(AgentSupervisor {
        supervisor: ConnectorSupervisor::for_external_origin(data_dir),
    });
    let connectors = ControlPlaneConnectors::attach(service, runtime, origin);
    let shutdown = Arc::new(Notify::new());
    let on_signal = Arc::clone(&shutdown);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            on_signal.notify_one();
        }
    });
    connectors.start(shutdown);
    info!("control-plane tunnel connectors supervised in this process (ADR-107)");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use temps_ingress::wire::{Artifact, DriverChoice, ForwardedJa4, ProcessSpec, Readiness};

    fn spec() -> NodeConnectorSpec {
        let mut secrets = BTreeMap::new();
        secrets.insert("TUNNEL_TOKEN".to_string(), "token-value".to_string());
        NodeConnectorSpec {
            id: 7,
            provider: "cloudflare_tunnel".into(),
            revision: "rev-1".into(),
            edge_port: 7480,
            client_address_header: Some("CF-Connecting-IP".into()),
            client_ja4: Some(ForwardedJa4 {
                header: "cf-ja4".into(),
                source: "cloudflare".into(),
            }),
            drivers: vec![DriverChoice::Process(ProcessSpec {
                artifacts: vec![Artifact {
                    os: "linux".into(),
                    arch: "x86_64".into(),
                    url: "https://example.com/cloudflared".into(),
                    sha256: "0".repeat(64),
                }],
                args: vec!["tunnel".into(), "run".into()],
                env: BTreeMap::new(),
                readiness: Readiness::HttpReady {
                    url: "http://127.0.0.1:20241/ready".into(),
                },
                stop_grace_secs: 10,
            })],
            secrets,
        }
    }

    #[test]
    fn a_control_plane_spec_reaches_the_supervisor_unchanged() {
        let (specs, refused) = to_agent_specs(vec![spec()]);
        assert!(refused.is_empty());
        let [agent] = specs.as_slice() else {
            panic!("expected one spec, got {}", specs.len());
        };
        assert_eq!(agent.id, 7);
        assert_eq!(agent.revision, "rev-1");
        assert_eq!(agent.edge_port, 7480);
        assert_eq!(
            agent.client_address_header.as_deref(),
            Some("CF-Connecting-IP")
        );
        assert_eq!(
            agent.client_ja4.as_ref().map(|j| j.header.as_str()),
            Some("cf-ja4")
        );
        assert_eq!(
            agent.secrets.get("TUNNEL_TOKEN").map(String::as_str),
            Some("token-value")
        );
        assert_eq!(
            serde_json::to_value(&agent.drivers).unwrap(),
            serde_json::to_value(&spec().drivers).unwrap()
        );
    }

    #[test]
    fn the_supervisors_status_is_recorded_field_for_field() {
        let status = from_agent_status(agent_wire::ConnectorStatus {
            connector_id: 7,
            driver: Some("process".into()),
            state: "ready".into(),
            ready_connections: Some(4),
            connector_ref: Some("connector-1".into()),
            last_error: None,
            restarts: 2,
        });
        assert_eq!(status.connector_id, 7);
        assert_eq!(status.driver.as_deref(), Some("process"));
        assert_eq!(status.state, "ready");
        assert_eq!(status.ready_connections, Some(4));
        assert_eq!(status.connector_ref.as_deref(), Some("connector-1"));
        assert_eq!(status.restarts, 2);
    }

    #[tokio::test]
    async fn the_adapter_runs_nothing_for_no_specs() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = AgentSupervisor {
            supervisor: ConnectorSupervisor::for_external_origin(dir.path().to_path_buf()),
        };
        assert!(runtime.apply(Vec::new()).await.is_empty());
        assert!(runtime.statuses().await.is_empty());
    }
}
