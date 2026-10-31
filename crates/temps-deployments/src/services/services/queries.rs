// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Deployment reads: listings, lookups, captured container logs, jobs,
//! activity and the row-to-DTO mapping shared by the write paths.

use super::domains::deployment_url_from_settings;
use super::*;

/// A public git repository + branch, returned only for projects whose repo
/// is actually public -- see [`DeploymentService::get_public_repo_reference`].
#[derive(Clone, Debug)]
pub struct RepoReference {
    pub owner: String,
    pub repo: String,
    pub branch: String,
}

impl DeploymentService {
    /// Return the currently served deployment media for each requested project.
    ///
    /// Media rows are selected in one ranked `DISTINCT ON` query. A
    /// non-preview production current deployment wins, followed by another
    /// current deployment, then the newest historical deployment with a
    /// screenshot. Historical fallbacks omit their URL because they are not
    /// guaranteed to be routable. The newest attempt status is selected
    /// independently because a failed attempt does not replace the older
    /// deployment that remains live.
    pub async fn get_latest_deployment_media(
        &self,
        project_ids: &[i32],
    ) -> Result<Vec<LatestDeploymentMedia>, DeploymentError> {
        const MAX_PROJECT_IDS: usize = 100;

        if project_ids.is_empty() {
            return Ok(Vec::new());
        }
        if project_ids.len() > MAX_PROJECT_IDS {
            return Err(DeploymentError::InvalidInput(format!(
                "latest deployment media accepts at most {MAX_PROJECT_IDS} project IDs; received {}",
                project_ids.len()
            )));
        }

        let placeholders = (1..=project_ids.len())
            .map(|index| format!("${index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let statement = sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            format!(
                "WITH latest_attempts AS ( \
                    SELECT DISTINCT ON (d.project_id) \
                        d.project_id, d.state AS latest_attempt_status \
                    FROM deployments d \
                    WHERE d.project_id IN ({placeholders}) \
                    ORDER BY d.project_id, d.created_at DESC, d.id DESC \
                 ), candidates AS ( \
                    SELECT d.project_id, d.slug, d.screenshot_location, TRUE AS is_current, \
                        CASE WHEN e.slug = 'production' AND NOT e.is_preview THEN 0 ELSE 1 END AS priority, \
                        e.updated_at AS candidate_updated_at, d.created_at, d.id \
                    FROM environments e \
                    JOIN deployments d \
                      ON d.id = e.current_deployment_id \
                     AND d.project_id = e.project_id \
                    WHERE e.project_id IN ({placeholders}) \
                      AND e.deleted_at IS NULL \
                    UNION ALL \
                    SELECT d.project_id, d.slug, d.screenshot_location, FALSE AS is_current, \
                        2 AS priority, d.created_at AS candidate_updated_at, d.created_at, d.id \
                    FROM deployments d \
                    WHERE d.project_id IN ({placeholders}) \
                      AND d.screenshot_location IS NOT NULL \
                 ), selected_media AS ( \
                    SELECT DISTINCT ON (project_id) \
                        project_id, slug, screenshot_location, is_current \
                    FROM candidates \
                    ORDER BY project_id, priority, candidate_updated_at DESC, created_at DESC, id DESC \
                 ) \
                 SELECT latest_attempts.project_id, latest_attempts.latest_attempt_status, \
                    selected_media.slug, selected_media.screenshot_location, \
                    COALESCE(selected_media.is_current, FALSE) AS is_current \
                 FROM latest_attempts \
                 LEFT JOIN selected_media USING (project_id)"
            ),
            project_ids.iter().copied().map(Into::into),
        );
        let rows = self.db.query_all(statement).await.map_err(|error| {
            DeploymentError::DatabaseError {
                reason: format!(
                    "Failed to query latest deployment media for project IDs {project_ids:?}: {error}"
                ),
            }
        })?;

        let settings = self.config_service.get_settings().await.map_err(|error| {
            DeploymentError::DeploymentError(format!(
                "Failed to load application settings for latest deployment media for project IDs {project_ids:?}: {error}"
            ))
        })?;
        let proxy_port = self.config_service.proxy_port();
        let mut media_by_project = HashMap::with_capacity(rows.len());
        for row in rows {
            let project_id = row.try_get("", "project_id").map_err(|error| {
                DeploymentError::DatabaseError {
                    reason: format!(
                        "Failed to decode project_id in latest deployment media query for project IDs {project_ids:?}: {error}"
                    ),
                }
            })?;
            let slug: Option<String> =
                row.try_get("", "slug")
                    .map_err(|error| DeploymentError::DatabaseError {
                        reason: format!(
                            "Failed to decode deployment slug for project {project_id}: {error}"
                        ),
                    })?;
            let latest_attempt_status: String =
                row.try_get("", "latest_attempt_status")
                    .map_err(|error| DeploymentError::DatabaseError {
                        reason: format!(
                            "Failed to decode latest deployment attempt status for project {project_id}: {error}"
                        ),
                    })?;
            let screenshot_location = row.try_get("", "screenshot_location").map_err(|error| {
                DeploymentError::DatabaseError {
                    reason: format!(
                        "Failed to decode screenshot location for project {project_id}: {error}"
                    ),
                }
            })?;
            let is_current: bool = row.try_get("", "is_current").map_err(|error| {
                DeploymentError::DatabaseError {
                    reason: format!(
                        "Failed to decode current-deployment state for project {project_id}: {error}"
                    ),
                }
            })?;
            let url = if is_current {
                let slug = slug.ok_or_else(|| DeploymentError::DatabaseError {
                    reason: format!(
                        "Current deployment media for project {project_id} is missing its deployment slug"
                    ),
                })?;
                Some(deployment_url_from_settings(&settings, proxy_port, &slug))
            } else {
                None
            };
            media_by_project.insert(
                project_id,
                LatestDeploymentMedia {
                    project_id,
                    latest_attempt_status,
                    url,
                    screenshot_location,
                },
            );
        }

        Ok(project_ids
            .iter()
            .filter_map(|project_id| media_by_project.remove(project_id))
            .collect())
    }

