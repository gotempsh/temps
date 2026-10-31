// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sandbox driver eval harness (ADR-010, ADR-055 §6).
//!
//! One contract, run against every sandbox driver through the driver facade
//! that serves every sandbox (`DriverSandboxProvider` over a
//! `SandboxBackendDriver`), so each backend is proven behind the same
//! `SandboxProvider` seam its callers hold:
//!
//! - capabilities are filters: a request needing a feature the driver lacks
//!   is refused before anything is created, with the missing feature named;
//!   a requested limit the driver cannot apply still creates the sandbox and
//!   says so (`LimitEnforced = False` with the reason);
//! - create, liveness, exec (output and exit codes), file write/read,
//!   stop/start and destroy behave the same on every driver;
//! - an optional operation the driver does not offer fails with a named
//!   error, never a silent success.
//!
//! No model or provider credentials are involved: the contract is the
//! sandbox runtime itself.
//!
//! Each driver skips at runtime, printing why, when its runtime is missing:
//! - Docker needs a reachable daemon and a local sandbox image (it never
//!   pulls one). The sandboxes run with `network_mode = none`, so no Docker
//!   network is created.
//! - microsandbox needs a hypervisor and the pinned runtime under the
//!   short test home `/tmp/temps-msb-e2e-$USER` (see
//!   `netpol_microsandbox_e2e.rs`); this harness links that runtime into its
//!   own data dir so its VM state stays separate. Run with
//!   `--test-threads=1`.
//! - Firecracker needs Linux with `/dev/kvm` and `TEMPS_FC_E2E=1` plus
//!   `TEMPS_DATA_DIR` (see `firecracker_e2e.rs`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use temps_agents::error::AgentError;
use temps_agents::sandbox::driver::{capabilities_for, SandboxBackendDriver};
use temps_agents::sandbox::facade::DriverSandboxProvider;
use temps_agents::sandbox::{SandboxBackend, SandboxCreateConfig, SandboxProvider};
use temps_workload::{ConditionKind, ConditionStatus, SandboxExt};

/// Sandbox images the Docker run may use, best first. Only local images are
/// used: the harness never pulls a multi-gigabyte image on a loaded host.
const DOCKER_IMAGES: &[&str] = &[
    "ghcr.io/gotempsh/temps-sandbox-node:0.1.0",
    "ghcr.io/gotempsh/temps-sandbox-nodejs:0.3.4-beta",
];

/// microVM drivers boot a kernel each; one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    format!("{}{:08x}", std::process::id(), nanos)
}

/// A host work dir the container runtime can bind: under the target dir,
/// which a Docker VM on macOS shares (the system temp dir it does not).
fn host_work_dir(label: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("sandbox-driver-eval")
        .join(label);
    std::fs::create_dir_all(&dir).expect("create the eval work dir");
    dir
}

fn create_config(
    backend: SandboxBackend,
    label: &str,
    image: Option<String>,
) -> SandboxCreateConfig {
    SandboxCreateConfig {
        node_id: None,
        owner_user_id: None,
        run_id: 0,
        container_name_override: Some(label.to_string()),
        host_work_dir: host_work_dir(label),
        workspace_volume: None,
        image,
        cpu_limit: Some(1.0),
        memory_limit_mb: Some(512),
        pids_limit: None,
        // Asked of every driver: the ones that cannot apply it must say so.
        disk_size_mb: Some(2048),
        allow_docker: false,
        egress_policy: Default::default(),
        network_mode: Some("none".to_string()),
        env_vars: HashMap::from([("EVAL_GREETING".to_string(), "from-env".to_string())]),
        idle_timeout: Duration::from_secs(600),
        backend: Some(backend),
    }
}

/// The facade the server builds, over one driver.
fn facade(backend: SandboxBackend, inner: Arc<dyn SandboxProvider>) -> Arc<DriverSandboxProvider> {
    let driver: Arc<dyn SandboxExt> = Arc::new(SandboxBackendDriver::new(backend, inner.clone()));
    Arc::new(DriverSandboxProvider::new(vec![driver], backend, inner))
}

fn show(result: &temps_agents::sandbox::SandboxExecResult) -> String {
    format!(
        "exit {} stdout {:?} stderr {:?}",
        result.exit_code, result.stdout, result.stderr
    )
}

fn sh(script: &str) -> Vec<String> {
    vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()]
}

/// What a contract run proved, printed as the summary line.
struct Proven {
    checks: Vec<&'static str>,
}

impl Proven {
    fn check(&mut self, name: &'static str) {
        println!("  ok: {name}");
        self.checks.push(name);
    }
}

