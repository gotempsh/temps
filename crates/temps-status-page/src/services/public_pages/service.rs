// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Management of publishable status pages (#1412).
//!
//! Every write commits to the database first and then updates the public
//! snapshot cache. Taking a page down (unpublish, delete, URL rotation, a
//! narrowed monitor selection) fails closed: if the new snapshot cannot be
//! built, the page is removed from the cache rather than left serving the old
//! content.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use chrono::Utc;
use rand::TryCryptoRng;
use sea_orm::sea_query::{Expr, Query, SelectStatement};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection,
    EntityTrait, FromQueryResult, JoinType, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect,
    RelationTrait, Set, Statement, TransactionTrait, Value,
};
use temps_config::ConfigService;
use temps_entities::{projects, status_monitors, status_page_monitors, status_pages};
use tracing::{error, info, warn};

use super::snapshot::StatusSnapshotCache;
use super::types::{
    CreateStatusPageRequest, PublicUrlStatus, StatusPageAppearance, StatusPageAppearanceInput,
    StatusPageListResponse, StatusPageMonitorInput, StatusPageMonitorResponse,
    StatusPagePreviewResponse, StatusPageProject, StatusPageResponse, UpdateStatusPageRequest,
    MAX_DESCRIPTION_CHARS, MAX_DISPLAY_NAME_CHARS, MAX_GROUP_NAME_CHARS,
    MAX_MONITORS_PER_STATUS_PAGE, MAX_PAGE_URL_CHARS, MAX_STATUS_PAGES, MAX_TITLE_CHARS,
};
use crate::services::StatusPageError;

/// Random bytes in a slug. 128 bits: infeasible to guess or enumerate.
pub const SLUG_BYTES: usize = 16;
/// Length of a slug: lowercase hex of [`SLUG_BYTES`].
pub const SLUG_LEN: usize = SLUG_BYTES * 2;

/// Whether `candidate` has the shape of a slug this service generates. Lets the
/// public handler reject garbage without a lookup.
pub fn is_well_formed_slug(candidate: &str) -> bool {
    candidate.len() == SLUG_LEN
        && candidate
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn generate_slug_with<R: TryCryptoRng>(rng: &mut R) -> Result<String, StatusPageError> {
    let mut bytes = [0u8; SLUG_BYTES];
    rng.try_fill_bytes(&mut bytes)
        .map_err(|e| StatusPageError::SlugGeneration {
            reason: e.to_string(),
        })?;
    Ok(hex::encode(bytes))
}

/// Generate a slug from the operating system's CSPRNG.
pub fn generate_slug() -> Result<String, StatusPageError> {
    generate_slug_with(&mut rand::rngs::SysRng)
}

fn has_disallowed_control(text: &str, allow_newlines: bool) -> bool {
    text.chars()
        .any(|c| c.is_control() && !(allow_newlines && (c == '\n' || c == '\r' || c == '\t')))
}

fn normalize_title(title: &str) -> Result<String, StatusPageError> {
    let title = title.trim();
    if title.is_empty() {
        return Err(StatusPageError::Validation(
            "Status page title cannot be empty".to_string(),
        ));
    }
    let length = title.chars().count();
    if length > MAX_TITLE_CHARS {
        return Err(StatusPageError::Validation(format!(
            "Status page title is {length} characters; the limit is {MAX_TITLE_CHARS}"
        )));
    }
    if has_disallowed_control(title, false) {
        return Err(StatusPageError::Validation(
            "Status page title cannot contain control characters".to_string(),
        ));
    }
    Ok(title.to_string())
}

fn normalize_description(description: Option<&str>) -> Result<Option<String>, StatusPageError> {
    let Some(description) = description.map(str::trim).filter(|d| !d.is_empty()) else {
        return Ok(None);
    };
    let length = description.chars().count();
    if length > MAX_DESCRIPTION_CHARS {
        return Err(StatusPageError::Validation(format!(
            "Status page description is {length} characters; the limit is {MAX_DESCRIPTION_CHARS}"
        )));
    }
    if has_disallowed_control(description, true) {
        return Err(StatusPageError::Validation(
            "Status page description cannot contain control characters".to_string(),
        ));
    }
    Ok(Some(description.to_string()))
}

/// `#rrggbb` (any case, `#` optional) to lowercase `#rrggbb`; empty clears.
pub(crate) fn normalize_accent_color(value: &str) -> Result<Option<String>, StatusPageError> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let hex = value.strip_prefix('#').unwrap_or(value);
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(StatusPageError::Validation(format!(
            "Accent color '{value}' is not a hex color such as #4f46e5"
        )));
    }
    Ok(Some(format!("#{}", hex.to_ascii_lowercase())))
}

/// An absolute `https://` URL without credentials; empty clears. Logos are
/// loaded by every visitor's browser, so plain `http://` (mixed content) and
/// other schemes are refused.
pub(crate) fn normalize_page_url(
    field: &str,
    value: &str,
) -> Result<Option<String>, StatusPageError> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let length = value.chars().count();
    if length > MAX_PAGE_URL_CHARS {
        return Err(StatusPageError::Validation(format!(
            "{field} is {length} characters; the limit is {MAX_PAGE_URL_CHARS}"
        )));
    }
    let parsed = url::Url::parse(value).map_err(|e| {
        StatusPageError::Validation(format!("{field} '{value}' is not a valid URL: {e}"))
    })?;
    if parsed.scheme() != "https" {
        return Err(StatusPageError::Validation(format!(
            "{field} must start with https://, got '{value}'"
        )));
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err(StatusPageError::Validation(format!(
            "{field} '{value}' has no host"
        )));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(StatusPageError::Validation(format!(
            "{field} cannot contain a username or password"
        )));
    }
    Ok(Some(parsed.to_string()))
}

