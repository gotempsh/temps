// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Durable coordination of identical builds through `identical_builds`.
//!
//! Every coordinated build registers a `waiting` row in its group
//! (`queue_key`) and then polls:
//!
//! 1. A `share` build first looks for a `succeeded` share build with the same
//!    fingerprint and access boundary that finished within the reuse window,
//!    and deploys its image instead of building.
//! 2. A `building` row whose heartbeat stopped (its process crashed or
//!    restarted) is marked `abandoned`, so it no longer blocks the group.
//! 3. The oldest live `waiting` row claims the group by switching itself to
//!    `building`. The partial unique index on `queue_key` where `state =
//!    'building'` makes the claim atomic across processes: a second claimer
//!    fails the index and keeps waiting.
//!
//! Waiting holds nothing but this row: no build slot on a worker (admission
//! happens when the build is sent, after the claim), no database connection
//! between polls and no lock. A waiting build whose deployment is cancelled
//! drops its future; [`WaitingGuard`] then marks the row `cancelled`. A
//! building lease ([`BuildLease`]) proves it is alive with a heartbeat task
//! and, dropped before it finished (its deployment was cancelled, for
//! instance because the project got a newer commit), marks itself
//! `cancelled`, so the next waiting build takes over instead of being
//! stranded. A failed build is marked `failed` and the next waiting build
//! builds itself: take-over, never a cascade of failures.
//!
//! Load: one indexed query per waiting build per `poll_interval`, one update
//! per building build per `heartbeat_interval`. Memory: nothing per group in
//! process memory; each waiting build is one sleeping task. Rows are pruned
//! seven days after they finish.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use sea_orm::{ConnectionTrait, DatabaseBackend, DbErr, QueryResult, SqlErr, Statement, Value};
use serde::{Deserialize, Serialize};
use temps_database::DbConnection;
use temps_entities::identical_build_mode::{
    IdenticalBuildMode, IdenticalBuildScope, IdenticalBuildState, MAX_WAIT_SECS, REUSE_WINDOW_SECS,
};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use super::{BuildIdentity, IdenticalBuildError};
use crate::jobs::build_image::BuiltImageConfig;

/// Finished rows older than this are deleted when their group next finishes
/// a build.
const PRUNE_AFTER: &str = "7 days";

/// How often and how long coordinated builds poll, prove they are alive and
/// wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoordinationTiming {
    /// How often a waiting build checks whether it may build or reuse.
    pub poll_interval: Duration,
    /// How often a building build refreshes its heartbeat.
    pub heartbeat_interval: Duration,
    /// A building row without a heartbeat for this long is taken over; a
    /// waiting row without one no longer holds its place in line.
    pub stale_after: Duration,
    /// The longest a build waits for its group, in total.
    pub max_wait: Duration,
    /// How long after it succeeded a share build's image is offered to
    /// identical builds that start later.
    pub reuse_window: Duration,
}

impl Default for CoordinationTiming {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(3),
            heartbeat_interval: Duration::from_secs(10),
            stale_after: Duration::from_secs(60),
            max_wait: Duration::from_secs(MAX_WAIT_SECS),
            reuse_window: Duration::from_secs(REUSE_WINDOW_SECS),
        }
    }
}

/// One build asking to take part in coordination.
#[derive(Debug, Clone)]
pub struct BuildTicket {
    pub deployment_id: i32,
    pub project_id: i32,
    pub job_id: String,
    pub mode: IdenticalBuildMode,
    pub scope: IdenticalBuildScope,
    pub identity: BuildIdentity,
    /// See [`access_boundary`].
    pub access_boundary: String,
    /// `owner/name`, for log lines.
    pub repository: String,
    pub commit_sha: String,
}

impl BuildTicket {
    /// The group this build queues in, prefixed with the scope so a source
    /// key never equals a fingerprint.
    pub fn queue_key(&self) -> String {
        match self.scope {
            IdenticalBuildScope::Fingerprint => {
                format!("fingerprint:{}", self.identity.fingerprint)
            }
            IdenticalBuildScope::Source => format!("source:{}", self.identity.source_key),
        }
    }
}

