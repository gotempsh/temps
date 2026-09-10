// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::user_agent::BrowserInfo;
use anyhow::Result;
use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::{DateTime, Utc};
use flate2::read::ZlibDecoder;
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, FromQueryResult,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, RelationTrait, Set, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Read;
use std::sync::Arc;
use temps_core::UtcDateTime;
use thiserror::Error;
use tracing::{debug, error, info};

use temps_entities::{
    ip_geolocations, session_replay_events, session_replay_ingest_batches, session_replay_sessions,
    visitor,
};

/// How many `session_replay_events` rows to write per `INSERT`.
///
/// Bounded by PostgreSQL's 65535 bind-parameter ceiling: each row binds 5
/// columns (`id` is generated), so the hard maximum is 13107 rows per
/// statement. 1000 stays far below that while keeping a single statement's
/// payload modest — rrweb full-snapshot events can each be tens of kilobytes.
const EVENT_INSERT_CHUNK_SIZE: usize = 1000;

/// Longest accepted client-supplied `batch_id`.
///
/// The SDK emits a UUID (36 chars) or a `batch_<ts>_<9 chars>` fallback, so
/// this is generous. A bound is required rather than optional: the value is
/// unauthenticated, lands in a unique btree index — whose ~2704-byte tuple
/// limit would otherwise turn a long id into a guaranteed 500 — and is echoed
/// into logs.
const MAX_BATCH_ID_LEN: usize = 128;

/// Largest payload a single ingest request may decompress to.
///
/// zlib expands roughly 1000:1 on repetitive input, so the request body limit
/// alone does not bound the work: this is what stops an attacker choosing how
/// much this endpoint allocates and inserts. Generous next to a real flush,
/// where a full-snapshot batch runs to a few megabytes at most.
const MAX_DECOMPRESSED_BYTES: usize = 16 * 1024 * 1024;

/// Most rrweb events one ingest request may carry.
///
/// Checked before the transaction opens so the pooled connection is never held
/// for an attacker-chosen number of inserts.
const MAX_EVENTS_PER_BATCH: usize = 20_000;

