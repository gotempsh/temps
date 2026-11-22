// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Removing images built for projects that no longer exist.
//!
//! Deleting a project removes the images its builds recorded (see
//! `ProjectImageCleaner` in `temps-core`), but that is best-effort: a daemon
//! that was unreachable, a removal Docker refused, or a build that finished
//! during the deletion leaves an image behind after the `deployment_jobs`
//! rows that named it are gone, so the row-based retention pass can never
//! pick it again. This sweep finds those by their build labels instead (see
//! [`super::image_ownership`]).
//!
//! It works from the deleted projects, not from the daemon: a trigger on
//! `projects` records each deleted project's ID in `deleted_project_images`
//! in the same transaction as the delete. Each run takes the least recently
//! checked of those records and asks each runtime for **one deleted
//! project's** images at a time, labelled with this install's ID and that
//! project's ID. Docker cannot limit or page an image listing, so what it
//! answers is never more than one deleted project's leftovers, and a run
//! with no deleted projects asks no daemon anything. A record is removed
//! once no runtime holds an image labelled with the project, and kept
//! (checked again on a later night) while one does or a runtime could not
//! be asked.
//!
//! An image is removed only:
//!
//! - when it is labelled with **this install's** ID, so another Temps
//!   install sharing the host is never touched, and images built before the
//!   labels existed are never touched either;
//! - when its project has **no row** in this database (a record whose
//!   project ID has a row again, after a restored backup, is dropped);
//! - when **no other project** still deploys the image's tag or ID, checked
//!   for each project's images in one query;
//! - when its tags are exactly the one its build gave it (recorded as
//!   `sh.temps.image.tag`), so an image someone retagged for themselves
//!   stays;
//! - when **no container**, running or stopped, was created from the image,
//!   judged by image ID; removed by that ID and never forced.
//!
//! It never selects by tag text (#1373), and it removes at most a
//! configured number of images per runtime and run, oldest first.

use std::collections::{BTreeSet, HashMap, HashSet};

use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use temps_docker_util::image_removal::{
    ImageRemovalOutcome, ImageRemovalRequest, LabelledImages, ListCursor, OwnerGuard,
    MAX_IMAGE_REMOVAL_BATCH,
};

use super::image_ownership::{
    self, ImageOwnershipError, IMAGE_INSTANCE_LABEL, IMAGE_PROJECT_LABEL,
};
use super::image_references;

/// Only these labels of a listed image are kept.
const IMAGE_LABEL_PREFIX: &str = "sh.temps.image.";

/// Most deleted projects one run checks. The rest wait for later runs, least
/// recently checked first.
pub const MAX_PROJECTS_PER_RUN: usize = 50;

/// An image on a daemon labelled as built by this install, with its
/// `sh.temps.image.*` labels. A worker's listing also says whether a
/// container uses it (`in_use_by`).
pub type OwnedImage = temps_docker_util::LabelledImage;

