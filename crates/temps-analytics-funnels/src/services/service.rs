use chrono::Utc;
use sea_orm::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use temps_core::UtcDateTime;
use temps_entities::{funnel_steps, funnels};

#[derive(Debug, Serialize, Deserialize)]
pub struct FunnelMetrics {
    pub funnel_id: i32,
    pub funnel_name: String,
    pub total_entries: u64,
    pub step_conversions: Vec<StepConversion>,
    pub overall_conversion_rate: f64,
    pub average_completion_time_seconds: f64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StepConversion {
    pub step_id: i32,
    pub step_name: String,
    pub step_order: i32,
    pub completions: u64,
    pub conversion_rate: f64, // Percentage of previous step that completed this step
    pub drop_off_rate: f64,
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

pub struct FunnelService {
    db: Arc<DatabaseConnection>,
}

impl FunnelService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
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

    /// Get all unique event types for a project
    /// Returns event names with their occurrence count, ordered by count descending
    pub async fn get_unique_events(&self, project_id: i32) -> Result<Vec<(String, i64)>, DbErr> {
        let db = self.db.as_ref();

        // Query to get unique events using COALESCE to check both event_name and event_type
        // Group by the coalesced value and count occurrences
        let sql = r#"
            SELECT
                COALESCE(event_name, event_type) as event,
                COUNT(*) as count
            FROM events
            WHERE project_id = $1
            GROUP BY COALESCE(event_name, event_type)
            ORDER BY count DESC, event ASC
        "#;

        let results = db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                vec![project_id.into()],
            ))
            .await?;

        let mut events = Vec::new();
        for row in results {
            let event_name: String = row.try_get("", "event")?;
            let count: i64 = row.try_get("", "count")?;
            events.push((event_name, count));
        }

        Ok(events)
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

    /// Get funnel metrics by querying events directly
    pub async fn get_funnel_metrics(
        &self,
        funnel_id: i32,
        filter: FunnelFilter,
    ) -> Result<FunnelMetrics, DbErr> {
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
        self.calculate_funnel_metrics_internal(
            funnel_id,
            &funnel.name,
            funnel.project_id,
            &steps_data,
            filter,
        )
        .await
    }

    /// Internal method to calculate funnel metrics
    /// Takes funnel_id, name, project_id, and steps data as parameters
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

        // Build a map of sessions that completed each step
        let mut step_conversions = Vec::new();
        let mut sessions_by_step: Vec<HashMap<String, UtcDateTime>> = Vec::new();

