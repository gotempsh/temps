// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared ClickHouse infrastructure: config, client construction, and a
//! migration runner.
//!
//! # Why this crate exists
//!
//! Four crates each vendor their own copy of the same three things —
//! `TEMPS_CLICKHOUSE_*` env parsing, a `clickhouse::Client` builder, and a
//! comment-stripping multi-statement migration runner with a
//! `ReplacingMergeTree` tracking table:
//!
//! - `temps-otel` (`crates/temps-otel/src/storage/clickhouse/`)
//! - `temps-analytics-backend`
//! - `temps-proxy`
//! - `temps-metrics`
//!
//! `temps-clickhouse` is the canonical implementation those four are meant to
//! migrate onto (one crate at a time, in separate follow-up work) instead of
//! re-fixing the same bugs four times. Until that migration happens, treat
//! this crate as the source of truth for *new* ClickHouse call sites.
//!
//! # Migration lists are append-only
//!
//! Callers build a `&[Migration]` list and pass it to [`apply_migrations`]
//! alongside a `tracking_table` name. Each migration is recorded by `name` in
//! the tracking table once applied, keyed only by that name — so:
//!
//! - **Never remove or reorder** an already-shipped migration's entry; a
//!   fresh install must apply every migration a self-hosted operator's
//!   existing install already has recorded, in the same order.
//! - **Never reuse a `name`** for different SQL; the runner treats a
//!   recorded name as "this exact migration already ran" and skips it.
//! - Only append new entries to the end of the list.

pub mod migrations;

use clickhouse::error::Error as ChError;

// ── Configuration ───────────────────────────────────────────────────────────

/// Connection configuration for a ClickHouse-backed store.
///
/// All four fields are required for a store to be considered "configured".
/// The instance's connection lives once in `temps_config::ServerConfig`
/// (`is_clickhouse_enabled` is the fail-closed rule: partial configuration
/// is "off"); the composition root builds this from it and hands it to each
/// ClickHouse-backed store. Stores never read the environment themselves.
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
    /// Build a config directly from field values (mainly for tests/callers
    /// that already resolved these from something other than the process
    /// environment, e.g. a `ServerConfig`).
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

    /// Build a `clickhouse::Client` scoped to this config's URL, database,
    /// and credentials.
    pub fn client(&self) -> clickhouse::Client {
        clickhouse::Client::default()
            .with_url(&self.url)
            .with_database(&self.database)
            .with_user(&self.user)
            .with_password(&self.password)
    }
}

// ── Errors ───────────────────────────────────────────────────────────────────

/// Errors surfaced by this crate's ClickHouse helpers.
#[derive(thiserror::Error, Debug)]
pub enum ClickHouseError {
    #[error("ClickHouse query failed: {0}")]
    Query(#[from] ChError),

    #[error(
        "ClickHouse database name '{0}' is invalid; only [A-Za-z0-9_] are permitted and it must be non-empty"
    )]
    InvalidDatabaseName(String),

    #[error("ClickHouse migration '{name}' failed: {source}")]
    Migration { name: String, source: ChError },

    #[error("failed to parse ClickHouse server version from '{0}'")]
    VersionParse(String),
}

impl ClickHouseError {
    /// Unwrap the inner `clickhouse::error::Error` out of a `Query` variant,
    /// falling back to a synthetic `Other` error for variants that never
    /// occur on the call sites that use this helper (`execute_multi` only
    /// ever produces `Query`). Used by the migration runner to attach a
    /// migration `name` to the underlying transport/DDL error without
    /// double-wrapping.
    pub(crate) fn into_query_error(self) -> ChError {
        match self {
            ClickHouseError::Query(source) => source,
            other => ChError::Other(other.to_string().into()),
        }
    }
}

// ── Migrations ───────────────────────────────────────────────────────────────

pub use migrations::{apply_migrations, Migration, MigrationReport};

// ── Server version ──────────────────────────────────────────────────────────

