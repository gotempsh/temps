// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Recording an environment's outbound connections: the environment side.
//!
//! Recording itself is not a deployment feature. The opt-in flow, the
//! ingest, the records, the activity query and the node-0 recorder are
//! written once, for every workload kind, in `temps-workload-egress`,
//! which sandboxes use too. This module is what is particular to an
//! environment's deployments:
//!
//! - **the adapter** ([`EnvironmentEgressAdapter`]): the opt-in is a column
//!   on the environment's own row (`environments.egress_recording`), so it
//!   survives redeploys. The workload is `deployment/<environment id>`,
//!   the identity network rules already give an environment. It always
//!   belongs to a project, and it runs on every node that hosts one of its
//!   containers;
//! - **who may see and change it** ([`EnvironmentEgress`]): whoever may
//!   read or write the project's environments, which the handlers check;
//! - **how its traffic reaches the recorder on this host** ([`traffic`]):
//!   its containers are started with the recorder as their HTTP(S) proxy
//!   (`temps_workload_egress::steering`), so it applies to containers
//!   started after it is turned on.

pub mod traffic;

#[cfg(test)]
pub(crate) mod tests;

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, JoinType, QueryFilter,
    QuerySelect, RelationTrait, Set,
};
use temps_entities::environment_workload_isolation::{self, EnvironmentWorkloadIsolation};
use temps_entities::environment_workload_mode::EnvironmentWorkloadMode;
use temps_entities::preset::Preset;
use temps_entities::{deployment_containers, deployments, environments, projects};
use temps_workload::{WorkloadKind, WorkloadRef};
use temps_workload_egress::local::LocalEgressRecorder;
use temps_workload_egress::steering::{proxy_variables, url_hosts, InstallationId, ProxyVariables};
use temps_workload_egress::{
    EgressActivityPage, EgressOutcomeFilter, EgressRecordingError, EgressRecordingService,
    EgressRecordingStatus, RecordedWorkload, WorkloadEgressAdapter, CONTROL_PLANE_NODE_ID,
};

/// The attribute every record of an environment carries with its id, which
/// the console's logs link filters on.
pub const ENVIRONMENT_ID_ATTRIBUTE: &str = "temps.environment.id";

/// What users call one, in messages.
const NOUN: &str = "environment";

/// The variables through which a deployment is told where the platform's
/// own endpoints are. Their hosts stay direct: telemetry an application
/// sends to its own Temps is not an outbound connection worth recording,
/// and it must work wherever that address resolves to.
const PLATFORM_URL_VARIABLES: [&str; 6] = [
    "TEMPS_API_URL",
    "SENTRY_DSN",
    "SENTRY_URL",
    "OTEL_EXPORTER_OTLP_ENDPOINT",
    "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
    "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
];

/// The identity of environment `id` as a workload.
pub fn workload(environment_id: i32) -> WorkloadRef {
    WorkloadRef::new(WorkloadKind::Deployment, i64::from(environment_id))
}

/// How a deployment's local replicas are steered to the recorder, decided
/// when its job is built.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RecordedEgress {
    /// The environment does not record.
    #[default]
    Off,
    /// The environment records, but this host's recorder is not serving
    /// deployments: the replicas start unrecorded, and the log says so.
    NotServing,
    /// Local replicas start with the recorder at this address as their
    /// HTTP(S) proxy, labelled as steered by this installation.
    Proxy(SocketAddr, InstallationId),
}

impl RecordedEgress {
    /// What `environment`'s next deployment does, given where this host's
    /// recorder serves deployments (if it does).
    pub fn plan(environment: &environments::Model, recorder: Option<&LocalEgressRecorder>) -> Self {
        if !environment.egress_recording {
            return Self::Off;
        }
        match recorder.and_then(|recorder| {
            recorder
                .address(WorkloadKind::Deployment)
                .map(|address| (address, recorder.installation()))
        }) {
            Some((address, installation)) => Self::Proxy(address, installation),
            None => Self::NotServing,
        }
    }

    /// The variables to add to a local replica's, when it is steered.
    pub fn variables(self, existing: &HashMap<String, String>) -> Option<ProxyVariables> {
        let Self::Proxy(address, _) = self else {
            return None;
        };
        let platform = url_hosts(
            PLATFORM_URL_VARIABLES
                .iter()
                .filter_map(|name| existing.get(*name).map(String::as_str)),
        );
        Some(proxy_variables(address, existing, &platform))
    }
}