/// Why an orphan sweep stopped. Nothing is removed after any of these.
#[derive(Debug, thiserror::Error)]
pub enum OrphanSweepError {
    #[error(transparent)]
    Ownership(#[from] ImageOwnershipError),

    #[error(
        "Failed to connect to the Docker daemon to list the images install {owner} built for \
         deleted project {project_id}: {source}"
    )]
    Connect {
        owner: String,
        project_id: i32,
        #[source]
        source: bollard::errors::Error,
    },

    #[error("Failed to list the images install {owner} built for deleted project {project_id}: {source}")]
    ListImages {
        owner: String,
        project_id: i32,
        /// Boxed: the Docker error is large and this variant is rare.
        #[source]
        source: Box<temps_docker_util::DockerUtilError>,
    },

    #[error("Failed to read the deleted projects whose images to sweep: {source}")]
    DeletedProjects {
        #[source]
        source: sea_orm::DbErr,
    },

    #[error("Failed to check which of {count} deleted projects have a row again: {source}")]
    ProjectLookup {
        count: usize,
        #[source]
        source: sea_orm::DbErr,
    },

    #[error("Failed to list the containers created from image {image_id}: {source}")]
    ContainerLookup {
        image_id: String,
        #[source]
        source: bollard::errors::Error,
    },

    #[error(
        "Failed to check which of {count} image references of deleted project {project_id} \
         other projects still name: {source}"
    )]
    ReferenceLookup {
        project_id: i32,
        count: usize,
        #[source]
        source: sea_orm::DbErr,
    },

    #[error("Failed to remove {count} images of deleted projects: {source}")]
    Removal {
        count: usize,
        /// Boxed: the Docker error is large and this variant is rare.
        #[source]
        source: Box<temps_docker_util::DockerUtilError>,
    },

    /// A worker node's agent failed while listing or removing its images.
    #[error("Failed while {operation} on worker node {node}: {source}")]
    Worker {
        node: String,
        operation: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// The worker nodes to sweep could not be read.
    #[error("Failed to read the worker nodes whose images to sweep: {source}")]
    Workers {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// What one run did on one runtime, over the deleted projects it checked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrphanSweepReport {
    /// Deleted projects whose images were looked for.
    pub projects: usize,
    /// Images of those projects listed.
    pub listed: usize,
    pub removed: usize,
    /// Kept: another project still names the image, its tags are not just
    /// the one its build gave it, a container uses it, or Docker refused.
    pub kept: usize,
    pub failed: usize,
    /// Images, and deleted projects not looked at, left for a later run by
    /// the per-run limit.
    pub deferred: usize,
}

/// What one runtime held for one deleted project, and what the sweep did
/// with it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectSweep {
    pub listed: usize,
    pub removed: usize,
    pub kept: usize,
    pub failed: usize,
    /// Images the per-run limit left for a later run.
    pub deferred: usize,
    /// Removals asked of the daemon, whatever it answered: what counts
    /// against the per-run limit.
    pub attempted: usize,
    /// The project's images still on the runtime after this run: listed or
    /// past the page, less those removed. Zero means none is left there.
    pub remaining: usize,
    /// Where the next run's listing of this project starts: after this page
    /// when it was handled whole and more images follow; at the same page
    /// when the limit deferred images on it and this run removed some (so
    /// retrying makes progress); `None` to start at the oldest.
    pub next: Option<ListCursor>,
}

/// The daemon operations the sweep needs: this host's daemon, or a worker
/// node's through its agent. Mockable in tests.
#[async_trait::async_trait]
pub trait OwnedImageDaemon: Send + Sync {
    /// Images labelled `sh.temps.image.instance=<owner>` and
    /// `sh.temps.image.project_id=<project_id>` after `after`, oldest first
    /// and bounded by
    /// [`temps_docker_util::image_removal::MAX_LABELLED_IMAGES`], with where
    /// the next page starts.
    async fn list_project_images(
        &self,
        owner: &str,
        project_id: i32,
        after: Option<&ListCursor>,
    ) -> Result<LabelledImages, OrphanSweepError>;

    /// One container, running or stopped, created from `image`, if any.
    async fn container_using_image(
        &self,
        image: &OwnedImage,
    ) -> Result<Option<String>, OrphanSweepError>;

    /// Remove each image no container uses (see
    /// [`temps_docker_util::remove_unused_images`]).
    async fn remove_unused_images(
        &self,
        requests: &[ImageRemovalRequest],
    ) -> Result<Vec<ImageRemovalOutcome>, OrphanSweepError>;
}

/// A worker node whose images the nightly sweep also covers.
pub struct WorkerImageDaemon {
    pub node_id: i32,
    pub node_name: String,
    pub daemon: std::sync::Arc<dyn OwnedImageDaemon>,
}

