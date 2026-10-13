// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Business logic for project groups. Authorization is the handler's job;
//! everything here assumes the caller has already been allowed.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::Utc;
use sea_orm::sea_query::{Condition, Expr, LockType, Query};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set, SqlErr, TransactionTrait,
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

/// The outcome of placing a project in a group: where it came from and the
/// routes of that previous group that the move dropped with it.
#[derive(Debug, Clone)]
pub struct ProjectPlacement {
    pub previous_group_id: Option<i32>,
    /// Routes deleted (by the database, as the project left its previous
    /// group) because they involved the project. Empty for a first placement
    /// or a no-op.
    pub dropped_routes: Vec<project_group_routes::Model>,
}

/// The outcome of deleting a group: the group itself, the projects that
/// became ungrouped, and the routes that were deleted with it.
#[derive(Debug, Clone)]
pub struct GroupDeletion {
    pub group: project_groups::Model,
    /// Live projects that were members, in display order. The projects
    /// themselves are untouched.
    pub released_project_ids: Vec<i32>,
    /// Routes of the group, deleted by the database along with it.
    pub dropped_routes: Vec<project_group_routes::Model>,
}

#[derive(Default)]
struct LoadedMembers {
    projects: Vec<projects::Model>,
    inherits_repository: HashSet<i32>,
}

/// What a set of projects is in right now, and what leaving it would drop.
struct LeavingState {
    /// `project_id -> group_id` for every listed project that has a
    /// membership, whatever the group.
    current_group: HashMap<i32, i32>,
    /// Routes the database deletes when the projects that are not already in
    /// the destination group leave their current group.
    dropped_routes: Vec<project_group_routes::Model>,
}

