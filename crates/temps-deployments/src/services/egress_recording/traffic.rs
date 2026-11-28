// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! How a deployment's traffic reaches the control plane's egress recorder.
//!
//! The recorder itself (one proxy, one table, for every workload kind) is
//! `temps_workload_egress::local`. This is the deployment kind's
//! [`LocalTraffic`]:
//!
//! **A container is steered when it starts.** A replica of an environment
//! that records is created with the recorder as its HTTPS proxy and the
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
//! **A first deployment is recorded from its first connection.** The deploy
//! path's refresh lists the steered containers, so a replica is in the
//! table before its deployment is marked complete, and recorded as soon as
//! its environment records: ingest takes this process's own records for an
//! environment placed nowhere yet (see `temps_workload_egress::ingest`).
//!
//! **Cost.** While a steered container was last seen or an environment
//! records, each refresh lists the containers (filtered by Docker to this
//! installation's label value and the deployment network, so another
//! installation's are never returned) and makes one query, by id, for which
//! of their environments record now. The list is never reused: an address
//! Docker freed and gave to another container must stop speaking for the
//! old environment at the next refresh, which is also what confirms a
//! recorded connection's caller, and nothing but a fresh list can say who
//! holds an address now. The recorder rebuilds its table only when what a
//! refresh read changed. An idle host lists nothing.

use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QuerySelect};
use temps_entities::environments;
use temps_workload::{WorkloadKind, WorkloadRef};
use temps_workload_egress::local::{LocalTraffic, LocalTrafficError, RecordedTraffic};
use temps_workload_egress::steering::{steered_by, steered_label, InstallationId, STEERED_LABEL};
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

