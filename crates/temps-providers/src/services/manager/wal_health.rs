// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The Postgres WAL and archive check for a database on a worker node, and
//! what the service page reads back.
//!
//! The health monitor probes a local database directly. A database on a
//! worker publishes its port on that worker, behind an address recorded as
//! `localhost`, so the control plane cannot connect: the probe runs on the
//! worker, through its agent. When it cannot run (an agent that predates the
//! route, an unreachable agent, a probe that fails), the reason is recorded
//! next to the snapshot, so the page says the check is not happening rather
//! than showing nothing.

use crate::externalsvc::postgres_wal_health::PostgresWalHealth;
use crate::remote_service_client::{AgentSupport, RemoteWalHealthRequest};
use crate::services::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use temps_entities::external_services;

/// Key under `external_services.health_metadata` for the WAL snapshot.
pub(crate) const POSTGRES_WAL_KEY: &str = "postgres_wal";
/// Key under `external_services.health_metadata` for why the WAL check last
/// did not produce a snapshot. Cleared by the next snapshot.
pub(crate) const POSTGRES_WAL_UNCHECKED_KEY: &str = "postgres_wal_unchecked";

/// Why the WAL check of a service did not run, as recorded for the page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WalHealthUnchecked {
    pub reason: String,
    pub checked_at: DateTime<Utc>,
}

/// The outcome of one WAL check.
#[derive(Debug, Clone)]
#[allow(
    clippy::large_enum_variant,
    reason = "one transient value per check, consumed at once; boxing the snapshot buys nothing"
)]
pub enum WalProbeOutcome {
    Snapshot(PostgresWalHealth),
    /// The check could not run or produced nothing; says why.
    Unchecked(String),
}

/// What the service page gets for a service's WAL health.
#[derive(Debug, Clone)]
pub enum WalHealthReading {
    /// The last check produced this snapshot.
    Snapshot(PostgresWalHealth),
    /// The last check could not run and said why, whether or not an older
    /// snapshot exists: that one no longer describes the database.
    Unchecked(WalHealthUnchecked),
    /// No snapshot and no recorded reason: not probed yet, or not Postgres.
    Missing,
}

/// `existing` health metadata with `outcome` recorded: a snapshot replaces
/// the previous one and clears any recorded reason; a reason is recorded and
/// the previous snapshot kept (its `probed_at` dates it). A recorded reason
/// is therefore always newer than the snapshot beside it, and is what
/// [`wal_health_reading`] reads.
pub(crate) fn record_wal_outcome(
    existing: Option<&serde_json::Value>,
    outcome: &WalProbeOutcome,
    now: DateTime<Utc>,
) -> Result<serde_json::Value, serde_json::Error> {
    let mut map = match existing {
        Some(serde_json::Value::Object(map)) => map.clone(),
        _ => serde_json::Map::new(),
    };
    match outcome {
        WalProbeOutcome::Snapshot(snapshot) => {
            map.insert(
                POSTGRES_WAL_KEY.to_string(),
                serde_json::to_value(snapshot)?,
            );
            map.remove(POSTGRES_WAL_UNCHECKED_KEY);
        }
        WalProbeOutcome::Unchecked(reason) => {
            map.insert(
                POSTGRES_WAL_UNCHECKED_KEY.to_string(),
                serde_json::to_value(WalHealthUnchecked {
                    reason: reason.clone(),
                    checked_at: now,
                })?,
            );
        }
    }
    Ok(serde_json::Value::Object(map))
}

/// The WAL health a service's `health_metadata` says: the reason the last
/// check could not run when one is recorded (it is newer than any snapshot
/// beside it, see [`record_wal_outcome`]), else the last snapshot. A value
/// that does not parse is logged and skipped.
pub(crate) fn wal_health_reading(
    service_id: i32,
    metadata: &serde_json::Value,
) -> WalHealthReading {
    if let Some(unchecked) = metadata.get(POSTGRES_WAL_UNCHECKED_KEY) {
        match serde_json::from_value(unchecked.clone()) {
            Ok(parsed) => return WalHealthReading::Unchecked(parsed),
            Err(error) => tracing::warn!(
                service_id,
                error = %error,
                "health_metadata.postgres_wal_unchecked did not parse"
            ),
        }
    }
    if let Some(snapshot) = metadata.get(POSTGRES_WAL_KEY) {
        match serde_json::from_value(snapshot.clone()) {
            Ok(parsed) => return WalHealthReading::Snapshot(parsed),
            Err(error) => tracing::warn!(
                service_id,
                error = %error,
                "health_metadata.postgres_wal did not parse"
            ),
        }
    }
    WalHealthReading::Missing
}

impl ExternalServiceManager {
    /// Run the WAL check of a standalone Postgres service on the worker that
    /// owns it, through that node's agent.
    pub async fn probe_worker_postgres_wal(
        &self,
        service: &external_services::Model,
        node_id: i32,
    ) -> WalProbeOutcome {
        let service_config = match self.get_service_config(service.id).await {
            Ok(config) => config,
            Err(error) => {
                return WalProbeOutcome::Unchecked(format!(
                    "WAL health is not checked: the configuration of service {} could not be \
                     loaded: {error}",
                    service.id
                ))
            }
        };
        let client = match self.get_remote_client(node_id).await {
            Ok(client) => client,
            Err(error) => {
                return WalProbeOutcome::Unchecked(format!(
                    "WAL health is not checked: cannot reach the agent on node {node_id}: {error}"
                ))
            }
        };
        let node = client.node_name().to_string();
        match client
            .probe_wal_health(RemoteWalHealthRequest { service_config })
            .await
        {
            Ok(AgentSupport::Supported(response)) => match response.snapshot {
                Some(snapshot) => match serde_json::from_value(snapshot) {
                    Ok(snapshot) => WalProbeOutcome::Snapshot(snapshot),
                    Err(error) => WalProbeOutcome::Unchecked(format!(
                        "WAL health is not checked: the agent on node {node} answered a snapshot \
                         this server cannot read ({error}); update the agent on node {node} to \
                         this release"
                    )),
                },
                None => WalProbeOutcome::Unchecked(format!(
                    "WAL health could not be read on node {node}: {}",
                    response
                        .error
                        .unwrap_or_else(|| "the probe returned no snapshot".to_string())
                )),
            },
            Ok(AgentSupport::Unsupported) => WalProbeOutcome::Unchecked(format!(
                "WAL health is not checked for this database: the agent on node {node} (id \
                 {node_id}) does not support the WAL probe; update the agent on node {node} to \
                 this release"
            )),
            Err(error) => WalProbeOutcome::Unchecked(format!(
                "WAL health could not be read on node {node}: {error}"
            )),
        }
    }

