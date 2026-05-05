//! Analytics backend abstraction.
//!
//! Defines the [`AnalyticsBackend`] trait that the analytics handlers depend on,
//! plus the shared DTOs used across implementations. Two implementations are
//! planned:
//!
//! - [`timescale::TimescaleBackend`] — PostgreSQL + TimescaleDB. Default.
//! - [`clickhouse::ClickHouseBackend`] — feature-gated, derived analytical replica
//!   used in the hybrid model where Postgres remains the system of record.
//!
//! Phase 1 only relocates the existing Timescale logic behind this trait so a
//! second backend can later be added without touching the handler layer.

pub mod error;
pub mod traits;
pub mod types;

pub mod timescale;

#[cfg(feature = "clickhouse")]
pub mod clickhouse;

pub use error::AnalyticsBackendError;
pub use traits::AnalyticsBackend;
