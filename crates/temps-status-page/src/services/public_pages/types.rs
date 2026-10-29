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

/// Most status pages one instance may have.
pub const MAX_STATUS_PAGES: u64 = 100;
/// Most monitors a single status page may show.
pub const MAX_MONITORS_PER_STATUS_PAGE: usize = 50;
/// Longest page title, in characters.
pub const MAX_TITLE_CHARS: usize = 120;
/// Longest page description, in characters.
pub const MAX_DESCRIPTION_CHARS: usize = 1000;
/// Longest public display name of a monitor, in characters.
pub const MAX_DISPLAY_NAME_CHARS: usize = 80;
/// Longest public name of a component group, in characters.
pub const MAX_GROUP_NAME_CHARS: usize = 80;
/// Days of uptime history a public page shows per monitor.
pub const PUBLIC_HISTORY_DAYS: u32 = 90;
/// Longest logo or website URL, in characters.
pub const MAX_PAGE_URL_CHARS: usize = 2048;

/// Color scheme of a public page.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StatusPageTheme {
    /// Follow the visitor's system preference.
    #[default]
    Auto,
    Light,
    Dark,
}

impl StatusPageTheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    /// Parse the stored value. Anything unexpected falls back to `Auto`; the
    /// column has a CHECK constraint, so that only guards against drift.
    pub fn from_stored(value: &str) -> Self {
        match value {
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => Self::Auto,
        }
    }
}

/// A monitor to show on a status page, with the name the public sees. The
/// monitor may belong to any project the caller can manage status pages of.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StatusPageMonitorInput {
    pub monitor_id: i32,
    /// Public name shown instead of the monitor's internal name, e.g. "API".
    pub display_name: String,
    /// Public name of the group to show the component in, e.g. "Temps
    /// Cloud". Components with the same group name are shown together, in
    /// the position of the group's first component. Omit (or send an empty
    /// string) to show it on its own.
    #[serde(default)]
    pub group_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateStatusPageRequest {
    /// Public page title, e.g. "Acme status".
    pub title: String,
    /// Optional short text shown under the title.
    #[serde(default)]
    pub description: Option<String>,
    /// Monitors to show, in display order; at least one. They may come from
    /// several projects: the page is authorized against all of them.
    pub monitors: Vec<StatusPageMonitorInput>,
    /// Look of the public page. Omitted fields take their defaults.
    #[serde(default)]
    pub appearance: Option<StatusPageAppearanceInput>,
}

/// Changes to how a public page looks. Every field is optional: omit it to
/// keep the current value. For the text fields an empty string clears it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct StatusPageAppearanceInput {
    #[serde(default)]
    pub theme: Option<StatusPageTheme>,
    /// Brand color as `#rrggbb`, used for the header and links.
    #[serde(default)]
    #[schema(example = "#4f46e5")]
    pub accent_color: Option<String>,
    /// `https://` URL of a logo shown in the header instead of the title.
    #[serde(default)]
    pub logo_url: Option<String>,
    /// `https://` URL of your site, linked from the header.
    #[serde(default)]
    pub website_url: Option<String>,
    /// Show uptime percentages next to the daily bars.
    #[serde(default)]
    pub show_uptime_percentage: Option<bool>,
    /// Show average response times (per day on hover, and a 90-day trend).
    #[serde(default)]
    pub show_response_times: Option<bool>,
}

/// How a public page looks, as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StatusPageAppearance {
    pub theme: StatusPageTheme,
    pub accent_color: Option<String>,
    pub logo_url: Option<String>,
    pub website_url: Option<String>,
    pub show_uptime_percentage: bool,
    pub show_response_times: bool,
}

impl Default for StatusPageAppearance {
    fn default() -> Self {
        Self {
            theme: StatusPageTheme::Auto,
            accent_color: None,
            logo_url: None,
            website_url: None,
            show_uptime_percentage: true,
            show_response_times: true,
        }
    }
}

