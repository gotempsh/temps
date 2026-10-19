// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Control-plane retention for the ClickHouse analytics replica.

use clickhouse::Client;
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, DbErr, Statement, TransactionTrait,
};
use temps_core::{MAX_ANALYTICS_RETENTION_DAYS, MIN_ANALYTICS_RETENTION_DAYS};
use thiserror::Error;

/// Cross-process fence between visitor cleanup and buffered event delivery.
/// Shared by fan-out/backfill for one batch; exclusive for one cleanup batch.
/// It never runs on event ingestion or the proxy request path.
const DELIVERY_LOCK: i64 = 0x54454d5053434845;
// Separate from delivery: reading a matching TTL must not pause event batches.
const TTL_RECONCILIATION_LOCK: i64 = 0x54454d5053434854;

pub async fn lock_event_delivery<C: ConnectionTrait>(txn: &C, deleting: bool) -> Result<(), DbErr> {
    let sql = if deleting {
        "SELECT pg_advisory_xact_lock($1)"
    } else {
        "SELECT pg_advisory_xact_lock_shared($1)"
    };
    tokio::time::timeout(
        std::time::Duration::from_secs(120),
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            [DELIVERY_LOCK.into()],
        )),
    )
    .await
    .map_err(|_| {
        DbErr::Custom("timed out waiting for ClickHouse event delivery fence after 120s".into())
    })??;
    Ok(())
}

#[derive(Debug, Error)]
pub enum ClickHouseRetentionError {
    #[error("Invalid ClickHouse events retention {days} days; expected 1..=3650")]
    InvalidDays { days: u32 },
    #[error("Failed to {operation} while coordinating ClickHouse analytics database '{database}': {source}")]
    Coordination {
        database: String,
        operation: &'static str,
        source: DbErr,
    },
    #[error("Failed to {operation} in ClickHouse analytics database '{database}': {reason}")]
    Query {
        database: String,
        operation: String,
        reason: String,
    },
}

fn coordination_error(
    client: &Client,
    operation: &'static str,
    source: DbErr,
) -> ClickHouseRetentionError {
    ClickHouseRetentionError::Coordination {
        database: client.database().unwrap_or("default").to_string(),
        operation,
        source,
    }
}

/// Serialize the read/ALTER pair across Temps processes sharing PostgreSQL.
/// Console/proxy startup and hourly jobs can otherwise both observe drift
/// and schedule the same expensive TTL materialization. The lock is released
/// on every error/cancellation; no installed-policy cache survives the pass.
pub async fn reconcile_events_ttl(
    db: &DatabaseConnection,
    client: &Client,
    days: u32,
) -> Result<bool, ClickHouseRetentionError> {
    validate_days(days)?;
    let txn = db
        .begin()
        .await
        .map_err(|e| coordination_error(client, "begin events TTL reconciliation", e))?;
    tokio::time::timeout(
        std::time::Duration::from_secs(120),
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT pg_advisory_xact_lock($1)",
            [TTL_RECONCILIATION_LOCK.into()],
        )),
    )
    .await
    .map_err(|_| {
        coordination_error(
            client,
            "acquire events TTL reconciliation lock",
            DbErr::Custom("timed out waiting for events TTL reconciliation after 120s".into()),
        )
    })?
    .map_err(|e| coordination_error(client, "acquire events TTL reconciliation lock", e))?;
    let changed = sync_events_ttl(client, days).await?;
    txn.commit()
        .await
        .map_err(|e| coordination_error(client, "commit events TTL reconciliation", e))?;
    Ok(changed)
}

fn validate_days(days: u32) -> Result<(), ClickHouseRetentionError> {
    if !(MIN_ANALYTICS_RETENTION_DAYS..=MAX_ANALYTICS_RETENTION_DAYS).contains(&days) {
        return Err(ClickHouseRetentionError::InvalidDays { days });
    }
    Ok(())
}

fn query_error(
    client: &Client,
    operation: impl Into<String>,
    reason: impl ToString,
) -> ClickHouseRetentionError {
    ClickHouseRetentionError::Query {
        database: client.database().unwrap_or("default").to_string(),
        operation: operation.into(),
        reason: reason.to_string(),
    }
}

