// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Project reads: lookups, search, pagination, statistics and row mapping.

use super::*;

async fn has_ready_deployment(
    db: &temps_database::DbConnection,
    hidden: &[i32],
) -> Result<bool, ProjectError> {
    // last_deployment is stamped when a Git deployment starts, so it is
    // not proof of activation. New completions preserve ready_at; older
    // deployments can still be identified by their successful lifecycle state.
    // Fetch one id, rather than counting or loading deployment history.
    let completed = deployments::Entity::find().filter(
        Condition::any()
            .add(deployments::Column::ReadyAt.is_not_null())
            .add(deployments::Column::State.is_in([
                "completed",
                "deployed",
                "superseded",
                "paused",
            ])),
    );
    let completed = if hidden.is_empty() {
        completed
    } else {
        completed.filter(deployments::Column::ProjectId.is_not_in(hidden.iter().copied()))
    };
    let completed = completed
        .select_only()
        .column(deployments::Column::Id)
        .into_tuple::<i32>()
        .one(db)
        .await
        .map_err(|e| {
            ProjectError::DatabaseConnectionError(format!(
                "Failed to check for a completed deployment in project statistics: {e}"
            ))
        })?
        .is_some();
    Ok(completed)
}

/// A project row plus the provider type of the Git connection it is linked to.
///
/// The provider type lives two hops away (`projects` →
/// `git_provider_connections` → `git_providers`), so the read queries LEFT JOIN
/// it in as one extra column rather than paying a second round trip. `LEFT` and
/// `Option` because most projects have no connection at all — a Docker-image or
/// uploaded-source project must still come back from the same query.
#[derive(Debug)]
struct ProjectWithGitProviderType {
    project: projects::Model,
    git_provider_type: Option<String>,
}

impl sea_orm::FromQueryResult for ProjectWithGitProviderType {
    fn from_query_result(result: &sea_orm::QueryResult, pre: &str) -> Result<Self, sea_orm::DbErr> {
        Ok(Self {
            project: projects::Model::from_query_result(result, pre)?,
            git_provider_type: result.try_get(pre, GIT_PROVIDER_TYPE_ALIAS)?,
        })
    }
}

/// Alias for the joined-in `git_providers.provider_type` column. Named rather
/// than bare so it can never collide with a `projects` column.
const GIT_PROVIDER_TYPE_ALIAS: &str = "git_provider_type";

