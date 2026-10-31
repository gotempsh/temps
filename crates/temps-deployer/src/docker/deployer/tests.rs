// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tests for [`super`].

use super::*;
use crate::docker::test_support::create_test_docker_runtime;
use crate::docker::DockerRuntime;
use crate::BuilderError;
use crate::{
    ContainerLogConfig, DeployRequest, PortMapping, Protocol, ResourceLimits, RestartPolicy,
};
use serial_test::serial;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Only Docker's 404 reports a container as gone. Callers record
/// `ContainerNotFound` as confirmed removal, so any other inspect failure
/// has to stay an error.
fn blkio(op: &str, value: u64) -> bollard::models::ContainerBlkioStatEntry {
    bollard::models::ContainerBlkioStatEntry {
        major: Some(8),
        minor: Some(0),
        op: Some(op.to_string()),
        value: Some(value),
    }
}

#[test]
fn block_io_sums_reads_and_writes_across_devices_and_cgroup_versions() {
    // cgroup v1 capitalises the operation, v2 does not; two devices.
    let entries = [
        blkio("Read", 100),
        blkio("Write", 40),
        blkio("read", 11),
        blkio("write", 2),
        blkio("Sync", 999),
        blkio("Total", 999),
    ];
    assert_eq!(block_io_bytes(&entries), (111, 42));
    assert_eq!(block_io_bytes(&[]), (0, 0));
}

#[test]
fn block_io_saturates_instead_of_wrapping() {
    let entries = [blkio("Read", u64::MAX), blkio("Read", 5)];
    assert_eq!(block_io_bytes(&entries).0, u64::MAX);
}

#[test]
fn only_a_docker_404_reports_a_container_as_not_found() {
    let missing = inspect_error(
        "abc123",
        bollard::errors::Error::DockerResponseServerError {
            status_code: 404,
            message: "No such container: abc123".to_string(),
        },
    );
    assert!(
        matches!(&missing, DeployerError::ContainerNotFound(message) if message.contains("abc123")),
        "{missing:?}"
    );

    for error in [
        bollard::errors::Error::DockerResponseServerError {
            status_code: 500,
            message: "context deadline exceeded".to_string(),
        },
        bollard::errors::Error::RequestTimeoutError,
    ] {
        let failed = inspect_error("abc123", error);
        assert!(
            matches!(&failed, DeployerError::Other(message) if message.contains("abc123")),
            "{failed:?}"
        );
    }
}

#[test]
fn inspected_ports_preserve_every_interface_binding() {
    let ports = HashMap::from([(
        "3000/tcp".to_string(),
        Some(vec![
            bollard::models::PortBinding {
                host_ip: Some("127.0.0.1".to_string()),
                host_port: Some("32001".to_string()),
            },
            bollard::models::PortBinding {
                host_ip: Some("0.0.0.0".to_string()),
                host_port: Some("32002".to_string()),
            },
        ]),
    )]);

    let mappings = parse_inspected_port_mappings(ports);

    assert_eq!(mappings.len(), 2);
    assert!(mappings.iter().any(|mapping| {
        mapping.host_ip.as_deref() == Some("127.0.0.1") && mapping.host_port == 32001
    }));
    assert!(mappings.iter().any(|mapping| {
        mapping.host_ip.as_deref() == Some("0.0.0.0") && mapping.host_port == 32002
    }));
}

