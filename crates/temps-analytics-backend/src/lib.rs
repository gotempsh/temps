//! Analytics backend abstraction.
//!
//! Defines the [`AnalyticsBackend`] trait that the analytics handlers depend on,
//! plus the shared DTOs used across implementations. Two implementations:
//!
//! - [`timescale::TimescaleBackend`] — PostgreSQL + TimescaleDB. The default
//!   when `TEMPS_CLICKHOUSE_*` is unset.
//! - [`clickhouse::ClickHouseBackend`] — derived columnar replica used when
//!   the operator points Temps at a ClickHouse cluster via env vars.
//!
//! Both backends are always linked into every binary; the choice is made at
//! runtime from `ServerConfig::is_clickhouse_enabled()`. Operators do NOT
//! need to rebuild Temps with a custom feature flag to enable ClickHouse —
//! flipping the env vars is enough.

pub mod clickhouse;
pub mod error;
pub mod migrations;
pub mod timescale;
pub mod traits;
pub mod types;

pub use error::AnalyticsBackendError;
pub use traits::AnalyticsBackend;
