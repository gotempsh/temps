//! ClickHouse-backed [`AnalyticsBackend`] implementation.
//!
//! Compiled only when the `clickhouse` feature is enabled. The query surface
//! that handlers depend on (`AnalyticsEvents` in `temps-analytics-events`)
//! is *not* implemented here yet — it requires translating ~2,000 lines of
//! Timescale SQL into ClickHouse dialect, which is being done query-by-query
//! in follow-up commits with a parity test harness against
//! `TimescaleBackend`.
//!
//! This module currently provides:
//! - A real connection pool via the official `clickhouse` Rust client.
//! - A working health check (`SELECT 1`).
//! - A configuration struct usable by the plugin layer.

use async_trait::async_trait;

use crate::error::AnalyticsBackendError;
use crate::traits::AnalyticsBackend;

/// Connection configuration for the ClickHouse backend.
///
/// Built from `temps-config` keys `analytics.clickhouse.{url,database,user,password}`.
#[derive(Clone)]
pub struct ClickHouseConfig {
    pub url: String,
    pub database: String,
    pub user: String,
    pub password: String,
}

// Manual Debug that masks the password so it can never leak into logs, panic
// messages, or tracing spans that capture the config with `{:?}`.
impl std::fmt::Debug for ClickHouseConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClickHouseConfig")
            .field("url", &self.url)
            .field("database", &self.database)
            .field("user", &self.user)
            .field("password", &"***")
            .finish()
    }
}

impl ClickHouseConfig {
    pub fn new(
        url: impl Into<String>,
        database: impl Into<String>,
        user: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            url: url.into(),
            database: database.into(),
            user: user.into(),
            password: password.into(),
        }
    }
}

pub struct ClickHouseBackend {
    client: ::clickhouse::Client,
}

impl ClickHouseBackend {
    /// Build a backend pointing at the given ClickHouse server.
    ///
    /// Does not validate connectivity — call [`AnalyticsBackend::health_check`]
    /// or one of the migration entry points to do that.
    pub fn new(config: ClickHouseConfig) -> Self {
        let client = ::clickhouse::Client::default()
            .with_url(config.url)
            .with_database(config.database)
            .with_user(config.user)
            .with_password(config.password);
        Self { client }
    }

    /// Borrow the underlying client. `pub(crate)` so query implementations
    /// added in subsequent commits can use it; handlers must go through the
    /// trait instead.
    #[allow(dead_code)]
    pub(crate) fn client(&self) -> &::clickhouse::Client {
        &self.client
    }

    /// Clone the underlying client out of the backend.
    ///
    /// `clickhouse::Client` is cheap to clone (it's an `Arc` of an HTTP
    /// connector internally). Plugin wiring uses this to share one client
    /// between the read-side `ClickHouseEventsBackend` and the fan-out
    /// worker so connections aren't doubled up.
    pub fn client_clone(&self) -> ::clickhouse::Client {
        self.client.clone()
    }
}

#[async_trait]
impl AnalyticsBackend for ClickHouseBackend {
    fn name(&self) -> &'static str {
        "clickhouse"
    }

    async fn health_check(&self) -> Result<(), AnalyticsBackendError> {
        // `clickhouse::Client::query` returns a builder; `.execute()` runs
        // statements that return no rows. A SELECT 1 with `.fetch_one::<u8>()`
        // verifies both connectivity and auth.
        self.client
            .query("SELECT 1")
            .fetch_one::<u8>()
            .await
            .map_err(|e| AnalyticsBackendError::BackendUnavailable {
                backend: "clickhouse".to_string(),
                reason: e.to_string(),
            })?;
        Ok(())
    }
}