#[tokio::test]
#[serial]
async fn test_container_lifecycle() {
    match create_test_docker_runtime().await {
        Ok(runtime) => {
            // First ensure we have an image to work with (alpine is usually available)
            let deploy_request = DeployRequest {
                image_name: "alpine:latest".to_string(),
                container_name: "lifecycle-test".to_string(),
                environment_vars: {
                    let mut env = HashMap::new();
                    env.insert("TEST_VAR".to_string(), "test_value".to_string());
                    env
                },
                secrets: HashMap::new(),
                port_mappings: vec![],
                network_name: None,
                extra_networks: Vec::new(),
                resource_limits: ResourceLimits {
                    cpu_limit: Some(0.5),
                    memory_limit_mb: Some(64),
                    disk_limit_mb: Some(256),
                },
                restart_policy: RestartPolicy::Never,
                log_path: PathBuf::from("/tmp/lifecycle-test.log"),
                command: Some(vec!["sleep".to_string(), "30".to_string()]),
                entrypoint: None,
                log_config: Some(ContainerLogConfig::app_default()),
                labels: HashMap::new(),
                project_slug: None,
                control_plane_grants_socket: false,
            };

            let deploy_result = runtime.deploy_container(deploy_request).await;

            match deploy_result {
                Ok(deploy_info) => {
                    println!("✅ Container deployed: {}", deploy_info.container_name);

                    // Test container operations
                    let container_id = &deploy_info.container_id;

                    // Test getting container info
                    if let Ok(info) = runtime.get_container_info(container_id).await {
                        println!("📋 Container info: {:?}", info.status);
                    }

                    // Test pause/resume
                    if let Ok(()) = runtime.pause_container(container_id).await {
                        println!("⏸️  Container paused");
                    }

                    if let Ok(()) = runtime.resume_container(container_id).await {
                        println!("▶️  Container resumed");
                    }

                    // Test stop
                    if let Ok(()) = runtime.stop_container(container_id).await {
                        println!("⏹️  Container stopped");
                    }

                    // Test remove
                    if let Ok(()) = runtime.remove_container(container_id).await {
                        println!("🗑️  Container removed");
                    }

                    println!("✅ Container lifecycle test completed");
                }
                Err(e) => {
                    println!("🔧 Container deployment failed (may be expected): {}", e);
                }
            }
        }
        Err(e) => {
            println!("🔧 Docker not available: {}", e);
        }
    }
}

/// Empirical proof of the "no limit unless opted in" contract: a deploy
/// with `cpu_limit: None` / `memory_limit_mb: None` must produce a Docker
/// container whose `HostConfig.NanoCpus` and `Memory` are 0 (unset =
/// uncapped), and an explicit limit must actually reach Docker. Inspects
/// the live container via bollard rather than trusting the request struct.
#[tokio::test]
#[serial]
async fn test_resource_limits_none_means_uncapped() {
    let runtime = match create_test_docker_runtime().await {
        Ok(r) => r,
        Err(e) => {
            println!("🔧 Docker not available, skipping: {}", e);
            return;
        }
    };
    let docker = match Docker::connect_with_local_defaults() {
        Ok(d) => d,
        Err(e) => {
            println!("🔧 Docker not available, skipping: {}", e);
            return;
        }
    };
    if docker.ping().await.is_err() {
        println!("🔧 Docker ping failed, skipping");
        return;
    }

    let base = |name: &str, limits: ResourceLimits| DeployRequest {
        image_name: "alpine:latest".to_string(),
        container_name: name.to_string(),
        environment_vars: HashMap::new(),
        secrets: HashMap::new(),
        port_mappings: vec![],
        network_name: None,
        extra_networks: Vec::new(),
        resource_limits: limits,
        restart_policy: RestartPolicy::Never,
        log_path: PathBuf::from(format!("/tmp/{}.log", name)),
        command: Some(vec!["sleep".to_string(), "30".to_string()]),
        entrypoint: None,
        log_config: Some(ContainerLogConfig::app_default()),
        labels: HashMap::new(),
        project_slug: None,
        control_plane_grants_socket: false,
    };

    let inspect_caps = |id: String| {
        let docker = docker.clone();
        async move {
            let i = docker
                .inspect_container(
                    &id,
                    None::<bollard::query_parameters::InspectContainerOptions>,
                )
                .await
                .expect("inspect");
            let hc = i.host_config.expect("host_config");
            (hc.nano_cpus.unwrap_or(0), hc.memory.unwrap_or(0))
        }
    };

    // --- Case 1: no limits configured -> container must be UNCAPPED ---
    let uncapped_name = "temps-rl-uncapped-test";
    let _ = runtime.remove_container(uncapped_name).await;
    let info = runtime
        .deploy_container(base(
            uncapped_name,
            ResourceLimits {
                cpu_limit: None,
                memory_limit_mb: None,
                disk_limit_mb: None,
            },
        ))
        .await
        .expect("deploy uncapped");
    let (nano_cpus, memory) = inspect_caps(info.container_id.clone()).await;
    let _ = runtime.remove_container(&info.container_id).await;
    println!(
        "UNCAPPED deploy -> NanoCpus={} Memory={} (both must be 0)",
        nano_cpus, memory
    );
    assert_eq!(
        nano_cpus, 0,
        "None cpu_limit leaked a CPU cap into the container ({})",
        nano_cpus
    );
    assert_eq!(
        memory, 0,
        "None memory_limit_mb leaked a memory cap into the container ({})",
        memory
    );

    // --- Case 2 (control): explicit limits MUST reach Docker ---
    let capped_name = "temps-rl-capped-test";
    let _ = runtime.remove_container(capped_name).await;
    let info = runtime
        .deploy_container(base(
            capped_name,
            ResourceLimits {
                cpu_limit: Some(0.5),
                memory_limit_mb: Some(64),
                disk_limit_mb: None,
            },
        ))
        .await
        .expect("deploy capped");
    let (nano_cpus, memory) = inspect_caps(info.container_id.clone()).await;
    let _ = runtime.remove_container(&info.container_id).await;
    println!(
        "CAPPED deploy -> NanoCpus={} Memory={} (expect 500000000 / 67108864)",
        nano_cpus, memory
    );
    assert_eq!(nano_cpus, 500_000_000, "explicit 0.5 CPU not applied");
    assert_eq!(memory, 64 * 1024 * 1024, "explicit 64MB not applied");
}

