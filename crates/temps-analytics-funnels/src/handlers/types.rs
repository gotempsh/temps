use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_core::DateTime;
use utoipa::ToSchema;

use crate::services::{FunnelService, SmartFilter};

pub struct AppState {
    pub funnel_service: Arc<FunnelService>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct FunnelResponse {
    pub id: i32,
    pub name: String,
    pub description: Option<String>,
    pub is_active: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreateFunnelResponse {
    pub funnel_id: i32,
    pub message: String,
}

// HTTP Request/Response types - separate from service layer types

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreateFunnelRequest {
    pub name: String,
    pub description: Option<String>,
    pub steps: Vec<CreateFunnelStep>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreateFunnelStep {
    pub event_name: String,
    #[serde(default)]
    pub event_filter: Vec<SmartFilter>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct FunnelMetricsResponse {
    pub funnel_id: i32,
    pub funnel_name: String,
    pub total_entries: u64,
    pub step_conversions: Vec<StepConversionResponse>,
    pub overall_conversion_rate: f64,
    pub average_completion_time_seconds: f64,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct StepConversionResponse {
    pub step_id: i32,
    pub step_name: String,
    pub step_order: i32,
    pub completions: u64,
    pub conversion_rate: f64,
    pub drop_off_rate: f64,
    pub average_time_to_complete_seconds: f64,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct GetFunnelMetricsQuery {
    pub environment_id: Option<i32>,
    pub country_code: Option<String>,
    pub start_date: Option<DateTime>,
    pub end_date: Option<DateTime>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EventTypesResponse {
    pub events: Vec<EventType>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EventType {
    pub name: String,
    pub count: i64,
}
