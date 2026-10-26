// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where the control plane keeps Shield events (ADR 066, D9, D11 and I8).
//!
//! The request path hands events to [`ShieldEventStore`], the
//! [`EventSink`] of this process: a `try_send` into a bounded channel, so a
//! store that falls behind drops events and never slows a request. One
//! [`ShieldEventWriter`] task drains the channel and writes batches of up to
//! [`MAX_BATCH`] events, or whatever arrived within [`FLUSH_INTERVAL`], to
//! ClickHouse `shield_events` when ClickHouse is configured and to the
//! Postgres `shield_events` table otherwise. A batch that fails to write is
//! logged and dropped, like the request log's.

use std::net::{IpAddr, Ipv6Addr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sea_orm::{ActiveValue::Set, DatabaseConnection, EntityTrait};
use serde::{Deserialize, Serialize};
use temps_entities::shield_events;
use temps_shield::{EventSink, ShieldEvent};
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// Events buffered before new ones are dropped. An event is well under
/// 2 KB (path and User-Agent are bounded), so the buffer stays under 8 MB.
pub const CHANNEL_CAPACITY: usize = 4096;

/// Events written in one insert.
pub const MAX_BATCH: usize = 500;

/// Longest an event waits in a partial batch.
pub const FLUSH_INTERVAL: Duration = Duration::from_secs(2);

/// Log a shed warning once per this many dropped events.
const DROP_LOG_EVERY: u64 = 10_000;

#[derive(Debug, thiserror::Error)]
pub enum ShieldEventStoreError {
    #[error("writing {count} Shield events to ClickHouse failed during {operation}: {reason}")]
    ClickHouse {
        operation: &'static str,
        count: usize,
        reason: String,
    },
    #[error("writing {count} Shield events to Postgres failed: {source}")]
    Postgres {
        count: usize,
        #[source]
        source: sea_orm::DbErr,
    },
}

/// Where the writer stores events, chosen once at startup the same way the
/// request log chooses its backend.
#[derive(Clone)]
pub enum EventBackend {
    ClickHouse(Box<::clickhouse::Client>),
    Postgres(Arc<DatabaseConnection>),
}

impl EventBackend {
    /// ClickHouse when `config` configures it, else Postgres. Its
    /// `shield_events` table is created by the proxy-log migrations, which
    /// the request log's storage applies at startup.
    pub fn from_config(
        config: Option<&temps_config::ServerConfig>,
        db: Arc<DatabaseConnection>,
        compression: temps_clickhouse::ClickHouseCompression,
    ) -> Self {
        match config {
            Some(config) if config.is_clickhouse_enabled() => {
                Self::ClickHouse(Box::new(temps_clickhouse::build_client(
                    config.clickhouse_url.as_deref().unwrap_or_default(),
                    config.clickhouse_database.as_deref().unwrap_or_default(),
                    config.clickhouse_user.as_deref().unwrap_or_default(),
                    config.clickhouse_password.as_deref().unwrap_or_default(),
                    compression,
                )))
            }
            _ => Self::Postgres(db),
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::ClickHouse(_) => "clickhouse",
            Self::Postgres(_) => "postgres",
        }
    }

    async fn write(&self, events: &[ShieldEvent]) -> Result<(), ShieldEventStoreError> {
        if events.is_empty() {
            return Ok(());
        }
        match self {
            Self::ClickHouse(client) => write_clickhouse(client, events).await,
            Self::Postgres(db) => write_postgres(db, events).await,
        }
    }
}

/// The control plane's [`EventSink`].
pub struct ShieldEventStore {
    sender: mpsc::Sender<ShieldEvent>,
    dropped: AtomicU64,
}

impl ShieldEventStore {
    /// The sink and the task that writes what it receives; the caller runs
    /// [`ShieldEventWriter::run`] on a runtime of its choosing.
    pub fn new(backend: EventBackend) -> (Arc<Self>, ShieldEventWriter) {
        let (sender, receiver) = mpsc::channel(CHANNEL_CAPACITY);
        (
            Arc::new(Self {
                sender,
                dropped: AtomicU64::new(0),
            }),
            ShieldEventWriter { receiver, backend },
        )
    }

    /// Events dropped since startup because the buffer was full.
    pub fn dropped_total(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl EventSink for ShieldEventStore {
    fn submit(&self, events: Vec<ShieldEvent>) {
        for event in events {
            if self.sender.try_send(event).is_err() {
                let total = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                if total == 1 || total.is_multiple_of(DROP_LOG_EVERY) {
                    warn!(
                        dropped_total = total,
                        capacity = CHANNEL_CAPACITY,
                        "Shield event buffer full; dropping events"
                    );
                }
            }
        }
    }
}

pub struct ShieldEventWriter {
    receiver: mpsc::Receiver<ShieldEvent>,
    backend: EventBackend,
}

impl ShieldEventWriter {
    /// Write batches until every sender is gone, then flush what is left.
    pub async fn run(mut self) {
        debug!(backend = self.backend.name(), "Shield event writer started");
        let mut batch = Vec::with_capacity(MAX_BATCH);
        loop {
            // Idle until the first event; then at most FLUSH_INTERVAL until
            // the batch is written.
            if self.receiver.recv_many(&mut batch, MAX_BATCH).await == 0 {
                return;
            }
            let deadline = tokio::time::Instant::now() + FLUSH_INTERVAL;
            while batch.len() < MAX_BATCH {
                let room = MAX_BATCH - batch.len();
                match tokio::time::timeout_at(deadline, self.receiver.recv_many(&mut batch, room))
                    .await
                {
                    Ok(0) => {
                        self.flush(&mut batch).await;
                        return;
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            self.flush(&mut batch).await;
        }
    }

    async fn flush(&self, batch: &mut Vec<ShieldEvent>) {
        if batch.is_empty() {
            return;
        }
        if let Err(error) = self.backend.write(batch).await {
            warn!(%error, "Dropping a batch of Shield events");
        }
        batch.clear();
    }
}

/// A `shield_events` row in ClickHouse, fields in the table's column order.
#[derive(::clickhouse::Row, Serialize, Deserialize, Debug, Clone, PartialEq)]
pub(crate) struct ChShieldEventRow {
    /// DateTime64(3): Unix milliseconds.
    pub timestamp: i64,
    pub project_id: i32,
    pub environment_id: i32,
    pub node_id: Option<i32>,
    pub request_id: String,
    pub kind: String,
    pub ip: Ipv6Addr,
    pub ja4: String,
    /// Enum8: `pass` 0, `log` 1, `challenge` 2, `block` 3.
    pub verdict: i8,
    pub mode: String,
    pub reasons: Vec<String>,
    pub signature: String,
    pub host: String,
    pub path: String,
    pub user_agent: String,
    pub difficulty: Option<u8>,
    pub solve_ms: Option<u32>,
}

impl From<&ShieldEvent> for ChShieldEventRow {
    fn from(event: &ShieldEvent) -> Self {
        Self {
            timestamp: event.timestamp_ms,
            project_id: event.project_id,
            environment_id: event.environment_id,
            node_id: event.node_id,
            request_id: event.request_id.clone(),
            kind: event.kind.as_str().to_string(),
            ip: ipv6(&event.ip),
            ja4: event.ja4.clone().unwrap_or_default(),
            verdict: verdict_code(&event.verdict),
            mode: event.mode.clone(),
            reasons: event.reasons.clone(),
            signature: event.signature.clone().unwrap_or_default(),
            host: event.host.clone(),
            path: event.path.clone(),
            user_agent: event.user_agent.clone(),
            difficulty: event.difficulty,
            solve_ms: event.solve_ms,
        }
    }
}

/// The address as ClickHouse `IPv6` stores it: IPv4 mapped into
/// `::ffff:0:0/96`, and `::` for an address the ingress could not resolve.
fn ipv6(text: &str) -> Ipv6Addr {
    match text.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => v4.to_ipv6_mapped(),
        Ok(IpAddr::V6(v6)) => v6,
        Err(_) => Ipv6Addr::UNSPECIFIED,
    }
}

/// The `verdict` enum value; anything unknown records as `pass`.
fn verdict_code(verdict: &str) -> i8 {
    match verdict {
        "log" => 1,
        "challenge" => 2,
        "block" => 3,
        _ => 0,
    }
}

async fn write_clickhouse(
    client: &::clickhouse::Client,
    events: &[ShieldEvent],
) -> Result<(), ShieldEventStoreError> {
    let count = events.len();
    let error = |operation, error: ::clickhouse::error::Error| ShieldEventStoreError::ClickHouse {
        operation,
        count,
        reason: error.to_string(),
    };
    let mut inserter = client
        .insert::<ChShieldEventRow>("shield_events")
        .await
        .map_err(|e| error("inserter setup", e))?;
    for event in events {
        inserter
            .write(&ChShieldEventRow::from(event))
            .await
            .map_err(|e| error("write", e))?;
    }
    inserter.end().await.map_err(|e| error("end", e))
}

async fn write_postgres(
    db: &DatabaseConnection,
    events: &[ShieldEvent],
) -> Result<(), ShieldEventStoreError> {
    let rows = events.iter().map(|event| shield_events::ActiveModel {
        timestamp: Set(chrono::DateTime::from_timestamp_millis(event.timestamp_ms)
            .unwrap_or_else(chrono::Utc::now)),
        project_id: Set(event.project_id),
        environment_id: Set(event.environment_id),
        node_id: Set(event.node_id),
        request_id: Set(event.request_id.clone()),
        kind: Set(event.kind.as_str().to_string()),
        ip: Set(event.ip.clone()),
        ja4: Set(event.ja4.clone()),
        verdict: Set(event.verdict.clone()),
        mode: Set(event.mode.clone()),
        reasons: Set(serde_json::Value::from(event.reasons.clone())),
        signature: Set(event.signature.clone()),
        host: Set(event.host.clone()),
        path: Set(event.path.clone()),
        user_agent: Set(event.user_agent.clone()),
        difficulty: Set(event.difficulty.map(i16::from)),
        solve_ms: Set(event
            .solve_ms
            .map(|ms| i32::try_from(ms).unwrap_or(i32::MAX))),
        ..Default::default()
    });
    shield_events::Entity::insert_many(rows)
        .exec(db)
        .await
        .map(|_| ())
        .map_err(|source| ShieldEventStoreError::Postgres {
            count: events.len(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ColumnTrait, QueryFilter, QueryOrder};
    use temps_shield::EventKind;

    fn event(request_id: &str, ip: &str, kind: EventKind) -> ShieldEvent {
        ShieldEvent {
            timestamp_ms: 1_800_000_000_123,
            project_id: 7,
            environment_id: 3,
            node_id: None,
            request_id: request_id.to_string(),
            kind,
            ip: ip.to_string(),
            ja4: Some("t13d1516h2_8daaf6152771_02713d6af862".to_string()),
            verdict: "block".to_string(),
            mode: "selective".to_string(),
            reasons: vec!["spamhaus_drop (185.93.89.0/24)".to_string()],
            signature: None,
            host: "app.example.com".to_string(),
            path: "/".to_string(),
            user_agent: "Mozilla/5.0".to_string(),
            difficulty: None,
            solve_ms: Some(840),
        }
    }

    #[test]
    fn rows_map_addresses_and_verdicts_as_the_table_stores_them() {
        let row = ChShieldEventRow::from(&event("r1", "185.93.89.7", EventKind::Verdict));
        assert_eq!(row.ip.to_string(), "::ffff:185.93.89.7");
        assert_eq!(row.verdict, 3);
        assert_eq!(row.kind, "verdict");
        assert_eq!(row.signature, "");
        assert_eq!(ipv6("2001:db8::1").to_string(), "2001:db8::1");
        assert_eq!(ipv6("unknown"), Ipv6Addr::UNSPECIFIED);
        for (text, code) in [("pass", 0), ("log", 1), ("challenge", 2), ("block", 3)] {
            assert_eq!(verdict_code(text), code);
        }
    }

    /// A full buffer drops events instead of blocking the request path.
    #[tokio::test]
    async fn a_full_buffer_drops_instead_of_blocking() {
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        let (store, writer) = ShieldEventStore::new(EventBackend::Postgres(db));
        let events = (0..CHANNEL_CAPACITY + 3)
            .map(|i| event(&format!("r{i}"), "203.0.113.9", EventKind::Verdict))
            .collect();
        store.submit(events);
        assert_eq!(store.dropped_total(), 3);
        drop(writer);
    }

    #[tokio::test]
    async fn events_reach_postgres_in_batches() {
        let db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) =>
            {
                eprintln!("Skipping Shield event store test: Docker runtime unavailable");
                return;
            }
            Err(error) => panic!("test database: {error}"),
        };
        let (store, writer) =
            ShieldEventStore::new(EventBackend::Postgres(db.connection_arc().clone()));
        let task = tokio::spawn(writer.run());
        store.submit(vec![
            event("r1", "185.93.89.7", EventKind::Verdict),
            event("r2", "203.0.113.9", EventKind::ChallengeSolved),
        ]);
        // Closing the channel flushes what is buffered.
        drop(store);
        task.await.unwrap();

        let rows = shield_events::Entity::find()
            .filter(shield_events::Column::EnvironmentId.eq(3))
            .order_by_asc(shield_events::Column::RequestId)
            .all(db.connection())
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].kind, "verdict");
        assert_eq!(rows[0].ip, "185.93.89.7");
        assert_eq!(
            rows[0].reasons,
            serde_json::json!(["spamhaus_drop (185.93.89.0/24)"])
        );
        assert_eq!(rows[0].timestamp.timestamp_millis(), 1_800_000_000_123);
        assert_eq!(rows[1].kind, "challenge_solved");
        assert_eq!(rows[1].solve_ms, Some(840));
    }

    #[tokio::test]
    async fn events_reach_clickhouse_with_the_contract_schema() {
        let Some((store, _container)) =
            crate::storage::clickhouse::tests::setup_clickhouse_store().await
        else {
            return;
        };
        let backend = EventBackend::ClickHouse(Box::new(store.client().clone()));
        backend
            .write(&[
                event("r1", "185.93.89.7", EventKind::Verdict),
                event("r2", "2001:db8::1", EventKind::ChallengeFailed),
            ])
            .await
            .expect("write to ClickHouse");
        let rows = store
            .client()
            .query("SELECT ?fields FROM shield_events ORDER BY request_id")
            .fetch_all::<ChShieldEventRow>()
            .await
            .expect("read shield_events");
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0],
            ChShieldEventRow::from(&event("r1", "185.93.89.7", EventKind::Verdict))
        );
        assert_eq!(rows[1].ip.to_string(), "2001:db8::1");
        assert_eq!(rows[1].kind, "challenge_failed");
    }
}
