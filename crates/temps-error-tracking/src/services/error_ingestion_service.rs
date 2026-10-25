// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use arc_swap::ArcSwapOption;
use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection, FromQueryResult, Set,
    Statement, TransactionTrait,
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

fn primary_exception(data: &CreateErrorEventData) -> Option<&super::types::ExceptionData> {
    if data.source.as_deref() == Some("sentry") {
        data.exceptions.last()
    } else {
        data.exceptions.first()
    }
}

fn sdk_fingerprint(data: &CreateErrorEventData) -> Option<Vec<&str>> {
    if data.source.as_deref() != Some("sentry") {
        return None;
    }
    let values = data
        .raw_sentry_event
        .as_ref()?
        .get("fingerprint")?
        .as_array()?;
    if values.is_empty() {
        return None;
    }
    values.iter().map(|v| v.as_str()).collect()
}

fn default_placeholder(value: &str) -> bool {
    matches!(value, "{{ default }}" | "{{default}}")
}

fn sdk_event_id(data: &CreateErrorEventData) -> Option<String> {
    if data.source.as_deref() != Some("sentry") {
        return None;
    }
    let id = data.raw_sentry_event.as_ref()?.get("event_id")?.as_str()?;
    (id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()) && id.bytes().any(|b| b != b'0'))
        .then(|| id.to_ascii_lowercase())
}

