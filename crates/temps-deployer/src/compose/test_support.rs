// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Test helpers shared by the `compose` submodules' tests.

use bollard::Docker;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use temps_core::DockerHandle;
use temps_entities::compose_security::{
    ComposeSecurityCheck as PolicyCheck, ComposeSecurityPolicy,
};

use crate::compose::{ComposeDeployRequest, ComposeError, ComposeExecutor};

pub(super) const LOOPBACK: std::net::IpAddr = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

/// A request with everything empty, so a test only states the fields it
/// actually exercises.
pub(super) fn secrets_test_request(
    project_name: &str,
    compose_content: &str,
    secrets: HashMap<String, String>,
) -> ComposeDeployRequest {
    ComposeDeployRequest {
        output_sink: None,
        project_name: project_name.to_string(),
        compose_content: compose_content.to_string(),
        env_content: None,
        work_dir: PathBuf::from("/tmp"),
        compose_path: None,
        environment_vars: HashMap::new(),
        secrets,
        secret_compose_services: HashMap::new(),
        build_args: HashMap::new(),
        labels: HashMap::new(),
        repo_dir: None,
        compose_override: None,
        relaxed_capability_services: Vec::new(),
        unsandboxed_services: Vec::new(),
        ready_timeout: None,
        routed_services: Vec::new(),
    }
}

pub(super) fn one_secret(key: &str, value: &str) -> HashMap<String, String> {
    HashMap::from([(key.to_string(), value.to_string())])
}

/// Build an executor for tests, skipping when Docker is unavailable.
pub(super) fn test_executor() -> Option<ComposeExecutor> {
    let docker = Docker::connect_with_defaults().ok()?;
    Some(ComposeExecutor::new(
        Arc::new(docker),
        PathBuf::from("/tmp/test"),
    ))
}

/// Build an executor with no Docker client at all -- the control-plane
/// serve profile. Never touches a real socket or daemon, so unlike
/// `test_executor()` this never needs to skip: it is deterministic on
/// every machine, CI included.
pub(super) fn disabled_executor(data_dir: PathBuf) -> ComposeExecutor {
    ComposeExecutor::new_with_handle(
        Arc::new(DockerHandle::disabled(
            temps_core::PROFILE_CONTROL_PLANE,
            temps_core::CONTROL_PLANE_DOCKER_REASON,
        )),
        data_dir,
    )
}

pub(super) fn violation_field(err: ComposeError) -> String {
    match err {
        ComposeError::SecurityPolicyViolation { field, .. } => field,
        other => panic!("expected SecurityPolicyViolation, got {other:?}"),
    }
}

pub(super) fn executor_with_checks_disabled(checks: &[PolicyCheck]) -> ComposeExecutor {
    disabled_executor(PathBuf::from("/tmp/temps-policy-unit")).with_security_policy(
        ComposeSecurityPolicy {
            disabled_checks: checks.iter().copied().collect(),
        },
    )
}

pub(super) fn deploy_request(compose_content: &str, compose_path: &str) -> ComposeDeployRequest {
    let mut request = secrets_test_request("temps-9-9", compose_content, HashMap::new());
    request.compose_path = Some(compose_path.to_string());
    request
}
