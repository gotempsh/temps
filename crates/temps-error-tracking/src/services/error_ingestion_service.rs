// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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

/// Pull a 32-hex-char OTel trace_id out of an error event's JSONB `data`
/// blob. Probes both the Sentry layout (`contexts.trace.trace_id`) and our
/// internal layout (`trace.trace_id`). Returns `None` when no valid
/// trace_id is present so the indexed column stays NULL for legacy events.
fn extract_trace_id_from_data(data: &serde_json::Value) -> Option<String> {
    fn validate(v: &serde_json::Value) -> Option<String> {
        let s = v.as_str()?.trim().to_ascii_lowercase();
        if s.len() == 32 && s.chars().all(|c| c.is_ascii_hexdigit()) && s.chars().any(|c| c != '0')
        {
            Some(s)
        } else {
            None
        }
    }

    // Sentry / our internal wrapper: data.sentry.contexts.trace.trace_id
    if let Some(v) = data.pointer("/sentry/contexts/trace/trace_id") {
        if let Some(s) = validate(v) {
            return Some(s);
        }
    }
    // Top-level Sentry payload: data.contexts.trace.trace_id
    if let Some(v) = data.pointer("/contexts/trace/trace_id") {
        if let Some(s) = validate(v) {
            return Some(s);
        }
    }
    // Custom layout via TraceContext: data.trace.trace_id
    if let Some(v) = data.pointer("/trace/trace_id") {
        if let Some(s) = validate(v) {
            return Some(s);
        }
    }
    None
}

/// The exception type and message a group is created from, from the first
/// exception or else the legacy top-level fields. When the message is
/// missing (most `captureMessage` payloads and Sentry SDKs that send only an
/// event-level message), probe the raw event for a usable one so the title
/// doesn't render as the useless literal "Error: Unknown error".
///
/// The similarity lookup embeds the same text (message, else type), so an
/// incoming event is compared against what each group was embedded from.
fn group_type_and_message(error_data: &CreateErrorEventData) -> (String, Option<String>) {
    let raw_message = || extract_message_from_raw(error_data.raw_sentry_event.as_ref());
    match error_data.exceptions.first() {
        Some(first_exception) => (
            first_exception.exception_type.clone(),
            first_exception
                .exception_value
                .clone()
                .filter(|s| !s.trim().is_empty())
                .or_else(raw_message),
        ),
        None => (
            error_data
                .exception_type
                .clone()
                .unwrap_or_else(|| "Error".to_string()),
            error_data
                .exception_value
                .clone()
                .filter(|s| !s.trim().is_empty())
                .or_else(raw_message),
        ),
    }
}

