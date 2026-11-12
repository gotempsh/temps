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
//! - only when **no container**, running or stopped, was created from the
//!   image, judged by image ID, and never forced.
//!
//! It never selects by tag text (#1373). Its cost grows with the number of
//! labelled images on this daemon, not with the size of the database, and it
//! removes at most a configured number of images per run, oldest first.

use std::collections::{BTreeSet, HashMap, HashSet};

use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use temps_docker_util::image_removal::{
    ImageRemovalOutcome, ImageRemovalRequest, MAX_IMAGE_REMOVAL_BATCH,
};

use super::image_ownership::{self, ImageOwnershipError, IMAGE_INSTANCE_LABEL};

/// An image on the daemon labelled as built by this install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedImage {
    pub image_id: String,
    pub tags: Vec<String>,
    pub labels: HashMap<String, String>,
    /// Seconds since the epoch, as the daemon reports it.
    pub created: i64,
}

/// Why an orphan sweep stopped. Nothing is removed after any of these.
#[derive(Debug, thiserror::Error)]
pub enum OrphanSweepError {
    #[error(transparent)]
    Ownership(#[from] ImageOwnershipError),

    #[error("Failed to connect to the Docker daemon to list images labelled {label}={instance_id}: {source}")]
    Connect {
        label: &'static str,
        instance_id: String,
        #[source]
        source: bollard::errors::Error,
    },

    #[error("Failed to list images labelled {label}={instance_id}: {source}")]
    ListImages {
        label: &'static str,
        instance_id: String,
        #[source]
        source: bollard::errors::Error,
    },

    #[error("Failed to check which of {count} labelled projects still exist: {source}")]
    ProjectLookup {
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
}

/// What one sweep did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OrphanSweepReport {
    /// Images labelled with this install's ID.
    pub labelled: usize,
    /// Of those, images whose project no longer exists.
    pub orphaned: usize,
    pub removed: usize,
    /// Kept because a container uses them or Docker refused (for example a
    /// tag in another repository still references the image).
    pub kept: usize,
    pub failed: usize,
    /// Orphans left for a later run by the per-run limit.
    pub deferred: usize,
}

/// The daemon operations the sweep needs, mockable in tests.
#[async_trait::async_trait]
pub trait OwnedImageDaemon: Send + Sync {
    /// Every image labelled `sh.temps.image.instance=<instance_id>`.
    async fn list_instance_images(
        &self,
        instance_id: &str,
    ) -> Result<Vec<OwnedImage>, OrphanSweepError>;