/// Apply `input` on top of `current`, validating every field that is present.
pub(crate) fn merge_appearance(
    mut current: StatusPageAppearance,
    input: &StatusPageAppearanceInput,
) -> Result<StatusPageAppearance, StatusPageError> {
    if let Some(theme) = input.theme {
        current.theme = theme;
    }
    if let Some(color) = &input.accent_color {
        current.accent_color = normalize_accent_color(color)?;
    }
    if let Some(logo) = &input.logo_url {
        current.logo_url = normalize_page_url("Logo URL", logo)?;
    }
    if let Some(website) = &input.website_url {
        current.website_url = normalize_page_url("Website URL", website)?;
    }
    if let Some(show) = input.show_uptime_percentage {
        current.show_uptime_percentage = show;
    }
    if let Some(show) = input.show_response_times {
        current.show_response_times = show;
    }
    Ok(current)
}

fn set_appearance(active: &mut status_pages::ActiveModel, appearance: StatusPageAppearance) {
    active.theme = Set(appearance.theme.as_str().to_string());
    active.accent_color = Set(appearance.accent_color);
    active.logo_url = Set(appearance.logo_url);
    active.website_url = Set(appearance.website_url);
    active.show_uptime_percentage = Set(appearance.show_uptime_percentage);
    active.show_response_times = Set(appearance.show_response_times);
}

/// One validated component of a page selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SelectedComponent {
    pub monitor_id: i32,
    pub display_name: String,
    pub group_name: Option<String>,
}

/// Validate the shape of a monitor selection (no database access). A page
/// always shows at least one component: its projects are what it is
/// authorized against.
fn normalize_selection(
    monitors: &[StatusPageMonitorInput],
) -> Result<Vec<SelectedComponent>, StatusPageError> {
    if monitors.is_empty() {
        return Err(StatusPageError::NoComponents);
    }
    if monitors.len() > MAX_MONITORS_PER_STATUS_PAGE {
        return Err(StatusPageError::Validation(format!(
            "A status page can show at most {MAX_MONITORS_PER_STATUS_PAGE} monitors; {} were given",
            monitors.len()
        )));
    }
    let mut seen = HashSet::new();
    let mut normalized = Vec::with_capacity(monitors.len());
    for input in monitors {
        if !seen.insert(input.monitor_id) {
            return Err(StatusPageError::Validation(format!(
                "Monitor {} is selected more than once",
                input.monitor_id
            )));
        }
        let name = input.display_name.trim();
        if name.is_empty() {
            return Err(StatusPageError::Validation(format!(
                "Monitor {} needs a public display name",
                input.monitor_id
            )));
        }
        let length = name.chars().count();
        if length > MAX_DISPLAY_NAME_CHARS {
            return Err(StatusPageError::Validation(format!(
                "Display name for monitor {} is {length} characters; the limit is {MAX_DISPLAY_NAME_CHARS}",
                input.monitor_id
            )));
        }
        if has_disallowed_control(name, false) {
            return Err(StatusPageError::Validation(format!(
                "Display name for monitor {} cannot contain control characters",
                input.monitor_id
            )));
        }
        let group_name = normalize_group_name(input.monitor_id, input.group_name.as_deref())?;
        normalized.push(SelectedComponent {
            monitor_id: input.monitor_id,
            display_name: name.to_string(),
            group_name,
        });
    }
    Ok(normalized)
}

/// A group name is trimmed; blank means "no group".
fn normalize_group_name(
    monitor_id: i32,
    group_name: Option<&str>,
) -> Result<Option<String>, StatusPageError> {
    let Some(name) = group_name.map(str::trim).filter(|n| !n.is_empty()) else {
        return Ok(None);
    };
    let length = name.chars().count();
    if length > MAX_GROUP_NAME_CHARS {
        return Err(StatusPageError::Validation(format!(
            "Group name for monitor {monitor_id} is {length} characters; the limit is {MAX_GROUP_NAME_CHARS}"
        )));
    }
    if has_disallowed_control(name, false) {
        return Err(StatusPageError::Validation(format!(
            "Group name for monitor {monitor_id} cannot contain control characters"
        )));
    }
    Ok(Some(name.to_string()))
}

/// Ids of the pages with a component whose monitor matches `condition` (an
/// expression over `status_monitors`). Like everywhere else, a
/// component of a project being deleted no longer counts: it is about to be
/// removed with the project, and the public page already leaves it out.
fn pages_with_components_where(condition: sea_orm::sea_query::SimpleExpr) -> SelectStatement {
    Query::select()
        .column((
            status_page_monitors::Entity,
            status_page_monitors::Column::StatusPageId,
        ))
        .from(status_page_monitors::Entity)
        .inner_join(
            status_monitors::Entity,
            Expr::col((status_monitors::Entity, status_monitors::Column::Id)).equals((
                status_page_monitors::Entity,
                status_page_monitors::Column::MonitorId,
            )),
        )
        .inner_join(
            projects::Entity,
            Expr::col((projects::Entity, projects::Column::Id))
                .equals((status_monitors::Entity, status_monitors::Column::ProjectId)),
        )
        .and_where(Expr::col((projects::Entity, projects::Column::IsDeleted)).eq(false))
        .and_where(condition)
        .to_owned()
}

/// Which pages a caller may list. Built by the handler from the caller's
/// identity; the service only applies it.
#[derive(Debug, Clone, Default)]
pub struct StatusPageListFilter {
    /// Only pages that show a component of this project.
    pub project_id: Option<i32>,
    /// Leave out every page with a component in one of these projects (the
    /// caller's hidden projects).
    pub hidden_project_ids: Vec<i32>,
    /// Only pages whose every component is in this project (a token bound to
    /// one project).
    pub confined_to_project: Option<i32>,
    /// Include pages without components. They are authorized by the
    /// instance-wide permission only, so only admins see them.
    pub include_pages_without_components: bool,
}

#[derive(Debug, FromQueryResult)]
struct SelectionWithName {
    status_page_id: i32,
    monitor_id: i32,
    display_name: String,
    group_name: Option<String>,
    position: i32,
    monitor_name: String,
    project_id: i32,
    project_name: String,
    project_slug: String,
    environment_id: Option<i32>,
    environment_name: Option<String>,
}

pub struct PublicStatusPageService {
    db: Arc<DatabaseConnection>,
    config_service: Arc<ConfigService>,
    cache: Arc<StatusSnapshotCache>,
}

