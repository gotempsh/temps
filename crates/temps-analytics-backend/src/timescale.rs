//! TimescaleDB-backed [`AnalyticsBackend`] implementation.
//!
//! Phase 1 ships only the skeleton: a thin wrapper around the existing
//! `DatabaseConnection`. Subsequent commits relocate query methods out of
//! `temps-analytics-events::services::events_service` into this module without
//! changing their SQL.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};

use crate::error::AnalyticsBackendError;
use crate::traits::AnalyticsBackend;

pub struct TimescaleBackend {
    db: Arc<DatabaseConnection>,
}

impl TimescaleBackend {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    /// Borrow the underlying connection. Kept `pub(crate)` so query methods
    /// added in subsequent commits can use it; handlers must go through the
    /// trait instead of touching this directly.
    #[allow(dead_code)] // used by query methods migrating in Phase 1 task #3
    pub(crate) fn db(&self) -> &DatabaseConnection {
        self.db.as_ref()
    }
}

#[async_trait]
impl AnalyticsBackend for TimescaleBackend {
    fn name(&self) -> &'static str {
        "timescale"
    }

    async fn health_check(&self) -> Result<(), AnalyticsBackendError> {
        self.db
            .execute(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT 1".to_string(),
            ))
            .await
            .map_err(|e| AnalyticsBackendError::BackendUnavailable {
                backend: "timescale".to_string(),
                reason: e.to_string(),
            })?;
        Ok(())
    }
}
