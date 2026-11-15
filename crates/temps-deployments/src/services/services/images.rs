// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Removing a deleted project's built images.
//!
//! A project's `deployments` and `deployment_jobs` rows go with the project
//! row (`ON DELETE CASCADE`), and those rows are the only record of which
//! images its builds produced: image retention picks candidates from them,
//! deliberately not from tag text (#1373). So the deletion reads them first,
//! as it reads the containers it removes, and removes the images after the
//! rows are gone:
//!
//! - **Which images**: the `image_tag` (and `image_id`) of each successful
//!   `BuildImageJob` of the project, the record retention trusts, and each
//!   platform's own tag and ID of a multi-platform build. Pulled or uploaded
//!   images never run that job and are never touched.
//! - **Never another project's**: a tag or image ID that a deployment or build
//!   of any other project names is skipped, decided in one query.
//! - **Never one in use**: each runtime removes an image only if no container
//!   on it, running or stopped, was created from it, judged by image ID and
//!   never forced (`temps_docker_util::image_removal`).
//! - **Where**: this host, every worker node that built one of the images,
//!   and every node that ran one of the project's containers, reached the way
//!   container cleanup reaches them ([`DeploymentService::deployer_for_node`]).
//! - **Best-effort**: nothing here can fail the deletion. What a runtime could
//!   not remove is logged with the project, image and reason, and stays
//!   labelled for the nightly sweep of images whose project no longer exists.

use super::*;
use temps_core::{
    ProjectImage, ProjectImageCleanupError, ProjectImageCleanupReport, ProjectImagePlan,
};
use temps_deployer::{ImageRemovalRequest, ImageRemovalStatus, MAX_IMAGE_REMOVAL_BATCH};
use temps_docker_housekeeping::services::orphaned_images::{
    OrphanSweepError, OwnedImageDaemon, WorkerImageDaemon, WorkerImageDaemonSet, WorkerImageDaemons,
};
use temps_docker_housekeeping::services::{image_ownership, image_references};

/// Most built images one project deletion reads. Newest first: an old image
/// is the likeliest to have been removed by retention already. A project with
/// more successful builds than this leaves the oldest to the nightly sweep.
const MAX_PROJECT_IMAGES: i64 = 5_000;

/// How long one runtime gets for one batch before the deletion moves on.
const RUNTIME_REMOVAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(150);

/// How long one deletion spends removing images across all runtimes, so a
/// project spread over many slow nodes cannot hold the delete request open
/// indefinitely. Runtimes not reached in time keep their copies (logged).
const TOTAL_REMOVAL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(300);

/// Split `images` into those to remove and the references that another
/// project's deployment or build names (`shared_refs`, tags or image IDs as
/// returned by [`image_references::referenced_images`]). A tag matches with
/// or without its implicit `:latest`, an ID with or without `sha256:`.
pub(super) fn partition_shared(
    images: Vec<ProjectImage>,
    shared_refs: &HashSet<String>,
) -> (Vec<ProjectImage>, Vec<String>) {
    let mut keep = Vec::new();
    let mut shared = Vec::new();
    for image in images {
        let tag_shared = image_references::names_tag(shared_refs, &image.reference);
        let id_shared = image
            .image_id
            .as_deref()
            .is_some_and(|id| image_references::names_image_id(shared_refs, id));
        if tag_shared || id_shared {
            shared.push(image.reference);
        } else {
            keep.push(image);
        }
    }
    (keep, shared)
}

/// The removal requests for `plan`: each recorded tag, expecting its
/// recorded ID, guarded by this install's owner label so an image another
/// install built is never removed on this install's records. An unlabelled
/// image (built before the labels) is kept as well when the runtime shows
/// another install's images: a copied database records the original
/// install's tags and IDs, and an unlabelled image there may be theirs.
pub(super) fn removal_requests(
    plan: &ProjectImagePlan,
    owner: Option<String>,
) -> Vec<ImageRemovalRequest> {
    let guard = temps_deployer::OwnerGuard {
        label: image_ownership::IMAGE_INSTANCE_LABEL.to_string(),
        owner,
        keep_unlabelled_when_shared: true,
    };
    plan.images
        .iter()
        .map(|image| {
            ImageRemovalRequest::new(image.reference.clone(), image.image_id.clone())
                .with_owner_guard(guard.clone())
        })
        .collect()
}

/// This install's owner ID from `read`, bounded by `deadline`. A read that
/// fails or runs out of time yields `None`, which keeps every labelled image:
/// the safe direction when ownership cannot be proved.
async fn owner_within_deadline<F, E>(
    project_id: i32,
    deadline: tokio::time::Instant,
    read: F,
) -> Option<String>
where
    F: std::future::Future<Output = Result<Option<String>, E>>,
    E: std::fmt::Display,
{
    match tokio::time::timeout_at(deadline, read).await {
        Ok(Ok(owner)) => owner,
        Ok(Err(error)) => {
            warn!(
                project_id,
                %error,
                "Could not read this install's image owner ID; keeping every labelled image of the deleted project"
            );
            None
        }
        Err(_) => {
            warn!(
                project_id,
                deadline_secs = TOTAL_REMOVAL_DEADLINE.as_secs(),
                "Out of time reading this install's image owner ID; keeping every labelled image of the deleted project"
            );
            None
        }
    }
}

