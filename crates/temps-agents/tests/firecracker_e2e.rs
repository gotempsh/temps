//! End-to-end lifecycle test for the Firecracker sandbox backend (ADR-029).
//!
//! Boots real microVMs, so it needs a provisioned host: KVM access,
//! `temps firecracker setup` completed, and the musl `temps-vm-agent` at
//! `<data_dir>/firecracker/bin/temps-vm-agent`. Gated behind
//! `TEMPS_FC_E2E=1` (plus `TEMPS_DATA_DIR`) so plain `cargo test` skips it
//! — same pattern as the Docker-gated eval harness.
//!
//!   TEMPS_FC_E2E=1 TEMPS_DATA_DIR=~/.temps-fc-test \
//!     cargo test -p temps-agents --test firecracker_e2e -- --nocapture
//!
//! Everything goes through `Arc<dyn SandboxProvider>` on the routing
//! provider — the exact seam every consumer uses.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use temps_agents::sandbox::firecracker::{FirecrackerSandboxConfig, FirecrackerSandboxProvider};
use temps_agents::sandbox::routing::RoutingSandboxProvider;
use temps_agents::sandbox::{SandboxBackend, SandboxCreateConfig, SandboxProvider};

fn gated() -> Option<PathBuf> {
    if std::env::var("TEMPS_FC_E2E").as_deref() != Ok("1") {
        eprintln!("skipping: set TEMPS_FC_E2E=1 (and TEMPS_DATA_DIR) to run");
        return None;
    }
    let data_dir = std::env::var("TEMPS_DATA_DIR").expect("TEMPS_FC_E2E=1 requires TEMPS_DATA_DIR");
    Some(PathBuf::from(data_dir))
}

fn create_config(run_id: i32) -> SandboxCreateConfig {
    SandboxCreateConfig {
        run_id,
        container_name_override: Some(format!("e2e{}", run_id)),
        host_work_dir: PathBuf::from("/tmp"),
        workspace_volume: None,
        image: None, // provider default (alpine)
        cpu_limit: Some(1.0),
        memory_limit_mb: Some(256),
        pids_limit: None,
        disk_size_mb: None,
        network_mode: None, // default = full (TAP + NAT)
        env_vars: HashMap::from([("SANDBOX_GREETING".to_string(), "from-env".to_string())]),
        idle_timeout: Duration::from_secs(300),
        backend: Some(SandboxBackend::Firecracker),
    }
}

#[tokio::test]
async fn firecracker_sandbox_full_lifecycle() {
    let Some(data_dir) = gated() else { return };

    let docker =
        Arc::new(bollard::Docker::connect_with_local_defaults().expect("docker (image toolchain)"));
    let firecracker: Arc<dyn SandboxProvider> = Arc::new(FirecrackerSandboxProvider::new(
        FirecrackerSandboxConfig::from_data_dir(data_dir),
        docker,
    ));
    assert!(
        firecracker.is_available().await,
        "backend not provisioned — run `temps firecracker setup` first"
    );

    // Consumers hold the routing provider; exercise that seam, not the
    // concrete type.
    let provider: Arc<dyn SandboxProvider> = Arc::new(RoutingSandboxProvider::new(
        HashMap::from([(SandboxBackend::Firecracker, firecracker)]),
        SandboxBackend::Firecracker,
    ));

    // ── create ──
    let started = std::time::Instant::now();
    let handle = provider.create(create_config(9001)).await.expect("create");
    println!(
        "created {} in {:.2}s",
        handle.sandbox_name,
        started.elapsed().as_secs_f64()
    );
    assert!(handle.sandbox_name.starts_with("temps-fcsandbox-"));
    assert!(provider.is_alive(&handle).await.expect("is_alive"));

    // ── exec: real userspace, isolated kernel, injected env ──
    let result = provider
        .exec(
            &handle,
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "uname -sr && echo greeting=$SANDBOX_GREETING && pwd".to_string(),
            ],
            HashMap::new(),
            None,
        )
        .await
        .expect("exec");
    println!("exec stdout: {}", result.stdout.trim());
    assert_eq!(result.exit_code, 0);
    assert!(
        result.stdout.contains("Linux 6.1.141"),
        "runs the guest kernel"
    );
    assert!(
        result.stdout.contains("greeting=from-env"),
        "create-time env injected"
    );
    assert!(
        result.stdout.contains("/workspace"),
        "work_dir is the exec cwd"
    );

    // ── egress: DNS resolution + HTTP through the TAP/NAT path ──
    let result = provider
        .exec(
            &handle,
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "wget -q -T 10 -O - http://detectportal.firefox.com/success.txt".to_string(),
            ],
            HashMap::new(),
            None,
        )
        .await
        .expect("exec egress check");
    assert_eq!(result.exit_code, 0, "egress failed: {}", result.stderr);
    assert!(
        result.stdout.contains("success"),
        "expected portal-check body"
    );

    // ── exec: stderr split + exit code ──
    let result = provider
        .exec(
            &handle,
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo to-out && echo to-err >&2 && exit 3".to_string(),
            ],
            HashMap::new(),
            None,
        )
        .await
        .expect("exec");
    assert_eq!(result.exit_code, 3);
    assert_eq!(result.stdout.trim(), "to-out");
    assert_eq!(result.stderr.trim(), "to-err");

    // ── file roundtrip (binary-safe) ──
    let payload: Vec<u8> = (0u16..=255).map(|b| b as u8).collect();
    provider
        .write_file(&handle, "/workspace/bin.dat", &payload, 0o600)
        .await
        .expect("write_file");
    let read_back = provider
        .read_file(&handle, "/workspace/bin.dat")
        .await
        .expect("read_file");
    assert_eq!(read_back, payload, "binary roundtrip");
    let missing = provider.read_file(&handle, "/workspace/nope").await;
    assert!(missing.is_err(), "missing file is an error");

    // ── stop → filesystem persists → start ──
    provider.stop(&handle).await.expect("stop");
    assert!(!provider
        .is_alive(&handle)
        .await
        .expect("is_alive after stop"));
    let recovered = provider
        .recover_by_name(&handle.sandbox_name)
        .await
        .expect("recover_by_name")
        .expect("stopped sandbox is recoverable");
    provider.start(&recovered).await.expect("start");
    assert!(provider
        .is_alive(&recovered)
        .await
        .expect("is_alive after start"));
    let result = provider
        .exec(
            &recovered,
            vec!["cat".to_string(), "/workspace/bin.dat".to_string()],
            HashMap::new(),
            None,
        )
        .await
        .expect("exec after restart");
    assert_eq!(result.exit_code, 0, "file survived stop/start");

    // ── destroy ──
    provider.destroy(&recovered, true).await.expect("destroy");
    assert!(
        provider
            .recover_by_name(&recovered.sandbox_name)
            .await
            .expect("recover after destroy")
            .is_none(),
        "destroyed sandbox is gone"
    );
    println!("full lifecycle OK");
}
