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
pub enum WalProbeOutcome {
    Snapshot(PostgresWalHealth),
    /// The check could not run or produced nothing; says why.
    Unchecked(String),
}

/// What the service page gets for a service's WAL health.
#[derive(Debug, Clone)]
pub enum WalHealthReading {
    Snapshot(PostgresWalHealth),
    /// No snapshot, and the last check said why.
    Unchecked(WalHealthUnchecked),
    /// No snapshot and no recorded reason: not probed yet, or not Postgres.
    Missing,
}

/// `existing` health metadata with `outcome` recorded: a snapshot replaces
/// the previous one and clears any recorded reason; a reason is recorded and
/// the previous snapshot kept (its `probed_at` dates it).
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

    /// The WAL health the service page shows: the last snapshot, else why
    /// the last check produced none.
    ///
    /// `ServiceNotFound` only when the row doesn't exist at all.
    pub async fn get_postgres_wal_health_reading(
        &self,
        service_id: i32,
    ) -> Result<WalHealthReading, ExternalServiceError> {
        let service = self.get_service(service_id).await?;
        let Some(metadata) = service.health_metadata else {
            return Ok(WalHealthReading::Missing);
        };
        if let Some(snapshot) = metadata.get(POSTGRES_WAL_KEY) {
            match serde_json::from_value(snapshot.clone()) {
                Ok(parsed) => return Ok(WalHealthReading::Snapshot(parsed)),
                Err(error) => tracing::warn!(
                    service_id,
                    error = %error,
                    "health_metadata.postgres_wal did not parse"
                ),
            }
        }
        if let Some(unchecked) = metadata.get(POSTGRES_WAL_UNCHECKED_KEY) {
            match serde_json::from_value(unchecked.clone()) {
                Ok(parsed) => return Ok(WalHealthReading::Unchecked(parsed)),
                Err(error) => tracing::warn!(
                    service_id,
                    error = %error,
                    "health_metadata.postgres_wal_unchecked did not parse"
                ),
            }
        }
        Ok(WalHealthReading::Missing)
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
            archiver_failed_count: None,
            archiver_last_failed_at: None,
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