impl DeploymentService {
    async fn read_project_image_plan(&self, project_id: i32) -> Result<ProjectImagePlan, DbErr> {
        use sea_orm::ActiveEnum;
        let build_succeeded = temps_entities::types::JobStatus::Success.to_value();
        let db = self.db.as_ref();

        // Every image the project's successful builds recorded: the primary
        // tag and, for a multi-platform build, each platform's own tag with
        // its own ID when recorded. Newest first, one row per tag.
        let rows = db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                r#"
                WITH builds AS (
                    SELECT dj.id, dj.outputs
                    FROM deployment_jobs dj
                    JOIN deployments d ON d.id = dj.deployment_id
                    WHERE d.project_id = $1
                      AND dj.job_type = 'BuildImageJob'
                      AND dj.status = $2
                ),
                built AS (
                    SELECT b.id,
                           b.outputs ->> 'image_tag' AS image_tag,
                           NULLIF(b.outputs ->> 'image_id', '') AS image_id
                    FROM builds b
                    UNION ALL
                    SELECT b.id, tag.value, NULLIF(platform_id.value, '')
                    FROM builds b
                    CROSS JOIN LATERAL json_each_text(
                        CASE WHEN json_typeof(b.outputs -> 'image_tags_by_platform') = 'object'
                             THEN b.outputs -> 'image_tags_by_platform' ELSE '{}'::json END
                    ) AS tag
                    LEFT JOIN LATERAL (
                        SELECT ids.value
                        FROM json_each_text(
                            CASE WHEN json_typeof(b.outputs -> 'image_ids_by_platform') = 'object'
                                 THEN b.outputs -> 'image_ids_by_platform' ELSE '{}'::json END
                        ) AS ids
                        WHERE ids.key = tag.key
                    ) AS platform_id ON TRUE
                )
                SELECT image_tag, image_id FROM (
                    SELECT DISTINCT ON (image_tag) image_tag, image_id, id
                    FROM built
                    WHERE NULLIF(image_tag, '') IS NOT NULL
                      AND image_tag NOT LIKE '%/%'
                    -- A local multi-platform build lists its primary tag
                    -- again among the platforms, without an ID: keep the
                    -- row that recorded one.
                    ORDER BY image_tag, id DESC, (image_id IS NULL)
                ) latest
                ORDER BY id DESC
                LIMIT $3
                "#,
                [
                    project_id.into(),
                    build_succeeded.into(),
                    MAX_PROJECT_IMAGES.into(),
                ],
            ))
            .await?;
        let images = rows
            .into_iter()
            .map(|row| {
                Ok(ProjectImage {
                    reference: row.try_get("", "image_tag")?,
                    image_id: row.try_get("", "image_id")?,
                })
            })
            .collect::<Result<Vec<_>, DbErr>>()?;
        if images.is_empty() {
            return Ok(ProjectImagePlan {
                project_id,
                ..Default::default()
            });
        }

        // Every other project's reference to any of these tags or IDs, in
        // one query shared with the orphan sweep.
        let mut candidates: Vec<String> = Vec::with_capacity(images.len() * 3);
        for image in &images {
            candidates.extend(image_references::tag_spellings(&image.reference));
            if let Some(id) = &image.image_id {
                candidates.extend(image_references::image_id_spellings(id));
            }
        }
        let shared_refs =
            image_references::referenced_images(db, &candidates, Some(project_id)).await?;
        let (images, shared) = partition_shared(images, &shared_refs);

        // Runtimes that may hold the images: the nodes that built them and
        // the nodes that ran the project's containers, if they still exist.
        // This host is always asked; it answers "absent" when it has none.
        // Only reachable nodes are contacted: a down node would hold the
        // delete response open until the removal deadline.
        let node_rows = db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                r#"
                SELECT DISTINCT n.id AS node_id, n.status = ANY($2::text[]) AS reachable
                FROM (
                    SELECT dc.node_id
                    FROM deployment_containers dc
                    JOIN deployments d ON d.id = dc.deployment_id
                    WHERE d.project_id = $1 AND dc.node_id IS NOT NULL
                    UNION
                    SELECT (dj.outputs ->> 'builder_node_id')::int
                    FROM deployment_jobs dj
                    JOIN deployments d ON d.id = dj.deployment_id
                    WHERE d.project_id = $1
                      AND dj.job_type = 'BuildImageJob'
                      AND COALESCE(dj.outputs ->> 'builder_node_id', '') ~ '^[0-9]{1,9}$'
                    UNION
                    SELECT per_platform.value::int
                    FROM deployment_jobs dj
                    JOIN deployments d ON d.id = dj.deployment_id
                    CROSS JOIN LATERAL json_each_text(
                        CASE WHEN json_typeof(dj.outputs -> 'builder_node_ids_by_platform') = 'object'
                             THEN dj.outputs -> 'builder_node_ids_by_platform'
                             ELSE '{}'::json END
                    ) AS per_platform
                    WHERE d.project_id = $1
                      AND dj.job_type = 'BuildImageJob'
                      AND per_platform.value ~ '^[0-9]{1,9}$'
                ) holders
                JOIN nodes n ON n.id = holders.node_id
                ORDER BY n.id
                "#,
                [
                    project_id.into(),
                    SWEPT_NODE_STATUSES
                        .iter()
                        .map(|status| status.to_string())
                        .collect::<Vec<String>>()
                        .into(),
                ],
            ))
            .await?;
        let mut nodes = vec![None];
        let mut offline_nodes = Vec::new();
        for row in node_rows {
            let node_id = row.try_get::<i32>("", "node_id")?;
            if row.try_get::<bool>("", "reachable")? {
                nodes.push(Some(node_id));
            } else {
                offline_nodes.push(node_id);
            }
        }

        Ok(ProjectImagePlan {
            project_id,
            images,
            nodes,
            offline_nodes,
            shared,
        })
    }

    /// Remove `requests` from one runtime, adding what happened to `report`.
    /// Returns `false` if the runtime could not be asked at all.
    async fn remove_images_on_runtime(
        &self,
        project_id: i32,
        node_id: Option<i32>,
        requests: &[ImageRemovalRequest],
        deadline: tokio::time::Instant,
        report: &mut ProjectImageCleanupReport,
    ) -> bool {
        // Resolving a worker reads its row and configuration; bound it by the
        // same deadline, so a stalled read cannot hold the deletion open.
        let resolution = tokio::time::timeout(
            deadline.saturating_duration_since(tokio::time::Instant::now()),
            self.deployer_for_node(node_id),
        )
        .await;
        let resolved = match resolution {
            Ok(resolved) => resolved,
            Err(_) => {
                warn!(
                    project_id,
                    ?node_id,
                    images = requests.len(),
                    "Out of time resolving a runtime to remove a deleted project's images; they stay on it"
                );
                return false;
            }
        };
        let deployer = match resolved {
            Ok(deployer) => deployer,
            // No local daemon in this process (control-plane profile): this
            // host holds no images of ours.
            Err(DeploymentError::DockerUnavailable(_)) if node_id.is_none() => return true,
            Err(error) => {
                warn!(
                    project_id,
                    ?node_id,
                    images = requests.len(),
                    %error,
                    "Could not reach a runtime to remove a deleted project's images; they stay on it"
                );
                return false;
            }
        };
        for batch in requests.chunks(MAX_IMAGE_REMOVAL_BATCH) {
            let budget = RUNTIME_REMOVAL_TIMEOUT
                .min(deadline.saturating_duration_since(tokio::time::Instant::now()));
            let outcomes = match tokio::time::timeout(budget, deployer.remove_unused_images(batch))
                .await
            {
                Ok(Ok(outcomes)) => outcomes,
                Ok(Err(temps_deployer::DeployerError::DockerUnavailable(_)))
                    if node_id.is_none() =>
                {
                    return true;
                }
                Ok(Err(error)) => {
                    warn!(
                        project_id,
                        ?node_id,
                        images = batch.len(),
                        %error,
                        "Could not remove a deleted project's images on this runtime; they stay on it"
                    );
                    return false;
                }
                Err(_) => {
                    warn!(
                        project_id,
                        ?node_id,
                        images = batch.len(),
                        timeout_secs = budget.as_secs(),
                        "Removing a deleted project's images timed out on this runtime; the rest stay on it"
                    );
                    return false;
                }
            };
            for outcome in outcomes {
                match &outcome.status {
                    ImageRemovalStatus::Removed { image_id } => {
                        report.removed += 1;
                        info!(
                            project_id,
                            ?node_id,
                            image_ref = %outcome.reference,
                            image_id = %image_id,
                            "Removed image built for a deleted project"
                        );
                    }
                    ImageRemovalStatus::Absent => {}
                    status => {
                        let reason = status.reason().unwrap_or_default();
                        if status.is_failed() {
                            report.failed += 1;
                        } else {
                            report.skipped += 1;
                        }
                        warn!(
                            project_id,
                            ?node_id,
                            image_ref = %outcome.reference,
                            reason = %reason,
                            "Kept image built for a deleted project"
                        );
                    }
                }
            }
        }
        true
    }
}