/// The exception type and message a group is created from, from the first
/// exception (last for Sentry chains) or else the legacy top-level fields. When the message is
/// missing (most `captureMessage` payloads and Sentry SDKs that send only an
/// event-level message), probe the raw event for a usable one so the title
/// doesn't render as the useless literal "Error: Unknown error".
///
/// The similarity lookup embeds the same text (message, else type), so an
/// incoming event is compared against what each group was embedded from.
fn group_type_and_message(error_data: &CreateErrorEventData) -> (String, Option<String>) {
    let raw_message = || extract_message_from_raw(error_data.raw_sentry_event.as_ref());
    match primary_exception(error_data) {
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

#[derive(FromQueryResult)]
struct ErrorChunkRange {
    range_start: DateTime<Utc>,
    range_end: DateTime<Utc>,
}

pub(super) struct IngestedError {
    pub group_id: i32,
    pub created: bool,
    pub previous_status: String,
    pub inserted: bool,
}

fn is_transaction_abort(error: &ErrorTrackingError) -> bool {
    use sea_orm::{sqlx::Error as SqlxError, DbErr, RuntimeErr};
    match error {
        ErrorTrackingError::Database(
            DbErr::Exec(RuntimeErr::SqlxError(SqlxError::Database(error)))
            | DbErr::Query(RuntimeErr::SqlxError(SqlxError::Database(error))),
        ) => matches!(error.code().as_deref(), Some("40P01" | "40001")),
        _ => false,
    }
}

/// Service for ingesting and processing error events
pub struct ErrorIngestionService {
    db: Arc<DatabaseConnection>,
    tokenizer: Arc<dyn Tokenizer>,
    // One lock-free range per service/database, never a per-project/key cache.
    chunk_range: ArcSwapOption<ErrorChunkRange>,
}

impl ErrorIngestionService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        // Use HashTokenizer with vocab_size=10000 for production
        let tokenizer = Arc::new(HashTokenizer::new(10000)) as Arc<dyn Tokenizer>;
        Self {
            db,
            tokenizer,
            chunk_range: ArcSwapOption::empty(),
        }
    }

    /// Create with custom tokenizer
    pub fn with_tokenizer(db: Arc<DatabaseConnection>, tokenizer: Arc<dyn Tokenizer>) -> Self {
        Self {
            db,
            tokenizer,
            chunk_range: ArcSwapOption::empty(),
        }
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
        Ok(self.process_with_outcome(error_data).await?.group_id)
    }

    pub(super) async fn process_with_outcome(
        &self,
        error_data: CreateErrorEventData,
    ) -> Result<IngestedError, ErrorTrackingError> {
        let fingerprint = self.generate_fingerprint(&error_data);
        self.process_with_fingerprint(error_data, fingerprint).await
    }

    /// The orchestrator may capture submitted-frame identity before symbolication.
    pub(super) async fn process_with_fingerprint(
        &self,
        error_data: CreateErrorEventData,
        fingerprint: String,
    ) -> Result<IngestedError, ErrorTrackingError> {
        let mut attempt = 0;
        loop {
            let result = self.process_once(&error_data, &fingerprint).await;
            match &result {
                Err(error) if attempt < 4 && is_transaction_abort(error) => {
                    // Timescale's first-chunk FK creation can deadlock concurrent
                    // group inserts. PostgreSQL explicitly aborted this entire
                    // transaction, so retrying cannot duplicate an event/count.
                    // Connection/unknown-commit failures are never retried here.
                    self.chunk_range.store(None);
                    let delay = 50 * (1u64 << attempt) + u64::from(fingerprint.as_bytes()[0] % 17);
                    attempt += 1;
                    tracing::debug!(attempt, "Retrying aborted error ingestion transaction");
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                }
                _ => return result,
            }
        }
    }

    async fn process_once(
        &self,
        error_data: &CreateErrorEventData,
        fingerprint: &str,
    ) -> Result<IngestedError, ErrorTrackingError> {
        let event_time = Utc::now();
        let tx = self.db.begin().await?;
        let source_id = sdk_event_id(error_data);
        if let Some(id) = &source_id {
            tx.execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT pg_advisory_xact_lock(hashtextextended($1,$2))",
                [
                    format!("sentry-event:{id}").into(),
                    i64::from(error_data.project_id).into(),
                ],
            ))
            .await?;
            let cached=tx.query_one(Statement::from_sql_and_values(DatabaseBackend::Postgres,
                "SELECT error_group_id,environment_id,deployment_id,error_event_id,received_at FROM error_event_source_keys WHERE project_id=$1 AND sdk_event_id=$2 AND received_at >= NOW() - INTERVAL '7 days'",
                [error_data.project_id.into(),id.clone().into()])).await?;
            let needs_cache = cached.is_none();
            let previous=match cached {
                Some(row)=>Some(row),
                None=>tx.query_one(Statement::from_sql_and_values(DatabaseBackend::Postgres,
                    "SELECT error_group_id,environment_id,deployment_id,id AS error_event_id,timestamp AS received_at FROM error_events WHERE project_id=$1 AND data #>> '{sentry,event_id}'=$2 AND timestamp >= NOW() - INTERVAL '7 days' ORDER BY timestamp,id LIMIT 1",
                    [error_data.project_id.into(),id.clone().into()])).await?,
            };
            if let Some(row) = previous {
                let environment: Option<i32> = row.try_get("", "environment_id")?;
                let deployment: Option<i32> = row.try_get("", "deployment_id")?;
                if environment != error_data.environment_id
                    || deployment != error_data.deployment_id
                {
                    return Err(ErrorTrackingError::Validation(
                        "Sentry event identity conflicts with its authenticated scope".into(),
                    ));
                }
                let group_id: i32 = row.try_get("", "error_group_id")?;
                if needs_cache {
                    let record_id: i64 = row.try_get("", "error_event_id")?;
                    let received_at: DateTime<Utc> = row.try_get("", "received_at")?;
                    tx.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
                        "INSERT INTO error_event_source_keys(project_id,sdk_event_id,environment_id,deployment_id,error_group_id,error_event_id,received_at) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT DO NOTHING",
                        [error_data.project_id.into(),id.clone().into(),environment.into(),deployment.into(),group_id.into(),record_id.into(),received_at.into()])).await?;
                }
                tx.commit().await?;
                return Ok(IngestedError {
                    group_id,
                    created: false,
                    previous_status: String::new(),
                    inserted: false,
                });
            }
        }
        // Acquire the cold-chunk gate BEFORE group/FK writes. Timescale clones
        // child FKs during chunk creation and needs parent table locks.
        let refresh_chunk = self.enter_chunk_gate(&tx, event_time).await?;
        // A transaction-scoped PostgreSQL lock works across instances, releases
        // on rollback, and allocates no application-side per-fingerprint state.
        // Hash collisions only serialize unrelated fingerprints, never merge them.
        tx.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT pg_advisory_xact_lock(hashtextextended($1, $2))",
            [fingerprint.into(), i64::from(error_data.project_id).into()],
        ))
        .await?;
        let existing = self
            .find_group_by_fingerprint_on(&tx, fingerprint, error_data.project_id)
            .await?;
        let mut created = false;
        let group_id = if let Some(id) = existing {
            id
        } else {
            let (exception_type, exception_value) = group_type_and_message(error_data);
            let text = exception_value.unwrap_or_else(|| exception_type.clone());
            let similar = if error_data.source.as_deref() == Some("sentry") {
                None
            } else if let Some(embedding) = self.create_embedding(&text) {
                self.find_similar_group_by_embedding_on(
                    &tx,
                    &embedding,
                    error_data.project_id,
                    &exception_type,
                )
                .await?
            } else {
                None
            };
            match similar {
                Some(id) => id,
                None => {
                    created = true;
                    self.create_error_group_on(&tx, error_data, fingerprint, event_time)
                        .await?
                }
            }
        };
        let previous_status: String = tx
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT status FROM error_groups WHERE id=$1 FOR UPDATE",
                [group_id.into()],
            ))
            .await?
            .ok_or(ErrorTrackingError::GroupNotFound)?
            .try_get("", "status")?;
        tx.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "INSERT INTO error_group_fingerprints(project_id,fingerprint_hash,error_group_id) VALUES($1,$2,$3) ON CONFLICT DO NOTHING",
            [error_data.project_id.into(),fingerprint.into(),group_id.into()])).await?;
        let event_id = self
            .create_error_event_on(&tx, error_data, group_id, fingerprint, event_time)
            .await?;
        if let Some(id) = source_id {
            tx.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
                "INSERT INTO error_event_source_keys(project_id,sdk_event_id,environment_id,deployment_id,error_group_id,error_event_id,received_at) VALUES($1,$2,$3,$4,$5,$6,NOW()) ON CONFLICT(project_id,sdk_event_id) DO UPDATE SET environment_id=EXCLUDED.environment_id,deployment_id=EXCLUDED.deployment_id,error_group_id=EXCLUDED.error_group_id,error_event_id=EXCLUDED.error_event_id,received_at=EXCLUDED.received_at",
                [error_data.project_id.into(),id.into(),error_data.environment_id.into(),error_data.deployment_id.into(),group_id.into(),event_id.into()])).await?;
        }
        tx.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "UPDATE error_groups SET total_count = total_count + 1, last_seen = GREATEST(last_seen, $2), updated_at = NOW(), status = CASE WHEN status IN ('resolved','ignored') THEN 'unresolved' ELSE status END WHERE id = $1",
            [group_id.into(),event_time.into()])).await?;
        let observed_chunk = if refresh_chunk {
            Self::lookup_chunk(&tx, event_time).await?
        } else {
            None
        };
        tx.commit().await?;
        if let Some(range) = observed_chunk {
            self.chunk_range.store(Some(Arc::new(range)));
        }
        Ok(IngestedError {
            group_id,
            created,
            previous_status,
            inserted: true,
        })
    }

    // error_events also hash-partitions on id. A time range is warm only
    // after every space partition exists; one chunk alone is insufficient.
    async fn lookup_chunk<C: ConnectionTrait>(
        db: &C,
        at: DateTime<Utc>,
    ) -> Result<Option<ErrorChunkRange>, ErrorTrackingError> {
        Ok(ErrorChunkRange::find_by_statement(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "SELECT range_start,range_end FROM timescaledb_information.chunks WHERE hypertable_schema=current_schema() AND hypertable_name='error_events' AND range_start <= $1 AND range_end > $1 GROUP BY range_start,range_end HAVING count(*) >= COALESCE((SELECT max(num_partitions) FROM timescaledb_information.dimensions WHERE hypertable_schema=current_schema() AND hypertable_name='error_events'),1) LIMIT 1",[at.into()])).one(db).await?)
    }

    async fn enter_chunk_gate<C: ConnectionTrait>(
        &self,
        db: &C,
        at: DateTime<Utc>,
    ) -> Result<bool, ErrorTrackingError> {
        if self
            .chunk_range
            .load()
            .as_ref()
            .is_some_and(|range| at >= range.range_start && at < range.range_end)
        {
            return Ok(false);
        }
        if let Some(range) = Self::lookup_chunk(db, at).await? {
            self.chunk_range.store(Some(Arc::new(range)));
            return Ok(false);
        }
        // Shared across projects and processes, scoped to this schema. Cold
        // contenders wait without holding parent/group write locks. Steady-state
        // requests use the single cached range and never pay this query/lock.
        db.execute(Statement::from_string(DatabaseBackend::Postgres,
            "SELECT pg_advisory_xact_lock(hashtextextended(current_schema() || '.temps.error_events.chunk',0))")).await?;
        Ok(true)
    }

    /// Bounded indexed cleanup; a quiet instance performs no payload scans.
    pub async fn cleanup_source_keys(&self) -> Result<u64, ErrorTrackingError> {
        Ok(self.db.execute(Statement::from_string(DatabaseBackend::Postgres,
            "DELETE FROM error_event_source_keys WHERE (project_id,sdk_event_id) IN (SELECT project_id,sdk_event_id FROM error_event_source_keys WHERE received_at < NOW() - INTERVAL '7 days' ORDER BY received_at LIMIT 2000)")).await?.rows_affected())
    }

    /// Generate a fingerprint for error matching
    pub fn generate_fingerprint(&self, error_data: &CreateErrorEventData) -> String {
        // Use first exception for fingerprint, or fall back to legacy fields
        let (exception_type, exception_value, stack_trace) =
            if let Some(first_exception) = primary_exception(error_data) {
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

        let signature = if error_data.source.as_deref() == Some("sentry") {
            let frames = stack_trace.as_ref().and_then(|v| {
                v.as_array()
                    .or_else(|| v.get("frames").and_then(|v| v.as_array()))
            });
            match frames {
                Some(frames) => {
                    let app_only = frames
                        .iter()
                        .any(|v| v.get("in_app").and_then(|v| v.as_bool()) == Some(true));
                    let signature = frames
                        .iter()
                        .rev()
                        .filter(|v| {
                            !app_only || v.get("in_app").and_then(|v| v.as_bool()) == Some(true)
                        })
                        .filter_map(|frame| {
                            let filename = frame
                                .get("filename")
                                .or_else(|| frame.get("abs_path"))
                                .and_then(|v| v.as_str());
                            let function = frame.get("function").and_then(|v| v.as_str());
                            if filename.is_none() && function.is_none() {
                                return None;
                            }
                            Some(format!(
                                "{}:{}",
                                self.normalize_filename(filename.unwrap_or("unknown")),
                                function.unwrap_or("anonymous")
                            ))
                        })
                        .take(3)
                        .collect::<Vec<_>>()
                        .join("|");
                    if signature.is_empty() {
                        "unknown".into()
                    } else {
                        signature
                    }
                }
                None => "unknown".into(),
            }
        } else {
            self.extract_stack_signature(stack_trace, 3)
        };
        let template = error_data
            .raw_sentry_event
            .as_ref()
            .and_then(|v| v.pointer("/logentry/message"))
            .and_then(|v| v.as_str())
            .unwrap_or(&exception_value);
        let components = if error_data.source.as_deref() == Some("sentry") && signature != "unknown"
        {
            vec![exception_type, signature]
        } else {
            vec![
                exception_type,
                self.normalize_error_message(template),
                signature,
            ]
        };
        let mut hasher = Sha256::new();
        hasher.update(components.join("||").as_bytes());
        let default = hex::encode(hasher.finalize());
        match sdk_fingerprint(error_data) {
            None => default,
            Some(values) if values.len() == 1 && default_placeholder(values[0]) => default,
            Some(values) => {
                // Length framing preserves component boundaries without a JSON
                // allocation or ambiguous delimiter joins.
                let mut hasher = Sha256::new();
                hasher.update(b"temps.sentry.fingerprint.v1\0");
                for value in values {
                    let value = if default_placeholder(value) {
                        default.as_str()
                    } else {
                        value
                    };
                    hasher.update((value.len() as u64).to_be_bytes());
                    hasher.update(value.as_bytes());
                }
                hex::encode(hasher.finalize())
            }
        }
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
        self.find_group_by_fingerprint_on(self.db.as_ref(), fingerprint, project_id)
            .await
    }

    async fn find_group_by_fingerprint_on<C: ConnectionTrait>(
        &self,
        db: &C,
        fingerprint: &str,
        project_id: i32,
    ) -> Result<Option<i32>, ErrorTrackingError> {
        if let Some(row)=db.query_one(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "SELECT error_group_id FROM error_group_fingerprints WHERE project_id=$1 AND fingerprint_hash=$2",
            [project_id.into(),fingerprint.into()])).await? {
            return Ok(Some(row.try_get("","error_group_id")?));
        }
        // Legacy fingerprints not yet indexed are resolved once and cached by
        // process_error_event. The lowest group ID keeps future events stable
        // without merging historical issues or changing their triage state.
        // error_events can outlive its group (compressed hypertable chunks), so
        // only a group that still exists in this project is a valid answer;
        // otherwise the caller's group lock would fail with GroupNotFound on
        // every event carrying this fingerprint.
        let row = db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT e.error_group_id FROM error_events e JOIN error_groups g ON g.id=e.error_group_id AND g.project_id=e.project_id WHERE e.project_id=$1 AND e.fingerprint_hash=$2 ORDER BY e.error_group_id LIMIT 1",
                [project_id.into(), fingerprint.into()],
            ))
            .await?;
        row.map(|row| row.try_get("", "error_group_id"))
            .transpose()
            .map_err(Into::into)
    }

    /// Find similar error group using vector cosine similarity
    ///
    /// Uses pgvector's cosine distance operator (<=>)
    /// Hardcoded similarity threshold: 0.15 (lower = more similar, 0 = identical)
    ///
    /// Only searches unresolved and assigned groups (excludes resolved and ignored)
    /// of the same `error_type`: the embedding is built from the message alone.
    async fn find_similar_group_by_embedding_on<C: ConnectionTrait>(
        &self,
        db: &C,
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
            .one(db)
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
    async fn create_error_group_on<C: ConnectionTrait>(
        &self,
        db: &C,
        error_data: &CreateErrorEventData,
        _fingerprint: &str,
        event_time: DateTime<Utc>,
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
            first_seen: Set(event_time),
            last_seen: Set(event_time),
            total_count: Set(0),
            status: Set("unresolved".to_string()),
            assigned_to: Set(None),
            project_id: Set(error_data.project_id),
            environment_id: Set(error_data.environment_id),
            deployment_id: Set(error_data.deployment_id),
            visitor_id: Set(error_data.visitor_id),
            created_at: Set(event_time),
            updated_at: Set(event_time),
            ..Default::default()
        };

        let group = new_group.insert(db).await?;
        Ok(group.id)
    }

    /// Create an error event within a group
    async fn create_error_event_on<C: ConnectionTrait>(
        &self,
        db: &C,
        error_data: &CreateErrorEventData,
        group_id: i32,
        fingerprint: &str,
        event_time: DateTime<Utc>,
    ) -> Result<i64, ErrorTrackingError> {
        // Populate native typed context for every source, while retaining the
        // original Sentry document for wire fields without a native equivalent.
        let data_json = {
            use temps_entities::error_events::{
                DeviceContext, EnvironmentContext, ErrorEventData, RequestContext, StackFrame,
                TraceContext, UserContext,
            };

            // Build structured data from individual fields (for non-Sentry sources)
            let event_data = ErrorEventData {
                source: Some(error_data.source.clone().unwrap_or_else(|| "custom".into())),
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
                sentry: error_data.raw_sentry_event.clone(),
            };
            event_data
                .to_json_value()
                .unwrap_or_else(|| serde_json::json!({}))
        };

        // Preserve legacy ordering; Sentry chains end with the primary exception.
        let (event_exception_type, event_exception_value) =
            if let Some(first_exception) = primary_exception(error_data) {
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
            timestamp: Set(event_time),
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

        let event = new_event.insert(db).await?;
        Ok(event.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn concurrent_distinct_groups_survive_first_timescale_chunk_creation() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let first = create_test_project(&db).await;
        let second = create_test_project(&db).await;
        let service = Arc::new(ErrorIngestionService::new(db.clone()));
        let mut tasks = tokio::task::JoinSet::new();
        for index in 0..8 {
            let service = service.clone();
            tasks.spawn(async move {
                let mut data = create_test_error_data(if index % 2 == 0 { first } else { second });
                data.exception_type = Some(format!("SyntheticType{index}"));
                service.process_error_event(data).await.unwrap()
            });
        }
        while let Some(result) = tasks.join_next().await {
            result.unwrap();
        }
        let groups = error_groups::Entity::find().all(db.as_ref()).await.unwrap();
        assert_eq!(groups.len(), 8);
        assert!(groups.iter().all(|group| group.total_count == 1));
        assert_eq!(
            error_events::Entity::find()
                .count(db.as_ref())
                .await
                .unwrap(),
            8
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn concurrent_first_and_repeated_events_have_one_group_and_exact_count() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let project = create_test_project(&db).await;
        let other_project = create_test_project(&db).await;
        let service = Arc::new(ErrorIngestionService::new(db.clone()));
        let mut created_count = 0;
        let mut regressions = 0;
        for wave in 0..2 {
            if wave == 1 {
                db.execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "UPDATE error_groups SET status='resolved' WHERE project_id=$1",
                    [project.into()],
                ))
                .await
                .unwrap();
            }
            let mut tasks = tokio::task::JoinSet::new();
            for _ in 0..24 {
                let service = service.clone();
                tasks.spawn(async move {
                    service
                        .process_with_outcome(create_test_error_data(project))
                        .await
                        .unwrap()
                });
            }
            while let Some(result) = tasks.join_next().await {
                let outcome = result.unwrap();
                if outcome.created {
                    created_count += 1;
                }
                if outcome.previous_status == "resolved" {
                    regressions += 1;
                }
            }
        }
        assert_eq!(
            created_count, 1,
            "only the transaction creating the group owns its new-issue outcome"
        );
        assert_eq!(
            regressions, 1,
            "only one concurrent event reopens a resolved issue"
        );
        let groups = error_groups::Entity::find()
            .filter(error_groups::Column::ProjectId.eq(project))
            .all(db.as_ref())
            .await
            .unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].total_count, 48);
        assert_eq!(
            error_events::Entity::find()
                .filter(error_events::Column::ErrorGroupId.eq(groups[0].id))
                .count(db.as_ref())
                .await
                .unwrap(),
            48
        );
        let isolated = service
            .process_error_event(create_test_error_data(other_project))
            .await
            .unwrap();
        assert_ne!(isolated, groups[0].id);
        db.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE error_groups SET total_count=2 WHERE id=$1",
            [groups[0].id.into()],
        ))
        .await
        .unwrap();
        use temps_migrations::MigrationTrait;
        temps_migrations::ReconcileErrorGroupCountsMigration
            .up(&temps_migrations::SchemaManager::new(db.as_ref()))
            .await
            .unwrap();
        assert_eq!(
            error_groups::Entity::find_by_id(groups[0].id)
                .one(db.as_ref())
                .await
                .unwrap()
                .unwrap()
                .total_count,
            48
        );
        // A failed event insert rolls back both its new group and its counter.
        let mut invalid = create_test_error_data(project);
        invalid.exception_type = Some("RollbackSyntheticError".into());
        invalid.raw_sentry_event =
            Some(serde_json::json!({"message":"invalid \u{0000} JSONB text"}));
        assert!(service.process_error_event(invalid).await.is_err());
        assert_eq!(
            error_groups::Entity::find()
                .filter(error_groups::Column::ProjectId.eq(project))
                .count(db.as_ref())
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            error_groups::Entity::find_by_id(groups[0].id)
                .one(db.as_ref())
                .await
                .unwrap()
                .unwrap()
                .total_count,
            48
        );
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
    async fn legacy_fingerprint_fallback_ignores_events_of_deleted_group() {
        let test_db = setup_test_db().await;
        let db = test_db.connection_arc();
        let service = ErrorIngestionService::new(db.clone());
        let project_id = create_test_project(&db).await;
        let deleted_group = service
            .process_error_event(create_test_error_data(project_id))
            .await
            .expect("Failed to process first error event");

        // Leave the event behind as an orphan of its group, and un-index the
        // fingerprint so the next ingest takes the legacy error_events path.
        let tx = db.begin().await.unwrap();
        tx.execute_unprepared("SET LOCAL session_replication_role = replica")
            .await
            .unwrap();
        tx.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "DELETE FROM error_groups WHERE id=$1",
            [deleted_group.into()],
        ))
        .await
        .unwrap();
        tx.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "DELETE FROM error_group_fingerprints WHERE error_group_id=$1",
            [deleted_group.into()],
        ))
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let group_id = service
            .process_error_event(create_test_error_data(project_id))
            .await
            .expect("an orphaned legacy event must not block ingest of its fingerprint");

        assert_ne!(group_id, deleted_group);
        let group = error_groups::Entity::find_by_id(group_id)
            .one(db.as_ref())
            .await
            .unwrap()
            .expect("new group should exist");
        assert_eq!(group.project_id, project_id);
        assert_eq!(group.total_count, 1);
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn sdk_retries_grouping_queries_and_rollback_are_atomic() {
        use super::super::{error_crud_service::ErrorCRUDService, types::ErrorEventFilter};
        let test_db = match TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) =>
            {
                eprintln!("SDK atomic ingestion test not run: container runtime unavailable");
                return;
            }
            Err(error) => panic!("SDK atomic ingestion test setup failed: {error}"),
        };
        let db = test_db.connection_arc();
        let project = create_test_project(&db).await;
        let other_project = create_test_project(&db).await;
        let service = Arc::new(ErrorIngestionService::new(db.clone()));
        let data = |project_id, id: u128, fingerprint: &str, ty: &str| CreateErrorEventData {
            source: Some("sentry".into()),
            raw_sentry_event: Some(serde_json::json!({
                "event_id":format!("{id:032x}"), "fingerprint":[fingerprint],
                "request":{"url":"https://synthetic.example/path","custom":null},
                "custom":{"large":9007199254740993_u64}
            })),
            project_id,
            exception_type: Some(ty.into()),
            exception_value: Some("Synthetic same message".into()),
            user_id: Some("synthetic-user".into()),
            browser: Some("SyntheticBrowser".into()),
            extra_context: Some(serde_json::json!({"tags":{"synthetic.key":"test'value"}})),
            ..Default::default()
        };
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..48 {
            let service = service.clone();
            let event = data(project, 1, "same", "TypeError");
            tasks.spawn(async move { service.process_with_outcome(event).await.unwrap() });
        }
        let mut stored = 0;
        let mut group = 0;
        while let Some(result) = tasks.join_next().await {
            let result = result.unwrap();
            stored += usize::from(result.inserted);
            if group != 0 {
                assert_eq!(group, result.group_id);
            }
            group = result.group_id;
        }
        assert_eq!(stored, 1);
        assert_eq!(
            error_events::Entity::find()
                .count(db.as_ref())
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            error_groups::Entity::find_by_id(group)
                .one(db.as_ref())
                .await
                .unwrap()
                .unwrap()
                .total_count,
            1
        );
        // Upgrade path: a stored event without a receipt remains deduplicated.
        db.execute_unprepared("DELETE FROM error_event_source_keys")
            .await
            .unwrap();
        assert!(
            !service
                .process_with_outcome(data(project, 1, "same", "TypeError"))
                .await
                .unwrap()
                .inserted
        );
        assert_eq!(
            service
                .process_with_outcome(data(project, 2, "same", "RangeError"))
                .await
                .unwrap()
                .group_id,
            group
        );
        assert_ne!(
            service
                .process_with_outcome(data(project, 3, "different", "TypeError"))
                .await
                .unwrap()
                .group_id,
            group
        );
        let other = service
            .process_with_outcome(data(other_project, 1, "same", "TypeError"))
            .await
            .unwrap();
        assert!(other.inserted);
        assert_ne!(other.group_id, group);
        let mut bad_scope = data(project, 1, "same", "TypeError");
        bad_scope.environment_id = Some(999);
        assert!(matches!(
            service.process_with_outcome(bad_scope).await,
            Err(ErrorTrackingError::Validation(_))
        ));
        let crud = ErrorCRUDService::new(db.clone());
        let filter = ErrorEventFilter {
            user_id: Some("synthetic-user".into()),
            tag_key: Some("synthetic.key".into()),
            tag_value: Some("test'value".into()),
        };
        let (events, count) = crud
            .list_error_events_filtered(group, project, None, None, &filter)
            .await
            .unwrap();
        assert_eq!(count, 2);
        assert_eq!(events[0].browser.as_deref(), Some("SyntheticBrowser"));
        assert_eq!(
            events[0]
                .data
                .as_ref()
                .unwrap()
                .pointer("/sentry/custom/large")
                .and_then(|v| v.as_u64()),
            Some(9007199254740993)
        );
        assert_eq!(
            crud.list_error_events_filtered(group, other_project, None, None, &filter)
                .await
                .unwrap()
                .1,
            0
        );
        let unmatched = ErrorEventFilter {
            user_id: Some("absent".into()),
            ..Default::default()
        };
        assert_eq!(
            crud.list_error_events_filtered(group, project, None, None, &unmatched)
                .await
                .unwrap()
                .1,
            0
        );
        let invalid = ErrorEventFilter {
            tag_key: Some("unpaired".into()),
            ..Default::default()
        };
        assert!(matches!(
            crud.list_error_events_filtered(group, project, None, None, &invalid)
                .await,
            Err(ErrorTrackingError::Validation(_))
        ));
        // Historical Sentry pairs remain queryable without rewriting the raw
        // event or requiring a data migration. Reversed pairs must not match.
        db.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres,
            "UPDATE error_events SET data=jsonb_set(data,'{trace,extra,tags}',$1::jsonb) WHERE project_id=$2 AND error_group_id=$3",
            [serde_json::json!([["synthetic.key", "test'value"]]).into(), project.into(), group.into()])).await.unwrap();
        assert_eq!(
            crud.list_error_events_filtered(group, project, None, None, &filter)
                .await
                .unwrap()
                .1,
            2
        );
        let reversed = ErrorEventFilter {
            tag_key: Some("test'value".into()),
            tag_value: Some("synthetic.key".into()),
            ..Default::default()
        };
        assert_eq!(
            crud.list_error_events_filtered(group, project, None, None, &reversed)
                .await
                .unwrap()
                .1,
            0
        );
        // Force a permanent native write failure; no group, count or receipt may leak.
        db.execute_unprepared("ALTER TABLE error_events ADD CONSTRAINT synthetic_reject CHECK (exception_type <> 'SyntheticRejected')").await.unwrap();
        assert!(service
            .process_with_outcome(data(project, 4, "rollback", "SyntheticRejected"))
            .await
            .is_err());
        assert_eq!(
            error_events::Entity::find()
                .count(db.as_ref())
                .await
                .unwrap(),
            4
        );
        assert_eq!(
            error_groups::Entity::find()
                .count(db.as_ref())
                .await
                .unwrap(),
            3
        );
        db.execute_unprepared("ALTER TABLE error_events DROP CONSTRAINT synthetic_reject")
            .await
            .unwrap();
        assert!(
            service
                .process_with_outcome(data(project, 4, "rollback", "SyntheticRejected"))
                .await
                .unwrap()
                .inserted
        );
        let mut default_event = data(project, 5, "same", "TypeError");
        default_event.raw_sentry_event.as_mut().unwrap()["fingerprint"] = serde_json::Value::Null;
        assert_ne!(
            service
                .process_with_outcome(default_event)
                .await
                .unwrap()
                .group_id,
            group,
            "default grouping must not fuzzily join a custom fingerprint"
        );
        db.execute_unprepared(
            "UPDATE error_event_source_keys SET received_at=NOW()-INTERVAL '8 days'",
        )
        .await
        .unwrap();
        assert_eq!(service.cleanup_source_keys().await.unwrap(), 6);
        // The surviving native row still protects retries even after receipt expiry.
        assert!(
            !service
                .process_with_outcome(data(project, 4, "rollback", "SyntheticRejected"))
                .await
                .unwrap()
                .inserted
        );
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

