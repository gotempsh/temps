// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! DTOs for publishable status pages.
//!
//! Two audiences, two families of types:
//!
//! - **Admin** (`StatusPage*`): returned to authenticated project members. They
//!   carry internal identifiers and the monitor's internal name so the console
//!   can manage the page.
//! - **Public** (`Public*`): the only shapes the unauthenticated endpoint ever
//!   serializes. They are built from scratch from an allowlist of fields —
//!   admin-chosen display names, fixed status vocabularies, timestamps and
//!   explicitly public incident text — and contain no ids, URLs, hostnames,
//!   project/environment names or probe error text. Never add a field here
//!   without checking it against that list.

use serde::{Deserialize, Serialize};
use temps_core::UtcDateTime;
use utoipa::ToSchema;

/// Most status pages a single project may own.
pub const MAX_STATUS_PAGES_PER_PROJECT: u64 = 10;
/// Most monitors a single status page may show.
pub const MAX_MONITORS_PER_STATUS_PAGE: usize = 50;
/// Longest page title, in characters.
pub const MAX_TITLE_CHARS: usize = 120;
/// Longest page description, in characters.
pub const MAX_DESCRIPTION_CHARS: usize = 1000;
/// Longest public display name of a monitor, in characters.
pub const MAX_DISPLAY_NAME_CHARS: usize = 80;
/// Days of uptime history a public page shows per monitor.
pub const PUBLIC_HISTORY_DAYS: u32 = 90;