/// The worker nodes one run covers, and those it could not.
#[derive(Default)]
pub struct WorkerImageDaemonSet {
    pub reachable: Vec<WorkerImageDaemon>,
    /// Worker nodes that may hold images but cannot be asked now (offline,
    /// or failing to connect). A deleted project's record is kept until
    /// every node has been checked, so their images are swept once they are
    /// back.
    pub unreachable: Vec<i32>,
}

/// The worker nodes the nightly sweep covers, implemented where nodes are
/// known (`temps-deployments`). A worker's own housekeeping has no database,
/// so it cannot tell which projects still exist: the control plane does it.
#[async_trait::async_trait]
pub trait WorkerImageDaemons: Send + Sync {
    /// The worker nodes, split into those that can be asked now and those
    /// that cannot (each logged).
    async fn worker_image_daemons(&self) -> Result<WorkerImageDaemonSet, OrphanSweepError>;
}

#[async_trait::async_trait]
impl OwnedImageDaemon for super::DefaultDockerClient {
    async fn list_project_images(
        &self,
        owner: &str,
        project_id: i32,
        after: Option<&ListCursor>,
    ) -> Result<LabelledImages, OrphanSweepError> {
        let docker = bollard::Docker::connect_with_unix_defaults().map_err(|source| {
            OrphanSweepError::Connect {
                owner: owner.to_string(),
                project_id,
                source,
            }
        })?;
        let project = project_id.to_string();
        let listing = temps_docker_util::list_labelled_images(
            &docker,
            &[
                (IMAGE_INSTANCE_LABEL, owner),
                (IMAGE_PROJECT_LABEL, project.as_str()),
            ],
            IMAGE_LABEL_PREFIX,
            false,
            after,
        )
        .await
        .map_err(|source| OrphanSweepError::ListImages {
            owner: owner.to_string(),
            project_id,
            source: Box::new(source),
        })?;
        if listing.unlisted > 0 {
            tracing::info!(
                project_id,
                listed = listing.images.len(),
                unlisted = listing.unlisted,
                "More images of a deleted project than one sweep lists; the next run continues after these"
            );
        }
        Ok(listing)
    }

    async fn container_using_image(
        &self,
        image: &OwnedImage,
    ) -> Result<Option<String>, OrphanSweepError> {
        let in_use_error = |source| OrphanSweepError::ContainerLookup {
            image_id: image.image_id.clone(),
            source,
        };
        let docker = bollard::Docker::connect_with_unix_defaults().map_err(in_use_error)?;
        temps_docker_util::image_removal::container_using_image(&docker, &image.image_id)
            .await
            .map_err(in_use_error)
    }

    async fn remove_unused_images(
        &self,
        requests: &[ImageRemovalRequest],
    ) -> Result<Vec<ImageRemovalOutcome>, OrphanSweepError> {
        let removal_error = |source| OrphanSweepError::Removal {
            count: requests.len(),
            source: Box::new(source),
        };
        let docker = bollard::Docker::connect_with_unix_defaults().map_err(|source| {
            removal_error(temps_docker_util::DockerUtilError::ImageInventory {
                operation: "connect to the Docker daemon",
                source,
            })
        })?;
        temps_docker_util::remove_unused_images(&docker, requests)
            .await
            .map_err(removal_error)
    }
}

/// A deleted project waiting for its images to be swept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedProject {
    pub project_id: i32,
    /// When the row was deleted: a record is only removed or touched while
    /// it still names this deletion, so a project ID deleted again during a
    /// run keeps its fresh record.
    pub deleted_at: chrono::DateTime<chrono::Utc>,
}

/// Up to `limit` deleted projects, least recently checked first.
async fn deleted_projects<C: ConnectionTrait>(
    db: &C,
    limit: usize,
) -> Result<Vec<DeletedProject>, OrphanSweepError> {
    let read_error = |source| OrphanSweepError::DeletedProjects { source };
    let rows = db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT project_id, deleted_at FROM deleted_project_images \
             ORDER BY checked_at ASC NULLS FIRST, project_id LIMIT $1",
            [i64::try_from(limit).unwrap_or(i64::MAX).into()],
        ))
        .await
        .map_err(read_error)?;
    rows.into_iter()
        .map(|row| {
            Ok(DeletedProject {
                project_id: row.try_get("", "project_id").map_err(read_error)?,
                deleted_at: row.try_get("", "deleted_at").map_err(read_error)?,
            })
        })
        .collect()
}