/// The contract. Every assertion goes through `provider`, the facade.
async fn contract(
    backend: SandboxBackend,
    provider: Arc<DriverSandboxProvider>,
    config: impl Fn() -> SandboxCreateConfig,
) -> Proven {
    let (_, caps) = capabilities_for(backend);
    let mut proven = Proven { checks: Vec::new() };

    // ── Capabilities are filters: refused before anything is created ──
    let mut ineligible = config();
    let missing = if !caps.guest_docker {
        ineligible.allow_docker = true;
        "guest docker"
    } else {
        ineligible.workspace_volume = Some(format!("eval-{}", suffix()));
        "workspace volume"
    };
    match provider.create(ineligible).await {
        Err(AgentError::Validation { message }) => {
            let lower = message.to_lowercase().replace(['_', '-'], " ");
            assert!(
                lower.contains(missing),
                "{backend}: the refusal names '{missing}': {message}"
            );
        }
        Err(other) => panic!("{backend}: expected a validation refusal, got {other}"),
        Ok(handle) => {
            let _ = provider.destroy(&handle, true).await;
            panic!("{backend}: a request needing {missing} was placed on a driver without it");
        }
    }
    proven.check("ineligible request refused, missing feature named");

    // ── create, with a limit only some drivers apply ──
    let plan = provider.plan_create(&config()).expect("plan the create");
    assert_eq!(plan.backend, backend, "placed on the requested driver");
    let handle = provider.create(config()).await.expect("create");
    let cleanup = handle.clone();
    let body = async {
        assert_eq!(handle.backend, backend);
        let unenforced = handle
            .conditions
            .iter()
            .find(|c| c.kind == ConditionKind::LimitEnforced && c.status == ConditionStatus::False);
        let (driver_caps, _) = capabilities_for(backend);
        if driver_caps.limits.disk {
            assert!(
                unenforced.is_none(),
                "{backend} applies disk limits: {:?}",
                handle.conditions
            );
        } else {
            let condition = unenforced.unwrap_or_else(|| {
                panic!(
                    "{backend} cannot apply a disk limit and must say so: {:?}",
                    handle.conditions
                )
            });
            assert!(
                condition.message.to_lowercase().contains("disk"),
                "{backend}: the reason names the disk limit: {}",
                condition.message
            );
        }
        let mut proven = Proven { checks: Vec::new() };
        proven.check("create; LimitEnforced reflects the driver's limits");

        assert!(provider.is_alive(&handle).await.expect("is_alive"));
        proven.check("alive after create");

        // ── exec: output, env, exit codes ──
        let result = provider
            .exec(
                &handle,
                sh("echo eval-$((6*7)) greeting=$EVAL_GREETING"),
                HashMap::new(),
                None,
            )
            .await
            .expect("exec");
        assert_eq!(result.exit_code, 0, "{backend}: {}", show(&result));
        assert!(
            result.stdout.contains("eval-42"),
            "{backend}: {}",
            show(&result)
        );
        assert!(
            result.stdout.contains("greeting=from-env"),
            "{backend}: create-time env reaches execs: {}",
            show(&result)
        );
        let failed = provider
            .exec(
                &handle,
                sh("echo to-stderr >&2; exit 3"),
                HashMap::new(),
                None,
            )
            .await
            .expect("exec a failing command");
        assert_eq!(failed.exit_code, 3, "{backend}: {}", show(&failed));
        assert!(
            failed.stderr.contains("to-stderr"),
            "{backend}: {}",
            show(&failed)
        );
        proven.check("exec: stdout, env, stderr and exit codes");

        // ── file access ──
        let path = "/tmp/temps-driver-eval.txt";
        let payload = b"line one\nline two \xe2\x9c\x93\n".to_vec();
        provider
            .write_file(&handle, path, &payload, 0o644)
            .await
            .expect("write_file");
        assert_eq!(
            provider.read_file(&handle, path).await.expect("read_file"),
            payload,
            "{backend}: bytes round-trip"
        );
        let cat = provider
            .exec(
                &handle,
                sh(&format!("wc -l < {path}")),
                HashMap::new(),
                None,
            )
            .await
            .expect("exec reads the written file");
        assert_eq!(cat.stdout.trim(), "2", "{backend}: {}", show(&cat));
        proven.check("file write/read round-trip, visible to execs");

        // ── an optional operation the driver lacks fails by name ──
        if !caps.resize_disk {
            let err = provider
                .resize_disk(&handle, 4096)
                .await
                .expect_err("resize refused");
            assert!(
                err.to_string().to_lowercase().contains("resize"),
                "{backend}: {err}"
            );
            proven.check("resize_disk refused by name (not offered)");
        }
        if !caps.snapshot {
            let err = provider
                .take_snapshot(&handle, None, 1024 * 1024 * 1024)
                .await
                .expect_err("snapshot refused");
            assert!(
                err.to_string().to_lowercase().contains("snapshot"),
                "{backend}: {err}"
            );
            proven.check("take_snapshot refused by name (not offered)");
        }

        // ── stop / start ──
        provider.stop(&handle).await.expect("stop");
        assert!(
            !provider
                .is_alive(&handle)
                .await
                .expect("is_alive after stop"),
            "{backend}: stopped"
        );
        provider.start(&handle).await.expect("start");
        assert!(provider
            .is_alive(&handle)
            .await
            .expect("is_alive after start"));
        let after = provider
            .exec(&handle, sh("echo back"), HashMap::new(), None)
            .await
            .expect("exec after start");
        assert!(after.stdout.contains("back"), "{backend}: {}", show(&after));
        proven.check("stop, start, exec after restart");

        proven
    };
    let outcome = std::panic::AssertUnwindSafe(body).catch_unwind().await;
    let destroyed = provider.destroy(&cleanup, true).await;
    let checks = match outcome {
        Ok(checks) => checks,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    destroyed.expect("destroy");
    match provider.is_alive(&cleanup).await {
        Ok(alive) => assert!(!alive, "{backend}: gone after destroy"),
        Err(AgentError::SandboxNotFound { .. }) => {}
        Err(other) => panic!("{backend}: liveness after destroy failed: {other}"),
    }
    proven.checks.extend(checks.checks);
    proven.check("destroy");
    proven
}

fn summary(backend: SandboxBackend, proven: &Proven) {
    println!(
        "sandbox driver eval: {backend} passed {} checks through the enforce facade",
        proven.checks.len()
    );
}

// ── Docker ──────────────────────────────────────────────────────────────

async fn docker_or_skip() -> Option<(Arc<bollard::Docker>, String)> {
    let docker = match bollard::Docker::connect_with_defaults() {
        Ok(docker) => docker,
        Err(e) => {
            println!("Docker not available, skipping the docker driver eval: {e}");
            return None;
        }
    };
    if let Err(e) = docker.ping().await {
        println!("Docker daemon not responding, skipping the docker driver eval: {e}");
        return None;
    }
    for image in DOCKER_IMAGES {
        if docker.inspect_image(image).await.is_ok() {
            return Some((Arc::new(docker), (*image).to_string()));
        }
    }
    println!(
        "no local sandbox image ({}), skipping the docker driver eval; the harness never pulls one",
        DOCKER_IMAGES.join(", ")
    );
    None
}

#[tokio::test]
async fn docker_driver_passes_the_sandbox_contract() {
    let Some((docker, image)) = docker_or_skip().await else {
        return;
    };
    let _serial = SERIAL.lock().await;
    let inner: Arc<dyn SandboxProvider> =
        Arc::new(temps_agents::sandbox::docker::DockerSandboxProvider::new(
            docker,
            temps_agents::sandbox::docker::DockerSandboxConfig {
                runtime: "custom".to_string(),
                custom_image: image.clone(),
                network_mode: "none".to_string(),
                ..Default::default()
            },
        ));
    let label = format!("p6eval-{}", suffix());
    println!("docker driver eval: image {image}, sandbox temps-sandbox-{label}");
    let proven = contract(
        SandboxBackend::Docker,
        facade(SandboxBackend::Docker, inner),
        || create_config(SandboxBackend::Docker, &label, Some(image.clone())),
    )
    .await;
    summary(SandboxBackend::Docker, &proven);
}

// ── microsandbox ────────────────────────────────────────────────────────

/// The short runtime home shared with `netpol_microsandbox_e2e.rs`.
fn shared_msb_home() -> PathBuf {
    let user = std::env::var("USER").unwrap_or_else(|_| "user".to_string());
    PathBuf::from("/tmp").join(format!("temps-msb-e2e-{user}"))
}

/// A data dir whose `microsandbox` home links the shared runtime (`bin`,
/// `lib`) and image cache, with its own database and VM state. Short, so
/// the runtime's Unix socket paths stay under macOS's 104-byte cap.
fn msb_data_dir() -> Result<PathBuf, String> {
    let shared = shared_msb_home();
    if !shared.join("bin").join("msb").exists() {
        return Err(format!(
            "no microsandbox runtime under {} (install it with \
             TEMPS_E2E_MICROSANDBOX_INSTALL=1 cargo test -p temps-agents --test \
             netpol_microsandbox_e2e)",
            shared.display()
        ));
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "user".to_string());
    let data_dir = PathBuf::from("/tmp").join(format!("tmsb6-{user}"));
    let home = data_dir.join("microsandbox");
    std::fs::create_dir_all(&home).map_err(|e| format!("create {}: {e}", home.display()))?;
    for entry in ["bin", "lib", "cache"] {
        let link = home.join(entry);
        if link.symlink_metadata().is_err() {
            std::os::unix::fs::symlink(shared.join(entry), &link)
                .map_err(|e| format!("link {}: {e}", link.display()))?;
        }
    }
    Ok(data_dir)
}

#[tokio::test]
async fn microsandbox_driver_passes_the_sandbox_contract() {
    use temps_agents::sandbox::microsandbox::{
        MicrosandboxSandboxConfig, MicrosandboxSandboxProvider,
    };
    let data_dir = match msb_data_dir() {
        Ok(dir) => dir,
        Err(reason) => {
            println!("skipping the microsandbox driver eval: {reason}");
            return;
        }
    };
    let provider = match MicrosandboxSandboxProvider::new(MicrosandboxSandboxConfig::from_data_dir(
        data_dir.clone(),
    )) {
        Ok(provider) => provider,
        Err(e) => {
            println!("skipping the microsandbox driver eval: {e}");
            return;
        }
    };
    if let Err(reason) = provider.probe() {
        println!("skipping the microsandbox driver eval: {reason}");
        return;
    }
    let _serial = SERIAL.lock().await;
    let inner: Arc<dyn SandboxProvider> = Arc::new(provider);
    let label = format!("p6eval-{}", suffix());
    println!(
        "microsandbox driver eval: data dir {}, sandbox {label}",
        data_dir.display()
    );
    let proven = contract(
        SandboxBackend::Microsandbox,
        facade(SandboxBackend::Microsandbox, inner),
        || {
            create_config(
                SandboxBackend::Microsandbox,
                &label,
                Some("alpine:3.20".to_string()),
            )
        },
    )
    .await;
    summary(SandboxBackend::Microsandbox, &proven);
}

// ── Firecracker ─────────────────────────────────────────────────────────

fn firecracker_or_skip() -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        println!(
            "skipping the firecracker driver eval: Firecracker needs Linux with /dev/kvm \
             (this host is {})",
            std::env::consts::OS
        );
        return None;
    }
    if !std::path::Path::new("/dev/kvm").exists() {
        println!("skipping the firecracker driver eval: /dev/kvm is missing");
        return None;
    }
    if std::env::var("TEMPS_FC_E2E").as_deref() != Ok("1") {
        println!(
            "skipping the firecracker driver eval: set TEMPS_FC_E2E=1 and TEMPS_DATA_DIR \
             (a data dir provisioned by `temps firecracker setup`)"
        );
        return None;
    }
    match std::env::var("TEMPS_DATA_DIR") {
        Ok(dir) => Some(PathBuf::from(dir)),
        Err(_) => {
            println!("skipping the firecracker driver eval: TEMPS_FC_E2E=1 needs TEMPS_DATA_DIR");
            None
        }
    }
}

