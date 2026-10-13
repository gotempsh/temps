// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Business logic for project groups. Authorization is the handler's job;
//! everything here assumes the caller has already been allowed.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::Utc;
use sea_orm::sea_query::{Condition, Expr, OnConflict, Query};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect, Set, SqlErr, TransactionTrait,
};
use serde::Deserialize;
use temps_entities::{project_group_members, project_group_routes, project_groups, projects};
use utoipa::ToSchema;

use crate::error::ProjectGroupError;

pub const DEFAULT_PAGE_SIZE: u64 = 20;
pub const MAX_PAGE_SIZE: u64 = 100;
const MAX_NAME_LEN: usize = 100;
const MAX_SLUG_LEN: usize = 100;
const MAX_DESCRIPTION_LEN: usize = 1000;

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateProjectGroupRequest {
    #[schema(example = "Acme Shop")]
    pub name: String,
    /// URL-safe identifier. Derived from `name` when omitted.
    #[schema(example = "acme-shop")]
    pub slug: Option<String>,
    pub description: Option<String>,
    /// Projects to place in the group right away, in display order. A
    /// project that is already in another group is moved.
    #[serde(default)]
    pub project_ids: Vec<i32>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct UpdateProjectGroupRequest {
    pub name: Option<String>,
    pub slug: Option<String>,
    /// An empty string clears the description.
    pub description: Option<String>,
}

/// A group together with the (non-deleted) projects in it, ordered for
/// display.
#[derive(Debug, Clone)]
pub struct ProjectGroupWithMembers {
    pub group: project_groups::Model,
    pub members: Vec<projects::Model>,
    /// Members that follow the group's repository and branch.
    pub inherits_repository: HashSet<i32>,
}

#[derive(Default)]
struct LoadedMembers {
    projects: Vec<projects::Model>,
    inherits_repository: HashSet<i32>,
}

pub struct ProjectGroupService {
    db: Arc<DatabaseConnection>,
}

impl ProjectGroupService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Resolves 1-indexed pagination with the platform defaults.
    pub fn page_params(page: Option<u64>, page_size: Option<u64>) -> (u64, u64) {
        let page = page.unwrap_or(1).max(1);
        let size = page_size
            .unwrap_or(DEFAULT_PAGE_SIZE)
            .clamp(1, MAX_PAGE_SIZE);
        (page, size)
    }

    /// Lowercases `name` and collapses every run of non-alphanumeric
    /// characters into a single `-`, trimming dashes from both ends.
    pub fn slugify(name: &str) -> String {
        let mut slug = String::with_capacity(name.len());
        let mut pending_dash = false;
        for c in name.chars() {
            if c.is_ascii_alphanumeric() {
                if pending_dash && !slug.is_empty() {
                    slug.push('-');
                }
                pending_dash = false;
                slug.push(c.to_ascii_lowercase());
            } else {
                pending_dash = true;
            }
        }
        slug.truncate(MAX_SLUG_LEN);
        slug.trim_end_matches('-').to_string()
    }

    pub(crate) fn validate_name(name: &str) -> Result<String, ProjectGroupError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(ProjectGroupError::Validation {
                message: "Project group name cannot be empty".into(),
            });
        }
        if name.chars().count() > MAX_NAME_LEN {
            return Err(ProjectGroupError::Validation {
                message: format!(
                    "Project group name '{name}' is longer than {MAX_NAME_LEN} characters"
                ),
            });
        }
        Ok(name.to_string())
    }

    pub(crate) fn validate_slug(slug: &str) -> Result<(), ProjectGroupError> {
        if slug.is_empty() {
            return Err(ProjectGroupError::Validation {
                message: "Project group slug cannot be empty; use letters or digits in the name \
                          or pass an explicit slug"
                    .into(),
            });
        }
        if slug.len() > MAX_SLUG_LEN {
            return Err(ProjectGroupError::Validation {
                message: format!(
                    "Project group slug '{slug}' is longer than {MAX_SLUG_LEN} characters"
                ),
            });
        }
        let valid_chars = slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !valid_chars || slug.starts_with('-') || slug.ends_with('-') {
            return Err(ProjectGroupError::Validation {
                message: format!(
                    "Project group slug '{slug}' must use lowercase letters, digits and \
                     dashes, and cannot start or end with a dash"
                ),
            });
        }
        Ok(())
    }

    fn normalize_description(
        description: Option<String>,
    ) -> Result<Option<String>, ProjectGroupError> {
        let Some(description) = description else {
            return Ok(None);
        };
        let description = description.trim().to_string();
        if description.chars().count() > MAX_DESCRIPTION_LEN {
            return Err(ProjectGroupError::Validation {
                message: format!(
                    "Project group description is longer than {MAX_DESCRIPTION_LEN} characters"
                ),
            });
        }
        Ok((!description.is_empty()).then_some(description))
    }

    fn map_write_error(
        slug: &str,
        operation: &str,
    ) -> impl FnOnce(sea_orm::DbErr) -> ProjectGroupError {
        let slug = slug.to_string();
        let operation = operation.to_string();
        move |err| match err.sql_err() {
            Some(SqlErr::UniqueConstraintViolation(_)) => ProjectGroupError::SlugConflict { slug },
            _ => ProjectGroupError::Database {
                operation,
                source: err,
            },
        }
    }

    async fn ensure_slug_free(
        &self,
        slug: &str,
        except_group_id: Option<i32>,
    ) -> Result<(), ProjectGroupError> {
        let existing = project_groups::Entity::find()
            .filter(project_groups::Column::Slug.eq(slug))
            .one(self.db.as_ref())
            .await
            .map_err(ProjectGroupError::db(format!(
                "checking whether project group slug '{slug}' is taken"
            )))?;
        match existing {
            Some(group) if Some(group.id) != except_group_id => {
                Err(ProjectGroupError::SlugConflict {
                    slug: slug.to_string(),
                })
            }
            _ => Ok(()),
        }
    }

    /// Loads the non-deleted member projects of every group in `group_ids`
    /// with a single join, ordered by position then name.
    async fn load_members(
        &self,
        group_ids: &[i32],
    ) -> Result<HashMap<i32, LoadedMembers>, ProjectGroupError> {
        let mut by_group: HashMap<i32, LoadedMembers> = HashMap::new();
        if group_ids.is_empty() {
            return Ok(by_group);
        }
        let rows = project_group_members::Entity::find()
            .filter(project_group_members::Column::GroupId.is_in(group_ids.iter().copied()))
            .find_also_related(projects::Entity)
            .filter(projects::Column::IsDeleted.eq(false))
            .order_by_asc(project_group_members::Column::Position)
            .order_by_asc(projects::Column::Name)
            .all(self.db.as_ref())
            .await
            .map_err(ProjectGroupError::db(format!(
                "loading members of project groups {group_ids:?}"
            )))?;
        for (membership, project) in rows {
            if let Some(project) = project {
                let loaded = by_group.entry(membership.group_id).or_default();
                if membership.inherits_repository {
                    loaded.inherits_repository.insert(project.id);
                }
                loaded.projects.push(project);
            }
        }
        Ok(by_group)
    }

    async fn with_members(
        &self,
        group: project_groups::Model,
    ) -> Result<ProjectGroupWithMembers, ProjectGroupError> {
        let loaded = self
            .load_members(&[group.id])
            .await?
            .remove(&group.id)
            .unwrap_or_default();
        Ok(ProjectGroupWithMembers {
            group,
            members: loaded.projects,
            inherits_repository: loaded.inherits_repository,
        })
    }

    async fn find_group(&self, group_id: i32) -> Result<project_groups::Model, ProjectGroupError> {
        project_groups::Entity::find_by_id(group_id)
            .one(self.db.as_ref())
            .await
            .map_err(ProjectGroupError::db(format!(
                "loading project group {group_id}"
            )))?
            .ok_or(ProjectGroupError::NotFound { group_id })
    }

    async fn find_live_project(
        &self,
        project_id: i32,
    ) -> Result<projects::Model, ProjectGroupError> {
        projects::Entity::find_by_id(project_id)
            .filter(projects::Column::IsDeleted.eq(false))
            .one(self.db.as_ref())
            .await
            .map_err(ProjectGroupError::db(format!(
                "loading project {project_id}"
            )))?
            .ok_or(ProjectGroupError::ProjectNotFound { project_id })
    }

    /// Whether a caller who cannot see `hidden` projects may see this group:
    /// an empty group reveals nothing, but a group whose every live member
    /// is hidden would leak its name, so it is treated as nonexistent — the
    /// same rule the project list applies to project names.
    pub fn is_visible(group: &ProjectGroupWithMembers, hidden: &HashSet<i32>) -> bool {
        group.members.is_empty() || group.members.iter().any(|p| !hidden.contains(&p.id))
    }

    /// Groups ordered by name: a stable, scannable order matters more for a
    /// navigation list than recency. Groups that [`Self::is_visible`] would
    /// reject for `hidden` are excluded in SQL so `total` and pagination
    /// only count what the caller can see.
    pub async fn list_groups(
        &self,
        page: Option<u64>,
        page_size: Option<u64>,
        hidden: &HashSet<i32>,
    ) -> Result<(Vec<ProjectGroupWithMembers>, u64), ProjectGroupError> {
        let (page, size) = Self::page_params(page, page_size);
        let mut query = project_groups::Entity::find();
        if !hidden.is_empty() {
            let live_members = Query::select()
                .expr(Expr::val(1))
                .from(project_group_members::Entity)
                .inner_join(
                    projects::Entity,
                    Expr::col((projects::Entity, projects::Column::Id)).equals((
                        project_group_members::Entity,
                        project_group_members::Column::ProjectId,
                    )),
                )
                .and_where(
                    Expr::col((
                        project_group_members::Entity,
                        project_group_members::Column::GroupId,
                    ))
                    .equals((project_groups::Entity, project_groups::Column::Id)),
                )
                .and_where(Expr::col((projects::Entity, projects::Column::IsDeleted)).eq(false))
                .to_owned();
            let visible_members = live_members
                .clone()
                .and_where(
                    Expr::col((
                        project_group_members::Entity,
                        project_group_members::Column::ProjectId,
                    ))
                    .is_not_in(hidden.iter().copied()),
                )
                .to_owned();
            query = query.filter(
                Condition::any()
                    .add(Expr::exists(live_members).not())
                    .add(Expr::exists(visible_members)),
            );
        }
        let paginator = query
            .order_by_asc(project_groups::Column::Name)
            .order_by_asc(project_groups::Column::Id)
            .paginate(self.db.as_ref(), size);
        let total = paginator
            .num_items()
            .await
            .map_err(ProjectGroupError::db("counting project groups"))?;
        let groups = paginator
            .fetch_page(page - 1)
            .await
            .map_err(ProjectGroupError::db(format!(
                "listing project groups page {page}"
            )))?;

        let ids: Vec<i32> = groups.iter().map(|g| g.id).collect();
        let mut members = self.load_members(&ids).await?;
        let items = groups
            .into_iter()
            .map(|group| {
                let loaded = members.remove(&group.id).unwrap_or_default();
                ProjectGroupWithMembers {
                    group,
                    members: loaded.projects,
                    inherits_repository: loaded.inherits_repository,
                }
            })
            .collect();
        Ok((items, total))
    }

    pub async fn get_group(
        &self,
        group_id: i32,
    ) -> Result<ProjectGroupWithMembers, ProjectGroupError> {
        let group = self.find_group(group_id).await?;
        self.with_members(group).await
    }

    pub async fn get_group_by_slug(
        &self,
        slug: &str,
    ) -> Result<ProjectGroupWithMembers, ProjectGroupError> {
        let group = project_groups::Entity::find()
            .filter(project_groups::Column::Slug.eq(slug))
            .one(self.db.as_ref())
            .await
            .map_err(ProjectGroupError::db(format!(
                "loading project group by slug '{slug}'"
            )))?
            .ok_or_else(|| ProjectGroupError::SlugNotFound {
                slug: slug.to_string(),
            })?;
        self.with_members(group).await
    }

    /// The group `project_id` belongs to, if any.
    pub async fn group_for_project(
        &self,
        project_id: i32,
    ) -> Result<Option<ProjectGroupWithMembers>, ProjectGroupError> {
        let membership = project_group_members::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
            .map_err(ProjectGroupError::db(format!(
                "loading group membership of project {project_id}"
            )))?;
        match membership {
            Some(m) => Ok(Some(self.get_group(m.group_id).await?)),
            None => Ok(None),
        }
    }

    /// Ids of the live projects in `group_id`, for authorization checks.
    pub async fn member_project_ids(&self, group_id: i32) -> Result<Vec<i32>, ProjectGroupError> {
        Ok(self
            .get_group(group_id)
            .await?
            .members
            .iter()
            .map(|p| p.id)
            .collect())
    }

    pub async fn create_group(
        &self,
        req: CreateProjectGroupRequest,
    ) -> Result<ProjectGroupWithMembers, ProjectGroupError> {
        let name = Self::validate_name(&req.name)?;
        let slug = match req.slug.as_deref().map(str::trim) {
            Some(slug) if !slug.is_empty() => slug.to_string(),
            _ => Self::slugify(&name),
        };
        Self::validate_slug(&slug)?;
        let description = Self::normalize_description(req.description)?;
        self.ensure_slug_free(&slug, None).await?;

        // Deduplicate while preserving order, then validate all IDs exist in
        // one query rather than N individual lookups.
        let mut project_ids: Vec<i32> = Vec::with_capacity(req.project_ids.len());
        for id in req.project_ids {
            if !project_ids.contains(&id) {
                project_ids.push(id);
            }
        }
        if !project_ids.is_empty() {
            let found: HashSet<i32> = projects::Entity::find()
                .filter(projects::Column::Id.is_in(project_ids.iter().copied()))
                .filter(projects::Column::IsDeleted.eq(false))
                .select_only()
                .column(projects::Column::Id)
                .into_tuple::<i32>()
                .all(self.db.as_ref())
                .await
                .map_err(ProjectGroupError::db("checking project IDs for new group"))?
                .into_iter()
                .collect();
            if let Some(&missing) = project_ids.iter().find(|id| !found.contains(id)) {
                return Err(ProjectGroupError::ProjectNotFound {
                    project_id: missing,
                });
            }
        }

        let txn = self.db.begin().await.map_err(ProjectGroupError::db(
            "starting project group create transaction",
        ))?;
        let now = Utc::now();
        let group = project_groups::ActiveModel {
            name: Set(name),
            slug: Set(slug.clone()),
            description: Set(description),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&txn)
        .await
        .map_err(Self::map_write_error(&slug, "inserting project group"))?;

        for (position, project_id) in project_ids.iter().enumerate() {
            Self::upsert_membership(&txn, group.id, *project_id, position as i32).await?;
        }
        txn.commit().await.map_err(ProjectGroupError::db(format!(
            "committing project group '{slug}'"
        )))?;

        self.with_members(group).await
    }

    pub async fn update_group(
        &self,
        group_id: i32,
        req: UpdateProjectGroupRequest,
    ) -> Result<ProjectGroupWithMembers, ProjectGroupError> {
        let group = self.find_group(group_id).await?;
        let mut active: project_groups::ActiveModel = group.clone().into();
        let mut slug_for_errors = group.slug.clone();

        if let Some(name) = req.name.as_deref() {
            active.name = Set(Self::validate_name(name)?);
        }
        if let Some(slug) = req.slug.as_deref().map(str::trim) {
            Self::validate_slug(slug)?;
            if slug != group.slug {
                self.ensure_slug_free(slug, Some(group_id)).await?;
            }
            slug_for_errors = slug.to_string();
            active.slug = Set(slug.to_string());
        }
        if req.description.is_some() {
            active.description = Set(Self::normalize_description(req.description)?);
        }
        active.updated_at = Set(Utc::now());

        let updated = active
            .update(self.db.as_ref())
            .await
            .map_err(Self::map_write_error(
                &slug_for_errors,
                &format!("updating project group {group_id}"),
            ))?;
        self.with_members(updated).await
    }

    /// Deletes the group and returns it with the ids of the projects that
    /// became ungrouped. The projects themselves are untouched.
    pub async fn delete_group(
        &self,
        group_id: i32,
    ) -> Result<(project_groups::Model, Vec<i32>), ProjectGroupError> {
        let ProjectGroupWithMembers { group, members, .. } = self.get_group(group_id).await?;
        project_groups::Entity::delete_by_id(group_id)
            .exec(self.db.as_ref())
            .await
            .map_err(ProjectGroupError::db(format!(
                "deleting project group {group_id}"
            )))?;
        Ok((group, members.into_iter().map(|p| p.id).collect()))
    }

    async fn upsert_membership<C: sea_orm::ConnectionTrait>(
        conn: &C,
        group_id: i32,
        project_id: i32,
        position: i32,
    ) -> Result<(), ProjectGroupError> {
        project_group_members::Entity::insert(project_group_members::ActiveModel {
            project_id: Set(project_id),
            group_id: Set(group_id),
            position: Set(position),
            // A project entering a group (or moving between groups) never
            // follows the new group's repository until asked to.
            inherits_repository: Set(false),
            added_at: Set(Utc::now()),
        })
        .on_conflict(
            OnConflict::column(project_group_members::Column::ProjectId)
                .update_columns([
                    project_group_members::Column::GroupId,
                    project_group_members::Column::Position,
                    project_group_members::Column::InheritsRepository,
                    project_group_members::Column::AddedAt,
                ])
                .to_owned(),
        )
        .exec_without_returning(conn)
        .await
        .map_err(ProjectGroupError::db(format!(
            "placing project {project_id} in project group {group_id}"
        )))?;
        Ok(())
    }

    /// Places `project_id` at the end of `group_id`, moving it out of any
    /// other group. Returns the group it was in before, if any. Adding a
    /// project to the group it is already in is a no-op.
    pub async fn add_project(
        &self,
        group_id: i32,
        project_id: i32,
    ) -> Result<Option<i32>, ProjectGroupError> {
        self.find_group(group_id).await?;
        self.find_live_project(project_id).await?;

        let previous = project_group_members::Entity::find_by_id(project_id)
            .one(self.db.as_ref())
            .await
            .map_err(ProjectGroupError::db(format!(
                "loading group membership of project {project_id}"
            )))?;
        if let Some(ref m) = previous {
            if m.group_id == group_id {
                return Ok(Some(group_id));
            }
        }

        // Compute and insert/update the position atomically in a single
        // statement to eliminate the max()+upsert race that can occur when two
        // concurrent callers both read the same max_position.
        use sea_orm::ConnectionTrait;
        let now = Utc::now();
        self.db
            .as_ref()
            .execute(sea_orm::Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                "INSERT INTO project_group_members \
                     (project_id, group_id, position, inherits_repository, added_at) \
                 VALUES ($1, $2, \
                     COALESCE((SELECT MAX(position) + 1 \
                                FROM project_group_members WHERE group_id = $2), 0), \
                     FALSE, $3) \
                 ON CONFLICT (project_id) DO UPDATE SET \
                     group_id = EXCLUDED.group_id, \
                     position = EXCLUDED.position, \
                     inherits_repository = FALSE, \
                     added_at = EXCLUDED.added_at",
                [project_id.into(), group_id.into(), now.into()],
            ))
            .await
            .map_err(ProjectGroupError::db(format!(
                "placing project {project_id} in project group {group_id}"
            )))?;
        Ok(previous.map(|m| m.group_id))
    }

    /// Returns all path routes in `group_id` that involve `project_id` as
    /// either the source or the target. Used before removing a member to
    /// collect the routes the DB trigger will cascade-delete, so the caller
    /// can audit and access-check them before the deletion happens.
    pub async fn routes_involving_project(
        &self,
        group_id: i32,
        project_id: i32,
    ) -> Result<Vec<project_group_routes::Model>, ProjectGroupError> {
        project_group_routes::Entity::find()
            .filter(project_group_routes::Column::GroupId.eq(group_id))
            .filter(
                Condition::any()
                    .add(project_group_routes::Column::SourceProjectId.eq(project_id))
                    .add(project_group_routes::Column::TargetProjectId.eq(project_id)),
            )
            .all(self.db.as_ref())
            .await
            .map_err(ProjectGroupError::db(format!(
                "loading routes involving project {project_id} in group {group_id}"
            )))
    }

    pub async fn remove_project(
        &self,
        group_id: i32,
        project_id: i32,
    ) -> Result<(), ProjectGroupError> {
        let result = project_group_members::Entity::delete_many()
            .filter(project_group_members::Column::ProjectId.eq(project_id))
            .filter(project_group_members::Column::GroupId.eq(group_id))
            .exec(self.db.as_ref())
            .await
            .map_err(ProjectGroupError::db(format!(
                "removing project {project_id} from project group {group_id}"
            )))?;
        if result.rows_affected == 0 {
            self.find_group(group_id).await?;
            return Err(ProjectGroupError::NotAMember {
                group_id,
                project_id,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_collapses_separators_and_trims() {
        assert_eq!(ProjectGroupService::slugify("Acme Shop"), "acme-shop");
        assert_eq!(
            ProjectGroupService::slugify("  --Acme__Shop!! v2 "),
            "acme-shop-v2"
        );
        assert_eq!(ProjectGroupService::slugify("¡Hola!"), "hola");
        assert_eq!(ProjectGroupService::slugify("日本"), "");
    }

    #[test]
    fn slugify_truncates_to_max_len_without_trailing_dash() {
        let name = format!("{} tail", "a".repeat(MAX_SLUG_LEN - 1));
        let slug = ProjectGroupService::slugify(&name);
        assert!(slug.len() <= MAX_SLUG_LEN);
        assert!(!slug.ends_with('-'));
    }

    #[test]
    fn validate_slug_rejects_bad_shapes() {
        assert!(ProjectGroupService::validate_slug("acme-shop").is_ok());
        for bad in ["", "-acme", "acme-", "Acme", "acme shop", "acme_shop"] {
            assert!(
                ProjectGroupService::validate_slug(bad).is_err(),
                "expected '{bad}' to be rejected"
            );
        }
    }

    #[test]
    fn validate_name_trims_and_bounds() {
        assert_eq!(
            ProjectGroupService::validate_name("  Acme  ").unwrap(),
            "Acme"
        );
        assert!(ProjectGroupService::validate_name("   ").is_err());
        assert!(ProjectGroupService::validate_name(&"x".repeat(MAX_NAME_LEN + 1)).is_err());
    }

    #[test]
    fn empty_description_clears() {
        assert_eq!(
            ProjectGroupService::normalize_description(Some("  ".into())).unwrap(),
            None
        );
        assert_eq!(
            ProjectGroupService::normalize_description(Some(" hi ".into())).unwrap(),
            Some("hi".into())
        );
    }

    #[test]
    fn page_params_clamp() {
        assert_eq!(
            ProjectGroupService::page_params(None, None),
            (1, DEFAULT_PAGE_SIZE)
        );
        assert_eq!(
            ProjectGroupService::page_params(Some(0), Some(1000)),
            (1, MAX_PAGE_SIZE)
        );
    }

    // -----------------------------------------------------------------
    // Database-backed behaviour. These boot a TimescaleDB container and
    // skip (not fail) when no container runtime is available.
    // -----------------------------------------------------------------

    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
    use temps_database::test_utils::TestDatabase;

    async fn try_test_db() -> Option<TestDatabase> {
        match TestDatabase::with_migrations().await {
            Ok(db) => Some(db),
            Err(e) => {
                eprintln!("Docker/TimescaleDB unavailable, skipping project group test: {e}");
                None
            }
        }
    }

    async fn insert_project(conn: &impl ConnectionTrait, id: i32, name: &str) {
        let stmt = Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            INSERT INTO projects (
                id, name, repo_name, repo_owner, directory, main_branch,
                preset, preset_config, deployment_config, created_at, updated_at,
                slug, is_deleted, deleted_at, last_deployment, is_public_repo,
                git_url, git_provider_connection_id, attack_mode,
                enable_preview_environments, source_type
            )
            VALUES (
                $1, $2, 'repo', 'owner', '.', 'main',
                'nextjs', NULL, NULL, NOW(), NOW(),
                $3, FALSE, NULL, NULL, TRUE,
                NULL, NULL, FALSE, FALSE, 'git'
            )
            "#,
            [id.into(), name.into(), format!("{name}-{id}").into()],
        );
        conn.execute(stmt).await.expect("insert project");
    }

    fn create_req(name: &str, project_ids: Vec<i32>) -> CreateProjectGroupRequest {
        CreateProjectGroupRequest {
            name: name.into(),
            slug: None,
            description: None,
            project_ids,
        }
    }

    fn ids(group: &ProjectGroupWithMembers) -> Vec<i32> {
        group.members.iter().map(|p| p.id).collect()
    }

    #[tokio::test]
    async fn create_places_projects_in_order_and_derives_slug() {
        let Some(db) = try_test_db().await else {
            return;
        };
        let conn = db.connection_arc();
        insert_project(conn.as_ref(), 9101, "web").await;
        insert_project(conn.as_ref(), 9102, "api").await;
        let svc = ProjectGroupService::new(conn);

        let group = svc
            .create_group(create_req("Acme Shop", vec![9102, 9101, 9102]))
            .await
            .unwrap();
        assert_eq!(group.group.slug, "acme-shop");
        // Duplicates are dropped and the requested order is kept.
        assert_eq!(ids(&group), vec![9102, 9101]);

        let by_slug = svc.get_group_by_slug("acme-shop").await.unwrap();
        assert_eq!(by_slug.group.id, group.group.id);
    }

    #[tokio::test]
    async fn duplicate_slug_is_a_conflict() {
        let Some(db) = try_test_db().await else {
            return;
        };
        let svc = ProjectGroupService::new(db.connection_arc());
        svc.create_group(create_req("Acme", vec![])).await.unwrap();
        let err = svc
            .create_group(create_req("acme", vec![]))
            .await
            .unwrap_err();
        assert!(matches!(err, ProjectGroupError::SlugConflict { ref slug } if slug == "acme"));
    }

    #[tokio::test]
    async fn create_with_missing_project_fails_without_creating_the_group() {
        let Some(db) = try_test_db().await else {
            return;
        };
        let svc = ProjectGroupService::new(db.connection_arc());
        let err = svc
            .create_group(create_req("Ghost", vec![424242]))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ProjectGroupError::ProjectNotFound { project_id: 424242 }
        ));
        let (groups, total) = svc.list_groups(None, None, &HashSet::new()).await.unwrap();
        assert_eq!(total, 0);
        assert!(groups.is_empty());
    }

    #[tokio::test]
    async fn adding_moves_a_project_between_groups() {
        let Some(db) = try_test_db().await else {
            return;
        };
        let conn = db.connection_arc();
        insert_project(conn.as_ref(), 9201, "web").await;
        insert_project(conn.as_ref(), 9202, "api").await;
        let svc = ProjectGroupService::new(conn);

        let a = svc.create_group(create_req("A", vec![9201])).await.unwrap();
        let b = svc.create_group(create_req("B", vec![9202])).await.unwrap();

        let previous = svc.add_project(b.group.id, 9201).await.unwrap();
        assert_eq!(previous, Some(a.group.id));
        assert!(svc.get_group(a.group.id).await.unwrap().members.is_empty());
        // Appended after the existing member.
        assert_eq!(
            ids(&svc.get_group(b.group.id).await.unwrap()),
            vec![9202, 9201]
        );

        // Re-adding to the same group is a no-op.
        assert_eq!(
            svc.add_project(b.group.id, 9201).await.unwrap(),
            Some(b.group.id)
        );
        let membership = svc.group_for_project(9201).await.unwrap().unwrap();
        assert_eq!(membership.group.id, b.group.id);
    }

    #[tokio::test]
    async fn moving_a_project_stops_it_following_the_old_repository() {
        let Some(db) = try_test_db().await else {
            return;
        };
        let conn = db.connection_arc();
        insert_project(conn.as_ref(), 9251, "web").await;
        let svc = ProjectGroupService::new(conn.clone());

        let a = svc
            .create_group(create_req("Mono A", vec![9251]))
            .await
            .unwrap();
        let b = svc
            .create_group(create_req("Mono B", vec![]))
            .await
            .unwrap();
        assert!(
            a.inherits_repository.is_empty(),
            "a project never follows a repository just by joining"
        );

        // As if it had been asked to follow A's repository.
        project_group_members::Entity::update_many()
            .col_expr(
                project_group_members::Column::InheritsRepository,
                Expr::value(true),
            )
            .filter(project_group_members::Column::ProjectId.eq(9251))
            .exec(conn.as_ref())
            .await
            .unwrap();
        assert!(svc
            .get_group(a.group.id)
            .await
            .unwrap()
            .inherits_repository
            .contains(&9251));

        // B may point at another repository: moving resets the flag.
        svc.add_project(b.group.id, 9251).await.unwrap();
        let b = svc.get_group(b.group.id).await.unwrap();
        assert_eq!(ids(&b), vec![9251]);
        assert!(b.inherits_repository.is_empty());
    }

    #[tokio::test]
    async fn remove_reports_non_members_and_missing_groups() {
        let Some(db) = try_test_db().await else {
            return;
        };
        let conn = db.connection_arc();
        insert_project(conn.as_ref(), 9301, "web").await;
        let svc = ProjectGroupService::new(conn);
        let group = svc.create_group(create_req("G", vec![9301])).await.unwrap();

        svc.remove_project(group.group.id, 9301).await.unwrap();
        assert!(svc.group_for_project(9301).await.unwrap().is_none());

        let err = svc.remove_project(group.group.id, 9301).await.unwrap_err();
        assert!(matches!(err, ProjectGroupError::NotAMember { .. }));
        let err = svc.remove_project(999_999, 9301).await.unwrap_err();
        assert!(matches!(
            err,
            ProjectGroupError::NotFound { group_id: 999_999 }
        ));
    }

    #[tokio::test]
    async fn deleting_a_group_ungroups_but_keeps_projects() {
        let Some(db) = try_test_db().await else {
            return;
        };
        let conn = db.connection_arc();
        insert_project(conn.as_ref(), 9401, "web").await;
        let svc = ProjectGroupService::new(conn.clone());
        let group = svc.create_group(create_req("G", vec![9401])).await.unwrap();

        let (deleted, released) = svc.delete_group(group.group.id).await.unwrap();
        assert_eq!(deleted.id, group.group.id);
        assert_eq!(released, vec![9401]);
        assert!(svc.group_for_project(9401).await.unwrap().is_none());
        let project = projects::Entity::find_by_id(9401)
            .one(conn.as_ref())
            .await
            .unwrap();
        assert!(
            project.is_some(),
            "deleting a group must not delete its projects"
        );
    }

    #[tokio::test]
    async fn soft_deleted_projects_are_not_listed_or_addable() {
        let Some(db) = try_test_db().await else {
            return;
        };
        let conn = db.connection_arc();
        insert_project(conn.as_ref(), 9501, "web").await;
        insert_project(conn.as_ref(), 9502, "old").await;
        let svc = ProjectGroupService::new(conn.clone());
        let group = svc
            .create_group(create_req("G", vec![9501, 9502]))
            .await
            .unwrap();

        conn.execute_unprepared("UPDATE projects SET is_deleted = TRUE WHERE id = 9502")
            .await
            .unwrap();
        assert_eq!(
            ids(&svc.get_group(group.group.id).await.unwrap()),
            vec![9501]
        );

        let err = svc.add_project(group.group.id, 9502).await.unwrap_err();
        assert!(matches!(
            err,
            ProjectGroupError::ProjectNotFound { project_id: 9502 }
        ));
    }

    #[tokio::test]
    async fn update_renames_and_checks_slug_conflicts() {
        let Some(db) = try_test_db().await else {
            return;
        };
        let svc = ProjectGroupService::new(db.connection_arc());
        let a = svc.create_group(create_req("A", vec![])).await.unwrap();
        svc.create_group(create_req("B", vec![])).await.unwrap();

        let updated = svc
            .update_group(
                a.group.id,
                UpdateProjectGroupRequest {
                    name: Some("Alpha".into()),
                    slug: Some("alpha".into()),
                    description: Some("Storefront".into()),
                },
            )
            .await
            .unwrap();
        assert_eq!(updated.group.name, "Alpha");
        assert_eq!(updated.group.slug, "alpha");
        assert_eq!(updated.group.description.as_deref(), Some("Storefront"));

        let err = svc
            .update_group(
                a.group.id,
                UpdateProjectGroupRequest {
                    slug: Some("b".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ProjectGroupError::SlugConflict { .. }));
    }

    #[tokio::test]
    async fn groups_whose_members_are_all_hidden_are_invisible() {
        let Some(db) = try_test_db().await else {
            return;
        };
        let conn = db.connection_arc();
        insert_project(conn.as_ref(), 9601, "mine").await;
        insert_project(conn.as_ref(), 9602, "theirs").await;
        insert_project(conn.as_ref(), 9603, "theirs-too").await;
        let svc = ProjectGroupService::new(conn);

        let mixed = svc
            .create_group(create_req("Mixed", vec![9601, 9602]))
            .await
            .unwrap();
        let foreign = svc
            .create_group(create_req("Foreign", vec![9603]))
            .await
            .unwrap();
        let empty = svc.create_group(create_req("Empty", vec![])).await.unwrap();

        let hidden: HashSet<i32> = [9602, 9603].into_iter().collect();
        let (groups, total) = svc.list_groups(None, None, &hidden).await.unwrap();
        let slugs: Vec<&str> = groups.iter().map(|g| g.group.slug.as_str()).collect();
        // Name order; the all-hidden group is gone and not counted.
        assert_eq!(slugs, vec!["empty", "mixed"]);
        assert_eq!(total, 2);

        assert!(ProjectGroupService::is_visible(&mixed, &hidden));
        assert!(ProjectGroupService::is_visible(&empty, &hidden));
        assert!(!ProjectGroupService::is_visible(&foreign, &hidden));

        // Nothing hidden: everything is listed.
        let (_, total) = svc.list_groups(None, None, &HashSet::new()).await.unwrap();
        assert_eq!(total, 3);
    }
}