/// Probe a raw Sentry payload for a usable human-readable message.
///
/// SDKs that send `captureMessage()` (no exception) put the text in
/// `logentry.formatted` (the rendered string after parameter
/// substitution) or `logentry.message` (the template). SDKs sending
/// `captureException()` put it in `exception.values[0].value`. Some
/// older SDKs use the deprecated top-level `message`. We probe in
/// priority order and return the first non-empty hit.
pub fn extract_message_from_raw(raw: Option<&serde_json::Value>) -> Option<String> {
    let raw = raw?;
    let candidates = [
        "/logentry/formatted",
        "/logentry/message",
        "/message",
        "/exception/values/0/value",
        "/breadcrumbs/values/0/message",
        "/extra/message",
    ];
    for path in candidates {
        if let Some(s) = raw.pointer(path).and_then(|v| v.as_str()) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

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

        // 3. Try vector similarity search (fallback). The lookup must embed
        // the same text a group is created from (see `group_type_and_message`),
        // otherwise the vectors never match. The embedding only covers the
        // message, so the lookup is also restricted to the same exception
        // type: `TypeError: x is not a function` and `RangeError: x is not a
        // function` embed identically but are different errors.
        let (exception_type, exception_value) = group_type_and_message(&error_data);
        let embedding_text = exception_value.unwrap_or_else(|| exception_type.clone());

        if let Some(embedding) = self.create_embedding(&embedding_text) {
            if let Some(similar_group_id) = self
                .find_similar_group_by_embedding(&embedding, error_data.project_id, &exception_type)
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
        hex::encode(hasher.finalize())
    }

    /// Normalize error messages for consistent grouping
    /// Replaces dynamic values (IDs, UUIDs, numbers, paths, URLs) with placeholders
    fn normalize_error_message(&self, message: &str) -> String {
        use regex::Regex;
        use std::sync::LazyLock;

        static UUID_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}").unwrap()
        });
        static HEX_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"\b(0x)?[0-9a-f]{8,}\b").unwrap());
        static URL_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"https?://[\w./\-?=&%]+").unwrap());
        static EMAIL_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"\b[\w._%+-]+@[\w.-]+\.[a-z]{2,}\b").unwrap());
        static UNIX_PATH_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"/[\w/.]+\.[\w]+").unwrap());
        static WIN_PATH_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"[a-z]:\\[\w\\]+\.[\w]+").unwrap());
        static IP_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"\b\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}\b").unwrap());
        static TABLE_REF_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"\b(\w+)_\d+\b").unwrap());
        static NUMBER_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b\d{4,}\b").unwrap());
        static QUOTED_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r#"["']([^"']{10,})["']"#).unwrap());

        let mut normalized = message.to_lowercase();

        // Replace UUIDs (e.g., 550e8400-e29b-41d4-a716-446655440000) - FIRST
        normalized = UUID_RE.replace_all(&normalized, "<uuid>").to_string();
        // Replace hex IDs (e.g., 0x1a2b3c4d, deadbeef) - SECOND
        normalized = HEX_RE.replace_all(&normalized, "<hex_id>").to_string();
        // Replace URLs (http/https) - BEFORE paths
        normalized = URL_RE.replace_all(&normalized, "<url>").to_string();
        // Replace email addresses - BEFORE paths
        normalized = EMAIL_RE.replace_all(&normalized, "<email>").to_string();
        // Replace file paths (Unix and Windows style) - AFTER URLs/emails
        normalized = UNIX_PATH_RE.replace_all(&normalized, "<path>").to_string();
        normalized = WIN_PATH_RE.replace_all(&normalized, "<path>").to_string();
        // Replace IP addresses (v4) - BEFORE table refs and numbers
        normalized = IP_RE.replace_all(&normalized, "<ip>").to_string();
        // Replace database table references (table_123, users_456)
        normalized = TABLE_REF_RE
            .replace_all(&normalized, "${1}_<id>")
            .to_string();
        // Replace numeric IDs and timestamps (standalone numbers of 4+ digits)
        normalized = NUMBER_RE.replace_all(&normalized, "<num>").to_string();
        // Replace quoted strings (often dynamic user input) - FINAL
        normalized = QUOTED_RE
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

    /// Public wrapper for checking if a fingerprint already exists.
    /// Returns the group_id if found.
    pub async fn find_group_by_fingerprint_public(
        &self,
        fingerprint: &str,
        project_id: i32,
    ) -> Option<i32> {
        self.find_group_by_fingerprint(fingerprint, project_id)
            .await
            .ok()
            .flatten()
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
    /// of the same `error_type`: the embedding is built from the message alone.
    async fn find_similar_group_by_embedding(
        &self,
        embedding: &error_groups::PgVector,
        project_id: i32,
        error_type: &str,
    ) -> Result<Option<i32>, ErrorTrackingError> {
        #[derive(Debug, FromQueryResult)]
        struct SimilarGroup {
            id: i32,
            // pgvector's `<=>` returns `double precision`: decoding it as
            // `f32` failed every lookup that matched a row, which rejected
            // the event instead of grouping it.
            distance: f64,
        }

        // Cosine distance threshold, bound as FLOAT8 like the `<=>` result.
        const SIMILARITY_THRESHOLD: f64 = 0.15;

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
              AND error_type = $4
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
                    error_type.into(),
                ],
            ))
            .one(self.db.as_ref())
            .await?;

        if let Some(group) = &result {
            tracing::debug!(
                group_id = group.id,
                distance = group.distance,
                "error grouped by embedding similarity"
            );
        }
        Ok(result.map(|r| r.id))
    }

    /// Create a new error group with embedding
    async fn create_error_group(
        &self,
        error_data: &CreateErrorEventData,
        _fingerprint: &str,
    ) -> Result<i32, ErrorTrackingError> {
        let (exception_type, exception_value) = group_type_and_message(error_data);

        let title = match exception_value.as_deref() {
            Some(v) if !v.trim().is_empty() => format!(
                "{}: {}",
                exception_type,
                v.chars().take(100).collect::<String>()
            ),
            _ => exception_type.clone(),
        };
        // `exception_value` flows on into the embedding; fall back to the
        // type so we still get a meaningful similarity vector.
        let exception_value = exception_value.unwrap_or_else(|| exception_type.clone());

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

        // Promote the OTel trace_id (if any) from the JSONB data blob to the
        // top-level `trace_id_indexed` column so the unified Observe view can
        // join error rows to their originating request/span without a JSON
        // probe. Probes both the Sentry layout (`contexts.trace.trace_id`)
        // and our internal layout (`trace.trace_id`); validates 32-hex
        // before persisting.
        let trace_id_indexed = extract_trace_id_from_data(&data_json);

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
            trace_id_indexed: Set(trace_id_indexed),
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
    use super::*;
    use sea_orm::PaginatorTrait;
    use std::sync::Arc;
    use temps_database::test_utils::TestDatabase;
    use temps_entities::{error_events, error_groups, projects};

    async fn setup_test_db() -> TestDatabase {
        TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database")
    }

    async fn create_test_project(db: &Arc<DatabaseConnection>) -> i32 {
        use temps_entities::preset::Preset;
        use uuid::Uuid;

        let unique_slug = format!("test-project-{}", Uuid::new_v4());
        let project = projects::ActiveModel {
            name: Set("Test Project".to_string()),
            repo_name: Set("test-repo".to_string()),
            repo_owner: Set("test-owner".to_string()),
            directory: Set("/test".to_string()),
            main_branch: Set("main".to_string()),
            slug: Set(unique_slug),
            preset: Set(Preset::NextJs),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            ..Default::default()
        };

        project
            .insert(db.as_ref())
            .await
            .expect("Failed to create project")
            .id
    }

    fn create_test_error_data(project_id: i32) -> CreateErrorEventData {
        CreateErrorEventData {
            source: Some("test".to_string()),
            exception_type: Some("TypeError".to_string()),
            exception_value: Some("Cannot read property 'foo' of undefined".to_string()),
            stack_trace: Some(serde_json::json!([
                {
                    "filename": "/app/index.js",
                    "function": "doSomething",
                    "lineno": 42
                }
            ])),
            project_id,
            ..Default::default()
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_process_error_event_creates_new_group() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorIngestionService::new(db.clone());

        let project_id = create_test_project(&db).await;
        let error_data = create_test_error_data(project_id);

        let group_id = service
            .process_error_event(error_data)
            .await
            .expect("Failed to process error event");

        // Verify group was created
        let group = error_groups::Entity::find_by_id(group_id)
            .one(db.as_ref())
            .await
            .expect("Failed to fetch group")
            .expect("Group not found");

        assert_eq!(group.project_id, project_id);
        assert_eq!(group.total_count, 1);
        assert_eq!(group.status, "unresolved");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_process_error_event_groups_similar_errors() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorIngestionService::new(db.clone());

        let project_id = create_test_project(&db).await;

        // Process first error
        let error_data1 = create_test_error_data(project_id);
        let group_id1 = service
            .process_error_event(error_data1.clone())
            .await
            .expect("Failed to process first error");

        // Process second identical error
        let error_data2 = error_data1.clone();
        let group_id2 = service
            .process_error_event(error_data2)
            .await
            .expect("Failed to process second error");

        // Should be grouped together
        assert_eq!(group_id1, group_id2);

        // Verify count was incremented
        let group = error_groups::Entity::find_by_id(group_id1)
            .one(db.as_ref())
            .await
            .expect("Failed to fetch group")
            .expect("Group not found");

        assert_eq!(group.total_count, 2);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_generate_fingerprint_is_consistent() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorIngestionService::new(db);

        let error_data = CreateErrorEventData {
            exception_type: Some("TypeError".to_string()),
            exception_value: Some("Test error".to_string()),
            stack_trace: Some(serde_json::json!([{"filename": "test.js", "function": "test"}])),
            project_id: 1,
            ..Default::default()
        };

        let fingerprint1 = service.generate_fingerprint(&error_data);
        let fingerprint2 = service.generate_fingerprint(&error_data);

        assert_eq!(fingerprint1, fingerprint2);
        assert!(!fingerprint1.is_empty());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_generate_fingerprint_differs_for_different_errors() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorIngestionService::new(db);

        let error_data1 = CreateErrorEventData {
            exception_type: Some("TypeError".to_string()),
            exception_value: Some("Error 1".to_string()),
            stack_trace: None,
            project_id: 1,
            ..Default::default()
        };

        let error_data2 = CreateErrorEventData {
            exception_type: Some("ReferenceError".to_string()),
            exception_value: Some("Error 2".to_string()),
            stack_trace: None,
            project_id: 1,
            ..Default::default()
        };

        let fingerprint1 = service.generate_fingerprint(&error_data1);
        let fingerprint2 = service.generate_fingerprint(&error_data2);

        assert_ne!(fingerprint1, fingerprint2);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_normalize_error_message() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorIngestionService::new(db);

        let message1 = "Error: Connection failed at line 123";
        let message2 = "ERROR: CONNECTION FAILED AT LINE 123";

        let normalized1 = service.normalize_error_message(message1);
        let normalized2 = service.normalize_error_message(message2);

        assert_eq!(normalized1, normalized2);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_process_error_event_creates_event() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorIngestionService::new(db.clone());

        let project_id = create_test_project(&db).await;
        let error_data = create_test_error_data(project_id);

        let group_id = service
            .process_error_event(error_data)
            .await
            .expect("Failed to process error event");

        // Verify event was created
        let events = error_events::Entity::find()
            .filter(error_events::Column::ErrorGroupId.eq(group_id))
            .all(db.as_ref())
            .await
            .expect("Failed to fetch events");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].exception_type, "TypeError");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_extract_stack_signature() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorIngestionService::new(db);

        let stack_trace = Some(serde_json::json!([
            {"filename": "/app/src/index.js", "function": "main"},
            {"filename": "/app/src/utils.js", "function": "helper"},
            {"filename": "/app/src/lib.js", "function": "doWork"},
        ]));

        let signature = service.extract_stack_signature(&stack_trace, 3);

        assert!(signature.contains("index.js"));
        assert!(signature.contains("main"));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_normalize_error_message_replaces_uuids() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorIngestionService::new(db);

        let message = "Error: Resource 550e8400-e29b-41d4-a716-446655440000 not found";
        let normalized = service.normalize_error_message(message);

        assert!(normalized.contains("<uuid>"));
        assert!(!normalized.contains("550e8400"));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_normalize_error_message_replaces_hex_ids() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorIngestionService::new(db);

        let message = "Error: Transaction 0xdeadbeef1234 failed";
        let normalized = service.normalize_error_message(message);

        assert!(normalized.contains("<hex_id>"));
        assert!(!normalized.contains("deadbeef"));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_normalize_error_message_replaces_numbers() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorIngestionService::new(db);

        let message = "Error: User 123456 failed to authenticate";
        let normalized = service.normalize_error_message(message);

        assert!(normalized.contains("<num>"));
        assert!(!normalized.contains("123456"));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_normalize_error_message_replaces_paths() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorIngestionService::new(db);

        let message1 = "Error: Cannot read /home/user/app/config.json";
        let normalized1 = service.normalize_error_message(message1);
        assert!(normalized1.contains("<path>"));

        let message2 = "Error: File C:\\Users\\Admin\\file.txt not found";
        let normalized2 = service.normalize_error_message(message2);
        assert!(normalized2.contains("<path>"));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_normalize_error_message_replaces_urls() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorIngestionService::new(db);

        let message = "Error: Failed to fetch https://api.example.com/users/123";
        let normalized = service.normalize_error_message(message);

        assert!(normalized.contains("<url>"));
        assert!(!normalized.contains("example.com"));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_normalize_error_message_replaces_emails() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorIngestionService::new(db);

        let message = "Error: Email user@example.com already exists";
        let normalized = service.normalize_error_message(message);

        assert!(normalized.contains("<email>"));
        assert!(!normalized.contains("user@example"));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_normalize_error_message_replaces_ips() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorIngestionService::new(db);

        let message = "Error: Connection to 192.168.1.100 timeout";
        let normalized = service.normalize_error_message(message);

        assert!(normalized.contains("<ip>"));
        assert!(!normalized.contains("192.168"));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_normalize_error_message_replaces_table_refs() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorIngestionService::new(db);

        let message = "Error: Foreign key constraint failed on table users_123";
        let normalized = service.normalize_error_message(message);

        assert!(normalized.contains("users_<id>"));
        assert!(!normalized.contains("users_123"));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_normalize_error_message_groups_similar_errors() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorIngestionService::new(db);

        // These errors should normalize to the same message
        let message1 = "Error: User 12345 not found at 192.168.1.100";
        let message2 = "Error: User 67890 not found at 10.0.0.5";
        let message3 = "Error: User 99999 not found at 172.16.0.1";

        let normalized1 = service.normalize_error_message(message1);
        let normalized2 = service.normalize_error_message(message2);
        let normalized3 = service.normalize_error_message(message3);

        // All should normalize to the same pattern
        assert_eq!(normalized1, normalized2);
        assert_eq!(normalized2, normalized3);
        assert!(normalized1.contains("<num>"));
        assert!(normalized1.contains("<ip>"));
    }

    /// Same message, different stack: the fingerprints differ, so grouping has to
    /// go through the pgvector similarity lookup.
    fn error_data_with_frame(
        project_id: i32,
        filename: &str,
        function: &str,
    ) -> CreateErrorEventData {
        CreateErrorEventData {
            stack_trace: Some(serde_json::json!([
                {
                    "filename": filename,
                    "function": function,
                    "lineno": 7
                }
            ])),
            ..create_test_error_data(project_id)
        }
    }

    /// Regression: `embedding <=> $1::vector` is FLOAT8, and decoding it into an
    /// `f32` failed the whole ingest with a ColumnDecode error as soon as the
    /// similarity query matched a row, so the event was dropped.
    #[tokio::test]
    #[serial_test::serial]
    async fn test_process_error_event_groups_by_embedding_when_fingerprint_differs() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorIngestionService::new(db.clone());
        let project_id = create_test_project(&db).await;

        let first = error_data_with_frame(project_id, "/app/a.js", "handlerA");
        let second = error_data_with_frame(project_id, "/app/b.js", "handlerB");
        assert_ne!(
            service.generate_fingerprint(&first),
            service.generate_fingerprint(&second),
            "precondition: the two events must not share a fingerprint"
        );

        let group_a = service
            .process_error_event(first)
            .await
            .expect("first event must be stored");
        let group_b = service
            .process_error_event(second)
            .await
            .expect("an event matched by embedding similarity must be stored, not rejected");

        assert_eq!(
            group_a, group_b,
            "similar event must join the existing group"
        );

        let group = error_groups::Entity::find_by_id(group_a)
            .one(db.as_ref())
            .await
            .expect("Failed to fetch group")
            .expect("Group not found");
        assert_eq!(group.total_count, 2);

        let events = error_events::Entity::find()
            .filter(error_events::Column::ErrorGroupId.eq(group_a))
            .count(db.as_ref())
            .await
            .expect("Failed to count events");
        assert_eq!(events, 2, "both events must be persisted");
    }

    /// Resolved groups are excluded from the similarity lookup: a similar event
    /// opens a new group instead of reviving the resolved one.
    #[tokio::test]
    #[serial_test::serial]
    async fn test_similarity_lookup_skips_resolved_groups() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorIngestionService::new(db.clone());
        let project_id = create_test_project(&db).await;

        let group_a = service
            .process_error_event(error_data_with_frame(project_id, "/app/a.js", "handlerA"))
            .await
            .expect("first event must be stored");

        let mut resolved: error_groups::ActiveModel = error_groups::Entity::find_by_id(group_a)
            .one(db.as_ref())
            .await
            .expect("Failed to fetch group")
            .expect("Group not found")
            .into();
        resolved.status = Set("resolved".to_string());
        resolved
            .update(db.as_ref())
            .await
            .expect("Failed to resolve group");

        let group_b = service
            .process_error_event(error_data_with_frame(project_id, "/app/b.js", "handlerB"))
            .await
            .expect("second event must be stored");

        assert_ne!(
            group_a, group_b,
            "a resolved group must not absorb new events"
        );
    }

    /// The similarity lookup is scoped to the event's project.
    #[tokio::test]
    #[serial_test::serial]
    async fn test_similarity_lookup_is_scoped_to_project() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorIngestionService::new(db.clone());
        let project_a = create_test_project(&db).await;
        let project_b = create_test_project(&db).await;

        let group_a = service
            .process_error_event(error_data_with_frame(project_a, "/app/a.js", "handlerA"))
            .await
            .expect("first event must be stored");
        let group_b = service
            .process_error_event(error_data_with_frame(project_b, "/app/b.js", "handlerB"))
            .await
            .expect("second event must be stored");

        assert_ne!(
            group_a, group_b,
            "groups must never be shared across projects"
        );
    }

    /// The embedding covers the message only, so two exception types with the
    /// same message embed identically; the lookup must keep them apart.
    #[tokio::test]
    #[serial_test::serial]
    async fn test_similarity_lookup_requires_same_error_type() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorIngestionService::new(db.clone());
        let project_id = create_test_project(&db).await;

        let type_error = error_data_with_frame(project_id, "/app/a.js", "handlerA");
        let range_error = CreateErrorEventData {
            exception_type: Some("RangeError".to_string()),
            ..error_data_with_frame(project_id, "/app/b.js", "handlerB")
        };

        let group_a = service
            .process_error_event(type_error)
            .await
            .expect("first event must be stored");
        let group_b = service
            .process_error_event(range_error)
            .await
            .expect("second event must be stored");

        assert_ne!(
            group_a, group_b,
            "a RangeError must not join a TypeError group with the same message"
        );
    }

    /// Sentry-style payloads carry an `exceptions` array. Same message, different
    /// frames: the second event must join the first group through the similarity
    /// lookup, which exercises the pgvector distance decode on that path too.
    #[tokio::test]
    #[serial_test::serial]
    async fn test_similarity_grouping_for_exception_array_payloads() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorIngestionService::new(db.clone());
        let project_id = create_test_project(&db).await;

        let event = |filename: &str| CreateErrorEventData {
            exceptions: vec![super::super::types::ExceptionData {
                exception_type: "RangeError".to_string(),
                exception_value: Some("Maximum call stack size exceeded".to_string()),
                stack_trace: Some(serde_json::json!([
                    { "filename": filename, "function": "recurse", "lineno": 3 }
                ])),
                mechanism: None,
                module: None,
                thread_id: None,
            }],
            source: Some("test".to_string()),
            project_id,
            ..Default::default()
        };

        let first = event("/app/a.js");
        let second = event("/app/b.js");
        assert_ne!(
            service.generate_fingerprint(&first),
            service.generate_fingerprint(&second),
            "precondition: the two events must not share a fingerprint"
        );

        let group_a = service
            .process_error_event(first)
            .await
            .expect("first event must be stored");
        let group_b = service
            .process_error_event(second)
            .await
            .expect("an event matched by embedding similarity must be stored, not rejected");
        assert_eq!(group_a, group_b);
    }
}