/// # Lock order
///
/// Every transaction that changes who is in which group takes its locks in
/// this order, and the route guard trigger
/// (`enforce_project_group_route_membership`) takes the same locks in the
/// same order, so the writers serialize against each other and cannot cycle:
///
/// 1. `projects` rows of every affected project, `FOR NO KEY UPDATE`, in
///    ascending id ([`Self::lock_projects`]). This is the lock that makes
///    "read the current memberships and routes, authorize, then write" safe
///    even for a project that is not in any group yet: there is no membership
///    row to lock for it, but there is always a project row. (`NO KEY UPDATE`
///    rather than `UPDATE`: foreign-key checks of unrelated inserts, such as
///    deployments, take `KEY SHARE` and are not blocked.) The route trigger
///    takes the same rows `FOR SHARE`.
/// 2. The destination `project_groups` row, `FOR KEY SHARE` when placing into
///    it, `FOR UPDATE` when deleting it.
/// 3. `project_group_members` rows, `FOR UPDATE`, in ascending project id.
/// 4. `project_group_routes` rows (read after 3, deleted by the membership
///    trigger or by the group's cascade).
///
/// Everything that reads memberships or routes to *decide* something (which
/// routes a move drops, whether they were authorized) does so only after
/// step 1, so the answer cannot change before the transaction ends. Writers
/// that skip step 1 are still safe against the route trigger (which locks the
/// membership rows) but not against each other: do not add one.
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
        Ok(self.create_group_checked(req, None).await?.0)
    }

    /// Like [`Self::create_group`], for callers that authorized the routes a
    /// move would drop. A listed project that is already in another group
    /// leaves it, and the database deletes that group's routes involving it.
    /// With `authorized_route_ids` set, the membership rows are locked and
    /// the routes re-read inside the transaction; a route that is not in the
    /// set (created after the caller checked) fails with
    /// [`ProjectGroupError::RouteSetChanged`] instead of being dropped
    /// unauthorized. Returns the routes that were dropped.
    pub async fn create_group_checked(
        &self,
        req: CreateProjectGroupRequest,
        authorized_route_ids: Option<&HashSet<i32>>,
    ) -> Result<(ProjectGroupWithMembers, Vec<project_group_routes::Model>), ProjectGroupError>
    {
        let name = Self::validate_name(&req.name)?;
        let slug = match req.slug.as_deref().map(str::trim) {
            Some(slug) if !slug.is_empty() => slug.to_string(),
            _ => Self::slugify(&name),
        };
        Self::validate_slug(&slug)?;
        let description = Self::normalize_description(req.description)?;
        self.ensure_slug_free(&slug, None).await?;

        // Deduplicate while preserving order.
        let mut project_ids: Vec<i32> = Vec::with_capacity(req.project_ids.len());
        for id in req.project_ids {
            if !project_ids.contains(&id) {
                project_ids.push(id);
            }
        }

        let txn = self.db.begin().await.map_err(ProjectGroupError::db(
            "starting project group create transaction",
        ))?;
        let result = async {
            // Step 1 of the lock order: lock the listed projects, then validate
            // them, so a project cannot be soft-deleted or moved by another
            // request between the check and the placement below.
            let locked = Self::lock_projects(&txn, &project_ids).await?;
            if let Some(&missing) = project_ids.iter().find(|id| locked.get(id) != Some(&false)) {
                return Err(ProjectGroupError::ProjectNotFound {
                    project_id: missing,
                });
            }
            // Only now read what the projects are in and which routes leaving
            // would drop (locking the membership rows, step 3), and authorize
            // exactly those.
            let LeavingState {
                current_group,
                dropped_routes,
            } = Self::leaving_state(&txn, &project_ids, None, true).await?;
            Self::ensure_routes_authorized(&dropped_routes, authorized_route_ids)?;
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
                Self::place_membership(
                    &txn,
                    group.id,
                    *project_id,
                    Some(position as i32),
                    current_group.get(project_id).copied(),
                )
                .await?;
            }
            Ok::<_, ProjectGroupError>((group, dropped_routes))
        }
        .await;
        let (group, dropped_routes) =
            Self::end_transaction(txn, result, format!("creation of project group '{slug}'"))
                .await?;

        Ok((self.with_members(group).await?, dropped_routes))
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
    ///
    /// The database deletes the group's routes with it; callers that expose
    /// this to users must authorize those routes first (see
    /// [`Self::routes_of_group`]) and use [`Self::delete_group_checked`].
    pub async fn delete_group(
        &self,
        group_id: i32,
    ) -> Result<(project_groups::Model, Vec<i32>), ProjectGroupError> {
        let deletion = self.remove_group(group_id, None).await?;
        Ok((deletion.group, deletion.released_project_ids))
    }

    /// [`Self::delete_group`] for a caller that authorized exactly
    /// `authorized_route_ids` (see [`Self::routes_of_group`]). The group row
    /// and every membership row are locked, then the routes are read again:
    /// a route created after the caller checked fails with
    /// [`ProjectGroupError::RouteSetChanged`] and nothing is deleted, rather
    /// than being dropped unauthorized. Returns the routes that were dropped.
    pub async fn delete_group_checked(
        &self,
        group_id: i32,
        authorized_route_ids: &HashSet<i32>,
    ) -> Result<GroupDeletion, ProjectGroupError> {
        self.remove_group(group_id, Some(authorized_route_ids))
            .await
    }

    async fn remove_group(
        &self,
        group_id: i32,
        authorized_route_ids: Option<&HashSet<i32>>,
    ) -> Result<GroupDeletion, ProjectGroupError> {
        let txn = self
            .db
            .begin()
            .await
            .map_err(ProjectGroupError::db(format!(
                "starting transaction to delete project group {group_id}"
            )))?;
        let result = async {
            // Lock order (see the type docs): the projects first, then the group
            // row, then its membership rows. Which projects are members is read
            // without a lock to know which rows to take; a project that joined in
            // between is covered by the group and membership locks below (its
            // route inserts and placements wait for this transaction).
            let member_ids: Vec<i32> = project_group_members::Entity::find()
                .filter(project_group_members::Column::GroupId.eq(group_id))
                .select_only()
                .column(project_group_members::Column::ProjectId)
                .order_by_asc(project_group_members::Column::ProjectId)
                .into_tuple::<i32>()
                .all(&txn)
                .await
                .map_err(ProjectGroupError::db(format!(
                    "listing members of project group {group_id}"
                )))?;
            Self::lock_projects(&txn, &member_ids).await?;
            // A route insert share-locks its endpoints' membership rows and a
            // membership insert key-share-locks the group row, so either lands
            // before the routes are read below or waits until the group is gone
            // and is rejected.
            let group = project_groups::Entity::find_by_id(group_id)
                .lock_exclusive()
                .one(&txn)
                .await
                .map_err(ProjectGroupError::db(format!(
                    "loading project group {group_id}"
                )))?
                .ok_or(ProjectGroupError::NotFound { group_id })?;
            let mut memberships = project_group_members::Entity::find()
                .filter(project_group_members::Column::GroupId.eq(group_id))
                .order_by_asc(project_group_members::Column::ProjectId)
                .lock_exclusive()
                .all(&txn)
                .await
                .map_err(ProjectGroupError::db(format!(
                    "locking memberships of project group {group_id}"
                )))?;
            let dropped_routes = Self::routes_in_group(&txn, group_id).await?;
            Self::ensure_routes_authorized(&dropped_routes, authorized_route_ids)?;

            // Only live projects are reported as released, in display order.
            memberships.sort_by_key(|m| (m.position, m.project_id));
            let member_ids: Vec<i32> = memberships.iter().map(|m| m.project_id).collect();
            let live: HashSet<i32> = if member_ids.is_empty() {
                HashSet::new()
            } else {
                projects::Entity::find()
                    .filter(projects::Column::Id.is_in(member_ids.iter().copied()))
                    .filter(projects::Column::IsDeleted.eq(false))
                    .select_only()
                    .column(projects::Column::Id)
                    .into_tuple::<i32>()
                    .all(&txn)
                    .await
                    .map_err(ProjectGroupError::db(format!(
                        "loading live members of project group {group_id}"
                    )))?
                    .into_iter()
                    .collect()
            };
            let released_project_ids = member_ids
                .into_iter()
                .filter(|id| live.contains(id))
                .collect();

            project_groups::Entity::delete_by_id(group_id)
                .exec(&txn)
                .await
                .map_err(ProjectGroupError::db(format!(
                    "deleting project group {group_id}"
                )))?;
            Ok::<_, ProjectGroupError>(GroupDeletion {
                group,
                released_project_ids,
                dropped_routes,
            })
        }
        .await;
        Self::end_transaction(txn, result, format!("deletion of project group {group_id}")).await
    }

    /// Puts `project_id` into `group_id` (at `position`, or at the end of the
    /// group when `None`), moving it out of whatever group it was in.
    ///
    /// The write is a compare-and-swap on the prior membership:
    /// `expected_prior_group` is the group the caller read under its locks
    /// (`None`: no membership). If the row is in any other state when the
    /// statement runs, it is left untouched and
    /// [`ProjectGroupError::MembershipChanged`] is returned, so a move can
    /// never silently take a project out of a group (and, through the
    /// membership trigger, delete that group's routes) that the caller did
    /// not authorize. Under the lock protocol this cannot happen; it is the
    /// last line of defence against a writer that skipped it.
    async fn place_membership<C: ConnectionTrait>(
        conn: &C,
        group_id: i32,
        project_id: i32,
        position: Option<i32>,
        expected_prior_group: Option<i32>,
    ) -> Result<(), ProjectGroupError> {
        // Group ids come from a SERIAL column, so -1 never matches a row:
        // "expected no membership" makes any conflict a failed swap.
        let expected = expected_prior_group.unwrap_or(-1);
        // A project entering a group (or moving between groups) never follows
        // the new group's repository until asked to. The default position is
        // computed in the same statement as the insert so two concurrent
        // placements cannot both read the same maximum.
        let rows = conn
            .query_all(sea_orm::Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                "INSERT INTO project_group_members \
                     (project_id, group_id, position, inherits_repository, added_at) \
                 VALUES ($1, $2, \
                     COALESCE($3::int, (SELECT COALESCE(MAX(position) + 1, 0) \
                                          FROM project_group_members WHERE group_id = $2)), \
                     FALSE, $4) \
                 ON CONFLICT (project_id) DO UPDATE SET \
                     group_id = EXCLUDED.group_id, \
                     position = EXCLUDED.position, \
                     inherits_repository = FALSE, \
                     added_at = EXCLUDED.added_at \
                 WHERE project_group_members.group_id = $5 \
                 RETURNING project_id",
                [
                    project_id.into(),
                    group_id.into(),
                    position.into(),
                    Utc::now().into(),
                    expected.into(),
                ],
            ))
            .await
            .map_err(|e| match e.sql_err() {
                // The destination group was deleted after it was checked.
                Some(SqlErr::ForeignKeyConstraintViolation(_)) => {
                    ProjectGroupError::NotFound { group_id }
                }
                _ => ProjectGroupError::Database {
                    operation: format!("placing project {project_id} in project group {group_id}"),
                    source: e,
                },
            })?;
        if rows.is_empty() {
            return Err(ProjectGroupError::MembershipChanged {
                group_id,
                project_id,
            });
        }
        Ok(())
    }

    /// Step 1 of the lock order: locks the rows of `projects` for the whole
    /// transaction, `FOR NO KEY UPDATE`, in ascending id. Returns
    /// `id -> is_deleted` for the ids that exist; callers that need live
    /// projects check it, because the answer cannot change afterwards.
    async fn lock_projects<C: ConnectionTrait>(
        conn: &C,
        project_ids: &[i32],
    ) -> Result<HashMap<i32, bool>, ProjectGroupError> {
        if project_ids.is_empty() {
            return Ok(HashMap::new());
        }
        Ok(projects::Entity::find()
            .select_only()
            .column(projects::Column::Id)
            .column(projects::Column::IsDeleted)
            .filter(projects::Column::Id.is_in(project_ids.iter().copied()))
            .order_by_asc(projects::Column::Id)
            .lock(LockType::NoKeyUpdate)
            .into_tuple::<(i32, bool)>()
            .all(conn)
            .await
            .map_err(ProjectGroupError::db(format!(
                "locking projects {project_ids:?} for a group membership change"
            )))?
            .into_iter()
            .collect())
    }

    /// Places `project_id` at the end of `group_id`, moving it out of any
    /// other group. Returns the group it was in before, if any. Adding a
    /// project to the group it is already in is a no-op.
    ///
    /// Moving a project out of a group deletes that group's routes involving
    /// it; callers that expose this to users must authorize those routes
    /// first and use [`Self::add_project_checked`].
    pub async fn add_project(
        &self,
        group_id: i32,
        project_id: i32,
    ) -> Result<Option<i32>, ProjectGroupError> {
        Ok(self
            .place_project(group_id, project_id, None)
            .await?
            .previous_group_id)
    }

    /// [`Self::add_project`] for a caller that authorized exactly
    /// `authorized_route_ids` (see [`Self::routes_dropped_by_leaving_group`]).
    /// The routes the move drops are read again under a row lock on the
    /// membership; if one is not in the set, nothing changes and
    /// [`ProjectGroupError::RouteSetChanged`] is returned.
    pub async fn add_project_checked(
        &self,
        group_id: i32,
        project_id: i32,
        authorized_route_ids: &HashSet<i32>,
    ) -> Result<ProjectPlacement, ProjectGroupError> {
        self.place_project(group_id, project_id, Some(authorized_route_ids))
            .await
    }

    async fn place_project(
        &self,
        group_id: i32,
        project_id: i32,
        authorized_route_ids: Option<&HashSet<i32>>,
    ) -> Result<ProjectPlacement, ProjectGroupError> {
        let txn = self
            .db
            .begin()
            .await
            .map_err(ProjectGroupError::db(format!(
                "starting transaction to place project {project_id} in project group {group_id}"
            )))?;
        let result = async {
            // Step 1 of the lock order. The project row exists whether or not the
            // project is in a group, so this serializes against every other
            // membership change and route insert of the project even when there
            // is no membership row yet to lock.
            let locked = Self::lock_projects(&txn, &[project_id]).await?;
            // Step 2: the destination group cannot be deleted while we place into
            // it.
            let group_exists = project_groups::Entity::find_by_id(group_id)
                .select_only()
                .column(project_groups::Column::Id)
                .lock(LockType::KeyShare)
                .into_tuple::<i32>()
                .one(&txn)
                .await
                .map_err(ProjectGroupError::db(format!(
                    "locking project group {group_id}"
                )))?
                .is_some();
            if !group_exists {
                return Err(ProjectGroupError::NotFound { group_id });
            }
            if locked.get(&project_id) != Some(&false) {
                return Err(ProjectGroupError::ProjectNotFound { project_id });
            }
            // Step 3: the membership row. Read only now, with the project locked,
            // so what is found is what the move drops.
            let previous = project_group_members::Entity::find_by_id(project_id)
                .lock_exclusive()
                .one(&txn)
                .await
                .map_err(ProjectGroupError::db(format!(
                    "loading group membership of project {project_id}"
                )))?;
            if let Some(ref m) = previous {
                if m.group_id == group_id {
                    return Ok(ProjectPlacement {
                        previous_group_id: Some(group_id),
                        dropped_routes: Vec::new(),
                    });
                }
            }
            let dropped_routes = match previous {
                Some(ref m) => Self::routes_of_member(&txn, m.group_id, project_id).await?,
                None => Vec::new(),
            };
            Self::ensure_routes_authorized(&dropped_routes, authorized_route_ids)?;

            Self::place_membership(
                &txn,
                group_id,
                project_id,
                None,
                previous.as_ref().map(|m| m.group_id),
            )
            .await?;
            Ok::<_, ProjectGroupError>(ProjectPlacement {
                previous_group_id: previous.map(|m| m.group_id),
                dropped_routes,
            })
        }
        .await;
        Self::end_transaction(
            txn,
            result,
            format!("placement of project {project_id} in project group {group_id}"),
        )
        .await
    }

    /// Fails when `dropped` holds a route outside `authorized_route_ids`.
    /// `None` means the caller does not authorize per route (internal
    /// callers), and nothing is checked.
    fn ensure_routes_authorized(
        dropped: &[project_group_routes::Model],
        authorized_route_ids: Option<&HashSet<i32>>,
    ) -> Result<(), ProjectGroupError> {
        let Some(authorized) = authorized_route_ids else {
            return Ok(());
        };
        match dropped.iter().find(|r| !authorized.contains(&r.id)) {
            Some(route) => Err(ProjectGroupError::RouteSetChanged {
                group_id: route.group_id,
                route_id: route.id,
            }),
            None => Ok(()),
        }
    }

    /// Every route of `group_id`, in id order.
    async fn routes_in_group<C: ConnectionTrait>(
        conn: &C,
        group_id: i32,
    ) -> Result<Vec<project_group_routes::Model>, ProjectGroupError> {
        project_group_routes::Entity::find()
            .filter(project_group_routes::Column::GroupId.eq(group_id))
            .order_by_asc(project_group_routes::Column::Id)
            .all(conn)
            .await
            .map_err(ProjectGroupError::db(format!(
                "loading routes of project group {group_id}"
            )))
    }

    /// Routes of `group_id` that have `project_id` as source or target.
    async fn routes_of_member<C: ConnectionTrait>(
        conn: &C,
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
            .order_by_asc(project_group_routes::Column::Id)
            .all(conn)
            .await
            .map_err(ProjectGroupError::db(format!(
                "loading routes involving project {project_id} in group {group_id}"
            )))
    }

    /// What `project_ids` are in now, and the routes the database deletes
    /// when every one of them leaves the group it is in, except groups equal
    /// to `keep_group_id` (a project already in the destination group does not
    /// move). Two queries regardless of how many projects are listed. With
    /// `lock`, the membership rows are locked `FOR UPDATE` (in project id
    /// order) so the answer stays true until the surrounding transaction
    /// ends; the caller must already hold the project rows (see
    /// [`Self::lock_projects`]), or a project without a membership row can
    /// still be placed by another request in between.
    async fn leaving_state<C: ConnectionTrait>(
        conn: &C,
        project_ids: &[i32],
        keep_group_id: Option<i32>,
        lock: bool,
    ) -> Result<LeavingState, ProjectGroupError> {
        if project_ids.is_empty() {
            return Ok(LeavingState {
                current_group: HashMap::new(),
                dropped_routes: Vec::new(),
            });
        }
        let mut memberships = project_group_members::Entity::find()
            .filter(project_group_members::Column::ProjectId.is_in(project_ids.iter().copied()))
            .order_by_asc(project_group_members::Column::ProjectId);
        if lock {
            memberships = memberships.lock_exclusive();
        }
        let memberships = memberships
            .all(conn)
            .await
            .map_err(ProjectGroupError::db(format!(
                "loading group memberships of projects {project_ids:?}"
            )))?;
        let current_group: HashMap<i32, i32> = memberships
            .iter()
            .map(|m| (m.project_id, m.group_id))
            .collect();
        let leaving: HashSet<(i32, i32)> = memberships
            .into_iter()
            .filter(|m| Some(m.group_id) != keep_group_id)
            .map(|m| (m.group_id, m.project_id))
            .collect();
        if leaving.is_empty() {
            return Ok(LeavingState {
                current_group,
                dropped_routes: Vec::new(),
            });
        }
        let group_ids: HashSet<i32> = leaving.iter().map(|(g, _)| *g).collect();
        let leaving_ids: HashSet<i32> = leaving.iter().map(|(_, p)| *p).collect();
        let routes = project_group_routes::Entity::find()
            .filter(project_group_routes::Column::GroupId.is_in(group_ids.iter().copied()))
            .filter(
                Condition::any()
                    .add(
                        project_group_routes::Column::SourceProjectId
                            .is_in(leaving_ids.iter().copied()),
                    )
                    .add(
                        project_group_routes::Column::TargetProjectId
                            .is_in(leaving_ids.iter().copied()),
                    ),
            )
            .order_by_asc(project_group_routes::Column::Id)
            .all(conn)
            .await
            .map_err(ProjectGroupError::db(format!(
                "loading routes involving projects {project_ids:?}"
            )))?;
        let dropped_routes = routes
            .into_iter()
            .filter(|r| {
                leaving.contains(&(r.group_id, r.source_project_id))
                    || leaving.contains(&(r.group_id, r.target_project_id))
            })
            .collect();
        Ok(LeavingState {
            current_group,
            dropped_routes,
        })
    }

    /// The routes that placing each of `project_ids` in `destination_group_id`
    /// (`None`: a group that does not exist yet) would delete, because the
    /// project leaves its current group and a route only lives while both its
    /// projects are in the route's group. Read-only; callers authorize
    /// these before the move and pass their ids to the `*_checked` mutators.
    pub async fn routes_dropped_by_leaving_group(
        &self,
        project_ids: &[i32],
        destination_group_id: Option<i32>,
    ) -> Result<Vec<project_group_routes::Model>, ProjectGroupError> {
        Ok(
            Self::leaving_state(self.db.as_ref(), project_ids, destination_group_id, false)
                .await?
                .dropped_routes,
        )
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
        Self::routes_of_member(self.db.as_ref(), group_id, project_id).await
    }

    /// All routes of `group_id`: the routes the database deletes with the
    /// group. Read-only; callers authorize these before deleting the group
    /// and pass their ids to [`Self::delete_group_checked`].
    pub async fn routes_of_group(
        &self,
        group_id: i32,
    ) -> Result<Vec<project_group_routes::Model>, ProjectGroupError> {
        Self::routes_in_group(self.db.as_ref(), group_id).await
    }

    /// Slugs of the groups in `group_ids`, one query. Ids that no longer
    /// exist are absent.
    pub async fn group_slugs(
        &self,
        group_ids: &[i32],
    ) -> Result<HashMap<i32, String>, ProjectGroupError> {
        if group_ids.is_empty() {
            return Ok(HashMap::new());
        }
        Ok(project_groups::Entity::find()
            .filter(project_groups::Column::Id.is_in(group_ids.iter().copied()))
            .all(self.db.as_ref())
            .await
            .map_err(ProjectGroupError::db(format!(
                "loading slugs of project groups {group_ids:?}"
            )))?
            .into_iter()
            .map(|g| (g.id, g.slug))
            .collect())
    }

    /// Removes `project_id` from `group_id`. The database drops the group's
    /// routes involving it; callers that expose this to users must authorize
    /// those first and use [`Self::remove_project_checked`].
    pub async fn remove_project(
        &self,
        group_id: i32,
        project_id: i32,
    ) -> Result<(), ProjectGroupError> {
        self.remove_member(group_id, project_id, None).await?;
        Ok(())
    }

    /// [`Self::remove_project`] for a caller that authorized exactly
    /// `authorized_route_ids`. The routes involving the project are read
    /// again under a row lock on its membership, so a route created after the
    /// caller checked fails with [`ProjectGroupError::RouteSetChanged`] rather
    /// than being dropped unauthorized. Returns the routes that were dropped.
    pub async fn remove_project_checked(
        &self,
        group_id: i32,
        project_id: i32,
        authorized_route_ids: &HashSet<i32>,
    ) -> Result<Vec<project_group_routes::Model>, ProjectGroupError> {
        self.remove_member(group_id, project_id, Some(authorized_route_ids))
            .await
    }

    async fn remove_member(
        &self,
        group_id: i32,
        project_id: i32,
        authorized_route_ids: Option<&HashSet<i32>>,
    ) -> Result<Vec<project_group_routes::Model>, ProjectGroupError> {
        let txn = self
            .db
            .begin()
            .await
            .map_err(ProjectGroupError::db(format!(
                "starting transaction to remove project {project_id} from project group {group_id}"
            )))?;
        let result = async {
            // Step 1 of the lock order, whether or not the project is still
            // live: removing a soft-deleted project must keep working.
            Self::lock_projects(&txn, &[project_id]).await?;
            let membership = project_group_members::Entity::find_by_id(project_id)
                .filter(project_group_members::Column::GroupId.eq(group_id))
                .lock_exclusive()
                .one(&txn)
                .await
                .map_err(ProjectGroupError::db(format!(
                    "loading membership of project {project_id} in project group {group_id}"
                )))?;
            if membership.is_none() {
                return Ok::<_, ProjectGroupError>(None);
            }
            let dropped_routes = Self::routes_of_member(&txn, group_id, project_id).await?;
            Self::ensure_routes_authorized(&dropped_routes, authorized_route_ids)?;
            project_group_members::Entity::delete_many()
                .filter(project_group_members::Column::ProjectId.eq(project_id))
                .filter(project_group_members::Column::GroupId.eq(group_id))
                .exec(&txn)
                .await
                .map_err(ProjectGroupError::db(format!(
                    "removing project {project_id} from project group {group_id}"
                )))?;
            Ok(Some(dropped_routes))
        }
        .await;
        let outcome = Self::end_transaction(
            txn,
            result,
            format!("removal of project {project_id} from project group {group_id}"),
        )
        .await?;
        match outcome {
            Some(dropped_routes) => Ok(dropped_routes),
            None => {
                // Tell a missing group from a project that is not in it.
                self.find_group(group_id).await?;
                Err(ProjectGroupError::NotAMember {
                    group_id,
                    project_id,
                })
            }
        }
    }

    /// Ends `txn` according to `result`: commit on success. On failure the
    /// transaction is rolled back explicitly rather than by dropping it, so
    /// its row locks are released now and the connection does not go back to
    /// the pool "idle in transaction" holding them.
    async fn end_transaction<T>(
        txn: sea_orm::DatabaseTransaction,
        result: Result<T, ProjectGroupError>,
        operation: String,
    ) -> Result<T, ProjectGroupError> {
        match result {
            Ok(value) => {
                txn.commit()
                    .await
                    .map_err(ProjectGroupError::db(format!("committing {operation}")))?;
                Ok(value)
            }
            Err(error) => {
                if let Err(rollback) = txn.rollback().await {
                    tracing::warn!(
                        operation = %operation,
                        error = %rollback,
                        "project groups: rollback failed after {error}"
                    );
                }
                Err(error)
            }
        }
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

    fn route_model(id: i32, group_id: i32) -> project_group_routes::Model {
        let now = Utc::now();
        project_group_routes::Model {
            id,
            group_id,
            source_project_id: 1,
            target_project_id: 2,
            path_prefix: "/api".into(),
            strip_prefix: false,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn unauthorized_routes_fail_the_checked_change() {
        let dropped = [route_model(4, 9), route_model(5, 9)];
        // Callers that do not authorize per route are not checked.
        assert!(ProjectGroupService::ensure_routes_authorized(&dropped, None).is_ok());
        let both: HashSet<i32> = [4, 5].into_iter().collect();
        assert!(ProjectGroupService::ensure_routes_authorized(&dropped, Some(&both)).is_ok());
        // A route created after authorization is reported by id.
        let only_first: HashSet<i32> = [4].into_iter().collect();
        let err = ProjectGroupService::ensure_routes_authorized(&dropped, Some(&only_first))
            .expect_err("route 5 was never authorized");
        assert!(matches!(
            err,
            ProjectGroupError::RouteSetChanged {
                group_id: 9,
                route_id: 5
            }
        ));
        // Nothing to drop is always fine, even with an empty authorized set.
        assert!(ProjectGroupService::ensure_routes_authorized(&[], Some(&HashSet::new())).is_ok());
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

    async fn insert_route(
        conn: &impl ConnectionTrait,
        group_id: i32,
        source: i32,
        target: i32,
        prefix: &str,
    ) -> i32 {
        let now = Utc::now();
        project_group_routes::ActiveModel {
            group_id: Set(group_id),
            source_project_id: Set(source),
            target_project_id: Set(target),
            path_prefix: Set(prefix.into()),
            strip_prefix: Set(false),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(conn)
        .await
        .expect("insert route")
        .id
    }

    async fn route_ids_in(conn: &DatabaseConnection, group_id: i32) -> Vec<i32> {
        project_group_routes::Entity::find()
            .filter(project_group_routes::Column::GroupId.eq(group_id))
            .order_by_asc(project_group_routes::Column::Id)
            .all(conn)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect()
    }

    /// Group `old` holds A(front), B(api) and C, with routes A->B and C->A;
    /// returns `(svc, conn, old_group_id, [a, b, c], route ids [ab, ca])`.
    async fn grouped_with_routes(
        db: &TestDatabase,
        base: i32,
    ) -> (
        ProjectGroupService,
        Arc<DatabaseConnection>,
        i32,
        [i32; 3],
        [i32; 2],
    ) {
        let conn = db.connection_arc();
        let ids = [base, base + 1, base + 2];
        for (i, id) in ids.iter().enumerate() {
            insert_project(conn.as_ref(), *id, &format!("svc{i}")).await;
        }
        let svc = ProjectGroupService::new(conn.clone());
        let old = svc
            .create_group(create_req(&format!("Old {base}"), ids.to_vec()))
            .await
            .unwrap();
        let ab = insert_route(conn.as_ref(), old.group.id, ids[0], ids[1], "/api").await;
        let ca = insert_route(conn.as_ref(), old.group.id, ids[2], ids[0], "/c").await;
        (svc, conn, old.group.id, ids, [ab, ca])
    }

    #[tokio::test]
    async fn routes_dropped_by_leaving_cover_both_ends_and_skip_the_destination() {
        let Some(db) = try_test_db().await else {
            return;
        };
        let (svc, _conn, old, [a, b, c], [ab, ca]) = grouped_with_routes(&db, 9701).await;

        // A leaving takes both the route it sources and the one it targets.
        let dropped = svc
            .routes_dropped_by_leaving_group(&[a], None)
            .await
            .unwrap();
        assert_eq!(
            dropped.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![ab, ca]
        );
        // B only has A->B; a route shared by two leaving projects is listed once.
        let dropped = svc
            .routes_dropped_by_leaving_group(&[b, a], None)
            .await
            .unwrap();
        assert_eq!(
            dropped.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![ab, ca]
        );
        // "Moving" into the group it is already in drops nothing.
        assert!(svc
            .routes_dropped_by_leaving_group(&[a, b, c], Some(old))
            .await
            .unwrap()
            .is_empty());
        // Ungrouped or unknown projects drop nothing.
        assert!(svc
            .routes_dropped_by_leaving_group(&[424242], None)
            .await
            .unwrap()
            .is_empty());
        assert!(svc
            .routes_dropped_by_leaving_group(&[], None)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn removing_with_authorized_routes_reports_what_was_dropped() {
        let Some(db) = try_test_db().await else {
            return;
        };
        let (svc, conn, old, [a, b, _c], [ab, ca]) = grouped_with_routes(&db, 9711).await;

        let authorized: HashSet<i32> = [ab, ca].into_iter().collect();
        let dropped = svc
            .remove_project_checked(old, a, &authorized)
            .await
            .unwrap();
        assert_eq!(
            dropped.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![ab, ca]
        );
        assert!(route_ids_in(conn.as_ref(), old).await.is_empty());
        assert!(svc.group_for_project(a).await.unwrap().is_none());
        // B stays in the group.
        assert_eq!(
            svc.group_for_project(b).await.unwrap().unwrap().group.id,
            old
        );
    }

    #[tokio::test]
    async fn removing_with_an_unauthorized_route_changes_nothing() {
        let Some(db) = try_test_db().await else {
            return;
        };
        let (svc, conn, old, [a, _b, _c], [ab, ca]) = grouped_with_routes(&db, 9721).await;

        // The caller only authorized A->B; C->A appeared (or was not
        // inspected), so the removal must be refused whole.
        let authorized: HashSet<i32> = [ab].into_iter().collect();
        let err = svc
            .remove_project_checked(old, a, &authorized)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ProjectGroupError::RouteSetChanged { route_id, .. } if route_id == ca),
            "got {err:?}"
        );
        assert_eq!(route_ids_in(conn.as_ref(), old).await, vec![ab, ca]);
        assert_eq!(
            svc.group_for_project(a).await.unwrap().unwrap().group.id,
            old
        );

        // Non-members and missing groups keep their errors.
        let err = svc
            .remove_project_checked(old, 424242, &HashSet::new())
            .await
            .unwrap_err();
        assert!(matches!(err, ProjectGroupError::NotAMember { .. }));
    }

    #[tokio::test]
    async fn moving_with_an_unauthorized_route_changes_nothing() {
        let Some(db) = try_test_db().await else {
            return;
        };
        let (svc, conn, old, [a, _b, _c], [ab, ca]) = grouped_with_routes(&db, 9731).await;
        let dest = svc.create_group(create_req("Dest", vec![])).await.unwrap();

        let err = svc
            .add_project_checked(dest.group.id, a, &HashSet::new())
            .await
            .unwrap_err();
        assert!(matches!(err, ProjectGroupError::RouteSetChanged { .. }));
        assert_eq!(route_ids_in(conn.as_ref(), old).await, vec![ab, ca]);
        assert_eq!(
            svc.group_for_project(a).await.unwrap().unwrap().group.id,
            old
        );

        let authorized: HashSet<i32> = [ab, ca].into_iter().collect();
        let placement = svc
            .add_project_checked(dest.group.id, a, &authorized)
            .await
            .unwrap();
        assert_eq!(placement.previous_group_id, Some(old));
        assert_eq!(
            placement
                .dropped_routes
                .iter()
                .map(|r| r.id)
                .collect::<Vec<_>>(),
            vec![ab, ca]
        );
        assert!(route_ids_in(conn.as_ref(), old).await.is_empty());
        assert_eq!(
            svc.group_for_project(a).await.unwrap().unwrap().group.id,
            dest.group.id
        );
    }

    #[tokio::test]
    async fn moving_within_the_same_group_drops_nothing() {
        let Some(db) = try_test_db().await else {
            return;
        };
        let (svc, conn, old, [a, _b, _c], [ab, ca]) = grouped_with_routes(&db, 9741).await;
        let placement = svc
            .add_project_checked(old, a, &HashSet::new())
            .await
            .unwrap();
        assert_eq!(placement.previous_group_id, Some(old));
        assert!(placement.dropped_routes.is_empty());
        assert_eq!(route_ids_in(conn.as_ref(), old).await, vec![ab, ca]);
    }

    #[tokio::test]
    async fn deleting_with_an_unauthorized_route_changes_nothing() {
        let Some(db) = try_test_db().await else {
            return;
        };
        let (svc, conn, old, [a, b, c], [ab, ca]) = grouped_with_routes(&db, 9751).await;

        // The caller only authorized A->B; C->A appeared (or was not
        // inspected), so the deletion must be refused whole.
        let authorized: HashSet<i32> = [ab].into_iter().collect();
        let err = svc
            .delete_group_checked(old, &authorized)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ProjectGroupError::RouteSetChanged { group_id, route_id } if group_id == old && route_id == ca),
            "got {err:?}"
        );
        assert_eq!(route_ids_in(conn.as_ref(), old).await, vec![ab, ca]);
        for project in [a, b, c] {
            assert_eq!(
                svc.group_for_project(project)
                    .await
                    .unwrap()
                    .unwrap()
                    .group
                    .id,
                old
            );
        }
        assert!(svc.get_group(old).await.is_ok());

        // A missing group keeps its error, even with an empty authorized set.
        let err = svc
            .delete_group_checked(999_999, &HashSet::new())
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ProjectGroupError::NotFound { group_id: 999_999 }
        ));
    }

    #[tokio::test]
    async fn deleting_with_authorized_routes_reports_what_was_dropped() {
        let Some(db) = try_test_db().await else {
            return;
        };
        let (svc, conn, old, [a, b, c], [ab, ca]) = grouped_with_routes(&db, 9761).await;
        // A soft-deleted member is not reported as released.
        conn.execute_unprepared(&format!(
            "UPDATE projects SET is_deleted = TRUE WHERE id = {c}"
        ))
        .await
        .unwrap();

        let listed = svc.routes_of_group(old).await.unwrap();
        let authorized: HashSet<i32> = listed.iter().map(|r| r.id).collect();
        assert_eq!(authorized, [ab, ca].into_iter().collect());
        let deletion = svc.delete_group_checked(old, &authorized).await.unwrap();
        assert_eq!(deletion.group.id, old);
        assert_eq!(deletion.released_project_ids, vec![a, b]);
        assert_eq!(
            deletion
                .dropped_routes
                .iter()
                .map(|r| r.id)
                .collect::<Vec<_>>(),
            vec![ab, ca]
        );
        assert!(route_ids_in(conn.as_ref(), old).await.is_empty());
        assert!(matches!(
            svc.get_group(old).await.unwrap_err(),
            ProjectGroupError::NotFound { .. }
        ));
        assert!(svc.group_for_project(a).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn creating_a_group_from_grouped_projects_checks_their_routes() {
        let Some(db) = try_test_db().await else {
            return;
        };
        let (svc, conn, old, [a, _b, _c], [ab, ca]) = grouped_with_routes(&db, 9751).await;

        let err = svc
            .create_group_checked(create_req("Fresh", vec![a]), Some(&HashSet::new()))
            .await
            .unwrap_err();
        assert!(matches!(err, ProjectGroupError::RouteSetChanged { .. }));
        // The group was not created and the routes survive.
        assert!(matches!(
            svc.get_group_by_slug("fresh").await,
            Err(ProjectGroupError::SlugNotFound { .. })
        ));
        assert_eq!(route_ids_in(conn.as_ref(), old).await, vec![ab, ca]);

        let authorized: HashSet<i32> = [ab, ca].into_iter().collect();
        let (created, dropped) = svc
            .create_group_checked(create_req("Fresh", vec![a]), Some(&authorized))
            .await
            .unwrap();
        assert_eq!(ids(&created), vec![a]);
        assert_eq!(
            dropped.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![ab, ca]
        );
        assert!(route_ids_in(conn.as_ref(), old).await.is_empty());
    }

    #[tokio::test]
    async fn group_slugs_resolves_many_in_one_call() {
        let Some(db) = try_test_db().await else {
            return;
        };
        let svc = ProjectGroupService::new(db.connection_arc());
        let a = svc
            .create_group(create_req("Slug A", vec![]))
            .await
            .unwrap();
        let b = svc
            .create_group(create_req("Slug B", vec![]))
            .await
            .unwrap();
        let slugs = svc
            .group_slugs(&[a.group.id, b.group.id, 999_999])
            .await
            .unwrap();
        assert_eq!(slugs.len(), 2);
        assert_eq!(slugs[&a.group.id], "slug-a");
        assert_eq!(slugs[&b.group.id], "slug-b");
        assert!(svc.group_slugs(&[]).await.unwrap().is_empty());
    }

    // -----------------------------------------------------------------
    // Concurrency: the lock protocol. Each test holds an uncommitted
    // transaction ("B") that did what a concurrent request would, starts the
    // service call under test ("A") while it is open, and checks that A
    // waited and then saw B's work.
    // -----------------------------------------------------------------

    /// Runs `fut` on its own task and asserts that it is still waiting after
    /// a pause, i.e. that it is blocked on a lock the open transaction holds.
    async fn spawn_expecting_block<T, F>(what: &str, fut: F) -> tokio::task::JoinHandle<T>
    where
        T: Send + 'static,
        F: std::future::Future<Output = T> + Send + 'static,
    {
        let handle = tokio::spawn(fut);
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert!(
            !handle.is_finished(),
            "{what} must wait for the concurrent transaction instead of reading around it"
        );
        handle
    }

    async fn finished<T>(handle: tokio::task::JoinHandle<T>) -> T {
        match tokio::time::timeout(std::time::Duration::from_secs(20), handle).await {
            Ok(Ok(value)) => value,
            Ok(Err(e)) => panic!("task failed: {e}"),
            Err(_) => panic!("task still blocked 20s after the other transaction committed"),
        }
    }

    /// The reported interleaving: P is in no group when request A looks, so
    /// A reads no membership and authorizes no routes. Meanwhile request B
    /// puts P into a group with Q and routes P -> Q. A must not then move
    /// B's fresh membership and let the database delete the route unnoticed.
    #[tokio::test]
    async fn moving_an_ungrouped_project_waits_for_a_concurrent_placement_and_route() {
        use sea_orm::TransactionTrait;
        let Some(db) = try_test_db().await else {
            return;
        };
        let conn = db.connection_arc();
        let (p, q) = (9801, 9802);
        insert_project(conn.as_ref(), p, "ungrouped").await;
        insert_project(conn.as_ref(), q, "peer").await;
        let svc = Arc::new(ProjectGroupService::new(conn.clone()));
        let shared = svc
            .create_group(create_req("Shared 9801", vec![q]))
            .await
            .unwrap()
            .group
            .id;
        let target = svc
            .create_group(create_req("Target 9801", vec![]))
            .await
            .unwrap()
            .group
            .id;

        // B: places P next to Q and routes to it, not committed yet.
        let b = conn.begin().await.unwrap();
        ProjectGroupService::lock_projects(&b, &[p]).await.unwrap();
        ProjectGroupService::place_membership(&b, shared, p, None, None)
            .await
            .unwrap();
        let route = insert_route(&b, shared, p, q, "/api").await;

        // A authorized nothing: when it looked, P had no routes.
        let a = {
            let svc = svc.clone();
            spawn_expecting_block("moving an ungrouped project", async move {
                svc.add_project_checked(target, p, &HashSet::new()).await
            })
            .await
        };
        b.commit().await.unwrap();

        let err = finished(a).await.unwrap_err();
        assert!(
            matches!(err, ProjectGroupError::RouteSetChanged { group_id, route_id } if group_id == shared && route_id == route),
            "got {err:?}"
        );
        // B's placement and route are intact, and nothing moved.
        assert_eq!(route_ids_in(conn.as_ref(), shared).await, vec![route]);
        assert_eq!(
            svc.group_for_project(p).await.unwrap().unwrap().group.id,
            shared
        );
    }

    /// Same interleaving through group creation: a new group that lists P.
    #[tokio::test]
    async fn creating_a_group_with_an_ungrouped_project_waits_for_a_concurrent_route() {
        use sea_orm::TransactionTrait;
        let Some(db) = try_test_db().await else {
            return;
        };
        let conn = db.connection_arc();
        let (p, q) = (9811, 9812);
        insert_project(conn.as_ref(), p, "ungrouped").await;
        insert_project(conn.as_ref(), q, "peer").await;
        let svc = Arc::new(ProjectGroupService::new(conn.clone()));
        let shared = svc
            .create_group(create_req("Shared 9811", vec![q]))
            .await
            .unwrap()
            .group
            .id;

        let b = conn.begin().await.unwrap();
        ProjectGroupService::lock_projects(&b, &[p]).await.unwrap();
        ProjectGroupService::place_membership(&b, shared, p, None, None)
            .await
            .unwrap();
        let route = insert_route(&b, shared, p, q, "/api").await;

        let a = {
            let svc = svc.clone();
            spawn_expecting_block("creating a group from an ungrouped project", async move {
                svc.create_group_checked(create_req("Fresh 9811", vec![p]), Some(&HashSet::new()))
                    .await
                    .map(|(group, _)| group.group.id)
            })
            .await
        };
        b.commit().await.unwrap();

        let err = finished(a).await.unwrap_err();
        assert!(
            matches!(err, ProjectGroupError::RouteSetChanged { route_id, .. } if route_id == route),
            "got {err:?}"
        );
        assert_eq!(route_ids_in(conn.as_ref(), shared).await, vec![route]);
        assert_eq!(
            svc.group_for_project(p).await.unwrap().unwrap().group.id,
            shared
        );
        // The refused request left no group behind.
        assert!(matches!(
            svc.get_group_by_slug("fresh-9811").await.unwrap_err(),
            ProjectGroupError::SlugNotFound { .. }
        ));
    }

    /// A route insert that is still uncommitted when a membership change or
    /// the group's deletion starts: the change waits, re-reads the routes and
    /// refuses to drop the one nobody authorized.
    #[tokio::test]
    async fn membership_changes_wait_for_a_concurrent_route_insert() {
        use sea_orm::TransactionTrait;
        let Some(db) = try_test_db().await else {
            return;
        };
        let conn = db.connection_arc();
        let svc = Arc::new(ProjectGroupService::new(conn.clone()));

        for (i, change) in ["remove", "move", "delete"].into_iter().enumerate() {
            let base = 9821 + (i as i32) * 10;
            let (p, q) = (base, base + 1);
            insert_project(conn.as_ref(), p, "front").await;
            insert_project(conn.as_ref(), q, "api").await;
            let group = svc
                .create_group(create_req(&format!("Race {change}"), vec![p, q]))
                .await
                .unwrap()
                .group
                .id;
            let other = svc
                .create_group(create_req(&format!("Other {change}"), vec![]))
                .await
                .unwrap()
                .group
                .id;

            // B inserts P -> Q and has not committed.
            let b = conn.begin().await.unwrap();
            let route = insert_route(&b, group, p, q, "/api").await;

            let a = {
                let svc = svc.clone();
                let nothing = HashSet::new();
                spawn_expecting_block(change, async move {
                    match change {
                        "remove" => svc
                            .remove_project_checked(group, p, &nothing)
                            .await
                            .map(|_| ()),
                        "move" => svc
                            .add_project_checked(other, p, &nothing)
                            .await
                            .map(|_| ()),
                        _ => svc.delete_group_checked(group, &nothing).await.map(|_| ()),
                    }
                })
                .await
            };
            b.commit().await.unwrap();

            let err = finished(a).await.unwrap_err();
            assert!(
                matches!(err, ProjectGroupError::RouteSetChanged { route_id, .. } if route_id == route),
                "{change}: got {err:?}"
            );
            assert_eq!(
                route_ids_in(conn.as_ref(), group).await,
                vec![route],
                "{change}: the route must survive"
            );
            assert_eq!(
                svc.group_for_project(p).await.unwrap().unwrap().group.id,
                group,
                "{change}: the project must not move"
            );
        }
    }

    /// Defence in depth: the write itself refuses to take a project out of a
    /// group other than the one the caller read.
    #[tokio::test]
    async fn placement_fails_when_the_prior_membership_is_not_the_expected_one() {
        let Some(db) = try_test_db().await else {
            return;
        };
        let conn = db.connection_arc();
        let (p, q) = (9851, 9852);
        insert_project(conn.as_ref(), p, "mover").await;
        insert_project(conn.as_ref(), q, "stayer").await;
        let svc = ProjectGroupService::new(conn.clone());
        let home = svc
            .create_group(create_req("Home 9851", vec![p, q]))
            .await
            .unwrap()
            .group
            .id;
        let elsewhere = svc
            .create_group(create_req("Elsewhere 9851", vec![]))
            .await
            .unwrap()
            .group
            .id;
        let route = insert_route(conn.as_ref(), home, p, q, "/api").await;

        // The caller believes P is ungrouped, or in another group.
        for expected in [None, Some(elsewhere)] {
            let err =
                ProjectGroupService::place_membership(conn.as_ref(), elsewhere, p, None, expected)
                    .await
                    .unwrap_err();
            assert!(
                matches!(err, ProjectGroupError::MembershipChanged { group_id, project_id } if group_id == elsewhere && project_id == p),
                "expected {expected:?}: got {err:?}"
            );
            assert_eq!(
                svc.group_for_project(p).await.unwrap().unwrap().group.id,
                home
            );
            assert_eq!(route_ids_in(conn.as_ref(), home).await, vec![route]);
        }

        // The right expectation moves it, and the route goes with it.
        ProjectGroupService::place_membership(conn.as_ref(), elsewhere, p, None, Some(home))
            .await
            .unwrap();
        assert_eq!(
            svc.group_for_project(p).await.unwrap().unwrap().group.id,
            elsewhere
        );
        assert!(route_ids_in(conn.as_ref(), home).await.is_empty());

        // A project with no membership is placed only when none is expected.
        let r = 9853;
        insert_project(conn.as_ref(), r, "newcomer").await;
        ProjectGroupService::place_membership(conn.as_ref(), home, r, Some(0), None)
            .await
            .unwrap();
        assert_eq!(
            svc.group_for_project(r).await.unwrap().unwrap().group.id,
            home
        );
    }

    /// Competing placements of one project all take the same locks in the
    /// same order: they serialize, none deadlocks, and one membership wins.
    #[tokio::test]
    async fn concurrent_placements_of_one_project_serialize() {
        let Some(db) = try_test_db().await else {
            return;
        };
        let conn = db.connection_arc();
        let (p, q) = (9861, 9862);
        insert_project(conn.as_ref(), p, "contended").await;
        insert_project(conn.as_ref(), q, "peer").await;
        let svc = Arc::new(ProjectGroupService::new(conn.clone()));
        let g1 = svc
            .create_group(create_req("Left 9861", vec![q]))
            .await
            .unwrap()
            .group
            .id;
        let g2 = svc
            .create_group(create_req("Right 9861", vec![]))
            .await
            .unwrap()
            .group
            .id;

        let handles: Vec<_> = (0..8)
            .map(|i| {
                let svc = svc.clone();
                let group = if i % 2 == 0 { g1 } else { g2 };
                tokio::spawn(async move { svc.add_project(group, p).await })
            })
            .collect();
        for handle in handles {
            finished(handle).await.unwrap();
        }
        let group = svc.group_for_project(p).await.unwrap().unwrap().group.id;
        assert!(group == g1 || group == g2);
    }
}
