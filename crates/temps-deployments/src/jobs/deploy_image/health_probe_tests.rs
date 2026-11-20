// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use std::collections::VecDeque;
use temps_core::repo_config::HealthCheckConfig;
use temps_deployer::{ContainerInfo, DeployerError, ExecResult};

struct ProbeDeployer {
    states: Mutex<VecDeque<ContainerInfo>>,
    exits: Mutex<VecDeque<i64>>,
    exec_timeout: bool,
    exec_error: bool,
}

impl ProbeDeployer {
    fn running() -> Self {
        Self {
            states: Mutex::new(VecDeque::from([ContainerInfo {
                status: DeployerContainerStatus::Running,
                restart_count: Some(0),
                ..Default::default()
            }])),
            exits: Mutex::new(VecDeque::from([0])),
            exec_timeout: false,
            exec_error: false,
        }
    }
}

#[async_trait]
impl ContainerDeployer for ProbeDeployer {
    async fn get_container_info(&self, _id: &str) -> Result<ContainerInfo, DeployerError> {
        let mut states = self.states.lock().unwrap();
        let state = states.front().unwrap().clone();
        if states.len() > 1 {
            states.pop_front();
        }
        Ok(state)
    }
    async fn exec_in_container(
        &self,
        id: &str,
        _command: Vec<String>,
        timeout: Duration,
    ) -> Result<ExecResult, DeployerError> {
        if self.exec_timeout {
            return Err(DeployerError::ExecTimeout {
                container_id: id.into(),
                timeout_secs: timeout.as_secs(),
            });
        }
        if self.exec_error {
            return Err(DeployerError::ExecFailed {
                container_id: id.into(),
                reason: "probe output".into(),
            });
        }
        let mut exits = self.exits.lock().unwrap();
        let code = *exits.front().unwrap();
        if exits.len() > 1 {
            exits.pop_front();
        }
        Ok(ExecResult {
            exit_code: code,
            output: "probe output".into(),
            truncated: false,
        })
    }
    async fn deploy_container(
        &self,
        _: DeployRequest,
    ) -> Result<temps_deployer::DeployResult, DeployerError> {
        unreachable!()
    }
    async fn start_container(&self, _: &str) -> Result<(), DeployerError> {
        unreachable!()
    }
    async fn stop_container(&self, _: &str) -> Result<(), DeployerError> {
        unreachable!()
    }
    async fn pause_container(&self, _: &str) -> Result<(), DeployerError> {
        unreachable!()
    }
    async fn resume_container(&self, _: &str) -> Result<(), DeployerError> {
        unreachable!()
    }
    async fn remove_container(&self, _: &str) -> Result<(), DeployerError> {
        unreachable!()
    }
    async fn get_container_stats(
        &self,
        _: &str,
    ) -> Result<temps_deployer::ContainerStats, DeployerError> {
        unreachable!()
    }
    async fn list_containers(&self) -> Result<Vec<ContainerInfo>, DeployerError> {
        unreachable!()
    }
    async fn get_container_logs(&self, _: &str) -> Result<String, DeployerError> {
        unreachable!()
    }
    async fn stream_container_logs(
        &self,
        _: &str,
    ) -> Result<Box<dyn futures::Stream<Item = String> + Send + Unpin>, DeployerError> {
        unreachable!()
    }
}

fn probe(kind: &str) -> HealthCheckConfig {
    HealthCheckConfig {
        probe_type: Some(kind.into()),
        stable_for: 1,
        interval: 1,
        command: vec!["check-worker".into()],
        ..Default::default()
    }
}

#[test]
fn health_probe_http_keeps_existing_accepted_and_failed_statuses() {
    for code in [200, 204, 301, 302, 404, 405] {
        assert!(http_probe_status_is_healthy(
            reqwest::StatusCode::from_u16(code).unwrap()
        ));
    }
    for code in [400, 401, 403, 429, 500, 502, 503] {
        assert!(!http_probe_status_is_healthy(
            reqwest::StatusCode::from_u16(code).unwrap()
        ));
    }
}

#[tokio::test]
async fn health_probe_process_requires_stability_and_is_bounded_by_startup_timeout() {
    let deployer = ProbeDeployer::running();
    let start = tokio::time::Instant::now();
    assert!(wait_for_non_http_probe(
        &deployer,
        "worker",
        &probe("process"),
        None,
        Duration::from_secs(2),
        None,
    )
    .await
    .is_ok());
    assert!(start.elapsed() >= Duration::from_secs(1));
    assert!(wait_for_non_http_probe(
        &deployer,
        "worker",
        &probe("process"),
        None,
        Duration::from_millis(20),
        None,
    )
    .await
    .is_err());
}