    pub async fn get_project_deployments(
        &self,
        project_id: i32,
        page: Option<i64>,
        per_page: Option<i64>,
        environment_id: Option<i32>,
    ) -> Result<DeploymentListResponse, DeploymentError> {
        // Clamp before the `as u64` cast: an out-of-range or negative i64 here
        // wraps to a huge u64 on cast, and Sea-ORM's OFFSET (page_size * page)
        // then overflows the i64 bind sea-query-binder sends to Postgres,
        // panicking with `TryFromIntError(PosOverflow)` and taking down the
        // whole HTTP listener task -- not just this request. Every caller
        // (REST and MCP) reaches this cast, so it must be enforced here, not
        // only at a caller's argument-parsing boundary.
        let page = page.unwrap_or(1).clamp(1, i64::from(i32::MAX)) as u64;
        let per_page = per_page.unwrap_or(10).clamp(1, 100) as u64;

        // Build base query with project_id filter
        let mut query =
            deployments::Entity::find().filter(deployments::Column::ProjectId.eq(project_id));

        let mut total_query =
            deployments::Entity::find().filter(deployments::Column::ProjectId.eq(project_id));

        // Add environment_id filter if provided
        if let Some(env_id) = environment_id {
            query = query.filter(deployments::Column::EnvironmentId.eq(env_id));
            total_query = total_query.filter(deployments::Column::EnvironmentId.eq(env_id));
        }

        let total = total_query
            .count(self.db.as_ref())
            .await
            .map_err(|e| DeploymentError::Other(e.to_string()))?;

        let results = query
            .order_by_desc(deployments::Column::CreatedAt)
            .paginate(self.db.as_ref(), per_page)
            .fetch_page(page - 1)
            .await
            .map_err(|e| DeploymentError::Other(e.to_string()))?;

        if results.is_empty() && page == 1 {
            return Ok(DeploymentListResponse {
                deployments: Vec::new(),
                total: 0,
                page: page as i64,
                per_page: per_page as i64,
            });
        }

        // Collect all unique environment IDs
        let env_ids: Vec<i32> = results
            .iter()
            .map(|d| d.environment_id)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        // Fetch all environments with their domains in a single query
        let environments_with_domains = self.get_environments_with_domains(&env_ids, false).await?;

        // For each deployment, check if it's the current deployment for any environment
        let mut deployments_with_info = Vec::new();
        for deployment in results {
            let is_current = environments::Entity::find()
                .filter(environments::Column::ProjectId.eq(project_id))
                .filter(environments::Column::CurrentDeploymentId.eq(deployment.id))
                .one(self.db.as_ref())
                .await
                .map_err(|e| DeploymentError::Other(e.to_string()))?
                .is_some();

            let environment = environments_with_domains
                .get(&deployment.environment_id)
                .cloned();

            deployments_with_info.push(
                self.map_db_deployment_to_deployment(deployment, is_current, environment)
                    .await,
            );
        }

        Ok(DeploymentListResponse {
            deployments: deployments_with_info,
            total: total as i64,
            page: page as i64,
            per_page: per_page as i64,
        })
    }

