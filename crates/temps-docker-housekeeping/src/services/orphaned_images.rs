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

/// Remove `project`'s record (`retire`), or note it was checked so other
/// records go first next run. A failure is logged: the record is then
/// checked again, which is harmless.
async fn settle_record<C: ConnectionTrait>(db: &C, project: &DeletedProject, retire: bool) {
    let sql = if retire {
        "DELETE FROM deleted_project_images WHERE project_id = $1 AND deleted_at = $2"
    } else {
        "UPDATE deleted_project_images SET checked_at = now() \
         WHERE project_id = $1 AND deleted_at = $2"
    };
    if let Err(error) = db
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            [project.project_id.into(), project.deleted_at.into()],
        ))
        .await
    {
        tracing::warn!(
            project_id = project.project_id,
            retire,
            %error,
            "Could not update the record of a deleted project's images; it is checked again next run"
        );
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
        let mut pending = Vec::with_capacity(records.len());
        for record in records {
            if existing.contains(&record.project_id) {
                settle_record(self.db.as_ref(), &record, true).await;
            } else {
                pending.push(record);
            }
        }
        if pending.is_empty() {
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

        let mut clean: HashMap<i32, bool> = pending
            .iter()
            .map(|record| (record.project_id, true))
            .collect();
        for (node_id, name, daemon) in &runtimes {
            self.sweep_runtime(
                *node_id,
                name,
                daemon.as_ref(),
                &owner,
                &pending,
                &mut clean,
                &mut summary,
            )
            .await;
        }

        for record in &pending {
            let finished = every_runtime_asked && clean.get(&record.project_id) == Some(&true);
            settle_record(self.db.as_ref(), record, finished).await;
            if finished {
                self.cursors()
                    .retain(|(_, project_id), _| *project_id != record.project_id);
                summary.finished.push(record.project_id);
            } else {
                summary.pending.push(record.project_id);
            }
        }
        summary
    }

    /// Sweep `projects` on one runtime, marking in `clean` each project it
    /// still holds images of (or could not be asked about).
    #[allow(clippy::too_many_arguments, reason = "one runtime's share of a run")]
    async fn sweep_runtime(
        &self,
        node_id: Option<i32>,
        name: &str,
        daemon: &dyn OwnedImageDaemon,
        owner: &str,
        projects: &[DeletedProject],
        clean: &mut HashMap<i32, bool>,
        summary: &mut OrphanSweepSummary,
    ) {
        let mut report = OrphanSweepReport::default();
        let mut budget = self.max_per_run;
        for (index, project) in projects.iter().enumerate() {
            let project_id = project.project_id;
            if budget == 0 {
                // Left for a later run, like the images the limit defers.
                report.deferred += projects.len() - index;
                for later in &projects[index..] {
                    clean.insert(later.project_id, false);
                }
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
                    if sweep.remaining > 0 {
                        clean.insert(project_id, false);
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
                    for later in &projects[index..] {
                        clean.insert(later.project_id, false);
                    }
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
mod tests {
    use super::*;
    use temps_docker_util::image_removal::ImageRemovalStatus;

    fn image(id: &str, project: Option<&str>, created: i64) -> OwnedImage {
        let mut labels =
            HashMap::from([(IMAGE_INSTANCE_LABEL.to_string(), "install-a".to_string())]);
        if let Some(project) = project {
            labels.insert(IMAGE_PROJECT_LABEL.to_string(), project.to_string());
        }
        labels.insert(
            image_ownership::IMAGE_TAG_LABEL.to_string(),
            format!("{id}:latest"),
        );
        OwnedImage {
            image_id: id.to_string(),
            tags: vec![format!("{id}:latest")],
            labels,
            created,
            in_use_by: None,
        }
    }

    /// A scripted daemon: lists the `images` labelled with the project asked
    /// for, reports `held` as used by a container, records what it was asked
    /// to remove, and answers `answer`.
    struct ScriptedDaemon {
        images: Vec<OwnedImage>,
        held: Vec<&'static str>,
        answer: fn(&ImageRemovalRequest) -> ImageRemovalStatus,
        asked: std::sync::Mutex<Vec<String>>,
        /// The owner and project each listing asked for.
        listed_for: std::sync::Mutex<Vec<(String, i32)>>,
        /// The cursor each listing was given.
        listed_after: std::sync::Mutex<Vec<Option<ListCursor>>>,
        /// The cursor each listing answers with, in turn (then `None`).
        pages: std::sync::Mutex<Vec<Option<ListCursor>>>,
    }

    impl ScriptedDaemon {
        fn new(images: Vec<OwnedImage>) -> Self {
            Self {
                images,
                held: Vec::new(),
                answer: |request| ImageRemovalStatus::Removed {
                    image_id: request.expected_image_id.clone().unwrap_or_default(),
                },
                asked: Default::default(),
                listed_for: Default::default(),
                listed_after: Default::default(),
                pages: Default::default(),
            }
        }

        fn asked(&self) -> Vec<String> {
            self.asked.lock().expect("asked mutex").clone()
        }

        fn listed_for(&self) -> Vec<(String, i32)> {
            self.listed_for.lock().expect("listed mutex").clone()
        }
    }

    #[async_trait::async_trait]
    impl OwnedImageDaemon for ScriptedDaemon {
        async fn list_project_images(
            &self,
            owner: &str,
            project_id: i32,
            after: Option<&ListCursor>,
        ) -> Result<LabelledImages, OrphanSweepError> {
            self.listed_for
                .lock()
                .expect("listed mutex")
                .push((owner.to_string(), project_id));
            self.listed_after
                .lock()
                .expect("after mutex")
                .push(after.cloned());
            let mut pages = self.pages.lock().expect("pages mutex");
            let next = if pages.is_empty() {
                None
            } else {
                pages.remove(0)
            };
            let project = project_id.to_string();
            Ok(LabelledImages {
                images: self
                    .images
                    .iter()
                    .filter(|image| image.labels.get(IMAGE_PROJECT_LABEL) == Some(&project))
                    .cloned()
                    .collect(),
                unlisted: usize::from(next.is_some()),
                next,
            })
        }

        async fn container_using_image(
            &self,
            image: &OwnedImage,
        ) -> Result<Option<String>, OrphanSweepError> {
            let image_id = image.image_id.as_str();
            Ok(self
                .held
                .contains(&image_id)
                .then(|| format!("holder-of-{image_id}")))
        }

        async fn remove_unused_images(
            &self,
            requests: &[ImageRemovalRequest],
        ) -> Result<Vec<ImageRemovalOutcome>, OrphanSweepError> {
            self.asked
                .lock()
                .expect("asked mutex")
                .extend(requests.iter().map(|r| r.reference.clone()));
            Ok(requests
                .iter()
                .map(|request| ImageRemovalOutcome {
                    reference: request.reference.clone(),
                    status: (self.answer)(request),
                })
                .collect())
        }
    }

    fn rows(
        column: &str,
        values: &[sea_orm::Value],
    ) -> Vec<std::collections::BTreeMap<String, sea_orm::Value>> {
        values
            .iter()
            .map(|value| std::collections::BTreeMap::from([(column.to_string(), value.clone())]))
            .collect()
    }

    /// The references other projects still hold, for one project's images.
    fn references_db(named_elsewhere: &[&str]) -> sea_orm::DatabaseConnection {
        use sea_orm::{MockDatabase, Value};
        let named: Vec<Value> = named_elsewhere
            .iter()
            .map(|reference| Value::from(reference.to_string()))
            .collect();
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([rows("reference", &named)])
            .into_connection()
    }

    #[tokio::test]
    async fn a_sweep_removes_a_deleted_project_s_images_within_the_limit() {
        let mut daemon = ScriptedDaemon::new(vec![
            image("live", Some("1"), 10),
            image("gone-b", Some("2"), 30),
            image("gone-a", Some("2"), 20),
            image("gone-c", Some("2"), 40),
        ]);
        daemon.answer = |request| {
            if request.reference == "gone-b:latest" {
                ImageRemovalStatus::Refused {
                    image_id: "gone-b".to_string(),
                    reason: "conflict".to_string(),
                }
            } else {
                ImageRemovalStatus::Removed {
                    image_id: request.reference.clone(),
                }
            }
        };
        let db = references_db(&[]);

        let sweep = sweep_deleted_project_images(&db, &daemon, "install-a", 2, None, 2)
            .await
            .expect("sweep");

        assert_eq!(
            daemon.listed_for(),
            [("install-a".to_string(), 2)],
            "only this install's images of the deleted project are listed"
        );
        assert_eq!(
            daemon.asked(),
            ["gone-a:latest", "gone-b:latest"],
            "oldest first, by their one tag, bounded by the limit"
        );
        assert_eq!(
            sweep,
            ProjectSweep {
                listed: 3,
                removed: 1,
                kept: 1,
                failed: 0,
                deferred: 1,
                attempted: 2,
                remaining: 2,
                next: None,
            }
        );
    }

    /// Images that must stay never take a slot in the batch: a backlog of
    /// them cannot starve a newer image that could go.
    #[tokio::test]
    async fn held_shared_and_retagged_images_do_not_use_up_the_limit() {
        let mut retagged = image("gone-retagged", Some("2"), 25);
        retagged.tags.push("operator-backup:keep".to_string());
        // The operator retagged it and removed the build's tag.
        let mut replaced = image("gone-replaced", Some("2"), 26);
        replaced.tags = vec!["operator-backup:v1".to_string()];
        let mut unrecorded = image("gone-unrecorded", Some("2"), 27);
        unrecorded.labels.remove(image_ownership::IMAGE_TAG_LABEL);
        let mut daemon = ScriptedDaemon::new(vec![
            image("gone-held", Some("2"), 10),
            image("gone-shared", Some("2"), 20),
            retagged,
            replaced,
            unrecorded,
            image("gone-free", Some("2"), 30),
        ]);
        daemon.held = vec!["gone-held"];
        // Another project still deploys the shared image, naming it without
        // its implicit `:latest`.
        let db = references_db(&["gone-shared"]);

        let sweep = sweep_deleted_project_images(&db, &daemon, "install-a", 2, None, 1)
            .await
            .expect("sweep");

        assert_eq!(daemon.asked(), ["gone-free:latest"]);
        assert_eq!(
            sweep,
            ProjectSweep {
                listed: 6,
                removed: 1,
                kept: 5,
                attempted: 1,
                remaining: 5,
                ..Default::default()
            }
        );
    }

    /// A project with more images than one page: images the limit deferred
    /// are retried on the same page when the run made progress; a page whose
    /// batch is refused every time (dependent child images, a failing node)
    /// moves on instead of pinning the listing there for good.
    #[tokio::test]
    async fn a_page_is_retried_only_while_its_removals_make_progress() {
        let page_start = ListCursor {
            created: 7,
            image_id: "sha256:07".to_string(),
        };
        let page_end = ListCursor {
            created: 99,
            image_id: "sha256:99".to_string(),
        };
        let pages = || {
            vec![
                image("gone-a", Some("2"), 10),
                image("gone-b", Some("2"), 20),
                image("gone-c", Some("2"), 30),
            ]
        };

        let progressing = ScriptedDaemon::new(pages());
        *progressing.pages.lock().expect("pages") = vec![Some(page_end.clone())];
        let sweep = sweep_deleted_project_images(
            &references_db(&[]),
            &progressing,
            "install-a",
            2,
            Some(&page_start),
            2,
        )
        .await
        .expect("sweep");
        assert_eq!((sweep.removed, sweep.deferred), (2, 1));
        assert_eq!(sweep.next, Some(page_start.clone()), "the same page again");
        assert_eq!(sweep.remaining, 2, "one deferred, one past the page");

        for answer in [
            (|request: &ImageRemovalRequest| ImageRemovalStatus::Refused {
                image_id: request.reference.clone(),
                reason: "image has dependent child images".to_string(),
            }) as fn(&ImageRemovalRequest) -> ImageRemovalStatus,
            |_| ImageRemovalStatus::Failed {
                reason: "agent unreachable".to_string(),
            },
        ] {
            let mut stuck = ScriptedDaemon::new(pages());
            stuck.answer = answer;
            *stuck.pages.lock().expect("pages") = vec![Some(page_end.clone())];
            let sweep = sweep_deleted_project_images(
                &references_db(&[]),
                &stuck,
                "install-a",
                2,
                Some(&page_start),
                2,
            )
            .await
            .expect("sweep");
            assert_eq!((sweep.removed, sweep.deferred), (0, 1));
            assert_eq!(
                sweep.next,
                Some(page_end.clone()),
                "no progress on this page: the listing moves on"
            );
        }
    }

    #[tokio::test]
    async fn a_failed_reference_lookup_removes_nothing() {
        let daemon = ScriptedDaemon::new(vec![image("gone", Some("2"), 1)]);
        let db = sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom("connection reset".to_string())])
            .into_connection();
        let result = sweep_deleted_project_images(&db, &daemon, "install-a", 2, None, 10).await;
        assert!(
            matches!(
                result,
                Err(OrphanSweepError::ReferenceLookup { project_id: 2, .. })
            ),
            "{result:?}"
        );
        assert!(daemon.asked().is_empty());
    }

    /// Reachable nodes with their daemons, and the IDs of unreachable ones.
    type ScriptedNodes = (Vec<(i32, std::sync::Arc<ScriptedDaemon>)>, Vec<i32>);

    /// Worker nodes as the [`WorkerImageDaemons`] say: reachable ones, and
    /// the IDs of those that cannot be asked.
    struct ScriptedWorkers(Result<ScriptedNodes, &'static str>);

    #[async_trait::async_trait]
    impl WorkerImageDaemons for ScriptedWorkers {
        async fn worker_image_daemons(&self) -> Result<WorkerImageDaemonSet, OrphanSweepError> {
            match &self.0 {
                Ok((nodes, unreachable)) => Ok(WorkerImageDaemonSet {
                    reachable: nodes
                        .iter()
                        .map(|(node_id, daemon)| WorkerImageDaemon {
                            node_id: *node_id,
                            node_name: format!("worker-{node_id}"),
                            daemon: daemon.clone(),
                        })
                        .collect(),
                    unreachable: unreachable.clone(),
                }),
                Err(reason) => Err(OrphanSweepError::Workers {
                    source: (*reason).into(),
                }),
            }
        }
    }

    /// A mock database for one run: the identity row, the recorded deleted
    /// projects, those with a row again, then one reference lookup per
    /// listing that found images, and an answer for each record settled.
    fn run_db(
        deleted: &[i32],
        restored: &[i32],
        reference_lookups: usize,
    ) -> std::sync::Arc<sea_orm::DatabaseConnection> {
        use sea_orm::{MockDatabase, MockExecResult, Value};
        let deleted_at = chrono::Utc::now();
        let records: Vec<std::collections::BTreeMap<String, Value>> = deleted
            .iter()
            .map(|id| {
                std::collections::BTreeMap::from([
                    ("project_id".to_string(), Value::from(*id)),
                    ("deleted_at".to_string(), Value::from(deleted_at)),
                ])
            })
            .collect();
        let restored: Vec<Value> = restored.iter().map(|id| Value::from(*id)).collect();
        let mut db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([rows("instance_id", &[Value::from("install-a".to_string())])])
            .append_query_results([records])
            .append_query_results([rows("id", &restored)]);
        for _ in 0..reference_lookups {
            db = db.append_query_results([rows("reference", &[])]);
        }
        let settled = (0..deleted.len()).map(|_| MockExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        });
        std::sync::Arc::new(db.append_exec_results(settled).into_connection())
    }

    async fn marked_dir() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("temp dir");
        let marker = image_ownership::install_marker(dir.path(), true)
            .await
            .expect("marker")
            .expect("created");
        (dir, marker)
    }

    /// The sweep asks each runtime for each deleted project's images, with
    /// this install's owner ID (the database ID joined with the data
    /// directory's marker), and removes a project's record once no runtime
    /// holds any. A process that runs no workloads sweeps its workers only.
    #[tokio::test]
    async fn the_sweeper_asks_every_worker_for_each_deleted_project() {
        let (dir, marker) = marked_dir().await;
        let worker = std::sync::Arc::new(ScriptedDaemon::new(vec![
            image("gone", Some("2"), 1),
            image("live", Some("1"), 1),
        ]));
        let sweeper = OrphanImageSweeper::new(run_db(&[2, 3], &[], 1))
            .with_local_daemon(None)
            .with_worker_daemons(std::sync::Arc::new(ScriptedWorkers(Ok((
                vec![(7, worker.clone())],
                Vec::new(),
            )))))
            .with_data_dir(dir.path().to_path_buf());

        let summary = sweeper.run().await;

        let owner = format!("install-a.{marker}");
        assert_eq!(worker.listed_for(), [(owner.clone(), 2), (owner, 3)]);
        assert_eq!(worker.asked(), ["gone:latest"]);
        assert_eq!(summary.swept.len(), 1);
        assert_eq!(summary.swept[0].0, Some(7));
        assert_eq!(
            summary.swept[0].1,
            OrphanSweepReport {
                projects: 2,
                listed: 1,
                removed: 1,
                ..Default::default()
            }
        );
        assert_eq!(summary.finished, [2, 3]);
        assert!(
            summary.pending.is_empty() && summary.failed.is_empty(),
            "{summary:?}"
        );
    }

    /// With no deleted project recorded, no daemon is asked anything: the
    /// sweep's work follows deletions, not the images on the hosts.
    #[tokio::test]
    async fn with_no_deleted_projects_no_daemon_is_asked() {
        let (dir, _) = marked_dir().await;
        let local = std::sync::Arc::new(ScriptedDaemon::new(vec![image("gone", Some("2"), 1)]));
        let worker = std::sync::Arc::new(ScriptedDaemon::new(Vec::new()));
        let sweeper = OrphanImageSweeper::new(run_db(&[], &[], 0))
            .with_local_daemon(Some(local.clone()))
            .with_worker_daemons(std::sync::Arc::new(ScriptedWorkers(Ok((
                vec![(7, worker.clone())],
                Vec::new(),
            )))))
            .with_data_dir(dir.path().to_path_buf());

        let summary = sweeper.run().await;

        assert!(local.listed_for().is_empty() && worker.listed_for().is_empty());
        assert!(
            summary.swept.is_empty() && summary.failed.is_empty(),
            "{summary:?}"
        );
    }

    /// A record stays while the project's images cannot all be accounted
    /// for: a node that cannot be asked, or an image that must stay. It is
    /// checked again on a later run.
    #[tokio::test]
    async fn a_record_stays_while_a_node_is_unreachable_or_an_image_stays() {
        let (dir, _) = marked_dir().await;
        let local = std::sync::Arc::new(ScriptedDaemon::new(vec![image("gone", Some("2"), 1)]));
        let sweeper = OrphanImageSweeper::new(run_db(&[2], &[], 1))
            .with_local_daemon(Some(local.clone()))
            .with_worker_daemons(std::sync::Arc::new(ScriptedWorkers(Ok((
                Vec::new(),
                vec![9],
            )))))
            .with_data_dir(dir.path().to_path_buf());
        let summary = sweeper.run().await;
        assert_eq!(
            local.asked(),
            ["gone:latest"],
            "the reachable host is swept"
        );
        assert!(summary.finished.is_empty());
        assert_eq!(summary.pending, [2], "node 9 still has to be checked");

        let mut holding = ScriptedDaemon::new(vec![image("gone", Some("2"), 1)]);
        holding.held = vec!["gone"];
        let holding = std::sync::Arc::new(holding);
        let sweeper = OrphanImageSweeper::new(run_db(&[2], &[], 1))
            .with_local_daemon(Some(holding.clone()))
            .with_data_dir(dir.path().to_path_buf());
        let summary = sweeper.run().await;
        assert!(holding.asked().is_empty());
        assert_eq!(summary.pending, [2], "a container still uses its image");
    }

    /// A deleted project's ID that has a row again (a restored backup) is a
    /// live project: its record is dropped and its images are not touched.
    #[tokio::test]
    async fn a_restored_project_s_record_is_dropped_without_listing() {
        let (dir, _) = marked_dir().await;
        let local = std::sync::Arc::new(ScriptedDaemon::new(vec![image("back", Some("2"), 1)]));
        let sweeper = OrphanImageSweeper::new(run_db(&[2], &[2], 0))
            .with_local_daemon(Some(local.clone()))
            .with_data_dir(dir.path().to_path_buf());
        let summary = sweeper.run().await;
        assert!(local.listed_for().is_empty());
        assert!(summary.finished.is_empty() && summary.pending.is_empty());
    }

    /// The per-run limit spans the deleted projects on a runtime: once it is
    /// used up, the remaining projects wait for a later run.
    #[tokio::test]
    async fn the_limit_defers_the_remaining_projects() {
        let (dir, _) = marked_dir().await;
        let local = std::sync::Arc::new(ScriptedDaemon::new(vec![
            image("gone-2", Some("2"), 1),
            image("gone-3", Some("3"), 1),
        ]));
        let sweeper = OrphanImageSweeper::new(run_db(&[2, 3], &[], 1))
            .with_local_daemon(Some(local.clone()))
            .with_data_dir(dir.path().to_path_buf())
            .with_max_per_run(1);
        let summary = sweeper.run().await;
        assert_eq!(local.asked(), ["gone-2:latest"]);
        assert_eq!(local.listed_for().len(), 1, "project 3 is not listed");
        assert_eq!(summary.swept[0].1.deferred, 1);
        assert_eq!((summary.finished, summary.pending), (vec![2], vec![3]));
    }

    /// A project with more images on a runtime than one page is paged
    /// through: the next run starts where the last one stopped, and the
    /// end of its list starts over.
    #[tokio::test]
    async fn each_run_resumes_a_project_s_listing_where_the_last_one_stopped() {
        let (dir, _) = marked_dir().await;
        let page_end = ListCursor {
            created: 42,
            image_id: "sha256:42".to_string(),
        };
        let mut local = ScriptedDaemon::new(vec![image("gone", Some("2"), 1)]);
        // Each listing's image is held, so the project stays recorded.
        local.held = vec!["gone"];
        *local.pages.lock().expect("pages") = vec![Some(page_end.clone()), None];
        let local = std::sync::Arc::new(local);
        let db = {
            use sea_orm::{MockDatabase, MockExecResult, Value};
            let mut db = MockDatabase::new(DatabaseBackend::Postgres);
            for _ in 0..3 {
                db = db
                    .append_query_results([rows(
                        "instance_id",
                        &[Value::from("install-a".to_string())],
                    )])
                    .append_query_results([vec![std::collections::BTreeMap::from([
                        ("project_id".to_string(), Value::from(2)),
                        ("deleted_at".to_string(), Value::from(chrono::Utc::now())),
                    ])]])
                    .append_query_results([rows("id", &[])])
                    .append_query_results([rows("reference", &[])])
                    .append_exec_results([MockExecResult {
                        last_insert_id: 0,
                        rows_affected: 1,
                    }]);
            }
            std::sync::Arc::new(db.into_connection())
        };
        let sweeper = OrphanImageSweeper::new(db)
            .with_local_daemon(Some(local.clone()))
            .with_data_dir(dir.path().to_path_buf());

        for _ in 0..3 {
            sweeper.run().await;
        }

        assert_eq!(
            local.listed_after.lock().expect("after").as_slice(),
            [None, Some(page_end), None],
            "first page, then after it, then over from the oldest"
        );
    }

    /// If the worker nodes cannot be read, this host is still swept, the
    /// failure is reported rather than hidden, and the record stays.
    #[tokio::test]
    async fn an_unreadable_worker_list_does_not_stop_this_host_s_sweep() {
        let (dir, _) = marked_dir().await;
        let local = std::sync::Arc::new(ScriptedDaemon::new(vec![image("gone", Some("2"), 1)]));
        let sweeper = OrphanImageSweeper::new(run_db(&[2], &[], 1))
            .with_local_daemon(Some(local.clone()))
            .with_worker_daemons(std::sync::Arc::new(ScriptedWorkers(Err(
                "nodes table unreadable",
            ))))
            .with_data_dir(dir.path().to_path_buf());

        let summary = sweeper.run().await;

        assert_eq!(local.asked(), ["gone:latest"]);
        assert_eq!(summary.failed.len(), 1);
        assert!(
            summary.failed[0].1.contains("nodes table unreadable"),
            "{summary:?}"
        );
        assert_eq!(summary.pending, [2]);
    }

    /// Without the data directory's marker, no image can carry this
    /// install's owner ID: nothing is listed, let alone removed. A copied
    /// database in another install lands here until it builds.
    #[tokio::test]
    async fn without_the_install_marker_nothing_is_listed_or_removed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let local = std::sync::Arc::new(ScriptedDaemon::new(vec![image("gone", Some("2"), 1)]));
        let identity_only = std::sync::Arc::new(
            sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([rows(
                    "instance_id",
                    &[sea_orm::Value::from("install-a".to_string())],
                )])
                .into_connection(),
        );
        let sweeper = OrphanImageSweeper::new(identity_only)
            .with_local_daemon(Some(local.clone()))
            .with_data_dir(dir.path().to_path_buf());
        let summary = sweeper.run().await;
        assert!(
            summary.swept.is_empty() && summary.failed.is_empty(),
            "{summary:?}"
        );
        assert!(local.listed_for().is_empty());

        // Without a data directory at all, not even the database is read.
        let unconfigured = OrphanImageSweeper::new(std::sync::Arc::new(
            sea_orm::MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
        ))
        .with_local_daemon(Some(local.clone()));
        let summary = unconfigured.run().await;
        assert!(summary.swept.is_empty() && summary.failed.is_empty());
        assert!(local.listed_for().is_empty());
    }

    #[tokio::test]
    async fn a_failed_record_read_asks_no_daemon() {
        let (dir, _) = marked_dir().await;
        let local = std::sync::Arc::new(ScriptedDaemon::new(vec![image("gone", Some("2"), 1)]));
        let db = std::sync::Arc::new(
            sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([rows(
                    "instance_id",
                    &[sea_orm::Value::from("install-a".to_string())],
                )])
                .append_query_errors([sea_orm::DbErr::Custom("connection reset".to_string())])
                .into_connection(),
        );
        let sweeper = OrphanImageSweeper::new(db)
            .with_local_daemon(Some(local.clone()))
            .with_data_dir(dir.path().to_path_buf());
        let summary = sweeper.run().await;
        assert!(local.listed_for().is_empty());
        assert_eq!(summary.failed.len(), 1, "{summary:?}");
        assert!(
            summary.failed[0].1.contains("deleted projects"),
            "{summary:?}"
        );
    }

    async fn build_labelled_image(
        docker: &bollard::Docker,
        tag: &str,
        labels: &HashMap<String, String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use futures_util::StreamExt as _;
        let dockerfile = format!(
            "FROM busybox:1.36\nENV TEMPS_TEST_MARKER={}\n",
            uuid::Uuid::new_v4()
        );
        let mut header = tar::Header::new_gnu();
        header.set_size(dockerfile.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        let mut archive = tar::Builder::new(Vec::new());
        archive.append_data(&mut header, "Dockerfile", dockerfile.as_bytes())?;
        let context = archive.into_inner()?;
        let mut build = docker.build_image(
            bollard::query_parameters::BuildImageOptionsBuilder::default()
                .t(tag)
                .labels(labels)
                .rm(true)
                .build(),
            None,
            Some(bollard::body_full(bytes::Bytes::from(context))),
        );
        while let Some(step) = build.next().await {
            step?;
        }
        Ok(())
    }

    async fn insert_project(
        db: &sea_orm::DatabaseConnection,
        slug: &str,
    ) -> Result<temps_entities::projects::Model, sea_orm::DbErr> {
        use sea_orm::{ActiveModelTrait, ActiveValue::Set};
        let now = chrono::Utc::now();
        temps_entities::projects::ActiveModel {
            name: Set(format!("Project {slug}")),
            slug: Set(slug.to_string()),
            repo_owner: Set("test-owner".to_string()),
            repo_name: Set("test-repo".to_string()),
            preset: Set(temps_entities::preset::Preset::Dockerfile),
            directory: Set("/".to_string()),
            main_branch: Set("main".to_string()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await
    }

    /// Against a real daemon and database: deleting a project's row records
    /// it, and the sweep then removes its unused labelled image, while an
    /// image of that project a container was created from, an image of a
    /// live project, an image another install labelled, and an unlabelled
    /// image all survive. The record stays while the held image does, and
    /// goes once it is gone. Skips when Docker or Postgres is unavailable.
    #[tokio::test]
    async fn the_sweep_removes_only_unused_images_of_deleted_projects_on_a_real_daemon(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use bollard::query_parameters::{
            CreateContainerOptionsBuilder, CreateImageOptionsBuilder, RemoveContainerOptions,
            RemoveImageOptions,
        };
        use futures_util::StreamExt as _;
        use sea_orm::EntityTrait;

        let Ok(docker) = bollard::Docker::connect_with_unix_defaults() else {
            println!("Docker not available, skipping");
            return Ok(());
        };
        if docker.ping().await.is_err() {
            println!("Docker not available, skipping");
            return Ok(());
        }
        let test_db = match temps_test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) => {
                println!("Database unavailable, skipping: {error}");
                return Ok(());
            }
        };
        let db = test_db.connection_arc();
        let mut pull = docker.create_image(
            Some(
                CreateImageOptionsBuilder::default()
                    .from_image("busybox")
                    .tag("1.36")
                    .build(),
            ),
            None,
            None,
        );
        while let Some(step) = pull.next().await {
            if let Err(error) = step {
                println!("Could not pull busybox, skipping: {error}");
                return Ok(());
            }
        }

        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let live = insert_project(db.as_ref(), &format!("live-{nonce}")).await?;
        let doomed = insert_project(db.as_ref(), &format!("doomed-{nonce}")).await?;
        let data_dir = tempfile::tempdir()?;
        let instance = image_ownership::ensure_image_owner_id(db.as_ref(), data_dir.path()).await?;
        let tag = |name: &str| format!("temps-orphan-test-{name}-{nonce}:latest");
        let orphan = tag("orphan");
        let held = tag("held");
        let live_image = tag("live");
        let foreign = tag("foreign");
        let unlabelled = tag("unlabelled");
        let holder = format!("temps-orphan-test-holder-{nonce}");
        let images = [
            (
                &orphan,
                image_ownership::labels_for_tag(
                    &image_ownership::image_ownership_labels(&instance, doomed.id, 1),
                    &orphan,
                ),
            ),
            (
                &held,
                image_ownership::labels_for_tag(
                    &image_ownership::image_ownership_labels(&instance, doomed.id, 2),
                    &held,
                ),
            ),
            (
                &live_image,
                image_ownership::labels_for_tag(
                    &image_ownership::image_ownership_labels(&instance, live.id, 3),
                    &live_image,
                ),
            ),
            (
                &foreign,
                image_ownership::image_ownership_labels("another-install", doomed.id, 4),
            ),
            (&unlabelled, HashMap::new()),
        ];

        let mut prepared = Ok(());
        for (image_tag, labels) in &images {
            if let Err(error) = build_labelled_image(&docker, image_tag, labels).await {
                prepared = Err(format!("build {image_tag}: {error}"));
                break;
            }
        }
        if prepared.is_ok() {
            prepared = docker
                .create_container(
                    Some(CreateContainerOptionsBuilder::new().name(&holder).build()),
                    bollard::models::ContainerCreateBody {
                        image: Some(held.clone()),
                        cmd: Some(vec!["true".to_string()]),
                        ..Default::default()
                    },
                )
                .await
                .map(|_| ())
                .map_err(|error| format!("create holder: {error}"));
        }
        let sweeper = OrphanImageSweeper::new(db.clone())
            .with_data_dir(data_dir.path().to_path_buf())
            .with_max_per_run(10);
        // Every step's error is kept, not returned, so the Docker resources
        // below are removed whatever fails.
        let mut first = None;
        let mut second = None;
        if prepared.is_ok() {
            match temps_entities::projects::Entity::delete_by_id(doomed.id)
                .exec(db.as_ref())
                .await
            {
                Ok(_) => first = Some(sweeper.run().await),
                Err(error) => prepared = Err(format!("delete project {}: {error}", doomed.id)),
            }
        }
        let present = |image_tag: String| {
            let docker = docker.clone();
            async move { docker.inspect_image(&image_tag).await.is_ok() }
        };
        let orphan_left = present(orphan.clone()).await;
        let held_left = present(held.clone()).await;
        let live_left = present(live_image.clone()).await;
        let foreign_left = present(foreign.clone()).await;
        let unlabelled_left = present(unlabelled.clone()).await;

        // Once the container is gone, the next run removes the held image
        // and the record with it.
        let _ = docker
            .remove_container(
                &holder,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
        if prepared.is_ok() {
            second = Some(sweeper.run().await);
        }
        let held_left_after = present(held.clone()).await;

        // Remove only what this test created, before asserting.
        for (image_tag, _) in &images {
            let _ = docker
                .remove_image(
                    image_tag,
                    Some(RemoveImageOptions {
                        force: true,
                        ..Default::default()
                    }),
                    None,
                )
                .await;
        }

        // Docker and Postgres answered and busybox was pulled: a failure from
        // here on is the test's, not a missing service, so it fails.
        if let Err(reason) = prepared {
            return Err(format!("preparing the sweep test failed: {reason}").into());
        }
        let first = first.expect("the first run");
        assert!(first.failed.is_empty(), "{first:?}");
        let report = &first.swept[0].1;
        assert_eq!(
            report.listed, 2,
            "only this install's images of the deleted project are listed: {report:?}"
        );
        assert_eq!((report.removed, report.kept), (1, 1), "{report:?}");
        assert_eq!(
            first.pending,
            [doomed.id],
            "the held image keeps the record"
        );
        assert!(
            !orphan_left,
            "the unused image of the deleted project must be gone"
        );
        assert!(held_left, "an image a container was created from must stay");
        assert!(live_left, "an image of a live project must stay");
        assert!(foreign_left, "another install's image must stay");
        assert!(unlabelled_left, "an image without Temps labels must stay");

        let second = second.expect("the second run");
        assert!(!held_left_after, "{second:?}");
        assert_eq!(second.finished, [doomed.id], "{second:?}");
        let remaining = temps_entities::deleted_project_images::Entity::find_by_id(doomed.id)
            .one(db.as_ref())
            .await?;
        assert!(
            remaining.is_none(),
            "the record is gone once nothing is left"
        );
        Ok(())
    }
}