/// The image a succeeded share build produced: the build job outputs that
/// locate it, so a reusing deployment hands it to its nodes the way the
/// builder's own deployment does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedImage {
    pub image_tag: String,
    pub image_id: String,
    pub size_bytes: u64,
    /// The worker that holds the image; `None` for the control plane.
    #[serde(default)]
    pub builder_node_id: Option<i32>,
    #[serde(default)]
    pub image_tags_by_platform: HashMap<String, String>,
    #[serde(default)]
    pub builder_node_ids_by_platform: HashMap<String, i32>,
    #[serde(default)]
    pub image_ids_by_platform: HashMap<String, String>,
    #[serde(default)]
    pub image_config: Option<BuiltImageConfig>,
    #[serde(default)]
    pub dockerfile_preset: Option<String>,
    /// The directory the image was built from, relative to the checkout. A
    /// workspace build widens the configured context to the repository
    /// root, and the deploy job reads settings from where the build ran, so
    /// a reusing deployment resolves this in its own checkout. `None` when
    /// it could not be told; the reusing deployment then uses its own
    /// configured context.
    #[serde(default)]
    pub build_root: Option<String>,
    /// The Dockerfile the image was built from, relative to the checkout.
    #[serde(default)]
    pub dockerfile_path: Option<String>,
}

impl SharedImage {
    /// The worker nodes that hold this image (or one of its architectures);
    /// empty when the control plane holds it.
    pub fn holder_node_ids(&self) -> Vec<i32> {
        let mut ids: Vec<i32> = self
            .builder_node_id
            .into_iter()
            .chain(self.builder_node_ids_by_platform.values().copied())
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }
}

/// A build whose image this build deploys instead of building.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReusedBuild {
    /// The `identical_builds` row that built it.
    pub build_id: i32,
    pub deployment_id: i32,
    /// Whether the builder belongs to the same project.
    pub same_project: bool,
    pub image: SharedImage,
    pub repository: String,
    pub commit_sha: String,
}

/// The build a waiting build is waiting for, as far as its viewer may know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaderView {
    pub build_id: i32,
    /// `None` when the builder is in a project with a different access
    /// boundary: whoever reads this build's log may not see it.
    pub deployment_id: Option<i32>,
    pub same_project: bool,
    /// Whether this build will deploy the leader's image if it succeeds.
    pub will_share: bool,
}

/// What a waiting build reports to its log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionEvent {
    /// Waiting: for `leader` while it builds, or behind `ahead` older waiting
    /// builds when nothing builds right now.
    Waiting {
        leader: Option<LeaderView>,
        ahead: u64,
    },
    /// The build this one waited for ended without an image to share.
    LeaderEnded {
        leader: LeaderView,
        state: IdenticalBuildState,
    },
    /// An identical build's image can no longer be reached where it was
    /// built, so this build does not deploy it. Reuse requires the same
    /// access boundary, so the source may be named.
    ReuseUnavailable { deployment_id: i32, reason: String },
}

/// Receives [`AdmissionEvent`]s while a build waits, and answers what only
/// the build's own process can check.
#[async_trait]
pub trait AdmissionObserver: Send + Sync {
    async fn observe(&self, event: AdmissionEvent);

    /// Whether an image the control plane holds (no worker node in
    /// [`SharedImage::holder_node_ids`]) is still there, or why not. The
    /// coordinator checks worker-held images itself, against the nodes'
    /// status.
    async fn control_plane_image_missing(&self, _image: &SharedImage) -> Option<String> {
        None
    }
}

/// What a coordinated build does next.
#[derive(Debug)]
pub enum Admission {
    /// Build the image, holding the group until the lease is finished.
    Build(BuildLease),
    /// Deploy this image instead of building.
    Reuse(Box<ReusedBuild>),
}

/// Which teams may reach a project, as stored in
/// `identical_builds.access_boundary`: `unrestricted` for a project without
/// team grants, otherwise `teams:` and the sorted team ids. Two projects
/// only share images when this matches, so whoever can see one can see the
/// other.
pub async fn access_boundary<C: ConnectionTrait>(db: &C, project_id: i32) -> Result<String, DbErr> {
    let rows = db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT DISTINCT team_id FROM project_team_access WHERE project_id = $1 ORDER BY team_id",
            [Value::from(project_id)],
        ))
        .await?;
    let teams = rows
        .iter()
        .map(|row| row.try_get::<i32>("", "team_id"))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(boundary_label(&teams))
}

fn boundary_label(teams: &[i32]) -> String {
    if teams.is_empty() {
        "unrestricted".to_string()
    } else {
        let ids: Vec<String> = teams.iter().map(i32::to_string).collect();
        format!("teams:{}", ids.join(","))
    }
}

