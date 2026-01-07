//! Analytics Event Entity
//!
//! Represents captured analytics events from @temps-sdk/react-analytics.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "analytics_events")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,

    /// Type of event: "event", "speed", "session_init", "session_events"
    pub event_type: String,

    /// Name of the event (for "event" type): "page_view", "page_leave", "heartbeat", or custom
    pub event_name: Option<String>,

    /// Request path from the SDK (e.g., "/dashboard")
    pub request_path: Option<String>,

    /// Query string from the SDK (e.g., "?filter=active")
    pub request_query: Option<String>,

    /// Domain from the SDK (e.g., "localhost:3000")
    pub domain: Option<String>,

    /// Session ID from localStorage
    pub session_id: Option<String>,

    /// Request ID from meta tag
    pub request_id: Option<String>,

    /// Full JSON payload from the SDK
    pub payload: String,

    /// ISO 8601 timestamp when the event was received
    pub received_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

/// Event types that can be stored
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    /// Regular analytics event (page_view, page_leave, heartbeat, custom)
    Event,
    /// Web vitals / speed metrics
    Speed,
    /// Session replay initialization
    SessionInit,
    /// Session replay events (rrweb data)
    SessionEvents,
}

impl EventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            EventType::Event => "event",
            EventType::Speed => "speed",
            EventType::SessionInit => "session_init",
            EventType::SessionEvents => "session_events",
        }
    }
}

impl std::fmt::Display for EventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}
