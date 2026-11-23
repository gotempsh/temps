// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runtime AI model catalog (ADR 112).
//!
//! A Temps release bundles a price table (`services::pricing`) and per
//! provider model lists. Both age between releases, so a running instance
//! also keeps:
//!
//! * a **remote catalog**: a models.dev-compatible JSON document, by default
//!   the one published in the Temps repository, fetched on a bounded interval
//!   or on demand, validated, stored in `ai_model_catalog_entries`, and layered
//!   over the bundled price table (`pricing::install_catalog_prices`);
//! * **live discovery**: each active provider key's own model listing,
//!   refreshed on the same interval (`ProviderModelService::refresh`).
//!
//! Everything here is control plane: one fetch per interval, one key at a
//! time, nothing on the request path except the lock-free price lookup.
//! A failure records why, keeps the last good catalog and retries later;
//! the bundled table always remains the fallback.

pub mod error;
pub mod source;

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use temps_entities::{
    ai_model_catalog_entries, ai_model_catalog_settings, ai_provider_keys, ai_provider_models,
};
use utoipa::ToSchema;

pub use error::ModelCatalogError;
pub use source::{CatalogModel, ParsedCatalog};

use crate::providers::ProviderHttpClient;
use crate::services::pricing;
use crate::services::{ProviderKeyService, ProviderModelService};
use crate::types::ModelDetails;

/// The catalog published in the Temps repository: generated from the bundled
/// price table (see `catalog_file` tests), so a reviewed price change reaches
/// running instances without a release.
pub const DEFAULT_CATALOG_URL: &str =
    "https://raw.githubusercontent.com/gotempsh/temps/main/crates/temps-ai-gateway/catalog/model-catalog.json";

/// Default hours between background refreshes.
pub const DEFAULT_REFRESH_INTERVAL_HOURS: i32 = 24;
/// Bounds of `refresh_interval_hours` (the table's CHECK constraint).
pub const MIN_REFRESH_INTERVAL_HOURS: i32 = 1;
pub const MAX_REFRESH_INTERVAL_HOURS: i32 = 168;

/// How often the background task looks for due work.
const LOOP_TICK: Duration = Duration::from_secs(15 * 60);
/// Delay before the first background check, so boot is not slowed and the
/// startup model discovery the plugin runs is not repeated at once.
const STARTUP_DELAY: Duration = Duration::from_secs(2 * 60);
/// Wait at least this long after a failed attempt (or the interval, if
/// shorter) before trying again.
const FAILURE_RETRY: chrono::Duration = chrono::Duration::hours(1);
/// Deadline for one provider key's model discovery in the background.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(2 * 60);
/// Rows per INSERT when storing a catalog.
const INSERT_BATCH: usize = 500;
/// Longest stored error text.
const MAX_ERROR_LEN: usize = 500;
/// Most unpriced models listed in one report.
const MAX_UNPRICED_LISTED: usize = 100;

// ============================================================================
// DTOs
// ============================================================================

/// Where the catalog comes from and how it is refreshed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ModelCatalogSettings {
    /// Custom catalog URL; `null` uses `default_source_url`.
    pub source_url: Option<String>,
    /// The URL refreshes fetch.
    pub effective_source_url: String,
    /// The catalog published in the Temps repository.
    pub default_source_url: String,
    /// Refresh the catalog in the background.
    pub auto_refresh: bool,
    /// Hours between background refreshes (1 to 168).
    pub refresh_interval_hours: i32,
    /// Rediscover every active provider key's models on the same interval.
    pub discover_provider_models: bool,
}

/// A change to the catalog settings. Absent fields are left as they are.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct ModelCatalogSettingsUpdate {
    /// A custom `https://` catalog URL, or an empty string to go back to the
    /// catalog published in the Temps repository.
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default)]
    pub auto_refresh: Option<bool>,
    #[serde(default)]
    pub refresh_interval_hours: Option<i32>,
    #[serde(default)]
    pub discover_provider_models: Option<bool>,
}