#[tokio::test]
async fn test_list_containers() {
    match create_test_docker_runtime().await {
        Ok(runtime) => match runtime.list_containers().await {
            Ok(containers) => {
                println!("✅ Found {} containers", containers.len());
                for container in containers.iter().take(3) {
                    println!("  📦 {}: {:?}", container.container_name, container.status);
                }
            }
            Err(e) => {
                println!("🔧 Failed to list containers: {}", e);
            }
        },
        Err(e) => {
            println!("🔧 Docker not available: {}", e);
        }
    }
}

#[tokio::test]
async fn test_resource_limits_validation() {
    let resource_limits = ResourceLimits {
        cpu_limit: Some(2.0),
        memory_limit_mb: Some(512),
        disk_limit_mb: Some(1024),
    };

    // Test that resource limits are properly structured
    assert!(resource_limits.cpu_limit.unwrap() > 0.0);
    assert!(resource_limits.memory_limit_mb.unwrap() > 0);
    assert!(resource_limits.disk_limit_mb.unwrap() > 0);

    println!("✅ Resource limits validation passed");
}

#[tokio::test]
async fn test_port_mapping_validation() {
    let port_mapping = PortMapping {
        host_port: 8080,
        container_port: 80,
        protocol: Protocol::Tcp,
        host_ip: None,
    };

    assert_eq!(port_mapping.host_port, 8080);
    assert_eq!(port_mapping.container_port, 80);
    assert!(matches!(port_mapping.protocol, Protocol::Tcp));

    println!("✅ Port mapping validation passed");
}

#[tokio::test]
async fn test_restart_policy_enum() {
    let policies = [
        RestartPolicy::Never,
        RestartPolicy::Always,
        RestartPolicy::OnFailure,
        RestartPolicy::UnlessStopped,
    ];

    // Just test that all enum variants exist and can be created
    assert_eq!(policies.len(), 4);
    println!("✅ Restart policy enum validation passed");
}

#[tokio::test]
async fn test_error_types() {
    // Test that error types can be created and match properly
    let build_error = BuilderError::BuildFailed("test error".to_string());
    let deploy_error = DeployerError::DeploymentFailed("test deploy error".to_string());

    assert!(matches!(build_error, BuilderError::BuildFailed(_)));
    assert!(matches!(deploy_error, DeployerError::DeploymentFailed(_)));

    println!("✅ Error types validation passed");
}

// ── Container stats helpers ──────────────────────────────────────────────