/// A steered container: the environment Docker says it belongs to, and one
/// of its addresses on the deployment network (one entry per address: an
/// IPv4 one, a global IPv6 one, or both on a dual-stack network).
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
        .flat_map(|container| {
            let steered = || -> Option<(i32, Vec<IpAddr>)> {
                let labels = container.labels.as_ref()?;
                if !steered_by(labels.get(STEERED_LABEL)?, installation) {
                    return None;
                }
                let environment_id = labels.get(ENVIRONMENT_LABEL)?.parse().ok()?;
                let endpoint = container
                    .network_settings
                    .as_ref()?
                    .networks
                    .as_ref()?
                    .get(network)?;
                // An IPv6-only network leaves the IPv4 field empty: the
                // replica calls from its global IPv6 address instead.
                let addresses: Vec<IpAddr> = [
                    endpoint.ip_address.as_deref(),
                    endpoint.global_ipv6_address.as_deref(),
                ]
                .into_iter()
                .flatten()
                .filter_map(|address| address.parse().ok())
                .collect();
                Some((environment_id, addresses))
            };
            let (environment_id, addresses) = steered().unwrap_or_default();
            addresses.into_iter().map(move |address| SteeredContainer {
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
    /// This installation: only the containers it steered are its own.
    installation: InstallationId,
    /// Container lists a test hands out instead of asking Docker.
    #[cfg(test)]
    scripted: std::sync::Mutex<std::collections::VecDeque<Vec<bollard::models::ContainerSummary>>>,
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
            #[cfg(test)]
            scripted: Default::default(),
        }
    }

    /// The next list a test scripted, if any.
    #[cfg(test)]
    fn scripted_list(&self) -> Option<Vec<bollard::models::ContainerSummary>> {
        self.scripted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
    }

    #[cfg(not(test))]
    fn scripted_list(&self) -> Option<Vec<bollard::models::ContainerSummary>> {
        None
    }

    /// Lists this installation's containers steered to `address` on the
    /// deployment network. Docker filters by the exact label value and the
    /// network, so another installation's containers on a shared daemon are
    /// never even returned.
    async fn list(&self, address: SocketAddr) -> Result<Vec<SteeredContainer>, LocalTrafficError> {
        // Not known until a list succeeds: a failed one keeps the next
        // refreshes listing, so a replica a failed discovery missed is found
        // once Docker answers again, not only when it calls.
        self.steered_seen.store(true, Ordering::Relaxed);
        let label = format!(
            "{STEERED_LABEL}={}",
            steered_label(self.installation, address)
        );
        let containers = match self.scripted_list() {
            Some(containers) => containers,
            None => self
                .docker
                .list_containers(Some(bollard::query_parameters::ListContainersOptions {
                    filters: Some(HashMap::from([
                        ("label".to_string(), vec![label]),
                        ("network".to_string(), vec![self.network.clone()]),
                    ])),
                    ..Default::default()
                }))
                .await
                .map_err(LocalTrafficError::docker(
                    WorkloadKind::Deployment,
                    "listing the steered deployment containers",
                ))?,
        };
        let steered = steered_containers(&containers, &self.network, self.installation);
        self.steered_seen
            .store(!steered.is_empty(), Ordering::Relaxed);
        Ok(steered)
    }

    /// Whether any environment records now, whatever runs where. While one
    /// does, an ordinary refresh with no recent list lists the containers.
    /// One row at most, whatever the number of environments.
    async fn any_recording(&self) -> Result<bool, LocalTrafficError> {
        environments::Entity::find()
            .select_only()
            .column(environments::Column::Id)
            .filter(environments::Column::EgressRecording.eq(true))
            .filter(environments::Column::DeletedAt.is_null())
            .into_tuple::<i32>()
            .one(self.db.as_ref())
            .await
            .map(|id| id.is_some())
            .map_err(LocalTrafficError::database(
                WorkloadKind::Deployment,
                "reading whether an environment records",
            ))
    }

    /// Which of `containers`' environments record now: one query by id,
    /// bounded by the steered containers of this host, never by how many
    /// environments exist. Recorded whether or not the database places
    /// them here yet (a first replica before its deployment is complete):
    /// ingest takes this process's own records for an environment placed
    /// nowhere, and drops those of one placed on other nodes only.
    async fn recording_among(
        &self,
        containers: &[SteeredContainer],
    ) -> Result<BTreeSet<i32>, LocalTrafficError> {
        let ids: BTreeSet<i32> = containers.iter().map(|c| c.environment_id).collect();
        if ids.is_empty() {
            return Ok(BTreeSet::new());
        }
        environments::Entity::find()
            .select_only()
            .column(environments::Column::Id)
            .filter(environments::Column::Id.is_in(ids))
            .filter(environments::Column::EgressRecording.eq(true))
            .filter(environments::Column::DeletedAt.is_null())
            .into_tuple::<i32>()
            .all(self.db.as_ref())
            .await
            .map(|ids| ids.into_iter().collect())
            .map_err(LocalTrafficError::database(
                WorkloadKind::Deployment,
                "reading which of this host's steered environments record",
            ))
    }
}

#[async_trait]
impl LocalTraffic for DeploymentTraffic {
    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Deployment
    }

    /// The host's address on the deployment network, once it exists.
    async fn listen_address(&self) -> Result<Option<IpAddr>, LocalTrafficError> {
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
                return Err(LocalTrafficError::docker(
                    WorkloadKind::Deployment,
                    format!("inspecting the deployment network '{}'", self.network),
                )(error))
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
        address: SocketAddr,
        discover: bool,
    ) -> Result<RecordedTraffic, LocalTrafficError> {
        // A refresh lists while a steered container was last seen (it keeps
        // its way out after recording is turned off) or an environment
        // records (a replica of it may have just been steered, before
        // anything says where it runs). The query is asked only when the
        // last list found nothing: while containers are steered, a refresh
        // costs the list and one query, not two.
        if !discover && !self.steered_seen.load(Ordering::Relaxed) && !self.any_recording().await? {
            return Ok(RecordedTraffic::default());
        }
        let steered = self.list(address).await?;
        let recording = self.recording_among(&steered).await?;
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
        assert!(
            matches!(
                traffic.refresh(address, true).await,
                Err(LocalTrafficError::Docker {
                    kind: WorkloadKind::Deployment,
                    ..
                })
            ),
            "a failed Docker list is a Docker error"
        );
        assert!(
            traffic.refresh(address, false).await.is_err(),
            "an ordinary refresh lists again (and fails while Docker does)"
        );
    }

    const NETWORK: &str = "temps-network";

    /// While steered containers were last seen, a refresh lists them without
    /// asking whether any environment records (a database it cannot reach
    /// does not stop it); with none seen, it asks, and lists only if one
    /// does.
    #[tokio::test]
    async fn a_refresh_asks_whether_anything_records_only_when_nothing_is_steered() {
        use crate::services::egress_recording::tests::{fixture, PRODUCTION};
        use sea_orm::ConnectionTrait;

        let Some(f) = fixture().await else { return };
        let installation = InstallationId::from_bytes([1; 8]);
        let address: SocketAddr = "172.18.0.1:13128".parse().unwrap();
        let steered = steered_label(installation, address);
        let traffic = DeploymentTraffic::new(
            f.db.connection_arc(),
            bollard::Docker::connect_with_local_defaults().expect("docker client"),
            NETWORK.into(),
            installation,
        );
        traffic.scripted.lock().unwrap().push_back(vec![container(
            &[
                (STEERED_LABEL, steered.as_str()),
                (ENVIRONMENT_LABEL, &PRODUCTION.to_string()),
            ],
            &[(NETWORK, "172.18.0.5")],
        )]);
        // Seen: listed whatever any_recording would say.
        let listed = traffic.refresh(address, false).await.expect("listed");
        assert_eq!(listed.policies.len(), 1);

        // Nothing seen and nothing records: no list (the script is not
        // touched), an empty answer.
        traffic.scripted.lock().unwrap().push_back(vec![]);
        traffic
            .refresh(address, false)
            .await
            .expect("listed, empty");
        assert!(!traffic.steered_seen.load(Ordering::Relaxed));
        traffic.scripted.lock().unwrap().push_back(vec![]);
        let idle = traffic.refresh(address, false).await.expect("idle");
        assert!(idle.policies.is_empty());
        assert_eq!(
            traffic.scripted.lock().unwrap().len(),
            1,
            "an idle host with nothing recording does not list"
        );

        // An environment records: listed again.
        f.db.connection()
            .execute_unprepared(&format!(
                "UPDATE environments SET egress_recording = true WHERE id = {PRODUCTION}"
            ))
            .await
            .unwrap();
        traffic.refresh(address, false).await.expect("listed");
        assert!(traffic.scripted.lock().unwrap().is_empty());
    }

    /// An address Docker frees and gives to another container stops
    /// speaking for the old environment at the very next refresh, after a
    /// list that succeeded: to another environment's steered replica it
    /// speaks for that one, to an unsteered container for nobody.
    #[tokio::test]
    async fn a_reassigned_address_follows_the_next_successful_list() {
        use crate::services::egress_recording::tests::{fixture, PRODUCTION, STAGING};

        let Some(f) = fixture().await else { return };
        let installation = InstallationId::from_bytes([1; 8]);
        let address: SocketAddr = "172.18.0.1:13128".parse().unwrap();
        let steered = steered_label(installation, address);
        let replica = |environment: i32| {
            container(
                &[
                    (STEERED_LABEL, steered.as_str()),
                    (ENVIRONMENT_LABEL, &environment.to_string()),
                ],
                &[(NETWORK, "172.18.0.5")],
            )
        };
        let traffic = DeploymentTraffic::new(
            f.db.connection_arc(),
            bollard::Docker::connect_with_local_defaults().expect("docker client"),
            NETWORK.into(),
            installation,
        );
        traffic.scripted.lock().unwrap().extend([
            vec![replica(PRODUCTION)],
            vec![replica(STAGING)],
            // Given to an unsteered container: Docker's filter returns none.
            vec![],
        ]);
        let owner = |traffic: &RecordedTraffic| {
            temps_workload_netpol::EgressTable::build(&traffic.policies, &traffic.identities)
                .lookup("172.18.0.5".parse().unwrap())
                .map(|rules| rules.workload)
        };

        let first = traffic.refresh(address, false).await.expect("first list");
        assert_eq!(owner(&first), Some(super::super::workload(PRODUCTION)));
        let second = traffic.refresh(address, false).await.expect("second list");
        assert_eq!(
            owner(&second),
            Some(super::super::workload(STAGING)),
            "the new holder of the address, not the old"
        );
        let third = traffic.refresh(address, true).await.expect("third list");
        assert_eq!(owner(&third), None, "an unsteered holder speaks for nobody");
    }

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

    /// On an IPv6-only network a replica's IPv4 field is empty and it calls
    /// from its global IPv6 address; on a dual-stack one it may call from
    /// either. Both speak for its environment.
    #[test]
    fn a_steered_container_speaks_from_its_ipv6_address_too() {
        let ours = InstallationId::from_bytes([1; 8]);
        let label = temps_workload_egress::steering::steered_label(
            ours,
            "[fd00::1]:13128".parse().unwrap(),
        );
        let endpoint = |v4: Option<&str>, v6: Option<&str>| {
            let mut summary = container(
                &[(STEERED_LABEL, label.as_str()), (ENVIRONMENT_LABEL, "7")],
                &[(NETWORK, "")],
            );
            let networks = summary
                .network_settings
                .as_mut()
                .and_then(|settings| settings.networks.as_mut())
                .unwrap();
            let settings = networks.get_mut(NETWORK).unwrap();
            settings.ip_address = v4.map(str::to_string);
            settings.global_ipv6_address = v6.map(str::to_string);
            summary
        };
        let only_v6 = [endpoint(Some(""), Some("fd00::5"))];
        assert_eq!(
            steered_containers(&only_v6, NETWORK, ours),
            vec![SteeredContainer {
                environment_id: 7,
                address: "fd00::5".parse().unwrap(),
            }]
        );
        let dual = [endpoint(Some("172.18.0.5"), Some("fd00::5"))];
        assert_eq!(steered_containers(&dual, NETWORK, ours).len(), 2);
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

    /// A failed database read reaches the recorder as a database error,
    /// with its cause, never flattened into a message.
    #[tokio::test]
    async fn a_failed_database_read_keeps_its_cause() {
        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom("connection reset".to_string())])
            .into_connection();
        let traffic = DeploymentTraffic::new(
            Arc::new(db),
            bollard::Docker::connect_with_local_defaults().expect("docker client"),
            NETWORK.to_string(),
            InstallationId::from_bytes([1; 8]),
        );
        traffic.steered_seen.store(false, Ordering::Relaxed);
        let address: SocketAddr = "172.18.0.1:13128".parse().unwrap();
        match traffic.refresh(address, false).await {
            Err(LocalTrafficError::Database {
                kind: WorkloadKind::Deployment,
                operation,
                source: sea_orm::DbErr::Custom(message),
            }) => {
                assert_eq!(operation, "reading whether an environment records");
                assert_eq!(message, "connection reset");
            }
            other => panic!("expected a database error, got {other:?}"),
        }
    }

    /// Of the environments this host's steered containers belong to, those
    /// that record now are recorded, placed here or not yet (ingest decides
    /// placement); one that does not record, or is deleted, is not.
    #[tokio::test]
    async fn the_steered_environments_that_record_are_recorded() {
        use crate::services::egress_recording::tests::{fixture, ELSEWHERE, PRODUCTION, STAGING};
        use sea_orm::ConnectionTrait;

        let Some(f) = fixture().await else { return };
        let traffic = DeploymentTraffic::new(
            f.db.connection_arc(),
            bollard::Docker::connect_with_local_defaults().expect("docker client"),
            NETWORK.to_string(),
            InstallationId::from_bytes([1; 8]),
        );
        let steered = |ids: &[i32]| -> Vec<SteeredContainer> {
            ids.iter()
                .map(|id| SteeredContainer {
                    environment_id: *id,
                    address: "172.18.0.5".parse().unwrap(),
                })
                .collect()
        };
        let all = steered(&[PRODUCTION, STAGING, ELSEWHERE]);
        assert!(!traffic.any_recording().await.unwrap());
        assert!(traffic.recording_among(&all).await.unwrap().is_empty());
        assert!(
            traffic.recording_among(&[]).await.unwrap().is_empty(),
            "no container, no query"
        );
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
            traffic.recording_among(&all).await.unwrap(),
            BTreeSet::from([PRODUCTION, STAGING, ELSEWHERE]),
        );
        assert_eq!(
            traffic.recording_among(&steered(&[STAGING])).await.unwrap(),
            BTreeSet::from([STAGING]),
            "only the environments of the containers listed are read"
        );
        f.db.connection()
            .execute_unprepared(&format!(
                "UPDATE environments SET deleted_at = now() WHERE id = {STAGING}"
            ))
            .await
            .unwrap();
        assert_eq!(
            traffic.recording_among(&all).await.unwrap(),
            BTreeSet::from([PRODUCTION, ELSEWHERE]),
            "a deleted environment is not recorded"
        );
    }

    /// The first replica of an environment that records is steered before
    /// the database says where it runs. It must be in the table from its
    /// first connection, or a start-up that needs the internet is refused
    /// and never becomes ready; and it is recorded from that connection, so
    /// the calls that explain a failed start are kept.
    #[test]
    fn a_first_replica_is_recorded_before_it_is_placed() {
        let first = SteeredContainer {
            environment_id: 7,
            address: "172.18.0.5".parse().unwrap(),
        };
        // Its environment records: recorded from its first connection,
        // whatever the database places yet.
        let traffic = traffic_of(&[first], &BTreeSet::from([7]));
        let table =
            temps_workload_netpol::EgressTable::build(&traffic.policies, &traffic.identities);
        let rules = table
            .lookup(first.address)
            .expect("the new replica is in the table");
        assert!(rules.record, "recorded before it is placed");
        // Its environment does not record: in the table, let through.
        let traffic = traffic_of(&[first], &BTreeSet::new());
        let table =
            temps_workload_netpol::EgressTable::build(&traffic.policies, &traffic.identities);
        assert!(!table.lookup(first.address).expect("in the table").record);
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
