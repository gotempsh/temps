// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Removing images built for projects that no longer exist.
//!
//! Deleting a project removes the images its builds recorded (see
//! `ProjectImageCleaner` in `temps-core`), but that is best-effort: a daemon
//! that was unreachable, or a removal Docker refused, leaves the image behind
//! after the `deployment_jobs` rows that named it are gone, so the
//! row-based retention pass can never pick it again. This sweep finds those
//! by their build labels instead (see [`super::image_ownership`]):
//!
//! - only images labelled with **this install's** ID, so another Temps
//!   install sharing the host is never touched, and images built before the
//!   labels existed are never touched either;
//! - only when the labelled project has **no row** in this database, checked
//!   for the whole set of labelled projects in one query;
//! - only when **no other project** still names the image's tag or ID in a
//!   deployment or build (a promotion, an identical build), checked for the
//!   whole set of orphans in one query;
//! - only when its tags are exactly the one its build gave it (recorded as
//!   `sh.temps.image.tag`), so an image someone retagged for themselves
//!   stays;
//! - only when **no container**, running or stopped, was created from the
//!   image, judged by image ID; removed by that ID and never forced.
//!
//! It never selects by tag text (#1373). Its cost grows with the number of
//! labelled images on this daemon, not with the size of the database, and it
//! removes at most a configured number of images per run, oldest first.

use std::collections::{BTreeSet, HashSet};

use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use temps_docker_util::image_removal::{
    ImageRemovalOutcome, ImageRemovalRequest, MAX_IMAGE_REMOVAL_BATCH,
};

use super::image_ownership::{self, ImageOwnershipError, IMAGE_INSTANCE_LABEL};

/// Only these labels of a listed image are kept.
const IMAGE_LABEL_PREFIX: &str = "sh.temps.image.";
use super::image_references;

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
        "Failed to connect to the Docker daemon to list images labelled {label}={owner}: {source}"
    )]
    Connect {
        label: &'static str,
        owner: String,
        #[source]
        source: bollard::errors::Error,
    },

    #[error("Failed to list images labelled {label}={owner}: {source}")]
    ListImages {
        label: &'static str,
        owner: String,
        /// Boxed: the Docker error is large and this variant is rare.
        #[source]
        source: Box<temps_docker_util::DockerUtilError>,
    },

    #[error("Failed to check which of {count} labelled projects still exist: {source}")]
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

    #[error("Failed to check which of {count} orphaned image references other projects still name: {source}")]
    ReferenceLookup {
        count: usize,
        #[source]
        source: sea_orm::DbErr,
    },

    #[error("Failed to remove {count} orphaned images: {source}")]
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

/// What one sweep did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OrphanSweepReport {
    /// Images labelled with this install's owner ID.
    pub labelled: usize,
    /// Of those, images whose project no longer exists.
    pub orphaned: usize,
    pub removed: usize,
    /// Kept: another project still names the image, its tags are not just
    /// the one its build gave it, a container uses it, or Docker refused.
    pub kept: usize,
    pub failed: usize,
    /// Orphans left for a later run by the per-run limit.
    pub deferred: usize,
}