fn millis(duration: Duration) -> Value {
    Value::from(i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
}

fn statement(sql: &str, values: Vec<Value>) -> Statement {
    Statement::from_sql_and_values(DatabaseBackend::Postgres, sql, values)
}

/// Coordinates builds through the `identical_builds` table.
#[derive(Clone)]
pub struct IdenticalBuildCoordinator {
    db: Arc<DbConnection>,
    timing: CoordinationTiming,
}

impl std::fmt::Debug for IdenticalBuildCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdenticalBuildCoordinator")
            .field("timing", &self.timing)
            .finish()
    }
}

/// Marks the waiting row `cancelled` when the wait is abandoned (its
/// future dropped) before the row moved on.
struct WaitingGuard {
    db: Arc<DbConnection>,
    build_id: i32,
    deployment_id: i32,
    armed: bool,
}

impl WaitingGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for WaitingGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            // Without a runtime the row stays `waiting`; its heartbeat
            // stops, so it no longer holds anyone's place in line.
            return;
        };
        let db = self.db.clone();
        let (build_id, deployment_id) = (self.build_id, self.deployment_id);
        runtime.spawn(async move {
            let result = db
                .execute(statement(
                    "UPDATE identical_builds SET state = 'cancelled', \
                     detail = 'stopped waiting: the deployment was cancelled', \
                     finished_at = now(), updated_at = now() \
                     WHERE id = $1 AND state = 'waiting'",
                    vec![build_id.into()],
                ))
                .await;
            if let Err(error) = result {
                warn!(
                    build_id,
                    deployment_id,
                    %error,
                    "Could not mark a cancelled waiting build; it stops holding its place once its heartbeat is stale"
                );
            }
        });
    }
}

/// The right to build for a group. Finish it with [`BuildLease::succeed`] or
/// [`BuildLease::fail`]; dropping it unfinished marks the build `cancelled`
/// so the next waiting build takes over.
#[derive(Debug)]
pub struct BuildLease {
    db: Arc<DbConnection>,
    build_id: i32,
    deployment_id: i32,
    queue_key: String,
    heartbeat: Option<JoinHandle<()>>,
    finished: bool,
}

impl BuildLease {
    pub fn build_id(&self) -> i32 {
        self.build_id
    }

    /// The row was already recorded `reused`: stop the heartbeat without
    /// marking it cancelled.
    fn release_after_reuse(mut self) {
        self.finished = true;
    }

    /// The build succeeded. `image` is offered to identical share builds; a
    /// queue build passes `None`.
    pub async fn succeed(mut self, image: Option<&SharedImage>) -> Result<(), IdenticalBuildError> {
        let image = image
            .map(serde_json::to_value)
            .transpose()
            .map_err(|source| IdenticalBuildError::InvalidImage {
                build_id: self.build_id,
                deployment_id: self.deployment_id,
                source,
            })?;
        self.finish(IdenticalBuildState::Succeeded, None, image)
            .await
    }

    /// The build failed; the next waiting build builds instead.
    pub async fn fail(mut self, reason: &str) -> Result<(), IdenticalBuildError> {
        self.finish(IdenticalBuildState::Failed, Some(reason), None)
            .await
    }

    async fn finish(
        &mut self,
        state: IdenticalBuildState,
        detail: Option<&str>,
        image: Option<serde_json::Value>,
    ) -> Result<(), IdenticalBuildError> {
        self.finished = true;
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat.abort();
        }
        // A lease taken over as `abandoned` (its heartbeat stalled) still
        // records its outcome: the image it built is valid.
        self.db
            .execute(statement(
                "UPDATE identical_builds SET state = $2, detail = $3, image = $4, \
                 finished_at = now(), updated_at = now() \
                 WHERE id = $1 AND state IN ('building', 'abandoned')",
                vec![
                    self.build_id.into(),
                    state.as_str().into(),
                    detail.map(str::to_string).into(),
                    image.into(),
                ],
            ))
            .await
            .map_err(IdenticalBuildError::database(
                self.deployment_id,
                "record the outcome of",
            ))?;
        // Prune this group's history; a failure only leaves old rows behind.
        if let Err(error) = self
            .db
            .execute(statement(
                &format!(
                    "DELETE FROM identical_builds WHERE queue_key = $1 \
                     AND state NOT IN ('waiting', 'building') \
                     AND updated_at < now() - interval '{PRUNE_AFTER}'"
                ),
                vec![self.queue_key.clone().into()],
            ))
            .await
        {
            debug!(deployment_id = self.deployment_id, %error, "Could not prune old identical builds");
        }
        Ok(())
    }
}