/// Which of `project_ids` have a row, in one query by primary key.
async fn existing_project_ids<C: ConnectionTrait>(
    db: &C,
    project_ids: &BTreeSet<i32>,
) -> Result<HashSet<i32>, OrphanSweepError> {
    let lookup_error = |source| OrphanSweepError::ProjectLookup {
        count: project_ids.len(),
        source,
    };
    let ids: Vec<i32> = project_ids.iter().copied().collect();
    let rows = db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id FROM projects WHERE id = ANY($1)",
            [ids.into()],
        ))
        .await
        .map_err(lookup_error)?;
    rows.into_iter()
        .map(|row| row.try_get::<i32>("", "id").map_err(lookup_error))
        .collect()
}

/// Remove the `retire` records, and note the `touch` records were checked
/// so other records go first next run: one statement each, whatever the
/// number of records. A record is only matched while it still names the
/// deletion that was read. A failure is logged: the records are then
/// checked again, which is harmless.
async fn settle_records<C: ConnectionTrait>(
    db: &C,
    retire: &[DeletedProject],
    touch: &[DeletedProject],
) {
    const RETIRE: &str = "DELETE FROM deleted_project_images d \
         USING unnest($1::int[], $2::timestamptz[]) AS r(project_id, deleted_at) \
         WHERE d.project_id = r.project_id AND d.deleted_at = r.deleted_at";
    const TOUCH: &str = "UPDATE deleted_project_images d SET checked_at = now() \
         FROM unnest($1::int[], $2::timestamptz[]) AS r(project_id, deleted_at) \
         WHERE d.project_id = r.project_id AND d.deleted_at = r.deleted_at";
    for (records, sql, retiring) in [(retire, RETIRE, true), (touch, TOUCH, false)] {
        if records.is_empty() {
            continue;
        }
        let ids: Vec<i32> = records.iter().map(|record| record.project_id).collect();
        let deleted_at: Vec<chrono::DateTime<chrono::Utc>> =
            records.iter().map(|record| record.deleted_at).collect();
        if let Err(error) = db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                [ids.clone().into(), deleted_at.into()],
            ))
            .await
        {
            tracing::warn!(
                project_ids = ?ids,
                retiring,
                %error,
                "Could not update the records of deleted projects' images; they are checked again next run"
            );
        }
    }
}

/// The removal request for an orphan: by its one tag, expecting its ID, or
/// by ID if it has no tag. `None` when its tags are not exactly the one the
/// build gave it (`sh.temps.image.tag`): a second tag, or one that replaced
/// the build's, is an operator's retag, so the image is theirs. An image
/// built without that label is kept too: its one tag proves nothing.
fn removal_request(image: &OwnedImage, owner: &str) -> Option<ImageRemovalRequest> {
    // The sweep only lists images labelled with this owner: no unlabelled
    // image ever reaches it.
    let guard = OwnerGuard {
        label: IMAGE_INSTANCE_LABEL.to_string(),
        owner: Some(owner.to_string()),
        keep_unlabelled_when_shared: false,
    };
    let request = match image.tags.as_slice() {
        [] => Some(ImageRemovalRequest::new(
            image.image_id.clone(),
            Some(image.image_id.clone()),
        )),
        [tag] => {
            let built = image.labels.get(image_ownership::IMAGE_TAG_LABEL)?;
            image_references::tag_spellings(built)
                .contains(tag)
                .then(|| ImageRemovalRequest::new(tag.clone(), Some(image.image_id.clone())))
        }
        _ => None,
    };
    request.map(|request| request.with_owner_guard(guard))
}

