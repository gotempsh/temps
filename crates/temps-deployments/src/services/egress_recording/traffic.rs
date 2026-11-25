// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! How a deployment's traffic reaches the control plane's egress recorder.
//!
//! The recorder itself (one proxy, one table, for every workload kind) is
//! `temps_workload_egress::local`. This is the deployment kind's
//! [`LocalTraffic`]:
//!
//! **A container is steered when it starts.** A replica of an environment
//! that records is created with the recorder as its HTTP(S) proxy and the
//! label [`STEERED_LABEL`] (see `RecordedEgress`). Nothing changes for a
//! container that is already running.
//!
//! **Where the recorder listens**: the host's address on the network every
//! deployment container of this host is on.
//!
//! **Who is calling** comes from the source address: a container reaches
//! the recorder from its own address on that network, which only Docker
//! assigns, and its environment is the one Docker's own label names.
//!
//! **Every steered container stays in the table**, whether its environment
//! records now or not: its proxy variables do not change while it runs, so
//! an environment whose recording was turned off keeps its way out and is
//! simply no longer recorded. Each is held to the same policy: any public
//! destination, never a private, loopback, link-local or metadata address,
//! which a container reaches directly or not at all.
//!
//! **A new replica is announced.** The deploy path refreshes the recorder
//! as soon as it has created a steered container, so the application's
//! first connection already finds its address in the table.
//!
//! **Cost.** Each refresh is one query for the environments that record on
//! this host and, while any does or any steered container was last seen,
//! one local container list filtered to the label.

use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{
    ColumnTrait, DatabaseConnection, EntityTrait, JoinType, QueryFilter, QuerySelect, RelationTrait,
};
use temps_entities::{deployment_containers, deployments, environments};
use temps_workload::{WorkloadKind, WorkloadRef};
use temps_workload_egress::local::{LocalTraffic, RecordedTraffic};
use temps_workload_egress::steering::STEERED_LABEL;
use temps_workload_netpol::rule::{Action, PortRange};
use temps_workload_netpol::{EgressPolicy, EgressRule, EgressTarget, HostPattern, IdentityMap};

/// The label Docker carries with a deployment container's environment id.
pub const ENVIRONMENT_LABEL: &str = "sh.temps.environment";

/// What a steered container is held to: any public destination on any
/// port. Recording observes; it adds no restriction beyond never letting
/// the host's proxy reach what the container could not reach itself.
pub fn observe_policy(subject: WorkloadRef, record: bool) -> EgressPolicy {
    let rules = HostPattern::parse("*")
        .map(|any| {
            vec![EgressRule {
                id: 1,
                target: EgressTarget::Host(any),
                ports: Some(PortRange {
                    start: 1,
                    end: u16::MAX,
                }),
                action: Action::Allow,
            }]
        })
        .unwrap_or_default();
    EgressPolicy {
        subject,
        rules,
        record,
        public_only: true,
    }
}

/// A steered container: the environment Docker says it belongs to, and its
/// address on the deployment network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SteeredContainer {
    pub environment_id: i32,
    pub address: IpAddr,
}

/// The steered containers among `containers`, by what only Docker records:
/// the labels they were created with and their address on `network`.
pub fn steered_containers(
    containers: &[bollard::models::ContainerSummary],
    network: &str,
) -> Vec<SteeredContainer> {
    containers
        .iter()
        .filter_map(|container| {
            let labels = container.labels.as_ref()?;
            labels.get(STEERED_LABEL)?;
            let environment_id = labels.get(ENVIRONMENT_LABEL)?.parse().ok()?;
            let address = container
                .network_settings
                .as_ref()?
                .networks
                .as_ref()?
                .get(network)?
                .ip_address
                .as_deref()?
                .parse()
                .ok()?;
            Some(SteeredContainer {
                environment_id,
                address,
            })
        })
        .collect()
}

/// The table entries for `containers`, recording those whose environment
/// is in `recording`.
pub fn traffic_of(containers: &[SteeredContainer], recording: &BTreeSet<i32>) -> RecordedTraffic {
    let mut identities = IdentityMap::new();
    let mut environments = BTreeSet::new();
    for container in containers {
        let subject = super::workload(container.environment_id);
        identities.insert(subject, container.address);
        environments.insert(container.environment_id);
    }
    RecordedTraffic {
        policies: environments
            .into_iter()
            .map(|id| observe_policy(super::workload(id), recording.contains(&id)))
            .collect(),
        identities,
    }
}

