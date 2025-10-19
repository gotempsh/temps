use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Request to create a new session
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreateSessionRequest {
    pub user_id: Option<String>,
    pub visitor_id: String,
    pub metadata: Option<serde_json::Value>,
}

/// Session response
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SessionResponse {
    pub id: i32,
    pub user_id: Option<String>,
    pub visitor_id: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub duration: Option<i64>,
    pub metadata: Option<serde_json::Value>,
}

/// Session event for replay
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SessionEvent {
    pub timestamp: i64,
    pub event_type: String,
    pub data: serde_json::Value,
}

/// Request to add events to a session
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AddSessionEventsRequest {
    pub events: Vec<SessionEvent>,
}

/// Session replay response
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SessionReplayResponse {
    pub session: SessionResponse,
    pub events: Vec<SessionEvent>,
}
