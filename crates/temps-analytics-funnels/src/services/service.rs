use chrono::{Duration, Utc};
use sea_orm::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;
use temps_core::UtcDateTime;
use temps_entities::{funnel_steps, funnels};
use tokio::sync::RwLock;

// ============================================================================
// Caching Infrastructure
// ============================================================================

/// Cache entry with expiration time
#[derive(Clone, Debug)]
struct CacheEntry<T> {
    value: T,
    expires_at: chrono::DateTime<Utc>,
}

impl<T> CacheEntry<T> {
    fn new(value: T, ttl_minutes: i64) -> Self {
        Self {
            value,
            expires_at: Utc::now() + Duration::minutes(ttl_minutes),
        }
    }

    fn is_expired(&self) -> bool {
        Utc::now() > self.expires_at
    }
}

/// Cache key for funnel metrics
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FunnelMetricsCacheKey {
    pub funnel_id: i32,
    pub environment_id: Option<i32>,
    pub country_code: Option<String>,
    pub start_date: Option<i64>, // Unix timestamp for hashing
    pub end_date: Option<i64>,
}

impl FunnelMetricsCacheKey {
    pub fn from_filter(funnel_id: i32, filter: &FunnelFilter) -> Self {
        Self {
            funnel_id,
            environment_id: filter.environment_id,
            country_code: filter.country_code.clone(),
            start_date: filter.start_date.map(|d| d.timestamp()),
            end_date: filter.end_date.map(|d| d.timestamp()),
        }
    }
}

/// Maximum number of entries in the metrics cache.
/// This prevents unbounded memory growth from many unique funnel/filter combinations.
const DEFAULT_MAX_CACHE_ENTRIES: usize = 1000;

/// Generic TTL-based cache with bounded capacity.
/// When the cache exceeds `max_capacity`, expired entries are evicted first,
/// then oldest entries are removed to make room.
pub struct MetricsCache<K, V>
where
    K: Eq + Hash + Clone,
    V: Clone,
{
    cache: Arc<RwLock<HashMap<K, CacheEntry<V>>>>,
    default_ttl_minutes: i64,
    max_capacity: usize,
}

impl<K, V> MetricsCache<K, V>
where
    K: Eq + Hash + Clone,
    V: Clone,
{
    pub fn new(default_ttl_minutes: i64) -> Self {
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
            default_ttl_minutes,
            max_capacity: DEFAULT_MAX_CACHE_ENTRIES,
        }
    }

    pub async fn get(&self, key: &K) -> Option<V> {
        let cache = self.cache.read().await;
        if let Some(entry) = cache.get(key) {
            if !entry.is_expired() {
                return Some(entry.value.clone());
            }
        }
        None
    }

    pub async fn set(&self, key: K, value: V) {
        let mut cache = self.cache.write().await;

        // Evict expired entries if at capacity
        if cache.len() >= self.max_capacity {
            cache.retain(|_, entry| !entry.is_expired());
        }

        // If still at capacity after expiry cleanup, remove oldest entries
        if cache.len() >= self.max_capacity {
            let to_remove = cache.len() - self.max_capacity + 1;
            // Find the entries with the earliest expiration times
            let mut entries_by_expiry: Vec<(K, chrono::DateTime<Utc>)> = cache
                .iter()
                .map(|(k, entry)| (k.clone(), entry.expires_at))
                .collect();
            entries_by_expiry.sort_by_key(|(_, expires_at)| *expires_at);

            for (k, _) in entries_by_expiry.into_iter().take(to_remove) {
                cache.remove(&k);
            }
        }

        cache.insert(key, CacheEntry::new(value, self.default_ttl_minutes));
    }

    pub async fn invalidate(&self, key: &K) {
        let mut cache = self.cache.write().await;
        cache.remove(key);
    }

    pub async fn invalidate_by_funnel(&self, funnel_id: i32)
    where
        K: std::fmt::Debug,
    {
        let mut cache = self.cache.write().await;
        // Remove all entries for this funnel (checks if key contains funnel_id)
        cache.retain(|k, _| {
            let k_str = format!("{:?}", k);
            !k_str.contains(&format!("funnel_id: {}", funnel_id))
        });
    }

    pub async fn cleanup_expired(&self) {
        let mut cache = self.cache.write().await;
        cache.retain(|_, entry| !entry.is_expired());
    }
}