/// The daemon operations the sweep needs: this host's daemon, or a worker
/// node's through its agent. Mockable in tests.
#[async_trait::async_trait]
pub trait OwnedImageDaemon: Send + Sync {
    /// Images labelled `sh.temps.image.instance=<owner>`, oldest first and
    /// bounded by [`temps_docker_util::image_removal::MAX_LABELLED_IMAGES`].
    async fn list_instance_images(&self, owner: &str) -> Result<Vec<OwnedImage>, OrphanSweepError>;

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

/// The worker nodes the nightly sweep covers, implemented where nodes are
/// known (`temps-deployments`). A worker's own housekeeping has no database,
/// so it cannot tell which projects still exist: the control plane does it.
#[async_trait::async_trait]
pub trait WorkerImageDaemons: Send + Sync {
    /// The reachable worker nodes. A node that cannot be reached is left out
    /// (and logged); its images are swept on a later night.
    async fn worker_image_daemons(&self) -> Result<Vec<WorkerImageDaemon>, OrphanSweepError>;
}

#[async_trait::async_trait]
impl OwnedImageDaemon for super::DefaultDockerClient {
    async fn list_instance_images(&self, owner: &str) -> Result<Vec<OwnedImage>, OrphanSweepError> {
        let docker = bollard::Docker::connect_with_unix_defaults().map_err(|source| {
            OrphanSweepError::Connect {
                label: IMAGE_INSTANCE_LABEL,
                owner: owner.to_string(),
                source,
            }
        })?;
        let listing = temps_docker_util::list_labelled_images(
            &docker,
            IMAGE_INSTANCE_LABEL,
            owner,
            IMAGE_LABEL_PREFIX,
            false,
        )
        .await
        .map_err(|source| OrphanSweepError::ListImages {
            label: IMAGE_INSTANCE_LABEL,
            owner: owner.to_string(),
            source: Box::new(source),
        })?;
        if listing.unlisted > 0 {
            tracing::warn!(
                listed = listing.images.len(),
                unlisted = listing.unlisted,
                "More of this install's images than one sweep lists; the newest wait for a later run"
            );
        }
        Ok(listing.images)
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

/// The orphans among `images`: labelled with a project `existing` does not
/// contain, oldest first. An image whose project label is missing or
/// malformed is never an orphan: it cannot be tied to a project at all.
pub fn select_orphans<'a>(
    images: &'a [OwnedImage],
    existing_projects: &HashSet<i32>,
) -> Vec<&'a OwnedImage> {
    let mut orphans: Vec<&OwnedImage> = images
        .iter()
        .filter(|image| {
            image_ownership::labelled_project_id(&image.labels)
                .is_some_and(|project_id| !existing_projects.contains(&project_id))
        })
        .collect();
    orphans.sort_by(|a, b| {
        a.created
            .cmp(&b.created)
            .then_with(|| a.image_id.cmp(&b.image_id))
    });
    orphans
}

/// Which of `project_ids` still have a row, in one query by primary key.
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

/// The removal request for an orphan: by its one tag, expecting its ID, or
/// by ID if it has no tag. `None` when its tags are not exactly the one the
/// build gave it (`sh.temps.image.tag`): a second tag, or one that replaced
/// the build's, is an operator's retag, so the image is theirs. An image
/// built without that label is kept too: its one tag proves nothing.
fn removal_request(image: &OwnedImage) -> Option<ImageRemovalRequest> {
    match image.tags.as_slice() {
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
    }
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

/// Remove up to `max_per_run` images this install built for projects that
/// no longer exist. See the module documentation for the safety rules.
///
/// Images that must stay (named by another project, retagged, used by a
/// container) are set aside **before** the limit is applied, so a backlog
/// of held images can never fill every night's batch and starve newer
/// orphans that could go.
pub async fn sweep_orphaned_project_images<C: ConnectionTrait>(
    db: &C,
    daemon: &dyn OwnedImageDaemon,
    owner: &str,
    max_per_run: usize,
) -> Result<OrphanSweepReport, OrphanSweepError> {
    let images = daemon.list_instance_images(owner).await?;
    let mut report = OrphanSweepReport {
        labelled: images.len(),
        ..Default::default()
    };
    let project_ids: BTreeSet<i32> = images
        .iter()
        .filter_map(|image| image_ownership::labelled_project_id(&image.labels))
        .collect();
    if project_ids.is_empty() {
        return Ok(report);
    }
    let existing = existing_project_ids(db, &project_ids).await?;
    let orphans = select_orphans(&images, &existing);
    report.orphaned = orphans.len();
    if orphans.is_empty() {
        return Ok(report);
    }

    // The project is gone, but another project may still deploy or have
    // built the same image: any row naming its tags or ID keeps it.
    let mut candidates = Vec::new();
    for image in &orphans {
        for tag in &image.tags {
            candidates.extend(image_references::tag_spellings(tag));
        }
        candidates.extend(image_references::image_id_spellings(&image.image_id));
    }
    let named = image_references::referenced_images(db, &candidates, None)
        .await
        .map_err(|source| OrphanSweepError::ReferenceLookup {
            count: candidates.len(),
            source,
        })?;

    let limit = max_per_run.min(MAX_IMAGE_REMOVAL_BATCH);
    let mut selected: Vec<(&OwnedImage, ImageRemovalRequest)> = Vec::new();
    for (index, image) in orphans.iter().enumerate() {
        if selected.len() == limit {
            report.deferred = orphans.len() - index;
            break;
        }
        let named_elsewhere = image
            .tags
            .iter()
            .any(|tag| image_references::names_tag(&named, tag))
            || image_references::names_image_id(&named, &image.image_id);
        if named_elsewhere {
            report.kept += 1;
            log_kept(image, "another project's deployment or build names it");
            continue;
        }
        let Some(request) = removal_request(image) else {
            report.kept += 1;
            log_kept(image, "its tags are not just the one its build gave it");
            continue;
        };
        match daemon.container_using_image(image).await {
            Ok(None) => selected.push((image, request)),
            Ok(Some(container_id)) => {
                report.kept += 1;
                log_kept(
                    image,
                    &format!("container {container_id} was created from it"),
                );
            }
            Err(error) => {
                report.failed += 1;
                log_kept(image, &error.to_string());
            }
        }
    }
    if selected.is_empty() {
        return Ok(report);
    }

    let requests: Vec<ImageRemovalRequest> = selected
        .iter()
        .map(|(_, request)| request.clone())
        .collect();
    let outcomes = daemon.remove_unused_images(&requests).await?;
    for (outcome, (image, _)) in outcomes.iter().zip(selected.iter()) {
        if outcome.status.is_removed() {
            report.removed += 1;
            tracing::info!(
                image_id = %image.image_id,
                tags = ?image.tags,
                project_id = ?image_ownership::labelled_project_id(&image.labels),
                "Removed image built for a project that no longer exists"
            );
        } else if let Some(reason) = outcome.status.reason() {
            if outcome.status.is_failed() {
                report.failed += 1;
            } else {
                report.kept += 1;
            }
            log_kept(image, &reason);
        }
    }
    Ok(report)
}

/// Runs the sweep on this host's daemon and on every reachable worker node,
/// with this install's owner ID. Each runtime is swept on its own: one that
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
}

/// What one run of [`OrphanImageSweeper`] did, per runtime.
#[derive(Debug, Default)]
pub struct OrphanSweepSummary {
    /// `None` is this host, `Some(node_id)` a worker node.
    pub swept: Vec<(Option<i32>, OrphanSweepReport)>,
    /// Runtimes whose sweep failed; nothing was removed on them.
    pub failed: Vec<(Option<i32>, String)>,
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
        }
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

