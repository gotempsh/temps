use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, FromQueryResult, QueryFilter,
    Set,
};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use temps_embeddings::tokenizer::{HashTokenizer, Tokenizer};
use temps_entities::{error_events, error_groups};

use super::types::{CreateErrorEventData, ErrorTrackingError};

/// Service for ingesting and processing error events
pub struct ErrorIngestionService {
    db: Arc<DatabaseConnection>,
    tokenizer: Arc<dyn Tokenizer>,
}

impl ErrorIngestionService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        // Use HashTokenizer with vocab_size=10000 for production
        let tokenizer = Arc::new(HashTokenizer::new(10000)) as Arc<dyn Tokenizer>;
        Self { db, tokenizer }
    }

    /// Create with custom tokenizer
    pub fn with_tokenizer(db: Arc<DatabaseConnection>, tokenizer: Arc<dyn Tokenizer>) -> Self {
        Self { db, tokenizer }
    }

    /// Create an embedding from error message
    fn create_embedding(&self, message: &str) -> Option<error_groups::PgVector> {
        // Tokenize the message
        let tokens = self.tokenizer.encode(message).ok()?;

        // Create embedding from tokens (384 dimensions to match database)
        let embedding = error_groups::Model::create_embedding_from_tokens(&tokens, 384);

        Some(embedding)
    }

    /// Process a new error event - core entry point
    pub async fn process_error_event(
        &self,
        error_data: CreateErrorEventData,
    ) -> Result<i32, ErrorTrackingError> {
        // 1. Generate fingerprint for exact matching
        let fingerprint = self.generate_fingerprint(&error_data);

        // 2. Try to find existing group by fingerprint (fast path)
        if let Some(group_id) = self
            .find_group_by_fingerprint(&fingerprint, error_data.project_id)
            .await?
        {
            self.create_error_event(&error_data, group_id, &fingerprint)
                .await?;
            self.increment_group_count(group_id).await?;
            return Ok(group_id);
        }

        // 3. Try vector similarity search (fallback)
        // Use first exception for embedding text
        let embedding_text = if let Some(first_exception) = error_data.exceptions.first() {
            first_exception
                .exception_value
                .as_ref()
                .unwrap_or(&first_exception.exception_type)
                .clone()
        } else {
            error_data
                .exception_type
                .as_ref()
                .or(error_data.exception_value.as_ref())
                .unwrap_or(&"Unknown error".to_string())
                .clone()
        };

        if let Some(embedding) = self.create_embedding(&embedding_text) {
            if let Some(similar_group_id) = self
                .find_similar_group_by_embedding(&embedding, error_data.project_id)
                .await?
            {
                self.create_error_event(&error_data, similar_group_id, &fingerprint)
                    .await?;
                self.increment_group_count(similar_group_id).await?;
                return Ok(similar_group_id);
            }
        }

        // 4. Create new error group if no similar group found
        let group_id = self.create_error_group(&error_data, &fingerprint).await?;
        self.create_error_event(&error_data, group_id, &fingerprint)
            .await?;

        Ok(group_id)
    }

    /// Generate a fingerprint for error matching
    pub fn generate_fingerprint(&self, error_data: &CreateErrorEventData) -> String {
        // Use first exception for fingerprint, or fall back to legacy fields
        let (exception_type, exception_value, stack_trace) =
            if let Some(first_exception) = error_data.exceptions.first() {
                (
                    first_exception.exception_type.clone(),
                    first_exception.exception_value.clone().unwrap_or_default(),
                    &first_exception.stack_trace,
                )
            } else {
                (
                    error_data.exception_type.clone().unwrap_or_default(),
                    error_data.exception_value.clone().unwrap_or_default(),
                    &error_data.stack_trace,
                )
            };

        let components = [
            exception_type,
            self.normalize_error_message(&exception_value),
            self.extract_stack_signature(stack_trace, 3),
        ];

        let content = components.join("||");
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Normalize error messages for consistent grouping
    /// Replaces dynamic values (IDs, UUIDs, numbers, paths, URLs) with placeholders
    fn normalize_error_message(&self, message: &str) -> String {
        use regex::Regex;

        let mut normalized = message.to_lowercase();

        // Replace UUIDs (e.g., 550e8400-e29b-41d4-a716-446655440000) - FIRST
        let uuid_regex =
            Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}").unwrap();
        normalized = uuid_regex.replace_all(&normalized, "<uuid>").to_string();

        // Replace hex IDs (e.g., 0x1a2b3c4d, deadbeef) - SECOND
        let hex_regex = Regex::new(r"\b(0x)?[0-9a-f]{8,}\b").unwrap();
        normalized = hex_regex.replace_all(&normalized, "<hex_id>").to_string();

        // Replace URLs (http/https) - BEFORE paths (paths might match URL components)
        let url_regex = Regex::new(r"https?://[\w./\-?=&%]+").unwrap();
        normalized = url_regex.replace_all(&normalized, "<url>").to_string();

        // Replace email addresses - BEFORE paths
        let email_regex = Regex::new(r"\b[\w._%+-]+@[\w.-]+\.[a-z]{2,}\b").unwrap();
        normalized = email_regex.replace_all(&normalized, "<email>").to_string();

        // Replace file paths (Unix and Windows style) - AFTER URLs/emails
        let unix_path_regex = Regex::new(r"/[\w/.]+\.[\w]+").unwrap();
        normalized = unix_path_regex
            .replace_all(&normalized, "<path>")
            .to_string();

        let windows_path_regex = Regex::new(r"[a-z]:\\[\w\\]+\.[\w]+").unwrap();
        normalized = windows_path_regex
            .replace_all(&normalized, "<path>")
            .to_string();

        // Replace IP addresses (v4) - BEFORE table refs and numbers
        let ip_regex = Regex::new(r"\b\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}\b").unwrap();
        normalized = ip_regex.replace_all(&normalized, "<ip>").to_string();

        // Replace database table references (table_123, users_456) - BEFORE general numbers
        let table_ref_regex = Regex::new(r"\b(\w+)_\d+\b").unwrap();
        normalized = table_ref_regex
            .replace_all(&normalized, "${1}_<id>")
            .to_string();

        // Replace numeric IDs and timestamps (standalone numbers of 4+ digits) - LAST number operation
        let number_regex = Regex::new(r"\b\d{4,}\b").unwrap();
        normalized = number_regex.replace_all(&normalized, "<num>").to_string();

        // Replace quoted strings (often dynamic user input) - FINAL
        let quoted_string_regex = Regex::new(r#"["']([^"']{10,})["']"#).unwrap();
        normalized = quoted_string_regex
            .replace_all(&normalized, r#""<string>""#)
            .to_string();

        // Truncate to 200 characters
        normalized.chars().take(200).collect::<String>()
    }

    /// Extract stack trace signature for fingerprinting
    fn extract_stack_signature(
        &self,
        stack_trace: &Option<serde_json::Value>,
        depth: usize,
    ) -> String {
        if let Some(stack) = stack_trace {
            if let Some(frames) = stack.as_array() {
                return frames
                    .iter()
                    .take(depth)
                    .filter_map(|frame| {
                        let filename = frame.get("filename")?.as_str()?;
                        let function = frame.get("function")?.as_str().unwrap_or("anonymous");
                        Some(format!(
                            "{}:{}",
                            self.normalize_filename(filename),
                            function
                        ))
                    })
                    .collect::<Vec<_>>()
                    .join("|");
            }
        }
        "unknown".to_string()
    }

    /// Normalize filenames for consistent grouping
    fn normalize_filename(&self, filename: &str) -> String {
        // Remove absolute paths, keep relative structure
        filename
            .split('/')
            .next_back()
            .unwrap_or(filename)
            .to_string()
    }

    /// Find existing group by fingerprint hash within the same project
    async fn find_group_by_fingerprint(
        &self,
        fingerprint: &str,
        project_id: i32,
    ) -> Result<Option<i32>, ErrorTrackingError> {
        let result = error_events::Entity::find()
            .filter(error_events::Column::FingerprintHash.eq(fingerprint))
            .filter(error_events::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?;

        Ok(result.map(|event| event.error_group_id))
    }

    /// Find similar error group using vector cosine similarity
    ///
    /// Uses pgvector's cosine distance operator (<=>)
    /// Hardcoded similarity threshold: 0.15 (lower = more similar, 0 = identical)
    ///
    /// Only searches unresolved and assigned groups (excludes resolved and ignored)
    async fn find_similar_group_by_embedding(
        &self,
        embedding: &error_groups::PgVector,
        project_id: i32,
    ) -> Result<Option<i32>, ErrorTrackingError> {
        #[derive(Debug, FromQueryResult)]
        struct SimilarGroup {
            id: i32,
            #[allow(dead_code)]
            distance: f32,
        }

        const SIMILARITY_THRESHOLD: f32 = 0.15; // Cosine distance threshold

        // Convert embedding to array string for SQL
        let embedding_array = format!(
            "[{}]",
            embedding
                .0
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );

        // Query for similar error groups using pgvector cosine distance
        let sql = r#"
            SELECT id, embedding <=> $1::vector AS distance
            FROM error_groups
            WHERE project_id = $2
              AND embedding IS NOT NULL
              AND status IN ('unresolved', 'assigned')
              AND embedding <=> $1::vector < $3
            ORDER BY distance ASC
            LIMIT 1
            "#
        .to_string();

        let result: Option<SimilarGroup> =
            sea_orm::FromQueryResult::find_by_statement(sea_orm::Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Postgres,
                &sql,
                vec![
                    embedding_array.into(),
                    project_id.into(),
                    SIMILARITY_THRESHOLD.into(),
                ],
            ))
            .one(self.db.as_ref())
            .await?;

        Ok(result.map(|r| r.id))
    }

    /// Create a new error group with embedding
    async fn create_error_group(
        &self,
        error_data: &CreateErrorEventData,
        _fingerprint: &str,
    ) -> Result<i32, ErrorTrackingError> {
        // Use first exception for title, or fall back to legacy fields
        let (exception_type, exception_value) =
            if let Some(first_exception) = error_data.exceptions.first() {
                (
                    first_exception.exception_type.clone(),
                    first_exception
                        .exception_value
                        .clone()
                        .unwrap_or_else(|| "Unknown error".to_string()),
                )
            } else {
                (
                    error_data
                        .exception_type
                        .clone()
                        .unwrap_or_else(|| "Error".to_string()),
                    error_data
                        .exception_value
                        .clone()
                        .unwrap_or_else(|| "Unknown error".to_string()),
                )
            };

        let title = format!(
            "{}: {}",
            exception_type,
            exception_value.chars().take(100).collect::<String>()
        );

        // Create embedding from error message for similarity search (reuse exception_value from above)
        let embedding = self.create_embedding(&exception_value);

        let new_group = error_groups::ActiveModel {
            title: Set(title.clone()),
            error_type: Set(exception_type.clone()),
            message_template: Set(Some(exception_value.clone())),
            embedding: Set(embedding),
            first_seen: Set(Utc::now()),
            last_seen: Set(Utc::now()),
            total_count: Set(1),
            status: Set("unresolved".to_string()),
            assigned_to: Set(None),
            project_id: Set(error_data.project_id),
            environment_id: Set(error_data.environment_id),
            deployment_id: Set(error_data.deployment_id),
            visitor_id: Set(error_data.visitor_id),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };

        let group = new_group.insert(self.db.as_ref()).await?;
        Ok(group.id)
    }

    /// Create an error event within a group
    async fn create_error_event(
        &self,
        error_data: &CreateErrorEventData,
        group_id: i32,
        fingerprint: &str,
    ) -> Result<i64, ErrorTrackingError> {
        // Use raw Sentry event if available, otherwise build from individual fields
        let data_json = if let Some(raw_sentry) = &error_data.raw_sentry_event {
            // Wrap the raw Sentry event in our structure with source metadata
            let mut wrapper = serde_json::Map::new();
            wrapper.insert(
                "source".to_string(),
                serde_json::Value::String("sentry".to_string()),
            );
            wrapper.insert("sentry".to_string(), raw_sentry.clone());
            serde_json::Value::Object(wrapper)
        } else {
            use temps_entities::error_events::{
                DeviceContext, EnvironmentContext, ErrorEventData, RequestContext, StackFrame,
                TraceContext, UserContext,
            };

            // Build structured data from individual fields (for non-Sentry sources)
            let event_data = ErrorEventData {
                source: Some("custom".to_string()),
                user: Some(UserContext {
                    user_id: error_data.user_id.clone(),
                    email: error_data.user_email.clone(),
                    username: error_data.user_username.clone(),
                    ip_address: error_data.user_ip_address.clone(),
                    segment: error_data.user_segment.clone(),
                    session_id: error_data.session_id.clone(),
                    custom: error_data.user_context.clone(),
                }),
                device: Some(DeviceContext {
                    browser: error_data.browser.clone(),
                    browser_version: error_data.browser_version.clone(),
                    os: error_data.operating_system.clone(),
                    os_version: error_data.operating_system_version.clone(),
                    os_build: error_data.os_build.clone(),
                    os_kernel_version: error_data.os_kernel_version.clone(),
                    device_type: error_data.device_type.clone(),
                    device_arch: error_data.device_arch.clone(),
                    screen_width: error_data.screen_width,
                    screen_height: error_data.screen_height,
                    viewport_width: error_data.viewport_width,
                    viewport_height: error_data.viewport_height,
                    locale: error_data.locale.clone(),
                    timezone: error_data.timezone.clone(),
                    processor_count: error_data.device_processor_count,
                    processor_frequency: error_data.device_processor_frequency,
                    memory_size: error_data.device_memory_size,
                    free_memory: error_data.device_free_memory,
                    boot_time: error_data.device_boot_time.map(|dt| dt.to_string()),
                }),
                request: Some(RequestContext {
                    url: error_data.url.clone(),
                    method: error_data.method.clone(),
                    user_agent: error_data.user_agent.clone(),
                    referrer: error_data.referrer.clone(),
                    headers: error_data.headers.clone(),
                    cookies: error_data.request_cookies.clone(),
                    query_string: error_data.request_query_string.clone(),
                    post_data: error_data.request_data.clone(),
                }),
                // Try to parse stack trace into our format, extract frames if it's a Sentry stacktrace object
                stack_trace: error_data.stack_trace.as_ref().and_then(|st| {
                    // If it's a Sentry stacktrace object with "frames" field, extract the frames
                    if let serde_json::Value::Object(obj) = st {
                        if let Some(frames) = obj.get("frames") {
                            return serde_json::from_value::<Vec<StackFrame>>(frames.clone()).ok();
                        }
                    }
                    // Otherwise try to parse the whole thing as Vec<StackFrame>
                    serde_json::from_value::<Vec<StackFrame>>(st.clone()).ok()
                }),
                environment: Some(EnvironmentContext {
                    sdk_name: error_data.sdk_name.clone(),
                    sdk_version: error_data.sdk_version.clone(),
                    sdk_integrations: error_data
                        .sdk_integrations
                        .as_ref()
                        .and_then(|v| serde_json::from_value(v.clone()).ok()),
                    platform: error_data.platform.clone(),
                    release: error_data.release_version.clone(),
                    build: error_data.build_number.clone(),
                    server_name: error_data.server_name.clone(),
                    environment: error_data.environment.clone(),
                    runtime_name: error_data.runtime_name.clone(),
                    runtime_version: error_data.runtime_version.clone(),
                    app_start_time: error_data.app_start_time.map(|dt| dt.to_string()),
                    app_memory: error_data.app_memory,
                }),
                trace: Some(TraceContext {
                    transaction: error_data.transaction_name.clone(),
                    breadcrumbs: error_data
                        .breadcrumbs
                        .as_ref()
                        .and_then(|v| serde_json::from_value(v.clone()).ok()),
                    extra: error_data.extra_context.clone(),
                    contexts: error_data.contexts.clone(),
                }),
                sentry: None, // Can be populated from raw SDK payload if needed
            };
            event_data
                .to_json_value()
                .unwrap_or_else(|| serde_json::json!({}))
        };

        // Use first exception for event fields (legacy compatibility)
        let (event_exception_type, event_exception_value) =
            if let Some(first_exception) = error_data.exceptions.first() {
                (
                    first_exception.exception_type.clone(),
                    first_exception.exception_value.clone(),
                )
            } else {
                (
                    error_data
                        .exception_type
                        .clone()
                        .unwrap_or_else(|| "Error".to_string()),
                    error_data.exception_value.clone(),
                )
            };

        let new_event = error_events::ActiveModel {
            error_group_id: Set(group_id),
            fingerprint_hash: Set(fingerprint.to_string()),
            timestamp: Set(Utc::now()),
            exception_type: Set(event_exception_type),
            exception_value: Set(event_exception_value),
            source: Set(error_data.source.clone()),
            data: Set(Some(data_json)),
            project_id: Set(error_data.project_id),
            environment_id: Set(error_data.environment_id),
            deployment_id: Set(error_data.deployment_id),
            visitor_id: Set(error_data.visitor_id),
            ip_geolocation_id: Set(error_data.ip_geolocation_id),
            created_at: Set(Utc::now()),
            ..Default::default()
        };

        let event = new_event.insert(self.db.as_ref()).await?;
        Ok(event.id)
    }

    /// Increment error count for a group
    async fn increment_group_count(&self, group_id: i32) -> Result<(), ErrorTrackingError> {
        let group = error_groups::Entity::find_by_id(group_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(ErrorTrackingError::GroupNotFound)?;

        let mut group_update: error_groups::ActiveModel = group.into();
        group_update.total_count = Set(group_update.total_count.unwrap() + 1);
        group_update.last_seen = Set(Utc::now());
        group_update.updated_at = Set(Utc::now());
        group_update.update(self.db.as_ref()).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    include!("error_ingestion_tests.rs");
}