impl Drop for BuildLease {
    fn drop(&mut self) {
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat.abort();
        }
        if self.finished {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            // The heartbeat stopped with the task; the row is taken over as
            // abandoned once it is stale.
            return;
        };
        let db = self.db.clone();
        let (build_id, deployment_id) = (self.build_id, self.deployment_id);
        runtime.spawn(async move {
            let result = db
                .execute(statement(
                    "UPDATE identical_builds SET state = 'cancelled', \
                     detail = 'the build stopped before it finished (the deployment was cancelled)', \
                     finished_at = now(), updated_at = now() \
                     WHERE id = $1 AND state = 'building'",
                    vec![build_id.into()],
                ))
                .await;
            match result {
                Ok(_) => info!(
                    build_id,
                    deployment_id,
                    "Identical build stopped before finishing; the next waiting build takes over"
                ),
                Err(error) => warn!(
                    build_id,
                    deployment_id,
                    %error,
                    "Could not release a cancelled build; waiting builds take over once its heartbeat is stale"
                ),
            }
        });
    }
}

impl IdenticalBuildCoordinator {
    pub fn new(db: Arc<DbConnection>) -> Self {
        Self {
            db,
            timing: CoordinationTiming::default(),
        }
    }

    pub fn with_timing(mut self, timing: CoordinationTiming) -> Self {
        self.timing = timing;
        self
    }

    pub fn timing(&self) -> CoordinationTiming {
        self.timing
    }

    /// Wait until `ticket` may build, or until an identical share build's
    /// image can be deployed instead. Returns [`IdenticalBuildError::WaitTimedOut`]
    /// after [`CoordinationTiming::max_wait`].
    pub async fn admit(
        &self,
        ticket: &BuildTicket,
        observer: &dyn AdmissionObserver,
    ) -> Result<Admission, IdenticalBuildError> {
        let queue_key = ticket.queue_key();
        let build_id = self.register(ticket, &queue_key).await?;
        let mut guard = WaitingGuard {
            db: self.db.clone(),
            build_id,
            deployment_id: ticket.deployment_id,
            armed: true,
        };
        let started = Instant::now();
        let mut followed: Option<LeaderView> = None;
        let mut announced = false;
        // Identical builds whose image turned out to be unreachable: not
        // offered again to this build.
        let mut unreachable: Vec<i32> = Vec::new();
        loop {
            if ticket.mode.shares_images() {
                if let Some(reused) = self
                    .find_reachable_reusable(ticket, observer, &mut unreachable)
                    .await?
                {
                    self.mark_reused(build_id, ticket, &reused).await?;
                    guard.disarm();
                    info!(
                        deployment_id = ticket.deployment_id,
                        source_deployment_id = reused.deployment_id,
                        "Reusing the image of an identical build"
                    );
                    return Ok(Admission::Reuse(Box::new(reused)));
                }
            }

            self.reap_stale(&queue_key, ticket.deployment_id).await?;
            if self.try_claim(build_id, &queue_key, ticket).await? {
                guard.disarm();
                // Holding the group now (the lease releases it on any error
                // below). The build this one waited for may have succeeded
                // between the reuse check above and the claim: look once
                // more, or this would build an image it could reuse.
                let lease = self.lease(build_id, ticket, queue_key);
                if ticket.mode.shares_images() {
                    if let Some(reused) = self
                        .find_reachable_reusable(ticket, observer, &mut unreachable)
                        .await?
                    {
                        self.mark_reused(build_id, ticket, &reused).await?;
                        lease.release_after_reuse();
                        info!(
                            deployment_id = ticket.deployment_id,
                            source_deployment_id = reused.deployment_id,
                            "Reusing the image of an identical build that finished as this one claimed its group"
                        );
                        return Ok(Admission::Reuse(Box::new(reused)));
                    }
                }
                if let Some(leader) = followed.take() {
                    self.report_end(&leader, observer, ticket).await?;
                }
                return Ok(Admission::Build(lease));
            }

            let leader = self.current_leader(&queue_key, ticket).await?;
            if let Some(previous) = &followed {
                if leader.as_ref().map(|l| l.build_id) != Some(previous.build_id) {
                    self.report_end(previous, observer, ticket).await?;
                }
            }
            let leader_changed =
                leader.as_ref().map(|l| l.build_id) != followed.as_ref().map(|l| l.build_id);
            if !announced || (leader_changed && leader.is_some()) {
                let ahead = self.waiting_ahead(build_id, &queue_key, ticket).await?;
                observer
                    .observe(AdmissionEvent::Waiting {
                        leader: leader.clone(),
                        ahead,
                    })
                    .await;
                announced = true;
            }
            followed = leader;

            let waited = started.elapsed();
            if waited >= self.timing.max_wait {
                self.mark_timed_out(build_id, ticket, waited).await?;
                guard.disarm();
                return Err(IdenticalBuildError::WaitTimedOut {
                    deployment_id: ticket.deployment_id,
                    waited_secs: waited.as_secs(),
                    limit_secs: self.timing.max_wait.as_secs(),
                });
            }
            self.touch(build_id, ticket).await?;
            tokio::time::sleep(self.timing.poll_interval.min(self.timing.max_wait - waited)).await;
        }
    }

