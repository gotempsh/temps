// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/// Files sent with error events and feedback (ADR 111, D4).
pub mod attachments;
pub mod error_alert_service;
pub mod error_analytics_service;
pub mod error_crud_service;
pub mod error_ingestion_service;
pub mod error_tracking_service;
pub mod group_workflow;
pub mod inbound_filters;
pub mod performance;
pub mod source_map_service;
pub mod types;

pub use error_alert_service::ErrorAlertService;
pub use error_analytics_service::{ErrorAnalyticsService, ErrorDashboardStats};
pub use error_crud_service::ErrorCRUDService;
pub use error_ingestion_service::ErrorIngestionService;
pub use error_tracking_service::ErrorTrackingService;
pub use group_workflow::{
    BulkGroupAction, BulkGroupFilter, BulkGroupOutcome, BulkGroupSelector, GroupFingerprint,
    GroupWorkflowService, MergeOutcome, SplitOutcome, UnmergeOutcome, BULK_GROUP_LIMIT,
    MAX_MERGE_SOURCES, MAX_SPLIT_FINGERPRINTS,
};
pub use source_map_service::{SourceMapService, MAX_SOURCE_MAP_BYTES};
pub use types::*;

pub mod feedback_service;

pub mod release_service;

/// Release health from Sentry sessions (ADR 111, D3).
pub mod release_health;

pub mod client_outcome_service;
