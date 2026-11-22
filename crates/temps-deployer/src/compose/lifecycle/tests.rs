// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tests for [`super`].

use super::*;
use crate::compose::test_support::{disabled_executor, secrets_test_request, test_executor};
use crate::compose::{
    ComposeDeployRequest, ComposeError, ComposeExecutor, PreparedComposeDeploy,
    COMPOSE_PROJECT_LABEL,
};
use bollard::Docker;

/// Removes everything a test's Compose project created when the test ends, however
/// it ends. A `restart: always` container left behind by a panic would keep running.
struct ComposeStackCleanup {
    project_name: String,
    project_dir: std::path::PathBuf,
}

impl ComposeStackCleanup {
    fn new(project_name: &str, project_dir: &std::path::Path) -> Self {
        Self {
            project_name: project_name.to_string(),
            project_dir: project_dir.to_path_buf(),
        }
    }
}

/// Longest a test's cleanup may take. `Drop` is synchronous, so no async timeout
/// around the test could interrupt it if Docker stopped answering.
const COMPOSE_CLEANUP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// Run `command` until `deadline`, killing it if it is still running then.
/// `None` when it could not be started or ran out of time.
fn output_until(
    mut command: std::process::Command,
    deadline: std::time::Instant,
) -> Option<std::process::Output> {
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

impl Drop for ComposeStackCleanup {
    fn drop(&mut self) {
        let deadline = std::time::Instant::now() + COMPOSE_CLEANUP_DEADLINE;

        // By label first: this also catches one-off `run` containers.
        let mut list = std::process::Command::new("docker");
        list.args(["ps", "--all", "--quiet", "--filter"])
            .arg(format!(
                "label={COMPOSE_PROJECT_LABEL}={}",
                self.project_name
            ));
        if let Some(listed) = output_until(list, deadline) {
            let stdout = String::from_utf8_lossy(&listed.stdout);
            let containers: Vec<&str> = stdout.split_whitespace().collect();
            if !containers.is_empty() {
                let mut remove = std::process::Command::new("docker");
                remove
                    .args(["rm", "--force", "--volumes"])
                    .args(&containers);
                let _ = output_until(remove, deadline);
            }
        }

        // Then the project's networks and volumes.
        let mut down = std::process::Command::new("docker");
        down.args(["compose", "-p", &self.project_name])
            .args(["down", "--volumes", "--remove-orphans"])
            .current_dir(&self.project_dir);
        let _ = output_until(down, deadline);
    }
}

#[test]
fn a_cleanup_command_that_stalls_is_killed_at_the_deadline() {
    let mut stalled = std::process::Command::new("sleep");
    stalled.arg("30");
    let started = std::time::Instant::now();

    let output = output_until(stalled, started + std::time::Duration::from_millis(100));

    assert!(output.is_none());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "the command ran for {:?}",
        started.elapsed()
    );
}

#[test]
fn a_cleanup_command_that_finishes_returns_its_output() {
    let mut command = std::process::Command::new("sh");
    command.args(["-c", "echo listed"]);

    let output = output_until(
        command,
        std::time::Instant::now() + std::time::Duration::from_secs(30),
    )
    .expect("the command finishes well before the deadline");

    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "listed");
}

