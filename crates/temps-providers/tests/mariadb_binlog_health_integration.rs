//! Integration tests for the MariaDB binary-log health probe.
//!
//! Boots a real MariaDB in a Docker container, then drives the probe against
//! it under different binlog configurations and checks that the warning vector
//! reflects reality.
//!
//! Skips gracefully when Docker is unavailable (CI runners without docker,
//! local machines without it, etc.) — never marks tests as `#[ignore]`.

use std::time::Duration;

use sqlx::mysql::MySqlPoolOptions;
use temps_providers::externalsvc::mariadb_binlog_health::{self, BinlogWarning};
use testcontainers::{
    core::{ContainerPort, WaitFor},
    runners::AsyncRunner,
    ContainerAsync, GenericImage, ImageExt,
};

const ROOT_PASSWORD: &str = "probe-root-pw";

/// Boots a `mariadb:lts` container and waits for it to become
/// connection-ready. Returns the `mysql://` conn string and keeps the
/// container alive for the test's lifetime. `extra_cmd` is appended as the
/// container command so callers can enable binlog (`--log-bin=...` etc.).
///
/// Returns `None` when Docker is unavailable so tests skip rather than fail.
async fn boot_mariadb(extra_cmd: &[&str]) -> Option<(String, ContainerAsync<GenericImage>)> {
    let mut image = GenericImage::new("mariadb", "lts")
        .with_exposed_port(ContainerPort::Tcp(3306))
        .with_wait_for(WaitFor::message_on_stderr("ready for connections"))
        .with_env_var("MARIADB_ROOT_PASSWORD", ROOT_PASSWORD)
        .with_env_var("MARIADB_DATABASE", "appdb");

    if !extra_cmd.is_empty() {
        image = image.with_cmd(extra_cmd.iter().map(|s| s.to_string()));
    }

    let container = match image.start().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("⏭️  Docker unavailable, skipping: {e}");
            return None;
        }
    };

    let host = container.get_host().await.ok()?;
    let port = container.get_host_port_ipv4(3306).await.ok()?;

    // MariaDB logs "ready for connections" during init AND after final
    // startup; a short pause avoids racing the restart that briefly closes
    // inbound connections.
    tokio::time::sleep(Duration::from_secs(1)).await;

    let conn_str = format!("mysql://root:{ROOT_PASSWORD}@{host}:{port}/");

    // Sanity check: open one connection before handing back so the test
    // doesn't have to retry on the first probe.
    for attempt in 0..20 {
        match MySqlPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(3))
            .connect(&conn_str)
            .await
        {
            Ok(pool) => {
                pool.close().await;
                return Some((conn_str, container));
            }
            Err(_) if attempt < 19 => {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(e) => {
                eprintln!("⏭️  MariaDB never became reachable: {e}");
                return None;
            }
        }
    }
    None
}

// ── Tests ────────────────────────────────────────────────────────────

/// Stock `mariadb:lts` ships with binary logging OFF by default. The probe
/// must connect, observe `log_bin == false`, and emit `BinlogDisabled`.
#[tokio::test]
async fn probe_detects_binlog_disabled() {
    let Some((conn_str, _container)) = boot_mariadb(&[]).await else {
        return;
    };

    let snapshot = mariadb_binlog_health::probe_binlog_health(&conn_str)
        .await
        .expect("probe returned None against a reachable MariaDB");

    assert!(
        !snapshot.log_bin,
        "stock mariadb:lts should have binlog disabled, got log_bin={}",
        snapshot.log_bin
    );
    assert!(
        snapshot
            .warnings
            .iter()
            .any(|w| matches!(w, BinlogWarning::BinlogDisabled)),
        "expected BinlogDisabled warning, got: {:?}",
        snapshot.warnings
    );
}

/// With binlog explicitly enabled in ROW format, the probe must observe
/// `log_bin == true` and emit NO `BinlogDisabled` warning (and no
/// non-ROW-format warning either, since we asked for ROW).
#[tokio::test]
async fn probe_emits_no_disabled_warning_when_binlog_on() {
    let Some((conn_str, _container)) = boot_mariadb(&[
        "--log-bin=mysql-bin",
        "--server-id=1",
        "--binlog-format=ROW",
    ])
    .await
    else {
        return;
    };

    let snapshot = mariadb_binlog_health::probe_binlog_health(&conn_str)
        .await
        .expect("probe returned None against a reachable MariaDB");

    assert!(
        snapshot.log_bin,
        "binlog should be ON with --log-bin set, got log_bin={}",
        snapshot.log_bin
    );
    assert!(
        !snapshot
            .warnings
            .iter()
            .any(|w| matches!(w, BinlogWarning::BinlogDisabled)),
        "did not expect BinlogDisabled warning with binlog on, got: {:?}",
        snapshot.warnings
    );
    assert!(
        !snapshot
            .warnings
            .iter()
            .any(|w| matches!(w, BinlogWarning::NonRowBinlogFormat { .. })),
        "did not expect NonRowBinlogFormat warning with ROW format, got: {:?}",
        snapshot.warnings
    );
}

/// The probe must handle an unreachable server gracefully: return None
/// instead of panicking or erroring.
#[tokio::test]
async fn probe_returns_none_on_bad_connection() {
    let result = mariadb_binlog_health::probe_binlog_health("mysql://root:x@127.0.0.1:1/").await;
    assert!(result.is_none(), "expected None on unreachable host");
}