#[cfg(test)]
mod trace_id_extraction_tests {
    use super::extract_trace_id_from_data;
    use serde_json::json;

    #[test]
    fn extracts_from_sentry_wrapped_layout() {
        let data = json!({
            "source": "sentry",
            "sentry": {
                "contexts": {
                    "trace": { "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736" }
                }
            }
        });
        assert_eq!(
            extract_trace_id_from_data(&data),
            Some("4bf92f3577b34da6a3ce929d0e0e4736".to_string())
        );
    }

    #[test]
    fn extracts_from_top_level_contexts_layout() {
        let data = json!({
            "contexts": {
                "trace": { "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736" }
            }
        });
        assert_eq!(
            extract_trace_id_from_data(&data),
            Some("4bf92f3577b34da6a3ce929d0e0e4736".to_string())
        );
    }

    #[test]
    fn extracts_from_internal_trace_layout() {
        let data = json!({
            "trace": { "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736" }
        });
        assert_eq!(
            extract_trace_id_from_data(&data),
            Some("4bf92f3577b34da6a3ce929d0e0e4736".to_string())
        );
    }

    #[test]
    fn returns_none_when_missing() {
        let data = json!({ "exception": "boom" });
        assert_eq!(extract_trace_id_from_data(&data), None);
    }

    #[test]
    fn rejects_invalid_lengths_and_chars() {
        let bad = json!({ "trace": { "trace_id": "deadbeef" } });
        assert_eq!(extract_trace_id_from_data(&bad), None);

        let nonhex = json!({ "trace": { "trace_id": "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz" } });
        assert_eq!(extract_trace_id_from_data(&nonhex), None);
    }

    #[test]
    fn rejects_all_zero_invalid_trace_id() {
        let data = json!({
            "trace": { "trace_id": "00000000000000000000000000000000" }
        });
        assert_eq!(extract_trace_id_from_data(&data), None);
    }

    #[test]
    fn lowercases_input() {
        let data = json!({
            "trace": { "trace_id": "4BF92F3577B34DA6A3CE929D0E0E4736" }
        });
        assert_eq!(
            extract_trace_id_from_data(&data),
            Some("4bf92f3577b34da6a3ce929d0e0e4736".to_string())
        );
    }
}