    /// Find the deployment produced by a specific local-image-upload attempt,
    /// identified by the client-generated `upload_request_id` it was
    /// submitted with. Used both to short-circuit a retried upload before
    /// re-importing the image, and to let a client that lost the original
    /// HTTP response (e.g. after its own wait timed out) resolve the
    /// deployment by exact ID instead of guessing from timing.
    pub async fn find_deployment_by_upload_request_id(
        &self,
        project_id: i32,
        environment_id: i32,
        upload_request_id: &str,
    ) -> Result<Option<deployments::Model>, DeploymentError> {
        let deployment = deployments::Entity::find()
            .filter(deployments::Column::ProjectId.eq(project_id))
            .filter(deployments::Column::EnvironmentId.eq(environment_id))
            .filter(deployments::Column::UploadRequestId.eq(upload_request_id))
            .one(self.db.as_ref())
            .await?;
        Ok(deployment)
    }

    /// Most recent deployment of a project, or `None` when the project exists
    /// but has never been deployed (a normal state, not an error).
    ///
    /// A project that does not exist (or was soft-deleted) is a
    /// [`DeploymentError::NotFound`] naming the project, so callers can tell
    /// "nothing deployed yet" apart from "wrong project id".
    pub async fn get_last_deployment(
        &self,
        project_id: i32,
    ) -> Result<Option<Deployment>, DeploymentError> {
        // Check the project first: a soft-deleted project keeps its deployment
        // rows, and must read as "not found" rather than returning them.
        let project_exists = projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .one(self.db.as_ref())
            .await?
            .is_some();
        if !project_exists {
            return Err(DeploymentError::NotFound(format!(
                "project {} not found",
                project_id
            )));
        }

        let Some(deployment) = deployments::Entity::find()
            .filter(deployments::Column::ProjectId.eq(project_id))
            .order_by_desc(deployments::Column::CreatedAt)
            .one(self.db.as_ref())
            .await?
        else {
            return Ok(None);
        };

        // Check if this deployment is current for any environment
        let is_current = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::CurrentDeploymentId.eq(deployment.id))
            .one(self.db.as_ref())
            .await?
            .is_some();

        // Fetch environment with domains
        let environments_with_domains = self
            .get_environments_with_domains(&[deployment.environment_id], true)
            .await?;
        let environment = environments_with_domains
            .get(&deployment.environment_id)
            .cloned();

