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
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, FromQueryResult,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use temps_config::ConfigService;
use temps_entities::{projects, status_monitors, status_page_monitors, status_pages};
use tracing::{error, info, warn};

use super::snapshot::StatusSnapshotCache;
use super::types::{
    CreateStatusPageRequest, StatusPageListResponse, StatusPageMonitorInput,
    StatusPageMonitorResponse, StatusPageResponse, UpdateStatusPageRequest, MAX_DESCRIPTION_CHARS,
    MAX_DISPLAY_NAME_CHARS, MAX_MONITORS_PER_STATUS_PAGE, MAX_STATUS_PAGES_PER_PROJECT,
    MAX_TITLE_CHARS,
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

fn generate_slug_with<R: TryCryptoRng>(
    rng: &mut R,
    project_id: i32,
) -> Result<String, StatusPageError> {
    let mut bytes = [0u8; SLUG_BYTES];
    rng.try_fill_bytes(&mut bytes)
        .map_err(|e| StatusPageError::SlugGeneration {
            project_id,
            reason: e.to_string(),
        })?;
    Ok(hex::encode(bytes))
}

/// Generate a slug from the operating system's CSPRNG.
pub fn generate_slug(project_id: i32) -> Result<String, StatusPageError> {
    generate_slug_with(&mut rand::rngs::SysRng, project_id)
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

/// Validate the shape of a monitor selection (no database access).
fn normalize_selection(
    monitors: &[StatusPageMonitorInput],
) -> Result<Vec<(i32, String)>, StatusPageError> {
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
        normalized.push((input.monitor_id, name.to_string()));
    }
    Ok(normalized)
}

#[derive(Debug, FromQueryResult)]
struct SelectionWithName {
    status_page_id: i32,
    monitor_id: i32,
    display_name: String,
    position: i32,
    monitor_name: String,
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

    /// Resolve the owning project, for authorization on by-id routes.
    pub async fn get_project_id(&self, status_page_id: i32) -> Result<i32, StatusPageError> {
        status_pages::Entity::find_by_id(status_page_id)
            .select_only()
            .column(status_pages::Column::ProjectId)
            .into_tuple::<i32>()
            .one(self.db.as_ref())
            .await?
            .ok_or(StatusPageError::StatusPageNotFound { status_page_id })
    }

    pub async fn list(
        &self,
        project_id: i32,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<StatusPageListResponse, StatusPageError> {
        let page = page.unwrap_or(1).max(1);
        let page_size = page_size.unwrap_or(20).clamp(1, 100);
        let paginator = status_pages::Entity::find()
            .filter(status_pages::Column::ProjectId.eq(project_id))
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

    pub async fn create(
        &self,
        project_id: i32,
        request: CreateStatusPageRequest,
    ) -> Result<StatusPageResponse, StatusPageError> {
        let title = normalize_title(&request.title)?;
        let description = normalize_description(request.description.as_deref())?;
        let selection = normalize_selection(&request.monitors)?;
        self.ensure_monitors_in_project(project_id, &selection)
            .await?;

        let slug = generate_slug(project_id)?;
        let txn = self.db.begin().await?;
        let result = async {
            // Lock the project row so concurrent creates cannot both pass the
            // per-project limit, and so a missing project is a 404, not an FK
            // error.
            let project = projects::Entity::find_by_id(project_id)
                .filter(projects::Column::IsDeleted.eq(false))
                .lock_exclusive()
                .one(&txn)
                .await?;
            if project.is_none() {
                return Err(StatusPageError::ProjectNotFound { project_id });
            }
            let existing = status_pages::Entity::find()
                .filter(status_pages::Column::ProjectId.eq(project_id))
                .count(&txn)
                .await?;
            if existing >= MAX_STATUS_PAGES_PER_PROJECT {
                return Err(StatusPageError::StatusPageLimitReached {
                    project_id,
                    limit: MAX_STATUS_PAGES_PER_PROJECT,
                });
            }
            let model = status_pages::ActiveModel {
                project_id: Set(project_id),
                slug: Set(slug),
                title: Set(title),
                description: Set(description),
                is_published: Set(false),
                published_at: Set(None),
                ..Default::default()
            }
            .insert(&txn)
            .await?;
            Self::write_selection(&txn, model.id, &selection).await?;
            Ok(model)
        }
        .await;
        let model = Self::finish(txn, result).await?;

        info!(
            status_page_id = model.id,
            project_id,
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
        let project_id = self.get_project_id(status_page_id).await?;
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
            self.ensure_monitors_in_project(project_id, selection)
                .await?;
        }

        // Read and write the page under a row lock so a concurrent publish
        // cannot interleave with a selection change.
        let txn = self.db.begin().await?;
        let result = async {
            let model = Self::find_locked(&txn, status_page_id).await?;
            if model.is_published && selection.as_ref().is_some_and(|s| s.is_empty()) {
                return Err(StatusPageError::PublishWithoutMonitors { status_page_id });
            }
            let mut active: status_pages::ActiveModel = model.into();
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
            let selected = status_page_monitors::Entity::find()
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
        info!(
            status_page_id,
            project_id = model.project_id,
            "published status page"
        );
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
        info!(
            status_page_id,
            project_id = model.project_id,
            "unpublished status page"
        );
        self.to_response(model).await
    }

    /// Replace the public URL with a fresh one. The old URL stops working
    /// immediately.
    pub async fn rotate_slug(
        &self,
        status_page_id: i32,
    ) -> Result<StatusPageResponse, StatusPageError> {
        let project_id = self.get_project_id(status_page_id).await?;
        let slug = generate_slug(project_id)?;
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
        info!(
            status_page_id,
            project_id = model.project_id,
            "rotated status page URL"
        );
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

    /// Rebuild every published page of a project, e.g. after an incident
    /// update was made public or private. Fails closed: on error the project's
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

    /// Stop serving every page of a deleted project.
    pub async fn remove_project_pages(&self, project_id: i32) {
        self.cache.remove_project(project_id).await;
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

    async fn ensure_monitors_in_project(
        &self,
        project_id: i32,
        selection: &[(i32, String)],
    ) -> Result<(), StatusPageError> {
        if selection.is_empty() {
            return Ok(());
        }
        let requested: Vec<i32> = selection.iter().map(|(id, _)| *id).collect();
        let found: HashSet<i32> = status_monitors::Entity::find()
            .select_only()
            .column(status_monitors::Column::Id)
            .filter(status_monitors::Column::Id.is_in(requested.clone()))
            .filter(status_monitors::Column::ProjectId.eq(project_id))
            .into_tuple::<i32>()
            .all(self.db.as_ref())
            .await?
            .into_iter()
            .collect();
        let missing: Vec<i32> = requested
            .into_iter()
            .filter(|id| !found.contains(id))
            .collect();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(StatusPageError::MonitorsNotInProject {
                monitor_ids: missing,
                project_id,
            })
        }
    }

    async fn write_selection<C: sea_orm::ConnectionTrait>(
        conn: &C,
        status_page_id: i32,
        selection: &[(i32, String)],
    ) -> Result<(), StatusPageError> {
        status_page_monitors::Entity::delete_many()
            .filter(status_page_monitors::Column::StatusPageId.eq(status_page_id))
            .exec(conn)
            .await?;
        if selection.is_empty() {
            return Ok(());
        }
        let rows = selection
            .iter()
            .enumerate()
            .map(
                |(position, (monitor_id, name))| status_page_monitors::ActiveModel {
                    status_page_id: Set(status_page_id),
                    monitor_id: Set(*monitor_id),
                    display_name: Set(name.clone()),
                    position: Set(position as i32),
                    ..Default::default()
                },
            );
        status_page_monitors::Entity::insert_many(rows)
            .exec(conn)
            .await?;
        Ok(())
    }

    /// Base URL the page's public links are built on, and whether it came from
    /// the operator's configuration.
    ///
    /// `public_ingress_url` / `external_url` when set. When neither is, the
    /// effective ingress URL is the built-in `http://localho.st` default, which
    /// has no port and does not reach this server — links built on it 404.
    /// The server cannot know the origin the operator reaches the console on
    /// (its bind address is not that), so the links are *relative* paths and
    /// `configured` is `false`: a client resolves them against the console it
    /// is already talking to, and shows the operator the setting to configure.
    async fn public_base(&self) -> PublicBase {
        let configured = match self.config_service.get_settings().await {
            Ok(settings) => settings.configured_public_ingress_url(),
            Err(e) => {
                warn!(
                    error = %e,
                    "could not read the public URL settings; status page links are returned relative"
                );
                None
            }
        };
        public_base_from(configured)
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
        let rows = status_page_monitors::Entity::find()
            .select_only()
            .column(status_page_monitors::Column::StatusPageId)
            .column(status_page_monitors::Column::MonitorId)
            .column(status_page_monitors::Column::DisplayName)
            .column(status_page_monitors::Column::Position)
            .column_as(status_monitors::Column::Name, "monitor_name")
            .inner_join(status_monitors::Entity)
            .filter(status_page_monitors::Column::StatusPageId.is_in(ids))
            .order_by_asc(status_page_monitors::Column::StatusPageId)
            .order_by_asc(status_page_monitors::Column::Position)
            .order_by_asc(status_page_monitors::Column::Id)
            .into_model::<SelectionWithName>()
            .all(self.db.as_ref())
            .await?;
        let mut by_page: BTreeMap<i32, Vec<StatusPageMonitorResponse>> = BTreeMap::new();
        for row in rows {
            by_page
                .entry(row.status_page_id)
                .or_default()
                .push(StatusPageMonitorResponse {
                    monitor_id: row.monitor_id,
                    monitor_name: row.monitor_name,
                    display_name: row.display_name,
                    position: row.position,
                });
        }

        let base = self.public_base().await;
        let base_url = base.url;
        let url_configured = base.configured;
        Ok(models
            .into_iter()
            .map(|model| StatusPageResponse {
                public_url: format!("{base_url}/api/status/{}", model.slug),
                public_json_url: format!("{base_url}/api/status/{}/summary.json", model.slug),
                public_url_configured: url_configured,
                public_url_setup_path: if url_configured {
                    None
                } else {
                    Some(PUBLIC_URL_SETTINGS_PATH.to_string())
                },
                monitors: by_page.remove(&model.id).unwrap_or_default(),
                id: model.id,
                project_id: model.project_id,
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

/// Console path where the public URL is configured.
const PUBLIC_URL_SETTINGS_PATH: &str = "/settings";

/// The base public links are built on (see [`PublicStatusPageService::public_base`]).
#[derive(Debug, PartialEq, Eq)]
struct PublicBase {
    /// The configured origin, or empty when none is configured — links are
    /// then paths relative to the console origin.
    url: String,
    /// `false` when no public/external URL is configured and links are relative.
    configured: bool,
}

/// Choose the base URL: the configured one, else none (relative links).
fn public_base_from(configured: Option<String>) -> PublicBase {
    match configured {
        Some(url) => PublicBase {
            url: url.trim_end_matches('/').to_string(),
            configured: true,
        },
        None => PublicBase {
            url: String::new(),
            configured: false,
        },
    }
}

#[async_trait::async_trait]
impl temps_core::ProjectDeletionHook for PublicStatusPageService {
    async fn project_deletion_started(&self, project_id: i32) {
        self.remove_project_pages(project_id).await;
        match super::domains::release_project_certificates(self.db.as_ref(), project_id).await {
            Ok(released) if released > 0 => info!(
                project_id,
                released, "released status page certificates of a project being deleted"
            ),
            Ok(_) => {}
            Err(e) => error!(
                project_id,
                error = %e,
                "failed to release status page certificates of a project being deleted; they are deleted with the project"
            ),
        }
        info!(
            project_id,
            "stopped serving status pages of a project being deleted"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With no public/external URL configured the links used to be built on
    /// the port-less `http://localho.st` default, which 404s. They are now
    /// relative to the console origin the client already uses, and the
    /// response reports that the URL is not configured. (The server's bind
    /// address is not the origin an operator reaches the console on, so it is
    /// never put into a link.)
    #[test]
    fn unconfigured_public_url_yields_relative_links() {
        let base = public_base_from(None);
        assert_eq!(
            base,
            PublicBase {
                url: String::new(),
                configured: false
            }
        );
        assert_eq!(
            format!("{}/api/status/{}", base.url, "abc123"),
            "/api/status/abc123"
        );
    }

    #[test]
    fn configured_public_url_is_used_as_is_without_a_trailing_slash() {
        let base = public_base_from(Some("https://status.example.test/".to_string()));
        assert_eq!(base.url, "https://status.example.test");
        assert!(base.configured);
    }

    #[test]
    fn generated_slugs_are_128_bit_hex_and_distinct() {
        let mut seen = HashSet::new();
        for _ in 0..1000 {
            let slug = generate_slug(1).expect("OS RNG available");
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
        let err = generate_slug_with(&mut FailingRng, 42).expect_err("rng failure must surface");
        assert!(matches!(
            err,
            StatusPageError::SlugGeneration { project_id: 42, .. }
        ));
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
        };
        assert!(normalize_selection(&[input(1, "API"), input(1, "Again")]).is_err());
        assert!(normalize_selection(&[input(1, "  ")]).is_err());
        assert!(normalize_selection(&[input(1, &"x".repeat(MAX_DISPLAY_NAME_CHARS + 1))]).is_err());
        assert!(normalize_selection(&[input(1, "line\nbreak")]).is_err());
        let too_many: Vec<_> = (0..=MAX_MONITORS_PER_STATUS_PAGE as i32)
            .map(|i| input(i, "n"))
            .collect();
        assert!(normalize_selection(&too_many).is_err());
        assert_eq!(
            normalize_selection(&[input(2, " Web "), input(1, "API")]).ok(),
            Some(vec![(2, "Web".to_string()), (1, "API".to_string())])
        );
    }
}
