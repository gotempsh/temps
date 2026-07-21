use sea_orm::DbErr;
use serde::Serialize;
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