#[cfg(test)]
mod sdk_grouping_tests {
    use super::*;
    use serde_json::json;

    fn data() -> CreateErrorEventData {
        CreateErrorEventData {
            source: Some("sentry".into()),
            exception_type: Some("SyntheticError".into()),
            exception_value: Some("Synthetic failure".into()),
            stack_trace: Some(
                json!({"frames":[{"filename":"sdk.js","function":"transport","in_app":false},{"filename":"application.js","function":"execute","in_app":true}]}),
            ),
            ..Default::default()
        }
    }
    fn service() -> ErrorIngestionService {
        ErrorIngestionService::new(Arc::new(
            sea_orm::MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
        ))
    }
    #[test]
    fn default_token_extends_or_preserves_native_default_and_custom_values_override() {
        let svc = service();
        let mut item = data();
        let default = svc.generate_fingerprint(&item);
        for values in [json!([]), json!(["{{ default }}"]), json!(["{{default}}"])] {
            item.raw_sentry_event = Some(json!({"fingerprint":values}));
            assert_eq!(svc.generate_fingerprint(&item), default);
        }
        item.raw_sentry_event = Some(json!({"fingerprint":["{{ default }}","sample"]}));
        let extended = svc.generate_fingerprint(&item);
        assert_ne!(extended, default);
        item.exception_type = Some("AnotherError".into());
        assert_ne!(svc.generate_fingerprint(&item), extended);
        item.raw_sentry_event = Some(json!({"fingerprint":["custom","sample"]}));
        let custom = svc.generate_fingerprint(&item);
        item.exception_type = Some("DifferentError".into());
        item.exception_value = Some("Different message".into());
        assert_eq!(svc.generate_fingerprint(&item), custom);
        item.raw_sentry_event = Some(json!({"fingerprint":["customsample"]}));
        assert_ne!(svc.generate_fingerprint(&item), custom);
    }
    #[test]
    fn fingerprint_order_repeated_defaults_and_stackless_messages_are_stable() {
        let svc = service();
        let mut item = data();
        let fingerprint = |values: serde_json::Value| {
            let mut event = item.clone();
            event.raw_sentry_event = Some(json!({"fingerprint":values}));
            svc.generate_fingerprint(&event)
        };
        assert_ne!(
            fingerprint(json!(["a", "b"])),
            fingerprint(json!(["b", "a"]))
        );
        assert_ne!(
            fingerprint(json!(["{{ default }}"])),
            fingerprint(json!(["{{ default }}", "{{ default }}"]))
        );
        assert_eq!(
            fingerprint(json!(["", "synthetic"])),
            fingerprint(json!(["", "synthetic"]))
        );
        item.stack_trace = None;
        let first = svc.generate_fingerprint(&item);
        item.exception_value = Some("Synthetic failure 1234".into());
        let second = svc.generate_fingerprint(&item);
        item.exception_value = Some("Synthetic failure 5678".into());
        assert_eq!(svc.generate_fingerprint(&item), second);
        assert_ne!(first, second);
    }

    #[test]
    fn sentry_stack_shapes_use_recent_app_frames_and_ignore_incidental_messages() {
        let svc = service();
        let mut item = data();
        let before = svc.generate_fingerprint(&item);
        item.exception_value = Some("Another incidental message".into());
        assert_eq!(svc.generate_fingerprint(&item), before);
        item.stack_trace.as_mut().unwrap()["frames"][0]["function"] = json!("differentSdkFrame");
        assert_eq!(svc.generate_fingerprint(&item), before);
        item.stack_trace.as_mut().unwrap()["frames"][1]["function"] = json!("anotherOperation");
        assert_ne!(svc.generate_fingerprint(&item), before);
        item.stack_trace = Some(json!({"frames":[{"instruction_addr":"0x1"}]}));
        let missing_symbols = svc.generate_fingerprint(&item);
        item.exception_value = Some("A different failure without symbols".into());
        assert_ne!(svc.generate_fingerprint(&item), missing_symbols);
    }
}