fn log_kept(image: &OwnedImage, reason: &str) {
    tracing::warn!(
        image_id = %image.image_id,
        tags = ?image.tags,
        project_id = ?image_ownership::labelled_project_id(&image.labels),
        reason = %reason,
        "Kept image built for a project that no longer exists"
    );
}

/// Remove up to `max_removals` of the images this install built for the
/// deleted project `project_id` on one runtime, oldest first. See the
/// module documentation for the safety rules; the caller has checked that
/// the project has no row.
///
/// Images that must stay (named by another project, retagged, used by a
/// container) are set aside **before** the limit is applied, so a backlog
/// of held images can never fill every night's batch and starve images
/// that could go.
pub async fn sweep_deleted_project_images<C: ConnectionTrait>(
    db: &C,
    daemon: &dyn OwnedImageDaemon,
    owner: &str,
    project_id: i32,
    after: Option<&ListCursor>,
    max_removals: usize,
) -> Result<ProjectSweep, OrphanSweepError> {
    let listing = daemon.list_project_images(owner, project_id, after).await?;
    // The daemon filtered by the labels; this only guards against one that
    // did not.
    let mut images: Vec<&OwnedImage> = listing
        .images
        .iter()
        .filter(|image| image_ownership::labelled_project_id(&image.labels) == Some(project_id))
        .collect();
    images.sort_by(|a, b| {
        a.created
            .cmp(&b.created)
            .then_with(|| a.image_id.cmp(&b.image_id))
    });
    let mut sweep = ProjectSweep {
        listed: images.len(),
        remaining: images.len() + listing.unlisted,
        next: listing.next.clone(),
        ..Default::default()
    };
    if images.is_empty() {
        return Ok(sweep);
    }

    // The project is gone, but another project may still deploy the same
    // image: any deployment naming its tags or ID keeps it.
    let mut candidates = Vec::new();
    for image in &images {
        for tag in &image.tags {
            candidates.extend(image_references::tag_spellings(tag));
        }
        candidates.extend(image_references::image_id_spellings(&image.image_id));
    }
    let named = image_references::referenced_images(db, &candidates, None)
        .await
        .map_err(|source| OrphanSweepError::ReferenceLookup {
            project_id,
            count: candidates.len(),
            source,
        })?;

    let limit = max_removals.min(MAX_IMAGE_REMOVAL_BATCH);
    let mut selected: Vec<(&OwnedImage, ImageRemovalRequest)> = Vec::new();
    for (index, image) in images.iter().enumerate() {
        if selected.len() == limit {
            sweep.deferred = images.len() - index;
            break;
        }
        let named_elsewhere = image
            .tags
            .iter()
            .any(|tag| image_references::names_tag(&named, tag))
            || image_references::names_image_id(&named, &image.image_id);
        if named_elsewhere {
            sweep.kept += 1;
            log_kept(image, "another project's deployment names it");
            continue;
        }
        let Some(request) = removal_request(image, owner) else {
            sweep.kept += 1;
            log_kept(image, "its tags are not just the one its build gave it");
            continue;
        };
        match daemon.container_using_image(image).await {
            Ok(None) => selected.push((image, request)),
            Ok(Some(container_id)) => {
                sweep.kept += 1;
                log_kept(
                    image,
                    &format!("container {container_id} was created from it"),
                );
            }
            Err(error) => {
                sweep.failed += 1;
                log_kept(image, &error.to_string());
            }
        }
    }
    if selected.is_empty() {
        return Ok(sweep);
    }

    let requests: Vec<ImageRemovalRequest> = selected
        .iter()
        .map(|(_, request)| request.clone())
        .collect();
    sweep.attempted = requests.len();
    let outcomes = daemon.remove_unused_images(&requests).await?;
    for (outcome, (image, _)) in outcomes.iter().zip(selected.iter()) {
        if outcome.status.is_removed() {
            sweep.removed += 1;
            tracing::info!(
                image_id = %image.image_id,
                tags = ?image.tags,
                project_id,
                "Removed image built for a project that no longer exists"
            );
        } else if let Some(reason) = outcome.status.reason() {
            if outcome.status.is_failed() {
                sweep.failed += 1;
            } else {
                sweep.kept += 1;
            }
            log_kept(image, &reason);
        }
    }
    sweep.remaining = sweep.remaining.saturating_sub(sweep.removed);
    // Images the limit deferred on this page are retried next run, by
    // listing this page again rather than moving on. Only when this run
    // removed something: each retry then shrinks the page, while a batch the
    // daemon refuses every time (dependent child images, a failing node)
    // must not pin the listing to this page for good; it then moves on and
    // comes back after reaching the newest image.
    if sweep.deferred > 0 && sweep.removed > 0 {
        sweep.next = after.cloned();
    }
    Ok(sweep)
}