#[tokio::test]
async fn firecracker_driver_passes_the_sandbox_contract() {
    use temps_agents::sandbox::firecracker::{
        FirecrackerSandboxConfig, FirecrackerSandboxProvider,
    };
    let Some(data_dir) = firecracker_or_skip() else {
        return;
    };
    let docker = match bollard::Docker::connect_with_defaults() {
        Ok(docker) => Arc::new(docker),
        Err(e) => {
            println!("skipping the firecracker driver eval: its image toolchain needs Docker: {e}");
            return;
        }
    };
    let provider =
        // A test host may lack the root launcher; with one, VMs go through it.
        FirecrackerSandboxProvider::new(
            FirecrackerSandboxConfig::from_data_dir(data_dir).allowing_unjailed(),
            docker,
        );
    if !provider.is_available().await {
        println!(
            "skipping the firecracker driver eval: the backend is not provisioned \
             (run `temps firecracker setup`)"
        );
        return;
    }
    let _serial = SERIAL.lock().await;
    let inner: Arc<dyn SandboxProvider> = Arc::new(provider);
    let label = format!("p6eval{}", suffix());
    let proven = contract(
        SandboxBackend::Firecracker,
        facade(SandboxBackend::Firecracker, inner),
        || create_config(SandboxBackend::Firecracker, &label, None),
    )
    .await;
    summary(SandboxBackend::Firecracker, &proven);
}
