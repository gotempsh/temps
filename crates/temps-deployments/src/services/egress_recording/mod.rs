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
//!   its containers are started with the recorder as their HTTPS proxy
//!   (`temps_workload_egress::steering`), so it applies to containers
//!   started after it is turned on.

pub mod traffic;

#[cfg(test)]
pub(crate) mod tests;

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ColumnTrait, DatabaseConnection, EntityTrait, JoinType, PaginatorTrait, QueryFilter,
    QuerySelect, RelationTrait,
};
use temps_entities::environment_workload_isolation::{self, EnvironmentWorkloadIsolation};
use temps_entities::environment_workload_mode::EnvironmentWorkloadMode;
use temps_entities::preset::Preset;
use temps_entities::{deployment_containers, deployments, environments, projects};
use temps_workload::{WorkloadKind, WorkloadRef};
use temps_workload_egress::local::LocalEgressRecorder;
use temps_workload_egress::steering::{
    proxy_variables_with_image, url_hosts, InstallationId, ProxyVariables,
};
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
    /// HTTPS proxy, labelled as steered by this installation.
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
    /// `image` are its image's `ENV` entries that name internal hosts
    /// ([`temps_workload_egress::steering::image_host_variables`]): kept
    /// direct unless `existing` replaces them.
    pub fn variables(
        self,
        existing: &HashMap<String, String>,
        image: &HashMap<String, String>,
    ) -> Option<ProxyVariables> {
        let Self::Proxy(address, _) = self else {
            return None;
        };
        let platform = url_hosts(
            PLATFORM_URL_VARIABLES
                .iter()
                .filter_map(|name| existing.get(*name).map(String::as_str)),
        );
        Some(proxy_variables_with_image(
            address, existing, image, &platform,
        ))
    }
}

/// Whether `project` deploys as files rather than a running container: its
/// preset has nothing to build, or builds static output. What the planner
/// decides from; an unreadable preset is treated as a container.
fn serves_files(project: &projects::Model) -> bool {
    match temps_presets::get_preset_for_storage(project.preset, project.preset_config.as_ref()) {
        Ok(Some(preset)) => !preset.needs_container_build() || preset.static_output_dir().is_some(),
        _ => false,
    }
}

/// [`running_container`] for a status already read.
fn is_running_status(status: Option<&str>) -> bool {
    matches!(status, None | Some("running"))
}

