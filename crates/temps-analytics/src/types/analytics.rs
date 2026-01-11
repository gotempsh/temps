use sea_orm::DbErr;
use serde::Serialize;
use std::collections::HashMap;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AnalyticsError {
    #[error("Database error: {0}")]
    DatabaseError(#[from] DbErr),
    #[error("Session not found")]
    SessionNotFound(String),
    #[error("Invalid visitor ID: {0}")]
    InvalidVisitorId(String),
    #[error("Other error: {0}")]
    Other(String),
}

#[derive(Debug, Serialize)]
pub struct AnalyticsMetricsModel {
    pub unique_visitors: i64,
    pub total_visits: i64,
    pub total_page_views: i64,
    pub views_per_visit: f64,
    pub average_visit_duration: f64,
    pub bounce_rate: f64,
    pub engagement_rate: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Referer {
    pub url: String,
    pub views: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Page {
    pub path: String,
    pub views: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Browser {
    pub name: String,
    pub views: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct OperatingSystem {
    pub name: String,
    pub views: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Location {
    pub country: String,
    pub city: String,
    pub views: u64,
}

#[derive(Debug, Serialize)]
pub struct SessionMetrics {
    pub max_duration_seconds: i32,
    pub mean_duration_seconds: f64,
    pub median_duration_seconds: i32,
    pub bounce_rate: f64,
    pub engagement_rate: f64,
    pub total_sessions: i64,
    pub sessions_with_duration: i64,
    pub sessions_by_country: HashMap<String, u64>,
}