#[tokio::test]
async fn health_probe_crash_after_start_and_restart_loop_fail_for_every_kind() {
    for kind in ["process", "exec", "tcp"] {
        let deployer = ProbeDeployer::running();
        deployer.states.lock().unwrap().front_mut().unwrap().status =
            DeployerContainerStatus::Exited;
        let result = wait_for_non_http_probe(
            &deployer,
            "worker",
            &probe(kind),
            Some("127.0.0.1:1"),
            Duration::from_secs(2),
            None,
        )
        .await;
        assert!(result.is_err(), "{kind}");
        let deployer = ProbeDeployer::running();
        deployer
            .states
            .lock()
            .unwrap()
            .front_mut()
            .unwrap()
            .restart_count = Some(CRASH_LOOP_RESTART_THRESHOLD);
        assert!(wait_for_non_http_probe(
            &deployer,
            "worker",
            &probe(kind),
            Some("127.0.0.1:1"),
            Duration::from_secs(2),
            None,
        )
        .await
        .is_err());
    }
    let deployer = ProbeDeployer::running();
    let mut exited = deployer.states.lock().unwrap().front().unwrap().clone();
    exited.status = DeployerContainerStatus::Exited;
    deployer.states.lock().unwrap().push_back(exited);
    assert!(wait_for_non_http_probe(
        &deployer,
        "worker",
        &probe("process"),
        None,
        Duration::from_secs(3),
        None,
    )
    .await
    .is_err());
}

#[tokio::test]
async fn health_probe_process_restart_resets_the_stability_window() {
    let deployer = ProbeDeployer::running();
    let mut restarted = deployer.states.lock().unwrap().front().unwrap().clone();
    restarted.restart_count = Some(1);
    deployer.states.lock().unwrap().push_back(restarted);
    let start = tokio::time::Instant::now();
    assert!(wait_for_non_http_probe(
        &deployer,
        "worker",
        &probe("process"),
        None,
        Duration::from_secs(4),
        None,
    )
    .await
    .is_ok());
    assert!(start.elapsed() >= Duration::from_secs(2));
}

#[tokio::test]
async fn health_probe_exec_retries_nonzero_then_passes_and_rejects_unknown_commands() {
    let deployer = ProbeDeployer::running();
    *deployer.exits.lock().unwrap() = VecDeque::from([1, 0]);
    assert!(wait_for_non_http_probe(
        &deployer,
        "worker",
        &probe("exec"),
        None,
        Duration::from_secs(3),
        None,
    )
    .await
    .is_ok());
    *deployer.exits.lock().unwrap() = VecDeque::from([127]);
    let error = wait_for_non_http_probe(
        &deployer,
        "worker",
        &probe("exec"),
        None,
        Duration::from_secs(3),
        None,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("health.command"));
    *deployer.exits.lock().unwrap() = VecDeque::from([1]);
    assert!(wait_for_non_http_probe(
        &deployer,
        "worker",
        &probe("exec"),
        None,
        Duration::from_millis(20),
        None,
    )
    .await
    .is_err());
}

struct ProbeLog(Mutex<Vec<String>>);

#[async_trait]
impl crate::workflow::LogWriter for ProbeLog {
    async fn write_log(&self, message: String) -> Result<(), WorkflowError> {
        self.0.lock().unwrap().push(message);
        Ok(())
    }
    fn stage_id(&self) -> i32 {
        1
    }
}