// ============================================================================
// Domain Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunnelMetrics {
    pub funnel_id: i32,
    pub funnel_name: String,
    pub total_entries: u64,
    pub step_conversions: Vec<StepConversion>,
    pub overall_conversion_rate: f64,
    pub average_completion_time_seconds: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepConversion {
    pub step_id: i32,
    pub step_name: String,
    pub step_order: i32,
    pub completions: u64,
    pub conversion_rate: f64, // Percentage of previous step that completed this step (0 for step 1)
    pub drop_off_rate: f64,   // 0 for step 1 (N/A)
    pub average_time_to_complete_seconds: f64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateFunnelRequest {
    pub name: String,
    pub description: Option<String>,
    pub steps: Vec<CreateFunnelStep>,
}

/// Smart filter presets for common funnel patterns
#[derive(Debug, Serialize, Deserialize, Clone, utoipa::ToSchema)]
#[serde(rename_all = "snake_case", tag = "type", content = "value")]
pub enum SmartFilter {
    /// Match specific page path
    PagePath(String),
    /// Match specific hostname
    Hostname(String),
    /// Match UTM source
    UtmSource(String),
    /// Match UTM campaign
    UtmCampaign(String),
    /// Match UTM medium
    UtmMedium(String),
    /// Match referrer hostname
    ReferrerHostname(String),
    /// Match specific channel (organic, paid, direct, referral, etc.)
    Channel(String),
    /// Match device type (mobile, desktop, tablet)
    DeviceType(String),
    /// Match browser
    Browser(String),
    /// Match operating system
    OperatingSystem(String),
    /// Match language
    Language(String),
    /// Match custom event_data by JSON path
    /// Format: {"path": "user.plan", "value": "premium"}
    /// This will match events where event_data->'user'->>'plan' = 'premium'
    CustomData { path: String, value: String },
}

impl SmartFilter {
    /// Convert smart filter to column name and value for simple filters
    /// Returns None for CustomData which requires special JSON path handling
    pub fn to_condition(&self) -> Option<(&str, String)> {
        match self {
            SmartFilter::PagePath(path) => Some(("pathname", path.clone())),
            SmartFilter::Hostname(host) => Some(("hostname", host.clone())),
            SmartFilter::UtmSource(source) => Some(("utm_source", source.clone())),
            SmartFilter::UtmCampaign(campaign) => Some(("utm_campaign", campaign.clone())),
            SmartFilter::UtmMedium(medium) => Some(("utm_medium", medium.clone())),
            SmartFilter::ReferrerHostname(referrer) => {
                Some(("referrer_hostname", referrer.clone()))
            }
            SmartFilter::Channel(channel) => Some(("channel", channel.clone())),
            SmartFilter::DeviceType(device) => Some(("device_type", device.clone())),
            SmartFilter::Browser(browser) => Some(("browser", browser.clone())),
            SmartFilter::OperatingSystem(os) => Some(("operating_system", os.clone())),
            SmartFilter::Language(lang) => Some(("language", lang.clone())),
            SmartFilter::CustomData { .. } => None, // Handled separately
        }
    }

    /// Generate SQL condition for JSON path queries (CustomData only)
    /// Returns SQL fragment like: event_data->'user'->>'plan' = 'premium'
    pub fn to_json_condition(&self) -> Option<String> {
        match self {
            SmartFilter::CustomData { path, value } => {
                // Split path by '.' to build JSON path query
                let parts: Vec<&str> = path.split('.').collect();
                if parts.is_empty() {
                    return None;
                }

                // Build JSON path: event_data::jsonb->'key1'->'key2'->>'key3'
                // Cast text to jsonb first since event_data is stored as text
                let mut json_path = "event_data::jsonb".to_string();

                for (i, part) in parts.iter().enumerate() {
                    // Validate part is safe (alphanumeric + underscore only)
                    if !part.chars().all(|c| c.is_alphanumeric() || c == '_') {
                        return None;
                    }

                    if i == parts.len() - 1 {
                        // Last element uses ->> to get text value
                        json_path.push_str(&format!("->>'{}'", part));
                    } else {
                        // Intermediate elements use -> to get JSON
                        json_path.push_str(&format!("->'{}'", part));
                    }
                }

                // Escape single quotes in value
                let escaped_value = value.replace('\'', "''");
                Some(format!("{} = '{}'", json_path, escaped_value))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateFunnelStep {
    pub event_name: String,

    /// Event filters - use predefined filter patterns
    /// Example: [{"type": "page_path", "value": "/"}, {"type": "utm_source", "value": "google"}]
    #[serde(default)]
    pub event_filter: Vec<SmartFilter>,
}

impl CreateFunnelStep {
    /// Serialize filters to JSON string for storage
    /// Stores both simple column filters and CustomData JSON path filters
    pub fn serialize_filters(&self) -> Option<String> {
        if self.event_filter.is_empty() {
            return None;
        }

        let mut map = serde_json::Map::new();

        // Add simple column filters
        for filter in &self.event_filter {
            if let Some((column, value)) = filter.to_condition() {
                map.insert(column.to_string(), Value::String(value));
            }
        }

        // Add CustomData filters under special key
        let custom_data_filters: Vec<serde_json::Value> = self
            .event_filter
            .iter()
            .filter_map(|f| {
                if let SmartFilter::CustomData { path, value } = f {
                    Some(serde_json::json!({
                        "path": path,
                        "value": value
                    }))
                } else {
                    None
                }
            })
            .collect();

        if !custom_data_filters.is_empty() {
            map.insert(
                "_custom_data".to_string(),
                Value::Array(custom_data_filters),
            );
        }

        serde_json::to_string(&map).ok()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FunnelFilter {
    pub project_id: Option<i32>,
    pub environment_id: Option<i32>,
    pub country_code: Option<String>,
    pub start_date: Option<UtcDateTime>,
    pub end_date: Option<UtcDateTime>,
}

/// Default cache TTL in minutes (5 minutes)
const METRICS_CACHE_TTL_MINUTES: i64 = 5;

pub struct FunnelService {
    db: Arc<DatabaseConnection>,
    metrics_cache: MetricsCache<FunnelMetricsCacheKey, FunnelMetrics>,
}

impl FunnelService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self {
            db,
            metrics_cache: MetricsCache::new(METRICS_CACHE_TTL_MINUTES),
        }
    }

    /// Create with custom cache TTL (for testing)
    pub fn with_cache_ttl(db: Arc<DatabaseConnection>, cache_ttl_minutes: i64) -> Self {
        Self {
            db,
            metrics_cache: MetricsCache::new(cache_ttl_minutes),
        }
    }

    /// Invalidate cache for a specific funnel (call after funnel updates)
    pub async fn invalidate_funnel_cache(&self, funnel_id: i32) {
        self.metrics_cache.invalidate_by_funnel(funnel_id).await;
    }

    /// Cleanup expired cache entries (can be called periodically)
    pub async fn cleanup_cache(&self) {
        self.metrics_cache.cleanup_expired().await;
    }

    // Removed process_event_for_funnels_static - no longer needed

    /// List all active funnels for a project
    pub async fn list_funnels(&self, project_id: i32) -> Result<Vec<funnels::Model>, DbErr> {
        let db = self.db.as_ref();
        funnels::Entity::find()
            .filter(funnels::Column::ProjectId.eq(project_id))
            .filter(funnels::Column::IsActive.eq(true))
            .order_by_asc(funnels::Column::CreatedAt)
            .all(db)
            .await
    }

    /// Update an existing funnel
    pub async fn update_funnel(
        &self,
        project_id: i32,
        funnel_id: i32,
        request: CreateFunnelRequest,
    ) -> Result<(), DbErr> {
        let db = self.db.as_ref();

        // Find the funnel and verify it belongs to the project
        let funnel = funnels::Entity::find_by_id(funnel_id)
            .filter(funnels::Column::ProjectId.eq(project_id))
            .one(db)
            .await?
            .ok_or_else(|| DbErr::RecordNotFound("Funnel not found".to_string()))?;

        // Update funnel
        let mut funnel: funnels::ActiveModel = funnel.into();
        funnel.name = Set(request.name);
        funnel.description = Set(request.description);
        funnel.updated_at = Set(Utc::now());
        funnel.update(db).await?;

        // Delete existing steps
        funnel_steps::Entity::delete_many()
            .filter(funnel_steps::Column::FunnelId.eq(funnel_id))
            .exec(db)
            .await?;

        // Create new steps
        for (index, step) in request.steps.iter().enumerate() {
            let step_model = funnel_steps::ActiveModel {
                funnel_id: Set(funnel_id),
                step_order: Set(index as i32 + 1),
                event_name: Set(step.event_name.clone()),
                event_filter: Set(step.serialize_filters()),
                created_at: Set(Utc::now()),
                ..Default::default()
            };
            funnel_steps::Entity::insert(step_model).exec(db).await?;
        }

        // Invalidate cache for this funnel
        self.invalidate_funnel_cache(funnel_id).await;

        Ok(())
    }

    /// Delete a funnel (soft delete)
    pub async fn delete_funnel(&self, project_id: i32, funnel_id: i32) -> Result<(), DbErr> {
        let db = self.db.as_ref();

        // Find the funnel and verify it belongs to the project
        let funnel = funnels::Entity::find_by_id(funnel_id)
            .filter(funnels::Column::ProjectId.eq(project_id))
            .one(db)
            .await?
            .ok_or_else(|| DbErr::RecordNotFound("Funnel not found".to_string()))?;

        // Soft delete by setting is_active to false
        let mut funnel: funnels::ActiveModel = funnel.into();
        funnel.is_active = Set(false);
        funnel.updated_at = Set(Utc::now());
        funnel.update(db).await?;

        // Invalidate cache for this funnel
        self.invalidate_funnel_cache(funnel_id).await;

        Ok(())
    }

    /// Create a new funnel with steps
    pub async fn create_funnel(
        &self,
        project_id: i32,
        request: CreateFunnelRequest,
    ) -> Result<i32, DbErr> {
        let db = self.db.as_ref();

        // Create funnel without transaction
        let funnel = funnels::ActiveModel {
            project_id: Set(project_id),
            name: Set(request.name),
            description: Set(request.description),
            is_active: Set(true),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };

        let funnel_result = funnels::Entity::insert(funnel).exec(db).await?;
        let funnel_id = funnel_result.last_insert_id;

        // Create funnel steps
        for (index, step) in request.steps.iter().enumerate() {
            let step_model = funnel_steps::ActiveModel {
                funnel_id: Set(funnel_id),
                step_order: Set(index as i32 + 1),
                event_name: Set(step.event_name.clone()),
                event_filter: Set(step.serialize_filters()),
                created_at: Set(Utc::now()),
                ..Default::default()
            };

            funnel_steps::Entity::insert(step_model).exec(db).await?;
        }

        Ok(funnel_id)
    }

    /// Get unique event types for a project with pagination
    /// Returns (events with counts, total count) ordered by count descending
    pub async fn get_unique_events(
        &self,
        project_id: i32,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<(String, i64)>, u64), DbErr> {
        let db = self.db.as_ref();

        let page = page.unwrap_or(1).max(1);
        let page_size = page_size.unwrap_or(50).min(100); // Default 50, max 100
        let offset = (page - 1) * page_size;

        // Query to get total count of unique events
        let count_sql = r#"
            SELECT COUNT(DISTINCT COALESCE(event_name, event_type)) as total
            FROM events
            WHERE project_id = $1
        "#;

        let count_result = db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                count_sql,
                vec![project_id.into()],
            ))
            .await?;

        let total: i64 = count_result
            .map(|r| r.try_get("", "total").unwrap_or(0))
            .unwrap_or(0);

        // Query to get paginated unique events
        let sql = r#"
            SELECT
                COALESCE(event_name, event_type) as event,
                COUNT(*) as count
            FROM events
            WHERE project_id = $1
            GROUP BY COALESCE(event_name, event_type)
            ORDER BY count DESC, event ASC
            LIMIT $2 OFFSET $3
        "#;

        let results = db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                vec![
                    project_id.into(),
                    (page_size as i64).into(),
                    (offset as i64).into(),
                ],
            ))
            .await?;

        let mut events = Vec::new();
        for row in results {
            let event_name: String = row.try_get("", "event")?;
            let count: i64 = row.try_get("", "count")?;
            events.push((event_name, count));
        }

        Ok((events, total as u64))
    }

    /// Preview funnel metrics without saving the funnel
    pub async fn preview_funnel_metrics(
        &self,
        project_id: i32,
        request: CreateFunnelRequest,
        filter: FunnelFilter,
    ) -> Result<FunnelMetrics, DbErr> {
        // Convert request steps to preview step format (without IDs)
        let preview_steps: Vec<(i32, String, Option<String>)> = request
            .steps
            .iter()
            .enumerate()
            .map(|(index, step)| {
                (
                    (index + 1) as i32, // step_order
                    step.event_name.clone(),
                    step.serialize_filters(),
                )
            })
            .collect();

        // Calculate metrics using the preview steps
        self.calculate_funnel_metrics_internal(
            0, // funnel_id = 0 for preview
            &request.name,
            project_id,
            &preview_steps,
            filter,
        )
        .await
    }

    /// Get funnel metrics by querying events directly (with caching)
    pub async fn get_funnel_metrics(
        &self,
        funnel_id: i32,
        filter: FunnelFilter,
    ) -> Result<FunnelMetrics, DbErr> {
        // Check cache first
        let cache_key = FunnelMetricsCacheKey::from_filter(funnel_id, &filter);
        if let Some(cached) = self.metrics_cache.get(&cache_key).await {
            tracing::debug!("Cache hit for funnel {} metrics", funnel_id);
            return Ok(cached);
        }

        tracing::debug!(
            "Cache miss for funnel {} metrics, calculating...",
            funnel_id
        );

        let db = self.db.as_ref();

        // Get funnel and its steps
        let funnel = funnels::Entity::find_by_id(funnel_id)
            .one(db)
            .await?
            .ok_or_else(|| DbErr::RecordNotFound("Funnel not found".to_string()))?;

        let steps = funnel_steps::Entity::find()
            .filter(funnel_steps::Column::FunnelId.eq(funnel_id))
            .order_by_asc(funnel_steps::Column::StepOrder)
            .all(db)
            .await?;

        // Convert steps to internal format
        let steps_data: Vec<(i32, String, Option<String>)> = steps
            .iter()
            .map(|s| (s.step_order, s.event_name.clone(), s.event_filter.clone()))
            .collect();

        // Calculate metrics using the internal method
        let metrics = self
            .calculate_funnel_metrics_internal(
                funnel_id,
                &funnel.name,
                funnel.project_id,
                &steps_data,
                filter,
            )
            .await?;

        // Store in cache
        self.metrics_cache.set(cache_key, metrics.clone()).await;

        Ok(metrics)
    }

    /// Internal method to calculate funnel metrics using database-side aggregation
    /// Uses a single SQL query with CTEs and window functions for efficiency
    async fn calculate_funnel_metrics_internal(
        &self,
        funnel_id: i32,
        funnel_name: &str,
        project_id: i32,
        steps_data: &[(i32, String, Option<String>)], // (step_order, event_name, event_filter)
        filter: FunnelFilter,
    ) -> Result<FunnelMetrics, DbErr> {
        let db = self.db.as_ref();

        if steps_data.is_empty() {
            return Ok(FunnelMetrics {
                funnel_id,
                funnel_name: funnel_name.to_string(),
                total_entries: 0,
                step_conversions: vec![],
                overall_conversion_rate: 0.0,
                average_completion_time_seconds: 0.0,
            });
        }

        // Build a single SQL query with CTEs for each step
        // This avoids N+1 queries and keeps all data in the database
        let (query, values) =
            self.build_funnel_aggregation_query(project_id, steps_data, &filter)?;

        tracing::debug!("Funnel aggregation query: {}", query);

        // Execute the aggregation query
        let result = db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &query,
                values,
            ))
            .await?;

        // Parse results and build metrics
        self.parse_funnel_results(funnel_id, funnel_name, steps_data, result)
    }

    /// Build the SQL query for funnel aggregation using CTEs
    /// Returns (query_string, parameter_values)
    fn build_funnel_aggregation_query(
        &self,
        project_id: i32,
        steps_data: &[(i32, String, Option<String>)],
        filter: &FunnelFilter,
    ) -> Result<(String, Vec<sea_orm::Value>), DbErr> {
        let mut values: Vec<sea_orm::Value> = vec![project_id.into()];
        let mut param_index = 2;

        // Build global filter conditions
        let mut global_conditions = Vec::new();
        if let Some(env_id) = filter.environment_id {
            global_conditions.push(format!("environment_id = ${}", param_index));
            values.push(env_id.into());
            param_index += 1;
        }
        if let Some(start_date) = filter.start_date {
            global_conditions.push(format!("timestamp >= ${}", param_index));
            values.push(start_date.into());
            param_index += 1;
        }
        if let Some(end_date) = filter.end_date {
            global_conditions.push(format!("timestamp <= ${}", param_index));
            values.push(end_date.into());
            param_index += 1;
        }
        // Add country_code filter via ip_geolocations join
        if let Some(ref country_code) = filter.country_code {
            global_conditions.push(format!(
                "ip_geolocation_id IN (SELECT id FROM ip_geolocations WHERE country_code = ${})",
                param_index
            ));
            values.push(country_code.clone().into());
            param_index += 1;
        }

        let global_filter = if global_conditions.is_empty() {
            String::new()
        } else {
            format!(" AND {}", global_conditions.join(" AND "))
        };

        // Build CTEs for each step
        let mut ctes = Vec::new();

        for (step_idx, (_step_order, event_name, event_filter_opt)) in steps_data.iter().enumerate()
        {
            let step_num = step_idx + 1;
            let event_param_idx = param_index;
            values.push(event_name.clone().into());
            param_index += 1;

            // Build step-specific filter conditions
            let step_filters =
                self.build_step_filter_conditions(event_filter_opt, &mut values, &mut param_index);

            let cte = format!(
                r#"step{step_num}_events AS (
    SELECT
        session_id,
        MIN(timestamp) as first_timestamp
    FROM events
    WHERE project_id = $1
      AND COALESCE(event_name, event_type) = ${event_param_idx}
      AND session_id IS NOT NULL
      {global_filter}
      {step_filters}
    GROUP BY session_id
)"#,
                step_num = step_num,
                event_param_idx = event_param_idx,
                global_filter = global_filter,
                step_filters = step_filters
            );
            ctes.push(cte);
        }

        // Build the funnel join query
        // Each step is joined with temporal ordering (step N must happen after step N-1)
        let funnel_join = self.build_funnel_join_query(steps_data.len());

        // Build the aggregation query
        let aggregation = self.build_aggregation_select(steps_data.len());

        let query = format!(
            "WITH {ctes},\n{funnel_join}\n{aggregation}",
            ctes = ctes.join(",\n"),
            funnel_join = funnel_join,
            aggregation = aggregation
        );

        Ok((query, values))
    }

    /// Build step-specific filter conditions from event_filter JSON
    fn build_step_filter_conditions(
        &self,
        event_filter_opt: &Option<String>,
        values: &mut Vec<sea_orm::Value>,
        param_index: &mut usize,
    ) -> String {
        let mut conditions = Vec::new();

        if let Some(event_filter_str) = event_filter_opt {
            if let Ok(filter_obj) =
                serde_json::from_str::<serde_json::Map<String, Value>>(event_filter_str)
            {
                for (key, value) in filter_obj.iter() {
                    // Handle CustomData filters
                    if key == "_custom_data" {
                        if let Value::Array(custom_filters) = value {
                            for custom_filter in custom_filters {
                                if let (Some(path), Some(filter_value)) = (
                                    custom_filter.get("path").and_then(|v| v.as_str()),
                                    custom_filter.get("value").and_then(|v| v.as_str()),
                                ) {
                                    let smart_filter = SmartFilter::CustomData {
                                        path: path.to_string(),
                                        value: filter_value.to_string(),
                                    };
                                    if let Some(json_condition) = smart_filter.to_json_condition() {
                                        conditions.push(format!("AND {}", json_condition));
                                    }
                                }
                            }
                        }
                        continue;
                    }

                    // Validate column name
                    let allowed_columns = [
                        "pathname",
                        "hostname",
                        "page_path",
                        "referrer",
                        "referrer_hostname",
                        "utm_source",
                        "utm_medium",
                        "utm_campaign",
                        "utm_term",
                        "utm_content",
                        "channel",
                        "device_type",
                        "browser",
                        "operating_system",
                        "language",
                    ];

                    if !allowed_columns.contains(&key.as_str()) {
                        tracing::warn!("Invalid filter column '{}' in funnel step, skipping", key);
                        continue;
                    }

                    // Build parameterized condition
                    match value {
                        Value::String(s) => {
                            conditions.push(format!("AND {} = ${}", key, param_index));
                            values.push(s.as_str().into());
                            *param_index += 1;
                        }
                        Value::Number(n) => {
                            conditions.push(format!("AND {} = ${}", key, param_index));
                            if let Some(n_i64) = n.as_i64() {
                                values.push(n_i64.into());
                            } else if let Some(n_f64) = n.as_f64() {
                                values.push(n_f64.into());
                            }
                            *param_index += 1;
                        }
                        Value::Bool(b) => {
                            conditions.push(format!("AND {} = ${}", key, param_index));
                            values.push((*b).into());
                            *param_index += 1;
                        }
                        Value::Null => {
                            conditions.push(format!("AND {} IS NULL", key));
                        }
                        _ => {
                            tracing::warn!(
                                "Unsupported filter value type for column '{}', skipping",
                                key
                            );
                        }
                    }
                }
            }
        }

        conditions.join("\n      ")
    }

    /// Build the funnel join CTE that combines all steps with temporal ordering
    fn build_funnel_join_query(&self, num_steps: usize) -> String {
        if num_steps == 1 {
            return r#"funnel_sessions AS (
    SELECT
        s1.session_id,
        s1.first_timestamp as step1_time
    FROM step1_events s1
)"#
            .to_string();
        }

        let mut select_cols = vec!["s1.session_id".to_string()];
        let mut joins = Vec::new();

        for i in 1..=num_steps {
            select_cols.push(format!("s{}.first_timestamp as step{}_time", i, i));
        }

        // Build LEFT JOINs with temporal ordering
        for i in 2..=num_steps {
            joins.push(format!(
                "LEFT JOIN step{i}_events s{i} ON s1.session_id = s{i}.session_id AND s{i}.first_timestamp >= s{prev}.first_timestamp",
                i = i,
                prev = i - 1
            ));
        }

        format!(
            r#"funnel_sessions AS (
    SELECT
        {select_cols}
    FROM step1_events s1
    {joins}
)"#,
            select_cols = select_cols.join(",\n        "),
            joins = joins.join("\n    ")
        )
    }

    /// Build the final aggregation SELECT statement
    fn build_aggregation_select(&self, num_steps: usize) -> String {
        let mut select_parts = vec![
            "COUNT(*) as total_entries".to_string(),
            "COUNT(step1_time) as step1_completions".to_string(),
        ];

        // Add completion counts for each step
        for i in 2..=num_steps {
            select_parts.push(format!("COUNT(step{}_time) as step{}_completions", i, i));
        }

        // Add average time between consecutive steps
        for i in 2..=num_steps {
            select_parts.push(format!(
                "AVG(EXTRACT(EPOCH FROM (step{i}_time - step{prev}_time))) as avg_time_{prev}_to_{i}",
                i = i,
                prev = i - 1
            ));
        }

        // Add total funnel completion time (first to last step)
        if num_steps > 1 {
            select_parts.push(format!(
                "AVG(EXTRACT(EPOCH FROM (step{}_time - step1_time))) as avg_total_time",
                num_steps
            ));
        }

        format!(
            "SELECT \n    {}\nFROM funnel_sessions",
            select_parts.join(",\n    ")
        )
    }

    /// Parse the aggregation query results into FunnelMetrics
    fn parse_funnel_results(
        &self,
        funnel_id: i32,
        funnel_name: &str,
        steps_data: &[(i32, String, Option<String>)],
        result: Option<QueryResult>,
    ) -> Result<FunnelMetrics, DbErr> {
        let result = match result {
            Some(r) => r,
            None => {
                return Ok(FunnelMetrics {
                    funnel_id,
                    funnel_name: funnel_name.to_string(),
                    total_entries: 0,
                    step_conversions: vec![],
                    overall_conversion_rate: 0.0,
                    average_completion_time_seconds: 0.0,
                });
            }
        };

        let total_entries: i64 = result.try_get("", "total_entries").unwrap_or(0);
        let num_steps = steps_data.len();

        let mut step_conversions = Vec::new();
        let mut previous_completions = total_entries as u64;

        for (step_idx, (step_order, event_name, _)) in steps_data.iter().enumerate() {
            let step_num = step_idx + 1;
            let completions: i64 = result
                .try_get("", &format!("step{}_completions", step_num))
                .unwrap_or(0);
            let completions = completions as u64;

            // Step 1: conversion rate = completions / total_entries (how many entries completed step 1)
            // Subsequent steps: conversion rate = completions / previous_step_completions
            let (conversion_rate, drop_off_rate) = if step_num == 1 {
                if total_entries > 0 {
                    let rate = (completions as f64 / total_entries as f64) * 100.0;
                    (rate, 100.0 - rate)
                } else {
                    (0.0, 0.0)
                }
            } else if previous_completions > 0 {
                let rate = (completions as f64 / previous_completions as f64) * 100.0;
                (rate, 100.0 - rate)
            } else {
                (0.0, 100.0)
            };

            // Get average time to complete this step (from previous step)
            let avg_time = if step_num > 1 {
                result
                    .try_get::<Option<f64>>(
                        "",
                        &format!("avg_time_{}_to_{}", step_num - 1, step_num),
                    )
                    .unwrap_or(None)
                    .unwrap_or(0.0)
            } else {
                0.0
            };

            step_conversions.push(StepConversion {
                step_id: 0,
                step_name: event_name.clone(),
                step_order: *step_order,
                completions,
                conversion_rate,
                drop_off_rate,
                average_time_to_complete_seconds: avg_time,
            });

            previous_completions = completions;
        }

        // Get final step completions for overall conversion
        let final_completions = step_conversions.last().map(|s| s.completions).unwrap_or(0);

        let overall_conversion_rate = if total_entries > 0 {
            (final_completions as f64 / total_entries as f64) * 100.0
        } else {
            0.0
        };

        // Get average total completion time
        let average_completion_time_seconds = if num_steps > 1 {
            result
                .try_get::<Option<f64>>("", "avg_total_time")
                .unwrap_or(None)
                .unwrap_or(0.0)
        } else {
            0.0
        };

        Ok(FunnelMetrics {
            funnel_id,
            funnel_name: funnel_name.to_string(),
            total_entries: total_entries as u64,
            step_conversions,
            overall_conversion_rate,
            average_completion_time_seconds,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_database::test_utils::TestDatabase;
    use temps_entities::{
        deployments, environments, events, projects, upstream_config::UpstreamList,
    };

    async fn create_test_project(db: Arc<DatabaseConnection>) -> (i32, i32, i32) {
        // Create project
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            slug: Set("test-project-no-git".to_string()),
            repo_owner: Set("test_project".to_string()),
            repo_name: Set("test_project".to_string()),
            preset: Set(temps_entities::preset::Preset::NextJs),
            directory: Set("/".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            main_branch: Set("main".to_string()),
            is_deleted: Set(false),
            is_public_repo: Set(false),
            ..Default::default()
        };
        let project_result = projects::Entity::insert(project)
            .exec(db.as_ref())
            .await
            .unwrap();
        let project_id = project_result.last_insert_id;

        // Create environment (required since environment_id is NOT NULL in events)
        let environment = environments::ActiveModel {
            project_id: Set(project_id),
            name: Set("test".to_string()),
            slug: Set("test".to_string()),
            subdomain: Set("test.temps.localhost".to_string()),
            host: Set("localhost".to_string()),
            upstreams: Set(UpstreamList::default()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let env_result = environments::Entity::insert(environment)
            .exec(db.as_ref())
            .await
            .unwrap();
        let environment_id = env_result.last_insert_id;

        // Create deployment (required since deployment_id is NOT NULL in events)
        let deployment = deployments::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(environment_id),
            commit_sha: Set(Some("test123".to_string())),
            commit_message: Set(Some("Test commit".to_string())),
            slug: Set("http://test.temps.localhost".to_string()),
            state: Set("active".to_string()),
            metadata: Set(Some(
                temps_entities::deployments::DeploymentMetadata::default(),
            )),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };
        let deployment_result = deployments::Entity::insert(deployment)
            .exec(db.as_ref())
            .await
            .unwrap();
        let deployment_id = deployment_result.last_insert_id;

        (project_id, environment_id, deployment_id)
    }

    #[tokio::test]
    async fn test_funnel_with_event_type() {
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");
        let db = test_db.db.clone();
        let (project_id, environment_id, deployment_id) = create_test_project(db.clone()).await;
        let service = FunnelService::new(db.clone());

        // Create a funnel with steps based on event_type
        let funnel_request = CreateFunnelRequest {
            name: "Login Funnel".to_string(),
            description: Some("Test funnel for login flow".to_string()),
            steps: vec![
                CreateFunnelStep {
                    event_name: "page_view".to_string(),
                    event_filter: vec![],
                },
                CreateFunnelStep {
                    event_name: "user_login".to_string(),
                    event_filter: vec![],
                },
            ],
        };

        let funnel_id = service
            .create_funnel(project_id, funnel_request)
            .await
            .expect("Failed to create funnel");

        // Create test events with event_type (not event_name)
        let now = Utc::now();

        // Session 1: Completed both steps
        let session_1 = "session_1";
        events::Entity::insert(events::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(Some(environment_id)),
            deployment_id: Set(Some(deployment_id)),
            session_id: Set(Some(session_1.to_string())),
            event_type: Set("page_view".to_string()),
            event_name: Set(None), // NULL - should still match via event_type
            timestamp: Set(now),
            hostname: Set("test.com".to_string()),
            pathname: Set("/".to_string()),
            page_path: Set("/".to_string()),
            href: Set("http://test.com".to_string()),
            ..Default::default()
        })
        .exec(db.as_ref())
        .await
        .expect("Failed to insert page_view event");

        events::Entity::insert(events::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(Some(environment_id)),
            deployment_id: Set(Some(deployment_id)),
            session_id: Set(Some(session_1.to_string())),
            event_type: Set("user_login".to_string()),
            event_name: Set(None), // NULL - should still match via event_type
            timestamp: Set(now + chrono::Duration::seconds(5)),
            hostname: Set("test.com".to_string()),
            pathname: Set("/login".to_string()),
            page_path: Set("/login".to_string()),
            href: Set("http://test.com/login".to_string()),
            ..Default::default()
        })
        .exec(db.as_ref())
        .await
        .expect("Failed to insert user_login event");

        // Session 2: Only completed first step
        let session_2 = "session_2";
        events::Entity::insert(events::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(Some(environment_id)),
            deployment_id: Set(Some(deployment_id)),
            session_id: Set(Some(session_2.to_string())),
            event_type: Set("page_view".to_string()),
            event_name: Set(None),
            timestamp: Set(now),
            hostname: Set("test.com".to_string()),
            pathname: Set("/".to_string()),
            page_path: Set("/".to_string()),
            href: Set("http://test.com".to_string()),
            ..Default::default()
        })
        .exec(db.as_ref())
        .await
        .expect("Failed to insert page_view event for session 2");

        // Get funnel metrics
        let metrics = service
            .get_funnel_metrics(
                funnel_id,
                FunnelFilter {
                    project_id: Some(project_id),
                    environment_id: None,
                    country_code: None,
                    start_date: None,
                    end_date: None,
                },
            )
            .await
            .expect("Failed to get funnel metrics");

        // Assertions
        assert_eq!(metrics.funnel_name, "Login Funnel");
        assert_eq!(
            metrics.total_entries, 2,
            "Should have 2 sessions entering the funnel"
        );
        assert_eq!(metrics.step_conversions.len(), 2, "Should have 2 steps");

        // First step: page_view
        let step1 = &metrics.step_conversions[0];
        assert_eq!(step1.step_name, "page_view");
        assert_eq!(
            step1.completions, 2,
            "Both sessions should complete page_view"
        );
        assert_eq!(
            step1.conversion_rate, 100.0,
            "100% conversion from entry to step 1"
        );

        // Second step: user_login
        let step2 = &metrics.step_conversions[1];
        assert_eq!(step2.step_name, "user_login");
        assert_eq!(
            step2.completions, 1,
            "Only 1 session should complete user_login"
        );
        assert_eq!(
            step2.conversion_rate, 50.0,
            "50% conversion from step 1 to step 2"
        );
        assert_eq!(step2.drop_off_rate, 50.0, "50% drop-off rate");

        // Overall conversion
        assert_eq!(
            metrics.overall_conversion_rate, 50.0,
            "Overall conversion should be 50%"
        );
    }

    #[tokio::test]
    async fn test_funnel_with_event_name() {
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");
        let db = test_db.db.clone();
        let (project_id, environment_id, deployment_id) = create_test_project(db.clone()).await;
        let service = FunnelService::new(db.clone());

        // Create a funnel with custom event names
        let funnel_request = CreateFunnelRequest {
            name: "Custom Events Funnel".to_string(),
            description: None,
            steps: vec![
                CreateFunnelStep {
                    event_name: "button_click".to_string(),
                    event_filter: vec![],
                },
                CreateFunnelStep {
                    event_name: "form_submit".to_string(),
                    event_filter: vec![],
                },
            ],
        };

        let funnel_id = service
            .create_funnel(project_id, funnel_request)
            .await
            .expect("Failed to create funnel");

        let now = Utc::now();
        let session_1 = "session_custom_1";

        // Create events with event_name set (custom events)
        events::Entity::insert(events::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(Some(environment_id)),
            deployment_id: Set(Some(deployment_id)),
            session_id: Set(Some(session_1.to_string())),
            event_type: Set("custom".to_string()),
            event_name: Set(Some("button_click".to_string())), // Custom event name
            timestamp: Set(now),
            hostname: Set("test.com".to_string()),
            pathname: Set("/".to_string()),
            page_path: Set("/".to_string()),
            href: Set("http://test.com".to_string()),
            ..Default::default()
        })
        .exec(db.as_ref())
        .await
        .expect("Failed to insert button_click event");

        events::Entity::insert(events::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(Some(environment_id)),
            deployment_id: Set(Some(deployment_id)),
            session_id: Set(Some(session_1.to_string())),
            event_type: Set("custom".to_string()),
            event_name: Set(Some("form_submit".to_string())), // Custom event name
            timestamp: Set(now + chrono::Duration::seconds(3)),
            hostname: Set("test.com".to_string()),
            pathname: Set("/".to_string()),
            page_path: Set("/".to_string()),
            href: Set("http://test.com".to_string()),
            ..Default::default()
        })
        .exec(db.as_ref())
        .await
        .expect("Failed to insert form_submit event");

        // Get funnel metrics
        let metrics = service
            .get_funnel_metrics(
                funnel_id,
                FunnelFilter {
                    project_id: Some(project_id),
                    environment_id: None,
                    country_code: None,
                    start_date: None,
                    end_date: None,
                },
            )
            .await
            .expect("Failed to get funnel metrics");

        // Assertions
        assert_eq!(metrics.total_entries, 1);
        assert_eq!(metrics.step_conversions[0].completions, 1);
        assert_eq!(metrics.step_conversions[1].completions, 1);
        assert_eq!(metrics.overall_conversion_rate, 100.0);
    }

    #[tokio::test]
    async fn test_funnel_with_mixed_events() {
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");
        let db = test_db.db.clone();
        let (project_id, environment_id, deployment_id) = create_test_project(db.clone()).await;
        let service = FunnelService::new(db.clone());

        // Create a funnel mixing built-in and custom events
        let funnel_request = CreateFunnelRequest {
            name: "Mixed Events Funnel".to_string(),
            description: None,
            steps: vec![
                CreateFunnelStep {
                    event_name: "page_view".to_string(), // Built-in (event_type)
                    event_filter: vec![],
                },
                CreateFunnelStep {
                    event_name: "signup_clicked".to_string(), // Custom (event_name)
                    event_filter: vec![],
                },
            ],
        };

        let funnel_id = service
            .create_funnel(project_id, funnel_request)
            .await
            .expect("Failed to create funnel");

        let now = Utc::now();
        let session_1 = "session_mixed_1";

        // Built-in event (event_type only)
        events::Entity::insert(events::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(Some(environment_id)),
            deployment_id: Set(Some(deployment_id)),
            session_id: Set(Some(session_1.to_string())),
            event_type: Set("page_view".to_string()),
            event_name: Set(None),
            timestamp: Set(now),
            hostname: Set("test.com".to_string()),
            pathname: Set("/".to_string()),
            page_path: Set("/".to_string()),
            href: Set("http://test.com".to_string()),
            ..Default::default()
        })
        .exec(db.as_ref())
        .await
        .expect("Failed to insert page_view event");

        // Custom event (event_name set)
        events::Entity::insert(events::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(Some(environment_id)),
            deployment_id: Set(Some(deployment_id)),
            session_id: Set(Some(session_1.to_string())),
            event_type: Set("custom".to_string()),
            event_name: Set(Some("signup_clicked".to_string())),
            timestamp: Set(now + chrono::Duration::seconds(2)),
            hostname: Set("test.com".to_string()),
            pathname: Set("/signup".to_string()),
            page_path: Set("/signup".to_string()),
            href: Set("http://test.com/signup".to_string()),
            ..Default::default()
        })
        .exec(db.as_ref())
        .await
        .expect("Failed to insert signup_clicked event");

        // Get funnel metrics
        let metrics = service
            .get_funnel_metrics(
                funnel_id,
                FunnelFilter {
                    project_id: Some(project_id),
                    environment_id: None,
                    country_code: None,
                    start_date: None,
                    end_date: None,
                },
            )
            .await
            .expect("Failed to get funnel metrics");

        // Assertions
        assert_eq!(metrics.total_entries, 1);
        assert_eq!(
            metrics.step_conversions[0].completions, 1,
            "page_view from event_type should match"
        );
        assert_eq!(
            metrics.step_conversions[1].completions, 1,
            "signup_clicked from event_name should match"
        );
        assert_eq!(metrics.overall_conversion_rate, 100.0);
    }

    #[tokio::test]
    async fn test_funnel_step_ordering() {
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");
        let db = test_db.db.clone();
        let (project_id, environment_id, deployment_id) = create_test_project(db.clone()).await;
        let service = FunnelService::new(db.clone());

        let funnel_request = CreateFunnelRequest {
            name: "Order Test Funnel".to_string(),
            description: None,
            steps: vec![
                CreateFunnelStep {
                    event_name: "step1".to_string(),
                    event_filter: vec![],
                },
                CreateFunnelStep {
                    event_name: "step2".to_string(),
                    event_filter: vec![],
                },
            ],
        };

        let funnel_id = service
            .create_funnel(project_id, funnel_request)
            .await
            .expect("Failed to create funnel");

        let now = Utc::now();
        let session_1 = "session_order_1";

        // Insert events in WRONG order (step2 before step1)
        events::Entity::insert(events::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(Some(environment_id)),
            deployment_id: Set(Some(deployment_id)),
            session_id: Set(Some(session_1.to_string())),
            event_type: Set("step2".to_string()),
            event_name: Set(None),
            timestamp: Set(now),
            hostname: Set("test.com".to_string()),
            pathname: Set("/".to_string()),
            page_path: Set("/".to_string()),
            href: Set("http://test.com".to_string()),
            ..Default::default()
        })
        .exec(db.as_ref())
        .await
        .expect("Failed to insert step2 event");

        events::Entity::insert(events::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(Some(environment_id)),
            deployment_id: Set(Some(deployment_id)),
            session_id: Set(Some(session_1.to_string())),
            event_type: Set("step1".to_string()),
            event_name: Set(None),
            timestamp: Set(now + chrono::Duration::seconds(5)),
            hostname: Set("test.com".to_string()),
            pathname: Set("/".to_string()),
            page_path: Set("/".to_string()),
            href: Set("http://test.com".to_string()),
            ..Default::default()
        })
        .exec(db.as_ref())
        .await
        .expect("Failed to insert step1 event");

        // Get funnel metrics
        let metrics = service
            .get_funnel_metrics(
                funnel_id,
                FunnelFilter {
                    project_id: Some(project_id),
                    environment_id: None,
                    country_code: None,
                    start_date: None,
                    end_date: None,
                },
            )
            .await
            .expect("Failed to get funnel metrics");

        // Should NOT count step2 because step1 happened after it
        assert_eq!(metrics.total_entries, 1, "Should have 1 entry (step1)");
        assert_eq!(metrics.step_conversions[0].completions, 1);
        assert_eq!(
            metrics.step_conversions[1].completions, 0,
            "step2 should not count because it happened before step1"
        );
        assert_eq!(metrics.overall_conversion_rate, 0.0);
    }

    #[tokio::test]
    async fn test_funnel_with_event_filters() {
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");
        let db = test_db.db.clone();
        let (project_id, environment_id, deployment_id) = create_test_project(db.clone()).await;
        let service = FunnelService::new(db.clone());

        // Create a funnel with smart filters
        let funnel_request = CreateFunnelRequest {
            name: "Homepage to Login Funnel".to_string(),
            description: Some("Track users from homepage to login".to_string()),
            steps: vec![
                CreateFunnelStep {
                    event_name: "page_view".to_string(),
                    event_filter: vec![SmartFilter::PagePath("/".to_string())],
                },
                CreateFunnelStep {
                    event_name: "page_view".to_string(),
                    event_filter: vec![SmartFilter::PagePath("/login".to_string())],
                },
            ],
        };

        let funnel_id = service
            .create_funnel(project_id, funnel_request)
            .await
            .expect("Failed to create funnel");

        let now = Utc::now();
        let session_1 = "session_filter_1";

        // Session 1: Visits homepage, then login page (should complete funnel)
        events::Entity::insert(events::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(Some(environment_id)),
            deployment_id: Set(Some(deployment_id)),
            session_id: Set(Some(session_1.to_string())),
            event_type: Set("page_view".to_string()),
            event_name: Set(None),
            pathname: Set("/".to_string()),
            timestamp: Set(now),
            hostname: Set("test.com".to_string()),
            page_path: Set("/".to_string()),
            href: Set("http://test.com".to_string()),
            ..Default::default()
        })
        .exec(db.as_ref())
        .await
        .expect("Failed to insert homepage view");

        events::Entity::insert(events::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(Some(environment_id)),
            deployment_id: Set(Some(deployment_id)),
            session_id: Set(Some(session_1.to_string())),
            event_type: Set("page_view".to_string()),
            event_name: Set(None),
            pathname: Set("/login".to_string()),
            timestamp: Set(now + chrono::Duration::seconds(10)),
            hostname: Set("test.com".to_string()),
            page_path: Set("/login".to_string()),
            href: Set("http://test.com/login".to_string()),
            ..Default::default()
        })
        .exec(db.as_ref())
        .await
        .expect("Failed to insert login page view");

        // Session 2: Visits homepage, then about page (should NOT complete funnel)
        let session_2 = "session_filter_2";
        events::Entity::insert(events::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(Some(environment_id)),
            deployment_id: Set(Some(deployment_id)),
            session_id: Set(Some(session_2.to_string())),
            event_type: Set("page_view".to_string()),
            event_name: Set(None),
            pathname: Set("/".to_string()),
            timestamp: Set(now),
            hostname: Set("test.com".to_string()),
            page_path: Set("/".to_string()),
            href: Set("http://test.com".to_string()),
            ..Default::default()
        })
        .exec(db.as_ref())
        .await
        .expect("Failed to insert homepage view for session 2");

        events::Entity::insert(events::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(Some(environment_id)),
            deployment_id: Set(Some(deployment_id)),
            session_id: Set(Some(session_2.to_string())),
            event_type: Set("page_view".to_string()),
            event_name: Set(None),
            pathname: Set("/about".to_string()),
            timestamp: Set(now + chrono::Duration::seconds(10)),
            hostname: Set("test.com".to_string()),
            page_path: Set("/about".to_string()),
            href: Set("http://test.com/about".to_string()),
            ..Default::default()
        })
        .exec(db.as_ref())
        .await
        .expect("Failed to insert about page view");

        // Get funnel metrics
        let metrics = service
            .get_funnel_metrics(
                funnel_id,
                FunnelFilter {
                    project_id: Some(project_id),
                    environment_id: None,
                    country_code: None,
                    start_date: None,
                    end_date: None,
                },
            )
            .await
            .expect("Failed to get funnel metrics");

        // Assertions
        assert_eq!(metrics.funnel_name, "Homepage to Login Funnel");
        assert_eq!(metrics.total_entries, 2, "Both sessions visited homepage");
        assert_eq!(metrics.step_conversions.len(), 2);

        // Step 1: homepage (pathname = "/")
        let step1 = &metrics.step_conversions[0];
        assert_eq!(step1.step_name, "page_view");
        assert_eq!(step1.completions, 2, "Both sessions visited homepage");
        assert_eq!(step1.conversion_rate, 100.0);

        // Step 2: login page (pathname = "/login")
        let step2 = &metrics.step_conversions[1];
        assert_eq!(step2.step_name, "page_view");
        assert_eq!(step2.completions, 1, "Only session_1 visited /login");
        assert_eq!(
            step2.conversion_rate, 50.0,
            "50% went from homepage to login"
        );

        // Overall conversion
        assert_eq!(metrics.overall_conversion_rate, 50.0);
    }

    // Helper function to create events
    #[allow(clippy::too_many_arguments)]
    async fn create_event(
        db: &DatabaseConnection,
        project_id: i32,
        environment_id: i32,
        deployment_id: i32,
        session_id: &str,
        event_type: &str,
        event_name: Option<&str>,
        pathname: &str,
        timestamp: UtcDateTime,
        utm_source: Option<&str>,
    ) {
        create_event_with_data(
            db,
            project_id,
            environment_id,
            deployment_id,
            session_id,
            event_type,
            event_name,
            pathname,
            timestamp,
            utm_source,
            None,
        )
        .await;
    }

    // Helper function to create events with custom event_data
    #[allow(clippy::too_many_arguments)]
    async fn create_event_with_data(
        db: &DatabaseConnection,
        project_id: i32,
        environment_id: i32,
        deployment_id: i32,
        session_id: &str,
        event_type: &str,
        event_name: Option<&str>,
        pathname: &str,
        timestamp: UtcDateTime,
        utm_source: Option<&str>,
        event_data: Option<serde_json::Value>,
    ) {
        events::Entity::insert(events::ActiveModel {
            project_id: Set(project_id),
            environment_id: Set(Some(environment_id)),
            deployment_id: Set(Some(deployment_id)),
            session_id: Set(Some(session_id.to_string())),
            event_type: Set(event_type.to_string()),
            event_name: Set(event_name.map(|s| s.to_string())),
            pathname: Set(pathname.to_string()),
            timestamp: Set(timestamp),
            hostname: Set("test.com".to_string()),
            page_path: Set(pathname.to_string()),
            href: Set(format!("http://test.com{}", pathname)),
            utm_source: Set(utm_source.map(|s| s.to_string())),
            event_data: Set(event_data.map(|v| serde_json::to_string(&v).unwrap())),
            ..Default::default()
        })
        .exec(db)
        .await
        .expect("Failed to insert event");
    }

    #[tokio::test]
    async fn test_multi_visitor_funnel_real_scenario() {
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");
        let db = test_db.db.clone();
        let (project_id, environment_id, deployment_id) = create_test_project(db.clone()).await;
        let service = FunnelService::new(db.clone());

        // Create a realistic signup funnel: homepage -> pricing -> signup
        let funnel_request = CreateFunnelRequest {
            name: "Signup Funnel".to_string(),
            description: Some("Track user journey from homepage to signup".to_string()),
            steps: vec![
                CreateFunnelStep {
                    event_name: "page_view".to_string(),
                    event_filter: vec![SmartFilter::PagePath("/".to_string())],
                },
                CreateFunnelStep {
                    event_name: "page_view".to_string(),
                    event_filter: vec![SmartFilter::PagePath("/pricing".to_string())],
                },
                CreateFunnelStep {
                    event_name: "user_signup".to_string(),
                    event_filter: vec![],
                },
            ],
        };

        let funnel_id = service
            .create_funnel(project_id, funnel_request)
            .await
            .expect("Failed to create funnel");

        let now = Utc::now();

        // Visitor 1: Complete funnel (homepage -> pricing -> signup)
        let v1_session1 = "visitor1_session1";
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            v1_session1,
            "page_view",
            None,
            "/",
            now,
            None,
        )
        .await;
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            v1_session1,
            "page_view",
            None,
            "/pricing",
            now + chrono::Duration::seconds(30),
            None,
        )
        .await;
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            v1_session1,
            "custom",
            Some("user_signup"),
            "/signup",
            now + chrono::Duration::seconds(60),
            None,
        )
        .await;

        // Visitor 2: Drop off at pricing (homepage -> pricing)
        let v2_session1 = "visitor2_session1";
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            v2_session1,
            "page_view",
            None,
            "/",
            now,
            None,
        )
        .await;
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            v2_session1,
            "page_view",
            None,
            "/pricing",
            now + chrono::Duration::seconds(20),
            None,
        )
        .await;
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            v2_session1,
            "page_view",
            None,
            "/blog",
            now + chrono::Duration::seconds(50),
            None,
        )
        .await;

        // Visitor 3: Drop off at homepage (homepage only)
        let v3_session1 = "visitor3_session1";
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            v3_session1,
            "page_view",
            None,
            "/",
            now,
            None,
        )
        .await;
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            v3_session1,
            "page_view",
            None,
            "/blog",
            now + chrono::Duration::seconds(10),
            None,
        )
        .await;

        // Visitor 4: Skip pricing, direct to signup (homepage -> signup) - should NOT complete
        let v4_session1 = "visitor4_session1";
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            v4_session1,
            "page_view",
            None,
            "/",
            now,
            None,
        )
        .await;
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            v4_session1,
            "custom",
            Some("user_signup"),
            "/signup",
            now + chrono::Duration::seconds(15),
            None,
        )
        .await;

        // Visitor 5: Multiple sessions, completes funnel in second session
        let v5_session1 = "visitor5_session1";
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            v5_session1,
            "page_view",
            None,
            "/",
            now,
            None,
        )
        .await;
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            v5_session1,
            "page_view",
            None,
            "/about",
            now + chrono::Duration::seconds(5),
            None,
        )
        .await;

        let v5_session2 = "visitor5_session2";
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            v5_session2,
            "page_view",
            None,
            "/",
            now + chrono::Duration::hours(2),
            None,
        )
        .await;
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            v5_session2,
            "page_view",
            None,
            "/pricing",
            now + chrono::Duration::hours(2) + chrono::Duration::seconds(45),
            None,
        )
        .await;
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            v5_session2,
            "custom",
            Some("user_signup"),
            "/signup",
            now + chrono::Duration::hours(2) + chrono::Duration::seconds(120),
            None,
        )
        .await;

        // Get funnel metrics
        let metrics = service
            .get_funnel_metrics(
                funnel_id,
                FunnelFilter {
                    project_id: Some(project_id),
                    environment_id: None,
                    country_code: None,
                    start_date: None,
                    end_date: None,
                },
            )
            .await
            .expect("Failed to get funnel metrics");

        // Assertions
        assert_eq!(metrics.funnel_name, "Signup Funnel");

        // 6 sessions entered (5 visitors with v5 having 2 sessions, all visited homepage)
        assert_eq!(
            metrics.total_entries, 6,
            "6 sessions should enter the funnel at homepage"
        );
        assert_eq!(metrics.step_conversions.len(), 3, "Should have 3 steps");

        // Step 1: Homepage (all 6 sessions)
        let step1 = &metrics.step_conversions[0];
        assert_eq!(step1.step_name, "page_view");
        assert_eq!(step1.completions, 6, "All 6 sessions viewed homepage");
        assert_eq!(
            step1.conversion_rate, 100.0,
            "100% conversion at entry step"
        );

        // Step 2: Pricing page (v1, v2, v5_session2 = 3 sessions)
        let step2 = &metrics.step_conversions[1];
        assert_eq!(step2.step_name, "page_view");
        assert_eq!(
            step2.completions, 3,
            "3 sessions viewed pricing (v1, v2, v5_session2)"
        );
        assert_eq!(
            step2.conversion_rate, 50.0,
            "50% went from homepage to pricing"
        );
        assert_eq!(step2.drop_off_rate, 50.0, "50% dropped off");

        // Step 3: Signup (v1, v5_session2 = 2 sessions)
        let step3 = &metrics.step_conversions[2];
        assert_eq!(step3.step_name, "user_signup");
        assert_eq!(
            step3.completions, 2,
            "2 sessions completed signup (v1, v5_session2)"
        );
        assert!(
            (step3.conversion_rate - 66.67).abs() < 0.01,
            "~67% went from pricing to signup, got {}",
            step3.conversion_rate
        );

        // Overall conversion: 2 completions out of 6 entries = 33.33%
        assert!(
            (metrics.overall_conversion_rate - 33.33).abs() < 0.01,
            "~33% overall conversion, got {}",
            metrics.overall_conversion_rate
        );
    }

    #[tokio::test]
    async fn test_utm_source_filtering() {
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");
        let db = test_db.db.clone();
        let (project_id, environment_id, deployment_id) = create_test_project(db.clone()).await;
        let service = FunnelService::new(db.clone());

        // Create funnel tracking conversions from specific UTM source
        let funnel_request = CreateFunnelRequest {
            name: "Google Ads Funnel".to_string(),
            description: Some("Track conversions from Google Ads traffic".to_string()),
            steps: vec![
                CreateFunnelStep {
                    event_name: "page_view".to_string(),
                    event_filter: vec![
                        SmartFilter::PagePath("/".to_string()),
                        SmartFilter::UtmSource("google".to_string()),
                    ],
                },
                CreateFunnelStep {
                    event_name: "user_signup".to_string(),
                    event_filter: vec![],
                },
            ],
        };

        let funnel_id = service
            .create_funnel(project_id, funnel_request)
            .await
            .expect("Failed to create funnel");

        let now = Utc::now();

        // Session 1: From Google, completes signup
        let s1 = "session_google_1";
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            s1,
            "page_view",
            None,
            "/",
            now,
            Some("google"),
        )
        .await;
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            s1,
            "custom",
            Some("user_signup"),
            "/signup",
            now + chrono::Duration::seconds(30),
            Some("google"),
        )
        .await;

        // Session 2: From Google, doesn't complete
        let s2 = "session_google_2";
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            s2,
            "page_view",
            None,
            "/",
            now,
            Some("google"),
        )
        .await;
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            s2,
            "page_view",
            None,
            "/about",
            now + chrono::Duration::seconds(15),
            Some("google"),
        )
        .await;

        // Session 3: From Facebook (should NOT enter funnel)
        let s3 = "session_facebook_1";
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            s3,
            "page_view",
            None,
            "/",
            now,
            Some("facebook"),
        )
        .await;
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            s3,
            "custom",
            Some("user_signup"),
            "/signup",
            now + chrono::Duration::seconds(20),
            Some("facebook"),
        )
        .await;

        // Session 4: Direct traffic (should NOT enter funnel)
        let s4 = "session_direct_1";
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            s4,
            "page_view",
            None,
            "/",
            now,
            None,
        )
        .await;
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            s4,
            "custom",
            Some("user_signup"),
            "/signup",
            now + chrono::Duration::seconds(10),
            None,
        )
        .await;

        // Get funnel metrics
        let metrics = service
            .get_funnel_metrics(
                funnel_id,
                FunnelFilter {
                    project_id: Some(project_id),
                    environment_id: None,
                    country_code: None,
                    start_date: None,
                    end_date: None,
                },
            )
            .await
            .expect("Failed to get funnel metrics");

        // Assertions
        assert_eq!(metrics.funnel_name, "Google Ads Funnel");

        // Only 2 Google sessions should enter
        assert_eq!(
            metrics.total_entries, 2,
            "Only Google traffic should enter funnel"
        );

        // Step 1: Homepage with utm_source=google
        let step1 = &metrics.step_conversions[0];
        assert_eq!(step1.completions, 2, "Both Google sessions viewed homepage");
        assert_eq!(step1.conversion_rate, 100.0);

        // Step 2: Signup (only s1 completed)
        let step2 = &metrics.step_conversions[1];
        assert_eq!(
            step2.completions, 1,
            "Only 1 Google session completed signup"
        );
        assert_eq!(step2.conversion_rate, 50.0, "50% conversion");

        // Overall
        assert_eq!(metrics.overall_conversion_rate, 50.0);
    }

    #[tokio::test]
    async fn test_custom_data_filtering() {
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");
        let db = test_db.db.clone();
        let (project_id, environment_id, deployment_id) = create_test_project(db.clone()).await;
        let service = FunnelService::new(db.clone());

        // Create funnel tracking premium user signups
        // Step 1: Visit pricing page
        // Step 2: Custom signup event with plan=premium
        let funnel_request = CreateFunnelRequest {
            name: "Premium Signup Funnel".to_string(),
            description: Some("Track conversions to premium plan".to_string()),
            steps: vec![
                CreateFunnelStep {
                    event_name: "page_view".to_string(),
                    event_filter: vec![SmartFilter::PagePath("/pricing".to_string())],
                },
                CreateFunnelStep {
                    event_name: "user_signup".to_string(),
                    event_filter: vec![SmartFilter::CustomData {
                        path: "plan".to_string(),
                        value: "premium".to_string(),
                    }],
                },
            ],
        };

        let funnel_id = service
            .create_funnel(project_id, funnel_request)
            .await
            .expect("Failed to create funnel");

        let now = Utc::now();

        // Session 1: Views pricing, signs up for premium (completes funnel)
        let s1 = "session_premium_1";
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            s1,
            "page_view",
            None,
            "/pricing",
            now,
            None,
        )
        .await;
        create_event_with_data(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            s1,
            "custom",
            Some("user_signup"),
            "/signup",
            now + chrono::Duration::seconds(30),
            None,
            Some(serde_json::json!({"plan": "premium", "price": 99})),
        )
        .await;

        // Session 2: Views pricing, signs up for free (should NOT complete funnel)
        let s2 = "session_free_1";
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            s2,
            "page_view",
            None,
            "/pricing",
            now,
            None,
        )
        .await;
        create_event_with_data(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            s2,
            "custom",
            Some("user_signup"),
            "/signup",
            now + chrono::Duration::seconds(20),
            None,
            Some(serde_json::json!({"plan": "free", "price": 0})),
        )
        .await;

        // Session 3: Views pricing, signs up for basic (should NOT complete funnel)
        let s3 = "session_basic_1";
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            s3,
            "page_view",
            None,
            "/pricing",
            now,
            None,
        )
        .await;
        create_event_with_data(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            s3,
            "custom",
            Some("user_signup"),
            "/signup",
            now + chrono::Duration::seconds(15),
            None,
            Some(serde_json::json!({"plan": "basic", "price": 29})),
        )
        .await;

        // Session 4: Views pricing only, doesn't sign up
        let s4 = "session_no_signup";
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            s4,
            "page_view",
            None,
            "/pricing",
            now,
            None,
        )
        .await;

        // Session 5: Another premium signup (completes funnel)
        let s5 = "session_premium_2";
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            s5,
            "page_view",
            None,
            "/pricing",
            now,
            None,
        )
        .await;
        create_event_with_data(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            s5,
            "custom",
            Some("user_signup"),
            "/signup",
            now + chrono::Duration::seconds(45),
            None,
            Some(serde_json::json!({"plan": "premium", "price": 99, "annual": true})),
        )
        .await;

        // Get funnel metrics
        let metrics = service
            .get_funnel_metrics(
                funnel_id,
                FunnelFilter {
                    project_id: Some(project_id),
                    environment_id: None,
                    country_code: None,
                    start_date: None,
                    end_date: None,
                },
            )
            .await
            .expect("Failed to get funnel metrics");

        // Assertions
        assert_eq!(metrics.funnel_name, "Premium Signup Funnel");

        // All 5 sessions viewed pricing page
        assert_eq!(metrics.total_entries, 5, "All 5 sessions viewed pricing");

        // Step 1: Pricing page
        let step1 = &metrics.step_conversions[0];
        assert_eq!(step1.step_name, "page_view");
        assert_eq!(step1.completions, 5, "All 5 sessions viewed pricing");
        assert_eq!(step1.conversion_rate, 100.0);

        // Step 2: Premium signup only (s1 and s5)
        let step2 = &metrics.step_conversions[1];
        assert_eq!(step2.step_name, "user_signup");
        assert_eq!(
            step2.completions, 2,
            "Only 2 sessions signed up for premium"
        );
        assert_eq!(step2.conversion_rate, 40.0, "40% converted to premium");

        // Overall conversion: 2 premium signups out of 5 entries
        assert_eq!(metrics.overall_conversion_rate, 40.0);
    }

    #[tokio::test]
    async fn test_nested_custom_data_filtering() {
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");
        let db = test_db.db.clone();
        let (project_id, environment_id, deployment_id) = create_test_project(db.clone()).await;
        let service = FunnelService::new(db.clone());

        // Create funnel tracking enterprise customer signups
        // Filter by nested JSON path: user.tier = "enterprise"
        let funnel_request = CreateFunnelRequest {
            name: "Enterprise Signup Funnel".to_string(),
            description: Some("Track enterprise tier signups".to_string()),
            steps: vec![
                CreateFunnelStep {
                    event_name: "page_view".to_string(),
                    event_filter: vec![SmartFilter::PagePath("/enterprise".to_string())],
                },
                CreateFunnelStep {
                    event_name: "user_signup".to_string(),
                    event_filter: vec![SmartFilter::CustomData {
                        path: "user.tier".to_string(),
                        value: "enterprise".to_string(),
                    }],
                },
            ],
        };

        let funnel_id = service
            .create_funnel(project_id, funnel_request)
            .await
            .expect("Failed to create funnel");

        let now = Utc::now();

        // Session 1: Enterprise signup (completes funnel)
        let s1 = "session_enterprise_1";
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            s1,
            "page_view",
            None,
            "/enterprise",
            now,
            None,
        )
        .await;
        create_event_with_data(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            s1,
            "custom",
            Some("user_signup"),
            "/signup",
            now + chrono::Duration::seconds(30),
            None,
            Some(serde_json::json!({
                "user": {
                    "tier": "enterprise",
                    "seats": 100,
                    "contract_value": 50000
                }
            })),
        )
        .await;

        // Session 2: Business tier signup (should NOT complete)
        let s2 = "session_business_1";
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            s2,
            "page_view",
            None,
            "/enterprise",
            now,
            None,
        )
        .await;
        create_event_with_data(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            s2,
            "custom",
            Some("user_signup"),
            "/signup",
            now + chrono::Duration::seconds(20),
            None,
            Some(serde_json::json!({
                "user": {
                    "tier": "business",
                    "seats": 10
                }
            })),
        )
        .await;

        // Session 3: Another enterprise signup
        let s3 = "session_enterprise_2";
        create_event(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            s3,
            "page_view",
            None,
            "/enterprise",
            now,
            None,
        )
        .await;
        create_event_with_data(
            db.as_ref(),
            project_id,
            environment_id,
            deployment_id,
            s3,
            "custom",
            Some("user_signup"),
            "/signup",
            now + chrono::Duration::seconds(60),
            None,
            Some(serde_json::json!({
                "user": {
                    "tier": "enterprise",
                    "seats": 500
                }
            })),
        )
        .await;

        // Get funnel metrics
        let metrics = service
            .get_funnel_metrics(
                funnel_id,
                FunnelFilter {
                    project_id: Some(project_id),
                    environment_id: None,
                    country_code: None,
                    start_date: None,
                    end_date: None,
                },
            )
            .await
            .expect("Failed to get funnel metrics");

        // Assertions
        assert_eq!(metrics.funnel_name, "Enterprise Signup Funnel");

        // All 3 sessions viewed enterprise page
        assert_eq!(metrics.total_entries, 3);

        // Step 1: Enterprise page
        let step1 = &metrics.step_conversions[0];
        assert_eq!(step1.completions, 3);

        // Step 2: Enterprise tier signups only (s1 and s3)
        let step2 = &metrics.step_conversions[1];
        assert_eq!(step2.completions, 2, "Only 2 enterprise tier signups");
        assert!(
            (step2.conversion_rate - 66.67).abs() < 0.01,
            "~67% converted to enterprise, got {}",
            step2.conversion_rate
        );

        // Overall conversion
        assert!((metrics.overall_conversion_rate - 66.67).abs() < 0.01);
    }

    // ========================================================================
    // MetricsCache unit tests (no DB required)
    // ========================================================================

    #[tokio::test]
    async fn test_cache_basic_get_set() {
        let cache: MetricsCache<String, String> = MetricsCache::new(5);

        // Miss on empty cache
        assert!(cache.get(&"key1".to_string()).await.is_none());

        // Set and hit
        cache.set("key1".to_string(), "value1".to_string()).await;
        assert_eq!(
            cache.get(&"key1".to_string()).await,
            Some("value1".to_string())
        );
    }

    #[tokio::test]
    async fn test_cache_expired_entries_not_returned() {
        // TTL of 0 minutes — entries expire immediately
        let cache: MetricsCache<String, String> = MetricsCache::new(0);

        cache.set("key1".to_string(), "value1".to_string()).await;

        // Should miss because TTL is 0 (already expired)
        assert!(
            cache.get(&"key1".to_string()).await.is_none(),
            "Expired entry should not be returned"
        );
    }

    #[tokio::test]
    async fn test_cache_invalidate() {
        let cache: MetricsCache<String, String> = MetricsCache::new(60);

        cache.set("key1".to_string(), "value1".to_string()).await;
        assert!(cache.get(&"key1".to_string()).await.is_some());

        cache.invalidate(&"key1".to_string()).await;
        assert!(cache.get(&"key1".to_string()).await.is_none());
    }

    #[tokio::test]
    async fn test_cache_cleanup_expired() {
        // TTL of 0 minutes
        let cache: MetricsCache<String, String> = MetricsCache::new(0);

        cache.set("key1".to_string(), "value1".to_string()).await;
        cache.set("key2".to_string(), "value2".to_string()).await;

        // Both entries are expired (TTL=0), cleanup should remove them
        cache.cleanup_expired().await;

        let inner = cache.cache.read().await;
        assert_eq!(inner.len(), 0, "All expired entries should be cleaned up");
    }

    #[tokio::test]
    async fn test_cache_bounded_capacity_evicts_expired_first() {
        // Create cache with very small max_capacity for testing
        let cache: MetricsCache<i32, String> = MetricsCache {
            cache: Arc::new(RwLock::new(HashMap::new())),
            default_ttl_minutes: 60,
            max_capacity: 3,
        };

        // Fill to capacity
        cache.set(1, "a".to_string()).await;
        cache.set(2, "b".to_string()).await;
        cache.set(3, "c".to_string()).await;

        {
            let inner = cache.cache.read().await;
            assert_eq!(inner.len(), 3, "Cache should be at capacity");
        }

        // Manually expire entry 1
        {
            let mut inner = cache.cache.write().await;
            if let Some(entry) = inner.get_mut(&1) {
                entry.expires_at = Utc::now() - Duration::minutes(1);
            }
        }

        // Adding a new entry should evict the expired entry first
        cache.set(4, "d".to_string()).await;

        {
            let inner = cache.cache.read().await;
            assert_eq!(inner.len(), 3, "Should still be at capacity (3)");
            assert!(
                !inner.contains_key(&1),
                "Expired entry 1 should have been evicted"
            );
            assert!(inner.contains_key(&4), "New entry 4 should be present");
        }
    }

    #[tokio::test]
    async fn test_cache_bounded_capacity_evicts_oldest_when_no_expired() {
        let cache: MetricsCache<i32, String> = MetricsCache {
            cache: Arc::new(RwLock::new(HashMap::new())),
            default_ttl_minutes: 60,
            max_capacity: 3,
        };

        // Fill to capacity with entries that expire at different times
        cache.set(1, "a".to_string()).await;

        // Give entry 2 a later expiration by inserting after a tiny delay
        // (or by manipulating the cache directly)
        cache.set(2, "b".to_string()).await;
        cache.set(3, "c".to_string()).await;

        // Make entry 1 expire soonest (oldest expiration)
        {
            let mut inner = cache.cache.write().await;
            if let Some(entry) = inner.get_mut(&1) {
                entry.expires_at = Utc::now() + Duration::minutes(1); // soonest
            }
            if let Some(entry) = inner.get_mut(&2) {
                entry.expires_at = Utc::now() + Duration::minutes(30);
            }
            if let Some(entry) = inner.get_mut(&3) {
                entry.expires_at = Utc::now() + Duration::minutes(60);
            }
        }

        // No expired entries, so adding a 4th should evict the one with earliest expiration (key=1)
        cache.set(4, "d".to_string()).await;

        {
            let inner = cache.cache.read().await;
            assert_eq!(inner.len(), 3, "Should still be at capacity");
            assert!(
                !inner.contains_key(&1),
                "Oldest-by-expiration entry (key=1) should be evicted"
            );
            assert!(inner.contains_key(&2), "Entry 2 should remain");
            assert!(inner.contains_key(&3), "Entry 3 should remain");
            assert!(inner.contains_key(&4), "New entry 4 should be present");
        }
    }

    #[tokio::test]
    async fn test_cache_under_capacity_does_not_evict() {
        let cache: MetricsCache<i32, String> = MetricsCache {
            cache: Arc::new(RwLock::new(HashMap::new())),
            default_ttl_minutes: 60,
            max_capacity: 10,
        };

        cache.set(1, "a".to_string()).await;
        cache.set(2, "b".to_string()).await;
        cache.set(3, "c".to_string()).await;

        {
            let inner = cache.cache.read().await;
            assert_eq!(inner.len(), 3, "All 3 entries should be present");
        }
    }

    #[tokio::test]
    async fn test_cache_invalidate_by_funnel() {
        let cache: MetricsCache<FunnelMetricsCacheKey, String> = MetricsCache::new(60);

        let key1 = FunnelMetricsCacheKey {
            funnel_id: 1,
            environment_id: None,
            country_code: None,
            start_date: None,
            end_date: None,
        };
        let key2 = FunnelMetricsCacheKey {
            funnel_id: 1,
            environment_id: Some(5),
            country_code: None,
            start_date: None,
            end_date: None,
        };
        let key3 = FunnelMetricsCacheKey {
            funnel_id: 2,
            environment_id: None,
            country_code: None,
            start_date: None,
            end_date: None,
        };

        cache.set(key1, "metrics1".to_string()).await;
        cache.set(key2, "metrics2".to_string()).await;
        cache.set(key3, "metrics3".to_string()).await;

        // Invalidate funnel 1 — should remove key1 and key2, keep key3
        cache.invalidate_by_funnel(1).await;

        {
            let inner = cache.cache.read().await;
            assert_eq!(inner.len(), 1, "Only funnel 2 entry should remain");
            assert!(
                inner.contains_key(&FunnelMetricsCacheKey {
                    funnel_id: 2,
                    environment_id: None,
                    country_code: None,
                    start_date: None,
                    end_date: None,
                }),
                "Funnel 2 entry should still be present"
            );
        }
    }
}