/// How far one run got with a deleted project, over every runtime.
#[derive(Debug, Clone, Copy, Default)]
struct ProjectProgress {
    /// A runtime may still hold one of its images: not seen as clean.
    left_behind: bool,
    /// A runtime never got to it in this run.
    passed_over: bool,
}

/// Note that a runtime never got to `projects` in this run.
fn pass_over(progress: &mut HashMap<i32, ProjectProgress>, projects: &[DeletedProject]) {
    for project in projects {
        let state = progress.entry(project.project_id).or_default();
        state.left_behind = true;
        state.passed_over = true;
    }
}

/// Runs the sweep on this host's daemon and on every reachable worker node,
/// with this install's owner ID, for the deleted projects recorded in
/// `deleted_project_images`. Each runtime is swept on its own: one that
/// fails is logged and the others still run.
pub struct OrphanImageSweeper {
    db: std::sync::Arc<temps_database::DbConnection>,
    /// This host's daemon; `None` in a process that runs no workloads.
    local: Option<std::sync::Arc<dyn OwnedImageDaemon>>,
    workers: Option<std::sync::Arc<dyn WorkerImageDaemons>>,
    /// The install's data directory, which holds its half of the owner ID.
    /// Without it nothing is swept: no image could be proved ours.
    data_dir: Option<std::path::PathBuf>,
    max_per_run: usize,
    /// Where the next listing of a deleted project starts on a runtime
    /// (`None` is this host). Only set while a project has more images there
    /// than one page; kept in memory: after a restart the listing starts at
    /// the oldest again, which only costs one more pass.
    cursors: std::sync::Mutex<HashMap<(Option<i32>, i32), ListCursor>>,
}

/// What one run of [`OrphanImageSweeper`] did.
#[derive(Debug, Default)]
pub struct OrphanSweepSummary {
    /// Per runtime: `None` is this host, `Some(node_id)` a worker node.
    pub swept: Vec<(Option<i32>, OrphanSweepReport)>,
    /// Runtimes whose sweep failed, and failures that stopped the run.
    pub failed: Vec<(Option<i32>, String)>,
    /// Deleted projects no runtime holds an image of any more: their
    /// records were removed.
    pub finished: Vec<i32>,
    /// Deleted projects checked again on a later run.
    pub pending: Vec<i32>,
}

impl OrphanImageSweeper {
    /// Sweeps this host's daemon only, up to 200 images per runtime and run.
    pub fn new(db: std::sync::Arc<temps_database::DbConnection>) -> Self {
        Self {
            db,
            local: Some(std::sync::Arc::new(super::DefaultDockerClient)),
            workers: None,
            data_dir: None,
            max_per_run: 200,
            cursors: Default::default(),
        }
    }

    fn cursors(&self) -> std::sync::MutexGuard<'_, HashMap<(Option<i32>, i32), ListCursor>> {
        // A plain map replaced entry by entry: a poisoned lock still holds a
        // usable one.
        self.cursors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn with_local_daemon(
        mut self,
        local: Option<std::sync::Arc<dyn OwnedImageDaemon>>,
    ) -> Self {
        self.local = local;
        self
    }

