// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use temps_core::UtcDateTime;

use sea_orm::{DbErr, FromQueryResult};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Represents a single exception with its stack trace
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExceptionData {
    /// The type/class of the exception (e.g., "TypeError", "ValueError")
    pub exception_type: String,
    /// The exception message/value
    pub exception_value: Option<String>,
    /// The stack trace for this specific exception
    pub stack_trace: Option<serde_json::Value>,
    /// Mechanism information (how the exception was captured)
    pub mechanism: Option<serde_json::Value>,
    /// Module where the exception occurred
    pub module: Option<String>,
    /// Thread ID if available
    pub thread_id: Option<String>,
}

#[derive(Error, Debug)]
pub enum ErrorTrackingError {
    #[error("Database error: {0}")]
    Database(#[from] DbErr),

    #[error("Error group not found")]
    GroupNotFound,

    /// One or more group ids in a bulk request do not belong to the project.
    /// Maps to HTTP 404 Not Found.
    #[error(
        "Error groups {group_ids:?} are not in project {project_id}; a bulk request must name only this project's groups"
    )]
    GroupsNotFound {
        project_id: i32,
        group_ids: Vec<i32>,
    },

    /// A group id that is not a group of the project. Maps to HTTP 404.
    #[error("Error group {group_id} not found in project {project_id}")]
    GroupNotInProject { project_id: i32, group_id: i32 },

    /// A comment id that is not a comment on the group. Maps to HTTP 404.
    #[error("Comment {comment_id} not found on error group {group_id}")]
    CommentNotFound { group_id: i32, comment_id: i64 },

    /// A comment may only be changed by its author (or, for a delete, a
    /// project administrator). Maps to HTTP 403.
    #[error(
        "Comment {comment_id} on error group {group_id} can only be {action} by its author{admins}"
    )]
    NotCommentAuthor {
        group_id: i32,
        comment_id: i64,
        action: &'static str,
        admins: &'static str,
    },

    /// A saved search id that is not one of the project's. Maps to HTTP 404.
    #[error("Saved search {saved_search_id} not found in project {project_id}")]
    SavedSearchNotFound {
        project_id: i32,
        saved_search_id: i32,
    },

    /// A merge or split names a group that is already merged into another.
    /// Maps to HTTP 409 Conflict.
    #[error(
        "Error group {group_id} is already merged into group {merged_into_group_id}; unmerge it first"
    )]
    GroupAlreadyMerged {
        group_id: i32,
        merged_into_group_id: i32,
    },

    /// A merge would move a group that has itself absorbed other groups,
    /// which would make unmerging ambiguous. Maps to HTTP 409 Conflict.
    #[error(
        "Error group {group_id} has other groups merged into it; unmerge those before merging it away"
    )]
    GroupIsMergeTarget { group_id: i32 },

    /// Unmerge was asked for a group that is not part of a merge.
    /// Maps to HTTP 409 Conflict.
    #[error("Error group {group_id} is not merged into another group")]
    GroupNotMerged { group_id: i32 },

    /// A split names fingerprints that do not route to the group.
    /// Maps to HTTP 404 Not Found.
    #[error(
        "Fingerprints {fingerprints:?} do not belong to error group {group_id}, or belong to a merge"
    )]
    FingerprintsNotInGroup {
        group_id: i32,
        fingerprints: Vec<String>,
    },

    /// A saved search someone other than its owner tried to change.
    /// Maps to HTTP 403 Forbidden.
    #[error(
        "Saved search {saved_search_id} in project {project_id} can only be {action} by its owner{admins}"
    )]
    NotSavedSearchOwner {
        project_id: i32,
        saved_search_id: i32,
        action: &'static str,
        admins: &'static str,
    },

    /// The owner already has a saved search with this name in the project.
    /// Maps to HTTP 409 Conflict.
    #[error("You already have a saved search named '{name}' in project {project_id}")]
    SavedSearchExists { project_id: i32, name: String },

    /// The owner holds as many saved searches in the project as they may.
    /// Maps to HTTP 400 Bad Request.
    #[error(
        "User {user_id} already has the maximum of {limit} saved searches in project {project_id}; delete one first"
    )]
    SavedSearchLimit {
        project_id: i32,
        user_id: i32,
        limit: u64,
    },

    /// An inbound filter id that is not a rule of the project.
    /// Maps to HTTP 404 Not Found.
    #[error("Inbound filter {filter_id} not found in project {project_id}")]
    InboundFilterNotFound { project_id: i32, filter_id: i32 },

    /// A built-in inbound filter kind the project already has.
    /// Maps to HTTP 409 Conflict.
    #[error("Project {project_id} already has a '{kind}' inbound filter")]
    InboundFilterExists { project_id: i32, kind: String },

    /// The project holds as many inbound filters as it may.
    /// Maps to HTTP 400 Bad Request.
    #[error("Project {project_id} already has the maximum of {limit} inbound filters")]
    InboundFilterLimit { project_id: i32, limit: u64 },

    #[error("Error event not found")]
    EventNotFound,

    /// An attachment id that is not one of the project's, or whose stored
    /// object is gone (ADR 111, D4). Maps to HTTP 404 Not Found.
    #[error("Attachment {attachment_id} not found in project {project_id}")]
    AttachmentNotFound { project_id: i32, attachment_id: i64 },

    /// Blob storage is not set up or cannot be reached, so an attachment
    /// cannot be read or deleted. Maps to HTTP 503 Service Unavailable.
    #[error("Attachment storage is unavailable for project {project_id}: {reason}")]
    AttachmentStorageUnavailable { project_id: i32, reason: String },

    /// No issue link with that id on the group (ADR 111, D5). 404.
    #[error("Issue link {link_id} not found on error group {group_id}")]
    IssueLinkNotFound { group_id: i32, link_id: i64 },

    /// The URL is already linked to the group, or an issue opened for the
    /// group is still open in the repository. 409.
    #[error("Error group {group_id} is already linked to {url}")]
    IssueAlreadyLinked { group_id: i32, url: String },

    /// The group has as many linked issues as it may. 409.
    #[error("Error group {group_id} already has the maximum of {limit} linked issues")]
    IssueLinkLimit { group_id: i32, limit: usize },

    /// Another request is opening an issue for the group. 409.
    #[error("An issue is already being opened for error group {group_id}; try again in a moment")]
    IssueCreationInProgress { group_id: i32 },

    /// The issue was opened in the repository, but its link could not be
    /// stored afterwards (the group filled up meanwhile, or the database
    /// failed). The issue exists; it can be linked by its URL. 409 when the
    /// group is full or already has it, 500 otherwise.
    #[error(
        "Opened issue #{number} ({url}) for error group {group_id}, but could not link it: {source}; link it by its URL to keep it with the group"
    )]
    IssueOpenedNotLinked {
        group_id: i32,
        number: i64,
        url: String,
        #[source]
        source: Box<ErrorTrackingError>,
    },

    /// The server has no way to reach repositories. 503.
    #[error("Source control is unavailable for project {project_id}: {reason}")]
    SourceControlUnavailable { project_id: i32, reason: String },

    /// The project's repository is not connected, not supported, or refused
    /// or failed the request; the status follows the cause.
    #[error("{source}")]
    SourceControl {
        project_id: i32,
        #[source]
        source: temps_core::SourceControlError,
    },

    #[error("Invalid fingerprint")]
    InvalidFingerprint,

    #[error("Vector embedding service error: {0}")]
    EmbeddingService(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Project not found")]
    ProjectNotFound,

    /// The project already has the maximum number of error alert rules.
    /// Maps to HTTP 409 Conflict.
    #[error(
        "Project {project_id} already has {existing} error alert rules; the limit is {limit} per project. Delete or reuse an existing rule"
    )]
    AlertRuleLimitReached {
        project_id: i32,
        existing: u64,
        limit: u64,
    },

    /// Returned when an uploaded file or request body exceeds the allowed size.
    ///
    /// Maps to HTTP 413 Payload Too Large.
    #[error("Payload too large: {field} exceeds {limit_bytes} bytes (got {actual_bytes})")]
    PayloadTooLarge {
        field: String,
        limit_bytes: usize,
        actual_bytes: usize,
    },
}

