// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Startup preflight and background re-check of the ClickHouse connection.
//!
//! The probe is one `SELECT version()` — the cheapest query that returns a row
//! and so exercises the response decoder, which `SELECT 1` against a server
//! that errors out does not. Its outcome is kept in a [`ClickHouseHealth`] the
//! settings API reads, so an operator sees *why* ClickHouse-backed features are
//! empty instead of finding `Code: 60` on a dashboard later.
//!
//! Nothing here switches compression mode: a failing probe is reported, with
//! both fixes named, and the operator chooses.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use temps_core::ClickHouseCompression;
use tracing::{debug, error, info, warn};

use crate::diagnostics::{classify_error, CompressionMethod, FailureKind, COMPRESSION_REMEDY};
use crate::{preflight, ClickHouseConfig, ClickHouseError, ServerVersion};

/// How often a failing connection is re-probed.
pub const FAILING_RECHECK_INTERVAL: Duration = Duration::from_secs(60);
/// How often a healthy connection is re-probed. A healthy connection is
/// already exercised by real traffic; this only notices a server upgrade.
pub const HEALTHY_RECHECK_INTERVAL: Duration = Duration::from_secs(300);

/// What the last probe found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    /// No probe has finished yet.
    NotChecked,
    /// The server answered and its response decoded.
    Reachable { server_version: String },
    /// The probe failed. `message` is the full, operator-facing error.
    Failing {
        kind: FailureKind,
        message: String,
        /// The compression method byte seen on the wire, for decode failures.
        observed_method: Option<CompressionMethod>,
    },
}

/// A point-in-time copy of a [`ClickHouseHealth`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthSnapshot {
    /// `host[:port]` of the configured server; never carries credentials.
    pub host: String,
    pub database: String,
    /// The compression mode the probing (and every other) client uses.
    pub mode: ClickHouseCompression,
    pub state: ConnectionState,
    pub last_checked_at: Option<DateTime<Utc>>,
}

#[derive(Debug)]
struct Inner {
    state: ConnectionState,
    last_checked_at: Option<DateTime<Utc>>,
}

/// Shared, cheaply readable record of the ClickHouse connection's health.
///
/// Written by one background task a minute at most and read by the settings
/// handler, so a plain mutex is the right tool; nothing on a request path
/// touches it.
#[derive(Debug)]
pub struct ClickHouseHealth {
    host: String,
    database: String,
    mode: ClickHouseCompression,
    inner: Mutex<Inner>,
}

impl ClickHouseHealth {
    /// A record for `config` with no probe result yet.
    pub fn new(config: &ClickHouseConfig) -> Arc<Self> {
        Arc::new(Self {
            host: config.host_for_logs(),
            database: config.database.clone(),
            mode: config.compression,
            inner: Mutex::new(Inner {
                state: ConnectionState::NotChecked,
                last_checked_at: None,
            }),
        })
    }

    /// Copy out the current state.
    pub fn snapshot(&self) -> HealthSnapshot {
        let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        HealthSnapshot {
            host: self.host.clone(),
            database: self.database.clone(),
            mode: self.mode,
            state: inner.state.clone(),
            last_checked_at: inner.last_checked_at,
        }
    }

    /// Record a probe outcome, logging only when the picture changed: one
    /// actionable ERROR when decoding starts failing, an INFO when it
    /// recovers, and nothing for an unchanged state.
    pub fn record(&self, outcome: Result<ServerVersion, ClickHouseError>) {
        let next = match &outcome {
            Ok(version) => ConnectionState::Reachable {
                server_version: version.to_string(),
            },
            Err(error) => ConnectionState::Failing {
                kind: failure_kind(error),
                message: error.to_string(),
                observed_method: match error {
                    ClickHouseError::ResponseDecode { observed, .. } => *observed,
                    _ => None,
                },
            },
        };

        let previous = {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            inner.last_checked_at = Some(Utc::now());
            std::mem::replace(&mut inner.state, next.clone())
        };

        match (&previous, &next) {
            (before, after) if before == after => {
                debug!(host = %self.host, database = %self.database, "ClickHouse probe unchanged");
            }
            (_, ConnectionState::Reachable { server_version }) => match previous {
                ConnectionState::Failing { .. } => info!(
                    host = %self.host,
                    database = %self.database,
                    mode = %self.mode,
                    server_version = %server_version,
                    "ClickHouse connection recovered: responses decode again"
                ),
                _ => info!(
                    host = %self.host,
                    database = %self.database,
                    mode = %self.mode,
                    server_version = %server_version,
                    "ClickHouse preflight passed"
                ),
            },
            (
                _,
                ConnectionState::Failing {
                    kind: FailureKind::Decode,
                    observed_method,
                    message,
                },
            ) => error!(
                host = %self.host,
                database = %self.database,
                mode = %self.mode,
                observed_method = observed_method
                    .map_or_else(|| "unknown".to_string(), |m| m.to_string()),
                failure_kind = FailureKind::Decode.as_str(),
                remedy = COMPRESSION_REMEDY,
                "ClickHouse preflight failed: the server's responses cannot be decoded in the \
                 configured compression mode, so no ClickHouse-backed feature can work. {message}"
            ),
            (_, ConnectionState::Failing { kind, message, .. }) => warn!(
                host = %self.host,
                database = %self.database,
                mode = %self.mode,
                failure_kind = kind.as_str(),
                "ClickHouse preflight failed: {message}"
            ),
            (_, ConnectionState::NotChecked) => {}
        }
    }

