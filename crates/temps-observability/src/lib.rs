//! Unified observability surface for the project-level Observe page.
//!
//! Exposes one discriminated `ObservabilityEvent` row that covers request
//! logs, OTel spans, error events, and revenue events. Runtime logs are
//! intentionally **not** included — they live on a dedicated Logs page
//! because their volume would dominate the merged timeline and their
//! storage path (TimescaleDB hypertable + chunked file/S3 store) doesn't
//! compose with the per-kind LIMIT merge strategy.
//!
//! Everything the side panel needs to render is included on the row, so
//! the list view never needs a follow-up fetch in the common case.

pub mod cursor;
pub mod error;
pub mod filters;
pub mod handlers;
pub mod plugin;
pub mod service;
pub mod types;

pub use handlers::{configure_observability_routes, ObservabilityApiDoc, ObservabilityState};
pub use plugin::ObservabilityPlugin;
pub use service::ObservabilityService;

pub use cursor::Cursor;
pub use error::ObservabilityError;
pub use filters::{
    clamp_limit, merge_desc_by_ts, parse_kinds, EventFilters, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE,
};
pub use types::{
    project_headers, truncate_attributes, truncate_stacktrace, ErrorRow, EventKind,
    ObservabilityEvent, RequestRow, RevenueRow, SpanRow, HEADER_WHITELIST,
    SPAN_ATTRIBUTE_PREVIEW_KEYS, STACKTRACE_PREVIEW_FRAMES,
};