    /// Sweep every runtime once.
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
        if let Some(local) = &self.local {
            self.sweep_one(None, "this host", local.as_ref(), &owner, &mut summary)
                .await;
        }
        let Some(workers) = &self.workers else {
            return summary;
        };
        match workers.worker_image_daemons().await {
            Ok(nodes) => {
                for node in nodes {
                    let name = format!("worker node {} ({})", node.node_name, node.node_id);
                    self.sweep_one(
                        Some(node.node_id),
                        &name,
                        node.daemon.as_ref(),
                        &owner,
                        &mut summary,
                    )
                    .await;
                }
            }
            Err(error) => {
                tracing::error!(%error, "Could not read the worker nodes to sweep; their images wait for a later run");
                summary.failed.push((None, error.to_string()));
            }
        }
        summary
    }

    async fn sweep_one(
        &self,
        node_id: Option<i32>,
        name: &str,
        daemon: &dyn OwnedImageDaemon,
        owner: &str,
        summary: &mut OrphanSweepSummary,
    ) {
        match sweep_orphaned_project_images(self.db.as_ref(), daemon, owner, self.max_per_run).await
        {
            Ok(report) => {
                if report.orphaned == 0 {
                    tracing::debug!(
                        runtime = name,
                        labelled = report.labelled,
                        "No images of deleted projects to remove"
                    );
                } else {
                    tracing::info!(
                        runtime = name,
                        labelled = report.labelled,
                        orphaned = report.orphaned,
                        removed = report.removed,
                        kept = report.kept,
                        failed = report.failed,
                        deferred = report.deferred,
                        "Swept images built for deleted projects"
                    );
                }
                summary.swept.push((node_id, report));
            }
            Err(error) => {
                tracing::error!(
                    runtime = name,
                    %error,
                    "Could not sweep images built for deleted projects; nothing was removed there, retrying next run"
                );
                summary.failed.push((node_id, error.to_string()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use temps_docker_util::image_removal::ImageRemovalStatus;

    fn image(id: &str, project: Option<&str>, created: i64) -> OwnedImage {
        let mut labels =
            HashMap::from([(IMAGE_INSTANCE_LABEL.to_string(), "install-a".to_string())]);
        if let Some(project) = project {
            labels.insert(
                image_ownership::IMAGE_PROJECT_LABEL.to_string(),
                project.to_string(),
            );
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

    #[test]
    fn only_images_of_projects_without_a_row_are_orphans_oldest_first() {
        let images = vec![
            image("live", Some("1"), 10),
            image("gone-new", Some("2"), 30),
            image("gone-old", Some("3"), 20),
            image("unlabelled", None, 5),
            image("garbled", Some("seven"), 5),
        ];
        let existing = HashSet::from([1]);
        let orphans: Vec<&str> = select_orphans(&images, &existing)
            .into_iter()
            .map(|image| image.image_id.as_str())
            .collect();
        assert_eq!(orphans, vec!["gone-old", "gone-new"]);
    }

    /// A scripted daemon: lists `images`, reports `held` as used by a
    /// container, records what it was asked to remove, and answers `answer`.
    struct ScriptedDaemon {
        images: Vec<OwnedImage>,
        held: Vec<&'static str>,
        answer: fn(&ImageRemovalRequest) -> ImageRemovalStatus,
        asked: std::sync::Mutex<Vec<String>>,
        listed_for: std::sync::Mutex<Vec<String>>,
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
            }
        }

        fn asked(&self) -> Vec<String> {
            self.asked.lock().expect("asked mutex").clone()
        }
    }

    #[async_trait::async_trait]
    impl OwnedImageDaemon for ScriptedDaemon {
        async fn list_instance_images(
            &self,
            instance_id: &str,
        ) -> Result<Vec<OwnedImage>, OrphanSweepError> {
            self.listed_for
                .lock()
                .expect("listed mutex")
                .push(instance_id.to_string());
            Ok(self.images.clone())
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

    /// The projects that exist, then the references other projects still
    /// hold.
    fn mock_db(existing_projects: &[i32], named_elsewhere: &[&str]) -> sea_orm::DatabaseConnection {
        use sea_orm::{MockDatabase, Value};
        let projects: Vec<Value> = existing_projects
            .iter()
            .map(|id| Value::from(*id))
            .collect();
        let named: Vec<Value> = named_elsewhere
            .iter()
            .map(|reference| Value::from(reference.to_string()))
            .collect();
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([rows("id", &projects)])
            .append_query_results([rows("reference", &named)])
            .into_connection()
    }

    #[tokio::test]
    async fn a_sweep_removes_orphans_within_the_limit_and_counts_what_it_kept() {
        let mut daemon = ScriptedDaemon::new(vec![
            image("live", Some("1"), 10),
            image("gone-a", Some("2"), 20),
            image("gone-b", Some("2"), 30),
            image("gone-c", Some("3"), 40),
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
        let db = mock_db(&[1], &[]);

        let report = sweep_orphaned_project_images(&db, &daemon, "install-a", 2)
            .await
            .expect("sweep");

        assert_eq!(
            daemon.listed_for.lock().expect("listed").as_slice(),
            ["install-a"],
            "only this install's images are listed"
        );
        assert_eq!(
            daemon.asked(),
            ["gone-a:latest", "gone-b:latest"],
            "oldest orphans first, by their one tag, bounded by the limit; the live project's image is never asked"
        );
        assert_eq!(
            report,
            OrphanSweepReport {
                labelled: 4,
                orphaned: 3,
                removed: 1,
                kept: 1,
                failed: 0,
                deferred: 1,
            }
        );
    }

    /// Images that must stay never take a slot in the batch: a backlog of
    /// them cannot starve a newer orphan that could go.
    #[tokio::test]
    async fn held_shared_and_retagged_orphans_do_not_use_up_the_limit() {
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
        let db = mock_db(&[], &["gone-shared"]);

        let report = sweep_orphaned_project_images(&db, &daemon, "install-a", 1)
            .await
            .expect("sweep");

        assert_eq!(daemon.asked(), ["gone-free:latest"]);
        assert_eq!(
            report,
            OrphanSweepReport {
                labelled: 6,
                orphaned: 6,
                removed: 1,
                kept: 5,
                failed: 0,
                deferred: 0,
            }
        );
    }

    /// A worker node listed as the [`WorkerImageDaemons`] say.
    struct ScriptedWorkers(Result<Vec<(i32, std::sync::Arc<ScriptedDaemon>)>, &'static str>);

    #[async_trait::async_trait]
    impl WorkerImageDaemons for ScriptedWorkers {
        async fn worker_image_daemons(&self) -> Result<Vec<WorkerImageDaemon>, OrphanSweepError> {
            match &self.0 {
                Ok(nodes) => Ok(nodes
                    .iter()
                    .map(|(node_id, daemon)| WorkerImageDaemon {
                        node_id: *node_id,
                        node_name: format!("worker-{node_id}"),
                        daemon: daemon.clone(),
                    })
                    .collect()),
                Err(reason) => Err(OrphanSweepError::Workers {
                    source: (*reason).into(),
                }),
            }
        }
    }

    /// The identity row the owner ID is read from, then for each runtime
    /// swept the projects that exist and the references still held.
    fn sweeper_db(runtimes: usize) -> std::sync::Arc<sea_orm::DatabaseConnection> {
        use sea_orm::{MockDatabase, Value};
        let mut db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([rows("instance_id", &[Value::from("install-a".to_string())])]);
        for _ in 0..runtimes {
            db = db
                .append_query_results([rows("id", &[])])
                .append_query_results([rows("reference", &[])]);
        }
        std::sync::Arc::new(db.into_connection())
    }

    /// The sweep covers the worker nodes with this install's owner ID, the
    /// database ID joined with the data directory's marker. A process that
    /// runs no workloads sweeps its workers only.
    #[tokio::test]
    async fn the_sweeper_covers_worker_nodes_with_the_install_owner_id() {
        let dir = tempfile::tempdir().expect("temp dir");
        let marker = image_ownership::install_marker(dir.path(), true)
            .await
            .expect("marker")
            .expect("created");
        let worker = std::sync::Arc::new(ScriptedDaemon::new(vec![image("gone", Some("2"), 1)]));
        let sweeper = OrphanImageSweeper::new(sweeper_db(1))
            .with_local_daemon(None)
            .with_worker_daemons(std::sync::Arc::new(ScriptedWorkers(Ok(vec![(
                7,
                worker.clone(),
            )]))))
            .with_data_dir(dir.path().to_path_buf());

        let summary = sweeper.run().await;

        assert_eq!(
            worker.listed_for.lock().expect("listed").as_slice(),
            [format!("install-a.{marker}")]
        );
        assert_eq!(worker.asked(), ["gone:latest"]);
        assert_eq!(summary.swept.len(), 1);
        assert_eq!(summary.swept[0].0, Some(7));
        assert_eq!(summary.swept[0].1.removed, 1);
        assert!(summary.failed.is_empty(), "{summary:?}");
    }

    /// If the worker nodes cannot be read, this host is still swept, and
    /// the failure is reported rather than hidden.
    #[tokio::test]
    async fn an_unreadable_worker_list_does_not_stop_this_host_s_sweep() {
        let dir = tempfile::tempdir().expect("temp dir");
        image_ownership::install_marker(dir.path(), true)
            .await
            .expect("marker");
        let local = std::sync::Arc::new(ScriptedDaemon::new(vec![image("gone", Some("2"), 1)]));
        let sweeper = OrphanImageSweeper::new(sweeper_db(1))
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
    }

    /// Without the data directory's marker, no image can carry this
    /// install's owner ID: nothing is listed, let alone removed. A copied
    /// database in another install lands here until it builds.
    #[tokio::test]
    async fn without_the_install_marker_nothing_is_listed_or_removed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let local = std::sync::Arc::new(ScriptedDaemon::new(vec![image("gone", Some("2"), 1)]));
        let sweeper = OrphanImageSweeper::new(sweeper_db(0))
            .with_local_daemon(Some(local.clone()))
            .with_data_dir(dir.path().to_path_buf());
        let summary = sweeper.run().await;
        assert!(
            summary.swept.is_empty() && summary.failed.is_empty(),
            "{summary:?}"
        );
        assert!(local.listed_for.lock().expect("listed").is_empty());

        // Without a data directory at all, not even the database is read.
        let unconfigured = OrphanImageSweeper::new(std::sync::Arc::new(
            sea_orm::MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
        ))
        .with_local_daemon(Some(local.clone()));
        let summary = unconfigured.run().await;
        assert!(summary.swept.is_empty() && summary.failed.is_empty());
        assert!(local.listed_for.lock().expect("listed").is_empty());
    }

    #[tokio::test]
    async fn a_failed_project_lookup_removes_nothing() {
        let daemon = ScriptedDaemon::new(vec![image("gone", Some("2"), 1)]);
        let db = sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom("connection reset".to_string())])
            .into_connection();
        let result = sweep_orphaned_project_images(&db, &daemon, "install-a", 10).await;
        assert!(
            matches!(
                result,
                Err(OrphanSweepError::ProjectLookup { count: 1, .. })
            ),
            "{result:?}"
        );
        assert!(daemon.asked().is_empty());
    }

    #[tokio::test]
    async fn a_failed_reference_lookup_removes_nothing() {
        let daemon = ScriptedDaemon::new(vec![image("gone", Some("2"), 1)]);
        let db = sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([rows("id", &[])])
            .append_query_errors([sea_orm::DbErr::Custom("connection reset".to_string())])
            .into_connection();
        let result = sweep_orphaned_project_images(&db, &daemon, "install-a", 10).await;
        assert!(
            matches!(result, Err(OrphanSweepError::ReferenceLookup { .. })),
            "{result:?}"
        );
        assert!(daemon.asked().is_empty());
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

    /// Against a real daemon and database: after a project's row is gone,
    /// its labelled image is removed, while an image of that project a
    /// container was created from, an image of a live project, an image
    /// another install labelled, and an unlabelled image all survive. Skips
    /// when Docker or Postgres is unavailable.
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
        // Every step's error is kept, not returned, so the Docker resources
        // below are removed whatever fails.
        let mut swept = None;
        if prepared.is_ok() {
            match temps_entities::projects::Entity::delete_by_id(doomed.id)
                .exec(db.as_ref())
                .await
            {
                Ok(_) => {
                    swept = Some(
                        sweep_orphaned_project_images(
                            db.as_ref(),
                            &super::super::DefaultDockerClient,
                            &instance,
                            10,
                        )
                        .await,
                    );
                }
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

        // Remove only what this test created, before asserting.
        let _ = docker
            .remove_container(
                &holder,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
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
        let report = swept.expect("the sweep ran").expect("the sweep succeeds");
        assert_eq!(
            report.labelled, 3,
            "only this install's images are listed: {report:?}"
        );
        assert_eq!(report.orphaned, 2, "{report:?}");
        assert_eq!(report.removed, 1, "{report:?}");
        assert_eq!(report.kept, 1, "{report:?}");
        assert!(
            !orphan_left,
            "the unused image of the deleted project must be gone"
        );
        assert!(held_left, "an image a container was created from must stay");
        assert!(live_left, "an image of a live project must stay");
        assert!(foreign_left, "another install's image must stay");
        assert!(unlabelled_left, "an image without Temps labels must stay");
        Ok(())
    }
}