fn escape_like_literal(value: &str) -> String {
    value.chars().fold(String::new(), |mut escaped, character| {
        if matches!(character, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(character);
        escaped
    })
}

impl ProjectService {
    pub async fn get_projects(&self) -> Result<Vec<Project>, ProjectError> {
        let results = Self::with_git_provider_type(projects::Entity::find())
            // Most-recently-deployed first; never-deployed projects (NULL
            // last_deployment) sort last, not first — a NULL under DESC would
            // otherwise be treated as "deployed infinitely recently".
            .order_by_with_nulls(
                projects::Column::LastDeployment,
                sea_orm::Order::Desc,
                sea_orm::sea_query::NullOrdering::Last,
            )
            .order_by_desc(projects::Column::CreatedAt)
            .into_model::<ProjectWithGitProviderType>()
            .all(self.db.as_ref())
            .await
            .map_err(|e| ProjectError::Other(e.to_string()))?;

        Ok(results
            .into_iter()
            .map(Self::map_db_project_row_to_project)
            .collect())
    }

    pub async fn get_project(&self, project_id: i32) -> Result<Project, ProjectError> {
        let project_found_db =
            Self::with_git_provider_type(projects::Entity::find_by_id(project_id))
                .into_model::<ProjectWithGitProviderType>()
                .one(self.db.as_ref())
                .await
                .map_err(|e| ProjectError::Other(e.to_string()))?;

        project_found_db
            .map(Self::map_db_project_row_to_project)
            .ok_or(ProjectError::NotFound(format!(
                "project {} not found",
                project_id
            )))
    }

    pub async fn get_project_by_slug(&self, slug: &str) -> Result<Project, ProjectError> {
        let project_found_db = Self::with_git_provider_type(projects::Entity::find())
            .filter(projects::Column::Slug.eq(slug))
            .into_model::<ProjectWithGitProviderType>()
            .one(self.db.as_ref())
            .await?
            .ok_or(ProjectError::NotFound(format!(
                "project {} not found",
                slug
            )))?;
        let project_found = Self::map_db_project_row_to_project(project_found_db);

        Ok(project_found)
    }

    pub async fn get_projects_by_repo_owner_and_name(
        &self,
        repo_owner: &str,
        repo_name: &str,
    ) -> Result<Vec<Project>, ProjectError> {
        let projects_found_db = Self::with_git_provider_type(projects::Entity::find())
            .filter(projects::Column::RepoOwner.eq(repo_owner))
            .filter(projects::Column::RepoName.eq(repo_name))
            .into_model::<ProjectWithGitProviderType>()
            .all(self.db.as_ref())
            .await
            .map_err(|e| ProjectError::Other(e.to_string()))?;

        let projects_found: Vec<Project> = projects_found_db
            .into_iter()
            .map(Self::map_db_project_row_to_project)
            .collect();
        Ok(projects_found)
    }

    pub async fn find_project_by_repo(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<Project, ProjectError> {
        let project_found = Self::with_git_provider_type(projects::Entity::find())
            .filter(projects::Column::RepoOwner.eq(owner))
            .filter(projects::Column::RepoName.eq(repo))
            .into_model::<ProjectWithGitProviderType>()
            .one(self.db.as_ref())
            .await
            .map_err(|e| ProjectError::Other(format!("Database error: {}", e)))?;

        match project_found {
            Some(project) => Ok(Self::map_db_project_row_to_project(project)),
            None => Err(ProjectError::NotFound(format!(
                "Project not found for repository {}/{}",
                owner, repo
            ))),
        }
    }

    pub async fn get_projects_paginated(
        &self,
        page: i64,
        per_page: i64,
    ) -> Result<(Vec<Project>, i64), ProjectError> {
        self.get_projects_paginated_excluding(page, per_page, &[])
            .await
    }

    /// [`Self::get_projects_paginated`], minus a caller-supplied set of
    /// project ids.
    ///
    /// `hidden` comes from
    /// [`ProjectAccessChecker::hidden_project_ids`](temps_core::ProjectAccessChecker::hidden_project_ids)
    /// and is empty on an instance with no access grants configured, which
    /// makes this identical to the unfiltered query in that case. The
    /// exclusion is applied to the **count** as well as the page, so
    /// pagination doesn't advertise rows the caller can never see.
    pub async fn get_projects_paginated_excluding(
        &self,
        page: i64,
        per_page: i64,
        hidden: &[i32],
    ) -> Result<(Vec<Project>, i64), ProjectError> {
        self.get_projects_paginated_excluding_search(page, per_page, hidden, None)
            .await
    }

    pub async fn get_projects_paginated_excluding_search(
        &self,
        page: i64,
        per_page: i64,
        hidden: &[i32],
        search: Option<&str>,
    ) -> Result<(Vec<Project>, i64), ProjectError> {
        self.get_projects_paginated_filtered(page, per_page, hidden, search, None)
            .await
    }

    /// Paginated project list with an optional group-membership filter.
    ///
    /// `group_filter`:
    /// - `None` — all projects (existing behaviour)
    /// - `Some(GroupFilter::Ungrouped)` — services not in any project group
    /// - `Some(GroupFilter::InGroup(id))` — services that are members of
    ///   the given project group
    pub async fn get_projects_paginated_filtered(
        &self,
        page: i64,
        per_page: i64,
        hidden: &[i32],
        search: Option<&str>,
        group_filter: Option<GroupFilter>,
    ) -> Result<(Vec<Project>, i64), ProjectError> {
        use sea_orm::sea_query::extension::postgres::PgExpr;
        use sea_orm::sea_query::LikeExpr;
        use sea_orm::PaginatorTrait;
        use sea_orm::QueryOrder;

        if page < 1 {
            return Err(ProjectError::InvalidInput(
                "page must be greater than or equal to 1".to_string(),
            ));
        }
        if !(1..=100).contains(&per_page) {
            return Err(ProjectError::InvalidInput(
                "per_page must be between 1 and 100".to_string(),
            ));
        }

        let offset = page
            .checked_sub(1)
            .and_then(|zero_based_page| zero_based_page.checked_mul(per_page))
            .and_then(|offset| u64::try_from(offset).ok())
            .ok_or_else(|| {
                ProjectError::InvalidInput("page produces an invalid pagination offset".to_string())
            })?;

        // Build subquery for member project_ids once; reused for both count and
        // page queries so the filter is consistently applied to both.
        let group_member_subquery = || {
            sea_orm::sea_query::Query::select()
                .column(project_group_members::Column::ProjectId)
                .from(project_group_members::Entity)
                .to_owned()
        };
        let group_member_subquery_for_group = |group_id: i32| {
            sea_orm::sea_query::Query::select()
                .column(project_group_members::Column::ProjectId)
                .from(project_group_members::Entity)
                .and_where(
                    sea_orm::sea_query::Expr::col(project_group_members::Column::GroupId)
                        .eq(group_id),
                )
                .to_owned()
        };

        let filtered = || {
            let mut query = projects::Entity::find();
            if !hidden.is_empty() {
                query = query.filter(projects::Column::Id.is_not_in(hidden.iter().copied()));
            }
            if let Some(search) = search {
                let pattern = format!("%{}%", escape_like_literal(search));
                // Table-qualified: the page query joins `git_providers`, which
                // has a `name` column of its own, and an unqualified `name`
                // here is rejected by Postgres as ambiguous.
                query = query.filter(
                    Condition::any()
                        .add(
                            sea_orm::sea_query::Expr::col((
                                projects::Entity,
                                projects::Column::Name,
                            ))
                            .ilike(LikeExpr::new(pattern.clone())),
                        )
                        .add(
                            sea_orm::sea_query::Expr::col((
                                projects::Entity,
                                projects::Column::Slug,
                            ))
                            .ilike(LikeExpr::new(pattern.clone())),
                        ),
                );
            }
            // Apply group-membership filter using a subquery so we never join
            // into the page query (that already has a git_providers join).
            match group_filter {
                None => {}
                Some(GroupFilter::Ungrouped) => {
                    query =
                        query.filter(projects::Column::Id.not_in_subquery(group_member_subquery()));
                }
                Some(GroupFilter::InGroup(group_id)) => {
                    query = query.filter(
                        projects::Column::Id.in_subquery(group_member_subquery_for_group(group_id)),
                    );
                }
            }
            query
        };

        // Get total count
        let total = filtered()
            .count(self.db.as_ref())
            .await
            .map_err(|e| ProjectError::DatabaseConnectionError(e.to_string()))?
            as i64;

        // Get paginated projects. Never-deployed projects (NULL last_deployment)
        // sort last rather than first (a NULL under DESC would otherwise appear
        // as the most-recently-deployed project).
        // The count query above stays join-free: it counts projects, and the
        // provider join exists only to carry an extra column.
        let projects = Self::with_git_provider_type(filtered())
            .order_by_with_nulls(
                projects::Column::LastDeployment,
                sea_orm::Order::Desc,
                sea_orm::sea_query::NullOrdering::Last,
            )
            .order_by_desc(projects::Column::CreatedAt)
            .offset(offset)
            .limit(per_page as u64)
            .into_model::<ProjectWithGitProviderType>()
            .all(self.db.as_ref())
            .await
            .map_err(|e| ProjectError::DatabaseConnectionError(e.to_string()))?;

        let projects_found: Vec<Project> = projects
            .into_iter()
            .map(Self::map_db_project_row_to_project)
            .collect();

        Ok((projects_found, total))
    }

    pub async fn get_total_projects(&self) -> Result<i64, ProjectError> {
        use sea_orm::PaginatorTrait;
        // Get total count of projects
        let paginator = projects::Entity::find().paginate(self.db.as_ref(), 1);
        let total = paginator.num_items().await?;

        Ok(total as i64)
    }

    pub async fn get_project_statistics(&self) -> Result<ProjectStatistics, ProjectError> {
        self.get_project_statistics_excluding(&[]).await
    }

    /// [`Self::get_project_statistics`], minus a caller-supplied set of
    /// project ids.
    ///
    /// The count has to honour the same exclusion as the list, or the
    /// dashboard tells a scoped user how many projects exist on the
    /// instance while showing them only their own — a smaller leak than
    /// the names, but the same leak.
    pub async fn get_project_statistics_excluding(
        &self,
        hidden: &[i32],
    ) -> Result<ProjectStatistics, ProjectError> {
        use sea_orm::PaginatorTrait;

        let query = projects::Entity::find();
        let query = if hidden.is_empty() {
            query
        } else {
            query.filter(projects::Column::Id.is_not_in(hidden.iter().copied()))
        };

        let total_count = query
            .count(self.db.as_ref())
            .await
            .map_err(|e| ProjectError::DatabaseConnectionError(e.to_string()))?
            as i64;

        let has_completed_deployment = has_ready_deployment(self.db.as_ref(), hidden).await?;

        Ok(ProjectStatistics {
            total_count,
            has_completed_deployment,
        })
    }

    /// LEFT JOIN each project to the provider behind its Git connection and
    /// select that provider's type as one extra column.
    ///
    /// Neither hop can fan a project out into several rows (both are
    /// many-to-one), so this changes the row count of no query it is applied
    /// to, and it never needs a second round trip to fill the field in.
    fn with_git_provider_type(
        select: sea_orm::Select<projects::Entity>,
    ) -> sea_orm::Select<projects::Entity> {
        select
            .join(
                sea_orm::JoinType::LeftJoin,
                projects::Relation::GitProviderConnection.def(),
            )
            .join(
                sea_orm::JoinType::LeftJoin,
                git_provider_connections::Relation::Provider.def(),
            )
            .column_as(git_providers::Column::ProviderType, GIT_PROVIDER_TYPE_ALIAS)
    }

    fn map_db_project_row_to_project(row: ProjectWithGitProviderType) -> Project {
        let mut project = Self::map_db_project_to_project(row.project);
        project.git_provider_type = row.git_provider_type;
        project
    }

    /// Build the client-facing `Project` for a row a mutation just wrote.
    ///
    /// Writes return an updated `projects::Model` rather than going back
    /// through the joined read query, so the provider type is fetched here —
    /// one primary-key lookup, and none at all for a project with no
    /// connection. Answering a successful update with `git_provider_type:
    /// null` would tell a client that a connected project has no provider, and
    /// any client that trusts its own mutation response would downgrade a
    /// GitLab project back to a guess until its next read.
    ///
    /// Deliberately infallible. This runs *after* the write has committed, and
    /// it reads a display label — failing here would report a change the
    /// database has already accepted as a failed request, and would skip the
    /// `ProjectUpdated` job that several callers emit straight after this call,
    /// leaving routes and notifications unaware of a change that did happen. A
    /// lookup that fails is logged and leaves the field empty; the next read
    /// fills it in.
    pub(super) async fn map_written_project(&self, db_project: projects::Model) -> Project {
        let git_provider_type = match db_project.git_provider_connection_id {
            None => None,
            Some(connection_id) => {
                match git_provider_connections::Entity::find_by_id(connection_id)
                    .select_only()
                    .column(git_providers::Column::ProviderType)
                    .inner_join(git_providers::Entity)
                    .into_tuple::<String>()
                    .one(self.db.as_ref())
                    .await
                {
                    Ok(provider_type) => provider_type,
                    Err(e) => {
                        warn!(
                            "Failed to read the git provider type for connection {} of project {} \
                             after a successful write; responding without it: {}",
                            connection_id, db_project.id, e
                        );
                        None
                    }
                }
            }
        };

        let mut project = Self::map_db_project_to_project(db_project);
        project.git_provider_type = git_provider_type;
        project
    }

    /// Maps the project row alone, leaving `git_provider_type` empty.
    ///
    /// Private on purpose: every client-facing path must go through
    /// `map_db_project_row_to_project` (joined read) or `map_written_project`
    /// (mutation), so a new endpoint cannot quietly answer with "no provider"
    /// for a connected project.
    fn map_db_project_to_project(db_project: projects::Model) -> Project {
        // Extract deployment config fields
        let deployment_config = db_project.deployment_config.clone();

        // Convert preset to the runtime/UI slug (reconstructs nixpacks-{provider})
        let preset_str =
            temps_presets::runtime_slug(db_project.preset, db_project.preset_config.as_ref());

        // Handle repo_name and repo_owner - return None for empty strings (Git-less projects)
        let repo_name = if db_project.repo_name.is_empty() {
            None
        } else {
            Some(db_project.repo_name)
        };
        let repo_owner = if db_project.repo_owner.is_empty() {
            None
        } else {
            Some(db_project.repo_owner)
        };

        // Serialize preset_config to JSON value for the response
        let preset_config_json = db_project
            .preset_config
            .as_ref()
            .and_then(|config| serde_json::to_value(config).ok());

        let service_template = db_project.service_template.as_ref().and_then(|snapshot| {
            serde_json::from_value::<temps_core::templates::ServiceTemplateInstance>(
                snapshot.clone(),
            )
            .ok()
            .filter(|instance| instance.validate().is_ok())
        });
        let service_template_image_url = service_template
            .as_ref()
            .and_then(|instance| instance.template.image_url.clone());
        let service_template_version = service_template.map(|instance| instance.version);

        Project {
            id: db_project.id,
            slug: db_project.slug,
            name: db_project.name,
            repo_name,
            repo_owner,
            directory: db_project.directory,
            pull_only_root_directory: db_project.pull_only_root_directory,
            main_branch: db_project.main_branch,
            preset: (db_project.source_type != temps_entities::source_type::SourceType::External)
                .then_some(preset_str),
            template_slug: db_project.template_slug,
            service_template_image_url,
            service_template_version,
            preset_config: preset_config_json,
            created_at: db_project.created_at,
            updated_at: db_project.updated_at,
            automatic_deploy: deployment_config
                .clone()
                .and_then(|c| c.automatic_deploy)
                .unwrap_or(false),
            cpu_request: deployment_config.clone().and_then(|c| c.cpu_request),
            cpu_limit: deployment_config.clone().and_then(|c| c.cpu_limit),
            memory_request: deployment_config.clone().and_then(|c| c.memory_request),
            memory_limit: deployment_config.clone().and_then(|c| c.memory_limit),
            performance_metrics_enabled: deployment_config
                .clone()
                .map(|c| c.performance_metrics_enabled)
                .unwrap_or(false),
            last_deployment: db_project.last_deployment,
            project_type: db_project.project_type.to_string(),
            use_default_wildcard: true, // Deprecated field, always true
            custom_domain: None,        // Deprecated field, use project_domains table
            is_public_repo: db_project.is_public_repo,
            git_url: db_project.git_url,
            git_provider_connection_id: db_project.git_provider_connection_id,
            // Lives on the connection's provider row, not on the project row,
            // so it is filled in by the caller: `map_db_project_row_to_project`
            // from the joined read query, or `map_written_project` after a
            // mutation. Both are the only ways a `Project` reaches a client.
            git_provider_type: None,
            is_on_demand: false, // Deprecated field, default to false
            deployment_config: deployment_config.clone(),
            attack_mode: db_project.attack_mode,
            shield_mode: db_project.shield_mode,
            ai_alert_summaries_enabled: db_project.ai_alert_summaries_enabled,
            ai_api_traffic_summary_enabled: db_project.ai_api_traffic_summary_enabled,
            error_source_context_enabled: db_project.error_source_context_enabled,
            vulnerability_scanning_enabled: db_project.vulnerability_scanning_enabled,
            error_source_root: db_project.error_source_root,
            enable_preview_environments: db_project.enable_preview_environments,
            preview_envs_on_demand: db_project.preview_envs_on_demand,
            preview_envs_idle_timeout_seconds: db_project.preview_envs_idle_timeout_seconds,
            preview_envs_wake_timeout_seconds: db_project.preview_envs_wake_timeout_seconds,
            source_type: db_project.source_type,
            allow_alternate_sources: db_project.allow_alternate_sources,
            hosting: db_project.hosting,
            gitlab_webhook_id: db_project.gitlab_webhook_id,
            github_webhook_id: db_project.github_webhook_id,
            cross_project_trace_sharing: db_project.cross_project_trace_sharing,
            serves_own_temps_api: db_project.serves_own_temps_api,
            image_retention_hours: db_project.image_retention_hours,
            log_ingest_lines_per_sec: db_project.log_ingest_lines_per_sec,
            log_ingest_kib_per_sec: db_project.log_ingest_kib_per_sec,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn first_deploy_signal_requires_ready_and_excludes_hidden_projects() {
        use sea_orm::{DatabaseBackend, MockDatabase, Value};
        let row = std::collections::BTreeMap::from([("id", Value::Int(Some(42)))]);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row]])
            .into_connection();
        assert!(has_ready_deployment(&db, &[7, 9]).await.unwrap());
        let log = db.into_transaction_log();
        let statements = &log[0].statements()[0].sql;
        assert!(
            statements.contains("ready_at\" IS NOT NULL"),
            "{statements}"
        );
        assert!(statements.contains("project_id\" NOT IN"), "{statements}");
        assert!(statements.contains("state\" IN"), "{statements}");
        assert!(statements.contains("LIMIT"), "{statements}");
        assert!(!statements.contains("last_deployment"), "{statements}");
    }

    #[tokio::test]
    async fn first_deploy_signal_is_false_without_a_ready_deployment() {
        use sea_orm::{DatabaseBackend, MockDatabase, Value};
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<std::collections::BTreeMap<&str, Value>>::new()])
            .into_connection();
        assert!(!has_ready_deployment(&db, &[]).await.unwrap());
    }
}