impl PublicStatusPageService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        config_service: Arc<ConfigService>,
        cache: Arc<StatusSnapshotCache>,
    ) -> Self {
        Self {
            db,
            config_service,
            cache,
        }
    }

    /// The projects a page draws components from, sorted and distinct, for
    /// authorization on by-id routes. Empty for a page without components.
    ///
    /// Projects being deleted are left out. Deletion cannot be undone, the
    /// public page already ignores their components, and counting them would
    /// lock every non-admin owner out of the page (including the edit that
    /// removes the dead component) until the rows are gone.
    pub async fn page_project_ids(&self, status_page_id: i32) -> Result<Vec<i32>, StatusPageError> {
        let exists = status_pages::Entity::find_by_id(status_page_id)
            .select_only()
            .column(status_pages::Column::Id)
            .into_tuple::<i32>()
            .one(self.db.as_ref())
            .await?;
        if exists.is_none() {
            return Err(StatusPageError::StatusPageNotFound { status_page_id });
        }
        let mut ids: Vec<i32> = status_page_monitors::Entity::find()
            .select_only()
            .column(status_monitors::Column::ProjectId)
            .inner_join(status_monitors::Entity)
            .join(
                JoinType::InnerJoin,
                status_monitors::Relation::Project.def(),
            )
            .filter(projects::Column::IsDeleted.eq(false))
            .filter(status_page_monitors::Column::StatusPageId.eq(status_page_id))
            .distinct()
            .into_tuple::<i32>()
            .all(self.db.as_ref())
            .await?;
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }

    /// Every project with a component on some status page, sorted and
    /// distinct. Bounded by the number of projects; used to work out which
    /// of them a caller may not read status pages of before listing.
    pub async fn component_project_ids(&self) -> Result<Vec<i32>, StatusPageError> {
        let mut ids: Vec<i32> = status_page_monitors::Entity::find()
            .select_only()
            .column(status_monitors::Column::ProjectId)
            .inner_join(status_monitors::Entity)
            .join(
                JoinType::InnerJoin,
                status_monitors::Relation::Project.def(),
            )
            .filter(projects::Column::IsDeleted.eq(false))
            .distinct()
            .into_tuple::<i32>()
            .all(self.db.as_ref())
            .await?;
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }

    /// The projects of the requested monitors, sorted and distinct. Fails
    /// with every id that does not name a monitor of a live project, so the
    /// caller is authorized against exactly what the page would show.
    ///
    /// Runs before authorization, so the size limit is enforced here first:
    /// an oversized selection is a 400, never an unbounded `IN` list.
    pub async fn monitor_project_ids(
        &self,
        monitors: &[StatusPageMonitorInput],
    ) -> Result<Vec<i32>, StatusPageError> {
        if monitors.len() > MAX_MONITORS_PER_STATUS_PAGE {
            return Err(StatusPageError::Validation(format!(
                "A status page can show at most {MAX_MONITORS_PER_STATUS_PAGE} monitors; {} were given",
                monitors.len()
            )));
        }
        self.projects_of_monitors(monitors.iter().map(|m| m.monitor_id).collect())
            .await
    }

    async fn projects_of_monitors(&self, requested: Vec<i32>) -> Result<Vec<i32>, StatusPageError> {
        if requested.is_empty() {
            return Ok(Vec::new());
        }
        let found: Vec<(i32, i32)> = status_monitors::Entity::find()
            .select_only()
            .column(status_monitors::Column::Id)
            .column(status_monitors::Column::ProjectId)
            .inner_join(projects::Entity)
            .filter(projects::Column::IsDeleted.eq(false))
            .filter(status_monitors::Column::Id.is_in(requested.clone()))
            .into_tuple::<(i32, i32)>()
            .all(self.db.as_ref())
            .await?;
        let found_ids: HashSet<i32> = found.iter().map(|(id, _)| *id).collect();
        let mut missing: Vec<i32> = requested
            .into_iter()
            .filter(|id| !found_ids.contains(id))
            .collect();
        if !missing.is_empty() {
            missing.sort_unstable();
            missing.dedup();
            return Err(StatusPageError::MonitorsNotFound {
                monitor_ids: missing,
            });
        }
        let mut project_ids: Vec<i32> = found.into_iter().map(|(_, project)| project).collect();
        project_ids.sort_unstable();
        project_ids.dedup();
        Ok(project_ids)
    }

    pub async fn list(
        &self,
        filter: &StatusPageListFilter,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<StatusPageListResponse, StatusPageError> {
        let page = page.unwrap_or(1).max(1);
        let page_size = page_size.unwrap_or(20).clamp(1, 100);
        let project_col = Expr::col((status_monitors::Entity, status_monitors::Column::ProjectId));
        let mut query = status_pages::Entity::find();
        if let Some(project_id) = filter.project_id {
            query = query.filter(status_pages::Column::Id.in_subquery(
                pages_with_components_where(project_col.clone().eq(project_id)),
            ));
        }
        if !filter.hidden_project_ids.is_empty() {
            query = query.filter(
                status_pages::Column::Id.not_in_subquery(pages_with_components_where(
                    project_col
                        .clone()
                        .is_in(filter.hidden_project_ids.iter().copied()),
                )),
            );
        }
        if let Some(project_id) = filter.confined_to_project {
            query = query
                .filter(
                    status_pages::Column::Id.in_subquery(pages_with_components_where(
                        project_col.clone().eq(project_id),
                    )),
                )
                .filter(
                    status_pages::Column::Id.not_in_subquery(pages_with_components_where(
                        project_col.clone().ne(project_id),
                    )),
                );
        }
        if !filter.include_pages_without_components {
            query = query.filter(
                status_pages::Column::Id
                    .in_subquery(pages_with_components_where(Expr::val(true).into())),
            );
        }
        let paginator = query
            .order_by_desc(status_pages::Column::CreatedAt)
            .order_by_desc(status_pages::Column::Id)
            .paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let models = paginator.fetch_page(page - 1).await?;
        let items = self.to_responses(models).await?;
        Ok(StatusPageListResponse {
            items,
            total,
            page,
            page_size,
        })
    }

    pub async fn get(&self, status_page_id: i32) -> Result<StatusPageResponse, StatusPageError> {
        let model = self.find(status_page_id).await?;
        self.to_response(model).await
    }

    /// What the page would show visitors right now, whether or not it is
    /// published. Built on demand for project members; never cached and
    /// never served publicly.
    pub async fn preview(
        &self,
        status_page_id: i32,
    ) -> Result<StatusPagePreviewResponse, StatusPageError> {
        let model = self.find(status_page_id).await?;
        let page = self.cache.preview(&model).await?;
        let html = super::render::render_public_page_html(&page);
        Ok(StatusPagePreviewResponse { page, html })
    }

    pub async fn create(
        &self,
        request: CreateStatusPageRequest,
    ) -> Result<StatusPageResponse, StatusPageError> {
        let title = normalize_title(&request.title)?;
        let description = normalize_description(request.description.as_deref())?;
        let selection = normalize_selection(&request.monitors)?;
        let appearance = match &request.appearance {
            Some(input) => merge_appearance(StatusPageAppearance::default(), input)?,
            None => StatusPageAppearance::default(),
        };
        self.ensure_monitors_exist(&selection).await?;

        let slug = generate_slug()?;
        let txn = self.db.begin().await?;
        let result = async {
            // Serialize creates so concurrent ones cannot both pass the
            // instance-wide limit.
            txn.execute(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT pg_advisory_xact_lock(hashtextextended('status-pages:create', 0))",
            ))
            .await?;
            let existing = status_pages::Entity::find().count(&txn).await?;
            if existing >= MAX_STATUS_PAGES {
                return Err(StatusPageError::StatusPageLimitReached {
                    limit: MAX_STATUS_PAGES,
                });
            }
            let mut active = status_pages::ActiveModel {
                slug: Set(slug),
                title: Set(title),
                description: Set(description),
                is_published: Set(false),
                published_at: Set(None),
                ..Default::default()
            };
            set_appearance(&mut active, appearance);
            let model = active.insert(&txn).await?;
            Self::write_selection(&txn, model.id, &selection).await?;
            Ok(model)
        }
        .await;
        let model = Self::finish(txn, result).await?;

        info!(
            status_page_id = model.id,
            monitors = selection.len(),
            "created status page"
        );
        self.to_response(model).await
    }

    pub async fn update(
        &self,
        status_page_id: i32,
        request: UpdateStatusPageRequest,
    ) -> Result<StatusPageResponse, StatusPageError> {
        let title = request.title.as_deref().map(normalize_title).transpose()?;
        let description = request
            .description
            .as_deref()
            .map(|d| normalize_description(Some(d)))
            .transpose()?;
        let selection = request
            .monitors
            .as_deref()
            .map(normalize_selection)
            .transpose()?;
        if let Some(selection) = &selection {
            self.ensure_monitors_exist(selection).await?;
        }

        // Read and write the page under a row lock so a concurrent publish
        // cannot interleave with a selection change.
        let txn = self.db.begin().await?;
        let result = async {
            let model = Self::find_locked(&txn, status_page_id).await?;
            let appearance = request
                .appearance
                .as_ref()
                .map(|input| merge_appearance(StatusPageAppearance::from_model(&model), input))
                .transpose()?;
            let mut active: status_pages::ActiveModel = model.into();
            if let Some(appearance) = appearance {
                set_appearance(&mut active, appearance);
            }
            if let Some(title) = title {
                active.title = Set(title);
            }
            if let Some(description) = description {
                active.description = Set(description);
            }
            let model = active.update(&txn).await?;
            if let Some(selection) = &selection {
                Self::write_selection(&txn, status_page_id, selection).await?;
            }
            Ok(model)
        }
        .await;
        let model = Self::finish(txn, result).await?;

        // Always rebuild: cheap for one page, and correct even if the page was
        // published concurrently (a draft simply stays out of the cache).
        self.refresh_or_take_down(status_page_id).await;
        self.to_response(model).await
    }

    pub async fn publish(
        &self,
        status_page_id: i32,
    ) -> Result<StatusPageResponse, StatusPageError> {
        // The row lock serializes this with `update`, so the selection that
        // was checked is the one that goes live.
        let txn = self.db.begin().await?;
        let result = async {
            let model = Self::find_locked(&txn, status_page_id).await?;
            // Only live components: a page left with nothing but a deleted
            // project's would be "published" yet never served.
            let selected = status_page_monitors::Entity::find()
                .inner_join(status_monitors::Entity)
                .join(
                    JoinType::InnerJoin,
                    status_monitors::Relation::Project.def(),
                )
                .filter(projects::Column::IsDeleted.eq(false))
                .filter(status_page_monitors::Column::StatusPageId.eq(status_page_id))
                .count(&txn)
                .await?;
            if selected == 0 {
                return Err(StatusPageError::PublishWithoutMonitors { status_page_id });
            }
            if model.is_published {
                return Ok(model);
            }
            let mut active: status_pages::ActiveModel = model.into();
            active.is_published = Set(true);
            active.published_at = Set(Some(Utc::now()));
            Ok(active.update(&txn).await?)
        }
        .await;
        let model = Self::finish(txn, result).await?;
        self.refresh_or_take_down(status_page_id).await;
        info!(status_page_id, "published status page");
        self.to_response(model).await
    }

    pub async fn unpublish(
        &self,
        status_page_id: i32,
    ) -> Result<StatusPageResponse, StatusPageError> {
        // Same row lock as `publish`: a concurrent publish either commits
        // before this read (and is undone here) or waits and runs after.
        let txn = self.db.begin().await?;
        let result = async {
            let model = Self::find_locked(&txn, status_page_id).await?;
            if !model.is_published {
                return Ok(model);
            }
            let mut active: status_pages::ActiveModel = model.into();
            active.is_published = Set(false);
            active.published_at = Set(None);
            let model = active.update(&txn).await?;
            // An unpublished page's custom hostnames keep answering 404, and
            // their certificates are released so they are neither served nor
            // renewed. Republishing requires verifying them again.
            super::domains::release_page_certificates(&txn, status_page_id).await?;
            Ok(model)
        }
        .await;
        let model = Self::finish(txn, result).await?;
        self.cache.remove_page(status_page_id).await;
        info!(status_page_id, "unpublished status page");
        self.to_response(model).await
    }

    /// Replace the public URL with a fresh one. The old URL stops working
    /// immediately.
    pub async fn rotate_slug(
        &self,
        status_page_id: i32,
    ) -> Result<StatusPageResponse, StatusPageError> {
        let slug = generate_slug()?;
        let txn = self.db.begin().await?;
        let result = async {
            let model = Self::find_locked(&txn, status_page_id).await?;
            let mut active: status_pages::ActiveModel = model.into();
            active.slug = Set(slug);
            Ok(active.update(&txn).await?)
        }
        .await;
        let model = Self::finish(txn, result).await?;
        // Drop the old slug so it can never be served again, then rebuild
        // from the database: publishes under the new slug only if the page is
        // published *now*, whatever it was when this call started.
        self.cache.remove_page(status_page_id).await;
        self.refresh_or_take_down(status_page_id).await;
        info!(status_page_id, "rotated status page URL");
        self.to_response(model).await
    }

    pub async fn delete(&self, status_page_id: i32) -> Result<(), StatusPageError> {
        let result = status_pages::Entity::delete_by_id(status_page_id)
            .exec(self.db.as_ref())
            .await?;
        if result.rows_affected == 0 {
            return Err(StatusPageError::StatusPageNotFound { status_page_id });
        }
        self.cache.remove_page(status_page_id).await;
        info!(status_page_id, "deleted status page");
        Ok(())
    }

    /// Rebuild every published page showing a component of a project, e.g.
    /// after an incident update was made public or private. Fails closed: on
    /// error those
    /// pages stop being served until the background refresh succeeds.
    pub async fn refresh_project_pages(&self, project_id: i32) {
        if let Err(e) = self.cache.refresh_project(project_id).await {
            // The change may have made something private; never keep serving
            // the old content. Pages come back on the next successful refresh.
            error!(
                project_id,
                error = %e,
                "failed to refresh public status pages after an incident change; taking the project's pages offline until the next successful refresh"
            );
            self.cache.remove_project(project_id).await;
        }
    }

    /// Stop serving every page showing a component of a project.
    pub async fn remove_project_pages(&self, project_id: i32) {
        self.cache.remove_project(project_id).await;
    }

    /// Published pages that show a component of `project_id` and nothing
    /// from any other live project.
    async fn pages_left_without_components(
        &self,
        project_id: i32,
    ) -> Result<Vec<i32>, StatusPageError> {
        #[derive(Debug, FromQueryResult)]
        struct PageId {
            id: i32,
        }
        let rows = PageId::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            SELECT sp.id
            FROM status_pages sp
            WHERE sp.is_published
              AND EXISTS (
                  SELECT 1 FROM status_page_monitors spm
                  JOIN status_monitors sm ON sm.id = spm.monitor_id
                  WHERE spm.status_page_id = sp.id AND sm.project_id = $1)
              AND NOT EXISTS (
                  SELECT 1 FROM status_page_monitors spm
                  JOIN status_monitors sm ON sm.id = spm.monitor_id
                  JOIN projects p ON p.id = sm.project_id
                  WHERE spm.status_page_id = sp.id
                    AND sm.project_id <> $1
                    AND NOT p.is_deleted)
            ORDER BY sp.id
            "#,
            [project_id.into()],
        ))
        .all(self.db.as_ref())
        .await?;
        Ok(rows.into_iter().map(|r| r.id).collect())
    }

    async fn refresh_or_take_down(&self, status_page_id: i32) {
        if let Err(e) = self.cache.refresh_page(status_page_id).await {
            error!(
                status_page_id,
                error = %e,
                "failed to rebuild public status page snapshot; taking the page offline until the next successful refresh"
            );
            self.cache.remove_page(status_page_id).await;
        }
    }

    /// Commit on success; on failure roll back *now* rather than leaving the
    /// connection idle in a transaction (and its row locks held) until the
    /// pool happens to reuse it.
    async fn finish<T>(
        txn: sea_orm::DatabaseTransaction,
        result: Result<T, StatusPageError>,
    ) -> Result<T, StatusPageError> {
        match result {
            Ok(value) => {
                txn.commit().await?;
                Ok(value)
            }
            Err(error) => {
                if let Err(rollback) = txn.rollback().await {
                    warn!(
                        error = %rollback,
                        original_error = %error,
                        "failed to roll back status page transaction"
                    );
                }
                Err(error)
            }
        }
    }

    async fn find_locked(
        txn: &sea_orm::DatabaseTransaction,
        status_page_id: i32,
    ) -> Result<status_pages::Model, StatusPageError> {
        status_pages::Entity::find_by_id(status_page_id)
            .lock_exclusive()
            .one(txn)
            .await?
            .ok_or(StatusPageError::StatusPageNotFound { status_page_id })
    }

    async fn find(&self, status_page_id: i32) -> Result<status_pages::Model, StatusPageError> {
        status_pages::Entity::find_by_id(status_page_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(StatusPageError::StatusPageNotFound { status_page_id })
    }

    /// The service validates its own input rather than trusting that the
    /// handler did: a monitor removed since authorization is a 400 here,
    /// not a foreign-key failure on insert.
    async fn ensure_monitors_exist(
        &self,
        selection: &[SelectedComponent],
    ) -> Result<(), StatusPageError> {
        self.projects_of_monitors(selection.iter().map(|c| c.monitor_id).collect())
            .await
            .map(|_| ())
    }

    async fn write_selection<C: sea_orm::ConnectionTrait>(
        conn: &C,
        status_page_id: i32,
        selection: &[SelectedComponent],
    ) -> Result<(), StatusPageError> {
        status_page_monitors::Entity::delete_many()
            .filter(status_page_monitors::Column::StatusPageId.eq(status_page_id))
            .exec(conn)
            .await?;
        if selection.is_empty() {
            return Ok(());
        }
        let rows = selection.iter().enumerate().map(|(position, component)| {
            status_page_monitors::ActiveModel {
                status_page_id: Set(status_page_id),
                monitor_id: Set(component.monitor_id),
                display_name: Set(component.display_name.clone()),
                group_name: Set(component.group_name.clone()),
                position: Set(position as i32),
                ..Default::default()
            }
        });
        status_page_monitors::Entity::insert_many(rows)
            .exec(conn)
            .await?;
        Ok(())
    }

    /// Where public pages are addressed on this install, read from the
    /// current settings. A settings read failure is reported as "not
    /// configured" with the failure as the reason, so listing pages still
    /// works and the console shows what went wrong instead of a broken link.
    async fn public_url_base(&self) -> PublicUrlBase {
        match self.config_service.get_settings().await {
            Ok(settings) => PublicUrlBase::from_settings(&settings),
            Err(e) => {
                warn!(
                    error = %e,
                    "could not read the settings holding the public URL; status page URLs are withheld"
                );
                PublicUrlBase::unavailable(format!(
                    "The settings that hold the public URL could not be read: {e}."
                ))
            }
        }
    }

    async fn to_response(
        &self,
        model: status_pages::Model,
    ) -> Result<StatusPageResponse, StatusPageError> {
        let mut responses = self.to_responses(vec![model]).await?;
        responses.pop().ok_or_else(|| {
            StatusPageError::Internal("status page response conversion produced no item".into())
        })
    }

    async fn to_responses(
        &self,
        models: Vec<status_pages::Model>,
    ) -> Result<Vec<StatusPageResponse>, StatusPageError> {
        if models.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<i32> = models.iter().map(|m| m.id).collect();
        let placeholders = (1..=ids.len())
            .map(|i| format!("${i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let rows = SelectionWithName::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            format!(
                r#"
                SELECT spm.status_page_id, spm.monitor_id, spm.display_name, spm.group_name,
                       spm.position, sm.name AS monitor_name, sm.project_id,
                       p.name AS project_name, p.slug AS project_slug,
                       sm.environment_id, e.name AS environment_name
                FROM status_page_monitors spm
                JOIN status_monitors sm ON sm.id = spm.monitor_id
                JOIN projects p ON p.id = sm.project_id
                LEFT JOIN environments e ON e.id = sm.environment_id
                WHERE spm.status_page_id IN ({placeholders})
                  AND NOT p.is_deleted
                ORDER BY spm.status_page_id, spm.position, spm.id
                "#
            ),
            ids.iter().map(|id| Value::from(*id)).collect::<Vec<_>>(),
        ))
        .all(self.db.as_ref())
        .await?;
        let mut by_page: BTreeMap<i32, Vec<StatusPageMonitorResponse>> = BTreeMap::new();
        let mut projects_by_page: BTreeMap<i32, Vec<StatusPageProject>> = BTreeMap::new();
        for row in rows {
            let page_projects = projects_by_page.entry(row.status_page_id).or_default();
            if !page_projects.iter().any(|p| p.id == row.project_id) {
                page_projects.push(StatusPageProject {
                    id: row.project_id,
                    name: row.project_name.clone(),
                    slug: row.project_slug.clone(),
                });
            }
            by_page
                .entry(row.status_page_id)
                .or_default()
                .push(StatusPageMonitorResponse {
                    monitor_id: row.monitor_id,
                    monitor_name: row.monitor_name,
                    display_name: row.display_name,
                    group_name: row.group_name,
                    position: row.position,
                    project_id: row.project_id,
                    project_name: row.project_name,
                    environment_id: row.environment_id,
                    environment_name: row.environment_name,
                });
        }

        let base = self.public_url_base().await;
        Ok(models
            .into_iter()
            .map(|model| StatusPageResponse {
                public_url: base.page_url(&model.slug),
                public_json_url: base.json_url(&model.slug),
                public_url_status: base.status.clone(),
                monitors: by_page.remove(&model.id).unwrap_or_default(),
                projects: projects_by_page.remove(&model.id).unwrap_or_default(),
                appearance: StatusPageAppearance::from_model(&model),
                id: model.id,
                title: model.title,
                description: model.description,
                slug: model.slug,
                is_published: model.is_published,
                published_at: model.published_at,
                created_at: model.created_at,
                updated_at: model.updated_at,
            })
            .collect())
    }
}