/// The catalog's settings and the outcome of its last refresh.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ModelCatalogStatus {
    pub settings: ModelCatalogSettings,
    /// When a refresh last started.
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_attempt_at: Option<DateTime<Utc>>,
    /// When a refresh last succeeded.
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_refreshed_at: Option<DateTime<Utc>>,
    /// The URL the stored catalog came from.
    pub last_source_url: Option<String>,
    /// Why the last attempt failed; `null` after a success.
    pub last_error: Option<String>,
    /// Models stored by the last successful refresh.
    pub entry_count: i32,
    /// Entries the last successful refresh rejected as invalid.
    pub skipped_count: i32,
    /// Catalog prices currently applied over the bundled table.
    pub applied_price_count: usize,
    /// Models the bundled price table lists.
    pub bundled_model_count: usize,
    /// When provider keys' models were last rediscovered in the background.
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_discovery_at: Option<DateTime<Utc>>,
    /// When the next background refresh is due; `null` when automatic
    /// refresh is off.
    #[schema(value_type = Option<String>, format = DateTime)]
    pub next_refresh_at: Option<DateTime<Utc>>,
    /// A refresh is running right now.
    pub refreshing: bool,
}

/// What a successful refresh stored.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ModelCatalogRefreshResult {
    /// Models stored.
    pub stored: usize,
    /// Prices applied over the bundled table.
    pub applied_prices: usize,
    /// Entries rejected as invalid.
    pub skipped: usize,
    pub status: ModelCatalogStatus,
}

/// One stored catalog model.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ModelCatalogEntry {
    pub provider: String,
    pub model_id: String,
    pub display_name: String,
    /// USD per 1M input tokens; `null` when the catalog publishes no price.
    pub input_per_million: Option<f64>,
    pub output_per_million: Option<f64>,
    pub cache_read_per_million: Option<f64>,
    pub cache_write_per_million: Option<f64>,
    pub context_window: Option<i64>,
    pub max_output_tokens: Option<i64>,
    pub supports_tools: bool,
    pub supports_vision: bool,
    pub supports_reasoning: bool,
    pub deprecated: bool,
    pub release_date: Option<String>,
    pub source_url: String,
    #[schema(value_type = String, format = DateTime)]
    pub fetched_at: DateTime<Utc>,
}

impl From<ai_model_catalog_entries::Model> for ModelCatalogEntry {
    fn from(row: ai_model_catalog_entries::Model) -> Self {
        Self {
            provider: row.provider,
            model_id: row.model_id,
            display_name: row.display_name,
            input_per_million: row.input_per_million,
            output_per_million: row.output_per_million,
            cache_read_per_million: row.cache_read_per_million,
            cache_write_per_million: row.cache_write_per_million,
            context_window: row.context_window,
            max_output_tokens: row.max_output_tokens,
            supports_tools: row.supports_tools,
            supports_vision: row.supports_vision,
            supports_reasoning: row.supports_reasoning,
            deprecated: row.deprecated,
            release_date: row.release_date,
            source_url: row.source_url,
            fetched_at: row.fetched_at,
        }
    }
}

/// A page of stored catalog models.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ModelCatalogEntryPage {
    pub items: Vec<ModelCatalogEntry>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

/// Filter for the stored catalog.
#[derive(Debug, Clone, Default)]
pub struct ModelCatalogEntryFilter {
    pub provider: Option<String>,
    /// Case-insensitive substring of the model id or name.
    pub search: Option<String>,
}

/// A model enabled on a provider key that no price covers: its usage is
/// charged the worst-case rate and reported as "price unknown".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct UnpricedModel {
    pub provider_key_id: i32,
    pub provider_key_name: String,
    pub provider: String,
    pub model_id: String,
}