    /// Remove each image no container uses (see
    /// [`temps_docker_util::remove_unused_images`]).
    async fn remove_unused_images(
        &self,
        requests: &[ImageRemovalRequest],
    ) -> Result<Vec<ImageRemovalOutcome>, OrphanSweepError>;
}

#[async_trait::async_trait]
impl OwnedImageDaemon for super::DefaultDockerClient {
    async fn list_instance_images(
        &self,
        instance_id: &str,
    ) -> Result<Vec<OwnedImage>, OrphanSweepError> {
        let docker = bollard::Docker::connect_with_unix_defaults().map_err(|source| {
            OrphanSweepError::Connect {
                label: IMAGE_INSTANCE_LABEL,
                instance_id: instance_id.to_string(),
                source,
            }
        })?;
        let filters = HashMap::from([(
            "label".to_string(),
            vec![format!("{IMAGE_INSTANCE_LABEL}={instance_id}")],
        )]);
        let images = docker
            .list_images(Some(bollard::query_parameters::ListImagesOptions {
                all: false,
                filters: Some(filters),
                ..Default::default()
            }))
            .await
            .map_err(|source| OrphanSweepError::ListImages {
                label: IMAGE_INSTANCE_LABEL,
                instance_id: instance_id.to_string(),
                source,
            })?;
        Ok(images
            .into_iter()
            .map(|image| OwnedImage {
                image_id: image.id,
                tags: image
                    .repo_tags
                    .into_iter()
                    .filter(|tag| tag != "<none>:<none>")
                    .collect(),
                labels: image.labels,
                created: image.created,
            })
            .collect())
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

/// Remove up to `max_per_run` images this install built for projects that
/// no longer exist. See the module documentation for the safety rules.
pub async fn sweep_orphaned_project_images<C: ConnectionTrait>(
    db: &C,
    daemon: &dyn OwnedImageDaemon,
    max_per_run: usize,
) -> Result<OrphanSweepReport, OrphanSweepError> {
    // No ID yet means no build has labelled an image with one.
    let Some(instance_id) = image_ownership::load_image_instance_id(db).await? else {
        return Ok(OrphanSweepReport::default());
    };
    let images = daemon.list_instance_images(&instance_id).await?;
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
    let limit = max_per_run.min(MAX_IMAGE_REMOVAL_BATCH);
    report.deferred = orphans.len().saturating_sub(limit);
    // By ID, with that ID expected: the exact image whose labels were read.
    // A non-forced removal by ID is refused while a tag in another
    // repository still references the image, which keeps an image someone
    // retagged for themselves.
    let requests: Vec<ImageRemovalRequest> = orphans
        .iter()
        .take(limit)
        .map(|image| ImageRemovalRequest::new(image.image_id.clone(), Some(image.image_id.clone())))
        .collect();
    if requests.is_empty() {
        return Ok(report);
    }
    let outcomes = daemon.remove_unused_images(&requests).await?;
    for (outcome, image) in outcomes.iter().zip(orphans.iter()) {
        let project_id = image_ownership::labelled_project_id(&image.labels);
        if outcome.status.is_removed() {
            report.removed += 1;
            tracing::info!(
                image_id = %image.image_id,
                tags = ?image.tags,
                ?project_id,
                "Removed image built for a project that no longer exists"
            );
        } else if let Some(reason) = outcome.status.reason() {
            if outcome.status.is_failed() {
                report.failed += 1;
            } else {
                report.kept += 1;
            }
            tracing::warn!(
                image_id = %image.image_id,
                tags = ?image.tags,
                ?project_id,
                reason = %reason,
                "Kept image built for a project that no longer exists"
            );
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
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
        OwnedImage {
            image_id: id.to_string(),
            tags: vec![format!("{id}:latest")],
            labels,
            created,
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

    /// A scripted daemon: lists `images`, records what it was asked to
    /// remove, and answers with `answer` for each request.
    struct ScriptedDaemon {
        images: Vec<OwnedImage>,
        answer: fn(&ImageRemovalRequest) -> ImageRemovalStatus,
        asked: std::sync::Mutex<Vec<String>>,
        listed_for: std::sync::Mutex<Vec<String>>,
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

    fn mock_db(instance: Option<&str>, existing_projects: &[i32]) -> sea_orm::DatabaseConnection {
        use sea_orm::{MockDatabase, Value};
        use std::collections::BTreeMap;
        let identity: Vec<BTreeMap<String, Value>> = instance
            .map(|id| BTreeMap::from([("instance_id".to_string(), Value::from(id.to_string()))]))
            .into_iter()
            .collect();
        let projects: Vec<BTreeMap<String, Value>> = existing_projects
            .iter()
            .map(|id| BTreeMap::from([("id".to_string(), Value::from(*id))]))
            .collect();
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([identity])
            .append_query_results([projects])
            .into_connection()
    }

    #[tokio::test]
    async fn a_sweep_removes_orphans_within_the_limit_and_counts_what_it_kept() {
        let daemon = ScriptedDaemon {
            images: vec![
                image("live", Some("1"), 10),
                image("gone-a", Some("2"), 20),
                image("gone-b", Some("2"), 30),
                image("gone-c", Some("3"), 40),
            ],
            answer: |request| {
                if request.reference == "gone-b" {
                    ImageRemovalStatus::InUse {
                        image_id: "gone-b".to_string(),
                        container_id: "c0ffee".to_string(),
                    }
                } else {
                    ImageRemovalStatus::Removed {
                        image_id: request.reference.clone(),
                    }
                }
            },
            asked: Default::default(),
            listed_for: Default::default(),
        };
        let db = mock_db(Some("install-a"), &[1]);

        let report = sweep_orphaned_project_images(&db, &daemon, 2)
            .await
            .expect("sweep");

        assert_eq!(
            daemon.listed_for.lock().expect("listed").as_slice(),
            ["install-a"],
            "only this install's images are listed"
        );
        assert_eq!(
            daemon.asked.lock().expect("asked").as_slice(),
            ["gone-a", "gone-b"],
            "oldest orphans first, bounded by the limit; the live project's image is never asked"
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

    #[tokio::test]
    async fn without_an_install_id_nothing_is_listed_or_removed() {
        let daemon = ScriptedDaemon {
            images: vec![image("gone", Some("2"), 1)],
            answer: |_| ImageRemovalStatus::Absent,
            asked: Default::default(),
            listed_for: Default::default(),
        };
        let db = mock_db(None, &[]);
        let report = sweep_orphaned_project_images(&db, &daemon, 10)
            .await
            .expect("sweep");
        assert_eq!(report, OrphanSweepReport::default());
        assert!(daemon.listed_for.lock().expect("listed").is_empty());
        assert!(daemon.asked.lock().expect("asked").is_empty());
    }

    #[tokio::test]
    async fn a_failed_project_lookup_removes_nothing() {
        let daemon = ScriptedDaemon {
            images: vec![image("gone", Some("2"), 1)],
            answer: |_| ImageRemovalStatus::Absent,
            asked: Default::default(),
            listed_for: Default::default(),
        };
        let db = sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![std::collections::BTreeMap::from([(
                "instance_id".to_string(),
                sea_orm::Value::from("install-a".to_string()),
            )])]])
            .append_query_errors([sea_orm::DbErr::Custom("connection reset".to_string())])
            .into_connection();
        let result = sweep_orphaned_project_images(&db, &daemon, 10).await;
        assert!(
            matches!(
                result,
                Err(OrphanSweepError::ProjectLookup { count: 1, .. })
            ),
            "{result:?}"
        );
        assert!(daemon.asked.lock().expect("asked").is_empty());
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
        let instance = image_ownership::ensure_image_instance_id(db.as_ref()).await?;
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
                image_ownership::image_ownership_labels(&instance, doomed.id, 1),
            ),
            (
                &held,
                image_ownership::image_ownership_labels(&instance, doomed.id, 2),
            ),
            (
                &live_image,
                image_ownership::image_ownership_labels(&instance, live.id, 3),
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
        let mut swept = None;
        if prepared.is_ok() {
            temps_entities::projects::Entity::delete_by_id(doomed.id)
                .exec(db.as_ref())
                .await?;
            swept = Some(
                sweep_orphaned_project_images(db.as_ref(), &super::super::DefaultDockerClient, 10)
                    .await,
            );
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

        if let Err(reason) = prepared {
            println!("Test images could not be prepared ({reason}), skipping");
            return Ok(());
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