#[tokio::test]
async fn health_probe_exec_output_is_only_written_to_authorized_job_log() {
    let deployer = Arc::new(ProbeDeployer::running());
    *deployer.exits.lock().unwrap() = VecDeque::from([127]);
    let job = DeployImageJobBuilder::new(
        "worker",
        temps_core::docker_socket_grant::DeployCaller::Platform,
    )
    .job_id("deploy".into())
    .build_job_id("build".into())
    .target(DeploymentTarget::Docker {
        registry_url: "local".into(),
        network: None,
    })
    .service_name("worker".into())
    .build(deployer.clone())
    .unwrap();
    let log = Arc::new(ProbeLog(Mutex::new(Vec::new())));
    let context = WorkflowContext::new("worker".into(), 1, 1, 1, log.clone());
    let error = wait_for_non_http_probe(
        deployer.as_ref(),
        "worker",
        &probe("exec"),
        None,
        Duration::from_secs(2),
        Some((&job, &context)),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(!error.contains("probe output"));
    assert!(log
        .0
        .lock()
        .unwrap()
        .iter()
        .any(|line| line.contains("probe output")));
}

#[tokio::test]
async fn health_probe_exec_output_never_enters_failure_details() {
    for exit in [1, 126, 127] {
        let deployer = ProbeDeployer::running();
        *deployer.exits.lock().unwrap() = VecDeque::from([exit]);
        let error = wait_for_non_http_probe(
            &deployer,
            "worker",
            &probe("exec"),
            None,
            Duration::from_millis(20),
            None,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains(&exit.to_string()), "{error}");
        assert!(
            !error.contains("probe output"),
            "exec output must only appear in authorized job logs"
        );
    }
}

#[tokio::test]
async fn health_probe_runtime_diagnostics_are_not_persisted() {
    let mut deployer = ProbeDeployer::running();
    deployer.exec_error = true;
    let error = wait_for_non_http_probe(
        &deployer,
        "worker",
        &probe("exec"),
        None,
        Duration::from_secs(2),
        None,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("exec probe failed"));
    assert!(!error.contains("probe output"));
}

#[tokio::test]
async fn health_probe_exec_timeout_crash_during_command_and_unknown_kind_fail_closed() {
    let mut deployer = ProbeDeployer::running();
    deployer.exec_timeout = true;
    assert!(wait_for_non_http_probe(
        &deployer,
        "worker",
        &probe("exec"),
        None,
        Duration::from_secs(2),
        None,
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("timeout"));
    let deployer = ProbeDeployer::running();
    let mut exited = deployer.states.lock().unwrap().front().unwrap().clone();
    exited.status = DeployerContainerStatus::Exited;
    deployer.states.lock().unwrap().push_back(exited);
    assert!(wait_for_non_http_probe(
        &deployer,
        "worker",
        &probe("exec"),
        None,
        Duration::from_secs(2),
        None,
    )
    .await
    .is_err());
    assert!(wait_for_non_http_probe(
        &deployer,
        "worker",
        &probe("typo"),
        None,
        Duration::from_secs(2),
        None,
    )
    .await
    .is_err());
    let mut malformed = probe("exec");
    malformed.command.clear();
    assert!(wait_for_non_http_probe(
        &deployer,
        "worker",
        &malformed,
        None,
        Duration::from_secs(2),
        None,
    )
    .await
    .is_err());
}

#[tokio::test]
async fn health_probe_tcp_success_refused_connection_and_missing_port() {
    let deployer = ProbeDeployer::running();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    assert!(wait_for_non_http_probe(
        &deployer,
        "tcp-service",
        &probe("tcp"),
        Some(&address),
        Duration::from_secs(1),
        None,
    )
    .await
    .is_ok());
    drop(listener);
    assert!(wait_for_non_http_probe(
        &deployer,
        "tcp-service",
        &probe("tcp"),
        Some(&address),
        Duration::from_millis(20),
        None,
    )
    .await
    .is_err());
    assert!(wait_for_non_http_probe(
        &deployer,
        "tcp-service",
        &probe("tcp"),
        None,
        Duration::from_secs(1),
        None,
    )
    .await
    .is_err());
}

#[tokio::test]
async fn health_probe_tcp_rejects_a_forwarder_that_accepts_then_closes() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let forwarder = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            drop(stream);
        }
    });
    let error = wait_for_non_http_probe(
        &ProbeDeployer::running(),
        "tcp-service",
        &probe("tcp"),
        Some(&address),
        Duration::from_millis(250),
        None,
    )
    .await
    .unwrap_err();
    forwarder.abort();
    assert!(error.to_string().contains("TCP connection closed"));
}

#[tokio::test]
async fn health_probe_tcp_confirmation_accepts_a_banner_and_honors_deadlines() {
    use tokio::io::AsyncWriteExt;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stream = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (mut server, _) = listener.accept().await.unwrap();
    assert!(confirm_tcp_probe_connection(
        &stream,
        tokio::time::Instant::now() + Duration::from_millis(10)
    )
    .await
    .is_err());
    server.write_all(b"ready\n").await.unwrap();
    assert!(confirm_tcp_probe_connection(
        &stream,
        tokio::time::Instant::now() + Duration::from_secs(1)
    )
    .await
    .is_ok());
}