/// The steered deployment containers of this host.
pub struct DeploymentTraffic {
    db: Arc<DatabaseConnection>,
    docker: bollard::Docker,
    /// The network every deployment container of this host is on.
    network: String,
    /// Whether the last container list found a steered container. Starts
    /// true: a restart does not know what is running.
    steered_seen: AtomicBool,
}

impl DeploymentTraffic {
    pub fn new(db: Arc<DatabaseConnection>, docker: bollard::Docker, network: String) -> Self {
        Self {
            db,
            docker,
            network,
            steered_seen: AtomicBool::new(true),
        }
    }

    /// The environments that record now and have a container on this host,
    /// as the database has them. Only these are recorded: the control plane
    /// accepts a node's records only for workloads it placed there, so a
    /// container the database no longer knows (one being torn down) keeps
    /// its way out and is not recorded, instead of having this host's whole
    /// batch refused.
    async fn recording(&self) -> Result<BTreeSet<i32>, String> {
        deployment_containers::Entity::find()
            .select_only()
            .column(deployments::Column::EnvironmentId)
            .distinct()
            .join(
                JoinType::InnerJoin,
                deployment_containers::Relation::Deployment.def(),
            )
            .join(
                JoinType::InnerJoin,
                deployments::Relation::Environment.def(),
            )
            .filter(deployment_containers::Column::DeletedAt.is_null())
            .filter(deployment_containers::Column::NodeId.is_null())
            .filter(environments::Column::EgressRecording.eq(true))
            .filter(environments::Column::DeletedAt.is_null())
            .into_tuple::<i32>()
            .all(self.db.as_ref())
            .await
            .map(|ids| ids.into_iter().collect())
            .map_err(|e| format!("could not read the environments that record: {e}"))
    }
}