    /// Insert this build's `waiting` row, or take over the row of an earlier
    /// attempt of the same job.
    async fn register(
        &self,
        ticket: &BuildTicket,
        queue_key: &str,
    ) -> Result<i32, IdenticalBuildError> {
        let row = self
            .db
            .query_one(statement(
                "INSERT INTO identical_builds (deployment_id, project_id, job_id, mode, \
                    queue_key, fingerprint, access_boundary, state, repository, commit_sha) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, 'waiting', $8, $9) \
                 ON CONFLICT (deployment_id, job_id) DO UPDATE SET \
                    project_id = EXCLUDED.project_id, mode = EXCLUDED.mode, \
                    queue_key = EXCLUDED.queue_key, fingerprint = EXCLUDED.fingerprint, \
                    access_boundary = EXCLUDED.access_boundary, state = 'waiting', \
                    source_build_id = NULL, image = NULL, \
                    repository = EXCLUDED.repository, commit_sha = EXCLUDED.commit_sha, \
                    detail = NULL, heartbeat_at = now(), updated_at = now(), finished_at = NULL \
                 RETURNING id",
                vec![
                    ticket.deployment_id.into(),
                    ticket.project_id.into(),
                    ticket.job_id.clone().into(),
                    ticket.mode.as_str().into(),
                    queue_key.into(),
                    ticket.identity.fingerprint.clone().into(),
                    ticket.access_boundary.clone().into(),
                    ticket.repository.clone().into(),
                    ticket.commit_sha.clone().into(),
                ],
            ))
            .await
            .map_err(IdenticalBuildError::database(
                ticket.deployment_id,
                "register",
            ))?
            .ok_or_else(|| IdenticalBuildError::Database {
                deployment_id: ticket.deployment_id,
                operation: "register",
                source: DbErr::RecordNotInserted,
            })?;
        row.try_get::<i32>("", "id")
            .map_err(IdenticalBuildError::database(
                ticket.deployment_id,
                "register",
            ))
    }

    /// The newest reusable image this build can actually reach, skipping
    /// (and reporting once) those whose holder is gone.
    async fn find_reachable_reusable(
        &self,
        ticket: &BuildTicket,
        observer: &dyn AdmissionObserver,
        unreachable: &mut Vec<i32>,
    ) -> Result<Option<ReusedBuild>, IdenticalBuildError> {
        for candidate in self.find_reusable(ticket).await? {
            if unreachable.contains(&candidate.build_id) {
                continue;
            }
            let missing = match candidate.image.holder_node_ids() {
                nodes if nodes.is_empty() => {
                    observer.control_plane_image_missing(&candidate.image).await
                }
                nodes => self.inactive_holders(ticket, &nodes).await?,
            };
            match missing {
                None => return Ok(Some(candidate)),
                Some(reason) => {
                    info!(
                        deployment_id = ticket.deployment_id,
                        source_deployment_id = candidate.deployment_id,
                        %reason,
                        "Not reusing an identical build's image: it cannot be reached"
                    );
                    unreachable.push(candidate.build_id);
                    observer
                        .observe(AdmissionEvent::ReuseUnavailable {
                            deployment_id: candidate.deployment_id,
                            reason,
                        })
                        .await;
                }
            }
        }
        Ok(None)
    }

    /// Why the worker nodes holding an image cannot hand it over, if they
    /// cannot: a node that is not `active` (offline, draining, removed) is
    /// not asked, the deploy job would fail on it.
    async fn inactive_holders(
        &self,
        ticket: &BuildTicket,
        nodes: &[i32],
    ) -> Result<Option<String>, IdenticalBuildError> {
        let placeholders: Vec<String> = (1..=nodes.len()).map(|i| format!("${i}")).collect();
        let rows = self
            .db
            .query_all(statement(
                &format!(
                    "SELECT id FROM nodes WHERE status = 'active' AND id IN ({})",
                    placeholders.join(", ")
                ),
                nodes.iter().map(|id| Value::from(*id)).collect(),
            ))
            .await
            .map_err(IdenticalBuildError::database(
                ticket.deployment_id,
                "check the nodes holding a reusable image for",
            ))?;
        let active = rows
            .iter()
            .map(|row| row.try_get::<i32>("", "id"))
            .collect::<Result<Vec<_>, _>>()
            .map_err(IdenticalBuildError::database(
                ticket.deployment_id,
                "check the nodes holding a reusable image for",
            ))?;
        let gone: Vec<String> = nodes
            .iter()
            .filter(|id| !active.contains(id))
            .map(i32::to_string)
            .collect();
        Ok((!gone.is_empty()).then(|| {
            format!(
                "the build node{} holding it ({}) {} not active",
                if gone.len() == 1 { "" } else { "s" },
                gone.join(", "),
                if gone.len() == 1 { "is" } else { "are" },
            )
        }))
    }