/// The base every public status page address is built from, and whether it is
/// usable.
///
/// The base is the install's configured public ingress URL
/// (`public_ingress_url`, else `external_url`). When neither is set there is
/// no base: Temps does not guess a host, because the placeholder it uses
/// elsewhere (`http://localho.st`) is a loopback name that is wrong for anyone
/// but the machine itself and carries no port.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PublicUrlBase {
    /// Configured base without a trailing slash, `None` when unconfigured.
    base: Option<String>,
    status: PublicUrlStatus,
}

impl PublicUrlBase {
    fn from_settings(settings: &temps_core::AppSettings) -> Self {
        let Some(base) = settings.configured_public_ingress_url() else {
            return Self::unavailable(
                "Public URL is not configured. Set the external URL or the public ingress \
                 URL so published status pages get an address that people outside your \
                 network can open."
                    .to_string(),
            );
        };
        // Judged on the base itself: a configured loopback or private host
        // still produces a link, but it will not open for the public.
        let reason = settings
            .resolve_public_ingress()
            .unreachable_reason
            .map(|why| {
                format!(
                    "Status pages are addressed at {base}, which people outside this machine \
                     or network cannot open: {why}. Set a publicly reachable external URL or \
                     public ingress URL."
                )
            });
        Self {
            status: PublicUrlStatus {
                configured: true,
                setup_path: reason
                    .is_some()
                    .then(|| temps_core::public_ingress::PUBLIC_INGRESS_SETUP_PATH.to_string()),
                reason,
            },
            base: Some(base),
        }
    }