        for (step_index, (_step_order, event_name, event_filter_opt)) in
            steps_data.iter().enumerate()
        {
            // Query events matching this step
            // Use COALESCE to check both event_name and event_type since event_name is optional
            let mut where_conditions = vec![
                "project_id = $1".to_string(),
                "COALESCE(event_name, event_type) = $2".to_string(),
                "session_id IS NOT NULL".to_string(),
            ];
            let mut values: Vec<sea_orm::Value> =
                vec![project_id.into(), event_name.clone().into()];
            let mut param_index = 3;

            // Apply event-specific filters from step.event_filter
            if let Some(event_filter_str) = event_filter_opt {
                if let Ok(filter_obj) =
                    serde_json::from_str::<serde_json::Map<String, Value>>(event_filter_str)
                {
                    for (key, value) in filter_obj.iter() {
                        // Handle CustomData filters separately
                        if key == "_custom_data" {
                            if let Value::Array(custom_filters) = value {
                                for custom_filter in custom_filters {
                                    if let (Some(path), Some(filter_value)) = (
                                        custom_filter.get("path").and_then(|v| v.as_str()),
                                        custom_filter.get("value").and_then(|v| v.as_str()),
                                    ) {
                                        // Create SmartFilter and use to_json_condition
                                        let smart_filter = SmartFilter::CustomData {
                                            path: path.to_string(),
                                            value: filter_value.to_string(),
                                        };
                                        if let Some(json_condition) =
                                            smart_filter.to_json_condition()
                                        {
                                            where_conditions.push(json_condition);
                                        }
                                    }
                                }
                            }
                            continue;
                        }

                        // Validate that the column exists to prevent SQL injection
                        let allowed_columns = vec![
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
                            tracing::warn!(
                                "Invalid filter column '{}' in funnel step, skipping",
                                key
                            );
                            continue;
                        }

                        // Build condition based on value type using parameterized queries
                        match value {
                            Value::String(s) => {
                                where_conditions.push(format!("{} = ${}", key, param_index));
                                values.push(s.as_str().into());
                                param_index += 1;
                            }
                            Value::Number(n) => {
                                where_conditions.push(format!("{} = ${}", key, param_index));
                                if let Some(n_i64) = n.as_i64() {
                                    values.push(n_i64.into());
                                } else if let Some(n_f64) = n.as_f64() {
                                    values.push(n_f64.into());
                                }
                                param_index += 1;
                            }
                            Value::Bool(b) => {
                                where_conditions.push(format!("{} = ${}", key, param_index));
                                values.push((*b).into());
                                param_index += 1;
                            }
                            Value::Null => {
                                where_conditions.push(format!("{} IS NULL", key));
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

            // Apply global filters
            if let Some(env_id) = filter.environment_id {
                where_conditions.push(format!("environment_id = ${}", param_index));
                values.push(env_id.into());
                param_index += 1;
            }
            if let Some(start_date) = filter.start_date {
                where_conditions.push(format!("timestamp >= ${}", param_index));
                values.push(start_date.into());
                param_index += 1;
            }
            if let Some(end_date) = filter.end_date {
                where_conditions.push(format!("timestamp <= ${}", param_index));
                values.push(end_date.into());
            }

            let query = format!(
                "SELECT session_id, timestamp FROM events WHERE {} ORDER BY timestamp ASC",
                where_conditions.join(" AND ")
            );

            tracing::debug!("Funnel step {} query: {}", step_index + 1, query);

            #[derive(Debug, FromQueryResult)]
            struct EventResult {
                session_id: Option<String>,
                timestamp: UtcDateTime,
            }

            // Get all matching events using parameterized query
            let events = EventResult::find_by_statement(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                query,
                values,
            ))
            .all(db)
            .await?;

            // Group by session and keep the earliest occurrence
            let mut sessions_completed = HashMap::new();
            for event in events {
                if let Some(session_id) = event.session_id {
                    // Keep the earliest timestamp for each session
                    sessions_completed
                        .entry(session_id.clone())
                        .and_modify(|existing_time| {
                            if event.timestamp < *existing_time {
                                *existing_time = event.timestamp;
                            }
                        })
                        .or_insert(event.timestamp);
                }
            }

            tracing::debug!(
                "Step {}: '{}' - Found {} sessions: {:?}",
                step_index + 1,
                event_name,
                sessions_completed.len(),
                sessions_completed.keys().collect::<Vec<_>>()
            );

            // For first step, all sessions count
            // For subsequent steps, only count sessions that completed previous step
            let qualified_sessions = if step_index == 0 {
                sessions_completed.clone()
            } else {
                let previous_sessions = &sessions_by_step[step_index - 1];
                tracing::debug!(
                    "Step {}: Previous step had {} sessions: {:?}",
                    step_index + 1,
                    previous_sessions.len(),
                    previous_sessions.keys().collect::<Vec<_>>()
                );

                let filtered: HashMap<_, _> = sessions_completed
                    .into_iter()
                    .filter(|(session_id, timestamp)| {
                        // Only count if session completed previous step AND
                        // this step happened after (or at the same time as) the previous step
                        if let Some(prev_time) = previous_sessions.get(session_id) {
                            let qualifies = *timestamp >= *prev_time;
                            tracing::debug!(
                                "Step {}: Session '{}' - current: {:?}, prev: {:?}, qualifies: {}",
                                step_index + 1,
                                session_id,
                                timestamp,
                                prev_time,
                                qualifies
                            );
                            qualifies
                        } else {
                            tracing::debug!(
                                "Step {}: Session '{}' not found in previous step",
                                step_index + 1,
                                session_id
                            );
                            false
                        }
                    })
                    .collect();

                tracing::debug!(
                    "Step {}: After filtering, {} sessions qualify",
                    step_index + 1,
                    filtered.len()
                );

                filtered
            };

            sessions_by_step.push(qualified_sessions);
        }

        // Calculate metrics for each step
        let total_entries = sessions_by_step[0].len() as u64;
        let mut previous_step_completions = total_entries;

        for (step_index, (step_order, event_name, _event_filter)) in steps_data.iter().enumerate() {
            let completions = sessions_by_step[step_index].len() as u64;

            let conversion_rate = if previous_step_completions > 0 {
                (completions as f64 / previous_step_completions as f64) * 100.0
            } else {
                0.0
            };

            let drop_off_rate = 100.0 - conversion_rate;

            // Calculate average time to complete this step
            let avg_time = if step_index > 0 && completions > 0 {
                let current_sessions = &sessions_by_step[step_index];
                let previous_sessions = &sessions_by_step[step_index - 1];

                let mut times = Vec::new();
                for (session_id, current_time) in current_sessions {
                    if let Some(prev_time) = previous_sessions.get(session_id) {
                        let duration = current_time.signed_duration_since(*prev_time).num_seconds();
                        if duration >= 0 {
                            times.push(duration);
                        }
                    }
                }

                if !times.is_empty() {
                    times.iter().sum::<i64>() as f64 / times.len() as f64
                } else {
                    0.0
                }
            } else {
                0.0
            };

            step_conversions.push(StepConversion {
                step_id: 0, // Will be 0 for preview, actual ID for saved funnels
                step_name: event_name.clone(),
                step_order: *step_order,
                completions,
                conversion_rate,
                drop_off_rate,
                average_time_to_complete_seconds: avg_time,
            });

            previous_step_completions = completions;
        }

        // Calculate overall conversion rate
        let final_completions = sessions_by_step.last().map(|s| s.len() as u64).unwrap_or(0);
        let overall_conversion_rate = if total_entries > 0 {
            (final_completions as f64 / total_entries as f64) * 100.0
        } else {
            0.0
        };

        // Calculate average completion time for full funnel
        let average_completion_time = if !steps_data.is_empty() && final_completions > 0 {
            let last_step_sessions = &sessions_by_step[sessions_by_step.len() - 1];
            let first_step_sessions = &sessions_by_step[0];

            let mut completion_times = Vec::new();
            for (session_id, last_time) in last_step_sessions {
                if let Some(first_time) = first_step_sessions.get(session_id) {
                    let duration = last_time.signed_duration_since(*first_time).num_seconds();
                    if duration >= 0 {
                        completion_times.push(duration);
                    }
                }
            }

            if !completion_times.is_empty() {
                completion_times.iter().sum::<i64>() as f64 / completion_times.len() as f64
            } else {
                0.0
            }
        } else {
            0.0
        };

        Ok(FunnelMetrics {
            funnel_id,
            funnel_name: funnel_name.to_string(),
            total_entries,
            step_conversions,
            overall_conversion_rate,
            average_completion_time_seconds: average_completion_time,
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
}