#[async_trait::async_trait]
impl temps_core::ProjectImageCleaner for DeploymentService {
    async fn plan_project_image_cleanup(
        &self,
        project_id: i32,
    ) -> Result<ProjectImagePlan, ProjectImageCleanupError> {
        self.read_project_image_plan(project_id)
            .await
            .map_err(|error| ProjectImageCleanupError::Plan {
                project_id,
                source: Box::new(error),
            })
    }

    async fn remove_project_images(&self, plan: &ProjectImagePlan) -> ProjectImageCleanupReport {
        let mut report = ProjectImageCleanupReport {
            skipped: plan.shared.len() as u64,
            ..Default::default()
        };
        for reference in &plan.shared {
            info!(
                project_id = plan.project_id,
                image_ref = %reference,
                reason = "another project's deployment or build references it",
                "Kept image built for a deleted project"
            );
        }
        if plan.images.is_empty() {
            return report;
        }
        for node_id in &plan.offline_nodes {
            warn!(
                project_id = plan.project_id,
                node_id,
                images = plan.images.len(),
                "Worker node is not reachable; a deleted project's images stay on it until the nightly sweep reaches it"
            );
        }
        report.unreachable_runtimes += plan.offline_nodes.len() as u64;
        // The deadline covers everything after the row delete, reading the
        // owner ID included, so a stalled database or data directory cannot
        // hold the delete response and its audit entry open.
        let deadline = tokio::time::Instant::now() + TOTAL_REMOVAL_DEADLINE;
        // This install's owner ID: an image another install sharing the host
        // labelled is theirs, even when a copied database records it as
        // ours. Without the ID, every labelled image counts as someone
        // else's, so only images that predate the labels are removed.
        let owner = owner_within_deadline(
            plan.project_id,
            deadline,
            image_ownership::load_image_owner_id(self.db.as_ref(), &self.config_service.data_dir()),
        )
        .await;
        let requests = removal_requests(plan, owner);
        for node_id in &plan.nodes {
            if tokio::time::Instant::now() >= deadline {
                warn!(
                    project_id = plan.project_id,
                    ?node_id,
                    images = requests.len(),
                    deadline_secs = TOTAL_REMOVAL_DEADLINE.as_secs(),
                    "Out of time removing a deleted project's images; they stay on this runtime"
                );
                report.unreachable_runtimes += 1;
                continue;
            }
            if !self
                .remove_images_on_runtime(
                    plan.project_id,
                    *node_id,
                    &requests,
                    deadline,
                    &mut report,
                )
                .await
            {
                report.unreachable_runtimes += 1;
            }
        }
        report
    }
}

/// Node statuses whose daemons the nightly sweep covers: every status in
/// which the node is still part of the cluster and reachable.
const SWEPT_NODE_STATUSES: &[&str] = &["active", "draining", "drained"];

/// A node that may still hold images but is not contacted: the sweep keeps
/// the deleted projects it checks recorded until the node is back.
const OFFLINE_NODE_STATUS: &str = "offline";

/// A worker node's Docker daemon, reached through its agent, for the
/// nightly sweep of images whose project no longer exists. The node itself
/// checks usage while listing and again before each removal.
struct RemoteImageDaemon {
    node_name: String,
    remote: Arc<temps_deployer::remote::RemoteNodeDeployer>,
}

#[async_trait::async_trait]
impl OwnedImageDaemon for RemoteImageDaemon {
    async fn list_project_images(
        &self,
        owner: &str,
        project_id: i32,
        after: Option<&temps_deployer::ListCursor>,
    ) -> Result<temps_deployer::LabelledImages, OrphanSweepError> {
        let listing = temps_deployer::ContainerDeployer::list_owned_images(
            self.remote.as_ref(),
            owner,
            project_id,
            after,
        )
        .await
        .map_err(|error| OrphanSweepError::Worker {
            node: self.node_name.clone(),
            operation: "listing a deleted project's images",
            source: Box::new(error),
        })?;
        if listing.unlisted > 0 {
            info!(
                node = %self.node_name,
                project_id,
                listed = listing.images.len(),
                unlisted = listing.unlisted,
                "More images of a deleted project on the node than one sweep lists; the next run continues after these"
            );
        }
        Ok(listing)
    }