    /// Probe once and record the outcome.
    pub async fn check_once(&self, config: &ClickHouseConfig) {
        self.record(preflight(config).await);
    }

    /// Probe now, then keep re-probing in the background so recovery after the
    /// operator fixes the server shows up without a restart.
    ///
    /// Needs a Tokio runtime; without one the record stays `NotChecked` and a
    /// warning says so.
    pub fn spawn_monitor(config: ClickHouseConfig) -> Arc<Self> {
        let health = Self::new(&config);
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                let task_health = health.clone();
                handle.spawn(async move {
                    loop {
                        task_health.check_once(&config).await;
                        let interval = match task_health.snapshot().state {
                            ConnectionState::Failing { .. } | ConnectionState::NotChecked => {
                                FAILING_RECHECK_INTERVAL
                            }
                            ConnectionState::Reachable { .. } => HEALTHY_RECHECK_INTERVAL,
                        };
                        tokio::time::sleep(interval).await;
                    }
                });
            }
            Err(_) => warn!(
                host = %health.host,
                database = %health.database,
                "No tokio runtime available; the ClickHouse preflight probe will not run"
            ),
        }
        health
    }
}

fn failure_kind(error: &ClickHouseError) -> FailureKind {
    match error {
        ClickHouseError::ResponseDecode { .. } => FailureKind::Decode,
        ClickHouseError::ProbeTimeout { .. } => FailureKind::Unreachable,
        ClickHouseError::Query(source) => classify_error(source),
        ClickHouseError::Migration { source, .. } => classify_error(source),
        ClickHouseError::InvalidDatabaseName(_) | ClickHouseError::VersionParse(_) => {
            FailureKind::Other
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ClickHouseConfig {
        ClickHouseConfig::new(
            "http://u:hunter2@clickhouse.internal:8123",
            "temps",
            "u",
            "p",
        )
    }

    fn decode_error() -> ClickHouseError {
        ClickHouseError::ResponseDecode {
            host: "clickhouse.internal:8123".into(),
            database: "temps".into(),
            mode: ClickHouseCompression::Lz4,
            observed: Some(CompressionMethod::Zstd),
            source: clickhouse::error::Error::Decompression("incorrect magic number".into()),
        }
    }

    #[test]
    fn new_health_has_not_been_checked_and_hides_credentials() {
        let health = ClickHouseHealth::new(&config());
        let snapshot = health.snapshot();
        assert_eq!(snapshot.state, ConnectionState::NotChecked);
        assert_eq!(snapshot.last_checked_at, None);
        assert_eq!(snapshot.host, "clickhouse.internal:8123");
        assert!(!format!("{snapshot:?}").contains("hunter2"));
    }

    #[test]
    fn a_decode_failure_is_recorded_with_the_observed_method() {
        let health = ClickHouseHealth::new(&config());
        health.record(Err(decode_error()));

        let snapshot = health.snapshot();
        assert!(snapshot.last_checked_at.is_some());
        match snapshot.state {
            ConnectionState::Failing {
                kind,
                observed_method,
                message,
            } => {
                assert_eq!(kind, FailureKind::Decode);
                assert_eq!(observed_method, Some(CompressionMethod::Zstd));
                assert!(message.contains("network_compression_method"), "{message}");
            }
            other => panic!("expected a failing state, got {other:?}"),
        }
    }

    #[test]
    fn recovery_replaces_the_failure() {
        let health = ClickHouseHealth::new(&config());
        health.record(Err(decode_error()));
        health.record(Ok(ServerVersion {
            major: 26,
            minor: 9,
            patch: 1,
        }));

        assert_eq!(
            health.snapshot().state,
            ConnectionState::Reachable {
                server_version: "26.9.1".into()
            }
        );
    }

    #[test]
    fn transport_failures_are_not_reported_as_decode_failures() {
        let health = ClickHouseHealth::new(&config());
        health.record(Err(ClickHouseError::ProbeTimeout {
            host: "clickhouse.internal:8123".into(),
            database: "temps".into(),
            timeout_secs: 10,
        }));
        match health.snapshot().state {
            ConnectionState::Failing {
                kind,
                observed_method,
                ..
            } => {
                assert_eq!(kind, FailureKind::Unreachable);
                assert_eq!(observed_method, None);
            }
            other => panic!("expected a failing state, got {other:?}"),
        }
    }
}