    /// Succeeded share builds this build may deploy, newest first. Both
    /// projects must still be in `share` mode, and both must still have the
    /// access boundary recorded for this build: team grants changed since
    /// either build registered make the image unavailable rather than
    /// letting it cross a boundary.
    async fn find_reusable(
        &self,
        ticket: &BuildTicket,
    ) -> Result<Vec<ReusedBuild>, IdenticalBuildError> {
        const CURRENT_BOUNDARY: &str =
            "(SELECT COALESCE('teams:' || string_agg(team_id::text, ',' \
            ORDER BY team_id), 'unrestricted') FROM (SELECT DISTINCT team_id \
            FROM project_team_access WHERE project_id = {project}) teams)";
        let rows = self
            .db
            .query_all(statement(
                &format!(
                    "SELECT b.id, b.deployment_id, b.project_id, b.image, b.repository, b.commit_sha \
                     FROM identical_builds b \
                     JOIN projects source ON source.id = b.project_id \
                     JOIN projects requester ON requester.id = $4 \
                     WHERE b.fingerprint = $1 AND b.access_boundary = $2 AND b.mode = 'share' \
                       AND b.state = 'succeeded' AND b.image IS NOT NULL \
                       AND b.finished_at > now() - ($3::bigint * interval '1 millisecond') \
                       AND source.identical_build_mode = 'share' AND NOT source.is_deleted \
                       AND requester.identical_build_mode = 'share' \
                       AND {source_boundary} = $2 AND {requester_boundary} = $2 \
                     ORDER BY b.finished_at DESC LIMIT 5",
                    source_boundary = CURRENT_BOUNDARY.replace("{project}", "b.project_id"),
                    requester_boundary = CURRENT_BOUNDARY.replace("{project}", "$4"),
                ),
                vec![
                    ticket.identity.fingerprint.clone().into(),
                    ticket.access_boundary.clone().into(),
                    millis(self.timing.reuse_window),
                    ticket.project_id.into(),
                ],
            ))
            .await
            .map_err(IdenticalBuildError::database(
                ticket.deployment_id,
                "look up a reusable image for",
            ))?;
        let read = |row: &QueryResult| -> Result<_, DbErr> {
            Ok((
                row.try_get::<i32>("", "id")?,
                row.try_get::<i32>("", "deployment_id")?,
                row.try_get::<i32>("", "project_id")?,
                row.try_get::<serde_json::Value>("", "image")?,
                row.try_get::<String>("", "repository")?,
                row.try_get::<String>("", "commit_sha")?,
            ))
        };
        let mut reusable = Vec::with_capacity(rows.len());
        for row in rows {
            let (build_id, deployment_id, project_id, image, repository, commit_sha) = read(&row)
                .map_err(
                IdenticalBuildError::database(ticket.deployment_id, "read the reusable image for"),
            )?;
            let image: SharedImage = match serde_json::from_value(image) {
                Ok(image) => image,
                Err(error) => {
                    // Written by a different version, or damaged: skip it.
                    warn!(
                        build_id,
                        deployment_id = ticket.deployment_id,
                        %error,
                        "Ignoring an identical build whose stored image cannot be read"
                    );
                    continue;
                }
            };
            reusable.push(ReusedBuild {
                build_id,
                deployment_id,
                same_project: project_id == ticket.project_id,
                image,
                repository,
                commit_sha,
            });
        }
        Ok(reusable)
    }

    async fn mark_reused(
        &self,
        build_id: i32,
        ticket: &BuildTicket,
        reused: &ReusedBuild,
    ) -> Result<(), IdenticalBuildError> {
        self.db
            .execute(statement(
                "UPDATE identical_builds SET state = 'reused', source_build_id = $2, \
                 finished_at = now(), updated_at = now() WHERE id = $1",
                vec![build_id.into(), reused.build_id.into()],
            ))
            .await
            .map_err(IdenticalBuildError::database(
                ticket.deployment_id,
                "record the reuse for",
            ))?;
        Ok(())
    }