/// A monitor to show on a status page, with the name the public sees.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StatusPageMonitorInput {
    pub monitor_id: i32,
    /// Public name shown instead of the monitor's internal name, e.g. "API".
    pub display_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateStatusPageRequest {
    /// Public page title, e.g. "Acme status".
    pub title: String,
    /// Optional short text shown under the title.
    #[serde(default)]
    pub description: Option<String>,
    /// Monitors to show, in display order. Can be empty while drafting; a page
    /// needs at least one monitor before it can be published.
    #[serde(default)]
    pub monitors: Vec<StatusPageMonitorInput>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct UpdateStatusPageRequest {
    #[serde(default)]
    pub title: Option<String>,
    /// New description. An empty string clears it; omit to keep it.
    #[serde(default)]
    pub description: Option<String>,
    /// Replaces the whole monitor selection when present, in display order.
    #[serde(default)]
    pub monitors: Option<Vec<StatusPageMonitorInput>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StatusPageMonitorResponse {
    pub monitor_id: i32,
    /// The monitor's internal name. Shown to project members only; never
    /// published.
    pub monitor_name: String,
    /// The name shown on the public page.
    pub display_name: String,
    pub position: i32,
}

/// Whether published status pages have an address people can open.
///
/// Follows the capability-endpoint pattern: an unconfigured install answers
/// with `configured: false`, a reason and the console path that fixes it,
/// rather than a made-up URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PublicUrlStatus {
    /// `true` when `public_url` is present on the page.
    pub configured: bool,
    /// Why there is no public URL, or why the one there will not open from
    /// outside this network (for example a loopback address). `None` when
    /// nothing is known to be wrong.
    #[schema(example = "Public URL is not configured.")]
    pub reason: Option<String>,
    /// Console path where the operator sets it. Present whenever `reason` is.
    #[schema(example = "/settings#public-ingress")]
    pub setup_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StatusPageResponse {
    pub id: i32,
    pub project_id: i32,
    pub title: String,
    pub description: Option<String>,
    /// Unguessable token in the public URL.
    pub slug: String,
    pub is_published: bool,
    /// Public HTML page. Serves 404 while the page is unpublished. `None`
    /// when this install has no public address to build it from; see
    /// `public_url_status` for why and where to fix it. Temps never invents a
    /// host for it.
    pub public_url: Option<String>,
    /// Machine-readable form of the public page. `None` exactly when
    /// `public_url` is.
    pub public_json_url: Option<String>,
    /// Whether `public_url` exists, and if not (or if it will not open from
    /// outside this network) what to do about it.
    pub public_url_status: PublicUrlStatus,
    pub monitors: Vec<StatusPageMonitorResponse>,
    #[schema(value_type = Option<String>, format = "date-time")]
    pub published_at: Option<UtcDateTime>,
    #[schema(value_type = String, format = "date-time")]
    pub created_at: UtcDateTime,
    #[schema(value_type = String, format = "date-time")]
    pub updated_at: UtcDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StatusPageListResponse {
    pub items: Vec<StatusPageResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

/// Current state of a public component (one selected monitor), or of the page
/// as a whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PublicComponentStatus {
    /// No recent check result, paused, or not deployed yet.
    Unknown,
    Operational,
    Degraded,
    Outage,
}

impl PublicComponentStatus {
    /// Map the probe vocabulary written by the health checker onto the public
    /// one. Anything unrecognised is `Unknown`, so a new internal status can
    /// never surface verbatim.
    pub fn from_check_status(status: &str) -> Self {
        match status {
            "operational" => Self::Operational,
            "degraded" | "partial_outage" => Self::Degraded,
            "down" | "major_outage" => Self::Outage,
            _ => Self::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "No data",
            Self::Operational => "Operational",
            Self::Degraded => "Degraded performance",
            Self::Outage => "Outage",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Operational => "operational",
            Self::Degraded => "degraded",
            Self::Outage => "outage",
        }
    }
}

/// Summary of one UTC day of checks for one component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PublicDayStatus {
    NoData,
    Operational,
    Degraded,
    Outage,
}

impl PublicDayStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoData => "no_data",
            Self::Operational => "operational",
            Self::Degraded => "degraded",
            Self::Outage => "outage",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicUptimeDay {
    /// UTC calendar day, `YYYY-MM-DD`.
    pub date: String,
    pub status: PublicDayStatus,
    /// Share of successful checks that day, `null` when there were none.
    pub uptime_percentage: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicStatusComponent {
    /// Admin-chosen display name.
    pub name: String,
    pub status: PublicComponentStatus,
    /// Uptime over the whole history window, `null` without any checks.
    pub uptime_percentage: Option<f64>,
    /// One entry per day, oldest first, always `history_days` long.
    pub days: Vec<PublicUptimeDay>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PublicIncidentSeverity {
    Minor,
    Major,
    Critical,
}

impl PublicIncidentSeverity {
    pub fn from_internal(severity: &str) -> Self {
        match severity {
            "critical" => Self::Critical,
            "major" => Self::Major,
            _ => Self::Minor,
        }
    }

    pub fn heading(self) -> &'static str {
        match self {
            Self::Critical => "Major outage",
            Self::Major => "Partial outage",
            Self::Minor => "Degraded service",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PublicIncidentStatus {
    Investigating,
    Identified,
    Monitoring,
    Resolved,
}

impl PublicIncidentStatus {
    pub fn from_internal(status: &str) -> Self {
        match status {
            "identified" => Self::Identified,
            "monitoring" => Self::Monitoring,
            "resolved" => Self::Resolved,
            _ => Self::Investigating,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Investigating => "Investigating",
            Self::Identified => "Identified",
            Self::Monitoring => "Monitoring",
            Self::Resolved => "Resolved",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicIncidentUpdate {
    pub status: PublicIncidentStatus,
    /// Text of an update explicitly marked public.
    pub message: String,
    #[schema(value_type = String, format = "date-time")]
    pub created_at: UtcDateTime,
}

/// An incident with at least one public update. The internal title and
/// description are never published: the heading is derived from severity and
/// the text comes only from public updates.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicIncident {
    pub severity: PublicIncidentSeverity,
    pub status: PublicIncidentStatus,
    /// Display names of the selected monitors the incident affects; empty for
    /// a project-wide incident.
    pub affected_components: Vec<String>,
    #[schema(value_type = String, format = "date-time")]
    pub started_at: UtcDateTime,
    #[schema(value_type = Option<String>, format = "date-time")]
    pub resolved_at: Option<UtcDateTime>,
    /// Public updates, newest first.
    pub updates: Vec<PublicIncidentUpdate>,
}

/// The public, unauthenticated view of a published status page.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicStatusPageResponse {
    pub title: String,
    pub description: Option<String>,
    /// Worst current state across components and active public incidents.
    pub status: PublicComponentStatus,
    pub components: Vec<PublicStatusComponent>,
    /// Active and recently resolved incidents, newest first.
    pub incidents: Vec<PublicIncident>,
    pub history_days: u32,
    /// When this snapshot was computed. Public pages are served from a
    /// periodically refreshed snapshot, never computed per request.
    #[schema(value_type = String, format = "date-time")]
    pub generated_at: UtcDateTime,
}
