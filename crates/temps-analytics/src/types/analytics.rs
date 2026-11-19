// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::DbErr;
use serde::Serialize;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AnalyticsError {
    #[error("Database error: {0}")]
    DatabaseError(#[from] DbErr),
    #[error("Session not found")]
    SessionNotFound(String),
    #[error("Project {0} not found")]
    ProjectNotFound(i32),
    #[error("Invalid visitor ID: {0}")]
    InvalidVisitorId(String),
    #[error("Invalid enrichment data: {0}")]
    InvalidEnrichmentData(String),
    #[error("Other error: {0}")]
    Other(String),
    #[error("AI summary request is rate limited: {reason}")]
    AiRateLimited { reason: &'static str },
    /// The request-log backend refused the query for now (its per-project
    /// budget or the server's concurrent aggregations). Retrying shortly works;
    /// it is not a server fault.
    #[error("API traffic {operation} is busy ({reason}); retry in a few seconds")]
    TrafficBusy {
        operation: &'static str,
        reason: &'static str,
    },
    #[error(
        "API traffic {operation} exceeded its {timeout_seconds}-second deadline; narrow the \
         time range or retry"
    )]
    TrafficTimeout {
        operation: &'static str,
        timeout_seconds: u64,
    },
    /// The query cannot run as asked (an invalid filter, too many groups); the
    /// detail says what to change.
    #[error("API traffic {operation} cannot run as asked: {detail}")]
    TrafficInvalidQuery {
        operation: &'static str,
        detail: String,
    },
    /// Any other request-log backend failure. `detail` can name internal
    /// hosts, so it is logged and never returned to the caller.
    #[error("API traffic {operation} failed in the request-log backend: {detail}")]
    TrafficStorage {
        operation: &'static str,
        detail: String,
    },
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