    fn unavailable(reason: String) -> Self {
        Self {
            base: None,
            status: PublicUrlStatus {
                configured: false,
                reason: Some(reason),
                setup_path: Some(temps_core::public_ingress::PUBLIC_INGRESS_SETUP_PATH.to_string()),
            },
        }
    }

    fn page_url(&self, slug: &str) -> Option<String> {
        self.base
            .as_ref()
            .map(|base| format!("{base}/api/status/{slug}"))
    }

    fn json_url(&self, slug: &str) -> Option<String> {
        self.base
            .as_ref()
            .map(|base| format!("{base}/api/status/{slug}/summary.json"))
    }
}

#[async_trait::async_trait]
impl temps_core::ProjectDeletionHook for PublicStatusPageService {
    fn name(&self) -> &'static str {
        "status-pages"
    }

    /// Pages outlive the projects they draw from: the project is fenced
    /// (`is_deleted`) before this runs, so rebuilding drops its components
    /// from every page that shows them, and its monitors are deleted with its
    /// rows. Never fails the deletion: a failed rebuild takes those pages
    /// offline until the next successful refresh.
    async fn project_deletion_started(
        &self,
        project_id: i32,
    ) -> Result<(), temps_core::ProjectDeletionHookError> {
        // A published page with nothing left to show is unpublished, which
        // also releases its custom hostnames' certificates: nothing would be
        // served on them.
        match self.pages_left_without_components(project_id).await {
            Ok(page_ids) => {
                for status_page_id in page_ids {
                    if let Err(e) = self.unpublish(status_page_id).await {
                        error!(
                            project_id,
                            status_page_id,
                            error = %e,
                            "failed to unpublish a status page left without components by a project deletion"
                        );
                    }
                }
            }
            Err(e) => error!(
                project_id,
                error = %e,
                "failed to find status pages left without components by a project deletion"
            ),
        }
        self.refresh_project_pages(project_id).await;
        info!(
            project_id,
            "removed a project being deleted from the status pages that showed it"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(external: Option<&str>, ingress: Option<&str>) -> temps_core::AppSettings {
        temps_core::AppSettings {
            external_url: external.map(str::to_string),
            public_ingress_url: ingress.map(str::to_string),
            ..Default::default()
        }
    }

    const SLUG: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn unconfigured_install_has_no_public_url_and_no_invented_host() {
        let base = PublicUrlBase::from_settings(&settings(None, None));

        assert_eq!(base.page_url(SLUG), None);
        assert_eq!(base.json_url(SLUG), None);
        assert!(!base.status.configured);
        assert_eq!(
            base.status.setup_path.as_deref(),
            Some("/settings#public-ingress")
        );
        let reason = base
            .status
            .reason
            .expect("an unconfigured install explains itself");
        assert!(reason.contains("not configured"), "{reason}");
        assert!(!reason.contains("localho.st"), "{reason}");
    }

    #[test]
    fn blank_settings_count_as_unconfigured() {
        let base = PublicUrlBase::from_settings(&settings(Some("  "), Some("")));
        assert_eq!(base.page_url(SLUG), None);
        assert!(!base.status.configured);
    }

    #[test]
    fn external_url_is_used_when_no_ingress_url_is_set() {
        let base = PublicUrlBase::from_settings(&settings(Some("https://temps.example.com"), None));

        assert_eq!(
            base.page_url(SLUG).as_deref(),
            Some("https://temps.example.com/api/status/0123456789abcdef0123456789abcdef")
        );
        assert_eq!(
            base.json_url(SLUG).as_deref(),
            Some(
                "https://temps.example.com/api/status/0123456789abcdef0123456789abcdef/summary.json"
            )
        );
        assert_eq!(
            base.status,
            PublicUrlStatus {
                configured: true,
                reason: None,
                setup_path: None
            }
        );
    }

    #[test]
    fn ingress_url_takes_precedence_over_external_url() {
        let base = PublicUrlBase::from_settings(&settings(
            Some("https://console.example.com"),
            Some("https://hooks.example.com"),
        ));

        assert_eq!(
            base.page_url(SLUG).as_deref(),
            Some("https://hooks.example.com/api/status/0123456789abcdef0123456789abcdef")
        );
    }

    #[test]
    fn trailing_slashes_never_double_up() {
        let external =
            PublicUrlBase::from_settings(&settings(Some("https://temps.example.com//"), None));
        let ingress = PublicUrlBase::from_settings(&settings(
            Some("https://temps.example.com"),
            Some("https://hooks.example.com:8443/"),
        ));

        assert_eq!(
            external.page_url(SLUG).as_deref(),
            Some("https://temps.example.com/api/status/0123456789abcdef0123456789abcdef")
        );
        assert_eq!(
            ingress.page_url(SLUG).as_deref(),
            Some("https://hooks.example.com:8443/api/status/0123456789abcdef0123456789abcdef")
        );
    }

    #[test]
    fn configured_loopback_host_keeps_its_link_but_warns() {
        let base = PublicUrlBase::from_settings(&settings(Some("http://localhost:3000"), None));

        assert_eq!(
            base.page_url(SLUG).as_deref(),
            Some("http://localhost:3000/api/status/0123456789abcdef0123456789abcdef")
        );
        assert!(base.status.configured);
        let reason = base.status.reason.expect("loopback host is flagged");
        assert!(reason.contains("localhost"), "{reason}");
        assert_eq!(
            base.status.setup_path.as_deref(),
            Some("/settings#public-ingress")
        );
    }

    #[test]
    fn unreadable_settings_withhold_the_link_with_the_cause() {
        let base = PublicUrlBase::unavailable("The settings could not be read: boom.".into());
        assert_eq!(base.page_url(SLUG), None);
        assert!(!base.status.configured);
        assert!(base.status.reason.unwrap().contains("boom"));
        assert!(base.status.setup_path.is_some());
    }

    #[test]
    fn generated_slugs_are_128_bit_hex_and_distinct() {
        let mut seen = HashSet::new();
        for _ in 0..1000 {
            let slug = generate_slug().expect("OS RNG available");
            assert_eq!(slug.len(), SLUG_LEN);
            assert!(is_well_formed_slug(&slug), "{slug}");
            assert!(seen.insert(slug), "slug collision in 1000 draws");
        }
    }

    #[test]
    fn slug_shape_check_rejects_anything_else() {
        assert!(!is_well_formed_slug(""));
        assert!(!is_well_formed_slug("1"));
        assert!(!is_well_formed_slug(&"A".repeat(SLUG_LEN)));
        assert!(!is_well_formed_slug(&"g".repeat(SLUG_LEN)));
        assert!(!is_well_formed_slug(&"a".repeat(SLUG_LEN + 1)));
        assert!(!is_well_formed_slug("../../../../etc/passwd/aaaaaaaaaa"));
        assert!(is_well_formed_slug(&"a".repeat(SLUG_LEN)));
    }

    struct FailingRng;

    impl rand::TryRng for FailingRng {
        type Error = std::io::Error;

        fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
            Err(std::io::Error::other("entropy source unavailable"))
        }

        fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
            Err(std::io::Error::other("entropy source unavailable"))
        }

        fn try_fill_bytes(&mut self, _dst: &mut [u8]) -> Result<(), Self::Error> {
            Err(std::io::Error::other("entropy source unavailable"))
        }
    }

    impl rand::TryCryptoRng for FailingRng {}

    #[test]
    fn slug_generation_surfaces_rng_failure_with_context() {
        let err = generate_slug_with(&mut FailingRng).expect_err("rng failure must surface");
        assert!(matches!(err, StatusPageError::SlugGeneration { .. }));
        assert!(err.to_string().contains("entropy source unavailable"));
    }

    #[test]
    fn title_and_description_validation() {
        assert!(normalize_title("   ").is_err());
        assert!(normalize_title(&"x".repeat(MAX_TITLE_CHARS + 1)).is_err());
        assert!(normalize_title("bad\u{0}title").is_err());
        assert_eq!(
            normalize_title("  Status  ").ok(),
            Some("Status".to_string())
        );
        assert_eq!(normalize_description(Some("  ")).ok(), Some(None));
        assert_eq!(
            normalize_description(Some("line one\nline two")).ok(),
            Some(Some("line one\nline two".to_string()))
        );
        assert!(normalize_description(Some(&"x".repeat(MAX_DESCRIPTION_CHARS + 1))).is_err());
    }

    #[test]
    fn selection_validation() {
        let input = |id: i32, name: &str| StatusPageMonitorInput {
            monitor_id: id,
            display_name: name.to_string(),
            group_name: None,
        };
        assert!(matches!(
            normalize_selection(&[]),
            Err(StatusPageError::NoComponents)
        ));
        assert!(normalize_selection(&[input(1, "API"), input(1, "Again")]).is_err());
        assert!(normalize_selection(&[input(1, "  ")]).is_err());
        assert!(normalize_selection(&[input(1, &"x".repeat(MAX_DISPLAY_NAME_CHARS + 1))]).is_err());
        assert!(normalize_selection(&[input(1, "line\nbreak")]).is_err());
        let too_many: Vec<_> = (0..=MAX_MONITORS_PER_STATUS_PAGE as i32)
            .map(|i| input(i, "n"))
            .collect();
        assert!(normalize_selection(&too_many).is_err());
        let component = |id: i32, name: &str, group: Option<&str>| SelectedComponent {
            monitor_id: id,
            display_name: name.to_string(),
            group_name: group.map(str::to_string),
        };
        assert_eq!(
            normalize_selection(&[input(2, " Web "), input(1, "API")]).ok(),
            Some(vec![component(2, "Web", None), component(1, "API", None)])
        );
    }

    #[test]
    fn group_names_are_trimmed_blank_means_none_and_are_validated() {
        let input = |id: i32, group: Option<&str>| StatusPageMonitorInput {
            monitor_id: id,
            display_name: "API".to_string(),
            group_name: group.map(str::to_string),
        };
        let selection =
            normalize_selection(&[input(1, Some("  Temps Cloud ")), input(2, Some("   "))])
                .expect("valid selection");
        assert_eq!(selection[0].group_name.as_deref(), Some("Temps Cloud"));
        assert_eq!(selection[1].group_name, None);
        let too_long = "g".repeat(MAX_GROUP_NAME_CHARS + 1);
        let err = normalize_selection(&[input(3, Some(&too_long))]).expect_err("too long");
        assert!(err.to_string().contains("monitor 3"), "{err}");
        assert!(normalize_selection(&[input(4, Some("a\u{7}b"))]).is_err());
    }

    #[test]
    fn accent_color_is_normalized_to_lowercase_hex() {
        assert_eq!(
            normalize_accent_color("#4F46E5").unwrap(),
            Some("#4f46e5".to_string())
        );
        assert_eq!(
            normalize_accent_color(" 16a34a ").unwrap(),
            Some("#16a34a".to_string())
        );
        assert_eq!(normalize_accent_color("").unwrap(), None);
        for bad in ["#fff", "red", "#12345g", "#1234567", "#12;}x"] {
            assert!(
                matches!(
                    normalize_accent_color(bad),
                    Err(StatusPageError::Validation(_))
                ),
                "{bad} must be rejected"
            );
        }
    }

    #[test]
    fn page_urls_must_be_https_without_credentials() {
        assert_eq!(
            normalize_page_url("Logo URL", "https://cdn.example.com/logo.svg").unwrap(),
            Some("https://cdn.example.com/logo.svg".to_string())
        );
        assert_eq!(normalize_page_url("Logo URL", "  ").unwrap(), None);
        for bad in [
            "http://example.com/logo.png",
            "javascript:alert(1)",
            "data:image/png;base64,AAAA",
            "https://user:pass@example.com/",
            "/relative/logo.png",
        ] {
            assert!(
                matches!(
                    normalize_page_url("Logo URL", bad),
                    Err(StatusPageError::Validation(message)) if message.contains("Logo URL")
                ),
                "{bad} must be rejected"
            );
        }
        let long = format!("https://example.com/{}", "a".repeat(MAX_PAGE_URL_CHARS));
        assert!(normalize_page_url("Website URL", &long).is_err());
    }

    #[test]
    fn merge_appearance_changes_only_present_fields() {
        let current = StatusPageAppearance {
            accent_color: Some("#000000".to_string()),
            website_url: Some("https://example.com/".to_string()),
            ..StatusPageAppearance::default()
        };
        let merged = merge_appearance(
            current,
            &StatusPageAppearanceInput {
                theme: Some(super::super::types::StatusPageTheme::Dark),
                website_url: Some(String::new()),
                show_response_times: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(merged.theme, super::super::types::StatusPageTheme::Dark);
        assert_eq!(merged.accent_color.as_deref(), Some("#000000"));
        assert_eq!(merged.website_url, None);
        assert!(merged.show_uptime_percentage);
        assert!(!merged.show_response_times);

        assert!(merge_appearance(
            StatusPageAppearance::default(),
            &StatusPageAppearanceInput {
                logo_url: Some("http://insecure.example.com/x.png".to_string()),
                ..Default::default()
            },
        )
        .is_err());
    }
}