/// Enabled models no price covers.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UnpricedModelReport {
    /// Up to 100 models, ordered by provider key and model id.
    pub models: Vec<UnpricedModel>,
    /// How many there are in total.
    pub total: usize,
}

/// Why a refresh ran, for logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshTrigger {
    Manual,
    Scheduled,
}

impl RefreshTrigger {
    fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Scheduled => "scheduled",
        }
    }
}

// ============================================================================
// Scheduling rules (pure, tested)
// ============================================================================

fn interval_of(settings: &ai_model_catalog_settings::Model) -> chrono::Duration {
    chrono::Duration::hours(i64::from(
        settings
            .refresh_interval_hours
            .clamp(MIN_REFRESH_INTERVAL_HOURS, MAX_REFRESH_INTERVAL_HOURS),
    ))
}

/// When the next background refresh is due; `None` when it is off.
pub(crate) fn next_refresh_at(
    settings: &ai_model_catalog_settings::Model,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    if !settings.auto_refresh {
        return None;
    }
    let interval = interval_of(settings);
    let after_success = settings
        .last_refreshed_at
        .map_or(now, |refreshed| refreshed + interval);
    // A failed attempt waits before the next try, so an unreachable source
    // is not fetched every tick.
    let after_failure = match (settings.last_error.as_ref(), settings.last_attempt_at) {
        (Some(_), Some(attempt)) => attempt + interval.min(FAILURE_RETRY),
        _ => now,
    };
    Some(after_success.max(after_failure))
}

pub(crate) fn refresh_due(settings: &ai_model_catalog_settings::Model, now: DateTime<Utc>) -> bool {
    next_refresh_at(settings, now).is_some_and(|due| due <= now)
}

/// Whether provider keys' models are due for rediscovery. `booted_at` stands
/// in for a missing `last_discovery_at`: the plugin discovers every key once
/// at startup.
pub(crate) fn discovery_due(
    settings: &ai_model_catalog_settings::Model,
    now: DateTime<Utc>,
    booted_at: DateTime<Utc>,
) -> bool {
    settings.discover_provider_models
        && now
            - settings
                .last_discovery_at
                .unwrap_or(booted_at)
                .max(booted_at)
            >= interval_of(settings)
}

/// The settings row's values when no row exists yet.
pub(crate) fn default_settings_row() -> ai_model_catalog_settings::Model {
    let epoch = DateTime::<Utc>::UNIX_EPOCH;
    ai_model_catalog_settings::Model {
        id: ai_model_catalog_settings::SINGLETON_ID,
        source_url: None,
        auto_refresh: true,
        refresh_interval_hours: DEFAULT_REFRESH_INTERVAL_HOURS,
        discover_provider_models: true,
        last_attempt_at: None,
        last_refreshed_at: None,
        last_source_url: None,
        last_error: None,
        entry_count: 0,
        skipped_count: 0,
        last_discovery_at: None,
        created_at: epoch,
        updated_at: epoch,
    }
}

fn settings_view(row: &ai_model_catalog_settings::Model) -> ModelCatalogSettings {
    ModelCatalogSettings {
        source_url: row.source_url.clone(),
        effective_source_url: row
            .source_url
            .clone()
            .unwrap_or_else(|| DEFAULT_CATALOG_URL.to_string()),
        default_source_url: DEFAULT_CATALOG_URL.to_string(),
        auto_refresh: row.auto_refresh,
        refresh_interval_hours: row.refresh_interval_hours,
        discover_provider_models: row.discover_provider_models,
    }
}