/// A parsed ClickHouse `major.minor.patch[.build]` server version.
///
/// Only `major`/`minor`/`patch` participate in ordering; any trailing build
/// number or suffix (e.g. `-alpine`) is accepted while parsing but not
/// tracked, since none of our version-gated behavior needs finer granularity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ServerVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl ServerVersion {
    /// Parse a ClickHouse `version()` string such as `"25.3.1.2703"` or
    /// `"26.7.5.10-alpine"`. Requires at least `major.minor.patch`; any
    /// further `.build` segment or `-suffix` is ignored.
    pub fn parse(s: &str) -> Result<Self, ClickHouseError> {
        // Strip a trailing "-suffix" (e.g. "-alpine") before splitting on '.'.
        let core = s.split('-').next().unwrap_or(s);
        let mut parts = core.split('.');

        let next_u32 = |parts: &mut std::str::Split<'_, char>| -> Option<u32> {
            parts.next().and_then(|p| p.parse::<u32>().ok())
        };

        let major =
            next_u32(&mut parts).ok_or_else(|| ClickHouseError::VersionParse(s.to_string()))?;
        let minor =
            next_u32(&mut parts).ok_or_else(|| ClickHouseError::VersionParse(s.to_string()))?;
        let patch =
            next_u32(&mut parts).ok_or_else(|| ClickHouseError::VersionParse(s.to_string()))?;

        Ok(Self {
            major,
            minor,
            patch,
        })
    }

    /// True when this version is at least `major.minor` (patch-agnostic).
    pub fn at_least(self, major: u32, minor: u32) -> bool {
        (self.major, self.minor) >= (major, minor)
    }
}

impl std::fmt::Display for ServerVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Query the connected ClickHouse server's version via `SELECT version()`.
pub async fn server_version(client: &clickhouse::Client) -> Result<ServerVersion, ClickHouseError> {
    use clickhouse::Row;
    use serde::Deserialize;

    #[derive(Row, Deserialize)]
    struct VersionRow {
        version: String,
    }

    let row: VersionRow = client
        .query("SELECT version() AS version")
        .fetch_one()
        .await
        .map_err(ClickHouseError::Query)?;

    ServerVersion::parse(&row.version)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ServerVersion ────────────────────────────────────────────────────

    #[test]
    fn parses_standard_version() {
        let v = ServerVersion::parse("25.3.1.2703").unwrap();
        assert_eq!(
            v,
            ServerVersion {
                major: 25,
                minor: 3,
                patch: 1
            }
        );
    }

    #[test]
    fn parses_version_with_suffix() {
        let v = ServerVersion::parse("26.7.5.10-alpine").unwrap();
        assert_eq!(
            v,
            ServerVersion {
                major: 26,
                minor: 7,
                patch: 5
            }
        );
    }

    #[test]
    fn parses_version_without_build_segment() {
        let v = ServerVersion::parse("24.1.2").unwrap();
        assert_eq!(
            v,
            ServerVersion {
                major: 24,
                minor: 1,
                patch: 2
            }
        );
    }

    #[test]
    fn rejects_garbage_version_string() {
        assert!(ServerVersion::parse("not-a-version").is_err());
        assert!(ServerVersion::parse("").is_err());
        assert!(ServerVersion::parse("25").is_err());
        assert!(ServerVersion::parse("25.3").is_err());
    }

    #[test]
    fn display_round_trips_major_minor_patch() {
        let v = ServerVersion::parse("26.7.5.10-alpine").unwrap();
        assert_eq!(v.to_string(), "26.7.5");
    }

    #[test]
    fn at_least_compares_major_minor_only() {
        let v = ServerVersion {
            major: 25,
            minor: 3,
            patch: 1,
        };
        assert!(v.at_least(25, 3));
        assert!(v.at_least(25, 2));
        assert!(v.at_least(24, 99));
        assert!(!v.at_least(25, 4));
        assert!(!v.at_least(26, 0));
    }

    #[test]
    fn ord_compares_full_triple() {
        let a = ServerVersion {
            major: 25,
            minor: 3,
            patch: 1,
        };
        let b = ServerVersion {
            major: 25,
            minor: 3,
            patch: 2,
        };
        assert!(a < b);
    }

    #[test]
    fn debug_masks_password() {
        let cfg = ClickHouseConfig::new("http://localhost:8123", "temps", "default", "hunter2");
        let debug = format!("{cfg:?}");
        assert!(!debug.contains("hunter2"));
        assert!(debug.contains("***"));
    }
}