/// Native durable ingestion result; duplicate SDK events do not increment counts
/// or re-run alert/regression transitions.
#[derive(Debug, Clone, Copy)]
pub struct ErrorIngestReceipt {
    pub group_id: i32,
    pub stored: bool,
}

/// Native error metadata filters; both tags fields must be provided together.
#[derive(Debug, Default, Clone)]
pub struct ErrorEventFilter {
    pub user_id: Option<String>,
    pub tag_key: Option<String>,
    pub tag_value: Option<String>,
}

/// Removes NUL from every string and object key in a JSON value.
fn strip_nul(value: &mut serde_json::Value) {
    use serde_json::Value;
    match value {
        Value::String(text) => text.retain(|c| c != '\0'),
        Value::Array(items) => items.iter_mut().for_each(strip_nul),
        Value::Object(fields) => {
            if fields.keys().any(|key| key.contains('\0')) {
                // A key that was already clean keeps its value when a stripped key
                // lands on it: that's the field the SDK sent under its real name.
                let (dirty, clean): (Vec<_>, Vec<_>) = std::mem::take(fields)
                    .into_iter()
                    .partition(|(key, _)| key.contains('\0'));
                *fields = clean.into_iter().collect();
                for (key, field) in dirty {
                    fields.entry(key.replace('\0', "")).or_insert(field);
                }
            }
            fields.values_mut().for_each(strip_nul);
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// Looks for the escape serde_json writes for NUL, keeping none of the output, and
/// stops the serialization at the first one.
#[derive(Default)]
struct NulEscapeScanner {
    matched: usize,
    found: bool,
}

impl NulEscapeScanner {
    const ESCAPE: &[u8] = b"\\u0000";
}

impl std::io::Write for NulEscapeScanner {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        for byte in bytes {
            self.matched = if *byte == Self::ESCAPE[self.matched] {
                self.matched + 1
            } else {
                usize::from(*byte == Self::ESCAPE[0])
            };
            if self.matched == Self::ESCAPE.len() {
                self.found = true;
                return Err(std::io::Error::other("found a NUL escape"));
            }
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl CreateErrorEventData {
    /// The event with NUL removed. Postgres refuses NUL in text and jsonb, so one
    /// in any field would lose the whole event, and SDKs retry a 5xx forever.
    ///
    /// Every field has to be covered, including ones added later, so this looks at
    /// the serialized form, but streams it past a scanner instead of building it:
    /// this runs per event, and only an event with NUL pays for more than the scan.
    pub fn without_nul(self) -> Self {
        let mut scanner = NulEscapeScanner::default();
        if let Err(error) = serde_json::to_writer(&mut scanner, &self) {
            if !scanner.found {
                tracing::warn!("Could not scan error event for NUL: {}", error);
                return self;
            }
        }
        if !scanner.found {
            return self;
        }
        let cleaned = serde_json::to_value(&self).and_then(|mut value| {
            strip_nul(&mut value);
            serde_json::from_value(value)
        });
        match cleaned {
            Ok(cleaned) => cleaned,
            Err(error) => {
                tracing::warn!("Could not strip NUL from error event: {}", error);
                self
            }
        }
    }
}

/// Input data for creating a new error event
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CreateErrorEventData {
    // Source of the error event (e.g., "sentry", "custom", "bugsnag")
    pub source: Option<String>,

    /// `performance` for an issue a trace detector found (ADR 111, D2);
    /// `None` is an ordinary error.
    #[serde(default)]
    pub issue_type: Option<String>,
    /// Identity of this occurrence for retry deduplication when the event
    /// has no Sentry event id: the detector, trace and pattern. The same key
    /// is stored once.
    #[serde(default)]
    pub dedup_key: Option<String>,
    /// Grouping key used instead of the computed fingerprint: occurrences
    /// with the same key land in the same group.
    #[serde(default)]
    pub grouping_key: Option<String>,

    // Raw Sentry event (full event payload from Sentry SDK)
    pub raw_sentry_event: Option<serde_json::Value>,

    // Core error information - list of exceptions (Sentry can have multiple)
    pub exceptions: Vec<ExceptionData>,

    // Legacy fields for backward compatibility (derived from first exception if present)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exception_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exception_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack_trace: Option<serde_json::Value>,

    // Context
    pub url: Option<String>,
    pub user_agent: Option<String>,
    pub referrer: Option<String>,
    pub method: Option<String>,
    pub headers: Option<serde_json::Value>,

    // User information
    pub user_id: Option<String>,
    pub user_email: Option<String>,
    pub user_username: Option<String>,
    pub user_ip_address: Option<String>,
    pub user_segment: Option<String>,
    pub session_id: Option<String>,
    pub user_context: Option<serde_json::Value>,

    // Device/Browser context
    pub browser: Option<String>,
    pub browser_version: Option<String>,
    pub operating_system: Option<String>,
    pub operating_system_version: Option<String>,
    pub device_type: Option<String>,
    pub screen_width: Option<i16>,
    pub screen_height: Option<i16>,
    pub viewport_width: Option<i16>,
    pub viewport_height: Option<i16>,

    // Additional context
    pub request_context: Option<serde_json::Value>,
    pub extra_context: Option<serde_json::Value>,
    pub release_version: Option<String>,
    pub build_number: Option<String>,

    // Server information
    pub server_name: Option<String>,
    pub environment: Option<String>, // Sentry environment field

    // SDK information
    pub sdk_name: Option<String>,
    pub sdk_version: Option<String>,
    pub sdk_integrations: Option<serde_json::Value>, // JSONB - array of SDK integrations

    // Platform information
    pub platform: Option<String>, // node, javascript, python, etc.

    // Transaction information
    pub transaction_name: Option<String>,

    // Breadcrumbs
    pub breadcrumbs: Option<serde_json::Value>, // JSONB - array of breadcrumb objects

    // Request details (expanded)
    pub request_cookies: Option<serde_json::Value>, // JSONB
    pub request_query_string: Option<serde_json::Value>, // JSONB
    pub request_data: Option<serde_json::Value>,    // JSONB - POST data

    // Contexts (expanded device/OS info)
    pub contexts: Option<serde_json::Value>, // JSONB - full contexts object
    pub os_name: Option<String>,
    pub os_version: Option<String>,
    pub os_build: Option<String>,
    pub os_kernel_version: Option<String>,
    pub device_arch: Option<String>,
    pub device_processor_count: Option<i32>,
    pub device_processor_frequency: Option<i32>,
    pub device_memory_size: Option<i64>,
    pub device_free_memory: Option<i64>,
    pub device_boot_time: Option<UtcDateTime>,
    pub runtime_name: Option<String>,
    pub runtime_version: Option<String>,
    pub app_start_time: Option<UtcDateTime>,
    pub app_memory: Option<i64>,
    pub locale: Option<String>,
    pub timezone: Option<String>,

    // Relations
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub visitor_id: Option<i32>,
    pub ip_geolocation_id: Option<i32>,
}

/// Domain model for error groups
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorGroupDomain {
    pub id: i32,
    pub title: String,
    pub error_type: String,
    pub message_template: Option<String>,
    pub first_seen: UtcDateTime,
    pub last_seen: UtcDateTime,
    pub total_count: i32,
    pub status: String,
    pub assigned_to: Option<String>,
    /// Set when this group was merged into another one: its fingerprints and
    /// future events belong to that group now (ADR 111, D1.2).
    pub merged_into_group_id: Option<i32>,
    /// While the group is ignored with conditions: when it comes back
    /// (ADR 111, D1.3). `None` for any other group.
    pub ignore: Option<GroupIgnoreState>,
    pub resolution: Option<GroupResolutionState>,
    /// `error`, or `performance` for a group a trace detector filed.
    pub issue_type: String,
    /// The user or team the group is assigned to (ADR 111, D1.6).
    pub assignee: Option<super::group_workflow::GroupAssignee>,
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub visitor_id: Option<i32>,
    pub created_at: UtcDateTime,
    pub updated_at: UtcDateTime,
    /// Count of error events in the requested time window (None when no date range was requested)
    pub events_in_range: Option<i64>,
    /// Count of distinct affected visitors/users in the requested time window (None when no date range was requested)
    pub affected_users: Option<i64>,
    /// The deployment this group's `deployment_id` points to, resolved for the current page only.
    /// None when the group has no `deployment_id`, or (rarely) when the deployment row itself
    /// was deleted after the error group linked to it.
    pub deployment: Option<ErrorGroupDeploymentSummary>,
}

/// Minimal deployment info surfaced on an error group — just enough to show
/// "which commit was this error seen on" without pulling in the full
/// deployment response (build stats, screenshots, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorGroupDeploymentSummary {
    pub id: i32,
    pub commit_hash: Option<String>,
    pub commit_message: Option<String>,
    pub branch: Option<String>,
}

/// Domain model for error events
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorEventDomain {
    pub id: i64,
    pub error_group_id: i32,
    pub fingerprint_hash: String,
    pub timestamp: UtcDateTime,

    // Source of the error event
    pub source: Option<String>,

    // Core error data
    pub exception_type: String,
    pub exception_value: Option<String>,
    pub stack_trace: Option<serde_json::Value>,

    // Request context
    pub url: Option<String>,
    pub user_agent: Option<String>,
    pub referrer: Option<String>,
    pub method: Option<String>,
    pub headers: Option<serde_json::Value>,

    // User context
    pub user_id: Option<String>,
    pub user_email: Option<String>,
    pub user_username: Option<String>,
    pub user_ip_address: Option<String>,
    pub user_segment: Option<String>,
    pub session_id: Option<String>,
    pub user_context: Option<serde_json::Value>,

    // Device/Browser context
    pub browser: Option<String>,
    pub browser_version: Option<String>,
    pub operating_system: Option<String>,
    pub operating_system_version: Option<String>,
    pub device_type: Option<String>,
    pub screen_width: Option<i16>,
    pub screen_height: Option<i16>,
    pub viewport_width: Option<i16>,
    pub viewport_height: Option<i16>,

    // Request context
    pub request_context: Option<serde_json::Value>,

    // Additional context
    pub extra_context: Option<serde_json::Value>,

    // Release/Build information
    pub release_version: Option<String>,
    pub build_number: Option<String>,

    // Server information
    pub server_name: Option<String>,
    pub environment: Option<String>,

    // SDK information
    pub sdk_name: Option<String>,
    pub sdk_version: Option<String>,
    pub sdk_integrations: Option<serde_json::Value>,

    // Platform information
    pub platform: Option<String>,

    // Transaction information
    pub transaction_name: Option<String>,

    // Breadcrumbs
    pub breadcrumbs: Option<serde_json::Value>,

    // Request details (expanded)
    pub request_cookies: Option<serde_json::Value>,
    pub request_query_string: Option<serde_json::Value>,
    pub request_data: Option<serde_json::Value>,

    // Contexts (expanded device/OS info)
    pub contexts: Option<serde_json::Value>,
    pub os_name: Option<String>,
    pub os_version: Option<String>,
    pub os_build: Option<String>,
    pub os_kernel_version: Option<String>,
    pub device_arch: Option<String>,
    pub device_processor_count: Option<i32>,
    pub device_processor_frequency: Option<i32>,
    pub device_memory_size: Option<i64>,
    pub device_free_memory: Option<i64>,
    pub device_boot_time: Option<UtcDateTime>,
    pub runtime_name: Option<String>,
    pub runtime_version: Option<String>,
    pub app_start_time: Option<UtcDateTime>,
    pub app_memory: Option<i64>,
    pub locale: Option<String>,
    pub timezone: Option<String>,

    // Relations
    pub project_id: i32,
    pub environment_id: Option<i32>,
    pub deployment_id: Option<i32>,
    pub visitor_id: Option<i32>,

    // Geography
    pub ip_geolocation_id: Option<i32>,

    // Raw JSONB data (full transparency - contains complete error event)
    pub data: Option<serde_json::Value>,

    // Metadata
    pub created_at: UtcDateTime,
}

/// Statistics for error groups
#[derive(Debug, Serialize, Deserialize, FromQueryResult)]
pub struct ErrorGroupStats {
    pub total_groups: i64,
    pub unresolved_groups: i64,
    pub resolved_groups: i64,
    pub ignored_groups: i64,
}

/// Time series data point for error trends
#[derive(Debug, Serialize, Deserialize, FromQueryResult)]
pub struct ErrorTimeSeriesPoint {
    pub timestamp: UtcDateTime,
    pub count: i64,
}

#[cfg(test)]
mod without_nul_tests {
    use super::*;

    #[test]
    fn the_scanner_finds_an_escape_split_across_writes_and_nothing_else() {
        use std::io::Write;
        let mut scanner = NulEscapeScanner::default();
        scanner.write_all(b"xx\\\\u00").expect("no match yet");
        assert!(!scanner.found);
        assert!(scanner.write_all(b"00yy").is_err());
        assert!(scanner.found);

        let mut scanner = NulEscapeScanner::default();
        scanner.write_all(b"\\u0001 u0000").expect("no match");
        assert!(!scanner.found);
    }

    /// Which of `foo` and `foo\0` comes last depends on sort order. The value under
    /// the real name has to survive either way.
    #[test]
    fn a_stripped_key_never_replaces_the_field_with_that_name() {
        let mut value = serde_json::json!({
            "foo": "kept",
            "foo\u{0}": "from the nul key",
            "\u{0}bar": "first",
            "bar\u{0}": "second",
            "baz\u{0}": "only copy",
        });
        strip_nul(&mut value);
        assert_eq!(value["foo"], "kept");
        assert_eq!(value["baz"], "only copy");
        assert!(value.get("bar").is_some(), "{value}");
        assert_eq!(value.as_object().map(|fields| fields.len()), Some(3));
    }

    #[test]
    fn nul_is_removed_from_every_kind_of_field() {
        let event = CreateErrorEventData {
            exception_value: Some("nul \0 inside".to_string()),
            url: Some("https://app.example.test/\0".to_string()),
            raw_sentry_event: Some(serde_json::json!({
                "exception": {"values": [{"value": "nested \0 value"}]},
                "key\0": "kept",
            })),
            ..Default::default()
        }
        .without_nul();

        assert_eq!(event.exception_value.as_deref(), Some("nul  inside"));
        assert_eq!(event.url.as_deref(), Some("https://app.example.test/"));
        let raw = event.raw_sentry_event.unwrap();
        assert_eq!(
            raw["exception"]["values"][0]["value"].as_str(),
            Some("nested  value")
        );
        assert_eq!(raw["key"].as_str(), Some("kept"));
    }

    #[test]
    fn an_event_with_no_nul_is_unchanged() {
        // The text of an escape is not a NUL, and stays.
        let event = CreateErrorEventData {
            exception_value: Some("literal \\u0000 text".to_string()),
            screen_width: Some(1920),
            ..Default::default()
        }
        .without_nul();

        assert_eq!(
            event.exception_value.as_deref(),
            Some("literal \\u0000 text")
        );
        assert_eq!(event.screen_width, Some(1920));
    }
}

/// Refinements of the error group list beyond its original filters
/// (ADR 111, D1.6 and D1.8).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupListRefinement {
    /// Only groups with this assignee.
    pub assignee: Option<super::group_workflow::AssigneeFilter>,
    /// Case-insensitive substring of the title, error type or message.
    pub search: Option<String>,
    /// `error` or `performance` (ADR 111, D2); `None` for both.
    pub issue_type: Option<String>,
}

/// A group resolved in the next release (ADR 111, D1.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct GroupResolutionState {
    /// Always true; the field exists so clients can test one value.
    pub in_next_release: bool,
    /// Id of the release the group was resolved after; events from a newer
    /// release reopen it.
    pub after_release_id: Option<i32>,
    /// That release's version, filled in on the single-group read.
    pub after_release: Option<String>,
}

impl GroupResolutionState {
    /// The state of a group resolved in the next release; `None` otherwise.
    pub fn from_group(group: &temps_entities::error_groups::Model) -> Option<Self> {
        (group.status == "resolved" && group.resolved_in_next_release).then_some(Self {
            in_next_release: true,
            after_release_id: group.resolved_in_release_after_id,
            after_release: None,
        })
    }
}

/// The ignore conditions of an ignored group and how far along they are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct GroupIgnoreState {
    /// Stays ignored until this time.
    #[schema(value_type = Option<String>, format = DateTime)]
    pub until: Option<chrono::DateTime<chrono::Utc>>,
    /// Reopens when `total_count` reaches this value.
    pub until_event_count: Option<i32>,
    /// Reopens when this many new distinct users are affected.
    pub until_user_count: Option<i32>,
    /// New distinct users seen since it was ignored.
    pub user_count: i32,
    /// Stays ignored whatever happens.
    pub forever: bool,
}

impl GroupIgnoreState {
    /// The state of an ignored group with conditions; `None` otherwise.
    pub fn from_group(group: &temps_entities::error_groups::Model) -> Option<Self> {
        let has_conditions = group.ignore_forever
            || group.ignore_until.is_some()
            || group.ignore_until_event_count.is_some()
            || group.ignore_until_user_count.is_some();
        (group.status == "ignored" && has_conditions).then_some(Self {
            until: group.ignore_until,
            until_event_count: group.ignore_until_event_count,
            until_user_count: group.ignore_until_user_count,
            user_count: group.ignore_user_count,
            forever: group.ignore_forever,
        })
    }
}
