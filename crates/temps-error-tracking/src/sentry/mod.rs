// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! # Sentry Integration Module
//!
//! Sentry SDK ingestion adapters for native Temps errors and structured logs.
//! Other item types are reported unsupported rather than counted as stored.
//!
//! Shared infrastructure includes:
//! - Envelope parsing via relay-event-schema
//! - Event mapping to internal format
//! - HTTP ingestion endpoints
//! - DSN management
//!
//! ## Architecture
//!
//! ```text
//! Sentry SDK
//!     ↓
//! HTTP Handler (handlers.rs)
//!     ↓
//! Envelope Parser (envelope.rs)
//!     ↓
//! Provider dispatch (providers/sentry.rs)
//!     ↓
//! ErrorTrackingService / OtelService
//!     ↓
//! Native error / log storage and queries
//! ```
//!
//! ## DSN Flow
//!
//! ```text
//! Create DSN (dsn_handlers.rs)
//!     ↓
//! Store in DB (dsn_service.rs)
//!     ↓
//! Validate on ingestion (providers/sentry.rs)
//! ```

pub mod dsn_handlers;
pub mod dsn_service;
pub mod envelope;
pub mod handlers;
pub mod logs;
pub mod mapper;
pub mod rate_limiter;
pub mod service;
pub mod types;

// Re-exports for convenience
pub use dsn_service::DSNService;
pub use envelope::{Envelope, EnvelopeError, EnvelopeItem};
pub use handlers::SENTRY_TUNNEL_ROUTE_PATH;
pub use service::SentryIngestionService;
pub use types::{
    CreateDSNRequest, DSNResponse, ParsedDSN, ProjectDSN, SentryEventRequest, SentryEventResponse,
    SentryIngesterError,
};

pub mod check_ins;

pub mod metrics;
pub mod replay;
pub mod traces;

pub(crate) mod inference;

pub mod feedback;

pub mod statsd;

pub mod client_reports;

pub mod security;

pub(crate) mod identities;
