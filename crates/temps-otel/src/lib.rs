//! OpenTelemetry data collection, storage, and analysis for Temps.
//!
//! This crate implements:
//! - OTLP/HTTP ingest endpoints (metrics, traces, logs)
//! - Pluggable storage backend (default: TimescaleDB)
//! - Tail-based trace sampling
//! - Anomaly detection with time-aware baselines
//! - Pre-computed health summaries for the monitoring page
//! - OTel collector sidecar injection for deployed containers
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────┐    ┌──────────┐    ┌─────────────────┐
//! │ OTel SDK /   │───▶│ Ingest   │───▶│ OtelService     │
//! │ Collector    │    │ Handlers │    │ (orchestrates)  │
//! └─────────────┘    └──────────┘    └────────┬────────┘
//!                                             │
//!                    ┌────────────────────────┤
//!                    │                        │
//!              ┌─────▼──────┐         ┌──────▼────────┐
//!              │ Storage    │         │ Sampler       │
//!              │ Trait      │         │ (tail-based)  │
//!              └─────┬──────┘         └───────────────┘
//!                    │
//!              ┌─────▼──────┐
//!              │ TimescaleDB│  (or future: ClickHouse, etc.)
//!              └────────────┘
//! ```

pub mod anomaly;
pub mod error;
pub mod handlers;
pub mod ingest;
pub mod plugin;
pub mod proto;
pub mod services;
pub mod sidecar;
pub mod storage;
pub mod types;

pub use error::OtelError;
pub use plugin::OtelPlugin;
pub use services::OtelService;
pub use storage::OtelStorage;

#[cfg(test)]
pub mod test_support;

/// Application state shared across OTel HTTP handlers.
#[derive(Clone)]
pub struct OtelAppState {
    pub otel_service: std::sync::Arc<OtelService>,
}