/// Whether a client-supplied batch id is one the SDK could have produced.
///
/// Deliberately strict: UUIDs and the SDK's fallback id use only these
/// characters, so anything else is either a bug or an attempt to smuggle
/// newlines or control characters into the logs.
fn batch_id_rejection_reason(batch_id: &str) -> Option<String> {
    if batch_id.is_empty() {
        return Some("must not be empty".to_string());
    }
    if batch_id.len() > MAX_BATCH_ID_LEN {
        return Some(format!(
            "must be at most {MAX_BATCH_ID_LEN} characters, got {}",
            batch_id.len()
        ));
    }
    if let Some(bad) = batch_id
        .chars()
        .find(|c| !matches!(c, 'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '_' | ':' | '-'))
    {
        return Some(format!(
            "may only contain letters, digits and '.', '_', ':', '-' (found {bad:?})"
        ));
    }
    None
}

#[derive(Error, Debug)]
pub enum SessionReplayError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("Visitor not found: {0}")]
    VisitorNotFound(String),

    #[error("Session not found: {0}")]
    SessionNotFound(String),

    /// Returned when a caller supplies a session_replay_id that does not
    /// belong to the project resolved from the request host.  We surface
    /// this as SessionNotFound at the HTTP layer so that cross-project
    /// probing receives a 404 rather than a disclosure-leaking 403.
    #[error(
        "Session {session_replay_id} does not belong to project {project_id} (cross-project access attempt)"
    )]
    CrossProjectAccess {
        session_replay_id: String,
        project_id: i32,
    },

    #[error("Invalid packed data: {0}")]
    InvalidPackedData(String),

    #[error("Decompression error: {0}")]
    DecompressionError(String),

    #[error("JSON parsing error: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("Base64 decode error: {0}")]
    Base64Error(#[from] base64::DecodeError),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    /// The client-supplied batch id is not something the SDK could have
    /// produced. It reaches an unauthenticated route and lands in a unique
    /// index and the logs, so it is validated rather than trusted.
    #[error("Invalid batch id for session {session_replay_id}: {reason}")]
    InvalidBatchId {
        session_replay_id: String,
        reason: String,
    },

    /// The batch decompressed to more data, or more events, than a single
    /// ingest request is allowed to write.
    #[error(
        "Session replay batch for {session_replay_id} exceeds the ingest limit: {actual} {unit} (max {limit})"
    )]
    BatchTooLarge {
        session_replay_id: String,
        unit: &'static str,
        actual: usize,
        limit: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackedEvents {
    pub session_id: String,
    pub events: String,
    pub is_packed: bool,
    pub metadata: SessionMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMetadata {
    pub visitor_id: String,
    pub user_agent: String,
    pub language: String,
    pub timezone: String,
    pub screen: Screen,
    pub viewport: Viewport,
    pub timestamp: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Screen {
    pub width: u32,
    pub height: u32,
    #[serde(rename = "colorDepth")]
    pub color_depth: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnpackedEvents {
    pub session_id: String,
    pub events: Value,
    pub is_packed: bool,
    pub metadata: SessionMetadata,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VisitorInfo {
    pub id: i32,
    pub visitor_id: String,
    pub project_id: i32,
    pub environment_id: i32,
    pub first_seen: UtcDateTime,
    pub last_seen: UtcDateTime,
    pub user_agent: Option<String>,
    pub is_crawler: bool,
    pub crawler_name: Option<String>,
    pub custom_data: Option<serde_json::Value>,
}

// Typed query result struct for efficient row parsing
#[derive(Debug, FromQueryResult)]
pub struct SessionReplayQueryResult {
    // Session fields
    pub id: i32,
    pub session_replay_id: String,
    pub visitor_id: i32,
    pub created_at: Option<UtcDateTime>,
    pub user_agent: Option<String>,
    pub viewport_width: Option<i32>,
    pub viewport_height: Option<i32>,
    pub screen_width: Option<i32>,
    pub screen_height: Option<i32>,
    pub language: Option<String>,
    pub timezone: Option<String>,
    pub url: Option<String>,
    pub duration: Option<i32>,
    // Visitor fields (with aliases to avoid conflicts)
    pub visitor_internal_id: i32,
    pub visitor_uuid: String,
    pub visitor_project_id: i32,
    pub visitor_environment_id: i32,
    pub visitor_first_seen: UtcDateTime,
    pub visitor_last_seen: UtcDateTime,
    pub visitor_user_agent: Option<String>,
    pub visitor_is_crawler: bool,
    pub visitor_crawler_name: Option<String>,
    pub visitor_custom_data: Option<serde_json::Value>,
    // Geolocation fields
    pub visitor_city: Option<String>,
    pub visitor_country: Option<String>,
    pub visitor_country_code: Option<String>,
    pub visitor_region: Option<String>,
}

// Projection for list query
#[derive(Debug, FromQueryResult)]
struct SessionWithVisitorAndCountRow {
    // Session fields
    pub id: i32,
    pub session_replay_id: String,
    pub visitor_id: i32,
    pub created_at: Option<UtcDateTime>,
    pub user_agent: Option<String>,
    pub browser: Option<String>,
    pub browser_version: Option<String>,
    pub operating_system: Option<String>,
    pub operating_system_version: Option<String>,
    pub device_type: Option<String>,
    pub viewport_width: Option<i32>,
    pub viewport_height: Option<i32>,
    pub screen_width: Option<i32>,
    pub screen_height: Option<i32>,
    pub language: Option<String>,
    pub timezone: Option<String>,
    pub url: Option<String>,
    pub duration: Option<i32>,
    // Visitor fields
    pub visitor_internal_id: i32,
    pub visitor_uuid: String,
    pub visitor_project_id: i32,
    pub visitor_environment_id: i32,
    pub visitor_first_seen: UtcDateTime,
    pub visitor_last_seen: UtcDateTime,
    pub visitor_user_agent: Option<String>,
    pub visitor_is_crawler: bool,
    pub visitor_crawler_name: Option<String>,
    pub visitor_custom_data: Option<serde_json::Value>,
    // Geolocation fields (from ip_geolocations via visitor)
    pub visitor_city: Option<String>,
    pub visitor_country: Option<String>,
    pub visitor_country_code: Option<String>,
    pub visitor_region: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionReplayInfo {
    pub id: i32,
    pub session_replay_id: String,
    pub visitor_id: i32,
    pub created_at: Option<DateTime<Utc>>,
    pub user_agent: Option<String>,
    pub viewport_width: Option<i32>,
    pub viewport_height: Option<i32>,
    pub screen_width: Option<i32>,
    pub screen_height: Option<i32>,
    pub language: Option<String>,
    pub timezone: Option<String>,
    pub url: Option<String>,
    pub duration: Option<i32>,
    pub visitor: VisitorInfo,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionReplayWithVisitor {
    pub id: i32,
    pub session_replay_id: String,
    pub visitor_id: i32,
    pub created_at: Option<DateTime<Utc>>,
    pub user_agent: Option<String>,
    pub viewport_width: Option<i32>,
    pub viewport_height: Option<i32>,
    pub screen_width: Option<i32>,
    pub screen_height: Option<i32>,
    pub language: Option<String>,
    pub timezone: Option<String>,
    pub url: Option<String>,
    pub duration: Option<i32>,
    // Parsed user agent fields
    pub browser: Option<String>,
    pub browser_version: Option<String>,
    pub operating_system: Option<String>,
    pub operating_system_version: Option<String>,
    pub device_type: Option<String>,
    // Visitor info merged
    pub visitor_internal_id: i32,
    pub visitor_user_agent: Option<String>,
    pub visitor_uuid: String,
    pub visitor_project_id: i32,
    pub visitor_environment_id: i32,
    pub visitor_first_seen: UtcDateTime,
    pub visitor_last_seen: UtcDateTime,
    pub visitor_is_crawler: bool,
    pub visitor_crawler_name: Option<String>,
    pub visitor_custom_data: Option<serde_json::Value>,
    // Geolocation fields
    pub visitor_city: Option<String>,
    pub visitor_country: Option<String>,
    pub visitor_country_code: Option<String>,
    pub visitor_region: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionEvent {
    pub id: i32,
    pub session_id: i32,
    pub data: Value,
    pub timestamp: i64,
    pub event_type: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionReplayWithEvents {
    pub session: SessionReplayInfo,
    pub events: Vec<SessionEvent>,
}

/// Translate `visitor.environment_id`'s legacy `0` sentinel into `None`.
///
/// `visitor.environment_id` is `NOT NULL` with no foreign key, so the analytics
/// ingest path encodes "no environment" there as a magic `0`
/// (`events_service.rs`). `session_replay_sessions.environment_id` *does* have
/// a foreign key, so copying the sentinel across would FK-violate. Making that
/// column nullable is only half the fix; this is the other half.
fn environment_id_from_visitor(visitor_environment_id: i32) -> Option<i32> {
    (visitor_environment_id != 0).then_some(visitor_environment_id)
}

pub struct SessionReplayService {
    db: Arc<DatabaseConnection>,
}

impl SessionReplayService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Initialize a new session replay with metadata only
    /// This creates the session record without any events
    pub async fn initialize_session(
        &self,
        session_id: &str,
        metadata: SessionMetadata,
        project_id: i32,
        environment_id: Option<i32>,
        deployment_id: Option<i32>,
    ) -> Result<String, SessionReplayError> {
        info!("Initializing session: {} with metadata", session_id);

        // Look up visitor by visitor_id GUID, scoped to this project.
        // `visitor` is uniquely keyed on (visitor_id, project_id) — the same
        // visitor_id string can legitimately belong to different projects —
        // so an unscoped lookup would bind (and let a caller mutate) another
        // project's visitor row for any client-supplied visitor_id on the
        // keyed ingest path (ADR-040 §3).
        let visitor = visitor::Entity::find()
            .filter(visitor::Column::VisitorId.eq(&metadata.visitor_id))
            .filter(visitor::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?;

        let visitor = match visitor {
            Some(v) => v,
            None => {
                return Err(SessionReplayError::VisitorNotFound(
                    metadata.visitor_id.clone(),
                ));
            }
        };

        let visitor_id_int = visitor.id;
        // Parse timestamp
        let created_at = DateTime::parse_from_rfc3339(&metadata.timestamp)
            .map(|dt| dt.with_timezone(&Utc))
            .ok();

        // Check if session already exists by session_replay_id
        let existing = session_replay_sessions::Entity::find()
            .filter(session_replay_sessions::Column::SessionReplayId.eq(session_id))
            .one(self.db.as_ref())
            .await?;

        if existing.is_some() {
            info!(
                "Session {} already exists, skipping initialization",
                session_id
            );
            return Ok(session_id.to_string());
        }

        // Parse user agent
        let browser_info = BrowserInfo::from_user_agent(Some(&metadata.user_agent));

        // Create session
        let session_model = session_replay_sessions::ActiveModel {
            id: sea_orm::NotSet,
            session_replay_id: Set(session_id.to_string()),
            visitor_id: Set(visitor_id_int),
            project_id: Set(project_id),
            // NULL, never a `0` sentinel: there is no `environments.id = 0`
            // or `deployments.id = 0`, so `unwrap_or(0)` FK-violated on every
            // host that resolved to a project without a live deployment.
            environment_id: Set(environment_id),
            deployment_id: Set(deployment_id),
            created_at: Set(created_at),
            user_agent: Set(Some(metadata.user_agent)),
            browser: Set(browser_info.browser),
            browser_version: Set(browser_info.browser_version),
            operating_system: Set(browser_info.operating_system),
            operating_system_version: Set(browser_info.operating_system_version),
            device_type: Set(browser_info.device_type),
            viewport_width: Set(Some(metadata.viewport.width as i32)),
            viewport_height: Set(Some(metadata.viewport.height as i32)),
            screen_width: Set(Some(metadata.screen.width as i32)),
            screen_height: Set(Some(metadata.screen.height as i32)),
            language: Set(Some(metadata.language)),
            timezone: Set(Some(metadata.timezone)),
            url: Set(Some(metadata.url)),
            duration: Set(None), // Will be calculated as events are added
            is_active: Set(true),
        };

        session_model.insert(self.db.as_ref()).await?;
        info!("Session {} initialized successfully", session_id);

        Ok(session_id.to_string())
    }

    /// Add events to an existing session (events are already base64 encoded and compressed).
    ///
    /// `project_id` must match the project that owns the session.  If the
    /// session exists but belongs to a different project, `CrossProjectAccess`
    /// is returned so that the handler can surface a 404 — preventing
    /// cross-tenant event injection and avoiding existence disclosure.
    ///
    /// `batch_id`, when supplied, makes the call idempotent: a batch already
    /// recorded for this session is discarded and `Ok(0)` returned. The browser
    /// SDK resends a failed batch verbatim under a stable id, so without this a
    /// single timed-out request appends its events again on every retry. It is
    /// optional because older SDKs do not send one; those clients keep the old
    /// at-least-once behaviour.
    pub async fn add_session_events(
        &self,
        project_id: i32,
        session_id: &str,
        events_base64: &str,
        batch_id: Option<&str>,
    ) -> Result<usize, SessionReplayError> {
        info!("Adding events to session: {}", session_id);

        // Validate before touching the database: this route is unauthenticated,
        // so the cheapest rejection has to come first.
        if let Some(batch_id) = batch_id {
            if let Some(reason) = batch_id_rejection_reason(batch_id) {
                return Err(SessionReplayError::InvalidBatchId {
                    session_replay_id: session_id.to_string(),
                    reason,
                });
            }
        }

        // Verify session exists by session_replay_id AND project_id to prevent
        // cross-tenant injection: an attacker who guesses another tenant's
        // session_replay_id must not be able to append events to it.
        let session = session_replay_sessions::Entity::find()
            .filter(session_replay_sessions::Column::SessionReplayId.eq(session_id))
            .filter(session_replay_sessions::Column::IsActive.eq(true))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| SessionReplayError::SessionNotFound(session_id.to_string()))?;

        // Enforce project ownership after the session is found so we return
        // the same 404 path for both "not found" and "wrong project".
        if session.project_id != project_id {
            tracing::warn!(
                session_replay_id = %session_id,
                session_project_id = %session.project_id,
                request_project_id = %project_id,
                "Cross-project event injection attempt rejected"
            );
            return Err(SessionReplayError::CrossProjectAccess {
                session_replay_id: session_id.to_string(),
                project_id,
            });
        }

        // Decode and decompress events.
        //
        // Read one byte past the limit rather than trusting the stream: zlib
        // expands roughly 1000:1 on repetitive input, so the request body cap
        // does not bound what this allocates. Overshooting by a byte is what
        // makes "hit the limit" distinguishable from "exactly at the limit".
        let compressed = STANDARD.decode(events_base64)?;
        let decoder = ZlibDecoder::new(&compressed[..]);
        let mut decompressed = String::new();
        decoder
            .take(MAX_DECOMPRESSED_BYTES as u64 + 1)
            .read_to_string(&mut decompressed)
            .map_err(|e| {
                SessionReplayError::DecompressionError(format!(
                    "Failed to decompress events: {}",
                    e
                ))
            })?;

        if decompressed.len() > MAX_DECOMPRESSED_BYTES {
            return Err(SessionReplayError::BatchTooLarge {
                session_replay_id: session_id.to_string(),
                unit: "decompressed bytes",
                actual: decompressed.len(),
                limit: MAX_DECOMPRESSED_BYTES,
            });
        }

        let events: Value = serde_json::from_str(&decompressed)?;

        // Extract events handling both formats
        let events_to_store = self.extract_events_from_json(&events)?;
        let event_count = events_to_store.len();

        if event_count > MAX_EVENTS_PER_BATCH {
            return Err(SessionReplayError::BatchTooLarge {
                session_replay_id: session_id.to_string(),
                unit: "events",
                actual: event_count,
                limit: MAX_EVENTS_PER_BATCH,
            });
        }

        // A batch with nothing in it must not reach the transaction. Claiming a
        // marker for it would write a row per request while storing no events —
        // an unauthenticated client could grow the dedup table using empty
        // payloads. There is also nothing to deduplicate.
        if event_count == 0 {
            debug!(
                session_replay_id = %session_id,
                "Ignoring session replay batch that carried no events"
            );
            return Ok(0);
        }

        // The marker and the events go in together. Claiming the batch outside
        // a transaction would let a failed event insert leave the marker
        // behind, and the client's retry would then be discarded as a
        // duplicate — turning a retryable error into permanent data loss.
        let txn = self.db.begin().await?;

        if let Some(batch_id) = batch_id {
            let marker = session_replay_ingest_batches::ActiveModel {
                id: sea_orm::NotSet,
                session_id: Set(session.id),
                batch_id: Set(batch_id.to_string()),
                event_count: Set(event_count as i32),
                received_at: Set(Utc::now().into()),
            };

            // Claiming the batch *is* the duplicate check: the unique index on
            // (session_id, batch_id) decides it, so two concurrent retries of
            // the same batch cannot both win the way a read-then-write check
            // would allow.
            let claimed = session_replay_ingest_batches::Entity::insert(marker)
                .on_conflict(
                    OnConflict::columns([
                        session_replay_ingest_batches::Column::SessionId,
                        session_replay_ingest_batches::Column::BatchId,
                    ])
                    .do_nothing()
                    .to_owned(),
                )
                .exec_without_returning(&txn)
                .await?;

            if claimed == 0 {
                txn.rollback().await?;
                // `?batch_id` (Debug) rather than `%` — the value is
                // client-supplied, and Debug escapes control characters so a
                // crafted id cannot forge lines in an operator's log.
                debug!(
                    session_replay_id = %session_id,
                    ?batch_id,
                    "Discarded duplicate session replay batch"
                );
                return Ok(0);
            }
        }

        // Insert in batches rather than one round-trip per event. rrweb emits
        // hundreds to thousands of events per flush (mousemove/scroll sampling
        // dominates), so a row-at-a-time loop costs one network round-trip per
        // event: a few thousand events at even 10-20ms each exceeds the
        // proxy's upstream read timeout, the browser's retry then re-sends the
        // same batch, and the retries stack until the endpoint collapses under
        // its own queue. Batching turns that into a handful of statements.
        for chunk in events_to_store.chunks(EVENT_INSERT_CHUNK_SIZE) {
            let models = chunk.iter().map(|event| {
                let timestamp = event.get("timestamp").and_then(|t| t.as_i64()).unwrap_or(0);
                let event_type = event.get("type").and_then(|t| t.as_i64()).map(|t| t as i32);

                session_replay_events::ActiveModel {
                    id: sea_orm::NotSet,
                    session_id: Set(session.id),
                    data: Set(event.to_string()),
                    timestamp: Set(timestamp),
                    r#type: Set(event_type),
                    is_active: Set(true),
                }
            });

            // `exec_without_returning` skips the RETURNING clause — the
            // generated ids are not used, and not shipping them back keeps
            // the response for a 1000-row batch small.
            session_replay_events::Entity::insert_many(models)
                .exec_without_returning(&txn)
                .await?;
        }

        txn.commit().await?;

        // Recompute duration from ALL stored events (not just this batch)
        if event_count > 0 {
            self.compute_and_update_session_duration(session_id, session.id)
                .await?;
        }

        info!("Added {} events to session {}", event_count, session_id);
        Ok(event_count)
    }

    /// Store a packed session replay from rrweb
    pub async fn store_packed_session_replay(
        &self,
        packed_data: PackedEvents,
        deployment_id: Option<i32>,
    ) -> Result<String, SessionReplayError> {
        info!(
            "Storing packed session replay for session: {}",
            packed_data.session_id
        );

        // Look up visitor by visitor_id GUID
        let visitor = visitor::Entity::find()
            .filter(visitor::Column::VisitorId.eq(&packed_data.metadata.visitor_id))
            .one(self.db.as_ref())
            .await?;

        let visitor = match visitor {
            Some(v) => v,
            None => {
                return Err(SessionReplayError::VisitorNotFound(
                    packed_data.metadata.visitor_id.clone(),
                ));
            }
        };

        let visitor_id_int = visitor.id;
        let project_id = visitor.project_id;
        let environment_id = environment_id_from_visitor(visitor.environment_id);

        // Unpack the events
        let unpacked = self.unpack_events(&packed_data)?;

        // Parse timestamp
        let created_at = DateTime::parse_from_rfc3339(&packed_data.metadata.timestamp)
            .map(|dt| dt.with_timezone(&Utc))
            .ok();

        // Parse user agent
        let browser_info = BrowserInfo::from_user_agent(Some(&packed_data.metadata.user_agent));

        // Create session
        let session_model = session_replay_sessions::ActiveModel {
            id: sea_orm::NotSet,
            session_replay_id: Set(packed_data.session_id.clone()),
            visitor_id: Set(visitor_id_int),
            project_id: Set(project_id),
            environment_id: Set(environment_id),
            deployment_id: Set(deployment_id),
            created_at: Set(created_at),
            user_agent: Set(Some(packed_data.metadata.user_agent)),
            browser: Set(browser_info.browser),
            browser_version: Set(browser_info.browser_version),
            operating_system: Set(browser_info.operating_system),
            operating_system_version: Set(browser_info.operating_system_version),
            device_type: Set(browser_info.device_type),
            viewport_width: Set(Some(packed_data.metadata.viewport.width as i32)),
            viewport_height: Set(Some(packed_data.metadata.viewport.height as i32)),
            screen_width: Set(Some(packed_data.metadata.screen.width as i32)),
            screen_height: Set(Some(packed_data.metadata.screen.height as i32)),
            language: Set(Some(packed_data.metadata.language)),
            timezone: Set(Some(packed_data.metadata.timezone)),
            url: Set(Some(packed_data.metadata.url)),
            duration: Set(None), // Will be calculated later
            is_active: Set(true),
        };

        session_model.insert(self.db.as_ref()).await?;

        // Store events - handle both array and object formats
        let events_to_store = self.extract_events_from_json(&unpacked.events)?;
        let event_count = events_to_store.len();

        // Look up session integer ID once (not per-event)
        let session_int_id = session_replay_sessions::Entity::find()
            .filter(session_replay_sessions::Column::SessionReplayId.eq(&packed_data.session_id))
            .one(self.db.as_ref())
            .await?
            .map(|s| s.id)
            .unwrap_or(0); // This should exist since we just created it

        for event in events_to_store {
            let timestamp = event.get("timestamp").and_then(|t| t.as_i64()).unwrap_or(0);
            let event_type = event.get("type").and_then(|t| t.as_i64()).map(|t| t as i32);

            let event_model = session_replay_events::ActiveModel {
                id: sea_orm::NotSet,
                session_id: Set(session_int_id),
                data: Set(event.to_string()),
                timestamp: Set(timestamp),
                r#type: Set(event_type),
                is_active: Set(true),
            };

            event_model.insert(self.db.as_ref()).await?;
        }

        // Compute duration from all stored events
        if event_count > 0 {
            self.compute_and_update_session_duration(&packed_data.session_id, session_int_id)
                .await?;
        }

        info!(
            "Stored {} events for session {}",
            event_count, packed_data.session_id
        );

        Ok(packed_data.session_id)
    }

    /// Store or update a session replay with automatic visitor handling
    pub async fn store_or_update_session_replay(
        &self,
        session_id: &str,
        visitor_id: i32,
        packed_data: String,
        metadata: Option<SessionMetadata>,
        deployment_id: Option<i32>,
    ) -> Result<String, SessionReplayError> {
        info!(
            "Store or update session replay for session: {}, visitor: {}",
            session_id, visitor_id
        );

        // Check if session exists by session_replay_id
        let existing_session = session_replay_sessions::Entity::find()
            .filter(session_replay_sessions::Column::SessionReplayId.eq(session_id))
            .filter(session_replay_sessions::Column::IsActive.eq(true))
            .one(self.db.as_ref())
            .await?;

        if existing_session.is_none() {
            // Get visitor to extract project_id and environment_id
            let visitor = visitor::Entity::find_by_id(visitor_id)
                .one(self.db.as_ref())
                .await?
                .ok_or_else(|| SessionReplayError::VisitorNotFound(visitor_id.to_string()))?;

            let project_id = visitor.project_id;
            let environment_id = environment_id_from_visitor(visitor.environment_id);

            // Parse user agent if available
            let browser_info = if let Some(meta) = metadata.as_ref() {
                BrowserInfo::from_user_agent(Some(&meta.user_agent))
            } else {
                BrowserInfo::default()
            };

            // Create new session
            let now = Utc::now();
            let session_model = session_replay_sessions::ActiveModel {
                id: sea_orm::NotSet,
                session_replay_id: Set(session_id.to_string()),
                visitor_id: Set(visitor_id),
                project_id: Set(project_id),
                environment_id: Set(environment_id),
                deployment_id: Set(deployment_id),
                created_at: Set(Some(now)),
                user_agent: Set(metadata.as_ref().map(|m| m.user_agent.clone())),
                browser: Set(browser_info.browser),
                browser_version: Set(browser_info.browser_version),
                operating_system: Set(browser_info.operating_system),
                operating_system_version: Set(browser_info.operating_system_version),
                device_type: Set(browser_info.device_type),
                viewport_width: Set(metadata.as_ref().map(|m| m.viewport.width as i32)),
                viewport_height: Set(metadata.as_ref().map(|m| m.viewport.height as i32)),
                screen_width: Set(metadata.as_ref().map(|m| m.screen.width as i32)),
                screen_height: Set(metadata.as_ref().map(|m| m.screen.height as i32)),
                language: Set(metadata.as_ref().map(|m| m.language.clone())),
                timezone: Set(metadata.as_ref().map(|m| m.timezone.clone())),
                url: Set(metadata.as_ref().map(|m| m.url.clone())),
                duration: Set(None),
                is_active: Set(true),
            };
            session_model.insert(self.db.as_ref()).await?;
            info!("Created new session replay session: {}", session_id);
        }

        // Store events if provided - create a PackedEvents struct for unpacking
        let packed_events = PackedEvents {
            session_id: session_id.to_string(),
            events: packed_data,
            is_packed: true,
            metadata: metadata.clone().unwrap_or_else(|| {
                // Provide default metadata if not available
                SessionMetadata {
                    visitor_id: visitor_id.to_string(),
                    user_agent: String::from("Unknown"),
                    language: String::from("en"),
                    timezone: String::from("UTC"),
                    screen: Screen {
                        width: 1920,
                        height: 1080,
                        color_depth: 24,
                    },
                    viewport: Viewport {
                        width: 1920,
                        height: 1080,
                    },
                    timestamp: Utc::now().to_rfc3339(),
                    url: String::from(""),
                }
            }),
        };

        let unpacked = self.unpack_events(&packed_events)?;

        // Extract events handling both formats
        let events_to_store = self.extract_events_from_json(&unpacked.events)?;

        if !events_to_store.is_empty() {
            // Look up session integer ID once (not per-event)
            let session_int_id = session_replay_sessions::Entity::find()
                .filter(session_replay_sessions::Column::SessionReplayId.eq(session_id))
                .one(self.db.as_ref())
                .await?
                .map(|s| s.id)
                .unwrap_or(0); // This should exist

            for event in &events_to_store {
                let timestamp = event.get("timestamp").and_then(|t| t.as_i64()).unwrap_or(0);
                let event_type = event.get("type").and_then(|t| t.as_i64()).map(|t| t as i32);

                let event_model = session_replay_events::ActiveModel {
                    id: sea_orm::NotSet,
                    session_id: Set(session_int_id),
                    data: Set(event.to_string()),
                    timestamp: Set(timestamp),
                    r#type: Set(event_type),
                    is_active: Set(true),
                };

                event_model.insert(self.db.as_ref()).await?;
            }

            // Recompute duration from ALL stored events (not just this batch)
            self.compute_and_update_session_duration(session_id, session_int_id)
                .await?;

            info!(
                "Stored {} events for session {}",
                events_to_store.len(),
                session_id
            );
        }
        Ok(session_id.to_string())
    }

    /// Extract events from JSON value (handles both array and object with numeric keys)
    fn extract_events_from_json(&self, events: &Value) -> Result<Vec<Value>, SessionReplayError> {
        if let Some(events_array) = events.as_array() {
            // Already an array, return as-is
            return Ok(events_array.clone());
        } else if let Some(events_obj) = events.as_object() {
            // Object with numeric keys - convert to sorted array
            let mut events_vec = Vec::new();

            // Collect numeric keys and sort them
            let mut numeric_keys: Vec<usize> = events_obj
                .keys()
                .filter_map(|k| k.parse::<usize>().ok())
                .collect();
            numeric_keys.sort();

            // Extract events in order
            for key in numeric_keys {
                if let Some(event) = events_obj.get(&key.to_string()) {
                    events_vec.push(event.clone());
                }
            }

            // Also check for special keys like "v" for metadata
            debug!("Extracted {} events from object format", events_vec.len());

            return Ok(events_vec);
        }

        // Not an array or object with events
        Ok(Vec::new())
    }

    /// Unpack compressed rrweb events
    fn unpack_events(
        &self,
        packed_data: &PackedEvents,
    ) -> Result<UnpackedEvents, SessionReplayError> {
        debug!("Decoding base64 for session: {}", packed_data.session_id);

        // Decode base64
        let compressed = STANDARD.decode(&packed_data.events)?;

        debug!("Decompressing with zlib...");

        // Decompress with zlib
        let mut decoder = ZlibDecoder::new(&compressed[..]);
        let mut decompressed = String::new();
        decoder.read_to_string(&mut decompressed).map_err(|e| {
            SessionReplayError::DecompressionError(format!("Failed to decompress events: {}", e))
        })?;

        // Parse the decompressed JSON
        let events: Value = serde_json::from_str(&decompressed)?;

        // Log what format we received
        if events.is_array() {
            if let Some(arr) = events.as_array() {
                debug!("Successfully unpacked {} events (array format)", arr.len());
            }
        } else if events.is_object() {
            let event_count = self.extract_events_from_json(&events)?.len();
            debug!(
                "Successfully unpacked {} events (object format)",
                event_count
            );
        }

        Ok(UnpackedEvents {
            session_id: packed_data.session_id.clone(),
            events,
            is_packed: false,
            metadata: packed_data.metadata.clone(),
        })
    }

    /// Get session replays for a project
    pub async fn get_sessions_for_project(
        &self,
        project_id: i32,
        environment_id: Option<i32>,
        page: u64,
        per_page: u64,
    ) -> Result<(Vec<SessionReplayWithVisitor>, u64), SessionReplayError> {
        info!(
            "Getting session replays for project: {}, environment: {:?}",
            project_id, environment_id
        );

        // Build filtered base for total count. Exclude replays with no
        // measurable duration: both 0ms and NULL (never-finalized sessions,
        // typically single-burst bot traffic) have nothing to play back.
        // Also exclude soft-deleted sessions (is_active=false) — otherwise a
        // deleted replay keeps showing up here forever, since delete_session_replay
        // only flips the flag and no read path ever checked it.
        let mut count_select = session_replay_sessions::Entity::find()
            .filter(session_replay_sessions::Column::ProjectId.eq(project_id))
            .filter(session_replay_sessions::Column::Duration.gt(0))
            .filter(session_replay_sessions::Column::IsActive.eq(true));
        if let Some(env_id) = environment_id {
            count_select =
                count_select.filter(session_replay_sessions::Column::EnvironmentId.eq(env_id));
        }
        let total_count: u64 = count_select.count(self.db.as_ref()).await?;

        // Same duration/is_active filters as the count query above — must stay in sync.
        let mut query = session_replay_sessions::Entity::find()
            .filter(session_replay_sessions::Column::ProjectId.eq(project_id))
            .filter(session_replay_sessions::Column::Duration.gt(0))
            .filter(session_replay_sessions::Column::IsActive.eq(true))
            .inner_join(visitor::Entity)
            .join(
                sea_orm::JoinType::LeftJoin,
                visitor::Relation::IpGeolocations.def(),
            )
            .select_only()
            .columns([
                session_replay_sessions::Column::Id,
                session_replay_sessions::Column::SessionReplayId,
                session_replay_sessions::Column::VisitorId,
                session_replay_sessions::Column::CreatedAt,
                session_replay_sessions::Column::UserAgent,
                session_replay_sessions::Column::Browser,
                session_replay_sessions::Column::BrowserVersion,
                session_replay_sessions::Column::OperatingSystem,
                session_replay_sessions::Column::OperatingSystemVersion,
                session_replay_sessions::Column::DeviceType,
                session_replay_sessions::Column::ViewportWidth,
                session_replay_sessions::Column::ViewportHeight,
                session_replay_sessions::Column::ScreenWidth,
                session_replay_sessions::Column::ScreenHeight,
                session_replay_sessions::Column::Language,
                session_replay_sessions::Column::Timezone,
                session_replay_sessions::Column::Url,
                session_replay_sessions::Column::Duration,
            ])
            .expr_as(
                Expr::col((visitor::Entity, visitor::Column::Id)),
                "visitor_internal_id",
            )
            .expr_as(
                Expr::col((visitor::Entity, visitor::Column::VisitorId)),
                "visitor_uuid",
            )
            .expr_as(
                Expr::col((visitor::Entity, visitor::Column::ProjectId)),
                "visitor_project_id",
            )
            .expr_as(
                Expr::col((visitor::Entity, visitor::Column::EnvironmentId)),
                "visitor_environment_id",
            )
            .expr_as(
                Expr::col((visitor::Entity, visitor::Column::FirstSeen)),
                "visitor_first_seen",
            )
            .expr_as(
                Expr::col((visitor::Entity, visitor::Column::LastSeen)),
                "visitor_last_seen",
            )
            .expr_as(
                Expr::col((visitor::Entity, visitor::Column::UserAgent)),
                "visitor_user_agent",
            )
            .expr_as(
                Expr::col((visitor::Entity, visitor::Column::IsCrawler)),
                "visitor_is_crawler",
            )
            .expr_as(
                Expr::col((visitor::Entity, visitor::Column::CrawlerName)),
                "visitor_crawler_name",
            )
            .expr_as(
                Expr::col((visitor::Entity, visitor::Column::CustomData)),
                "visitor_custom_data",
            )
            // Geolocation fields from ip_geolocations (LEFT JOIN)
            .expr_as(
                Expr::col((ip_geolocations::Entity, ip_geolocations::Column::City)),
                "visitor_city",
            )
            .expr_as(
                Expr::col((ip_geolocations::Entity, ip_geolocations::Column::Country)),
                "visitor_country",
            )
            .expr_as(
                Expr::col((
                    ip_geolocations::Entity,
                    ip_geolocations::Column::CountryCode,
                )),
                "visitor_country_code",
            )
            .expr_as(
                Expr::col((ip_geolocations::Entity, ip_geolocations::Column::Region)),
                "visitor_region",
            )
            .order_by_desc(session_replay_sessions::Column::CreatedAt);

        if let Some(env_id) = environment_id {
            query = query.filter(session_replay_sessions::Column::EnvironmentId.eq(env_id));
        }

        let offset = (page.saturating_sub(1)) * per_page;
        let rows: Vec<SessionWithVisitorAndCountRow> = query
            .limit(per_page)
            .offset(offset)
            .into_model::<SessionWithVisitorAndCountRow>()
            .all(self.db.as_ref())
            .await?;

        let results = rows
            .into_iter()
            .map(|row| {
                SessionReplayWithVisitor {
                    id: row.id,
                    session_replay_id: row.session_replay_id,
                    visitor_id: row.visitor_id,
                    created_at: row.created_at,
                    user_agent: row.user_agent,
                    viewport_width: row.viewport_width,
                    viewport_height: row.viewport_height,
                    screen_width: row.screen_width,
                    screen_height: row.screen_height,
                    language: row.language,
                    timezone: row.timezone,
                    url: row.url,
                    duration: row.duration,
                    // Parsed user agent fields
                    browser: row.browser,
                    browser_version: row.browser_version,
                    operating_system: row.operating_system,
                    operating_system_version: row.operating_system_version,
                    device_type: row.device_type,
                    // Visitor info merged
                    visitor_internal_id: row.visitor_internal_id,
                    visitor_user_agent: row.visitor_user_agent,
                    visitor_uuid: row.visitor_uuid,
                    visitor_project_id: row.visitor_project_id,
                    visitor_environment_id: row.visitor_environment_id,
                    visitor_first_seen: row.visitor_first_seen,
                    visitor_last_seen: row.visitor_last_seen,
                    visitor_is_crawler: row.visitor_is_crawler,
                    visitor_crawler_name: row.visitor_crawler_name,
                    visitor_custom_data: row.visitor_custom_data,
                    // Geolocation fields
                    visitor_city: row.visitor_city,
                    visitor_country: row.visitor_country,
                    visitor_country_code: row.visitor_country_code,
                    visitor_region: row.visitor_region,
                }
            })
            .collect();

        Ok((results, total_count))
    }

    pub async fn get_sessions_for_visitor(
        &self,
        visitor_id: i32,
        page: u64,
        per_page: u64,
    ) -> Result<Vec<SessionReplayWithVisitor>, SessionReplayError> {
        info!("Getting session replays for visitor: {}", visitor_id);

        let offset = (page.saturating_sub(1)) * per_page;
        let query = format!(
            r#"
            SELECT
                s.id,
                s.session_replay_id,
                s.visitor_id,
                s.created_at,
                s.user_agent,
                s.browser,
                s.browser_version,
                s.operating_system,
                s.operating_system_version,
                s.device_type,
                s.viewport_width,
                s.viewport_height,
                s.screen_width,
                s.screen_height,
                s.language,
                s.timezone,
                s.url,
                s.duration,
                v.id as visitor_internal_id,
                v.visitor_id as visitor_uuid,
                v.project_id as visitor_project_id,
                v.environment_id as visitor_environment_id,
                v.first_seen as visitor_first_seen,
                v.last_seen as visitor_last_seen,
                v.user_agent as visitor_user_agent,
                v.is_crawler as visitor_is_crawler,
                v.crawler_name as visitor_crawler_name,
                v.custom_data as visitor_custom_data,
                g.city as visitor_city,
                g.country as visitor_country,
                g.country_code as visitor_country_code,
                g.region as visitor_region
            FROM session_replay_sessions s
            INNER JOIN visitor v ON s.visitor_id = v.id
            LEFT JOIN ip_geolocations g ON v.ip_address_id = g.id
            WHERE s.visitor_id = $1 AND s.duration > 0 AND s.is_active = true
            ORDER BY s.created_at DESC
            LIMIT {} OFFSET {}
            "#,
            per_page, offset
        );

        let statement = sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            &query,
            vec![visitor_id.into()],
        );

        #[derive(Debug, FromQueryResult)]
        struct SessionReplayWithVisitorQueryRow {
            pub id: i32,
            pub session_replay_id: String,
            pub visitor_id: i32,
            pub created_at: Option<UtcDateTime>,
            pub user_agent: Option<String>,
            pub browser: Option<String>,
            pub browser_version: Option<String>,
            pub operating_system: Option<String>,
            pub operating_system_version: Option<String>,
            pub device_type: Option<String>,
            pub viewport_width: Option<i32>,
            pub viewport_height: Option<i32>,
            pub screen_width: Option<i32>,
            pub screen_height: Option<i32>,
            pub language: Option<String>,
            pub timezone: Option<String>,
            pub url: Option<String>,
            pub duration: Option<i32>,
            pub visitor_internal_id: i32,
            pub visitor_uuid: String,
            pub visitor_project_id: i32,
            pub visitor_environment_id: i32,
            pub visitor_first_seen: UtcDateTime,
            pub visitor_last_seen: UtcDateTime,
            pub visitor_user_agent: Option<String>,
            pub visitor_is_crawler: bool,
            pub visitor_crawler_name: Option<String>,
            pub visitor_custom_data: Option<serde_json::Value>,
            pub visitor_city: Option<String>,
            pub visitor_country: Option<String>,
            pub visitor_country_code: Option<String>,
            pub visitor_region: Option<String>,
        }

        let query_results = SessionReplayWithVisitorQueryRow::find_by_statement(statement)
            .all(self.db.as_ref())
            .await?;

        let results = query_results
            .into_iter()
            .map(|row| {
                SessionReplayWithVisitor {
                    id: row.id,
                    session_replay_id: row.session_replay_id,
                    visitor_id: row.visitor_id,
                    created_at: row.created_at,
                    user_agent: row.user_agent,
                    viewport_width: row.viewport_width,
                    viewport_height: row.viewport_height,
                    screen_width: row.screen_width,
                    screen_height: row.screen_height,
                    language: row.language,
                    timezone: row.timezone,
                    url: row.url,
                    duration: row.duration,
                    // Parsed user agent fields
                    browser: row.browser,
                    browser_version: row.browser_version,
                    operating_system: row.operating_system,
                    operating_system_version: row.operating_system_version,
                    device_type: row.device_type,
                    // Visitor info merged
                    visitor_internal_id: row.visitor_internal_id,
                    visitor_user_agent: row.visitor_user_agent,
                    visitor_uuid: row.visitor_uuid,
                    visitor_project_id: row.visitor_project_id,
                    visitor_environment_id: row.visitor_environment_id,
                    visitor_first_seen: row.visitor_first_seen,
                    visitor_last_seen: row.visitor_last_seen,
                    visitor_is_crawler: row.visitor_is_crawler,
                    visitor_crawler_name: row.visitor_crawler_name,
                    visitor_custom_data: row.visitor_custom_data,
                    // Geolocation fields
                    visitor_city: row.visitor_city,
                    visitor_country: row.visitor_country,
                    visitor_country_code: row.visitor_country_code,
                    visitor_region: row.visitor_region,
                }
            })
            .collect();

        Ok(results)
    }

    /// Get a complete session replay with all events
    pub async fn get_session_replay(
        &self,
        session_id: i32,
    ) -> Result<SessionReplayWithEvents, SessionReplayError> {
        info!("Getting session replay: {}", session_id);

        // Get session with visitor data using join
        let query = r#"
            SELECT
                s.id,
                s.session_replay_id,
                s.visitor_id,
                s.created_at,
                s.user_agent,
                s.browser,
                s.browser_version,
                s.operating_system,
                s.operating_system_version,
                s.device_type,
                s.viewport_width,
                s.viewport_height,
                s.screen_width,
                s.screen_height,
                s.language,
                s.timezone,
                s.url,
                s.duration,
                v.id as visitor_internal_id,
                v.visitor_id as visitor_uuid,
                v.project_id as visitor_project_id,
                v.environment_id as visitor_environment_id,
                v.first_seen as visitor_first_seen,
                v.last_seen as visitor_last_seen,
                v.user_agent as visitor_user_agent,
                v.is_crawler as visitor_is_crawler,
                v.crawler_name as visitor_crawler_name,
                v.custom_data as visitor_custom_data,
                g.city as visitor_city,
                g.country as visitor_country,
                g.country_code as visitor_country_code,
                g.region as visitor_region
            FROM session_replay_sessions s
            INNER JOIN visitor v ON s.visitor_id = v.id
            LEFT JOIN ip_geolocations g ON v.ip_address_id = g.id
            WHERE s.id = $1 AND s.is_active = true
        "#;

        let statement = sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            query,
            vec![session_id.into()],
        );

        let row = SessionReplayQueryResult::find_by_statement(statement)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| SessionReplayError::SessionNotFound(session_id.to_string()))?;

        // Get events using the integer session ID.
        // Limit to 50 000 events to avoid loading hundreds of MB for very long sessions.
        const MAX_REPLAY_EVENTS: u64 = 50_000;

        let events = session_replay_events::Entity::find()
            .filter(session_replay_events::Column::SessionId.eq(row.id))
            .filter(session_replay_events::Column::IsActive.eq(true))
            .order_by_asc(session_replay_events::Column::Timestamp)
            .limit(MAX_REPLAY_EVENTS)
            .all(self.db.as_ref())
            .await?;

        let session_events: Result<Vec<SessionEvent>, SessionReplayError> = events
            .into_iter()
            .map(|event| {
                let data: Value = serde_json::from_str(&event.data)?;
                Ok(SessionEvent {
                    id: event.id,
                    session_id: event.session_id,
                    data,
                    timestamp: event.timestamp,
                    event_type: event.r#type,
                })
            })
            .collect();

        let session_events = session_events?;

        let visitor = VisitorInfo {
            id: row.visitor_internal_id,
            visitor_id: row.visitor_uuid,
            project_id: row.visitor_project_id,
            environment_id: row.visitor_environment_id,
            first_seen: row.visitor_first_seen,
            last_seen: row.visitor_last_seen,
            user_agent: row.visitor_user_agent,
            is_crawler: row.visitor_is_crawler,
            crawler_name: row.visitor_crawler_name,
            custom_data: row.visitor_custom_data,
        };

        let session_info = SessionReplayInfo {
            id: row.id,
            session_replay_id: row.session_replay_id,
            visitor_id: row.visitor_id,
            created_at: row.created_at,
            user_agent: row.user_agent,
            viewport_width: row.viewport_width,
            viewport_height: row.viewport_height,
            screen_width: row.screen_width,
            screen_height: row.screen_height,
            language: row.language,
            timezone: row.timezone,
            url: row.url,
            duration: row.duration,
            visitor,
        };

        Ok(SessionReplayWithEvents {
            session: session_info,
            events: session_events,
        })
    }

    /// Get session replay data without events (merged with visitor data)
    pub async fn get_session_replay_without_events(
        &self,
        session_id: i32,
    ) -> Result<SessionReplayWithVisitor, SessionReplayError> {
        info!("Getting session replay without events: {}", session_id);

        // Get session with visitor data using join and count events
        let query = r#"
            SELECT
                s.id,
                s.session_replay_id,
                s.visitor_id,
                s.created_at,
                s.user_agent,
                s.browser,
                s.browser_version,
                s.operating_system,
                s.operating_system_version,
                s.device_type,
                s.viewport_width,
                s.viewport_height,
                s.screen_width,
                s.screen_height,
                s.language,
                s.timezone,
                s.url,
                s.duration,
                v.id as visitor_internal_id,
                v.visitor_id as visitor_uuid,
                v.project_id as visitor_project_id,
                v.environment_id as visitor_environment_id,
                v.first_seen as visitor_first_seen,
                v.last_seen as visitor_last_seen,
                v.user_agent as visitor_user_agent,
                v.is_crawler as visitor_is_crawler,
                v.crawler_name as visitor_crawler_name,
                v.custom_data as visitor_custom_data,
                g.city as visitor_city,
                g.country as visitor_country,
                g.country_code as visitor_country_code,
                g.region as visitor_region
            FROM session_replay_sessions s
            INNER JOIN visitor v ON s.visitor_id = v.id
            LEFT JOIN ip_geolocations g ON v.ip_address_id = g.id
            WHERE s.id = $1 AND s.is_active = true
        "#;

        let statement = sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            query,
            vec![session_id.into()],
        );

        #[derive(Debug, FromQueryResult)]
        struct SessionReplayWithVisitorRow {
            pub id: i32,
            pub session_replay_id: String,
            pub visitor_id: i32,
            pub created_at: Option<UtcDateTime>,
            pub user_agent: Option<String>,
            pub browser: Option<String>,
            pub browser_version: Option<String>,
            pub operating_system: Option<String>,
            pub operating_system_version: Option<String>,
            pub device_type: Option<String>,
            pub viewport_width: Option<i32>,
            pub viewport_height: Option<i32>,
            pub screen_width: Option<i32>,
            pub screen_height: Option<i32>,
            pub language: Option<String>,
            pub timezone: Option<String>,
            pub url: Option<String>,
            pub duration: Option<i32>,
            pub visitor_internal_id: i32,
            pub visitor_uuid: String,
            pub visitor_project_id: i32,
            pub visitor_environment_id: i32,
            pub visitor_first_seen: UtcDateTime,
            pub visitor_last_seen: UtcDateTime,
            pub visitor_user_agent: Option<String>,
            pub visitor_is_crawler: bool,
            pub visitor_crawler_name: Option<String>,
            pub visitor_custom_data: Option<serde_json::Value>,
            pub visitor_city: Option<String>,
            pub visitor_country: Option<String>,
            pub visitor_country_code: Option<String>,
            pub visitor_region: Option<String>,
        }

        let row = SessionReplayWithVisitorRow::find_by_statement(statement)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| SessionReplayError::SessionNotFound(session_id.to_string()))?;

        Ok(SessionReplayWithVisitor {
            id: row.id,
            session_replay_id: row.session_replay_id,
            visitor_id: row.visitor_id,
            created_at: row.created_at,
            user_agent: row.user_agent,
            viewport_width: row.viewport_width,
            viewport_height: row.viewport_height,
            screen_width: row.screen_width,
            screen_height: row.screen_height,
            language: row.language,
            timezone: row.timezone,
            url: row.url,
            duration: row.duration,
            // Use parsed user agent fields from database
            browser: row.browser,
            browser_version: row.browser_version,
            operating_system: row.operating_system,
            operating_system_version: row.operating_system_version,
            device_type: row.device_type,
            // Visitor info merged
            visitor_internal_id: row.visitor_internal_id,
            visitor_user_agent: row.visitor_user_agent,
            visitor_uuid: row.visitor_uuid,
            visitor_project_id: row.visitor_project_id,
            visitor_environment_id: row.visitor_environment_id,
            visitor_first_seen: row.visitor_first_seen,
            visitor_last_seen: row.visitor_last_seen,
            visitor_is_crawler: row.visitor_is_crawler,
            visitor_crawler_name: row.visitor_crawler_name,
            visitor_custom_data: row.visitor_custom_data,
            // Geolocation fields
            visitor_city: row.visitor_city,
            visitor_country: row.visitor_country,
            visitor_country_code: row.visitor_country_code,
            visitor_region: row.visitor_region,
        })
    }

    /// Unpack events without storing them (useful for debugging or inspection)
    pub fn unpack_events_only(
        &self,
        packed_data: &PackedEvents,
    ) -> Result<UnpackedEvents, SessionReplayError> {
        self.unpack_events(packed_data)
    }

    /// Compute session duration from all stored events (global min/max timestamps).
    /// This queries the actual events table to get the true session span,
    /// not just the span of a single batch.
    async fn compute_and_update_session_duration(
        &self,
        session_replay_id: &str,
        session_int_id: i32,
    ) -> Result<(), SessionReplayError> {
        #[derive(Debug, FromQueryResult)]
        struct TimestampRange {
            min_ts: Option<i64>,
            max_ts: Option<i64>,
        }

        let statement = sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            r#"SELECT MIN(timestamp) as min_ts, MAX(timestamp) as max_ts
               FROM session_replay_events
               WHERE session_id = $1 AND is_active = true"#,
            vec![session_int_id.into()],
        );

        let result = TimestampRange::find_by_statement(statement)
            .one(self.db.as_ref())
            .await?;

        if let Some(TimestampRange {
            min_ts: Some(min),
            max_ts: Some(max),
        }) = result
        {
            let duration_ms = (max - min) as i32;
            self.update_session_duration(session_replay_id, duration_ms)
                .await?;
        }

        Ok(())
    }

    /// Update session duration
    pub async fn update_session_duration(
        &self,
        session_id: &str,
        duration: i32,
    ) -> Result<(), SessionReplayError> {
        info!(
            "Updating duration for session: {} to {} ms",
            session_id, duration
        );

        let session = session_replay_sessions::Entity::find()
            .filter(session_replay_sessions::Column::SessionReplayId.eq(session_id))
            .filter(session_replay_sessions::Column::IsActive.eq(true))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| SessionReplayError::SessionNotFound(session_id.to_string()))?;

        let mut session: session_replay_sessions::ActiveModel = session.into();
        session.duration = Set(Some(duration));
        session.update(self.db.as_ref()).await?;

        Ok(())
    }

    /// Hard-delete session replay events older than `retention_days` using
    /// the event `timestamp` column (unix milliseconds), then remove any
    /// sessions that no longer have events.
    pub async fn cleanup_old_session_events(&self, retention_days: i64) {
        let cutoff_ms = (Utc::now() - chrono::Duration::days(retention_days)).timestamp_millis();
        info!(
            "Session replay cleanup: deleting events older than {} ms (retention: {} days)",
            cutoff_ms, retention_days
        );

        // 1. Delete events older than cutoff directly by timestamp
        match session_replay_events::Entity::delete_many()
            .filter(session_replay_events::Column::Timestamp.lt(cutoff_ms))
            .exec(self.db.as_ref())
            .await
        {
            Ok(res) => {
                info!(
                    "Session replay cleanup: deleted {} event rows older than {} days",
                    res.rows_affected, retention_days
                );
            }
            Err(e) => {
                error!("Session replay cleanup: failed to delete old events: {}", e);
                return;
            }
        }

        // 2. Delete sessions that have no remaining events
        let cutoff_dt = Utc::now() - chrono::Duration::days(retention_days);
        match session_replay_sessions::Entity::delete_many()
            .filter(session_replay_sessions::Column::CreatedAt.lt(cutoff_dt))
            .exec(self.db.as_ref())
            .await
        {
            Ok(res) => {
                if res.rows_affected > 0 {
                    info!(
                        "Session replay cleanup: deleted {} old sessions",
                        res.rows_affected
                    );
                }
            }
            Err(e) => {
                error!(
                    "Session replay cleanup: failed to delete old sessions: {}",
                    e
                );
            }
        }
    }

    /// Delete a session replay (soft delete)
    pub async fn delete_session_replay(&self, session_id: &str) -> Result<(), SessionReplayError> {
        info!("Deleting session replay: {}", session_id);

        let session = session_replay_sessions::Entity::find()
            .filter(session_replay_sessions::Column::SessionReplayId.eq(session_id))
            .filter(session_replay_sessions::Column::IsActive.eq(true))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| SessionReplayError::SessionNotFound(session_id.to_string()))?;

        // Store the session ID before converting to ActiveModel
        let session_int_id = session.id;

        // Soft delete session
        let mut session: session_replay_sessions::ActiveModel = session.into();
        session.is_active = Set(false);
        session.update(self.db.as_ref()).await?;

        // Soft delete all events for this session in a single UPDATE instead of
        // loading every event into memory and updating one by one (N+1).
        session_replay_events::Entity::update_many()
            .col_expr(session_replay_events::Column::IsActive, Expr::value(false))
            .filter(session_replay_events::Column::SessionId.eq(session_int_id))
            .filter(session_replay_events::Column::IsActive.eq(true))
            .exec(self.db.as_ref())
            .await?;

        Ok(())
    }

    /// Return the project_id that owns the given session_replay_id string.
    ///
    /// Used by admin handlers that receive a string session_replay_id from
    /// URL path parameters and need a project_id to call `add_session_events`
    /// without doing an open cross-project lookup.
    pub async fn get_project_id_for_session(
        &self,
        session_replay_id: &str,
    ) -> Result<i32, SessionReplayError> {
        let session = session_replay_sessions::Entity::find()
            .filter(session_replay_sessions::Column::SessionReplayId.eq(session_replay_id))
            .filter(session_replay_sessions::Column::IsActive.eq(true))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| SessionReplayError::SessionNotFound(session_replay_id.to_string()))?;

        Ok(session.project_id)
    }

    // -----------------------------------------------------------------
    // Authorization lookups
    //
    // Most read/write handlers on this crate are keyed by session or
    // visitor id rather than project id, so there is no project_id in the
    // path for `project_access_guard!` to check. These resolve one so the
    // guard can run. They deliberately do NOT filter on `is_active`: an
    // authorization decision must be made for the row that exists, not
    // only for currently-live sessions, or ended sessions would fall out
    // of scoping entirely.
    // -----------------------------------------------------------------

    /// Project owning the session with this numeric primary key.
    pub async fn project_id_for_session_pk(
        &self,
        session_pk: i32,
    ) -> Result<i32, SessionReplayError> {
        let session = session_replay_sessions::Entity::find_by_id(session_pk)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| SessionReplayError::SessionNotFound(session_pk.to_string()))?;
        Ok(session.project_id)
    }

    /// Project owning the session with this `session_replay_id` string,
    /// regardless of whether it is still active.
    pub async fn project_id_for_session_replay_id(
        &self,
        session_replay_id: &str,
    ) -> Result<i32, SessionReplayError> {
        let session = session_replay_sessions::Entity::find()
            .filter(session_replay_sessions::Column::SessionReplayId.eq(session_replay_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| SessionReplayError::SessionNotFound(session_replay_id.to_string()))?;
        Ok(session.project_id)
    }

    /// Project the visitor belongs to.
    pub async fn project_id_for_visitor(&self, visitor_id: i32) -> Result<i32, SessionReplayError> {
        let visitor = temps_entities::visitor::Entity::find_by_id(visitor_id)
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| SessionReplayError::SessionNotFound(format!("visitor {visitor_id}")))?;
        Ok(visitor.project_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn make_session_model(
        id: i32,
        session_replay_id: &str,
        project_id: i32,
    ) -> session_replay_sessions::Model {
        session_replay_sessions::Model {
            id,
            session_replay_id: session_replay_id.to_string(),
            visitor_id: 1,
            project_id,
            environment_id: Some(1),
            deployment_id: Some(1),
            created_at: None,
            user_agent: None,
            browser: None,
            browser_version: None,
            operating_system: None,
            operating_system_version: None,
            device_type: None,
            viewport_width: None,
            viewport_height: None,
            screen_width: None,
            screen_height: None,
            language: None,
            timezone: None,
            url: None,
            duration: None,
            is_active: true,
        }
    }

    /// Cross-project injection attempt: session belongs to project 1 but
    /// caller presents project 2's host.  Must return CrossProjectAccess
    /// which the HTTP layer maps to 404 (no existence disclosure).
    #[tokio::test]
    async fn add_events_with_wrong_project_returns_not_found() {
        // Session is owned by project 1
        let session = make_session_model(42, "session-abc", 1);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![session]])
            .into_connection();

        let service = SessionReplayService::new(Arc::new(db));

        // Caller claims to be project 2
        let result = service
            .add_session_events(2, "session-abc", "dGVzdA==", None) // "test" in base64
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(
                err,
                SessionReplayError::CrossProjectAccess {
                    session_replay_id: ref sid,
                    project_id: 2
                } if sid == "session-abc"
            ),
            "Expected CrossProjectAccess, got: {:?}",
            err
        );
    }

    /// Session not found (wrong session_replay_id string).  Must return
    /// SessionNotFound so the HTTP layer emits 404.
    #[tokio::test]
    async fn add_events_session_not_found_returns_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![] as Vec<session_replay_sessions::Model>])
            .into_connection();

        let service = SessionReplayService::new(Arc::new(db));

        let result = service
            .add_session_events(1, "does-not-exist", "dGVzdA==", None)
            .await;

        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), SessionReplayError::SessionNotFound(ref s) if s == "does-not-exist"),
            "Expected SessionNotFound"
        );
    }

    fn make_visitor_model(environment_id: i32) -> visitor::Model {
        let now = chrono::Utc::now();
        visitor::Model {
            id: 5,
            visitor_id: "visitor-abc".to_string(),
            project_id: 7,
            environment_id,
            first_seen: now,
            last_seen: now,
            user_agent: None,
            ip_address_id: None,
            is_crawler: false,
            crawler_name: None,
            custom_data: None,
            has_activity: true,
            first_referrer: None,
            first_referrer_hostname: None,
            first_channel: None,
            first_utm_source: None,
            first_utm_medium: None,
            first_utm_campaign: None,
        }
    }

    fn make_session_metadata() -> SessionMetadata {
        SessionMetadata {
            visitor_id: "visitor-abc".to_string(),
            user_agent: "Mozilla/5.0".to_string(),
            language: "en-US".to_string(),
            timezone: "UTC".to_string(),
            screen: Screen {
                width: 1920,
                height: 1080,
                color_depth: 24,
            },
            viewport: Viewport {
                width: 1280,
                height: 720,
            },
            timestamp: "2026-08-31T00:00:00Z".to_string(),
            url: "https://app.example.com/".to_string(),
        }
    }

    /// Regression test for the live FK-violation fixed alongside ADR-040:
    /// `initialize_session` used to write `environment_id.unwrap_or(0)` /
    /// `deployment_id.unwrap_or(0)`. There is no `environments.id = 0` or
    /// `deployments.id = 0`, so `/api/_temps/session-replay/init` 500'd for
    /// every host that resolved to a project without a live deployment. The
    /// insert must bind NULL.
    #[tokio::test]
    async fn initialize_session_writes_null_scope_not_zero_sentinel() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // 1. visitor lookup
                .append_query_results(vec![vec![make_visitor_model(3)]])
                // 2. existing-session lookup (none)
                .append_query_results(vec![vec![] as Vec<session_replay_sessions::Model>])
                // 3. the insert
                .append_query_results(vec![vec![make_session_model(1, "session-abc", 7)]])
                .into_connection(),
        );
        let service = SessionReplayService::new(db.clone());

        let result = service
            .initialize_session("session-abc", make_session_metadata(), 7, None, None)
            .await;

        assert!(
            result.is_ok(),
            "a project without a deployment must still open a replay session: {:?}",
            result.err()
        );

        drop(service);
        let log = match Arc::try_unwrap(db) {
            Ok(conn) => conn.into_transaction_log(),
            Err(_) => panic!("service still holds a connection handle"),
        };
        let insert = format!("{:?}", log.get(2).expect("an INSERT must have been issued"));
        assert!(
            insert.contains("INSERT INTO") && insert.contains("session_replay_sessions"),
            "expected an insert into session_replay_sessions, got: {insert}"
        );
        assert!(
            !insert.contains("Int(Some(0))"),
            "the `0` sentinel must never reach an FK column: {insert}"
        );
        assert!(
            insert.contains("Int(None)"),
            "environment_id/deployment_id must be bound as NULL: {insert}"
        );
    }

    #[tokio::test]
    async fn initialize_session_preserves_concrete_scope() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![make_visitor_model(3)]])
                .append_query_results(vec![vec![] as Vec<session_replay_sessions::Model>])
                .append_query_results(vec![vec![make_session_model(1, "session-abc", 7)]])
                .into_connection(),
        );
        let service = SessionReplayService::new(db.clone());

        let result = service
            .initialize_session("session-abc", make_session_metadata(), 7, Some(3), Some(11))
            .await;

        assert!(result.is_ok());

        drop(service);
        let log = match Arc::try_unwrap(db) {
            Ok(conn) => conn.into_transaction_log(),
            Err(_) => panic!("service still holds a connection handle"),
        };
        let insert = format!("{:?}", log.get(2).expect("an INSERT must have been issued"));
        assert!(
            insert.contains("Int(Some(3))") && insert.contains("Int(Some(11))"),
            "a fully resolved route must keep its attribution: {insert}"
        );
    }

    /// `visitor.environment_id` is `NOT NULL` with no FK, so the analytics
    /// ingest path still encodes "no environment" there as a magic `0`. That
    /// sentinel must not be copied into `session_replay_sessions`, whose
    /// `environment_id` *does* have an FK.
    #[test]
    fn environment_id_from_visitor_maps_zero_sentinel_to_none() {
        assert_eq!(environment_id_from_visitor(0), None);
        assert_eq!(environment_id_from_visitor(3), Some(3));
    }

    /// Build the wire payload the browser SDK sends: zlib-compressed JSON,
    /// base64 encoded.
    fn encode_events(events: &[Value]) -> String {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        let json = serde_json::to_string(events).expect("serialize events");
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(json.as_bytes()).expect("compress events");
        STANDARD.encode(encoder.finish().expect("finish compression"))
    }

    fn make_events(count: usize) -> Vec<Value> {
        (0..count)
            .map(|i| {
                serde_json::json!({
                    "type": 3,
                    "timestamp": 1_700_000_000_000i64 + i as i64,
                    "data": { "source": 1, "positions": [{ "x": i, "y": i }] }
                })
            })
            .collect()
    }

    /// Count the INSERT statements issued against `session_replay_events`.
    fn count_event_inserts(log: &[sea_orm::Transaction]) -> usize {
        log.iter()
            .flat_map(|t| t.statements())
            .filter(|stmt| {
                let sql = &stmt.sql;
                sql.starts_with("INSERT INTO") && sql.contains("session_replay_events")
            })
            .count()
    }

    /// Drive `add_session_events` against a mock DB and return the statement log.
    async fn insert_log_for(event_count: usize) -> Vec<sea_orm::Transaction> {
        let session = make_session_model(42, "session-batch", 1);

        let mut mock = MockDatabase::new(DatabaseBackend::Postgres)
            // 1. session lookup
            .append_query_results(vec![vec![session]]);

        // 2. one exec result per expected INSERT chunk (extra results are
        //    harmless; too few would surface as a DB error).
        let chunks = event_count.div_ceil(EVENT_INSERT_CHUNK_SIZE);
        mock = mock.append_exec_results(
            (0..chunks)
                .map(|i| sea_orm::MockExecResult {
                    last_insert_id: (i + 1) as u64,
                    rows_affected: EVENT_INSERT_CHUNK_SIZE as u64,
                })
                .collect::<Vec<_>>(),
        );

        // 3. duration recompute aggregate — empty result means MIN/MAX are
        //    NULL, so the follow-up session UPDATE is skipped.
        mock = mock.append_query_results(vec![Vec::<
            std::collections::BTreeMap<String, sea_orm::Value>,
        >::new()]);

        let db = Arc::new(mock.into_connection());
        let service = SessionReplayService::new(db.clone());

        let payload = encode_events(&make_events(event_count));
        let result = service
            .add_session_events(1, "session-batch", &payload, None)
            .await;
        assert_eq!(
            result.expect("add_session_events should succeed"),
            event_count,
            "should report every event as stored"
        );

        drop(service);
        Arc::try_unwrap(db)
            .expect("service should be the only other Arc holder")
            .into_transaction_log()
    }

    /// Regression: rrweb flushes hundreds of events per request. Storing them
    /// one INSERT at a time cost one DB round-trip per event, which pushed a
    /// single ingest request past the proxy's upstream read timeout and made
    /// the endpoint 503 while the browser retried into the backlog.
    ///
    /// 250 events must cost exactly ONE INSERT, not 250.
    ///
    /// Note on the pre-fix failure mode: the old per-event loop used
    /// `ActiveModel::insert`, which on Postgres issues `INSERT ... RETURNING`
    /// — a *query*, not an exec. Against this mock it therefore fails on the
    /// first event with `RecordNotFound`, rather than reaching the count
    /// assertion below. Either way the test goes red on a reintroduced loop,
    /// which is what matters.
    #[tokio::test]
    async fn add_events_batches_inserts_into_single_statement() {
        let log = insert_log_for(250).await;
        let inserts = count_event_inserts(&log);

        assert_eq!(
            inserts, 1,
            "250 rrweb events must be written in 1 batched INSERT, got {inserts} \
             (a per-event insert loop regressed)"
        );
    }

    /// Batches larger than the chunk size are split, so a single statement
    /// never approaches PostgreSQL's 65535 bind-parameter ceiling.
    #[tokio::test]
    async fn add_events_chunks_batches_above_the_limit() {
        let event_count = EVENT_INSERT_CHUNK_SIZE * 2 + 1;
        let log = insert_log_for(event_count).await;
        let inserts = count_event_inserts(&log);

        assert_eq!(
            inserts, 3,
            "{event_count} events must be split into 3 chunked INSERT statements, got {inserts}"
        );
    }

    /// Count `INSERT` statements against the batch-marker table.
    fn count_marker_inserts(log: &[sea_orm::Transaction]) -> usize {
        log.iter()
            .flat_map(|t| t.statements())
            .filter(|stmt| {
                stmt.sql.starts_with("INSERT INTO")
                    && stmt.sql.contains("session_replay_ingest_batches")
            })
            .count()
    }

    /// Regression: the browser SDK resends a failed batch verbatim under the
    /// same `batch_id`. When the marker insert conflicts, the batch has
    /// already been stored and its events must NOT be appended again — that
    /// duplication is what made an idle visitor accumulate the same events
    /// over and over whenever ingest was slow enough to trigger retries.
    #[tokio::test]
    async fn add_events_discards_a_batch_that_was_already_ingested() {
        let session = make_session_model(42, "session-dupe", 1);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![session]])
            // Marker insert conflicts: ON CONFLICT DO NOTHING affects 0 rows.
            .append_exec_results(vec![sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        let db = Arc::new(db);
        let service = SessionReplayService::new(db.clone());
        let payload = encode_events(&make_events(10));

        let stored = service
            .add_session_events(1, "session-dupe", &payload, Some("batch-1"))
            .await
            .expect("a duplicate batch is not an error");

        assert_eq!(stored, 0, "a replayed batch must store no events");

        drop(service);
        let log = Arc::try_unwrap(db)
            .expect("service should be the only other Arc holder")
            .into_transaction_log();

        assert_eq!(
            count_event_inserts(&log),
            0,
            "no event rows may be written for a batch that already landed"
        );
    }

    /// The first delivery of a batch claims the marker and stores its events.
    #[tokio::test]
    async fn add_events_stores_a_batch_seen_for_the_first_time() {
        let session = make_session_model(42, "session-fresh", 1);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![session]])
            .append_exec_results(vec![
                // Marker claimed.
                sea_orm::MockExecResult {
                    last_insert_id: 1,
                    rows_affected: 1,
                },
                // Event batch insert.
                sea_orm::MockExecResult {
                    last_insert_id: 1,
                    rows_affected: 10,
                },
            ])
            .append_query_results(vec![Vec::<
                std::collections::BTreeMap<String, sea_orm::Value>,
            >::new()])
            .into_connection();

        let db = Arc::new(db);
        let service = SessionReplayService::new(db.clone());
        let payload = encode_events(&make_events(10));

        let stored = service
            .add_session_events(1, "session-fresh", &payload, Some("batch-2"))
            .await
            .expect("a fresh batch should store");

        assert_eq!(stored, 10);

        drop(service);
        let log = Arc::try_unwrap(db)
            .expect("service should be the only other Arc holder")
            .into_transaction_log();

        assert_eq!(
            count_marker_inserts(&log),
            1,
            "the batch must be claimed once"
        );
        assert_eq!(count_event_inserts(&log), 1, "events must still be batched");
    }

    /// Clients that predate batch ids keep working: no marker is written and
    /// the events are stored exactly as before.
    #[tokio::test]
    async fn add_events_without_a_batch_id_skips_the_marker_entirely() {
        let log = insert_log_for(10).await;

        assert_eq!(
            count_marker_inserts(&log),
            0,
            "omitting batchId must not touch the dedup table"
        );
        assert_eq!(count_event_inserts(&log), 1);
    }

    /// A batch id the SDK could never have produced is rejected before the
    /// service touches the database. The value is unauthenticated, lands in a
    /// unique btree index and is echoed into logs, so it is validated rather
    /// than trusted.
    #[tokio::test]
    async fn add_events_rejects_malformed_batch_ids() {
        // Over the 128-char bound; a long enough id would otherwise blow the
        // btree tuple limit and 500 inside the transaction.
        let too_long = "a".repeat(MAX_BATCH_ID_LEN + 1);
        // Newline: the log-forgery vector.
        let cases = [
            ("", "empty"),
            (too_long.as_str(), "over length"),
            ("batch\nINFO forged log line", "newline"),
            ("batch\u{1b}[31m", "ansi escape"),
            ("batch'; DROP TABLE--", "quote"),
        ];

        for (bad_id, label) in cases {
            // No query results queued: reaching the DB at all would error
            // differently, which is itself the assertion.
            let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
            let service = SessionReplayService::new(Arc::new(db));
            let payload = encode_events(&make_events(1));

            let err = service
                .add_session_events(1, "session-x", &payload, Some(bad_id))
                .await
                .expect_err(&format!("{label} batch id must be rejected"));

            assert!(
                matches!(err, SessionReplayError::InvalidBatchId { .. }),
                "{label} batch id should be InvalidBatchId, got {err:?}"
            );
        }
    }

    /// Ids the SDK actually emits must survive validation.
    #[test]
    fn batch_id_validation_accepts_what_the_sdk_emits() {
        // crypto.randomUUID()
        assert_eq!(
            batch_id_rejection_reason("3f8a1c2e-9b4d-4a7f-8e11-2c5d6a7b8c90"),
            None
        );
        // the SDK's non-secure-context fallback
        assert_eq!(
            batch_id_rejection_reason("batch_1787127505294_k3j2h1g0f"),
            None
        );
        assert_eq!(
            batch_id_rejection_reason(&"a".repeat(MAX_BATCH_ID_LEN)),
            None
        );
    }

    /// Regression: an empty batch used to claim a marker row while storing no
    /// events, giving an unauthenticated client a way to grow the dedup table
    /// with empty payloads. It must not reach the transaction at all.
    #[tokio::test]
    async fn add_events_ignores_an_empty_batch_without_claiming_a_marker() {
        let session = make_session_model(42, "session-empty", 1);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![session]])
            .into_connection();

        let db = Arc::new(db);
        let service = SessionReplayService::new(db.clone());
        let payload = encode_events(&[]);

        let stored = service
            .add_session_events(1, "session-empty", &payload, Some("batch-empty"))
            .await
            .expect("an empty batch is not an error");
        assert_eq!(stored, 0);

        drop(service);
        let log = Arc::try_unwrap(db)
            .expect("service should be the only other Arc holder")
            .into_transaction_log();

        assert_eq!(
            count_marker_inserts(&log),
            0,
            "an empty batch must not write a dedup marker"
        );
        assert_eq!(count_event_inserts(&log), 0);
    }

    /// The event-count cap is applied before the transaction opens, so the
    /// pooled connection is never held for an attacker-chosen number of
    /// inserts.
    #[tokio::test]
    async fn add_events_rejects_a_batch_over_the_event_cap() {
        let session = make_session_model(42, "session-huge", 1);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![session]])
            .into_connection();

        let db = Arc::new(db);
        let service = SessionReplayService::new(db.clone());
        let payload = encode_events(&make_events(MAX_EVENTS_PER_BATCH + 1));

        let err = service
            .add_session_events(1, "session-huge", &payload, Some("batch-huge"))
            .await
            .expect_err("an oversized batch must be rejected");

        assert!(
            matches!(
                err,
                SessionReplayError::BatchTooLarge { unit: "events", .. }
            ),
            "expected BatchTooLarge, got {err:?}"
        );

        drop(service);
        let log = Arc::try_unwrap(db)
            .expect("service should be the only other Arc holder")
            .into_transaction_log();
        assert_eq!(
            count_event_inserts(&log),
            0,
            "nothing may be written for a rejected batch"
        );
    }

    /// get_project_id_for_session returns the correct project_id when the
    /// session exists.
    #[tokio::test]
    async fn get_project_id_for_session_returns_correct_id() {
        let session = make_session_model(7, "session-xyz", 5);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![session]])
            .into_connection();

        let service = SessionReplayService::new(Arc::new(db));
        let project_id = service
            .get_project_id_for_session("session-xyz")
            .await
            .expect("should find session");

        assert_eq!(project_id, 5);
    }

    /// get_project_id_for_session returns SessionNotFound for an unknown ID.
    #[tokio::test]
    async fn get_project_id_for_session_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![] as Vec<session_replay_sessions::Model>])
            .into_connection();

        let service = SessionReplayService::new(Arc::new(db));
        let result = service.get_project_id_for_session("no-such-session").await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            SessionReplayError::SessionNotFound(_)
        ));
    }

    /// CrossProjectAccess error message includes both the session_replay_id
    /// and project_id so that abuse can be detected from logs.
    #[test]
    fn cross_project_access_error_message_includes_identifiers() {
        let err = SessionReplayError::CrossProjectAccess {
            session_replay_id: "session-abc".to_string(),
            project_id: 42,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("session-abc"),
            "error message must include session_replay_id, got: {msg}"
        );
        assert!(
            msg.contains("42"),
            "error message must include project_id, got: {msg}"
        );
    }

    // -----------------------------------------------------------------
    // Authorization lookups (added with the project-scoping fix)
    //
    // These resolve the project that owns a session/visitor so the
    // handlers can run `project_access_guard!`. If they ever stop
    // returning the owning project, the guard silently checks the wrong
    // project — so pin the behaviour.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn project_id_for_session_pk_returns_owning_project() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![make_session_model(7, "sess-x", 42)]])
            .into_connection();
        let svc = SessionReplayService::new(Arc::new(db));
        assert_eq!(svc.project_id_for_session_pk(7).await.unwrap(), 42);
    }

    #[tokio::test]
    async fn project_id_for_session_pk_errors_when_missing() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![Vec::<session_replay_sessions::Model>::new()])
            .into_connection();
        let svc = SessionReplayService::new(Arc::new(db));
        assert!(svc.project_id_for_session_pk(999).await.is_err());
    }

    /// Deliberately does NOT filter on `is_active`: an authorization
    /// decision has to be made for the row that exists, or an ended
    /// session falls out of scoping entirely and becomes readable by
    /// anyone.
    #[tokio::test]
    async fn project_id_for_session_replay_id_resolves_inactive_sessions() {
        let mut inactive = make_session_model(7, "sess-x", 42);
        inactive.is_active = false;
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results(vec![vec![inactive]])
            .into_connection();
        let svc = SessionReplayService::new(Arc::new(db));
        assert_eq!(
            svc.project_id_for_session_replay_id("sess-x")
                .await
                .unwrap(),
            42
        );
    }
}