/// Validate a settings change. Returns the new `source_url` value, if the
/// change sets one (`Some(None)` resets it to the default).
pub(crate) fn validate_update(
    update: &ModelCatalogSettingsUpdate,
) -> Result<Option<Option<String>>, ModelCatalogError> {
    if let Some(hours) = update.refresh_interval_hours {
        if !(MIN_REFRESH_INTERVAL_HOURS..=MAX_REFRESH_INTERVAL_HOURS).contains(&hours) {
            return Err(ModelCatalogError::InvalidSettings {
                field: "refresh_interval_hours",
                reason: format!(
                    "{hours} is outside {MIN_REFRESH_INTERVAL_HOURS} to {MAX_REFRESH_INTERVAL_HOURS} hours"
                ),
            });
        }
    }
    Ok(match update.source_url.as_deref().map(str::trim) {
        None => None,
        Some("") => Some(None),
        Some(url) => {
            source::validate_source_url(url)?;
            Some(Some(url.to_string()))
        }
    })
}

fn truncate_error(error: &ModelCatalogError) -> String {
    error.to_string().chars().take(MAX_ERROR_LEN).collect()
}

fn entry_row(
    model: &CatalogModel,
    source_url: &str,
    fetched_at: DateTime<Utc>,
) -> ai_model_catalog_entries::ActiveModel {
    ai_model_catalog_entries::ActiveModel {
        provider: Set(model.provider.clone()),
        model_id: Set(model.model_id.clone()),
        display_name: Set(model.display_name.clone()),
        input_per_million: Set(model.input_per_million),
        output_per_million: Set(model.output_per_million),
        cache_read_per_million: Set(model.cache_read_per_million),
        cache_write_per_million: Set(model.cache_write_per_million),
        context_window: Set(model.context_window),
        max_output_tokens: Set(model.max_output_tokens),
        supports_tools: Set(model.supports_tools),
        supports_vision: Set(model.supports_vision),
        supports_reasoning: Set(model.supports_reasoning),
        deprecated: Set(model.deprecated),
        release_date: Set(model.release_date.clone()),
        source_url: Set(source_url.to_string()),
        fetched_at: Set(fetched_at),
        ..Default::default()
    }
}

fn stored_price(row: &ai_model_catalog_entries::Model) -> Option<pricing::CatalogPrice> {
    Some(pricing::CatalogPrice {
        provider: row.provider.clone(),
        model: row.model_id.clone(),
        display_name: row.display_name.clone(),
        input_per_million: row.input_per_million?,
        output_per_million: row.output_per_million?,
        cache_read_per_million: row.cache_read_per_million,
        cache_write_per_million: row.cache_write_per_million,
        deprecated: row.deprecated,
    })
}

/// Whether a key's model has a price: its discovered listing publishes one,
/// or the effective price table covers its id.
fn model_is_priced(row: &ai_provider_models::Model) -> bool {
    let listed = row
        .metadata
        .clone()
        .and_then(|metadata| serde_json::from_value::<ModelDetails>(metadata).ok())
        .is_some_and(|details| {
            details
                .input_price_per_million
                .zip(details.output_price_per_million)
                .is_some_and(|(input, output)| input.is_finite() && output.is_finite())
        });
    listed || pricing::is_priced(&row.model_id)
}

// ============================================================================
// Service
// ============================================================================

pub struct ModelCatalogService {
    db: Arc<DatabaseConnection>,
    client: ProviderHttpClient,
    provider_key_service: Arc<ProviderKeyService>,
    provider_model_service: Arc<ProviderModelService>,
    /// Single flight for refreshes, manual and scheduled.
    refresh_lock: tokio::sync::Mutex<()>,
    booted_at: DateTime<Utc>,
}