#[tokio::test]
async fn a_test_that_panics_still_removes_its_compose_stack() {
    let Some(executor) = test_executor() else {
        return;
    };
    let docker = executor
        .docker
        .require()
        .expect("test_executor only builds an available handle")
        .clone();
    let compose_available = tokio::process::Command::new("docker")
        .args(["compose", "version"])
        .output()
        .await
        .is_ok_and(|output| output.status.success());
    if !compose_available || docker.inspect_image("alpine:latest").await.is_err() {
        println!("Docker Compose or alpine:latest is unavailable; skipping runtime test");
        return;
    }

    let project_dir = tempfile::tempdir().unwrap();
    let project_name = format!(
        "temps-stop-cleanup-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    );
    std::fs::write(
        project_dir.path().join("docker-compose.yml"),
        "services:\n  web:\n    image: alpine:latest\n    restart: always\n    command: [\"sleep\", \"300\"]\n",
    )
    .unwrap();
    let cleanup = ComposeStackCleanup::new(&project_name, project_dir.path());
    let up = tokio::process::Command::new("docker")
        .args(["compose", "-p", &project_name, "up", "-d"])
        .current_dir(project_dir.path())
        .output()
        .await
        .unwrap();
    assert!(
        up.status.success(),
        "{}",
        String::from_utf8_lossy(&up.stderr)
    );

    let panicked = tokio::spawn(async move {
        let _cleanup = cleanup;
        panic!("a failure partway through a test");
    })
    .await;
    assert!(panicked.is_err());

    let remaining = docker
        .list_containers(Some(bollard::query_parameters::ListContainersOptions {
            all: true,
            filters: Some(HashMap::from([(
                "label".to_string(),
                vec![format!("{COMPOSE_PROJECT_LABEL}={project_name}")],
            )])),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert!(remaining.is_empty(), "{remaining:?}");
}

#[tokio::test]
async fn a_stopped_stack_will_not_be_restarted_by_docker() {
    let Some(executor) = test_executor() else {
        return;
    };
    let docker = executor
        .docker
        .require()
        .expect("test_executor only builds an available handle")
        .clone();
    let compose_available = tokio::process::Command::new("docker")
        .args(["compose", "version"])
        .output()
        .await
        .is_ok_and(|output| output.status.success());
    if !compose_available || docker.inspect_image("alpine:latest").await.is_err() {
        println!("Docker Compose or alpine:latest is unavailable; skipping runtime test");
        return;
    }

    let project_dir = tempfile::tempdir().unwrap();
    let project_name = format!(
        "temps-stop-no-restart-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    );
    std::fs::write(
        project_dir.path().join("docker-compose.yml"),
        "services:\n  web:\n    image: alpine:latest\n    restart: always\n    command: [\"sleep\", \"300\"]\n",
    )
    .unwrap();
    let compose = |args: &[&str]| {
        let mut command = tokio::process::Command::new("docker");
        command
            .args(["compose", "-p", &project_name])
            .args(args)
            .current_dir(project_dir.path());
        command
    };
    let _cleanup = ComposeStackCleanup::new(&project_name, project_dir.path());
    let up = compose(&["up", "-d"]).output().await.unwrap();
    assert!(
        up.status.success(),
        "{}",
        String::from_utf8_lossy(&up.stderr)
    );
    // An operator's one-off container is not part of the stack.
    let one_off = compose(&["run", "-d", "--rm", "web"])
        .output()
        .await
        .unwrap();
    assert!(
        one_off.status.success(),
        "{}",
        String::from_utf8_lossy(&one_off.stderr)
    );

    let stopped = executor
        .stop_at(
            &project_name,
            Some(project_dir.path()),
            Some("docker-compose.yml"),
        )
        .await;

    let containers = docker
        .list_containers(Some(bollard::query_parameters::ListContainersOptions {
            all: true,
            filters: Some(HashMap::from([(
                "label".to_string(),
                vec![format!("{COMPOSE_PROJECT_LABEL}={project_name}")],
            )])),
            ..Default::default()
        }))
        .await
        .unwrap();
    let mut policies = Vec::new();
    for container in &containers {
        let one_off_container = container
            .labels
            .as_ref()
            .and_then(|labels| labels.get("com.docker.compose.oneoff"))
            .is_some_and(|value| value == "True");
        if one_off_container {
            continue;
        }
        let inspected = docker
            .inspect_container(
                container.id.as_deref().unwrap(),
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .unwrap();
        policies.push((
            inspected.state.and_then(|state| state.running),
            inspected
                .host_config
                .and_then(|config| config.restart_policy)
                .and_then(|policy| policy.name),
        ));
    }

    stopped.expect("stop should succeed");
    assert_eq!(
        policies,
        vec![(
            Some(false),
            Some(bollard::models::RestartPolicyNameEnum::NO)
        )]
    );
}

#[tokio::test]
async fn a_failed_stop_leaves_running_containers_their_restart_policy() {
    let Some(executor) = test_executor() else {
        return;
    };
    let docker = executor
        .docker
        .require()
        .expect("test_executor only builds an available handle")
        .clone();
    let compose_available = tokio::process::Command::new("docker")
        .args(["compose", "version"])
        .output()
        .await
        .is_ok_and(|output| output.status.success());
    if !compose_available || docker.inspect_image("alpine:latest").await.is_err() {
        println!("Docker Compose or alpine:latest is unavailable; skipping runtime test");
        return;
    }

    let project_dir = tempfile::tempdir().unwrap();
    let project_name = format!(
        "temps-stop-fails-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    );
    std::fs::write(
        project_dir.path().join("docker-compose.yml"),
        "services:\n  web:\n    image: alpine:latest\n    restart: always\n    command: [\"sleep\", \"300\"]\n",
    )
    .unwrap();
    let compose = |args: &[&str]| {
        let mut command = tokio::process::Command::new("docker");
        command
            .args(["compose", "-p", &project_name])
            .args(args)
            .current_dir(project_dir.path());
        command
    };
    let _cleanup = ComposeStackCleanup::new(&project_name, project_dir.path());
    let up = compose(&["up", "-d"]).output().await.unwrap();
    assert!(
        up.status.success(),
        "{}",
        String::from_utf8_lossy(&up.stderr)
    );

    let stopped = executor
        .stop_at(
            &project_name,
            Some(project_dir.path()),
            Some("no-such-compose-file.yml"),
        )
        .await;

    let containers = docker
        .list_containers(Some(bollard::query_parameters::ListContainersOptions {
            all: true,
            filters: Some(HashMap::from([(
                "label".to_string(),
                vec![format!("{COMPOSE_PROJECT_LABEL}={project_name}")],
            )])),
            ..Default::default()
        }))
        .await
        .unwrap();
    let inspected = docker
        .inspect_container(
            containers[0].id.as_deref().unwrap(),
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
        .unwrap();

    stopped.expect_err("the Compose file does not exist");
    assert_eq!(inspected.state.and_then(|state| state.running), Some(true));
    assert_eq!(
        inspected
            .host_config
            .and_then(|config| config.restart_policy)
            .and_then(|policy| policy.name),
        Some(bollard::models::RestartPolicyNameEnum::ALWAYS)
    );
}

#[tokio::test]
async fn a_running_service_outside_the_compose_file_keeps_its_restart_policy() {
    let Some(executor) = test_executor() else {
        return;
    };
    let docker = executor
        .docker
        .require()
        .expect("test_executor only builds an available handle")
        .clone();
    let compose_available = tokio::process::Command::new("docker")
        .args(["compose", "version"])
        .output()
        .await
        .is_ok_and(|output| output.status.success());
    if !compose_available || docker.inspect_image("alpine:latest").await.is_err() {
        println!("Docker Compose or alpine:latest is unavailable; skipping runtime test");
        return;
    }

    let project_dir = tempfile::tempdir().unwrap();
    let project_name = format!(
        "temps-stop-orphan-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    );
    let service = |name: &str| {
        format!(
            "  {name}:\n    image: alpine:latest\n    restart: always\n    command: [\"sleep\", \"300\"]\n"
        )
    };
    // The running deployment has a service the next one dropped.
    std::fs::write(
        project_dir.path().join("docker-compose.yml"),
        format!("services:\n{}{}", service("web"), service("retired")),
    )
    .unwrap();
    std::fs::write(
        project_dir.path().join("next.yml"),
        format!("services:\n{}", service("web")),
    )
    .unwrap();
    let compose = |args: &[&str]| {
        let mut command = tokio::process::Command::new("docker");
        command
            .args(["compose", "-p", &project_name])
            .args(args)
            .current_dir(project_dir.path());
        command
    };
    let _cleanup = ComposeStackCleanup::new(&project_name, project_dir.path());
    let up = compose(&["up", "-d"]).output().await.unwrap();
    assert!(
        up.status.success(),
        "{}",
        String::from_utf8_lossy(&up.stderr)
    );

    let stopped = executor
        .stop_at(&project_name, Some(project_dir.path()), Some("next.yml"))
        .await;

    let containers = docker
        .list_containers(Some(bollard::query_parameters::ListContainersOptions {
            all: true,
            filters: Some(HashMap::from([(
                "label".to_string(),
                vec![format!("{COMPOSE_PROJECT_LABEL}={project_name}")],
            )])),
            ..Default::default()
        }))
        .await
        .unwrap();
    let mut services = Vec::new();
    for container in &containers {
        let name = container
            .labels
            .as_ref()
            .and_then(|labels| labels.get("com.docker.compose.service"))
            .cloned();
        let inspected = docker
            .inspect_container(
                container.id.as_deref().unwrap(),
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .unwrap();
        services.push((
            name,
            inspected.state.and_then(|state| state.running),
            inspected
                .host_config
                .and_then(|config| config.restart_policy)
                .and_then(|policy| policy.name),
        ));
    }
    services.sort_by(|a, b| a.0.cmp(&b.0));

    stopped.expect("stop should succeed");
    assert_eq!(
        services,
        vec![
            (
                Some("retired".to_string()),
                Some(true),
                Some(bollard::models::RestartPolicyNameEnum::ALWAYS)
            ),
            (
                Some("web".to_string()),
                Some(false),
                Some(bollard::models::RestartPolicyNameEnum::NO)
            ),
        ]
    );
}

#[tokio::test]
async fn failed_compose_startup_returns_containers_without_removing_them() {
    let Some(executor) = test_executor() else {
        return;
    };
    let compose_available = tokio::process::Command::new("docker")
        .args(["compose", "version"])
        .output()
        .await
        .is_ok_and(|output| output.status.success());
    if !compose_available
        || executor
            .docker
            .require()
            .expect("test_executor only builds an available handle")
            .inspect_image("alpine:latest")
            .await
            .is_err()
    {
        println!("Docker Compose or alpine:latest is unavailable; skipping runtime test");
        return;
    }

    let project_dir = tempfile::tempdir().unwrap();
    let project_name = format!(
        "temps-failed-retention-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    );
    let compose = r#"
services:
  worker:
    image: alpine:latest
    command: ["sh", "-c", "echo retained-compose-diagnostic; sleep 30"]
    healthcheck:
      test: ["CMD", "false"]
      interval: 1s
      timeout: 1s
      retries: 1
"#;
    let request = ComposeDeployRequest {
        output_sink: None,
        project_name: project_name.clone(),
        compose_content: compose.to_string(),
        env_content: None,
        work_dir: project_dir.path().to_path_buf(),
        compose_path: None,
        environment_vars: HashMap::new(),
        secrets: HashMap::new(),
        secret_compose_services: HashMap::new(),
        build_args: HashMap::new(),
        labels: HashMap::from([
            ("sh.temps.managed".to_string(), "true".to_string()),
            ("sh.temps.project_id".to_string(), "42".to_string()),
            ("sh.temps.environment".to_string(), "7".to_string()),
        ]),
        repo_dir: None,
        compose_override: None,
        relaxed_capability_services: Vec::new(),
        unsandboxed_services: Vec::new(),
        ready_timeout: None,
        routed_services: Vec::new(),
    };
    executor
        .write_compose_files(project_dir.path(), &request, "failed-test")
        .await
        .unwrap();
    let prepared = PreparedComposeDeploy {
        effective_dir: project_dir.path().to_path_buf(),
        project_name: project_name.clone(),
        compose_file: "docker-compose.yml".to_string(),
        redact_values: Vec::new(),
        secret_generation: "failed-test".to_string(),
        has_materialized_secrets: false,
    };

    let failure = executor
        .deploy_prepared(prepared, &request)
        .await
        .expect_err("unhealthy service must fail readiness");

    assert!(
        matches!(
            &failure.error,
            ComposeError::CommandFailed { .. } | ComposeError::ServicesNotReady { .. }
        ),
        "unexpected startup failure: {}",
        failure.error
    );
    assert_eq!(
        failure.containers.len(),
        1,
        "startup error: {}",
        failure.error
    );
    assert!(failure.cleanup_containers.is_empty());
    assert!(failure.retention_error.is_none());
    assert_eq!(failure.containers[0].service_name, "worker");
    assert_eq!(failure.containers[0].status, "running");
    let retained_id = failure.containers[0].container_id.clone();
    let inspect = executor
        .docker
        .require()
        .expect("test_executor only builds an available handle")
        .inspect_container(&retained_id, None)
        .await
        .expect("failed candidate must still exist for log inspection");
    let labels = inspect
        .config
        .and_then(|config| config.labels)
        .unwrap_or_default();
    assert_eq!(
        labels.get("sh.temps.managed").map(String::as_str),
        Some("true")
    );
    let logs = executor.container_log_tail(&retained_id).await;
    assert!(
        logs.contains("retained-compose-diagnostic"),
        "logs were: {logs}"
    );

    executor
        .teardown_at(
            &project_name,
            Some(project_dir.path()),
            Some("docker-compose.yml"),
            &HashMap::new(),
            true,
        )
        .await
        .expect("runtime test must clean up its retained stack");
}

#[tokio::test]
async fn quickly_exiting_compose_service_never_reports_ready() {
    let Some(executor) = test_executor() else {
        return;
    };
    let compose_available = tokio::process::Command::new("docker")
        .args(["compose", "version"])
        .output()
        .await
        .is_ok_and(|output| output.status.success());
    if !compose_available
        || executor
            .docker
            .require()
            .expect("test_executor only builds an available handle")
            .inspect_image("alpine:latest")
            .await
            .is_err()
    {
        println!("Docker Compose or alpine:latest is unavailable; skipping runtime test");
        return;
    }

    let project_dir = tempfile::tempdir().unwrap();
    let project_name = format!(
        "temps-quick-exit-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    );
    let compose = r#"
services:
  worker:
    image: alpine:latest
    command: ["sh", "-c", "echo quick-exit-diagnostic; exit 17"]
"#;
    let request = ComposeDeployRequest {
        output_sink: None,
        project_name: project_name.clone(),
        compose_content: compose.to_string(),
        env_content: None,
        work_dir: project_dir.path().to_path_buf(),
        compose_path: None,
        environment_vars: HashMap::new(),
        secrets: HashMap::new(),
        secret_compose_services: HashMap::new(),
        build_args: HashMap::new(),
        labels: HashMap::from([
            ("sh.temps.managed".to_string(), "true".to_string()),
            ("sh.temps.project_id".to_string(), "42".to_string()),
            ("sh.temps.environment".to_string(), "7".to_string()),
        ]),
        repo_dir: None,
        compose_override: None,
        relaxed_capability_services: Vec::new(),
        unsandboxed_services: Vec::new(),
        ready_timeout: None,
        routed_services: Vec::new(),
    };
    executor
        .write_compose_files(project_dir.path(), &request, "quick-exit")
        .await
        .unwrap();
    let prepared = PreparedComposeDeploy {
        effective_dir: project_dir.path().to_path_buf(),
        project_name: project_name.clone(),
        compose_file: "docker-compose.yml".to_string(),
        redact_values: Vec::new(),
        secret_generation: "quick-exit".to_string(),
        has_materialized_secrets: false,
    };

    let failure = executor
        .deploy_prepared(prepared, &request)
        .await
        .expect_err("a quickly exiting service must never be reported ready");
    assert!(
        matches!(failure.error, ComposeError::ServicesNotReady { .. }),
        "startup error: {}",
        failure.error
    );
    assert_eq!(
        failure.containers.len(),
        1,
        "startup error: {}",
        failure.error
    );
    assert_eq!(failure.containers[0].status, "exited");
    let logs = executor
        .container_log_tail(&failure.containers[0].container_id)
        .await;
    assert!(logs.contains("quick-exit-diagnostic"), "logs were: {logs}");

    executor
        .teardown_at(
            &project_name,
            Some(project_dir.path()),
            Some("docker-compose.yml"),
            &HashMap::new(),
            true,
        )
        .await
        .expect("runtime test must clean up its retained stack");
}

#[tokio::test]
async fn failed_compose_with_public_host_binding_is_not_retainable() {
    let Some(executor) = test_executor() else {
        return;
    };
    let compose_available = tokio::process::Command::new("docker")
        .args(["compose", "version"])
        .output()
        .await
        .is_ok_and(|output| output.status.success());
    if !compose_available
        || executor
            .docker
            .require()
            .expect("test_executor only builds an available handle")
            .inspect_image("alpine:latest")
            .await
            .is_err()
    {
        println!("Docker Compose or alpine:latest is unavailable; skipping runtime test");
        return;
    }

    let project_dir = tempfile::tempdir().unwrap();
    let project_name = format!(
        "temps-failed-public-port-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    );
    let compose = r#"
services:
  worker:
    image: alpine:latest
    command: ["sh", "-c", "sleep 30"]
    ports:
      - "0.0.0.0::8080"
    healthcheck:
      test: ["CMD", "false"]
      interval: 1s
      timeout: 1s
      retries: 1
"#;
    let request = ComposeDeployRequest {
        output_sink: None,
        project_name: project_name.clone(),
        compose_content: compose.to_string(),
        env_content: None,
        work_dir: project_dir.path().to_path_buf(),
        compose_path: None,
        environment_vars: HashMap::new(),
        secrets: HashMap::new(),
        secret_compose_services: HashMap::new(),
        build_args: HashMap::new(),
        labels: HashMap::from([
            ("sh.temps.managed".to_string(), "true".to_string()),
            ("sh.temps.project_id".to_string(), "42".to_string()),
            ("sh.temps.environment".to_string(), "7".to_string()),
        ]),
        repo_dir: None,
        compose_override: None,
        relaxed_capability_services: Vec::new(),
        unsandboxed_services: Vec::new(),
        ready_timeout: None,
        routed_services: Vec::new(),
    };
    executor
        .write_compose_files(project_dir.path(), &request, "failed-public-port")
        .await
        .unwrap();
    let prepared = PreparedComposeDeploy {
        effective_dir: project_dir.path().to_path_buf(),
        project_name: project_name.clone(),
        compose_file: "docker-compose.yml".to_string(),
        redact_values: Vec::new(),
        secret_generation: "failed-public-port".to_string(),
        has_materialized_secrets: false,
    };

    let failure = executor
        .deploy_prepared(prepared, &request)
        .await
        .expect_err("unhealthy service must fail readiness");
    assert!(failure.containers.is_empty());
    assert_eq!(
        failure.cleanup_containers.len(),
        1,
        "startup error: {}",
        failure.error
    );
    let unsafe_container_id = failure.cleanup_containers[0].container_id.clone();
    let retention_error = failure
        .retention_error
        .expect("public host binding must block retention")
        .to_string();
    assert!(retention_error.contains("0.0.0.0"));
    assert!(retention_error.contains("loopback"));

    executor
        .remove_verified_containers(&failure.cleanup_containers, &request.labels)
        .await
        .expect("ownership-verified direct cleanup must remove the unsafe container");
    let simulated_teardown_failure = executor
        .teardown_at(
            &project_name,
            Some(project_dir.path()),
            Some("missing-compose.yml"),
            &HashMap::new(),
            true,
        )
        .await;
    assert!(simulated_teardown_failure.is_err());
    assert!(matches!(
        executor
            .docker
            .require()
            .expect("test_executor only builds an available handle")
            .inspect_container(&unsafe_container_id, None)
            .await,
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404,
            ..
        })
    ));

    executor
        .teardown_at(
            &project_name,
            Some(project_dir.path()),
            Some("docker-compose.yml"),
            &HashMap::new(),
            true,
        )
        .await
        .expect("runtime test must clean up the unsafe failed stack");
}

/// A Git deployment's checkout is gone by the time a later deployment
/// stops its stack (moving it to another node), so teardown has no
/// Compose file to run `down` from. It must still stop and remove the
/// project's containers, by label, and keep its volumes.
#[tokio::test]
async fn test_teardown_without_a_project_dir_removes_labelled_containers_and_keeps_volumes() {
    use bollard::models::{ContainerCreateBody, HostConfig};
    use bollard::query_parameters::{
        CreateContainerOptionsBuilder, CreateImageOptionsBuilder, ListContainersOptions,
        RemoveVolumeOptions,
    };
    use futures::StreamExt;

    let Ok(docker) = Docker::connect_with_local_defaults() else {
        println!("Docker not available, skipping");
        return;
    };
    if docker.ping().await.is_err() {
        println!("Docker not available, skipping");
        return;
    }
    const IMAGE: &str = "busybox:1.36";
    if docker.inspect_image(IMAGE).await.is_err() {
        let mut pull = docker.create_image(
            Some(CreateImageOptionsBuilder::new().from_image(IMAGE).build()),
            None,
            None,
        );
        while let Some(progress) = pull.next().await {
            if progress.is_err() {
                println!("Cannot pull {IMAGE}, skipping");
                return;
            }
        }
    }

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let project = format!("temps-sweep-{}-{nanos}", std::process::id());
    let volume = format!("{project}_data");
    let container_name = format!("{project}-web-1");
    let labels = HashMap::from([(COMPOSE_PROJECT_LABEL.to_string(), project.clone())]);

    // Every step that creates something runs inside this block, and the
    // cleanup below runs whichever step failed: a setup error must not
    // leave a container or volume behind on the developer's daemon.
    let outcome: Result<(Vec<bollard::models::ContainerSummary>, bool), String> = async {
        docker
            .create_volume(bollard::models::VolumeCreateRequest {
                name: Some(volume.clone()),
                labels: Some(labels.clone()),
                ..Default::default()
            })
            .await
            .map_err(|error| format!("create volume {volume}: {error}"))?;
        let container = docker
            .create_container(
                Some(
                    CreateContainerOptionsBuilder::new()
                        .name(&container_name)
                        .build(),
                ),
                ContainerCreateBody {
                    image: Some(IMAGE.to_string()),
                    cmd: Some(vec!["sleep".to_string(), "600".to_string()]),
                    labels: Some(labels.clone()),
                    host_config: Some(HostConfig {
                        binds: Some(vec![format!("{volume}:/data")]),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| format!("create container {container_name}: {error}"))?;
        docker
            .start_container(
                &container.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|error| format!("start container {container_name}: {error}"))?;

        let data_dir =
            tempfile::tempdir().map_err(|error| format!("create executor data dir: {error}"))?;
        let executor =
            ComposeExecutor::new(Arc::new(docker.clone()), data_dir.path().to_path_buf());
        if executor.project_dir(&project).exists() {
            return Err(format!("project dir for {project} unexpectedly exists"));
        }
        executor
            .teardown_at(&project, None, None, &HashMap::new(), true)
            .await
            .map_err(|error| format!("teardown of {project}: {error}"))?;

        let remaining = docker
            .list_containers(Some(ListContainersOptions {
                all: true,
                filters: Some(HashMap::from([(
                    "label".to_string(),
                    vec![format!("{COMPOSE_PROJECT_LABEL}={project}")],
                )])),
                ..Default::default()
            }))
            .await
            .map_err(|error| format!("list containers of {project}: {error}"))?;
        Ok((remaining, docker.inspect_volume(&volume).await.is_ok()))
    }
    .await;

    // By name, not by an id a failed step may never have returned.
    let _ = docker
        .remove_container(
            &container_name,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;
    let _ = docker
        .remove_volume(&volume, None::<RemoveVolumeOptions>)
        .await;

    let (remaining, volume_kept) = outcome.unwrap();
    assert!(remaining.is_empty(), "containers survived: {remaining:?}");
    assert!(volume_kept, "the stack's volume {volume} was removed");
}

#[tokio::test]
async fn test_project_lifecycle_lock_serializes_distinct_executor_instances() {
    let docker = Arc::new(
        Docker::connect_with_local_defaults()
            .expect("constructing the Docker client should not contact the daemon"),
    );
    let data_dir = tempfile::tempdir().unwrap();
    let first_executor = ComposeExecutor::new(docker.clone(), data_dir.path().to_path_buf());
    let second_executor = ComposeExecutor::new(docker, data_dir.path().to_path_buf());

    let first = first_executor
        .acquire_project_lifecycle_lock("temps-1-2")
        .await;
    let blocked = tokio::time::timeout(
        std::time::Duration::from_millis(25),
        second_executor.acquire_project_lifecycle_lock("temps-1-2"),
    )
    .await;
    assert!(
        blocked.is_err(),
        "a second executor entered the same project lifecycle concurrently"
    );

    drop(first);
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        second_executor.acquire_project_lifecycle_lock("temps-1-2"),
    )
    .await
    .expect("the next workflow should acquire the project after release");
}

/// `teardown_at` with `remove_secrets: false` must preserve the on-disk
/// secrets directory even when the project stack directory does not exist
/// (the early-return path is taken, which is what allows this test to run
/// without Docker).
#[tokio::test]
async fn test_teardown_at_remove_secrets_false_preserves_secrets_dir() {
    let docker = Docker::connect_with_defaults();
    if docker.is_err() {
        // Docker unavailable — we can still run this test because
        // teardown_at returns early when the project directory does not
        // exist, before it attempts any Docker call.
    }
    // Use a real temporary directory so the secrets-dir assertion is
    // meaningful (we are asserting on actual filesystem state).
    let tmp = tempfile::tempdir().expect("create temp dir");
    let data_dir = tmp.path().to_path_buf();

    // Build a minimal ComposeExecutor backed by a placeholder Docker
    // handle. teardown_at returns early before any Docker call when the
    // project directory does not exist, so the handle is never used.
    let docker_handle = match Docker::connect_with_defaults() {
        Ok(d) => Arc::new(d),
        Err(_) => {
            // No Docker: synthesise a handle via connect_with_local_defaults
            // which also won't be called. If even that fails, skip.
            match Docker::connect_with_local_defaults() {
                Ok(d) => Arc::new(d),
                Err(_) => return,
            }
        }
    };
    let executor = ComposeExecutor::new(docker_handle, data_dir.clone());

    // Manually create the secrets directory that teardown_at would
    // otherwise delete, mirroring what materialize_secrets() produces.
    let project_name = "test-remove-secrets-false";
    let secrets_dir = data_dir.join("compose-secrets").join(project_name);
    std::fs::create_dir_all(&secrets_dir).expect("create secrets dir");
    std::fs::write(secrets_dir.join("MY_SECRET"), b"hunter2").expect("write dummy secret");

    // Project directory intentionally does not exist → teardown_at returns
    // early after (conditionally) running the secrets-dir deletion logic.
    executor
        .teardown_at(project_name, None, None, &HashMap::new(), false)
        .await
        .expect("teardown_at should succeed with no project dir");

    assert!(
        secrets_dir.exists(),
        "secrets dir must still exist after teardown_at with remove_secrets=false"
    );
}

/// `teardown_at` with `remove_secrets: true` must delete the on-disk
/// secrets directory (same early-return path as above, no Docker needed).
#[tokio::test]
async fn test_teardown_at_remove_secrets_true_deletes_secrets_dir() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let data_dir = tmp.path().to_path_buf();

    let docker_handle = match Docker::connect_with_defaults() {
        Ok(d) => Arc::new(d),
        Err(_) => match Docker::connect_with_local_defaults() {
            Ok(d) => Arc::new(d),
            Err(_) => return,
        },
    };
    let executor = ComposeExecutor::new(docker_handle, data_dir.clone());

    let project_name = "test-remove-secrets-true";
    let secrets_dir = data_dir.join("compose-secrets").join(project_name);
    std::fs::create_dir_all(&secrets_dir).expect("create secrets dir");
    std::fs::write(secrets_dir.join("MY_SECRET"), b"hunter2").expect("write dummy secret");

    executor
        .teardown_at(project_name, None, None, &HashMap::new(), true)
        .await
        .expect("teardown_at should succeed with no project dir");

    assert!(
        !secrets_dir.exists(),
        "secrets dir must be deleted after teardown_at with remove_secrets=true"
    );
}

// --- Control-plane profile: no Docker client at all ---
//
// These tests build a `ComposeExecutor` from `DockerHandle::disabled`,
// never connecting to or even attempting to construct a real bollard
// client. They run unconditionally (no `test_executor()`/Docker skip),
// and assert both that the typed `DockerUnavailable` error surfaces and
// that it does so before any `docker compose` CLI invocation or bollard
// call -- a hang or a raw "command not found"/connection-refused error
// instead of this typed variant would mean a guard was bypassed.

#[tokio::test]
async fn disabled_handle_fails_prepare_and_pull_before_writing_any_compose_file() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let data_dir = tmp.path().to_path_buf();
    let executor = disabled_executor(data_dir.clone());

    let request = secrets_test_request(
        "disabled-handle-project",
        "services:\n  web:\n    image: alpine:latest\n",
        HashMap::new(),
    );

    let error = match executor.prepare_and_pull(&request).await {
        Ok(_) => panic!("no Docker client exists in this process"),
        Err(error) => error,
    };

    assert!(
        matches!(error, ComposeError::DockerUnavailable(_)),
        "expected DockerUnavailable, got {error:?}"
    );
    // The guard runs before `write_compose_files`: nothing should have
    // been written to disk for a project that was never touched.
    assert!(
        !data_dir
            .join("compose")
            .join("disabled-handle-project")
            .exists(),
        "prepare_and_pull must fail before writing any compose files"
    );
}

#[tokio::test]
async fn disabled_handle_fails_deploy_without_ever_shelling_out_to_docker_compose() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let executor = disabled_executor(tmp.path().to_path_buf());

    let request = secrets_test_request(
        "disabled-handle-deploy",
        "services:\n  web:\n    image: alpine:latest\n",
        HashMap::new(),
    );

    let error = executor
        .deploy(request)
        .await
        .expect_err("no Docker client exists in this process");
    assert!(
        matches!(error, ComposeError::DockerUnavailable(_)),
        "expected DockerUnavailable, got {error:?}"
    );
}

#[tokio::test]
async fn disabled_handle_fails_destroy() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let executor = disabled_executor(tmp.path().to_path_buf());

    let error = executor
        .destroy("disabled-handle-destroy")
        .await
        .expect_err("no Docker client exists in this process");
    assert!(
        matches!(error, ComposeError::DockerUnavailable(_)),
        "expected DockerUnavailable, got {error:?}"
    );
}

#[tokio::test]
async fn disabled_handle_fails_stop() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let executor = disabled_executor(tmp.path().to_path_buf());

    let error = executor
        .stop("disabled-handle-stop")
        .await
        .expect_err("no Docker client exists in this process");
    assert!(
        matches!(error, ComposeError::DockerUnavailable(_)),
        "expected DockerUnavailable, got {error:?}"
    );
}

#[tokio::test]
async fn disabled_handle_fails_teardown_at_when_a_project_directory_exists() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let data_dir = tmp.path().to_path_buf();
    let executor = disabled_executor(data_dir.clone());

    // teardown_at() returns Ok(()) early when the project directory does
    // not exist (no Docker call is needed for that path -- see the
    // no-daemon-needed tests above). Create it so this test actually
    // reaches the guard in front of `docker compose down`.
    let project_name = "disabled-handle-teardown";
    let project_dir = data_dir.join("compose").join(project_name);
    std::fs::create_dir_all(&project_dir).expect("create project dir");

    let error = executor
        .teardown_at(project_name, None, None, &HashMap::new(), false)
        .await
        .expect_err("no Docker client exists in this process");
    assert!(
        matches!(error, ComposeError::DockerUnavailable(_)),
        "expected DockerUnavailable, got {error:?}"
    );
}
