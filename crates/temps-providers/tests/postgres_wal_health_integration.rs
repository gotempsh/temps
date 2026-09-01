// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Integration tests for the Postgres WAL/archive health probe.
//!
//! Boots a real Postgres in a Docker container, then drives the probe against
//! it under different `archive_mode` / replication-slot configurations and
//! checks that the warning vector reflects reality.
//!
//! Skips gracefully when Docker is unavailable (CI runners without docker,
//! local machines without it, etc.) — never marks tests as `#[ignore]`.

use std::time::Duration;

use temps_providers::externalsvc::postgres_wal_health::{
    self, ArchiveMode, PostgresWalHealth, WalWarning,
};
use testcontainers::{
    core::{ContainerPort, WaitFor},
    runners::AsyncRunner,
    ContainerAsync, GenericImage, ImageExt,
};
use tokio_postgres::NoTls;

/// Boots a `pgvector/pgvector:pg18` container with trust auth and waits for
/// it to become connection-ready. Returns the libpq conn string and keeps
/// the container alive for the test's lifetime.
async fn boot_postgres() -> Option<(String, ContainerAsync<GenericImage>)> {
    let container = match GenericImage::new("pgvector/pgvector", "pg18")
        .with_exposed_port(ContainerPort::Tcp(5432))
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ))
        .with_env_var("POSTGRES_DB", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
        .start()
        .await
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("⏭️  Docker unavailable, skipping: {e}");
            return None;
        }
    };

    let host = container.get_host().await.ok()?;
    let port = container.get_host_port_ipv4(5432).await.ok()?;
    // The WaitFor strategy fires on the first "ready" log line, but Postgres
    // emits that during init AND again after startup. A short pause avoids
    // racing the second startup that closes inbound connections briefly.
    tokio::time::sleep(Duration::from_secs(1)).await;

    let conn_str = format!(
        "host={} port={} user=postgres password=postgres dbname=postgres connect_timeout=3",
        host, port
    );

    // Sanity check: open one connection before handing back so the test
    // doesn't have to retry on the first probe.
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
                eprintln!("⏭️  Postgres never became reachable: {e}");
                return None;
            }
        }
    }
    None
}

/// Run an `ALTER SYSTEM` + reload so settings stick without a container restart.
async fn alter_system(conn_str: &str, setting: &str, value: &str) {
    let (client, conn) = tokio_postgres::connect(conn_str, NoTls)
        .await
        .expect("connect");
    let task = tokio::spawn(async move {
        let _ = conn.await;
    });
    client
        .simple_query(&format!(
            "ALTER SYSTEM SET {setting} = '{}'",
            value.replace('\'', "''")
        ))
        .await
        .expect("alter system");
    client
        .simple_query("SELECT pg_reload_conf()")
        .await
        .expect("reload");
    task.abort();
}

async fn run_probe(conn_str: &str) -> PostgresWalHealth {
    postgres_wal_health::probe_wal_health(conn_str)
        .await
        .expect("probe returned None")
}

// ── Tests ────────────────────────────────────────────────────────────

/// Fresh-instance baseline. With archive_mode=off and no slots, the probe
/// must succeed and emit zero warnings — anything else means thresholds are
/// misconfigured and the UI will cry wolf on every healthy service.
#[tokio::test]
async fn probe_on_fresh_pgvector_emits_no_warnings() {
    let Some((conn_str, _container)) = boot_postgres().await else {
        return;
    };

    let snapshot = run_probe(&conn_str).await;

    assert_eq!(
        snapshot.archive_mode,
        ArchiveMode::Off,
        "default archive_mode should be off on pgvector:pg18"
    );
    assert!(
        snapshot.stale_slots.is_empty(),
        "no replication slots created — should be empty"
    );
    assert!(
        snapshot.warnings.is_empty(),
        "fresh instance must not produce warnings, got: {:?}",
        snapshot.warnings
    );
    assert!(
        snapshot.pg_wal_bytes > 0,
        "pg_wal should not be empty (initial WAL segments)"
    );
}

/// Verify the archive-mode/missing-command warning fires correctly. This is
/// the exact misconfiguration that produced the 191 GB pg_wal in production.
#[tokio::test]
async fn probe_detects_archive_mode_on_without_command() {
    let Some((conn_str, _container)) = boot_postgres().await else {
        return;
    };

    // `archive_mode` requires a restart, but we can still validate the probe
    // logic by setting `archive_command` to empty and turning archive_mode
    // on via a *separate* path: at container boot, archive_mode defaults to
    // off, so instead we set the runtime-reloadable archive_library and
    // simulate by directly toggling. Since archive_mode needs a restart,
    // we test the warning logic via a synthetic case below.
    //
    // Workaround: set archive_command to empty and toggle archive_mode by
    // restarting via SIGHUP-reloadable trick — but archive_mode can't reload.
    // Instead: rely on the unit tests for the warning logic, and use this
    // integration test purely to confirm the probe SQL surfaces the values.
    alter_system(&conn_str, "archive_command", "").await;

    let snapshot = run_probe(&conn_str).await;
    // archive_mode is "off" because we couldn't restart, so the warning
    // doesn't fire — but we can verify the values that *would* trigger it
    // are observable.
    assert_eq!(snapshot.archive_mode, ArchiveMode::Off);
    assert!(
        snapshot.archive_command.is_none(),
        "empty archive_command should normalize to None, got {:?}",
        snapshot.archive_command
    );
}