    pub fn with_worker_daemons(mut self, workers: std::sync::Arc<dyn WorkerImageDaemons>) -> Self {
        self.workers = Some(workers);
        self
    }

    pub fn with_data_dir(mut self, data_dir: std::path::PathBuf) -> Self {
        self.data_dir = Some(data_dir);
        self
    }

    pub fn with_max_per_run(mut self, max_per_run: usize) -> Self {
        self.max_per_run = max_per_run;
        self
    }

    /// Sweep the recorded deleted projects on every runtime once.
    pub async fn run(&self) -> OrphanSweepSummary {
        let mut summary = OrphanSweepSummary::default();
        let Some(data_dir) = &self.data_dir else {
            tracing::debug!(
                "No data directory configured; not sweeping images of deleted projects"
            );
            return summary;
        };
        let owner = match image_ownership::load_image_owner_id(self.db.as_ref(), data_dir).await {
            Ok(Some(owner)) => owner,
            // No build has labelled an image with this install's ID yet.
            Ok(None) => return summary,
            Err(error) => {
                tracing::error!(
                    %error,
                    "Could not read this install's image owner ID; not sweeping images of deleted projects"
                );
                summary.failed.push((None, error.to_string()));
                return summary;
            }
        };
        let records = match deleted_projects(self.db.as_ref(), MAX_PROJECTS_PER_RUN).await {
            Ok(records) => records,
            Err(error) => {
                tracing::error!(%error, "Could not read the deleted projects; not sweeping their images");
                summary.failed.push((None, error.to_string()));
                return summary;
            }
        };
        if records.is_empty() {
            tracing::debug!("No deleted projects whose images to sweep");
            return summary;
        }
        // A project ID with a row again (a restored backup) is a live
        // project: its images are not orphans, and its record goes.
        let ids: BTreeSet<i32> = records.iter().map(|record| record.project_id).collect();
        let existing = match existing_project_ids(self.db.as_ref(), &ids).await {
            Ok(existing) => existing,
            Err(error) => {
                tracing::error!(%error, "Could not check the deleted projects; not sweeping their images");
                summary.failed.push((None, error.to_string()));
                return summary;
            }
        };
        let mut retire = Vec::new();
        let mut pending = Vec::with_capacity(records.len());
        for record in records {
            if existing.contains(&record.project_id) {
                retire.push(record);
            } else {
                pending.push(record);
            }
        }
        if pending.is_empty() {
            settle_records(self.db.as_ref(), &retire, &[]).await;
            return summary;
        }

        // A record is removed only when every runtime that may hold the
        // project's images was asked and holds none.
        let mut every_runtime_asked = true;
        let mut runtimes: Vec<(Option<i32>, String, std::sync::Arc<dyn OwnedImageDaemon>)> =
            Vec::new();
        if let Some(local) = &self.local {
            runtimes.push((None, "this host".to_string(), local.clone()));
        }
        if let Some(workers) = &self.workers {
            match workers.worker_image_daemons().await {
                Ok(set) => {
                    if !set.unreachable.is_empty() {
                        every_runtime_asked = false;
                        tracing::info!(
                            nodes = ?set.unreachable,
                            "Worker nodes cannot be asked now; deleted projects stay recorded until they are"
                        );
                    }
                    for node in set.reachable {
                        let name = format!("worker node {} ({})", node.node_name, node.node_id);
                        runtimes.push((Some(node.node_id), name, node.daemon));
                    }
                }
                Err(error) => {
                    every_runtime_asked = false;
                    tracing::error!(%error, "Could not read the worker nodes to sweep; their images wait for a later run");
                    summary.failed.push((None, error.to_string()));
                }
            }
        }

        let mut progress: HashMap<i32, ProjectProgress> = pending
            .iter()
            .map(|record| (record.project_id, ProjectProgress::default()))
            .collect();
        for (node_id, name, daemon) in &runtimes {
            self.sweep_runtime(
                *node_id,
                name,
                daemon.as_ref(),
                &owner,
                &pending,
                &mut progress,
                &mut summary,
            )
            .await;
        }

        let mut touch = Vec::new();
        for record in pending {
            let state = progress
                .get(&record.project_id)
                .copied()
                .unwrap_or_default();
            if every_runtime_asked && !state.left_behind && !state.passed_over {
                self.cursors()
                    .retain(|(_, project_id), _| *project_id != record.project_id);
                summary.finished.push(record.project_id);
                retire.push(record);
            } else {
                summary.pending.push(record.project_id);
                // A project a runtime never got to (the limit was used up,
                // or the runtime failed first) keeps its place, so it is
                // looked at first next run rather than behind the ones that
                // used the limit up.
                if !state.passed_over {
                    touch.push(record);
                }
            }
        }
        settle_records(self.db.as_ref(), &retire, &touch).await;
        summary
    }

