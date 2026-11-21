// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Request and response shapes of status page maintenance windows.

use serde::{Deserialize, Serialize};
use temps_core::UtcDateTime;
use utoipa::ToSchema;

/// Where a window is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceState {
    /// Not started yet: every field can change.
    Scheduled,
    /// Started: only the title, description and end can change.
    InProgress,
    /// Ended on schedule.
    Completed,
    /// Cancelled before it started, or ended early.
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MaintenanceResponse {
    pub id: i32,
    pub status_page_id: i32,
    /// Shown on the public page and in announcements.
    pub title: String,
    pub description: Option<String>,
    #[schema(value_type = String, format = "date-time")]
    pub starts_at: UtcDateTime,
    #[schema(value_type = String, format = "date-time")]
    pub ends_at: UtcDateTime,
    #[schema(value_type = Option<String>, format = "date-time")]
    pub cancelled_at: Option<UtcDateTime>,
    pub state: MaintenanceState,
    /// The page's monitors the window covers.
    pub monitor_ids: Vec<i32>,
    #[schema(value_type = String, format = "date-time")]
    pub created_at: UtcDateTime,
    #[schema(value_type = String, format = "date-time")]
    pub updated_at: UtcDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MaintenanceListResponse {
    /// Newest first.
    pub items: Vec<MaintenanceResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateMaintenanceRequest {
    /// Public title, at most 120 characters.
    pub title: String,
    /// Public description, at most 2000 characters.
    #[serde(default)]
    pub description: Option<String>,
    /// No earlier than a few minutes ago: a window cannot be backdated.
    #[schema(value_type = String, format = "date-time")]
    pub starts_at: UtcDateTime,
    /// After `starts_at`, at most 7 days later.
    #[schema(value_type = String, format = "date-time")]
    pub ends_at: UtcDateTime,
    /// Monitors on this page the window covers; at least one.
    pub monitor_ids: Vec<i32>,
}

/// Fields left out are unchanged. Once a window has started, only `title`,
/// `description` and `ends_at` can change.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct UpdateMaintenanceRequest {
    pub title: Option<String>,
    /// An empty string removes the description.
    pub description: Option<String>,
    #[schema(value_type = Option<String>, format = "date-time")]
    pub starts_at: Option<UtcDateTime>,
    #[schema(value_type = Option<String>, format = "date-time")]
    pub ends_at: Option<UtcDateTime>,
    pub monitor_ids: Option<Vec<i32>>,
}