/// Re-read the stored table definition on every pass, including after restart.
/// Unqualified `events` and `currentDatabase()` use the client's configured DB.
/// Running Temps services use [`reconcile_events_ttl`] to coordinate callers.
/// No success cache: an unavailable table or denied ALTER is retried next pass.
/// `events_days` is required; zero/null do not mean unlimited retention.
pub async fn sync_events_ttl(client: &Client, days: u32) -> Result<bool, ClickHouseRetentionError> {
    validate_days(days)?;
    #[derive(clickhouse::Row, serde::Deserialize)]
    struct Definition {
        create_table_query: String,
    }
    let definition = client
        .query("SELECT formatQuery(create_table_query) AS create_table_query FROM system.tables WHERE database = currentDatabase() AND name = 'events'")
        .fetch_one::<Definition>()
        .await
        .map_err(|e| query_error(client, "read events TTL", e))?;
    if ttl_matches(&definition.create_table_query, days) {
        return Ok(false);
    }
    client
        .query(&format!(
            "ALTER TABLE events MODIFY TTL toDateTime(timestamp) + INTERVAL {days} DAY"
        ))
        // Keep ClickHouse's default materialization: existing parts need their
        // TTL metadata recalculated too, especially when shortening a window.
        .with_setting("materialize_ttl_after_modify", "1")
        .execute()
        .await
        .map_err(|e| query_error(client, format!("set events TTL to {days} days"), e))?;
    Ok(true)
}

fn ttl_matches(ddl: &str, days: u32) -> bool {
    // formatQuery gives a stable multiline layout even when system.tables
    // stores a single-line CREATE (including on ClickHouse 26.9). The table
    // TTL has its own line, unlike column TTLs. Compare the whole clause so
    // conditional/multiple rules
    // cannot be mistaken for the desired unconditional deletion window.
    let Some(ttl) = ddl
        .lines()
        .find_map(|line| line.trim().strip_prefix("TTL "))
    else {
        return false;
    };
    let normalized: String = ttl
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '`')
        .collect();
    let desired = format!("toDateTime(timestamp)+toIntervalDay({days})");
    normalized == desired || normalized == format!("{desired}DELETE")
}

