// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Request, response and error types of a project's identity settings.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use temps_entities::analytics_identity_mode::AnalyticsIdentityMode;
use thiserror::Error;
use utoipa::ToSchema;

/// How a project tells its visitors apart, and what that costs.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AnalyticsIdentitySettings {
    pub project_id: i32,
    /// `persistent` (the default): ids stored in a cookie or `localStorage`,
    /// visitors recognised across days. `cookieless`: nothing stored on the
    /// device; a visitor is a new visitor every day.
    pub mode: AnalyticsIdentityMode,
    /// Whether `identify()` from the browser is applied (ADR 109 D4). When
    /// `false`, only the app's backend can identify a visitor. Defaults to
    /// `true`. Ignored in `cookieless` mode, where identify stores nothing.
    pub accept_browser_identify: bool,
    /// What the browser SDK must be told for this mode, so it writes nothing
    /// either: `cookieless: true`, or `data-cookieless="true"` on the script
    /// tag. `None` in `persistent` mode.
    pub sdk_option: Option<String>,
    /// Reports that behave differently in this mode, for the console to say
    /// so next to them.
    pub limitations: Vec<String>,
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: DateTime<Utc>,
}

/// Body of `PUT /projects/{project_id}/analytics/identity`. Each field is
/// optional; an omitted field keeps its value, and at least one is required.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UpdateAnalyticsIdentitySettings {
    #[serde(default)]
    pub mode: Option<AnalyticsIdentityMode>,
    #[serde(default)]
    pub accept_browser_identify: Option<bool>,
}

#[derive(Debug, Error)]
pub enum AnalyticsIdentityError {
    #[error("Project {project_id} not found")]
    ProjectNotFound { project_id: i32 },

    #[error("Invalid identity settings for project {project_id}: {message}")]
    Validation { project_id: i32, message: String },

    #[error("Database error during {operation} for project {project_id}: {source}")]
    Database {
        project_id: i32,
        operation: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },
}

/// What cookieless mode changes, worded for the console.
pub const COOKIELESS_LIMITATIONS: &[&str] = &[
    "A returning visitor counts as a new visitor the next day.",
    "Unique visitors over several days are the sum of each day's unique visitors.",
    "Visitor profiles and journeys cover a single day.",
    "Session replay is off: a replay needs an id stored on the device.",
    "Identify stores nothing: a user id on a daily visitor would link its days together.",
];

impl AnalyticsIdentitySettings {
    pub fn new(
        project_id: i32,
        mode: AnalyticsIdentityMode,
        accept_browser_identify: bool,
        updated_at: DateTime<Utc>,
    ) -> Self {
        let cookieless = mode.is_cookieless();
        Self {
            project_id,
            mode,
            accept_browser_identify,
            sdk_option: cookieless.then(|| "cookieless: true".to_string()),
            limitations: if cookieless {
                COOKIELESS_LIMITATIONS
                    .iter()
                    .map(|s| s.to_string())
                    .collect()
            } else {
                Vec::new()
            },
            updated_at,
        }
    }
}