impl ModelCatalogService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        provider_key_service: Arc<ProviderKeyService>,
        provider_model_service: Arc<ProviderModelService>,
    ) -> Self {
        Self {
            db,
            client: ProviderHttpClient::new(source::FETCH_TIMEOUT),
            provider_key_service,
            provider_model_service,
            refresh_lock: tokio::sync::Mutex::new(()),
            booted_at: Utc::now(),
        }
    }

    /// The settings row, or the defaults when there is none.
    async fn settings_row(&self) -> Result<ai_model_catalog_settings::Model, ModelCatalogError> {
        Ok(
            ai_model_catalog_settings::Entity::find_by_id(ai_model_catalog_settings::SINGLETON_ID)
                .one(self.db.as_ref())
                .await?
                .unwrap_or_else(default_settings_row),
        )
    }

    /// Create the settings row with its defaults if it is missing, then
    /// return it. Concurrent callers are safe: the insert does nothing when
    /// the row exists.
    async fn ensure_settings_row<C: ConnectionTrait>(
        connection: &C,
    ) -> Result<ai_model_catalog_settings::Model, ModelCatalogError> {
        ai_model_catalog_settings::Entity::insert(ai_model_catalog_settings::ActiveModel {
            id: Set(ai_model_catalog_settings::SINGLETON_ID),
            ..Default::default()
        })
        .on_conflict(
            OnConflict::column(ai_model_catalog_settings::Column::Id)
                .do_nothing()
                .to_owned(),
        )
        .exec_without_returning(connection)
        .await?;
        ai_model_catalog_settings::Entity::find_by_id(ai_model_catalog_settings::SINGLETON_ID)
            .one(connection)
            .await?
            .ok_or_else(|| {
                ModelCatalogError::Database(sea_orm::DbErr::RecordNotFound(format!(
                    "ai_model_catalog_settings row {} after insert",
                    ai_model_catalog_settings::SINGLETON_ID
                )))
            })
    }

    pub async fn settings(&self) -> Result<ModelCatalogSettings, ModelCatalogError> {
        Ok(settings_view(&self.settings_row().await?))
    }

    /// Apply a settings change. Returns the new settings and the names of
    /// the fields that changed, for the audit log.
    pub async fn update_settings(
        &self,
        update: ModelCatalogSettingsUpdate,
    ) -> Result<(ModelCatalogSettings, Vec<&'static str>), ModelCatalogError> {
        let source_url = validate_update(&update)?;
        let row = Self::ensure_settings_row(self.db.as_ref()).await?;
        let mut changed = Vec::new();
        let mut active: ai_model_catalog_settings::ActiveModel = row.clone().into();
        if let Some(source_url) = source_url {
            if source_url != row.source_url {
                active.source_url = Set(source_url);
                changed.push("source_url");
            }
        }
        if let Some(auto_refresh) = update.auto_refresh {
            if auto_refresh != row.auto_refresh {
                active.auto_refresh = Set(auto_refresh);
                changed.push("auto_refresh");
            }
        }
        if let Some(hours) = update.refresh_interval_hours {
            if hours != row.refresh_interval_hours {
                active.refresh_interval_hours = Set(hours);
                changed.push("refresh_interval_hours");
            }
        }
        if let Some(discover) = update.discover_provider_models {
            if discover != row.discover_provider_models {
                active.discover_provider_models = Set(discover);
                changed.push("discover_provider_models");
            }
        }
        if changed.is_empty() {
            return Ok((settings_view(&row), changed));
        }
        let saved = active.update(self.db.as_ref()).await?;
        tracing::info!(
            changed = ?changed,
            auto_refresh = saved.auto_refresh,
            refresh_interval_hours = saved.refresh_interval_hours,
            custom_source = saved.source_url.is_some(),
            "AI model catalog settings updated"
        );
        Ok((settings_view(&saved), changed))
    }

    pub async fn status(&self) -> Result<ModelCatalogStatus, ModelCatalogError> {
        let row = self.settings_row().await?;
        Ok(self.status_of(&row))
    }

    fn status_of(&self, row: &ai_model_catalog_settings::Model) -> ModelCatalogStatus {
        ModelCatalogStatus {
            settings: settings_view(row),
            last_attempt_at: row.last_attempt_at,
            last_refreshed_at: row.last_refreshed_at,
            last_source_url: row.last_source_url.clone(),
            last_error: row.last_error.clone(),
            entry_count: row.entry_count,
            skipped_count: row.skipped_count,
            applied_price_count: pricing::catalog_price_count(),
            bundled_model_count: pricing::build_pricing().len(),
            last_discovery_at: row.last_discovery_at,
            next_refresh_at: next_refresh_at(row, Utc::now()),
            refreshing: self.refresh_lock.try_lock().is_err(),
        }
    }

    /// Stored catalog models, newest release first.
    pub async fn list_entries(
        &self,
        filter: ModelCatalogEntryFilter,
        page: u64,
        page_size: u64,
    ) -> Result<ModelCatalogEntryPage, ModelCatalogError> {
        let page_size = page_size.clamp(1, 100);
        let mut query = ai_model_catalog_entries::Entity::find();
        if let Some(provider) = filter.provider.filter(|p| !p.is_empty()) {
            query = query.filter(ai_model_catalog_entries::Column::Provider.eq(provider));
        }
        if let Some(search) = filter.search.map(|s| s.trim().to_lowercase()) {
            if !search.is_empty() {
                let pattern = format!(
                    "%{}%",
                    search
                        .replace('\\', "\\\\")
                        .replace('%', "\\%")
                        .replace('_', "\\_")
                );
                query = query.filter(
                    sea_orm::Condition::any()
                        .add(
                            sea_orm::sea_query::Expr::expr(sea_orm::sea_query::Func::lower(
                                sea_orm::sea_query::Expr::col(
                                    ai_model_catalog_entries::Column::ModelId,
                                ),
                            ))
                            .like(pattern.clone()),
                        )
                        .add(
                            sea_orm::sea_query::Expr::expr(sea_orm::sea_query::Func::lower(
                                sea_orm::sea_query::Expr::col(
                                    ai_model_catalog_entries::Column::DisplayName,
                                ),
                            ))
                            .like(pattern),
                        ),
                );
            }
        }
        let total = query.clone().count(self.db.as_ref()).await?;
        let items = query
            .order_by_desc(ai_model_catalog_entries::Column::ReleaseDate)
            .order_by_asc(ai_model_catalog_entries::Column::Provider)
            .order_by_asc(ai_model_catalog_entries::Column::ModelId)
            .offset(temps_core::config::page_offset(page, page_size))
            .limit(page_size)
            .all(self.db.as_ref())
            .await?;
        Ok(ModelCatalogEntryPage {
            items: items.into_iter().map(Into::into).collect(),
            total,
            page: page.max(1),
            page_size,
        })
    }

    /// Models enabled on an active vendor key that no price covers.
    /// Self-hosted keys are left out: their models have no vendor price by
    /// design and are recorded at zero.
    pub async fn unpriced_models(&self) -> Result<UnpricedModelReport, ModelCatalogError> {
        let rows = ai_provider_models::Entity::find()
            .find_also_related(ai_provider_keys::Entity)
            .filter(ai_provider_models::Column::IsEnabled.eq(true))
            .filter(ai_provider_models::Column::IsAvailable.eq(true))
            .filter(ai_provider_keys::Column::IsActive.eq(true))
            .order_by_asc(ai_provider_models::Column::ProviderKeyId)
            .order_by_asc(ai_provider_models::Column::ModelId)
            .all(self.db.as_ref())
            .await?;
        let mut unpriced = rows
            .into_iter()
            .filter_map(|(model, key)| key.map(|key| (model, key)))
            .filter(|(_, key)| !crate::self_hosted::is_self_hosted(&key.provider))
            .filter(|(model, _)| !model_is_priced(model))
            .map(|(model, key)| UnpricedModel {
                provider_key_id: key.id,
                provider_key_name: key.display_name,
                provider: key.provider,
                model_id: model.model_id,
            });
        let models: Vec<UnpricedModel> = unpriced.by_ref().take(MAX_UNPRICED_LISTED).collect();
        let total = models.len() + unpriced.count();
        Ok(UnpricedModelReport { models, total })
    }

    /// Apply the stored catalog's prices. Run once at startup so a restart
    /// without network access keeps the last catalog.
    pub async fn load_stored(&self) -> Result<usize, ModelCatalogError> {
        let rows = ai_model_catalog_entries::Entity::find()
            .limit(source::MAX_CATALOG_MODELS as u64)
            .all(self.db.as_ref())
            .await?;
        let prices: Vec<pricing::CatalogPrice> = rows.iter().filter_map(stored_price).collect();
        let applied = pricing::install_catalog_prices(&prices);
        tracing::debug!(
            stored = rows.len(),
            applied,
            "Applied the stored AI model catalog"
        );
        Ok(applied)
    }

    /// Fetch, validate and store the catalog, then apply its prices.
    ///
    /// A source failure is recorded on the settings row (and returned); the
    /// previous catalog stays stored and applied.
    pub async fn refresh(
        &self,
        trigger: RefreshTrigger,
    ) -> Result<ModelCatalogRefreshResult, ModelCatalogError> {
        let _flight = self
            .refresh_lock
            .try_lock()
            .map_err(|_| ModelCatalogError::RefreshInProgress)?;
        let started = Instant::now();
        let row = Self::ensure_settings_row(self.db.as_ref()).await?;
        let url = row
            .source_url
            .clone()
            .unwrap_or_else(|| DEFAULT_CATALOG_URL.to_string());
        let attempt_at = Utc::now();
        let mut attempt: ai_model_catalog_settings::ActiveModel = row.into();
        attempt.last_attempt_at = Set(Some(attempt_at));
        attempt.update(self.db.as_ref()).await?;

        let fetched = async {
            let parsed_url = source::validate_source_url(&url)?;
            let body = source::fetch_catalog(&self.client, &parsed_url).await?;
            source::parse_catalog(&url, &body)
        }
        .await;
        let parsed = match fetched {
            Ok(parsed) => parsed,
            Err(error) => {
                tracing::warn!(
                    trigger = trigger.as_str(),
                    source_url = %url,
                    %error,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "AI model catalog refresh failed; the previous catalog stays in use"
                );
                if error.is_source_failure()
                    || matches!(error, ModelCatalogError::InvalidSourceUrl { .. })
                {
                    let row = Self::ensure_settings_row(self.db.as_ref()).await?;
                    let mut failed: ai_model_catalog_settings::ActiveModel = row.into();
                    failed.last_error = Set(Some(truncate_error(&error)));
                    failed.update(self.db.as_ref()).await?;
                }
                return Err(error);
            }
        };

        let (saved, applied_prices) = self.store_and_apply(&parsed, &url).await?;
        tracing::info!(
            trigger = trigger.as_str(),
            source_url = %url,
            stored = parsed.models.len(),
            applied_prices,
            skipped = parsed.skipped,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "AI model catalog refreshed"
        );
        Ok(ModelCatalogRefreshResult {
            stored: parsed.models.len(),
            applied_prices,
            skipped: parsed.skipped,
            status: ModelCatalogStatus {
                refreshing: false,
                ..self.status_of(&saved)
            },
        })
    }

    /// Replace the stored catalog with `parsed` in one transaction, record
    /// the success, then apply its prices. Returns the settings row and how
    /// many prices were applied. On a database error nothing changes: the
    /// transaction rolls back and the previous prices stay applied.
    pub(crate) async fn store_and_apply(
        &self,
        parsed: &ParsedCatalog,
        url: &str,
    ) -> Result<(ai_model_catalog_settings::Model, usize), ModelCatalogError> {
        let fetched_at = Utc::now();
        let txn = self.db.begin().await?;
        ai_model_catalog_entries::Entity::delete_many()
            .exec(&txn)
            .await?;
        for batch in parsed.models.chunks(INSERT_BATCH) {
            ai_model_catalog_entries::Entity::insert_many(
                batch.iter().map(|model| entry_row(model, url, fetched_at)),
            )
            .exec_without_returning(&txn)
            .await?;
        }
        let row = Self::ensure_settings_row(&txn).await?;
        let mut done: ai_model_catalog_settings::ActiveModel = row.into();
        done.last_refreshed_at = Set(Some(fetched_at));
        done.last_source_url = Set(Some(url.to_string()));
        done.last_error = Set(None);
        done.entry_count = Set(i32::try_from(parsed.models.len()).unwrap_or(i32::MAX));
        done.skipped_count = Set(i32::try_from(parsed.skipped).unwrap_or(i32::MAX));
        let saved = done.update(&txn).await?;
        txn.commit().await?;

        let prices: Vec<pricing::CatalogPrice> = parsed
            .models
            .iter()
            .filter_map(CatalogModel::price)
            .collect();
        Ok((saved, pricing::install_catalog_prices(&prices)))
    }

    /// Rediscover every active provider key's models, one key at a time.
    async fn discover_provider_models(&self) -> Result<(), ModelCatalogError> {
        let keys = match self.provider_key_service.list_active().await {
            Ok(keys) => keys,
            Err(error) => {
                tracing::warn!(%error, "Could not list provider keys for AI model discovery");
                return Ok(());
            }
        };
        let mut refreshed = 0usize;
        for key in &keys {
            match tokio::time::timeout(
                DISCOVERY_TIMEOUT,
                self.provider_model_service.refresh(key.id),
            )
            .await
            {
                Ok(Ok(_)) => refreshed += 1,
                Ok(Err(error)) => tracing::warn!(
                    provider_key_id = key.id,
                    %error,
                    "Background AI model discovery failed for a provider key; its models are unchanged"
                ),
                Err(_) => tracing::warn!(
                    provider_key_id = key.id,
                    timeout_secs = DISCOVERY_TIMEOUT.as_secs(),
                    "Background AI model discovery timed out for a provider key"
                ),
            }
        }
        let row = Self::ensure_settings_row(self.db.as_ref()).await?;
        let mut done: ai_model_catalog_settings::ActiveModel = row.into();
        done.last_discovery_at = Set(Some(Utc::now()));
        done.update(self.db.as_ref()).await?;
        tracing::info!(
            keys = keys.len(),
            refreshed,
            "Background AI model discovery finished"
        );
        Ok(())
    }

    /// One pass of the background task: refresh and rediscover when due.
    async fn run_due_work(&self) {
        let row = match self.settings_row().await {
            Ok(row) => row,
            Err(error) => {
                tracing::warn!(%error, "Could not read AI model catalog settings");
                return;
            }
        };
        let now = Utc::now();
        if refresh_due(&row, now) {
            match self.refresh(RefreshTrigger::Scheduled).await {
                Ok(_) | Err(ModelCatalogError::RefreshInProgress) => {}
                // Already logged and recorded by `refresh`.
                Err(error) if error.is_source_failure() => {}
                Err(error) => {
                    tracing::warn!(%error, "Scheduled AI model catalog refresh failed")
                }
            }
        }
        if discovery_due(&row, now, self.booted_at) {
            if let Err(error) = self.discover_provider_models().await {
                tracing::warn!(%error, "Could not record AI model discovery");
            }
        }
    }

    /// Apply the stored catalog, then check for due work every 15 minutes.
    /// The returned handle is the only one; aborting it stops the task.
    pub fn spawn_background(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(error) = self.load_stored().await {
                tracing::warn!(
                    %error,
                    "Could not apply the stored AI model catalog; the bundled price table is used"
                );
            }
            tokio::time::sleep(STARTUP_DELAY).await;
            let mut ticker = tokio::time::interval(LOOP_TICK);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                self.run_due_work().await;
            }
        })
    }
}

#[cfg(test)]
mod tests;
