//! ClickHouse-backed [`AnalyticsBackend`] implementation.
//!
//! Stub. Implementation lands in Phase 2 along with the columnar schema and
//! the outbox fan-out worker described in the hybrid plan. Compiled only when
//! the `clickhouse` feature is enabled.

use async_trait::async_trait;

use crate::error::AnalyticsBackendError;
use crate::traits::AnalyticsBackend;

pub struct ClickHouseBackend {
    _placeholder: (),
}

impl ClickHouseBackend {
    pub fn new() -> Self {
        Self { _placeholder: () }
    }
}

impl Default for ClickHouseBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AnalyticsBackend for ClickHouseBackend {
    fn name(&self) -> &'static str {
        "clickhouse"
    }

    async fn health_check(&self) -> Result<(), AnalyticsBackendError> {
        Err(AnalyticsBackendError::BackendUnavailable {
            backend: "clickhouse".to_string(),
            reason: "ClickHouse backend not yet implemented (Phase 2)".to_string(),
        })
    }
}