/// The environments in `rows` as the recording service sees workloads,
/// with the nodes their live containers are on (one query for all).
async fn recorded(
    db: &DatabaseConnection,
    rows: &[environments::Model],
) -> Result<Vec<RecordedWorkload>, EgressRecordingError> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let placements: Vec<(i32, Option<i32>)> = deployment_containers::Entity::find()
        .select_only()
        .column(deployments::Column::EnvironmentId)
        .column(deployment_containers::Column::NodeId)
        .distinct()
        .join(
            JoinType::InnerJoin,
            deployment_containers::Relation::Deployment.def(),
        )
        .filter(deployments::Column::EnvironmentId.is_in(rows.iter().map(|row| row.id)))
        .filter(deployment_containers::Column::DeletedAt.is_null())
        .into_tuple()
        .all(db)
        .await
        .map_err(EgressRecordingError::db(
            "reading where an environment's containers run",
        ))?;
    // A Compose stack's containers are started by Compose, not by the
    // deploy path that steers replicas to the recorder: nothing would ever
    // reach it, so such an environment cannot record at all.
    let compose: HashSet<i32> = projects::Entity::find()
        .select_only()
        .column(projects::Column::Id)
        .filter(projects::Column::Id.is_in(rows.iter().map(|row| row.project_id)))
        .filter(projects::Column::Preset.eq(Preset::DockerCompose))
        .into_tuple::<i32>()
        .all(db)
        .await
        .map_err(EgressRecordingError::db(
            "reading how an environment is deployed",
        ))?
        .into_iter()
        .collect();
    // Only `enforce` honours an environment's isolation, and a replica in a
    // microVM is never a container on the deployment network: the recorder
    // cannot tell its connections apart, so it is never steered and could
    // never record.
    let enforced: Vec<i32> = rows
        .iter()
        .filter(|row| row.workload_mode == EnvironmentWorkloadMode::Enforce)
        .map(|row| row.id)
        .collect();
    let isolated: HashMap<i32, EnvironmentWorkloadIsolation> = if enforced.is_empty() {
        HashMap::new()
    } else {
        environment_workload_isolation::Entity::find()
            .filter(environment_workload_isolation::Column::Id.is_in(enforced))
            .filter(
                environment_workload_isolation::Column::WorkloadIsolation
                    .ne(EnvironmentWorkloadIsolation::Process),
            )
            .all(db)
            .await
            .map_err(EgressRecordingError::db(
                "reading which environments run outside containers",
            ))?
            .into_iter()
            .map(|row| (row.id, row.workload_isolation))
            .collect()
    };
    let mut nodes: HashMap<i32, Vec<i32>> = HashMap::new();
    for (environment_id, node_id) in placements {
        let node_id = node_id.unwrap_or(CONTROL_PLANE_NODE_ID);
        let placed = nodes.entry(environment_id).or_default();
        if !placed.contains(&node_id) {
            placed.push(node_id);
        }
    }
    Ok(rows
        .iter()
        .map(|row| {
            let mut placed = nodes.remove(&row.id).unwrap_or_default();
            placed.sort_unstable();
            RecordedWorkload {
                workload: workload(row.id),
                public_id: row.id.to_string(),
                project_id: Some(row.project_id),
                nodes: placed,
                enabled: row.egress_recording,
                live: row.deleted_at.is_none(),
                unrecordable: if compose.contains(&row.project_id) {
                    Some(
                        "a Docker Compose environment's containers are started by Compose, \
                         which does not send them through the egress proxy; only image \
                         deployments record"
                            .to_string(),
                    )
                } else {
                    isolated.get(&row.id).map(|isolation| {
                        format!(
                            "the environment's replicas run with {} isolation (workload mode \
                             enforce), which the egress proxy cannot tell apart from the host; \
                             only container replicas record",
                            isolation.as_str()
                        )
                    })
                },
            }
        })
        .collect())
}

/// The environment kind's answers to the recording service.
pub struct EnvironmentEgressAdapter {
    db: Arc<DatabaseConnection>,
    /// Set once this host records its own deployments; absent, a change is
    /// only stored.
    recorder: OnceLock<Arc<LocalEgressRecorder>>,
}

