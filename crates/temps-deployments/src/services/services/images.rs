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
use temps_docker_housekeeping::services::image_references;
use temps_docker_housekeeping::services::orphaned_images::{
    OrphanSweepError, OwnedImageDaemon, WorkerImageDaemon, WorkerImageDaemons,
};

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
        let node_rows = db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                r#"
                SELECT DISTINCT n.id AS node_id
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
                [project_id.into()],
            ))
            .await?;
        let mut nodes = vec![None];
        for row in node_rows {
            nodes.push(Some(row.try_get::<i32>("", "node_id")?));
        }

        Ok(ProjectImagePlan {
            project_id,
            images,
            nodes,
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
        let requests: Vec<ImageRemovalRequest> = plan
            .images
            .iter()
            .map(|image| ImageRemovalRequest::new(image.reference.clone(), image.image_id.clone()))
            .collect();
        let deadline = tokio::time::Instant::now() + TOTAL_REMOVAL_DEADLINE;
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

/// A worker node's Docker daemon, reached through its agent, for the
/// nightly sweep of images whose project no longer exists. The node itself
/// checks usage while listing and again before each removal.
struct RemoteImageDaemon {
    node_name: String,
    remote: Arc<temps_deployer::remote::RemoteNodeDeployer>,
}

#[async_trait::async_trait]
impl OwnedImageDaemon for RemoteImageDaemon {
    async fn list_instance_images(
        &self,
        owner: &str,
    ) -> Result<
        Vec<temps_docker_housekeeping::services::orphaned_images::OwnedImage>,
        OrphanSweepError,
    > {
        let listing =
            temps_deployer::ContainerDeployer::list_owned_images(self.remote.as_ref(), owner)
                .await
                .map_err(|error| OrphanSweepError::Worker {
                    node: self.node_name.clone(),
                    operation: "listing this install's images",
                    source: Box::new(error),
                })?;
        if listing.unlisted > 0 {
            warn!(
                node = %self.node_name,
                listed = listing.images.len(),
                unlisted = listing.unlisted,
                "More of this install's images on the node than one sweep lists; the newest wait for a later run"
            );
        }
        Ok(listing.images)
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
    /// Active and draining nodes, reached the way container cleanup reaches
    /// them. A node that cannot be reached is left out and logged; its
    /// images are swept on a later night.
    async fn worker_image_daemons(&self) -> Result<Vec<WorkerImageDaemon>, OrphanSweepError> {
        let candidates: Vec<(i32, String)> = nodes::Entity::find()
            .filter(nodes::Column::Status.is_in(["active", "draining"]))
            .select_only()
            .column(nodes::Column::Id)
            .column(nodes::Column::Name)
            .order_by_asc(nodes::Column::Id)
            .into_tuple()
            .all(self.db.as_ref())
            .await
            .map_err(|error| OrphanSweepError::Workers {
                source: Box::new(error),
            })?;
        let mut daemons = Vec::with_capacity(candidates.len());
        for (node_id, node_name) in candidates {
            match self.container_operations_for_node(Some(node_id)).await {
                Ok(operations) => match operations.runtime_health_remote() {
                    Some(remote) => daemons.push(WorkerImageDaemon {
                        node_id,
                        node_name: node_name.clone(),
                        daemon: Arc::new(RemoteImageDaemon { node_name, remote }),
                    }),
                    None => debug!(node_id, "Node has no remote agent; not sweeping its images"),
                },
                Err(error) => warn!(
                    node_id,
                    node = %node_name,
                    %error,
                    "Could not reach a worker node to sweep images of deleted projects; it is swept on a later run"
                ),
            }
        }
        Ok(daemons)
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
        // Another project's identical build produced the riscv64 image.
        let other_deployment = record_build(
            db.as_ref(),
            &other,
            &other_env,
            "other-1",
            "other-1:latest",
            Some("sha256:dd"),
        )
        .await?;
        let _ = other_deployment;

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
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request).await.expect("read");
            let body = r#"{"success":true,"data":{"images":[{"image_id":"sha256:aa","tags":["app-1:latest"],"labels":{},"created":1,"in_use_by":"c0ffee"}],"unlisted":0},"error":null}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.expect("write");
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
            .list_instance_images("install-a.marker-b")
            .await
            .expect("listing");
        server.await.expect("mock agent");
        assert_eq!(images.len(), 1);
        assert_eq!(
            daemon
                .container_using_image(&images[0])
                .await
                .expect("usage"),
            Some("c0ffee".to_string())
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
            .list_instance_images("install-a.marker-b")
            .await
            .expect_err("nothing answers");
        assert!(error.to_string().contains("worker-9"), "{error}");
    }

    /// Nodes are read in one query; one that cannot be reached is left out
    /// instead of failing the whole sweep.
    #[tokio::test]
    async fn worker_nodes_that_cannot_be_reached_are_left_out() {
        use sea_orm::Value;
        use std::collections::BTreeMap;
        let db = Arc::new(
            sea_orm::MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![BTreeMap::from([
                    ("id".to_string(), Value::from(3_i32)),
                    ("name".to_string(), Value::from("gone-node".to_string())),
                ])]])
                // The node's own row has vanished since: not reachable.
                .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
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
        let service = super::super::tests::create_cleanup_service_for_test(db, runtime);
        let daemons = service
            .worker_image_daemons()
            .await
            .expect("the node list is read");
        assert!(daemons.is_empty());

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
