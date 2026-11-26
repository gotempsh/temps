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
//! **A first deployment is not locked out.** While any environment records,
//! every refresh lists the steered containers, so a replica is in the table
//! from its first connection, before its deployment is marked complete. It
//! is recorded once the database places it on this host; until then it is
//! let through unrecorded.
//!
//! **Cost.** Each refresh is one query for the environments that record
//! and, while any does or any steered container was last seen, one local
//! container list filtered to the label and one query for where they run.

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
use temps_workload_egress::steering::{steered_by, InstallationId, STEERED_LABEL};
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

/// The containers among `containers` that `installation` steered, by what
/// only Docker records: the labels they were created with and their address
/// on `network`. A container another installation steered on a shared
/// Docker daemon is never taken for this one's, whatever its environment id.
pub fn steered_containers(
    containers: &[bollard::models::ContainerSummary],
    network: &str,
    installation: InstallationId,
) -> Vec<SteeredContainer> {
    containers
        .iter()
        .filter_map(|container| {
            let labels = container.labels.as_ref()?;
            if !steered_by(labels.get(STEERED_LABEL)?, installation) {
                return None;
            }
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

/// Whether a refresh must list this host's containers: while an environment
/// records (a replica of it may have just been steered, before anything
/// says where it runs) or a steered container was last seen (it keeps its
/// way out after recording is turned off). Otherwise nothing can be in the
/// table and the list is skipped.
fn needs_listing(any_recording: bool, steered_seen: bool) -> bool {
    any_recording || steered_seen
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
    /// This installation: only the containers it steered are its own.
    installation: InstallationId,
}

impl DeploymentTraffic {
    pub fn new(
        db: Arc<DatabaseConnection>,
        docker: bollard::Docker,
        network: String,
        installation: InstallationId,
    ) -> Self {
        Self {
            db,
            docker,
            network,
            steered_seen: AtomicBool::new(true),
            installation,
        }
    }

    /// Whether any environment records now, whatever runs where. While one
    /// does, the containers are listed on every refresh: a new replica is
    /// steered before the database knows where it runs, and must be in the
    /// table from its first connection. One row at most, whatever the
    /// number of environments.
    async fn any_recording(&self) -> Result<bool, String> {
        environments::Entity::find()
            .select_only()
            .column(environments::Column::Id)
            .filter(environments::Column::EgressRecording.eq(true))
            .filter(environments::Column::DeletedAt.is_null())
            .into_tuple::<i32>()
            .one(self.db.as_ref())
            .await
            .map(|id| id.is_some())
            .map_err(|e| format!("could not read whether an environment records: {e}"))
    }

    /// The environments that record now and have a container on this host,
    /// as the database has them. Only these are recorded: the control plane
    /// accepts a node's records only for workloads it placed there, so a
    /// container the database does not place here yet (a first deployment's
    /// replica before it is ready) or any more (one being torn down) keeps
    /// its way out and is not recorded, instead of having this host's whole
    /// batch refused.
    async fn recorded_here(&self) -> Result<BTreeSet<i32>, String> {
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
            .map_err(|e| format!("could not read the environments recorded on this host: {e}"))
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

    /// With `discover`, the containers are listed whatever the last list
    /// found: a replica steered while nothing recorded (recording was turned
    /// off mid-deploy) is found all the same.
    async fn refresh(
        &self,
        _address: SocketAddr,
        discover: bool,
    ) -> Result<RecordedTraffic, String> {
        if !discover
            && !needs_listing(
                self.any_recording().await?,
                self.steered_seen.load(Ordering::Relaxed),
            )
        {
            return Ok(RecordedTraffic::default());
        }
        // Not known until a list succeeds: a failed one keeps the next
        // refreshes listing, so a replica a failed discovery missed is found
        // once Docker answers again, not only when it calls.
        self.steered_seen.store(true, Ordering::Relaxed);
        let recording = self.recorded_here().await?;
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
        let steered = steered_containers(&containers, &self.network, self.installation);
        self.steered_seen
            .store(!steered.is_empty(), Ordering::Relaxed);
        Ok(traffic_of(&steered, &recording))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::{ContainerSummary, ContainerSummaryNetworkSettings, EndpointSettings};

    /// A discovery whose list fails, after a list that found nothing and
    /// with nothing recording any more (recording turned off mid-deploy),
    /// leaves the next ordinary refreshes listing: the replica it missed is
    /// found once Docker answers again.
    #[tokio::test]
    async fn a_failed_discovery_keeps_the_next_refreshes_listing() {
        let Some(f) = crate::services::egress_recording::tests::fixture().await else {
            return;
        };
        // Nothing listens on port 1: every Docker call fails at once.
        let unreachable = bollard::Docker::connect_with_http(
            "http://127.0.0.1:1",
            2,
            bollard::API_DEFAULT_VERSION,
        )
        .expect("a client is built without connecting");
        let traffic = DeploymentTraffic::new(
            f.db.connection_arc(),
            unreachable,
            NETWORK.into(),
            InstallationId::from_bytes([1; 8]),
        );
        traffic.steered_seen.store(false, Ordering::Relaxed);
        let address: SocketAddr = "172.18.0.1:13128".parse().unwrap();
        assert!(traffic.refresh(address, true).await.is_err());
        assert!(
            traffic.refresh(address, false).await.is_err(),
            "an ordinary refresh lists again (and fails while Docker does)"
        );
    }

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

    /// Only a container this installation steered, of an environment Docker
    /// names, speaks for that environment, from its address on the
    /// deployment network and no other. One another installation steered on
    /// the same Docker daemon (same environment id, same network) is not
    /// this one's.
    #[test]
    fn only_steered_containers_speak_for_their_environment() {
        let ours = InstallationId::from_bytes([1; 8]);
        let theirs = InstallationId::from_bytes([2; 8]);
        let label = |installation| {
            temps_workload_egress::steering::steered_label(
                installation,
                "172.18.0.1:13128".parse().unwrap(),
            )
        };
        let our_label = label(ours);
        let their_label = label(theirs);
        let steered = [
            (STEERED_LABEL, our_label.as_str()),
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
                &[(STEERED_LABEL, our_label.as_str())],
                &[(NETWORK, "172.18.0.8")],
            ),
            container(
                &[
                    (STEERED_LABEL, our_label.as_str()),
                    (ENVIRONMENT_LABEL, "production"),
                ],
                &[(NETWORK, "172.18.0.9")],
            ),
            // Another installation's replica of an environment with the
            // same id, on the same network.
            container(
                &[
                    (STEERED_LABEL, their_label.as_str()),
                    (ENVIRONMENT_LABEL, "7"),
                ],
                &[(NETWORK, "172.18.0.10")],
            ),
            // Steered before installations were named: nobody's.
            container(
                &[
                    (STEERED_LABEL, "172.18.0.1:13128"),
                    (ENVIRONMENT_LABEL, "7"),
                ],
                &[(NETWORK, "172.18.0.11")],
            ),
        ];
        assert_eq!(
            steered_containers(&containers, NETWORK, ours),
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
            InstallationId::from_bytes([1; 8]),
        );
        assert!(!traffic.any_recording().await.unwrap());
        assert!(traffic.recorded_here().await.unwrap().is_empty());
        f.db.connection()
            .execute_unprepared(&format!(
                "UPDATE environments SET egress_recording = true \
                 WHERE id IN ({PRODUCTION}, {STAGING}, {ELSEWHERE})"
            ))
            .await
            .unwrap();
        assert!(
            traffic.any_recording().await.unwrap(),
            "any environment that records keeps the containers listed, placed or not"
        );
        assert_eq!(
            traffic.recorded_here().await.unwrap(),
            BTreeSet::from([PRODUCTION]),
            "staging's container is gone and the other project's is on a worker"
        );
    }

    /// The first replica of an environment that records is steered before
    /// the database says where it runs. It must be in the table from its
    /// first connection, or a start-up that needs the internet is refused
    /// and never becomes ready: while anything records, the containers are
    /// listed, and a replica not placed here yet is let through unrecorded.
    #[test]
    fn a_first_replica_is_let_through_before_it_is_placed() {
        assert!(
            needs_listing(true, false),
            "an idle host lists as soon as an environment records"
        );
        assert!(
            needs_listing(false, true),
            "a steered container keeps its way out"
        );
        assert!(!needs_listing(false, false));

        let first = SteeredContainer {
            environment_id: 7,
            address: "172.18.0.5".parse().unwrap(),
        };
        let traffic = traffic_of(&[first], &BTreeSet::new());
        let table =
            temps_workload_netpol::EgressTable::build(&traffic.policies, &traffic.identities);
        let rules = table
            .lookup(first.address)
            .expect("the new replica is in the table");
        assert!(!rules.record, "recorded once it is placed here");
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