    /// Sweep `projects` on one runtime, noting in `progress` each project it
    /// still holds images of, and each it never got to.
    #[allow(clippy::too_many_arguments, reason = "one runtime's share of a run")]
    async fn sweep_runtime(
        &self,
        node_id: Option<i32>,
        name: &str,
        daemon: &dyn OwnedImageDaemon,
        owner: &str,
        projects: &[DeletedProject],
        progress: &mut HashMap<i32, ProjectProgress>,
        summary: &mut OrphanSweepSummary,
    ) {
        let mut report = OrphanSweepReport::default();
        let mut budget = self.max_per_run;
        for (index, project) in projects.iter().enumerate() {
            let project_id = project.project_id;
            if budget == 0 {
                // Left for a later run, like the images the limit defers.
                report.deferred += projects.len() - index;
                pass_over(progress, &projects[index..]);
                break;
            }
            let after = self.cursors().get(&(node_id, project_id)).cloned();
            match sweep_deleted_project_images(
                self.db.as_ref(),
                daemon,
                owner,
                project_id,
                after.as_ref(),
                budget,
            )
            .await
            {
                Ok(sweep) => {
                    report.projects += 1;
                    report.listed += sweep.listed;
                    report.removed += sweep.removed;
                    report.kept += sweep.kept;
                    report.failed += sweep.failed;
                    report.deferred += sweep.deferred;
                    budget = budget.saturating_sub(sweep.attempted);
                    // Nothing left counts only for a listing from the
                    // oldest image: one that started past a cursor did not
                    // see the images before it, which may still be there.
                    if sweep.remaining > 0 || after.is_some() {
                        progress.entry(project_id).or_default().left_behind = true;
                    }
                    match sweep.next {
                        Some(next) => {
                            self.cursors().insert((node_id, project_id), next);
                        }
                        None => {
                            self.cursors().remove(&(node_id, project_id));
                        }
                    }
                }
                Err(error) => {
                    progress.entry(project_id).or_default().left_behind = true;
                    pass_over(progress, &projects[index + 1..]);
                    tracing::error!(
                        runtime = name,
                        project_id,
                        removed = report.removed,
                        %error,
                        "Could not sweep images built for deleted projects; the rest of this runtime waits for the next run"
                    );
                    summary.failed.push((node_id, error.to_string()));
                    return;
                }
            }
        }
        if report.listed == 0 {
            tracing::debug!(
                runtime = name,
                projects = report.projects,
                "No images of deleted projects to remove"
            );
        } else {
            tracing::info!(
                runtime = name,
                projects = report.projects,
                listed = report.listed,
                removed = report.removed,
                kept = report.kept,
                failed = report.failed,
                deferred = report.deferred,
                "Swept images built for deleted projects"
            );
        }
        summary.swept.push((node_id, report));
    }
}

#[cfg(test)]
mod tests;