impl StatusPageAppearance {
    pub fn from_model(model: &temps_entities::status_pages::Model) -> Self {
        Self {
            theme: StatusPageTheme::from_stored(&model.theme),
            accent_color: model.accent_color.clone(),
            logo_url: model.logo_url.clone(),
            website_url: model.website_url.clone(),
            show_uptime_percentage: model.show_uptime_percentage,
            show_response_times: model.show_response_times,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct UpdateStatusPageRequest {
    #[serde(default)]
    pub title: Option<String>,
    /// New description. An empty string clears it; omit to keep it.
    #[serde(default)]
    pub description: Option<String>,
    /// Replaces the whole monitor selection when present, in display order;
    /// it cannot be empty.
    #[serde(default)]
    pub monitors: Option<Vec<StatusPageMonitorInput>>,
    /// Changes to the page's look; omitted fields keep their value.
    #[serde(default)]
    pub appearance: Option<StatusPageAppearanceInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StatusPageMonitorResponse {
    pub monitor_id: i32,
    /// The monitor's internal name. Shown to project members only; never
    /// published.
    pub monitor_name: String,
    /// The name shown on the public page.
    pub display_name: String,
    /// Group the component is shown in, `None` when on its own.
    pub group_name: Option<String>,
    pub position: i32,
    /// Project the monitor belongs to.
    pub project_id: i32,
    pub project_name: String,
    /// Environment the monitor checks, `None` for a project-level monitor.
    pub environment_id: Option<i32>,
    pub environment_name: Option<String>,
}

/// A project a status page draws components from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StatusPageProject {
    pub id: i32,
    pub name: String,
    pub slug: String,
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
    /// Projects the page's components belong to, by name. Managing the page
    /// needs the matching permission on every one of them.
    pub projects: Vec<StatusPageProject>,
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
    pub appearance: StatusPageAppearance,
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
    /// Share of successful checks that day, `null` when there were none or
    /// the page hides uptime percentages.
    pub uptime_percentage: Option<f64>,
    /// Mean response time of that day's checks in milliseconds, `null` when
    /// none was measured or the page hides response times.
    pub avg_response_time_ms: Option<u32>,
    /// Checks run that day, `null` when the page hides uptime percentages
    /// (the counts would give the percentage away).
    pub total_checks: Option<u32>,
    /// Checks that found the service down; `null` like `total_checks`.
    pub failed_checks: Option<u32>,
    /// Checks that found it slow or partly unavailable; `null` like
    /// `total_checks`.
    pub degraded_checks: Option<u32>,
    /// Public headings of the incidents on this page that affected this
    /// component on that day.
    pub incidents: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicStatusComponent {
    /// Admin-chosen display name.
    pub name: String,
    /// Admin-chosen name of the group the component is shown in, `null` when
    /// it stands on its own.
    pub group: Option<String>,
    pub status: PublicComponentStatus,
    /// Uptime over the whole history window, `null` without any checks or
    /// when the page hides uptime percentages.
    pub uptime_percentage: Option<f64>,
    /// Mean response time over the last day with measurements, in
    /// milliseconds; `null` without any or when the page hides them.
    pub avg_response_time_ms: Option<u32>,
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
/// description are never published: the heading is the explicitly public
/// title, else derived from severity, and the text comes only from public
/// updates.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicIncident {
    /// The heading the admin wrote for the public, or one derived from the
    /// severity when they did not.
    pub title: String,
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
    /// How the page looks.
    pub appearance: StatusPageAppearance,
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

/// What a status page would show visitors right now, published or not.
/// Returned to project members only, so a draft can be checked before it
/// goes live.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StatusPagePreviewResponse {
    /// Exactly what the public JSON endpoint would serve.
    pub page: PublicStatusPageResponse,
    /// Exactly what the public HTML page would serve. Self-contained (inline
    /// CSS, no scripts), so it can be shown in a sandboxed frame.
    pub html: String,
}