    /// Mark building rows of the group whose heartbeat stopped `abandoned`.
    async fn reap_stale(
        &self,
        queue_key: &str,
        deployment_id: i32,
    ) -> Result<(), IdenticalBuildError> {
        let reaped = self
            .db
            .query_all(statement(
                "UPDATE identical_builds SET state = 'abandoned', \
                 detail = 'the build stopped reporting progress (its process stopped or restarted); another build took over', \
                 finished_at = now(), updated_at = now() \
                 WHERE queue_key = $1 AND state = 'building' \
                   AND heartbeat_at < now() - ($2::bigint * interval '1 millisecond') \
                 RETURNING id, deployment_id",
                vec![queue_key.into(), millis(self.timing.stale_after)],
            ))
            .await
            .map_err(IdenticalBuildError::database(
                deployment_id,
                "take over stalled builds for",
            ))?;
        for row in reaped {
            warn!(
                deployment_id,
                abandoned_build_id = row.try_get::<i32>("", "id").ok(),
                abandoned_deployment_id = row.try_get::<i32>("", "deployment_id").ok(),
                "An identical build stopped reporting progress; taking over its group"
            );
        }
        Ok(())
    }

    /// Switch this waiting row to `building` if nothing builds in the group
    /// and no older live build waits.
    async fn try_claim(
        &self,
        build_id: i32,
        queue_key: &str,
        ticket: &BuildTicket,
    ) -> Result<bool, IdenticalBuildError> {
        let result = self
            .db
            .execute(statement(
                "UPDATE identical_builds SET state = 'building', heartbeat_at = now(), \
                    updated_at = now() \
                 WHERE id = $1 AND state = 'waiting' \
                   AND NOT EXISTS (SELECT 1 FROM identical_builds b \
                        WHERE b.queue_key = $2 AND b.state = 'building') \
                   AND NOT EXISTS (SELECT 1 FROM identical_builds w \
                        WHERE w.queue_key = $2 AND w.state = 'waiting' AND w.id < $1 \
                          AND w.heartbeat_at > now() - ($3::bigint * interval '1 millisecond'))",
                vec![
                    build_id.into(),
                    queue_key.into(),
                    millis(self.timing.stale_after),
                ],
            ))
            .await;
        match result {
            Ok(result) => Ok(result.rows_affected() == 1),
            // Another process claimed the group between the check and the
            // write: the partial unique index refused this one.
            Err(error) if matches!(error.sql_err(), Some(SqlErr::UniqueConstraintViolation(_))) => {
                Ok(false)
            }
            Err(error) => Err(IdenticalBuildError::Database {
                deployment_id: ticket.deployment_id,
                operation: "claim the build group for",
                source: error,
            }),
        }
    }

