use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Request to create a new funnel
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreateFunnelRequest {
    pub name: String,
    pub description: Option<String>,
    pub steps: Vec<FunnelStep>,
}

/// A step in a funnel
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct FunnelStep {
    pub order: i32,
    pub event_name: String,
    pub conditions: Option<serde_json::Value>,
}

/// Funnel response
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct FunnelResponse {
    pub id: i32,
    pub name: String,
    pub description: Option<String>,
    pub steps: Vec<FunnelStep>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Funnel analysis result
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct FunnelAnalysisResponse {
    pub funnel_id: i32,
    pub funnel_name: String,
    pub total_entries: i64,
    pub steps: Vec<FunnelStepAnalysis>,
    pub conversion_rate: f64,
}

/// Analysis for a single funnel step
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct FunnelStepAnalysis {
    pub step_order: i32,
    pub event_name: String,
    pub users_count: i64,
    pub conversion_rate: f64,
    pub drop_off_rate: f64,
}