        Ok(Some(
            self.map_db_deployment_to_deployment(deployment, is_current, environment)
                .await,
        ))
    }

    pub async fn get_deployment(
        &self,
        project_id: i32,
        deployment_id: i32,
    ) -> Result<Deployment, DeploymentError> {
        // Get the deployment with its pipeline
        let deployment_with_pipeline = deployments::Entity::find()
            .filter(deployments::Column::ProjectId.eq(project_id))
            .filter(deployments::Column::Id.eq(deployment_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!(
                    "deployment {} for project {} not found",
                    deployment_id, project_id
                ))
            })?;

        let deployment = deployment_with_pipeline;

        // Check if this deployment is current for any environment
        let is_current = environments::Entity::find()
            .filter(environments::Column::ProjectId.eq(project_id))
            .filter(environments::Column::CurrentDeploymentId.eq(deployment_id))
            .one(self.db.as_ref())
            .await?
            .is_some();

        // Fetch environment with domains
        let environments_with_domains = self
            .get_environments_with_domains(&[deployment.environment_id], true)
            .await?;
        let environment = environments_with_domains
            .get(&deployment.environment_id)
            .cloned();

        Ok(self
            .map_db_deployment_to_deployment(deployment, is_current, environment)
            .await)
    }

    /// List the captured (historical) container-log dumps for a deployment.
    ///
    /// These are written just before a superseded deployment's containers are
    /// torn down (see `MarkDeploymentCompleteJob::capture_container_logs`), so
    /// they let a user read the logs of a container that no longer exists
    /// (e.g. "web-2" from a few days ago).
    ///
    /// Scoped to `project_id`: the deployment must belong to the caller's
    /// project or this returns `NotFound`, preventing cross-tenant access.
    pub async fn list_deployment_container_logs(
        &self,
        project_id: i32,
        deployment_id: i32,
    ) -> Result<Vec<deployment_container_logs::Model>, DeploymentError> {
        // Authorize: confirm the deployment is in this project before exposing
        // anything tied to it.
        deployments::Entity::find()
            .filter(deployments::Column::ProjectId.eq(project_id))
            .filter(deployments::Column::Id.eq(deployment_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!(
                    "deployment {} for project {} not found",
                    deployment_id, project_id
                ))
            })?;

        let logs = deployment_container_logs::Entity::find()
            .filter(deployment_container_logs::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_container_logs::Column::ProjectId.eq(project_id))
            .order_by_desc(deployment_container_logs::Column::CapturedAt)
            .all(self.db.as_ref())
            .await?;

        Ok(logs)
    }

    /// Read the captured text content for a single historical container-log
    /// dump, returning the metadata row alongside the log body.
    ///
    /// Scoped to `project_id` via the `log_id` row's own `project_id` column —
    /// a caller can only read dumps that belong to their project.
    pub async fn get_deployment_container_log_content(
        &self,
        project_id: i32,
        deployment_id: i32,
        log_id: i32,
    ) -> Result<(deployment_container_logs::Model, String), DeploymentError> {
        let row = deployment_container_logs::Entity::find()
            .filter(deployment_container_logs::Column::Id.eq(log_id))
            .filter(deployment_container_logs::Column::DeploymentId.eq(deployment_id))
            .filter(deployment_container_logs::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!(
                    "captured log {} for deployment {} in project {} not found",
                    log_id, deployment_id, project_id
                ))
            })?;

        // `log_path` is a server-generated relative path (never user input), so
        // `get_log_content` resolves it safely under the data dir.
        let content = self
            .log_service
            .get_log_content(&row.log_path)
            .await
            .map_err(|e| {
                DeploymentError::Other(format!(
                    "Failed to read captured log file for log {} (deployment {}): {}",
                    log_id, deployment_id, e
                ))
            })?;

        Ok((row, content))
    }

    pub(super) async fn map_db_deployment_to_deployment(
        &self,
        db_deployment: deployments::Model,
        is_current: bool,
        environment: Option<DeploymentEnvironment>,
    ) -> Deployment {
        // Use provided environment or create a basic one
        let environment = environment.unwrap_or_else(|| DeploymentEnvironment {
            id: db_deployment.environment_id,
            name: "Environment".to_string(),
            slug: "environment".to_string(),
            primary_url: None,
            no_address_reason: None,
            reachability_warning: None,
            domains: vec![],
        });

        // Extract commit information from deployment metadata or fields
        let commit_sha = db_deployment.commit_sha.clone();
        let commit_message = db_deployment.commit_message.clone();
        let branch_ref = db_deployment.branch_ref.clone();
        let tag_ref = db_deployment.tag_ref.clone();

        let repo_commit: Option<octocrab::models::repos::RepoCommit> =
            match &db_deployment.commit_json {
                Some(commit) => serde_json::from_value(commit.clone()).ok(),
                None => None,
            };
        let commit_author = repo_commit
            .clone()
            .and_then(|rc| rc.author.map(|a| a.login))
            .map(|login| login.to_string());
        let commit_date = repo_commit
            .clone()
            .and_then(|rc| rc.commit.committer.and_then(|c| c.date));

        // Compute the actual URL from the stored slug
        let deployment_url = self
            .compute_deployment_url(&db_deployment.slug)
            .await
            .unwrap_or_else(|_| format!("http://{}", db_deployment.slug));

        Deployment {
            id: db_deployment.id,
            project_id: db_deployment.project_id,
            environment_id: db_deployment.environment_id,
            environment,
            status: db_deployment.state,
            url: deployment_url,
            commit_hash: commit_sha,
            commit_message,
            branch: branch_ref,
            tag: tag_ref,
            created_at: db_deployment.created_at,
            started_at: db_deployment.started_at,
            finished_at: db_deployment.finished_at,
            screenshot_location: db_deployment.screenshot_location,
            commit_author,
            commit_date,
            is_current,
            cancelled_reason: db_deployment.cancelled_reason.clone(),
            deployment_config: db_deployment.deployment_config,
            metadata: db_deployment.metadata,
        }
    }

    /// Get all jobs for a deployment owned by the requested project.
    ///
    /// The project constraint is part of this service method rather than only
    /// an HTTP guard because deployment IDs are globally enumerable and this
    /// result includes sensitive workflow metadata.
    pub async fn get_deployment_jobs(
        &self,
        project_id: i32,
        deployment_id: i32,
    ) -> Result<Vec<temps_entities::deployment_jobs::Model>, DeploymentError> {
        use temps_entities::deployment_jobs;

        let deployment_exists = deployments::Entity::find_by_id(deployment_id)
            .filter(deployments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await
            .map_err(|e| DeploymentError::DatabaseError {
                reason: e.to_string(),
            })?
            .is_some();

        if !deployment_exists {
            return Err(DeploymentError::NotFound(format!(
                "deployment {} for project {} not found",
                deployment_id, project_id
            )));
        }

        let jobs = deployment_jobs::Entity::find()
            .filter(deployment_jobs::Column::DeploymentId.eq(deployment_id))
            .order_by_asc(deployment_jobs::Column::ExecutionOrder)
            .all(self.db.as_ref())
            .await
            .map_err(|e| DeploymentError::DatabaseError {
                reason: e.to_string(),
            })?;

        Ok(jobs)
    }

    /// The project's git host reference (owner/repo + branch) for a
    /// deployment, but ONLY when the repo is public -- returns `None` for a
    /// private repo or a project with no git connection at all (e.g. a
    /// manual/CLI upload). Callers that surface this externally (e.g. in a
    /// GitHub issue template on a public repo) must never see a private
    /// repo's URL, so the `is_public_repo` check happens here rather than
    /// being left to each caller to remember.
    pub async fn get_public_repo_reference(
        &self,
        project_id: i32,
        deployment_id: i32,
    ) -> Result<Option<RepoReference>, DeploymentError> {
        use temps_entities::projects;

        let deployment = deployments::Entity::find_by_id(deployment_id)
            .filter(deployments::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await
            .map_err(|e| DeploymentError::DatabaseError {
                reason: e.to_string(),
            })?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!(
                    "deployment {} for project {} not found",
                    deployment_id, project_id
                ))
            })?;

        let project = projects::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
            .map_err(|e| DeploymentError::DatabaseError {
                reason: e.to_string(),
            })?
            .ok_or_else(|| DeploymentError::NotFound(format!("project {project_id} not found")))?;

        if !project.is_public_repo {
            return Ok(None);
        }

        Ok(Some(RepoReference {
            owner: project.repo_owner,
            repo: project.repo_name,
            branch: deployment.branch_ref.unwrap_or(project.main_branch),
        }))
    }

    /// The routing/identity slug of a project.
    ///
    /// Exists because the ADR-045 guards answer from the slug, not the id —
    /// `TEMPS_DOCKER_SOCKET_PROJECTS` names slugs — and handlers must not
    /// query `projects` themselves. One indexed primary-key lookup, on
    /// human-initiated paths only (exec, terminal); it is deliberately not
    /// used anywhere per-request.
    pub async fn project_slug(&self, project_id: i32) -> Result<String, DeploymentError> {
        projects::Entity::find_by_id(project_id)
            .select_only()
            .column(projects::Column::Slug)
            .into_tuple::<String>()
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| DeploymentError::NotFound(format!("Project {project_id} not found")))
    }

    /// Whether this deployment ever had the host Docker socket mounted into
    /// one of its containers (ADR 045).
    ///
    /// Used by exec/terminal authorization alongside (never instead of) the
    /// project's *current* slug: renaming a project away from a granted slug
    /// is admin-only, but it does not stop or recreate that project's
    /// already-running containers, so checking only the current slug would
    /// silently downgrade exec authorization on a still-root-equivalent
    /// container the moment an admin renames the project for an unrelated
    /// reason.
    pub async fn deployment_docker_socket_mounted(
        &self,
        deployment_id: i32,
    ) -> Result<bool, DeploymentError> {
        deployments::Entity::find_by_id(deployment_id)
            .select_only()
            .column(deployments::Column::DockerSocketMounted)
            .into_tuple::<bool>()
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| {
                DeploymentError::NotFound(format!("Deployment {deployment_id} not found"))
            })
    }

    /// Get deployment activity graph for the last N days
    /// Returns daily counts of unique commits deployed, with intensity levels for GitHub-style contribution graph
    /// Note: Only counts deployments that have a commit SHA, and counts each unique commit once per day
    pub async fn get_activity_graph(
        &self,
        project_id: Option<i32>,
        environment_id: Option<i32>,
        days: i32,
    ) -> Result<crate::handlers::types::ActivityGraphResponse, DeploymentError> {
        use chrono::{Duration, NaiveDate, NaiveTime, Utc};
        use std::collections::HashMap;

        let end_date = Utc::now().date_naive();
        let start_date = end_date - Duration::days(days as i64 - 1);

        // Convert NaiveDate to DateTime for comparison: start of the first
        // day through 23:59:59 of the last.
        let start_datetime = start_date.and_time(NaiveTime::MIN).and_utc();
        let end_datetime = end_date.and_time(NaiveTime::MIN).and_utc()
            + Duration::hours(23)
            + Duration::minutes(59)
            + Duration::seconds(59);

        // Build query using Sea-ORM
        let mut query = deployments::Entity::find()
            .filter(deployments::Column::CreatedAt.gte(start_datetime))
            .filter(deployments::Column::CreatedAt.lte(end_datetime));

        if let Some(pid) = project_id {
            query = query.filter(deployments::Column::ProjectId.eq(pid));
        }

        if let Some(eid) = environment_id {
            query = query.filter(deployments::Column::EnvironmentId.eq(eid));
        }

        // Fetch all deployments in the date range
        let deployments_list = query.all(self.db.as_ref()).await?;

        // Group deployments by date, counting unique commit SHAs per day
        // We use a HashMap of HashSet to track unique commits per date
        let mut commits_by_date: HashMap<NaiveDate, std::collections::HashSet<String>> =
            HashMap::new();
        for deployment in deployments_list {
            let date = deployment.created_at.date_naive();

            // Only count deployments with a commit SHA
            if let Some(commit_sha) = deployment.commit_sha {
                commits_by_date.entry(date).or_default().insert(commit_sha);
            }
        }

        // Convert unique commits to counts
        let mut activity_map: HashMap<NaiveDate, i64> = HashMap::new();
        for (date, commits) in commits_by_date {
            activity_map.insert(date, commits.len() as i64);
        }

        // Generate all days in the range (including days with zero activity)
        let mut days_vec = Vec::new();
        let mut total_count = 0i64;
        let mut current = start_date;

        while current <= end_date {
            let count = activity_map.get(&current).copied().unwrap_or(0);
            total_count += count;

            // Calculate intensity level for visualization
            // 0: No activity, 1: Low (1-2), 2: Medium (3-5), 3: High (6-10), 4: Very High (11+)
            let level = match count {
                0 => 0,
                1..=2 => 1,
                3..=5 => 2,
                6..=10 => 3,
                _ => 4,
            };

            days_vec.push(crate::handlers::types::ActivityDay {
                date: current.to_string(),
                count,
                level,
            });

            current = current.succ_opt().unwrap_or(current);
        }

        Ok(crate::handlers::types::ActivityGraphResponse {
            days: days_vec,
            total_count,
            start_date: start_date.to_string(),
            end_date: end_date.to_string(),
        })
    }
}
