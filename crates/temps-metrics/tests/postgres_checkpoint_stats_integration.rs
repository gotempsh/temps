//! Regression test for the PG17+ checkpoint-stats query in
//! [`temps_metrics::PostgresCollector`].
//!
//! On PG17+, `pg_stat_bgwriter.checkpoints_timed`/`checkpoints_req` were
//! dropped in favor of `pg_stat_checkpointer.num_timed`/`num_requested`. A
//! prior version of the collector tried to bridge both schemas with a single
//! `SELECT ... FROM pg_stat_checkpointer UNION ALL SELECT ... FROM
//! pg_stat_bgwriter WHERE NOT EXISTS (...)` query. Postgres resolves column
//! references in *every* arm of a `UNION` at parse time, regardless of which
//! arm `WHERE NOT EXISTS` would select at runtime — so on PG17+, where the
//! `pg_stat_bgwriter` arm references now-dropped columns, the whole
//! statement failed to parse with `column "checkpoints_timed" does not
//! exist`. In production this spammed logs every scrape cycle and silently
//! dropped `pg.checkpoints_timed_total`/`pg.checkpoints_req_total`/
//! `pg.checkpoint_rate` from dashboards.
//!
//! These tests boot real Postgres containers on both sides of the PG17
//! schema split and assert `collect()` succeeds and returns the checkpoint
//! counters either way.
//!
//! Skips gracefully when Docker is unavailable (CI runners without docker,
//! local machines without it, etc.) — never marks tests as `#[ignore]`.

use std::time::Duration;

use temps_metrics::{Collector, CollectorConfig, PostgresCollector, SourceKind};
use testcontainers::{
    core::{ContainerPort, WaitFor},
    runners::AsyncRunner,
    ContainerAsync, GenericImage, ImageExt,
};
use tokio_postgres::NoTls;

/// Boots an official `postgres:<tag>` container with trust auth and waits
/// for it to become connection-ready. Returns the libpq conn string and
/// keeps the container alive for the test's lifetime.
async fn boot_postgres(tag: &str) -> Option<(String, ContainerAsync<GenericImage>)> {
    let container = match GenericImage::new("postgres", tag)
        .with_exposed_port(ContainerPort::Tcp(5432))
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ))
        .with_env_var("POSTGRES_DB", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
        .start()
        .await
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skipping (Docker unavailable): {e}");
            return None;
        }
    };

    let host = container.get_host().await.ok()?;
    let port = container.get_host_port_ipv4(5432).await.ok()?;
    // The WaitFor strategy fires on the first "ready" log line, but Postgres
    // emits that during init AND again after startup. A short pause avoids
    // racing the second startup that closes inbound connections briefly.
    tokio::time::sleep(Duration::from_secs(1)).await;

    let conn_str =
        format!("host={host} port={port} user=postgres dbname=postgres connect_timeout=3");

    for attempt in 0..10 {
        match tokio_postgres::connect(&conn_str, NoTls).await {
            Ok((client, connection)) => {
                let task = tokio::spawn(async move {
                    let _ = connection.await;
                });
                drop(client);
                task.abort();
                return Some((conn_str, container));
            }
            Err(_) if attempt < 9 => {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(e) => {
                eprintln!("Postgres never became reachable: {e}");
                return None;
            }
        }
    }
    None
}

fn checkpoint_config(conn_str: &str) -> CollectorConfig {
    CollectorConfig::new(1, SourceKind::Database, conn_str).with_timeout(Duration::from_secs(10))
}

/// The exact scenario that broke in production: PG17+ has
/// `pg_stat_checkpointer` and no longer has
/// `pg_stat_bgwriter.checkpoints_timed`/`checkpoints_req`.
#[tokio::test]
async fn collect_on_pg17_returns_checkpoint_metrics_without_error() {
    let Some((conn_str, _container)) = boot_postgres("17-alpine").await else {
        return;
    };

    let collector = PostgresCollector::new();
    let points = collector
        .collect(&checkpoint_config(&conn_str))
        .await
        .expect("collect() must not error on PG17");

    for name in [
        "pg.checkpoints_timed_total",
        "pg.checkpoints_req_total",
        "pg.checkpoint_rate",
    ] {
        let found = points.iter().find(|p| p.name == name);
        assert!(
            found.is_some(),
            "expected metric {name} in points, got: {:?}",
            points.iter().map(|p| &p.name).collect::<Vec<_>>()
        );
        assert!(
            found.unwrap().value >= 0.0,
            "{name} must be non-negative, got {}",
            found.unwrap().value
        );
    }
}

/// Pre-PG17 fallback path: `pg_stat_checkpointer` doesn't exist, so the
/// collector must fall back to `pg_stat_bgwriter.checkpoints_timed`/
/// `checkpoints_req` and still succeed.
#[tokio::test]
async fn collect_on_pg16_falls_back_to_bgwriter_checkpoint_columns() {
    let Some((conn_str, _container)) = boot_postgres("16-alpine").await else {
        return;
    };

    let collector = PostgresCollector::new();
    let points = collector
        .collect(&checkpoint_config(&conn_str))
        .await
        .expect("collect() must not error on PG16");

    for name in [
        "pg.checkpoints_timed_total",
        "pg.checkpoints_req_total",
        "pg.checkpoint_rate",
    ] {
        assert!(
            points.iter().any(|p| p.name == name),
            "expected metric {name} in points on PG16 fallback path, got: {:?}",
            points.iter().map(|p| &p.name).collect::<Vec<_>>()
        );
    }
}