impl EnvironmentEgressAdapter {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self {
            db,
            recorder: OnceLock::new(),
        }
    }

    /// Wires the recorder a change refreshes. Set once.
    pub fn set_recorder(&self, recorder: Arc<LocalEgressRecorder>) {
        let _ = self.recorder.set(recorder);
    }
}

#[async_trait]
impl WorkloadEgressAdapter for EnvironmentEgressAdapter {
    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Deployment
    }

    fn noun(&self) -> &'static str {
        NOUN
    }

    fn id_attribute(&self) -> &'static str {
        ENVIRONMENT_ID_ATTRIBUTE
    }

    async fn load(&self, ids: &[i64]) -> Result<Vec<RecordedWorkload>, EgressRecordingError> {
        // An id outside the column's range names no environment.
        let ids: Vec<i32> = ids
            .iter()
            .filter_map(|id| i32::try_from(*id).ok())
            .collect();
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = environments::Entity::find()
            .filter(environments::Column::Id.is_in(ids))
            .all(self.db.as_ref())
            .await
            .map_err(EgressRecordingError::db(
                "loading the environments named by a batch of egress records",
            ))?;
        recorded(self.db.as_ref(), &rows).await
    }

    async fn set_enabled(
        &self,
        workload: WorkloadRef,
        enabled: bool,
    ) -> Result<(), EgressRecordingError> {
        let id = i32::try_from(workload.id).map_err(|_| EgressRecordingError::NotFound {
            noun: NOUN,
            id: workload.id.to_string(),
        })?;
        environments::ActiveModel {
            id: Set(id),
            egress_recording: Set(enabled),
            ..Default::default()
        }
        .update(self.db.as_ref())
        .await
        .map_err(EgressRecordingError::db(
            "updating an environment's egress recording",
        ))?;
        Ok(())
    }

    /// Containers already going through the recorder start or stop being
    /// recorded at once; the rest follow when they are next deployed.
    async fn changed(&self, _workload: &RecordedWorkload) {
        if let Some(recorder) = self.recorder.get() {
            recorder.refresh().await;
        }
    }
}

/// An environment's recording as the project's members see and change it:
/// resolves the environment in its project, then asks the recording
/// service. The handlers have checked the caller's access to the project.
pub struct EnvironmentEgress {
    db: Arc<DatabaseConnection>,
    recording: Arc<EgressRecordingService>,
}

impl EnvironmentEgress {
    pub fn new(db: Arc<DatabaseConnection>, recording: Arc<EgressRecordingService>) -> Self {
        Self { db, recording }
    }

    /// Environment `environment_id` of `project_id`, or not found. An
    /// environment of another project is not found, so a project's access
    /// never reaches another project's environment.
    async fn in_project(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<RecordedWorkload, EgressRecordingError> {
        let not_found = || EgressRecordingError::NotFound {
            noun: NOUN,
            id: environment_id.to_string(),
        };
        let row = environments::Entity::find_by_id(environment_id)
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::DeletedAt.is_null())
            .one(self.db.as_ref())
            .await
            .map_err(EgressRecordingError::db("looking up an environment"))?
            .ok_or_else(not_found)?;
        recorded(self.db.as_ref(), &[row])
            .await?
            .pop()
            .ok_or_else(not_found)
    }

    /// Whether the environment records its connections, and whether it can.
    pub async fn status(
        &self,
        project_id: i32,
        environment_id: i32,
    ) -> Result<EgressRecordingStatus, EgressRecordingError> {
        let environment = self.in_project(project_id, environment_id).await?;
        self.recording.status(&environment).await
    }

    /// Turns recording on or off for the environment. Returns the value
    /// before and the status after, for the audit log.
    pub async fn set_enabled(
        &self,
        project_id: i32,
        environment_id: i32,
        enabled: bool,
    ) -> Result<(bool, EgressRecordingStatus), EgressRecordingError> {
        let environment = self.in_project(project_id, environment_id).await?;
        self.recording.set_enabled(&environment, enabled).await
    }

    /// A page of the environment's recorded outbound connections, newest
    /// first.
    pub async fn activity(
        &self,
        project_id: i32,
        environment_id: i32,
        page: Option<u64>,
        per_page: Option<u64>,
        outcome: Option<EgressOutcomeFilter>,
    ) -> Result<EgressActivityPage, EgressRecordingError> {
        let environment = self.in_project(project_id, environment_id).await?;
        self.recording
            .activity(&environment, page, per_page, outcome)
            .await
    }
}