    async fn container_using_image(
        &self,
        image: &temps_docker_housekeeping::services::orphaned_images::OwnedImage,
    ) -> Result<Option<String>, OrphanSweepError> {
        // Reported by the node with the listing.
        Ok(image.in_use_by.clone())
    }

    async fn remove_unused_images(
        &self,
        requests: &[ImageRemovalRequest],
    ) -> Result<Vec<temps_deployer::ImageRemovalOutcome>, OrphanSweepError> {
        temps_deployer::ContainerDeployer::remove_unused_images(self.remote.as_ref(), requests)
            .await
            .map_err(|error| OrphanSweepError::Worker {
                node: self.node_name.clone(),
                operation: "removing images",
                source: Box::new(error),
            })
    }
}

#[async_trait::async_trait]
impl WorkerImageDaemons for DeploymentService {
    /// Every node still reachable: active, draining, and drained (a drained
    /// node runs no workloads but can still hold a deleted project's images,
    /// and stopping new workloads must not stop housekeeping). An offline
    /// node is not contacted, and one that cannot be reached is logged; both
    /// are reported as unreachable, so the deleted projects stay recorded
    /// until they have been checked. All rows are read in one query and each
    /// worker's client is built from its row.
    async fn worker_image_daemons(&self) -> Result<WorkerImageDaemonSet, OrphanSweepError> {
        let candidates = nodes::Entity::find()
            .filter(
                nodes::Column::Status.is_in(
                    SWEPT_NODE_STATUSES
                        .iter()
                        .copied()
                        .chain([OFFLINE_NODE_STATUS]),
                ),
            )
            .order_by_asc(nodes::Column::Id)
            .all(self.db.as_ref())
            .await
            .map_err(|error| OrphanSweepError::Workers {
                source: Box::new(error),
            })?;
        let mut set = WorkerImageDaemonSet::default();
        for node in candidates {
            let node_id = node.id;
            let node_name = node.name.clone();
            if node.status == OFFLINE_NODE_STATUS {
                set.unreachable.push(node_id);
                continue;
            }
            match self.remote_container_operations(node).await {
                Ok(operations) => match operations.runtime_health_remote() {
                    Some(remote) => set.reachable.push(WorkerImageDaemon {
                        node_id,
                        node_name: node_name.clone(),
                        daemon: Arc::new(RemoteImageDaemon { node_name, remote }),
                    }),
                    None => debug!(node_id, "Node has no remote agent; not sweeping its images"),
                },
                Err(error) => {
                    warn!(
                        node_id,
                        node = %node_name,
                        %error,
                        "Could not reach a worker node to sweep images of deleted projects; it is swept on a later run"
                    );
                    set.unreachable.push(node_id);
                }
            }
        }
        Ok(set)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(reference: &str, id: Option<&str>) -> ProjectImage {
        ProjectImage {
            reference: reference.to_string(),
            image_id: id.map(str::to_string),
        }
    }

    const ID: &str = "sha256:abababababababababababababababababababababababababababababababab";

    #[test]
    fn an_image_another_project_names_by_tag_or_by_id_is_kept() {
        let images = vec![
            image("app-1:latest", None),
            image("app-2:latest", Some(ID)),
            image("app-3:latest", Some("sha256:cd")),
        ];
        // Another project deploys app-1 by tag, and produced app-2's image
        // (same ID, its own tag) from an identical build.
        // Another project deploys app-1 by its bare name, and its identical
        // build recorded app-2's image ID without the `sha256:` prefix.
        let shared = HashSet::from([
            "app-1".to_string(),
            ID.trim_start_matches("sha256:").to_string(),
        ]);
        let (keep, kept_shared) = partition_shared(images, &shared);
        assert_eq!(keep, vec![image("app-3:latest", Some("sha256:cd"))]);
        assert_eq!(kept_shared, vec!["app-1:latest", "app-2:latest"]);
    }

    /// A read of the owner ID that never answers is cut off at the deadline,
    /// and the cleanup goes on keeping every labelled image.
    #[tokio::test]
    async fn a_stalled_owner_read_is_bounded_by_the_deadline() {
        let short = std::time::Duration::from_millis(50);
        let stalled = std::future::pending::<Result<Option<String>, std::io::Error>>();
        let started = tokio::time::Instant::now();
        assert_eq!(
            owner_within_deadline(7, started + short, stalled).await,
            None
        );
        let waited = started.elapsed();
        assert!(
            waited >= short && waited < std::time::Duration::from_secs(5),
            "cut off at the deadline, not left waiting: {waited:?}"
        );

        let far = tokio::time::Instant::now() + TOTAL_REMOVAL_DEADLINE;
        let failed =
            async { Err::<Option<String>, _>(std::io::Error::other("database unavailable")) };
        assert_eq!(owner_within_deadline(7, far, failed).await, None);
        let answered = async { Ok::<_, std::io::Error>(Some("install-a.marker".to_string())) };
        assert_eq!(
            owner_within_deadline(7, far, answered).await.as_deref(),
            Some("install-a.marker")
        );
    }

    /// Every request carries this install's owner check, so a copied
    /// database cannot remove another install's labelled images.
    #[test]
    fn removal_requests_keep_images_another_install_labelled() {
        let plan = ProjectImagePlan {
            project_id: 7,
            images: vec![image("app-1:latest", Some(ID))],
            nodes: vec![None],
            offline_nodes: Vec::new(),
            shared: Vec::new(),
        };
        let requests = removal_requests(&plan, Some("install-a.marker".to_string()));
        assert_eq!(requests.len(), 1);
        let guard = requests[0].owner_guard.as_ref().expect("guarded");
        assert_eq!(guard.label, image_ownership::IMAGE_INSTANCE_LABEL);
        assert_eq!(guard.owner.as_deref(), Some("install-a.marker"));
        assert!(
            guard.keep_unlabelled_when_shared,
            "an unlabelled image on a host another install shares may be theirs"
        );
        assert_eq!(requests[0].expected_image_id.as_deref(), Some(ID));
        // Without an owner ID the guard still applies: labelled images are
        // all someone else's.
        let unknown = removal_requests(&plan, None);
        assert_eq!(
            unknown[0].owner_guard.as_ref().map(|g| g.owner.clone()),
            Some(None)
        );
    }

    #[test]
    fn with_no_other_references_every_image_is_planned() {
        let images = vec![image("app-1:latest", Some(ID))];
        let (keep, shared) = partition_shared(images.clone(), &HashSet::new());
        assert_eq!(keep, images);
        assert!(shared.is_empty());
    }

    use sea_orm::{ActiveModelTrait, EntityTrait, Set};
    use temps_core::ProjectImageCleaner as _;
    use temps_entities::{deployment_jobs, types::JobStatus};

    async fn insert_project(
        db: &sea_orm::DatabaseConnection,
        slug: &str,
    ) -> Result<(projects::Model, environments::Model), DbErr> {
        let now = chrono::Utc::now();
        let project = projects::ActiveModel {
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
        .await?;
        let environment = environments::ActiveModel {
            project_id: Set(project.id),
            name: Set("Production".to_string()),
            slug: Set("production".to_string()),
            host: Set(format!("{slug}.example.test")),
            upstreams: Set(temps_entities::upstream_config::UpstreamList::default()),
            subdomain: Set(format!("{slug}.example.test")),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await?;
        Ok((project, environment))
    }

    /// A deployment of `project` whose build recorded `image_tag` (and
    /// `image_id`), as a successful `BuildImageJob` does.
    async fn record_build(
        db: &sea_orm::DatabaseConnection,
        project: &projects::Model,
        environment: &environments::Model,
        slug: &str,
        image_tag: &str,
        image_id: Option<&str>,
    ) -> Result<deployments::Model, DbErr> {
        let now = chrono::Utc::now();
        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set(slug.to_string()),
            state: Set("deployed".to_string()),
            image_name: Set(Some(image_tag.to_string())),
            metadata: Set(Some(deployments::DeploymentMetadata::default())),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await?;
        deployment_jobs::ActiveModel {
            deployment_id: Set(deployment.id),
            job_id: Set("build_image".to_string()),
            job_type: Set("BuildImageJob".to_string()),
            name: Set("Build Container Image".to_string()),
            log_id: Set(format!("build-log-{}", deployment.id)),
            status: Set(JobStatus::Success),
            outputs: Set(Some(serde_json::json!({
                "image_tag": image_tag,
                "image_id": image_id,
                "builder_node_id": null,
            }))),
            ..Default::default()
        }
        .insert(db)
        .await?;
        Ok(deployment)
    }

    async fn build_unique_image(
        docker: &bollard::Docker,
        tag: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        use futures::StreamExt as _;
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
                .rm(true)
                .build(),
            None,
            Some(bollard::body_full(bytes::Bytes::from(context))),
        );
        while let Some(step) = build.next().await {
            step?;
        }
        Ok(docker.inspect_image(tag).await?.id.unwrap_or_default())
    }

    /// Deleting a project, against a real database and daemon: its unused
    /// image goes; its image a container was created from, its image
    /// another project deploys, and an image no project recorded all stay;
    /// a recorded image already gone is simply not counted. Skips when
    /// Docker or Postgres is unavailable.
    #[tokio::test]
    async fn deleting_a_project_removes_only_its_unused_unshared_images_on_a_real_daemon(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use bollard::query_parameters::{
            CreateContainerOptionsBuilder, CreateImageOptionsBuilder, RemoveContainerOptions,
            RemoveImageOptions,
        };
        use futures::StreamExt as _;

        let Ok(docker) = bollard::Docker::connect_with_local_defaults() else {
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
        let tag = |name: &str| format!("temps-project-images-test-{name}-{nonce}:latest");
        let unused = tag("unused");
        let held = tag("held");
        let shared = tag("shared");
        let unrelated = tag("unrelated");
        let gone = tag("gone");
        let holder = format!("temps-project-images-test-holder-{nonce}");

        let mut ids = HashMap::new();
        let mut prepared: Result<(), String> = Ok(());
        for image_tag in [&unused, &held, &shared, &unrelated] {
            match build_unique_image(&docker, image_tag).await {
                Ok(id) => {
                    ids.insert(image_tag.clone(), id);
                }
                Err(error) => {
                    prepared = Err(format!("build {image_tag}: {error}"));
                    break;
                }
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

        let mut outcome = None;
        if prepared.is_ok() {
            let (doomed, doomed_env) =
                insert_project(db.as_ref(), &format!("doomed-{nonce}")).await?;
            let (other, other_env) = insert_project(db.as_ref(), &format!("other-{nonce}")).await?;
            for (index, image_tag) in [&unused, &held, &shared, &gone].into_iter().enumerate() {
                record_build(
                    db.as_ref(),
                    &doomed,
                    &doomed_env,
                    &format!("doomed-{nonce}-{index}"),
                    image_tag,
                    ids.get(image_tag).map(String::as_str),
                )
                .await?;
            }
            // Another project deploys the shared image (a promotion).
            deployments::ActiveModel {
                project_id: Set(other.id),
                environment_id: Set(other_env.id),
                slug: Set(format!("other-{nonce}-1")),
                state: Set("deployed".to_string()),
                image_name: Set(Some(shared.clone())),
                metadata: Set(Some(deployments::DeploymentMetadata::default())),
                created_at: Set(chrono::Utc::now()),
                updated_at: Set(chrono::Utc::now()),
                ..Default::default()
            }
            .insert(db.as_ref())
            .await?;

            let runtime: Arc<dyn temps_deployer::ContainerDeployer> =
                Arc::new(temps_deployer::docker::DockerRuntime::new(
                    Arc::new(docker.clone()),
                    false,
                    "bridge".to_string(),
                ));
            let service = super::super::tests::create_cleanup_service_for_test(db.clone(), runtime);
            let plan = service.plan_project_image_cleanup(doomed.id).await?;
            // The rows go before the images, as in the teardown.
            projects::Entity::delete_by_id(doomed.id)
                .exec(db.as_ref())
                .await?;
            let report = service.remove_project_images(&plan).await;
            outcome = Some((plan, report));
        }

        let present = |image_tag: String| {
            let docker = docker.clone();
            async move { docker.inspect_image(&image_tag).await.is_ok() }
        };
        let unused_left = present(unused.clone()).await;
        let held_left = present(held.clone()).await;
        let shared_left = present(shared.clone()).await;
        let unrelated_left = present(unrelated.clone()).await;

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
        for image_tag in [&unused, &held, &shared, &unrelated] {
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
        let (plan, report) = outcome.expect("the cleanup ran");
        assert_eq!(plan.shared, vec![shared.clone()], "{plan:?}");
        assert_eq!(
            plan.nodes,
            vec![None],
            "only this host holds the images: {plan:?}"
        );
        assert_eq!(
            report,
            ProjectImageCleanupReport {
                removed: 1,
                skipped: 2,
                failed: 0,
                unreachable_runtimes: 0,
            }
        );
        assert!(!unused_left, "the project's unused image must be gone");
        assert!(held_left, "an image a container was created from must stay");
        assert!(shared_left, "an image another project deploys must stay");
        assert!(unrelated_left, "an image no project recorded must stay");
        Ok(())
    }

    /// Only reachable nodes are contacted at deletion: a builder node that is
    /// offline is left out of the plan's runtimes (and reported), so a down
    /// node cannot hold the delete response open. Against a real database;
    /// skips without Postgres.
    #[tokio::test]
    async fn offline_builder_nodes_are_not_contacted_at_deletion(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let test_db = match temps_test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) => {
                println!("Database unavailable, skipping: {error}");
                return Ok(());
            }
        };
        let db = test_db.connection_arc();
        for (id, status) in [(3, "active"), (4, "offline"), (5, "drained")] {
            nodes::ActiveModel::from(node_row(id, status))
                .insert(db.as_ref())
                .await?;
        }
        let (project, environment) = insert_project(db.as_ref(), "built-on-workers").await?;
        let deployment = deployments::ActiveModel {
            project_id: Set(project.id),
            environment_id: Set(environment.id),
            slug: Set("built-on-workers-1".to_string()),
            state: Set("deployed".to_string()),
            metadata: Set(Some(deployments::DeploymentMetadata::default())),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;
        deployment_jobs::ActiveModel {
            deployment_id: Set(deployment.id),
            job_id: Set("build_image".to_string()),
            job_type: Set("BuildImageJob".to_string()),
            name: Set("Build Container Image".to_string()),
            log_id: Set("built-on-workers".to_string()),
            status: Set(JobStatus::Success),
            outputs: Set(Some(serde_json::json!({
                "image_tag": "built-on-workers-1:latest",
                "image_id": "sha256:ee",
                "builder_node_ids_by_platform": {
                    "linux/amd64": 3,
                    "linux/arm64": 4,
                    "linux/riscv64": 5
                }
            }))),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;
        let runtime: Arc<dyn temps_deployer::ContainerDeployer> = Arc::new(
            temps_deployer::remote::RemoteNodeDeployer::new(
                "http://127.0.0.1:9".to_string(),
                "unused".to_string(),
                "unused".to_string(),
            )
            .expect("create remote deployer"),
        );
        let service = super::super::tests::create_cleanup_service_for_test(db.clone(), runtime);

        let plan = service.plan_project_image_cleanup(project.id).await?;

        assert_eq!(plan.nodes, vec![None, Some(3), Some(5)], "{plan:?}");
        assert_eq!(plan.offline_nodes, vec![4], "{plan:?}");
        Ok(())
    }

    /// An offline node in the plan is counted as unreachable without being
    /// contacted.
    #[tokio::test]
    async fn offline_nodes_count_as_unreachable_without_being_asked() {
        let runtime: Arc<dyn temps_deployer::ContainerDeployer> = Arc::new(
            temps_deployer::remote::RemoteNodeDeployer::new(
                "http://127.0.0.1:9".to_string(),
                "unused".to_string(),
                "unused".to_string(),
            )
            .expect("create remote deployer"),
        );
        let db = Arc::new(sea_orm::MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let service = super::super::tests::create_cleanup_service_for_test(db, runtime);
        let plan = ProjectImagePlan {
            project_id: 7,
            images: vec![image("app-1:latest", Some(ID))],
            nodes: Vec::new(),
            offline_nodes: vec![4, 6],
            shared: Vec::new(),
        };
        let started = std::time::Instant::now();
        let report = service.remove_project_images(&plan).await;
        assert_eq!(report.unreachable_runtimes, 2);
        assert_eq!(report.removed + report.failed, 0);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    /// A multi-platform build's per-platform images are planned with their
    /// own IDs, and one another project's build also produced is skipped.
    /// Against a real database; skips without Postgres.
    #[tokio::test]
    async fn every_platform_image_of_a_build_is_planned() -> Result<(), Box<dyn std::error::Error>>
    {
        let test_db = match temps_test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) => {
                println!("Database unavailable, skipping: {error}");
                return Ok(());
            }
        };
        let db = test_db.connection_arc();
        let (doomed, doomed_env) = insert_project(db.as_ref(), "multi-arch").await?;
        let (other, other_env) = insert_project(db.as_ref(), "other").await?;
        let insert_build =
            |deployment_id: i32, outputs: serde_json::Value| deployment_jobs::ActiveModel {
                deployment_id: Set(deployment_id),
                job_id: Set("build_image".to_string()),
                job_type: Set("BuildImageJob".to_string()),
                name: Set("Build Container Image".to_string()),
                log_id: Set(format!("build-log-{deployment_id}")),
                status: Set(JobStatus::Success),
                outputs: Set(Some(outputs)),
                ..Default::default()
            };
        let deployment = record_build(
            db.as_ref(),
            &doomed,
            &doomed_env,
            "multi-arch-1",
            "multi-arch-1:latest",
            Some("sha256:aa"),
        )
        .await?;
        insert_build(
            deployment.id,
            serde_json::json!({
                "image_tag": "multi-arch-1:latest",
                "image_id": "sha256:aa",
                "image_tags_by_platform": {
                    "linux/arm64": "multi-arch-1:latest-arm64",
                    "linux/riscv64": "multi-arch-1:latest-riscv64"
                },
                "image_ids_by_platform": { "linux/arm64": "sha256:bb", "linux/riscv64": "sha256:dd" }
            }),
        )
        .insert(db.as_ref())
        .await?;
        // A local multi-platform build: the primary tag is listed again
        // among the platforms, and no per-platform IDs are recorded. The
        // primary must keep its recorded ID.
        let local = deployments::ActiveModel {
            project_id: Set(doomed.id),
            environment_id: Set(doomed_env.id),
            slug: Set("multi-arch-2".to_string()),
            state: Set("deployed".to_string()),
            metadata: Set(Some(deployments::DeploymentMetadata::default())),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(db.as_ref())
        .await?;
        insert_build(
            local.id,
            serde_json::json!({
                "image_tag": "multi-arch-2:latest",
                "image_id": "sha256:ee",
                "image_tags_by_platform": {
                    "linux/amd64": "multi-arch-2:latest",
                    "linux/arm64": "multi-arch-2:latest-arm64"
                }
            }),
        )
        .insert(db.as_ref())
        .await?;
        // Another project deploys the riscv64 image (a promotion of that
        // platform's tag).
        record_build(
            db.as_ref(),
            &other,
            &other_env,
            "other-1",
            "multi-arch-1:latest-riscv64",
            Some("sha256:dd"),
        )
        .await?;

        let runtime: Arc<dyn temps_deployer::ContainerDeployer> = Arc::new(
            temps_deployer::remote::RemoteNodeDeployer::new(
                "http://127.0.0.1:9".to_string(),
                "unused".to_string(),
                "unused".to_string(),
            )
            .expect("create remote deployer"),
        );
        let service = super::super::tests::create_cleanup_service_for_test(db.clone(), runtime);
        let mut plan = service.plan_project_image_cleanup(doomed.id).await?;
        plan.images.sort_by(|a, b| a.reference.cmp(&b.reference));

        assert_eq!(
            plan.images,
            vec![
                image("multi-arch-1:latest", Some("sha256:aa")),
                image("multi-arch-1:latest-arm64", Some("sha256:bb")),
                image("multi-arch-2:latest", Some("sha256:ee")),
                image("multi-arch-2:latest-arm64", None),
            ]
        );
        assert_eq!(plan.shared, vec!["multi-arch-1:latest-riscv64".to_string()]);
        Ok(())
    }

    /// A runtime that cannot be asked is counted and logged, and the call
    /// still returns: the deletion it is part of goes on.
    #[tokio::test]
    async fn an_unreachable_runtime_does_not_abort_the_cleanup() {
        let runtime: Arc<dyn temps_deployer::ContainerDeployer> = Arc::new(
            temps_deployer::remote::RemoteNodeDeployer::new(
                "http://127.0.0.1:9".to_string(),
                "unused".to_string(),
                "unreachable".to_string(),
            )
            .expect("create remote deployer"),
        );
        let db = Arc::new(sea_orm::MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let service = super::super::tests::create_cleanup_service_for_test(db, runtime);
        let plan = ProjectImagePlan {
            project_id: 7,
            images: vec![image("app-1:latest", Some(ID)), image("app-2:latest", None)],
            nodes: vec![None],
            offline_nodes: Vec::new(),
            shared: vec!["app-3:latest".to_string()],
        };

        let report = service.remove_project_images(&plan).await;

        assert_eq!(
            report,
            ProjectImageCleanupReport {
                removed: 0,
                skipped: 1,
                failed: 0,
                unreachable_runtimes: 1,
            }
        );
    }

    #[tokio::test]
    async fn a_project_without_built_images_plans_nothing() {
        let db = Arc::new(
            sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([
                    Vec::<std::collections::BTreeMap<String, sea_orm::Value>>::new(),
                ])
                .into_connection(),
        );
        let runtime: Arc<dyn temps_deployer::ContainerDeployer> = Arc::new(
            temps_deployer::remote::RemoteNodeDeployer::new(
                "http://127.0.0.1:9".to_string(),
                "unused".to_string(),
                "unused".to_string(),
            )
            .expect("create remote deployer"),
        );
        let service = super::super::tests::create_cleanup_service_for_test(db, runtime);
        let plan = service.plan_project_image_cleanup(7).await.expect("plan");
        assert_eq!(
            plan,
            ProjectImagePlan {
                project_id: 7,
                ..Default::default()
            }
        );
        assert_eq!(
            service.remove_project_images(&plan).await,
            ProjectImageCleanupReport::default()
        );
    }

    #[tokio::test]
    async fn a_failed_read_is_a_typed_plan_error_naming_the_project() {
        let db = Arc::new(
            sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors([DbErr::Custom("connection reset".to_string())])
                .into_connection(),
        );
        let runtime: Arc<dyn temps_deployer::ContainerDeployer> = Arc::new(
            temps_deployer::remote::RemoteNodeDeployer::new(
                "http://127.0.0.1:9".to_string(),
                "unused".to_string(),
                "unused".to_string(),
            )
            .expect("create remote deployer"),
        );
        let service = super::super::tests::create_cleanup_service_for_test(db, runtime);
        let error = service
            .plan_project_image_cleanup(7)
            .await
            .expect_err("a database error fails the plan");
        let message = error.to_string();
        assert!(message.contains("project 7"), "{message}");
        assert!(message.contains("connection reset"), "{message}");
    }

    /// A worker's images are listed and removed through its agent, and the
    /// node's own usage report is what keeps an in-use image.
    #[tokio::test]
    async fn a_worker_node_is_swept_through_its_agent() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock agent");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut request = Vec::new();
            let mut chunk = [0_u8; 4096];
            // Headers, then a body small enough to arrive with them or next.
            while !String::from_utf8_lossy(&request).contains("\"owner\"") {
                let read = stream.read(&mut chunk).await.expect("read");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
            }
            let body = r#"{"success":true,"data":{"images":[{"image_id":"sha256:aa","tags":["app-1:latest"],"labels":{},"created":1,"in_use_by":"c0ffee"}],"unlisted":0},"error":null}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.expect("write");
            String::from_utf8_lossy(&request).into_owned()
        });
        let daemon = RemoteImageDaemon {
            node_name: "worker-1".to_string(),
            remote: Arc::new(
                temps_deployer::remote::RemoteNodeDeployer::new(
                    format!("http://{address}"),
                    "token".to_string(),
                    "worker-1".to_string(),
                )
                .expect("remote"),
            ),
        };
        let images = daemon
            .list_project_images("install-a.marker-b", 7, None)
            .await
            .expect("listing")
            .images;
        let request = server.await.expect("mock agent");
        assert!(
            request.contains(r#""project_id":7"#),
            "one deleted project's images are asked for: {request}"
        );
        assert_eq!(images.len(), 1);
        assert_eq!(
            daemon
                .container_using_image(&images[0])
                .await
                .expect("usage"),
            Some("c0ffee".to_string())
        );
    }

    /// The labels a worker node filters by are the ones the builds stamp.
    #[test]
    fn the_node_lists_by_the_labels_builds_carry() {
        assert_eq!(
            temps_deployer::IMAGE_OWNER_LABEL,
            image_ownership::IMAGE_INSTANCE_LABEL
        );
        assert_eq!(
            temps_deployer::IMAGE_PROJECT_LABEL,
            image_ownership::IMAGE_PROJECT_LABEL
        );
    }

    /// An unreachable node fails as a worker error naming it, so the sweep
    /// logs it and goes on with the others.
    #[tokio::test]
    async fn an_unreachable_worker_names_itself_in_the_error() {
        let daemon = RemoteImageDaemon {
            node_name: "worker-9".to_string(),
            remote: Arc::new(
                temps_deployer::remote::RemoteNodeDeployer::new(
                    "http://127.0.0.1:9".to_string(),
                    "token".to_string(),
                    "worker-9".to_string(),
                )
                .expect("remote"),
            ),
        };
        let error = daemon
            .list_project_images("install-a.marker-b", 7, None)
            .await
            .expect_err("nothing answers");
        assert!(error.to_string().contains("worker-9"), "{error}");
    }

    fn node_row(id: i32, status: &str) -> nodes::Model {
        let now = chrono::Utc::now();
        nodes::Model {
            id,
            name: format!("worker-{id}"),
            token_hash: "hash".into(),
            // No agent token: the node cannot be reached.
            token_encrypted: None,
            address: "https://203.0.113.10:3100".into(),
            private_address: "10.0.0.5".into(),
            public_endpoint: None,
            wg_public_key: None,
            role: "worker".into(),
            status: status.into(),
            labels: serde_json::json!({}),
            capacity: serde_json::json!({}),
            last_heartbeat: None,
            edge_public_key: None,
            compute_cidr: None,
            architecture: None,
            underlay_address: None,
            mesh_wg_public_key: None,
            mesh_wg_endpoint: None,
            mesh_wg_address: None,
            dns_resolver_running: None,
            dns_resolver_tasks_alive: None,
            dns_resolver_last_sync_at: None,
            dns_resolver_consecutive_failures: 0,
            dns_resolver_last_error: None,
            dns_resolver_record_count: None,
            deployments_without_resolver: None,
            failover_at: None,
            drain_requested_at: None,
            build_limits: None,
            public_ingress_enabled: false,
            public_ingress_running: None,
            public_ingress_last_error: None,
            public_ingress_certificate_count: None,
            public_ingress_route_count: None,
            public_ingress_unsupported_route_count: None,
            public_ingress_unsupported_reasons: serde_json::json!([]),
            occupied_ranges: None,
            overlay_status: None,
            overlay_last_error: None,
            overlay_ready_cidr: None,
            overlay_remediation: None,
            overlay_network_name: None,
            overlay_vxlan_vni: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Every reachable status is swept, drained included, and all nodes are
    /// read in one query: building a worker's client reads no row of its
    /// own. A node that cannot be reached, or is offline (not contacted),
    /// is reported as unreachable instead of failing the whole sweep.
    #[tokio::test]
    async fn worker_nodes_are_read_in_one_query_and_unreachable_ones_reported() {
        let db = Arc::new(
            sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![
                    node_row(3, "active"),
                    node_row(4, "drained"),
                    node_row(5, "offline"),
                ]])
                .into_connection(),
        );
        let runtime: Arc<dyn temps_deployer::ContainerDeployer> = Arc::new(
            temps_deployer::remote::RemoteNodeDeployer::new(
                "http://127.0.0.1:9".to_string(),
                "unused".to_string(),
                "unused".to_string(),
            )
            .expect("remote"),
        );
        let service = super::super::tests::create_cleanup_service_for_test(db.clone(), runtime);
        let set = service
            .worker_image_daemons()
            .await
            .expect("the node list is read");
        assert!(set.reachable.is_empty(), "no node has an agent token");
        assert_eq!(set.unreachable, [3, 4, 5]);
        drop(service);

        let log = Arc::try_unwrap(db)
            .expect("the service released the connection")
            .into_transaction_log();
        assert_eq!(log.len(), 1, "one query for every node: {log:?}");
        let sql = format!("{:?}", log[0]);
        for status in SWEPT_NODE_STATUSES.iter().chain(&[OFFLINE_NODE_STATUS]) {
            assert!(sql.contains(status), "{status} is read: {sql}");
        }

        let failing = super::super::tests::create_cleanup_service_for_test(
            Arc::new(
                sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
                    .append_query_errors([DbErr::Custom("connection reset".to_string())])
                    .into_connection(),
            ),
            Arc::new(
                temps_deployer::remote::RemoteNodeDeployer::new(
                    "http://127.0.0.1:9".to_string(),
                    "unused".to_string(),
                    "unused".to_string(),
                )
                .expect("remote"),
            ),
        );
        assert!(matches!(
            failing.worker_image_daemons().await,
            Err(OrphanSweepError::Workers { .. })
        ));
    }
}
