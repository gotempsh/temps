// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-project analytics traffic settings and filters: which traffic is kept
//! out of a project's analytics. Datacenter/cloud traffic and known referrer
//! spam are excluded by default; users add their own referrer, IP/CIDR,
//! network (ASN) and country rules.

pub mod filter_handlers;
pub mod filters;
pub mod handlers;
mod ingest_cache;
pub mod referrer_excluded_views;
pub mod service;
pub mod spam_referrers;

pub use filter_handlers::{
    configure_traffic_filter_routes, CreateTrafficFilterRequest, FilteredTrafficReasonCount,
    FilteredTrafficResponse, TrafficFilterListResponse, TrafficFilterResponse,
    TrafficFiltersApiDoc,
};
pub use filters::{
    canonical_referrer_host, normalize_filter_value, CompiledTrafficFilters, FilteredTrafficCount,
    FilteredTrafficReason, FilteredTrafficSummary, NewTrafficFilter, TrafficAttributes,
    TrafficFilter, TrafficFilterError, TrafficFilterKind, TrafficFilterService,
    EXCLUSION_CRAWLER_NAMES, MAX_FILTERS_PER_KIND, SPAM_REFERRER_CRAWLER_NAME,
};
pub use handlers::{
    configure_traffic_settings_routes, DatacenterDetectionStatus, ProjectAnalyticsSettingsResponse,
    TrafficSettingsApiDoc, TrafficSettingsAppState, UpdateProjectAnalyticsSettingsRequest,
};
pub use referrer_excluded_views::ReferrerExcludedPageViews;
pub use service::{
    ProjectAnalyticsSettings, ProjectAnalyticsSettingsError, ProjectAnalyticsSettingsPatch,
    ProjectAnalyticsSettingsService, SettingsChange, DEFAULT_EXCLUDE_DATACENTER_TRAFFIC,
    DEFAULT_EXCLUDE_SPAM_REFERRERS,
};
pub use spam_referrers::{is_spam_referrer, spam_referrer_list_size};