/// Remove one bounded batch of project/visitor pairs. Called while holding
/// the delivery fence and visitor row locks, before committing PG deletion.
/// Wait for every replica: acceptance of an asynchronous mutation is not
/// sufficient to report visitor cleanup as successful.
pub async fn delete_visitor_events(
    client: &Client,
    visitors: &[(i32, i32)],
) -> Result<(), ClickHouseRetentionError> {
    let Some((first_project, first_visitor)) = visitors.first() else {
        return Ok(());
    };
    let predicates = vec!["(?, ?)"; visitors.len()].join(", ");
    let sql = format!("ALTER TABLE events DELETE WHERE (project_id, visitor_id) IN ({predicates})");
    let mut query = client.query(&sql).with_setting("mutations_sync", "2");
    for (project_id, visitor_id) in visitors {
        query = query.bind(project_id).bind(visitor_id);
    }
    query.execute().await.map_err(|e| {
        query_error(
            client,
            format!(
                "delete events for {} project-scoped visitors (first project {first_project}, visitor {first_visitor})",
                visitors.len()
            ),
            e,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_only_the_complete_table_ttl() {
        let ddl = "CREATE TABLE events (x String TTL timestamp + toIntervalDay(90))\nENGINE = MergeTree\nTTL toDateTime(timestamp) + toIntervalDay(90)\nSETTINGS index_granularity = 8192";
        assert!(ttl_matches(ddl, 90));
        assert!(!ttl_matches(ddl, 30));
        assert!(ttl_matches(
            "TTL toDateTime(`timestamp`) + toIntervalDay(90) DELETE",
            90
        ));
        for ttl in [
            "TTL toDateTime(timestamp) + toIntervalYear(5)",
            "TTL toDateTime(timestamp) + toIntervalDay(90) DELETE WHERE project_id = 1",
            "TTL toDateTime(timestamp) + toIntervalDay(90), toDateTime(timestamp) + toIntervalDay(30) TO VOLUME 'cold'",
            "CREATE TABLE events (x String TTL timestamp + toIntervalDay(90))",
            "",
        ] {
            assert!(!ttl_matches(ttl, 90), "{ttl}");
        }
    }

    #[tokio::test]
    async fn invalid_windows_fail_before_any_io_including_zero() {
        for days in [0, 3651, u32::MAX] {
            assert!(matches!(
                sync_events_ttl(&Client::default(), days).await,
                Err(ClickHouseRetentionError::InvalidDays { days: actual }) if actual == days
            ));
            assert!(matches!(
                reconcile_events_ttl(&DatabaseConnection::Disconnected, &Client::default(), days)
                    .await,
                Err(ClickHouseRetentionError::InvalidDays { .. })
            ));
        }
        // There is no unlimited events setting; an empty visitor batch is a no-op.
        delete_visitor_events(&Client::default(), &[])
            .await
            .unwrap();
    }

    /// Each invocation owns its container/database. No host database or
    /// pre-existing Docker resource is modified, and Drop removes the container.
    #[tokio::test]
    async fn clickhouse_ttl_upgrade_drift_restart_and_scoped_deletion() {
        use testcontainers::{
            core::{wait::HttpWaitStrategy, ContainerPort, Mount, WaitFor},
            runners::AsyncRunner,
            GenericImage, ImageExt,
        };
        if !match bollard::Docker::connect_with_defaults() {
            Ok(docker) => docker.ping().await.is_ok(),
            Err(_) => false,
        } {
            eprintln!("Docker unavailable; skipping real ClickHouse retention test");
            return;
        }
        let database = format!("retention_{}", uuid::Uuid::new_v4().simple());
        let pg = temps_database::test_utils::TestDatabase::new_isolated_with_config(
            &database,
            "test_user",
            "test_password",
        )
        .await
        .expect("start owned PostgreSQL test database");
        let container = GenericImage::new("clickhouse/clickhouse-server", "26.9")
            .with_exposed_port(ContainerPort::Tcp(8123))
            .with_wait_for(WaitFor::http(
                HttpWaitStrategy::new("/ping")
                    .with_port(ContainerPort::Tcp(8123))
                    .with_expected_status_code(200u16),
            ))
            .with_env_var("CLICKHOUSE_DB", &database)
            .with_env_var("CLICKHOUSE_PASSWORD", "test")
            .with_mount(Mount::tmpfs_mount("/var/lib/clickhouse"))
            .with_mount(Mount::tmpfs_mount("/var/log/clickhouse-server"))
            .start()
            .await
            .expect("start isolated ClickHouse");
        let host = container.get_host().await.unwrap();
        let port = container.get_host_port_ipv4(8123).await.unwrap();
        let config = crate::clickhouse::ClickHouseConfig::new(
            format!("http://{host}:{port}"),
            &database,
            "default",
            "test",
        );
        let client = crate::clickhouse::ClickHouseBackend::new(config.clone()).client_clone();
        crate::migrations::apply_migrations(&client).await.unwrap();
        // Historical migration TTL; upgraded installations start here too.
        // Separate pools represent independent console/proxy processes. Only
        // one may observe drift and schedule the initial materialization.
        let second_pool = sea_orm::Database::connect(&pg.database_url).await.unwrap();
        let (first, second) = tokio::join!(
            reconcile_events_ttl(pg.db.as_ref(), &client, 90),
            reconcile_events_ttl(&second_pool, &client, 90),
        );
        assert_eq!(u8::from(first.unwrap()) + u8::from(second.unwrap()), 1);
        second_pool.close().await.unwrap();
        assert!(!sync_events_ttl(&client, 90).await.unwrap());
        let restarted = crate::clickhouse::ClickHouseBackend::new(config).client_clone();
        assert!(!sync_events_ttl(&restarted, 90).await.unwrap());
        // Migration re-runs must not reset an operator's configured window.
        assert!(crate::migrations::apply_migrations(&client)
            .await
            .unwrap()
            .applied
            .is_empty());
        assert!(!sync_events_ttl(&client, 90).await.unwrap());
        // Actual drift, a shortened window, a raised window, and missing TTL.
        client
            .query("ALTER TABLE events MODIFY TTL toDateTime(timestamp) + INTERVAL 5 YEAR")
            .execute()
            .await
            .unwrap();
        assert!(sync_events_ttl(&client, 30).await.unwrap());
        assert!(!sync_events_ttl(&client, 30).await.unwrap());
        assert!(sync_events_ttl(&client, 3650).await.unwrap());
        assert!(sync_events_ttl(&client, 1).await.unwrap());
        assert!(!sync_events_ttl(&client, 1).await.unwrap());
        client
            .query("ALTER TABLE events REMOVE TTL")
            .execute()
            .await
            .unwrap();
        // A failed ALTER must be visible and must not prevent a later retry.
        let denied = client.clone().with_setting("readonly", "1");
        assert!(reconcile_events_ttl(pg.db.as_ref(), &denied, 90)
            .await
            .unwrap_err()
            .to_string()
            .contains("set events TTL"));
        // Failure releases the PostgreSQL lock so a later pass can retry.
        assert!(reconcile_events_ttl(pg.db.as_ref(), &client, 90)
            .await
            .unwrap());
        assert!(!sync_events_ttl(&client, 90).await.unwrap());
        // A matching policy needs no ALTER permission: this also proves a
        // repeated pass does not schedule another TTL rewrite.
        assert!(!sync_events_ttl(&denied, 90).await.unwrap());

        client
            .query(
                "INSERT INTO events (event_id, project_id, visitor_id, timestamp) VALUES \
            (1, 10, 7, now()), (2, 20, 7, now()), (3, 10, 8, now()), (4, 10, NULL, now())",
            )
            .execute()
            .await
            .unwrap();
        assert!(delete_visitor_events(&denied, &[(10, 7)]).await.is_err());
        assert_eq!(
            client
                .query("SELECT count() FROM events")
                .fetch_one::<u64>()
                .await
                .unwrap(),
            4
        );
        delete_visitor_events(&client, &[(10, 7)]).await.unwrap();
        // Same visitor number in another project, another visitor in the same
        // project, and unattributed events all survive.
        let ids = client
            .query("SELECT event_id FROM events ORDER BY event_id")
            .fetch_all::<i64>()
            .await
            .unwrap();
        assert_eq!(ids, vec![2, 3, 4]);
        delete_visitor_events(&client, &[(10, 7)]).await.unwrap();
        assert_eq!(
            client
                .query("SELECT count() FROM events")
                .fetch_one::<u64>()
                .await
                .unwrap(),
            3
        );
        // Current migrations remove identifying rollups as well as their
        // materialized-view inner storage; sessions is no longer provisioned.
        assert_eq!(client.query("SELECT count() FROM system.tables WHERE database = currentDatabase() AND (name = 'events_5m_mv' OR name = 'sessions' OR startsWith(name, '.inner'))").fetch_one::<u64>().await.unwrap(), 0);

        // Expiration is scheduled by CH, not a wall-clock deletion guarantee.
        client
            .query("ALTER TABLE events REMOVE TTL")
            .execute()
            .await
            .unwrap();
        client.query("INSERT INTO events (event_id, project_id, timestamp) VALUES (9, 10, now() - INTERVAL 100 DAY)").execute().await.unwrap();
        assert!(sync_events_ttl(&client, 90).await.unwrap());
        // Force a merge only in this isolated test to verify the expression
        // removes old rows without relying on background scheduling timing.
        client
            .query("OPTIMIZE TABLE events FINAL")
            .execute()
            .await
            .unwrap();
        assert_eq!(
            client
                .query("SELECT count() FROM events WHERE event_id = 9")
                .fetch_one::<u64>()
                .await
                .unwrap(),
            0
        );
    }
}
