use super::user_agent::BrowserInfo;
use anyhow::Result;
use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::{DateTime, Utc};
use flate2::read::ZlibDecoder;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, FromQueryResult,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Read;
use std::sync::Arc;
use temps_core::UtcDateTime;
use thiserror::Error;
use tracing::{debug, error, info};

use temps_entities::{session_replay_events, session_replay_sessions, visitor};

#[derive(Error, Debug)]
pub enum SessionReplayError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("Visitor not found: {0}")]
    VisitorNotFound(String),

    #[error("Session not found: {0}")]
    SessionNotFound(String),

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
    pub custom_data: Option<String>,
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
    pub visitor_custom_data: Option<String>,
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
    pub visitor_custom_data: Option<String>,
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
    pub visitor_custom_data: Option<String>,
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

        // Look up visitor by visitor_id GUID
        let visitor = visitor::Entity::find()
            .filter(visitor::Column::VisitorId.eq(&metadata.visitor_id))
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
            environment_id: Set(environment_id.unwrap_or(0)),
            deployment_id: Set(deployment_id.unwrap_or(0)),
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

    /// Add events to an existing session (events are already base64 encoded and compressed)
    pub async fn add_session_events(
        &self,
        session_id: &str,
        events_base64: &str,
    ) -> Result<usize, SessionReplayError> {
        info!("Adding events to session: {}", session_id);

        // Verify session exists by session_replay_id
        let session = session_replay_sessions::Entity::find()
            .filter(session_replay_sessions::Column::SessionReplayId.eq(session_id))
            .filter(session_replay_sessions::Column::IsActive.eq(true))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(|| SessionReplayError::SessionNotFound(session_id.to_string()))?;

        // Decode and decompress events
        let compressed = STANDARD.decode(events_base64)?;
        let mut decoder = ZlibDecoder::new(&compressed[..]);
        let mut decompressed = String::new();
        decoder.read_to_string(&mut decompressed).map_err(|e| {
            SessionReplayError::DecompressionError(format!("Failed to decompress events: {}", e))
        })?;

        let events: Value = serde_json::from_str(&decompressed)?;

        let mut event_count = 0;
        let mut min_timestamp: Option<i64> = None;
        let mut max_timestamp: Option<i64> = None;

        // Extract events handling both formats
        let events_to_store = self.extract_events_from_json(&events)?;

        for event in &events_to_store {
            let timestamp = event.get("timestamp").and_then(|t| t.as_i64()).unwrap_or(0);

            // Track min/max timestamps for duration calculation
            min_timestamp = Some(min_timestamp.map_or(timestamp, |min| min.min(timestamp)));
            max_timestamp = Some(max_timestamp.map_or(timestamp, |max| max.max(timestamp)));

            let event_type = event.get("type").and_then(|t| t.as_i64()).map(|t| t as i32);

            let event_model = session_replay_events::ActiveModel {
                id: sea_orm::NotSet,
                session_id: Set(session.id),
                data: Set(event.to_string()),
                timestamp: Set(timestamp),
                r#type: Set(event_type),
                is_active: Set(true),
            };

            event_model.insert(self.db.as_ref()).await?;
            event_count += 1;
        }

        // Update session duration if we have timestamps
        if let (Some(min), Some(max)) = (min_timestamp, max_timestamp) {
            let duration_ms = (max - min) as i32;

            // Get current duration if exists
            let current_duration = session.duration.unwrap_or(0);
            let new_duration = current_duration.max(duration_ms);

            if new_duration > current_duration {
                self.update_session_duration(session_id, new_duration)
                    .await?;
            }
        }

        info!("Added {} events to session {}", event_count, session_id);
        Ok(event_count)
    }

    /// Store a packed session replay from rrweb
    pub async fn store_packed_session_replay(
        &self,
        packed_data: PackedEvents,
        deployment_id: i32,
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
        let environment_id = visitor.environment_id;

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

        for event in events_to_store {
            let timestamp = event.get("timestamp").and_then(|t| t.as_i64()).unwrap_or(0);

            let event_type = event.get("type").and_then(|t| t.as_i64()).map(|t| t as i32);

            // Need to get the session's integer ID first
            let session_int_id = session_replay_sessions::Entity::find()
                .filter(
                    session_replay_sessions::Column::SessionReplayId.eq(&packed_data.session_id),
                )
                .one(self.db.as_ref())
                .await?
                .map(|s| s.id)
                .unwrap_or(0); // This should exist since we just created it

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
        deployment_id: i32,
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
            let environment_id = visitor.environment_id;

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
            let mut min_timestamp: Option<i64> = None;
            let mut max_timestamp: Option<i64> = None;

            for event in &events_to_store {
                let timestamp = event.get("timestamp").and_then(|t| t.as_i64()).unwrap_or(0);

                // Track min/max timestamps for duration calculation
                min_timestamp = Some(min_timestamp.map_or(timestamp, |min| min.min(timestamp)));
                max_timestamp = Some(max_timestamp.map_or(timestamp, |max| max.max(timestamp)));

                let event_type = event.get("type").and_then(|t| t.as_i64()).map(|t| t as i32);

                // Get the session's integer ID
                let session_int_id = session_replay_sessions::Entity::find()
                    .filter(session_replay_sessions::Column::SessionReplayId.eq(session_id))
                    .one(self.db.as_ref())
                    .await?
                    .map(|s| s.id)
                    .unwrap_or(0); // This should exist

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

            // Update session duration if we have timestamps
            if let (Some(min), Some(max)) = (min_timestamp, max_timestamp) {
                let duration_ms = (max - min) as i32;
                self.update_session_duration(session_id, duration_ms)
                    .await?;
            }

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

        // Build filtered base for total count
        let mut count_select = session_replay_sessions::Entity::find()
            .filter(session_replay_sessions::Column::ProjectId.eq(project_id));
        if let Some(env_id) = environment_id {
            count_select =
                count_select.filter(session_replay_sessions::Column::EnvironmentId.eq(env_id));
        }
        let total_count: u64 = count_select.count(self.db.as_ref()).await?;

        // Use SeaORM query builder
        let mut query = session_replay_sessions::Entity::find()
            .filter(session_replay_sessions::Column::ProjectId.eq(project_id))
            .inner_join(visitor::Entity)
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
                v.custom_data as visitor_custom_data
            FROM session_replay_sessions s
            INNER JOIN visitor v ON s.visitor_id = v.id
            WHERE s.visitor_id = $1
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
            pub visitor_custom_data: Option<String>,
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
                v.custom_data as visitor_custom_data
            FROM session_replay_sessions s
            INNER JOIN visitor v ON s.visitor_id = v.id
            WHERE s.id = $1
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

        // Get events using the integer session ID
        let events = session_replay_events::Entity::find()
            .filter(session_replay_events::Column::SessionId.eq(row.id))
            .order_by_asc(session_replay_events::Column::Timestamp)
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
                v.custom_data as visitor_custom_data
            FROM session_replay_sessions s
            INNER JOIN visitor v ON s.visitor_id = v.id
            WHERE s.id = $1
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
            pub visitor_custom_data: Option<String>,
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
        })
    }

    /// Unpack events without storing them (useful for debugging or inspection)
    pub fn unpack_events_only(
        &self,
        packed_data: &PackedEvents,
    ) -> Result<UnpackedEvents, SessionReplayError> {
        self.unpack_events(packed_data)
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

        // Soft delete all events for this session using the integer ID
        let events = session_replay_events::Entity::find()
            .filter(session_replay_events::Column::SessionId.eq(session_int_id))
            .filter(session_replay_events::Column::IsActive.eq(true))
            .all(self.db.as_ref())
            .await?;

        for event in events {
            let mut event: session_replay_events::ActiveModel = event.into();
            event.is_active = Set(false);
            event.update(self.db.as_ref()).await?;
        }

        Ok(())
    }
}

// Tests are commented out for now until we can resolve compilation issues
// #[cfg(test)]
// mod tests {
//     use super::*;
//     use serde_json::json;
//
//     #[test]
//     fn test_unpack_events_not_packed() {
//         // Test implementation would go here
//     }
// }
