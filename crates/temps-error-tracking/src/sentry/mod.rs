//! # Sentry Integration Module
//!
//! Complete Sentry SDK compatibility including:
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
//! Event Mapper (mapper.rs)
//!     ↓
//! Ingestion Service (service.rs)
//!     ↓
//! Error Ingestion Service
//!     ↓
//! Database (raw event in JSONB)
//! ```
//!
//! ## DSN Flow
//!
//! ```text
//! Create DSN (dsn_handlers.rs)
//!     ↓
//! Store in DB (dsn_service.rs)
//!     ↓
//! Validate on ingestion (service.rs)
//! ```

pub mod dsn_handlers;
pub mod dsn_service;
pub mod envelope;
pub mod handlers;
pub mod mapper;
pub mod service;
pub mod types;

// Re-exports for convenience
pub use dsn_service::DSNService;
pub use envelope::{Envelope, EnvelopeError, EnvelopeItem};
pub use service::SentryIngestionService;
pub use types::{
    CreateDSNRequest, DSNResponse, ParsedDSN, ProjectDSN, SentryEventRequest, SentryEventResponse,
    SentryIngesterError,
};