    fn lease(&self, build_id: i32, ticket: &BuildTicket, queue_key: String) -> BuildLease {
        let db = self.db.clone();
        let interval = self.timing.heartbeat_interval;
        let deployment_id = ticket.deployment_id;
        let heartbeat = tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                match db
                    .execute(statement(
                        "UPDATE identical_builds SET heartbeat_at = now() \
                         WHERE id = $1 AND state = 'building'",
                        vec![build_id.into()],
                    ))
                    .await
                {
                    Ok(result) if result.rows_affected() == 0 => {
                        warn!(
                            build_id,
                            deployment_id,
                            "This build's group was taken over while it was still building; it finishes anyway"
                        );
                        return;
                    }
                    Ok(_) => {}
                    Err(error) => warn!(
                        build_id,
                        deployment_id,
                        %error,
                        "Could not refresh the heartbeat of an identical build"
                    ),
                }
            }
        });
        BuildLease {
            db: self.db.clone(),
            build_id,
            deployment_id,
            queue_key,
            heartbeat: Some(heartbeat),
            finished: false,
        }
    }

    async fn current_leader(
        &self,
        queue_key: &str,
        ticket: &BuildTicket,
    ) -> Result<Option<LeaderView>, IdenticalBuildError> {
        let row = self
            .db
            .query_one(statement(
                "SELECT id, deployment_id, project_id, mode, fingerprint, access_boundary \
                 FROM identical_builds WHERE queue_key = $1 AND state = 'building' LIMIT 1",
                vec![queue_key.into()],
            ))
            .await
            .map_err(IdenticalBuildError::database(
                ticket.deployment_id,
                "read the running build for",
            ))?;
        let Some(row) = row else {
            return Ok(None);
        };
        let read = |row: &QueryResult| -> Result<_, DbErr> {
            Ok((
                row.try_get::<i32>("", "id")?,
                row.try_get::<i32>("", "deployment_id")?,
                row.try_get::<i32>("", "project_id")?,
                row.try_get::<String>("", "mode")?,
                row.try_get::<String>("", "fingerprint")?,
                row.try_get::<String>("", "access_boundary")?,
            ))
        };
        let (build_id, deployment_id, project_id, mode, fingerprint, boundary) = read(&row)
            .map_err(IdenticalBuildError::database(
                ticket.deployment_id,
                "read the running build for",
            ))?;
        let visible = boundary == ticket.access_boundary;
        Ok(Some(LeaderView {
            build_id,
            deployment_id: visible.then_some(deployment_id),
            same_project: project_id == ticket.project_id,
            will_share: visible
                && ticket.mode.shares_images()
                && mode == IdenticalBuildMode::Share.as_str()
                && fingerprint == ticket.identity.fingerprint,
        }))
    }

    async fn waiting_ahead(
        &self,
        build_id: i32,
        queue_key: &str,
        ticket: &BuildTicket,
    ) -> Result<u64, IdenticalBuildError> {
        let row = self
            .db
            .query_one(statement(
                "SELECT count(*) AS ahead FROM identical_builds \
                 WHERE queue_key = $1 AND state = 'waiting' AND id < $2 \
                   AND heartbeat_at > now() - ($3::bigint * interval '1 millisecond')",
                vec![
                    queue_key.into(),
                    build_id.into(),
                    millis(self.timing.stale_after),
                ],
            ))
            .await
            .map_err(IdenticalBuildError::database(
                ticket.deployment_id,
                "count the builds ahead of",
            ))?;
        Ok(row
            .and_then(|row| row.try_get::<i64>("", "ahead").ok())
            .map_or(0, |ahead| u64::try_from(ahead).unwrap_or(0)))
    }

    /// Tell the observer how the build it followed ended, unless it
    /// succeeded (then this build either reused its image or builds its own
    /// different one, which the next event says).
    async fn report_end(
        &self,
        leader: &LeaderView,
        observer: &dyn AdmissionObserver,
        ticket: &BuildTicket,
    ) -> Result<(), IdenticalBuildError> {
        let row = self
            .db
            .query_one(statement(
                "SELECT state FROM identical_builds WHERE id = $1",
                vec![leader.build_id.into()],
            ))
            .await
            .map_err(IdenticalBuildError::database(
                ticket.deployment_id,
                "read how the awaited build ended for",
            ))?;
        let state = row
            .and_then(|row| row.try_get::<String>("", "state").ok())
            .and_then(|state| parse_state(&state));
        // A deleted row (its deployment was removed) ended as cancelled.
        let state = state.unwrap_or(IdenticalBuildState::Cancelled);
        if state != IdenticalBuildState::Succeeded {
            observer
                .observe(AdmissionEvent::LeaderEnded {
                    leader: leader.clone(),
                    state,
                })
                .await;
        }
        Ok(())
    }

    async fn touch(&self, build_id: i32, ticket: &BuildTicket) -> Result<(), IdenticalBuildError> {
        self.db
            .execute(statement(
                "UPDATE identical_builds SET heartbeat_at = now() WHERE id = $1 AND state = 'waiting'",
                vec![build_id.into()],
            ))
            .await
            .map_err(IdenticalBuildError::database(
                ticket.deployment_id,
                "refresh the place in line of",
            ))?;
        Ok(())
    }

    async fn mark_timed_out(
        &self,
        build_id: i32,
        ticket: &BuildTicket,
        waited: Duration,
    ) -> Result<(), IdenticalBuildError> {
        self.db
            .execute(statement(
                "UPDATE identical_builds SET state = 'timed_out', detail = $2, \
                 finished_at = now(), updated_at = now() WHERE id = $1 AND state = 'waiting'",
                vec![
                    build_id.into(),
                    format!("stopped waiting after {}s", waited.as_secs()).into(),
                ],
            ))
            .await
            .map_err(IdenticalBuildError::database(
                ticket.deployment_id,
                "record the timeout of",
            ))?;
        Ok(())
    }
}

fn parse_state(state: &str) -> Option<IdenticalBuildState> {
    use sea_orm::ActiveEnum;
    IdenticalBuildState::try_from_value(&state.to_string()).ok()
}

#[cfg(test)]
#[path = "coordinator_tests.rs"]
mod tests;