/// A container row that runs, or has not said yet: not stopped, exited,
/// being removed or retired.
pub(crate) fn running_container() -> sea_orm::Condition {
    sea_orm::Condition::any()
        .add(deployment_containers::Column::Status.is_null())
        .add(deployment_containers::Column::Status.eq("running"))
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
    let current_deployments: Vec<i32> = rows
        .iter()
        .filter_map(|row| row.current_deployment_id)
        .collect();
    let placements: Vec<(i32, Option<i32>, Option<String>, i32)> =
        deployment_containers::Entity::find()
            .select_only()
            .column(deployments::Column::EnvironmentId)
            .column(deployment_containers::Column::NodeId)
            .column(deployment_containers::Column::Status)
            .column(deployment_containers::Column::DeploymentId)
            .distinct()
            .join(
                JoinType::InnerJoin,
                deployment_containers::Relation::Deployment.def(),
            )
            .filter(deployments::Column::EnvironmentId.is_in(rows.iter().map(|row| row.id)))
            .filter(deployment_containers::Column::DeletedAt.is_null())
            // Running containers, and those of each environment's current
            // deployment whatever their status: only a running replica places
            // the environment on a node (one stopped from the console neither
            // blocks nor allows recording on the node it no longer runs on),
            // and whether it runs images is said by a running one or by its
            // current deployment's (stopped from the console). Other stopped
            // rows (retained failed candidates, history) say nothing and are
            // never read, so this stays bounded by what runs now.
            .filter(
                sea_orm::Condition::any()
                    .add(running_container())
                    .add(deployment_containers::Column::DeploymentId.is_in(current_deployments)),
            )
            .into_tuple()
            .all(db)
            .await
            .map_err(EgressRecordingError::db(
                "reading where an environment's containers run",
            ))?;
    // A Compose stack's containers are started by Compose, not by the
    // deploy path that steers replicas to the recorder: nothing would ever
    // reach it, so such an environment cannot record at all.
    // A static site is published as files and served by the proxy: its
    // deployment starts no replica, so nothing is ever steered either. The
    // same preset checks the deployment planner makes.
    let mut compose: HashSet<i32> = HashSet::new();
    let mut static_sites: HashSet<i32> = HashSet::new();
    for project in projects::Entity::find()
        .filter(projects::Column::Id.is_in(rows.iter().map(|row| row.project_id)))
        .all(db)
        .await
        .map_err(EgressRecordingError::db(
            "reading how an environment is deployed",
        ))?
    {
        if project.preset == Preset::DockerCompose {
            compose.insert(project.id);
        } else if serves_files(&project) {
            static_sites.insert(project.id);
        }
    }
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
    let current: HashMap<i32, i32> = rows
        .iter()
        .filter_map(|row| row.current_deployment_id.map(|id| (row.id, id)))
        .collect();
    let mut runs_images: HashSet<i32> = HashSet::new();
    for (environment_id, node_id, status, deployment_id) in placements {
        let running = is_running_status(status.as_deref());
        if running || current.get(&environment_id) == Some(&deployment_id) {
            runs_images.insert(environment_id);
        }
        if !running {
            continue;
        }
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
            // A static project deployed as an image keeps its containers'
            // rows when they are stopped: only one whose current deployment
            // has none (and that runs none) is files alone.
            let files_only = !runs_images.contains(&row.id);
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
                } else if static_sites.contains(&row.project_id) && files_only {
                    // A static project can still be deployed as an image,
                    // which runs a replica like any other: only one whose
                    // current deployment has no container is files alone.
                    Some(
                        "a static site is published as files and served by the proxy: its \
                         deployments start no container to make outbound connections, so \
                         there is nothing to record. An environment deployed as a Docker \
                         image records"
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
    /// Held weakly: the recording service holds this adapter and the
    /// recorder holds the service, so a strong reference would keep the
    /// recorder (its tasks and its listener) alive for ever.
    recorder: OnceLock<std::sync::Weak<LocalEgressRecorder>>,
}

impl EnvironmentEgressAdapter {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self {
            db,
            recorder: OnceLock::new(),
        }
    }

    /// Wires the recorder a change refreshes. Set once.
    pub fn set_recorder(&self, recorder: &Arc<LocalEgressRecorder>) {
        let _ = self.recorder.set(Arc::downgrade(recorder));
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
    ) -> Result<bool, EgressRecordingError> {
        let id = i32::try_from(workload.id).map_err(|_| EgressRecordingError::NotFound {
            noun: NOUN,
            id: workload.id.to_string(),
        })?;
        // Only a live environment: one deleted since the caller resolved
        // it is not found, never reported as switched.
        let live = || {
            sea_orm::Condition::all()
                .add(environments::Column::Id.eq(id))
                .add(environments::Column::DeletedAt.is_null())
        };
        let result = environments::Entity::update_many()
            .col_expr(environments::Column::EgressRecording, Expr::value(enabled))
            .filter(live())
            .filter(environments::Column::EgressRecording.ne(enabled))
            .exec(self.db.as_ref())
            .await
            .map_err(EgressRecordingError::db(
                "updating an environment's egress recording",
            ))?;
        if result.rows_affected == 1 {
            return Ok(true);
        }
        // Nothing changed: already that value, or no longer there.
        let still = environments::Entity::find()
            .filter(live())
            .count(self.db.as_ref())
            .await
            .map_err(EgressRecordingError::db(
                "reading whether an environment is still live",
            ))?;
        if still == 0 {
            return Err(EgressRecordingError::NotFound {
                noun: NOUN,
                id: workload.id.to_string(),
            });
        }
        Ok(false)
    }

    /// Containers already going through the recorder start or stop being
    /// recorded at once; the rest follow when they are next deployed.
    /// Nothing to switch here: a container follows the opt-in when it is
    /// next deployed, which the status and the handler say.
    async fn changed(&self, _workload: &RecordedWorkload) -> Option<String> {
        if let Some(recorder) = self.recorder.get().and_then(std::sync::Weak::upgrade) {
            recorder.refresh().await;
        }
        None
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