fn cpu_sample(
    total: u64,
    system: u64,
    online_cpus: u32,
) -> bollard::models::ContainerStatsResponse {
    bollard::models::ContainerStatsResponse {
        cpu_stats: Some(bollard::models::ContainerCpuStats {
            cpu_usage: Some(bollard::models::ContainerCpuUsage {
                total_usage: Some(total),
                ..Default::default()
            }),
            system_cpu_usage: Some(system),
            online_cpus: Some(online_cpus),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// 50% on a 2-CPU host: cpu_delta=1e9 (1s of cpu-time), system_delta=4e9
/// (wall_ticks * cpus). (cpu_delta/system_delta)*online_cpus = 0.25*2 = 50%.
#[test]
fn cpu_percent_50pct_two_cpus() {
    let prev = cpu_sample(0, 0, 2);
    let curr = cpu_sample(1_000_000_000, 4_000_000_000, 2);
    let pct = cpu_percent_from_samples(&curr, &prev).unwrap();
    assert!((pct - 50.0).abs() < 0.01, "expected ~50%, got {pct}");
}

/// A 2-core container fully pinned reads as 200% — we intentionally
/// don't clamp to 100, since the UI shows "x.x% / N cores".
#[test]
fn cpu_percent_fully_pinned_two_cpus_reads_200pct() {
    let prev = cpu_sample(0, 0, 2);
    let curr = cpu_sample(2_000_000_000, 2_000_000_000, 2);
    let pct = cpu_percent_from_samples(&curr, &prev).unwrap();
    assert!((pct - 200.0).abs() < 0.01, "expected ~200%, got {pct}");
}

/// Identical samples (container idle or clock didn't advance) must
/// produce None rather than 0% — the old code returned a spurious
/// near-zero value here, which is what made the UI read "CPU 0.0%".
#[test]
fn cpu_percent_identical_samples_returns_none() {
    let prev = cpu_sample(5_000_000_000, 10_000_000_000, 4);
    let same = cpu_sample(5_000_000_000, 10_000_000_000, 4);
    assert!(cpu_percent_from_samples(&same, &prev).is_none());
}

#[test]
fn cpu_percent_missing_counters_returns_none() {
    let prev = bollard::models::ContainerStatsResponse {
        cpu_stats: Some(bollard::models::ContainerCpuStats {
            cpu_usage: None,
            system_cpu_usage: Some(0),
            online_cpus: Some(1),
            ..Default::default()
        }),
        ..Default::default()
    };
    let curr = cpu_sample(1_000_000_000, 1_000_000_000, 1);
    assert!(cpu_percent_from_samples(&curr, &prev).is_none());
}

fn mem_stats(
    usage: u64,
    limit: u64,
    cache: Option<(&'static str, u64)>,
) -> bollard::models::ContainerMemoryStats {
    let mut stats_map = std::collections::HashMap::new();
    if let Some((key, val)) = cache {
        stats_map.insert(key.to_string(), val);
    }
    bollard::models::ContainerMemoryStats {
        usage: Some(usage),
        limit: Some(limit),
        stats: if cache.is_some() {
            Some(stats_map)
        } else {
            None
        },
        ..Default::default()
    }
}

/// cgroup v2 hosts (Docker Desktop, modern Linux) report reclaimable
/// file pages as `inactive_file`. Subtract it so the number matches
/// `docker stats`.
#[test]
fn memory_usage_subtracts_inactive_file_cgroup_v2() {
    let mem = mem_stats(
        16 * 1024 * 1024 * 1024,
        16 * 1024 * 1024 * 1024,
        Some(("inactive_file", 8 * 1024 * 1024 * 1024)),
    );
    assert_eq!(
        memory_usage_excluding_cache(&mem).unwrap(),
        8 * 1024 * 1024 * 1024
    );
}

/// cgroup v1 surfaces the same data as `cache`. Subtract it.
#[test]
fn memory_usage_subtracts_cache_cgroup_v1() {
    let mem = mem_stats(
        10 * 1024 * 1024 * 1024,
        16 * 1024 * 1024 * 1024,
        Some(("cache", 3 * 1024 * 1024 * 1024)),
    );
    assert_eq!(
        memory_usage_excluding_cache(&mem).unwrap(),
        7 * 1024 * 1024 * 1024
    );
}

/// Hosts that report both should prefer `inactive_file` — matches the
/// Docker CLI exactly and is the more accurate signal.
#[test]
fn memory_usage_prefers_inactive_file_when_both_present() {
    let mut stats_map = std::collections::HashMap::new();
    stats_map.insert("inactive_file".to_string(), 4 * 1024 * 1024 * 1024);
    stats_map.insert("cache".to_string(), 6 * 1024 * 1024 * 1024);
    let mem = bollard::models::ContainerMemoryStats {
        usage: Some(10 * 1024 * 1024 * 1024),
        limit: Some(16 * 1024 * 1024 * 1024),
        stats: Some(stats_map),
        ..Default::default()
    };
    assert_eq!(
        memory_usage_excluding_cache(&mem).unwrap(),
        6 * 1024 * 1024 * 1024
    );
}

/// Without cache info, return raw usage rather than crashing.
#[test]
fn memory_usage_returns_raw_when_no_cache_info() {
    let mem = mem_stats(5 * 1024 * 1024 * 1024, 16 * 1024 * 1024 * 1024, None);
    assert_eq!(
        memory_usage_excluding_cache(&mem).unwrap(),
        5u64 * 1024 * 1024 * 1024
    );
}

/// Defensive: if `cache` is somehow larger than `usage` (sentinel
/// values, stat skew), don't underflow — return raw usage.
#[test]
fn memory_usage_handles_cache_larger_than_usage() {
    let mem = mem_stats(
        1024 * 1024,
        16 * 1024 * 1024 * 1024,
        Some(("cache", 10 * 1024 * 1024 * 1024)),
    );
    assert_eq!(memory_usage_excluding_cache(&mem).unwrap(), 1024 * 1024);
}

mod health_probe_exec_tests {
    use super::*;
    use testcontainers::{runners::AsyncRunner, GenericImage, ImageExt};

    #[tokio::test]
    async fn health_probe_local_exec_success_failure_output_limit_and_timeout(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let docker = match Docker::connect_with_local_defaults() {
            Ok(docker) => Arc::new(docker),
            Err(error) => {
                eprintln!("Skipping exec integration test: Docker unavailable: {error}");
                return Ok(());
            }
        };
        if docker.ping().await.is_err() {
            eprintln!("Skipping exec integration test: Docker unavailable");
            return Ok(());
        }
        let container = GenericImage::new("alpine", "3.21")
            .with_cmd(["sleep", "300"])
            .start()
            .await?;
        let runtime = DockerRuntime::new(
            docker.clone(),
            false,
            format!("health-probe-{}", uuid::Uuid::new_v4()),
        );
        let timeout = std::time::Duration::from_secs(5);
        let result = runtime
            .exec_in_container(container.id(), vec!["echo".into(), "ready".into()], timeout)
            .await?;
        assert_eq!(result.exit_code, 0);
        assert!(result.output.contains("ready"));
        assert!(!result.truncated);
        let result = runtime
            .exec_in_container(
                container.id(),
                vec![
                    "sh".into(),
                    "-c".into(),
                    "echo unhealthy >&2; exit 1".into(),
                ],
                timeout,
            )
            .await?;
        assert_eq!(result.exit_code, 1);
        assert!(result.output.contains("unhealthy"));
        let result = runtime
            .exec_in_container(
                container.id(),
                vec!["sh".into(), "-c".into(), "head -c 8192 /dev/zero".into()],
                timeout,
            )
            .await?;
        assert_eq!(result.output.len(), crate::MAX_EXEC_OUTPUT_BYTES);
        assert!(result.truncated);
        // Docker may report an exec launch failure through the API or as
        // exit 126/127. An API failure can stop the candidate for cleanup,
        // so keep it separate from the subsequent timeout assertion.
        let missing_command_container = GenericImage::new("alpine", "3.21")
            .with_cmd(["sleep", "300"])
            .start()
            .await?;
        match runtime
            .exec_in_container(
                missing_command_container.id(),
                vec!["missing-health-command".into()],
                timeout,
            )
            .await
        {
            Ok(result) => assert!(matches!(result.exit_code, 126 | 127)),
            Err(DeployerError::ExecFailed { .. }) => {}
            Err(error) => return Err(error.into()),
        }
        let result = runtime
            .exec_in_container(
                container.id(),
                vec!["sleep".into(), "60".into()],
                std::time::Duration::from_millis(100),
            )
            .await;
        assert!(matches!(result, Err(DeployerError::ExecTimeout { .. })));
        let mut running = true;
        for _ in 0..20 {
            let info = docker
                .inspect_container(container.id(), None::<InspectContainerOptions>)
                .await?;
            running = info.state.and_then(|state| state.running).unwrap_or(false);
            if !running {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(!running, "timed-out command must not be left running");
        Ok(())
    }
}

mod health_probe_docker_protocol_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const CANDIDATE_ID: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum ProbeScript {
        Complete,
        StallStart,
        StallStream,
        BrokenStream,
        InspectFailed,
        InspectRunning,
        InspectUnknown,
        StartFailed,
        StopFailsOnce,
    }

    #[derive(Default)]
    struct ProbeDockerState {
        started: AtomicBool,
        stopped: AtomicBool,
        inspections: AtomicUsize,
        stop_calls: AtomicUsize,
        restart_calls: AtomicUsize,
        stop_notice: tokio::sync::Notify,
    }

    async fn runtime(
        output: Vec<u8>,
        exit_code: Option<i64>,
        stall: bool,
    ) -> (
        DockerRuntime,
        tokio::task::JoinHandle<()>,
        Arc<ProbeDockerState>,
    ) {
        scripted_runtime(
            output,
            exit_code,
            if stall {
                ProbeScript::StallStart
            } else {
                ProbeScript::Complete
            },
        )
        .await
    }

    async fn scripted_runtime(
        output: Vec<u8>,
        exit_code: Option<i64>,
        script: ProbeScript,
    ) -> (
        DockerRuntime,
        tokio::task::JoinHandle<()>,
        Arc<ProbeDockerState>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(ProbeDockerState::default());
        let state_server = state.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let output = output.clone();
                let state = state_server.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buffer = [0; 1024];
                    loop {
                        let read = socket.read(&mut buffer).await.unwrap();
                        if read == 0 {
                            return;
                        }
                        request.extend_from_slice(&buffer[..read]);
                        if let Some(end) =
                            request.windows(4).position(|window| window == b"\r\n\r\n")
                        {
                            let head = String::from_utf8_lossy(&request[..end]);
                            let length = head
                                .lines()
                                .find_map(|line| {
                                    line.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .and_then(|value| value.trim().parse::<usize>().ok())
                                })
                                .unwrap_or(0);
                            if request.len() >= end + 4 + length {
                                break;
                            }
                        }
                    }
                    let path = String::from_utf8_lossy(&request)
                        .lines()
                        .next()
                        .unwrap()
                        .to_string();
                    if path.contains("/start") {
                        state.started.store(true, Ordering::SeqCst);
                        if matches!(script, ProbeScript::StallStart | ProbeScript::StopFailsOnce) {
                            state.stop_notice.notified().await;
                            return;
                        }
                        if script == ProbeScript::StartFailed {
                            let _ = socket.write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").await;
                            return;
                        }
                        socket.write_all(b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: tcp\r\n\r\n").await.unwrap();
                        let mut frame = vec![1, 0, 0, 0];
                        if script == ProbeScript::BrokenStream {
                            frame.extend_from_slice(&1024_u32.to_be_bytes());
                            frame.extend_from_slice(b"incomplete output");
                        } else {
                            frame.extend_from_slice(&(output.len() as u32).to_be_bytes());
                            frame.extend_from_slice(&output);
                        }
                        let _ = socket.write_all(&frame).await;
                        if script == ProbeScript::StallStream {
                            state.stop_notice.notified().await;
                        }
                    } else {
                        let mut status = "200 OK";
                        let body = if path.contains("/stop") {
                            assert!(
                                path.contains(&format!("/containers/{CANDIDATE_ID}/stop")),
                                "cleanup must use the immutable ID: {path}"
                            );
                            let call = state.stop_calls.fetch_add(1, Ordering::SeqCst);
                            if script == ProbeScript::StopFailsOnce && call == 0 {
                                status = "500 Internal Server Error";
                                "{}".into()
                            } else {
                                state.stopped.store(true, Ordering::SeqCst);
                                state.stop_notice.notify_waiters();
                                String::new()
                            }
                        } else if path.contains("/restart") {
                            state.restart_calls.fetch_add(1, Ordering::SeqCst);
                            String::new()
                        } else if path.contains("/json") {
                            let prepared = state.inspections.fetch_add(1, Ordering::SeqCst) == 0;
                            if !prepared && script == ProbeScript::InspectFailed {
                                status = "500 Internal Server Error";
                                "{}".into()
                            } else {
                                let running = if !prepared && script == ProbeScript::InspectUnknown
                                {
                                    None
                                } else {
                                    Some(!prepared && script == ProbeScript::InspectRunning)
                                };
                                serde_json::json!({ "ContainerID": CANDIDATE_ID, "Running": running, "ExitCode": exit_code }).to_string()
                            }
                        } else {
                            "{\"Id\":\"health-exec\"}".into()
                        };
                        let response = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                        let _ = socket.write_all(response.as_bytes()).await;
                    }
                    let _ = socket.shutdown().await;
                });
            }
        });
        let docker =
            Arc::new(Docker::connect_with_http(&address, 5, bollard::API_DEFAULT_VERSION).unwrap());
        (
            DockerRuntime::new(docker, false, "unused-test-network".into()),
            task,
            state,
        )
    }

    async fn wait_for_flag(flag: &AtomicBool) {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !flag.load(Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn health_probe_local_cleanup_survives_outer_deadlines_and_task_cancellation() {
        for script in [
            ProbeScript::StallStart,
            ProbeScript::StallStream,
            ProbeScript::StopFailsOnce,
        ] {
            for abort_task in [false, true] {
                let (runtime, server, state) =
                    scripted_runtime(b"waiting".to_vec(), Some(0), script).await;
                let operation = async move {
                    runtime
                        .exec_in_container(
                            "candidate-alias",
                            vec!["check-worker".into()],
                            std::time::Duration::from_secs(30),
                        )
                        .await
                };
                if abort_task {
                    let task = tokio::spawn(operation);
                    wait_for_flag(&state.started).await;
                    task.abort();
                    assert!(task.await.unwrap_err().is_cancelled());
                } else {
                    assert!(
                        tokio::time::timeout(std::time::Duration::from_millis(50), operation)
                            .await
                            .is_err()
                    );
                }
                wait_for_flag(&state.stopped).await;
                assert_eq!(state.restart_calls.load(Ordering::SeqCst), 0);
                assert_eq!(
                    state.stop_calls.load(Ordering::SeqCst),
                    if script == ProbeScript::StopFailsOnce {
                        2
                    } else {
                        1
                    }
                );
                server.abort();
            }
        }
    }

    #[tokio::test]
    async fn health_probe_local_cleanup_runs_on_ambiguous_start_stream_and_inspect_errors() {
        for script in [
            ProbeScript::StartFailed,
            ProbeScript::BrokenStream,
            ProbeScript::InspectFailed,
            ProbeScript::InspectRunning,
            ProbeScript::InspectUnknown,
        ] {
            let (runtime, server, state) =
                scripted_runtime(b"output".to_vec(), Some(0), script).await;
            let error = runtime
                .exec_in_container(
                    "candidate-alias",
                    vec!["check-worker".into()],
                    std::time::Duration::from_secs(2),
                )
                .await
                .unwrap_err();
            assert!(matches!(error, DeployerError::ExecFailed { .. }));
            wait_for_flag(&state.stopped).await;
            assert_eq!(state.restart_calls.load(Ordering::SeqCst), 0);
            server.abort();
        }
    }

    #[tokio::test]
    async fn health_probe_local_docker_protocol_captures_exit_status_and_truncates_output() {
        for code in [0, 1, 127] {
            let (runtime, server, state) = runtime(vec![b'x'; 8192], Some(code), false).await;
            let result = runtime
                .exec_in_container(
                    "worker-container",
                    vec!["check-worker".into()],
                    std::time::Duration::from_secs(2),
                )
                .await
                .unwrap();
            assert_eq!(result.exit_code, code);
            assert_eq!(result.output.len(), crate::MAX_EXEC_OUTPUT_BYTES);
            assert!(result.truncated);
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            assert_eq!(
                state.stop_calls.load(Ordering::SeqCst),
                0,
                "completed commands must disarm candidate cleanup, including nonzero exits"
            );
            server.abort();
        }
        let (runtime, server, _) = runtime(b"ready".to_vec(), None, false).await;
        assert!(runtime
            .exec_in_container(
                "worker-container",
                vec!["check-worker".into()],
                std::time::Duration::from_secs(2)
            )
            .await
            .is_err());
        server.abort();
    }

    #[tokio::test]
    async fn health_probe_local_docker_protocol_enforces_deadline_and_stops_candidate() {
        let (runtime, server, stopped) = runtime(Vec::new(), Some(0), true).await;
        let start = std::time::Instant::now();
        let result = runtime
            .exec_in_container(
                "worker-container",
                vec!["check-worker".into()],
                std::time::Duration::from_millis(50),
            )
            .await;
        assert!(matches!(result, Err(DeployerError::ExecTimeout { .. })));
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
        for _ in 0..20 {
            if stopped.stopped.load(Ordering::Relaxed) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            stopped.stopped.load(Ordering::Relaxed),
            "hung exec cleanup must target the candidate container"
        );
        server.abort();
    }
}
