// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Kernel integration tests for the WireGuard mesh lockdown.
//!
//! Each test drives the real `temps_network::mesh` entry points against the
//! host's nftables and asserts the state the kernel reports. They are run one
//! at a time by `tests/mesh_lockdown/run.sh`, which also generates real
//! traffic from a second node over a real tunnel between the tests and checks
//! what the lockdown lets through. Run directly they only check the
//! install/inspect/repair cycle.
//!
//! Every test builds the state it needs from scratch (and the table is shared
//! by the whole host), so they pass in any order, alone or together; a lock
//! serialises them because the default test runner is multi-threaded.
//!
//! Gated like the other kernel tests: `--features integration_kernel`, Linux
//! only, and `TEMPS_RUN_DIND_TESTS=1`.

#![cfg(all(feature = "integration_kernel", target_os = "linux"))]

use ipnet::Ipv4Net;
use temps_network::mesh::{ensure_lockdown, lockdown_state, LockdownState, MeshLockdown};

fn enabled() -> bool {
    std::env::var("TEMPS_RUN_DIND_TESTS").as_deref() == Ok("1")
}

fn lockdown(node_api_port: Option<u16>, relay: bool) -> MeshLockdown {
    MeshLockdown {
        vxlan_port: 4789,
        mesh: "10.201.0.0/16".parse::<Ipv4Net>().unwrap(),
        node_api_port,
        agent_api: None,
        relay,
    }
}

/// The lockdown is one host-wide table: tests must not interleave.
static HOST_TABLE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Start from no lockdown at all, whatever an earlier test left behind.
fn remove_table() {
    // Absent is fine: the point is only that it is gone afterwards.
    let _ = nft(&["delete", "table", "inet", "temps_mesh"]);
}

fn nft(args: &[&str]) -> std::process::Output {
    std::process::Command::new("nft")
        .args(args)
        .output()
        .expect("nft is on PATH in the harness")
}

#[tokio::test]
async fn lockdown_installs_once_and_is_then_current() {
    if !enabled() {
        return;
    }
    let _host = HOST_TABLE.lock().await;
    remove_table();

    let wanted = lockdown(None, false);
    assert_eq!(
        lockdown_state(&wanted).await.unwrap(),
        LockdownState::Missing
    );
    assert!(ensure_lockdown(&wanted).await.unwrap(), "first install");
    assert_eq!(
        lockdown_state(&wanted).await.unwrap(),
        LockdownState::Current
    );
    assert!(
        !ensure_lockdown(&wanted).await.unwrap(),
        "a current lockdown is not rewritten"
    );
}

#[tokio::test]
async fn a_changed_setting_replaces_the_table_atomically() {
    if !enabled() {
        return;
    }
    let _host = HOST_TABLE.lock().await;
    remove_table();

    let before = lockdown(None, false);
    let after = lockdown(Some(9999), false);
    assert!(ensure_lockdown(&before).await.unwrap());
    assert_eq!(
        lockdown_state(&after).await.unwrap(),
        LockdownState::Outdated,
        "an installed lockdown for other settings is outdated, not current"
    );
    assert!(ensure_lockdown(&after).await.unwrap());
    assert_eq!(
        lockdown_state(&after).await.unwrap(),
        LockdownState::Current
    );
    assert_eq!(
        lockdown_state(&before).await.unwrap(),
        LockdownState::Outdated
    );
}

#[tokio::test]
async fn a_flushed_table_is_repaired() {
    if !enabled() {
        return;
    }
    let _host = HOST_TABLE.lock().await;
    remove_table();

    let wanted = lockdown(Some(9999), false);
    assert!(ensure_lockdown(&wanted).await.unwrap());
    let flushed = nft(&["flush", "table", "inet", "temps_mesh"]);
    assert!(
        flushed.status.success(),
        "flush failed: {}",
        String::from_utf8_lossy(&flushed.stderr)
    );
    // The table exists but its rules, marker included, are gone.
    assert_ne!(
        lockdown_state(&wanted).await.unwrap(),
        LockdownState::Current
    );
    assert!(ensure_lockdown(&wanted).await.unwrap());
    assert_eq!(
        lockdown_state(&wanted).await.unwrap(),
        LockdownState::Current
    );
}

#[tokio::test]
async fn a_deleted_table_is_reinstalled() {
    if !enabled() {
        return;
    }
    let _host = HOST_TABLE.lock().await;
    remove_table();

    let wanted = lockdown(Some(9999), false);
    assert!(ensure_lockdown(&wanted).await.unwrap());
    let deleted = nft(&["delete", "table", "inet", "temps_mesh"]);
    assert!(
        deleted.status.success(),
        "delete failed: {}",
        String::from_utf8_lossy(&deleted.stderr)
    );
    assert_eq!(
        lockdown_state(&wanted).await.unwrap(),
        LockdownState::Missing
    );
    assert!(ensure_lockdown(&wanted).await.unwrap());
    assert_eq!(
        lockdown_state(&wanted).await.unwrap(),
        LockdownState::Current
    );
}