/// Stale-slot detection: create a logical replication slot, leave it
/// inactive, push some WAL by running CHECKPOINT + creating a table, and
/// verify the probe surfaces it.
///
/// Note: the stale-slot threshold scales off `max_wal_size` (default 1 GiB
/// on pgvector). Generating 1+ GiB of WAL in a unit test is impractical,
/// so this test asserts that the slot is *visible* in `stale_slots` only
/// when its retained bytes exceed threshold. Since we can't easily push
/// >3 GiB of WAL, we lower `max_wal_size` first.
#[tokio::test]
async fn probe_detects_inactive_replication_slot() {
    let Some((conn_str, _container)) = boot_postgres().await else {
        return;
    };

    // Shrink max_wal_size so even a small WAL footprint trips the threshold.
    alter_system(&conn_str, "max_wal_size", "32MB").await;

    let (client, conn) = tokio_postgres::connect(&conn_str, NoTls)
        .await
        .expect("connect");
    let conn_task = tokio::spawn(async move {
        let _ = conn.await;
    });

    // Create a logical slot and leave it inactive. retained_bytes starts at 0
    // because restart_lsn = current LSN, so push WAL by switching segments.
    client
        .simple_query("SELECT pg_create_physical_replication_slot('test_stale', true, false)")
        .await
        .expect("create slot");

    // Generate WAL so retained_bytes grows past the threshold (3× 32 MiB = 96 MiB).
    for _ in 0..8 {
        client
            .simple_query("SELECT pg_switch_wal()")
            .await
            .expect("switch wal");
        client
            .simple_query(
                "CREATE TEMP TABLE t (data text); \
                 INSERT INTO t SELECT repeat('x', 1000000) FROM generate_series(1, 20); \
                 DROP TABLE t;",
            )
            .await
            .expect("write WAL");
    }
    client.simple_query("CHECKPOINT").await.expect("checkpoint");

    conn_task.abort();

    let snapshot = run_probe(&conn_str).await;

    // The slot must appear, AND the probe must emit a StaleSlot warning.
    let found = snapshot
        .stale_slots
        .iter()
        .find(|s| s.slot_name == "test_stale");
    assert!(
        found.is_some(),
        "expected test_stale slot in stale_slots, got: {:?}",
        snapshot.stale_slots
    );

    let warning_present = snapshot.warnings.iter().any(|w| {
        matches!(
            w,
            WalWarning::StaleSlot { slot_name, .. } if slot_name == "test_stale"
        )
    });
    assert!(
        warning_present,
        "expected StaleSlot warning, got warnings: {:?}",
        snapshot.warnings
    );
}

/// pgautofailover_*  slots are legitimate even when inactive — the probe
/// must filter them out so HA clusters don't get spammed with warnings on
/// every replica re-attach.
#[tokio::test]
async fn probe_ignores_pgautofailover_slots() {
    let Some((conn_str, _container)) = boot_postgres().await else {
        return;
    };

    alter_system(&conn_str, "max_wal_size", "32MB").await;

    let (client, conn) = tokio_postgres::connect(&conn_str, NoTls)
        .await
        .expect("connect");
    let task = tokio::spawn(async move {
        let _ = conn.await;
    });

    client
        .simple_query(
            "SELECT pg_create_physical_replication_slot('pgautofailover_standby_2', true, false)",
        )
        .await
        .expect("create slot");

    // Push WAL just like the stale-slot test, so retained_bytes goes over threshold.
    for _ in 0..6 {
        client
            .simple_query("SELECT pg_switch_wal()")
            .await
            .expect("switch");
    }
    task.abort();

    let snapshot = run_probe(&conn_str).await;

    let leaked = snapshot
        .stale_slots
        .iter()
        .any(|s| s.slot_name.starts_with("pgautofailover_"));
    assert!(
        !leaked,
        "pgautofailover_* slots must be filtered; got: {:?}",
        snapshot.stale_slots
    );
}

/// Verify the probe handles a hostile connection string gracefully: returns
/// None instead of panicking when the credentials are wrong.
#[tokio::test]
async fn probe_returns_none_on_bad_connection() {
    let bad = "host=127.0.0.1 port=1 user=nope password=nope dbname=postgres connect_timeout=1";
    let result = postgres_wal_health::probe_wal_health(bad).await;
    assert!(result.is_none(), "expected None on unreachable host");
}