    /// The WAL health the service page shows: why the last check could not
    /// run, when it could not, else the last snapshot.
    ///
    /// `ServiceNotFound` only when the row doesn't exist at all.
    pub async fn get_postgres_wal_health_reading(
        &self,
        service_id: i32,
    ) -> Result<WalHealthReading, ExternalServiceError> {
        let service = self.get_service(service_id).await?;
        Ok(match service.health_metadata {
            Some(metadata) => wal_health_reading(service_id, &metadata),
            None => WalHealthReading::Missing,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::externalsvc::postgres_wal_health::ArchiveMode;

    fn snapshot() -> PostgresWalHealth {
        PostgresWalHealth {
            probed_at: Utc::now(),
            pg_wal_bytes: 1024,
            max_wal_size_bytes: 4096,
            archive_mode: ArchiveMode::On,
            archive_command: Some("wal-g wal-push %p".to_string()),
            archive_backlog: 0,
            archive_library: None,
            archive_status_unreadable: false,
            archiver_failed_count: None,
            archiver_last_failed_at: None,
            archiver_archived_count: None,
            archiver_last_archived_at: None,
            archiver_last_failed_wal: None,
            archive_oldest_ready_age_secs: None,
            stale_slots: Vec::new(),
            oldest_wal_age_secs: 0,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn a_reason_is_recorded_beside_the_last_snapshot_and_siblings() {
        let existing = serde_json::json!({
            "postgres_wal": serde_json::to_value(snapshot()).unwrap(),
            "other": 1,
        });
        let now = Utc::now();
        let updated = record_wal_outcome(
            Some(&existing),
            &WalProbeOutcome::Unchecked("agent too old".to_string()),
            now,
        )
        .expect("record");
        assert_eq!(updated["other"], 1);
        assert!(
            updated.get(POSTGRES_WAL_KEY).is_some(),
            "keeps the snapshot"
        );
        assert_eq!(
            updated[POSTGRES_WAL_UNCHECKED_KEY]["reason"],
            "agent too old"
        );
    }

    /// A worker produced a snapshot, then its probe started failing: the
    /// page must get the failure, not the old snapshot with HTTP 200, or its
    /// "WAL health is not being checked" warning never shows.
    #[test]
    fn a_later_failure_is_read_instead_of_the_older_snapshot() {
        let first = Utc::now();
        let with_snapshot = record_wal_outcome(None, &WalProbeOutcome::Snapshot(snapshot()), first)
            .expect("record");
        assert!(matches!(
            wal_health_reading(1, &with_snapshot),
            WalHealthReading::Snapshot(_)
        ));
        let failed = record_wal_outcome(
            Some(&with_snapshot),
            &WalProbeOutcome::Unchecked("cannot reach the agent on node 2".to_string()),
            first + chrono::Duration::minutes(5),
        )
        .expect("record");
        match wal_health_reading(1, &failed) {
            WalHealthReading::Unchecked(unchecked) => {
                assert_eq!(unchecked.reason, "cannot reach the agent on node 2")
            }
            other => panic!("the newer failure must be read, got {other:?}"),
        }
        // The next snapshot clears it again.
        let recovered = record_wal_outcome(
            Some(&failed),
            &WalProbeOutcome::Snapshot(snapshot()),
            first + chrono::Duration::minutes(10),
        )
        .expect("record");
        assert!(matches!(
            wal_health_reading(1, &recovered),
            WalHealthReading::Snapshot(_)
        ));
        assert!(matches!(
            wal_health_reading(1, &serde_json::json!({})),
            WalHealthReading::Missing
        ));
    }

    /// A recorded reason that does not parse does not hide the snapshot.
    #[test]
    fn an_unreadable_reason_falls_back_to_the_snapshot() {
        let metadata = serde_json::json!({
            "postgres_wal": serde_json::to_value(snapshot()).unwrap(),
            "postgres_wal_unchecked": "not an object",
        });
        assert!(matches!(
            wal_health_reading(1, &metadata),
            WalHealthReading::Snapshot(_)
        ));
    }

    #[test]
    fn a_snapshot_clears_the_recorded_reason() {
        let existing = serde_json::json!({
            "postgres_wal_unchecked": {"reason": "agent too old", "checked_at": Utc::now()},
        });
        let updated = record_wal_outcome(
            Some(&existing),
            &WalProbeOutcome::Snapshot(snapshot()),
            Utc::now(),
        )
        .expect("record");
        assert!(
            updated.get(POSTGRES_WAL_UNCHECKED_KEY).is_none(),
            "{updated}"
        );
        assert_eq!(updated[POSTGRES_WAL_KEY]["pg_wal_bytes"], 1024);
    }
}