#[async_trait]
impl LocalTraffic for DeploymentTraffic {
    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Deployment
    }

    /// The host's address on the deployment network, once it exists.
    async fn listen_address(&self) -> Result<Option<IpAddr>, String> {
        let network = match self
            .docker
            .inspect_network(
                &self.network,
                None::<bollard::query_parameters::InspectNetworkOptions>,
            )
            .await
        {
            Ok(network) => network,
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "could not inspect the deployment network '{}': {error}",
                    self.network
                ))
            }
        };
        Ok(network
            .ipam
            .and_then(|ipam| ipam.config)
            .into_iter()
            .flatten()
            .filter_map(|config| config.gateway)
            .find_map(|gateway| gateway.parse().ok()))
    }

    async fn refresh(&self, _address: SocketAddr) -> Result<RecordedTraffic, String> {
        let recording = self.recording().await?;
        if recording.is_empty() && !self.steered_seen.load(Ordering::Relaxed) {
            return Ok(RecordedTraffic::default());
        }
        let containers = self
            .docker
            .list_containers(Some(bollard::query_parameters::ListContainersOptions {
                filters: Some(HashMap::from([(
                    "label".to_string(),
                    vec![STEERED_LABEL.to_string()],
                )])),
                ..Default::default()
            }))
            .await
            .map_err(|e| format!("could not list the steered deployment containers: {e}"))?;
        let steered = steered_containers(&containers, &self.network);
        self.steered_seen
            .store(!steered.is_empty(), Ordering::Relaxed);
        Ok(traffic_of(&steered, &recording))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::{ContainerSummary, ContainerSummaryNetworkSettings, EndpointSettings};

    const NETWORK: &str = "temps-network";

    fn container(labels: &[(&str, &str)], networks: &[(&str, &str)]) -> ContainerSummary {
        ContainerSummary {
            labels: Some(
                labels
                    .iter()
                    .map(|(key, value)| (key.to_string(), value.to_string()))
                    .collect(),
            ),
            network_settings: Some(ContainerSummaryNetworkSettings {
                networks: Some(
                    networks
                        .iter()
                        .map(|(name, address)| {
                            (
                                name.to_string(),
                                EndpointSettings {
                                    ip_address: Some(address.to_string()),
                                    ..Default::default()
                                },
                            )
                        })
                        .collect(),
                ),
            }),
            ..Default::default()
        }
    }

    /// Only a container Docker says was steered, of an environment Docker
    /// names, speaks for that environment, from its address on the
    /// deployment network and no other.
    #[test]
    fn only_steered_containers_speak_for_their_environment() {
        let steered = [
            (STEERED_LABEL, "172.18.0.1:13128"),
            (ENVIRONMENT_LABEL, "7"),
        ];
        let containers = [
            container(&steered, &[(NETWORK, "172.18.0.5"), ("other", "10.9.0.5")]),
            // Never steered: its traffic does not come here.
            container(&[(ENVIRONMENT_LABEL, "8")], &[(NETWORK, "172.18.0.6")]),
            // Steered, but not on the network the recorder listens on.
            container(&steered, &[("other", "10.9.0.7")]),
            // Steered, but Docker names no environment.
            container(
                &[(STEERED_LABEL, "172.18.0.1:13128")],
                &[(NETWORK, "172.18.0.8")],
            ),
            container(
                &[(STEERED_LABEL, "x"), (ENVIRONMENT_LABEL, "production")],
                &[(NETWORK, "172.18.0.9")],
            ),
        ];
        assert_eq!(
            steered_containers(&containers, NETWORK),
            vec![SteeredContainer {
                environment_id: 7,
                address: "172.18.0.5".parse().unwrap(),
            }]
        );
    }

    /// Every steered container is in the table, so one whose environment
    /// stopped recording keeps its way out; only those that record are
    /// recorded.
    #[test]
    fn a_steered_container_keeps_its_way_out_when_recording_is_turned_off() {
        let at = |environment_id, address: &str| SteeredContainer {
            environment_id,
            address: address.parse().unwrap(),
        };
        let containers = [
            at(7, "172.18.0.5"),
            at(7, "172.18.0.6"),
            at(9, "172.18.0.7"),
        ];
        let traffic = traffic_of(&containers, &BTreeSet::from([7]));
        let table =
            temps_workload_netpol::EgressTable::build(&traffic.policies, &traffic.identities);
        let rules = |address: &str| table.lookup(address.parse().unwrap()).cloned();
        for replica in ["172.18.0.5", "172.18.0.6"] {
            let rules = rules(replica).expect("a replica of the recording environment");
            assert_eq!(rules.workload, super::super::workload(7));
            assert!(rules.record);
        }
        let off = rules("172.18.0.7").expect("still in the table");
        assert_eq!(off.workload, super::super::workload(9));
        assert!(!off.record, "not recorded, and not cut off");
        assert!(rules("172.18.0.99").is_none());
    }

    /// Only an environment that records and that the database places on
    /// this host is recorded here: not one that runs only on a worker, and
    /// not one whose container has been removed.
    #[tokio::test]
    async fn only_environments_placed_on_this_host_are_recorded_here() {
        use crate::services::egress_recording::tests::{fixture, ELSEWHERE, PRODUCTION, STAGING};
        use sea_orm::ConnectionTrait;

        let Some(f) = fixture().await else { return };
        let traffic = DeploymentTraffic::new(
            f.db.connection_arc(),
            bollard::Docker::connect_with_local_defaults().expect("docker client"),
            NETWORK.to_string(),
        );
        assert!(traffic.recording().await.unwrap().is_empty());
        f.db.connection()
            .execute_unprepared(&format!(
                "UPDATE environments SET egress_recording = true \
                 WHERE id IN ({PRODUCTION}, {STAGING}, {ELSEWHERE})"
            ))
            .await
            .unwrap();
        assert_eq!(
            traffic.recording().await.unwrap(),
            BTreeSet::from([PRODUCTION]),
            "staging's container is gone and the other project's is on a worker"
        );
    }

    /// Recording observes: any public destination on any port goes through,
    /// and nothing private does, so the host's proxy never reaches what the
    /// container could not.
    #[test]
    fn a_steered_container_reaches_any_public_destination_and_nothing_private() {
        use temps_workload_netpol::egress::{CompiledRules, Refusal};

        let rules = CompiledRules::new(&observe_policy(super::super::workload(7), true));
        assert!(rules.record && rules.public_only);
        let public: IpAddr = "93.184.216.34".parse().unwrap();
        for port in [443, 80, 5432, 8443] {
            let admission = rules
                .check_host("api.example.com", port)
                .unwrap_or_else(|refusal| panic!("port {port}: {refusal:?}"));
            assert_eq!(rules.check_addresses(&[public], port, admission), Ok(()));
        }
        let admission = rules.check_host("internal.example.com", 443).unwrap();
        for private in ["10.0.0.5", "127.0.0.1", "169.254.169.254", "172.18.0.1"] {
            assert_eq!(
                rules.check_addresses(&[private.parse().unwrap()], 443, admission),
                Err(Refusal::Private),
                "{private}"
            );
        }
    }
}
