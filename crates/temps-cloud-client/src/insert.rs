// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A `clickhouse::Client` pointed at Temps Cloud's telemetry **insert**
//! surface (ADR-043 §5a) — the write-side sibling of
//! [`crate::query::clickhouse_query_client`].
//!
//! # What this is
//!
//! ADR-040 §4 settled the read side: Cloud exposes `POST /v1/telemetry/query`,
//! a byte-level passthrough of the ClickHouse HTTP interface that a stock
//! `clickhouse::Client` can be pointed straight at. ADR-043 §5a specifies the
//! identical shape for writes: an `INSERT INTO <table> (<columns>) FORMAT
//! <row-format>` statement, in exactly the shape
//! `clickhouse::Client::insert::<T>(table)` already produces, scoped to the
//! caller's tenant and forwarded.
//!
//! # The exact URL is Cloud's choice, not settled here
//!
//! ADR-043 §5a is explicit that where Cloud serves the insert surface is a
//! private-repo decision, with one hard constraint: it must not be the read
//! proxy's handler, whose entire safety property is unconditionally injecting
//! `readonly=1`. [`TELEMETRY_INSERT_PATH`] is this side's working assumption
//! — `POST /v1/telemetry/insert`, chosen for the obvious symmetry with
//! `/v1/telemetry/query` — and **must be confirmed against the private
//! `temps-cloud-app` implementation** before this transport is used against a
//! real Cloud tenant. Nothing on the OSS side can verify this path is correct;
//! see the crate-level docs' note on the schema-coupling risk this shares with
//! the read side.
//!
//! # Auth is unchanged
//!
//! The same enrollment-derived instance token `POST /v1/telemetry` and
//! `POST /v1/telemetry/query` already use, carried the same way (HTTP Basic,
//! token in the password field) — see [`crate::query::ClickHouseQueryTarget`]
//! for why Basic and not `clickhouse`'s `X-ClickHouse-*` headers.
//!
//! # What this is deliberately not
//!
//! Not a batching, retry or dead-letter mechanism — that is
//! [`crate::outbox::TelemetryOutbox`] and its drain worker. This module only
//! turns a linked instance's credential into a ready-to-use insert-capable
//! `clickhouse::Client`.

use base64::Engine as _;

use crate::query::QUERY_PROXY_USER;
use crate::{CloudError, CloudLink};

/// Path of Temps Cloud's insert surface, appended to the same backend origin
/// every other Cloud call already uses.
///
/// **Working assumption, not a confirmed contract** — see the module docs.
pub const TELEMETRY_INSERT_PATH: &str = "/v1/telemetry/insert";

/// Same database Cloud's telemetry tables live in for reads
/// ([`crate::query::CLOUD_TELEMETRY_DATABASE`]) — inserts target the same
/// tenant-scoped database, just through a different surface.
pub use crate::query::CLOUD_TELEMETRY_DATABASE;

impl CloudLink {
    /// A `clickhouse::Client` that inserts rows into this tenant's Cloud
    /// telemetry tables (ADR-043 §5a).
    ///
    /// Same refusal shape as [`CloudLink::clickhouse_query_client`] — "not
    /// enrolled", "the state file is unreadable", "outbound Cloud calls are
    /// blocked" and "telemetry is switched off" each need a different fix, so
    /// this returns an error rather than `None`.
    ///
    /// Validation is left **on** (`clickhouse::Client::with_validation`
    /// defaults to enabled), which is mandatory per ADR-043 §5d: the
    /// names-and-types row format lets ClickHouse itself reject a column
    /// mismatch server-side, rather than writing garbage under the bare
    /// positional format.
    pub fn clickhouse_insert_client(&self) -> Result<clickhouse::Client, CloudError> {
        if !self.telemetry_enabled() {
            return Err(CloudError::FeatureDisabled {
                feature: "telemetry",
            });
        }
        let (base_url, token) = self.linked_credential()?;
        let backend = self.parse_backend(&base_url)?;
        let url = backend.endpoint(TELEMETRY_INSERT_PATH).to_string();

        let encoded =
            base64::engine::general_purpose::STANDARD.encode(format!("{QUERY_PROXY_USER}:{token}"));
        Ok(clickhouse::Client::default()
            .with_url(url)
            .with_database(CLOUD_TELEMETRY_DATABASE)
            .with_header("Authorization", format!("Basic {encoded}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CloudFeatureSwitches;

    fn enable_telemetry(link: &CloudLink) {
        link.set_feature_switches(CloudFeatureSwitches {
            telemetry: true,
            backups: false,
            notifications: false,
        })
        .expect("apply feature switches");
    }

    #[test]
    fn an_unenrolled_instance_refuses_to_build_an_insert_client() {
        let dir = tempfile::tempdir().expect("temp dir");
        let link = CloudLink::load(dir.path().to_path_buf(), "0.1.0-test");
        enable_telemetry(&link);

        assert!(matches!(
            link.clickhouse_insert_client(),
            Err(CloudError::NotEnrolled)
        ));
    }

    #[test]
    fn telemetry_switched_off_refuses_the_insert_client_before_reading_the_credential() {
        let dir = tempfile::tempdir().expect("temp dir");
        let link = CloudLink::load(dir.path().to_path_buf(), "0.1.0-test");
        // Feature switches default to off; never enabled here.

        match link.clickhouse_insert_client() {
            Err(CloudError::FeatureDisabled { feature }) => assert_eq!(feature, "telemetry"),
            other => panic!("expected the telemetry switch to refuse, got {other:?}"),
        }
    }

    #[test]
    fn the_insert_path_is_distinct_from_the_read_only_query_path() {
        // ADR-043 §5a's hard constraint: the insert surface must not share a
        // handler with the read proxy, whose safety rests entirely on
        // unconditionally injecting `readonly=1`.
        assert_ne!(TELEMETRY_INSERT_PATH, crate::query::TELEMETRY_QUERY_PATH);
    }
}
