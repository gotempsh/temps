// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tests for [`super`].

use super::*;
use crate::compose::test_support::{
    deploy_request, disabled_executor, one_secret, secrets_test_request, test_executor,
    violation_field, LOOPBACK,
};
use crate::compose::{ComposeDeployRequest, ComposeError, ComposeExecutor, TEMPS_SECRETS_OVERRIDE};
use bollard::Docker;
use std::sync::Arc;

/// On a worker, every loopback binding the policy admitted moves to the
/// node's private address with its ports and protocol unchanged; nothing
/// else in the file changes, and the control plane keeps loopback.
#[test]
fn loopback_published_ports_are_rebound_to_the_worker_address() {
    let compose = "services:\n  web:\n    image: app\n    ports:\n      - \"127.0.0.1::3000\"\n      - \"127.0.0.1:8443:443/udp\"\n      - target: 9000\n        published: 9001\n        host_ip: 127.0.0.1\n  worker:\n    image: job\n";
    let private: std::net::IpAddr = "10.20.0.5".parse().unwrap();

    let rebound =
        ComposeExecutor::rebind_loopback_published_ports(compose, private, "compose file").unwrap();
    let root: YamlValue = yaml_serde::from_str(&rebound).unwrap();
    let ports = root["services"]["web"]["ports"].as_sequence().unwrap();
    assert_eq!(ports[0].as_str(), Some("10.20.0.5::3000"));
    assert_eq!(ports[1].as_str(), Some("10.20.0.5:8443:443/udp"));
    assert_eq!(ports[2]["host_ip"].as_str(), Some("10.20.0.5"));
    assert_eq!(ports[2]["published"].as_u64(), Some(9001));
    assert_eq!(root["services"]["worker"]["image"].as_str(), Some("job"));
    assert!(!rebound.contains("127.0.0.1"), "{rebound}");

    // Loopback is the control plane: the file is written byte for byte.
    assert_eq!(
        ComposeExecutor::rebind_loopback_published_ports(compose, LOOPBACK, "compose file")
            .unwrap(),
        compose
    );
}

/// IPv6 private addresses need brackets in the short syntax, and a
/// binding an administrator allowed off loopback is left as written.
#[test]
fn rebinding_brackets_ipv6_and_keeps_non_loopback_bindings() {
    let compose =
        "services:\n  web:\n    ports:\n      - \"127.0.0.1::80\"\n      - \"0.0.0.0:53:53/udp\"\n";
    let private: std::net::IpAddr = "fd00::5".parse().unwrap();
    let rebound =
        ComposeExecutor::rebind_loopback_published_ports(compose, private, "compose file").unwrap();
    let root: YamlValue = yaml_serde::from_str(&rebound).unwrap();
    let ports = root["services"]["web"]["ports"].as_sequence().unwrap();
    assert_eq!(ports[0].as_str(), Some("[fd00::5]::80"));
    assert_eq!(ports[1].as_str(), Some("0.0.0.0:53:53/udp"));

    // A file with no ports is returned untouched.
    let no_ports = "services:\n  web:\n    image: app\n";
    assert_eq!(
        ComposeExecutor::rebind_loopback_published_ports(no_ports, private, "compose file")
            .unwrap(),
        no_ports
    );
}

#[test]
fn test_strip_excluded_services_removes_service_and_sequence_depends_on() {
    let docker = Docker::connect_with_defaults();
    if docker.is_err() {
        return;
    }
    let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

    let compose = r#"
services:
  app:
    image: myapp:latest
    depends_on:
      - db
      - redis
  db:
    image: postgres:16
  redis:
    image: redis:7
"#;
    let result = executor
        .strip_excluded_services(compose, &["db".to_string()])
        .unwrap();
    let parsed: yaml_serde::Value = yaml_serde::from_str(&result).unwrap();
    let services = parsed.get("services").unwrap().as_mapping().unwrap();
    assert!(!services.contains_key("db"));
    assert!(services.contains_key("redis"));
    let depends_on = services
        .get("app")
        .unwrap()
        .get("depends_on")
        .unwrap()
        .as_sequence()
        .unwrap();
    assert_eq!(depends_on.len(), 1);
    assert_eq!(depends_on[0].as_str(), Some("redis"));
}

#[test]
fn test_strip_excluded_services_removes_mapping_form_depends_on() {
    let docker = Docker::connect_with_defaults();
    if docker.is_err() {
        return;
    }
    let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

    let compose = r#"
services:
  app:
    image: myapp:latest
    depends_on:
      db:
        condition: service_healthy
  db:
    image: postgres:16
"#;
    let result = executor
        .strip_excluded_services(compose, &["db".to_string()])
        .unwrap();
    let parsed: yaml_serde::Value = yaml_serde::from_str(&result).unwrap();
    let services = parsed.get("services").unwrap().as_mapping().unwrap();
    assert!(!services.contains_key("db"));
    let depends_on = services
        .get("app")
        .unwrap()
        .get("depends_on")
        .unwrap()
        .as_mapping()
        .unwrap();
    assert!(depends_on.is_empty());
}

#[test]
fn test_strip_excluded_services_noop_for_absent_service() {
    let docker = Docker::connect_with_defaults();
    if docker.is_err() {
        return;
    }
    let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

    let compose = r#"
services:
  app:
    image: myapp:latest
"#;
    let result = executor
        .strip_excluded_services(compose, &["nonexistent".to_string()])
        .unwrap();
    let parsed: yaml_serde::Value = yaml_serde::from_str(&result).unwrap();
    assert!(parsed
        .get("services")
        .unwrap()
        .as_mapping()
        .unwrap()
        .contains_key("app"));
}

#[test]
fn test_strip_excluded_services_empty_list_returns_content_unchanged() {
    let docker = Docker::connect_with_defaults();
    if docker.is_err() {
        return;
    }
    let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

    let compose = "services:\n  app:\n    image: myapp:latest\n";
    let result = executor.strip_excluded_services(compose, &[]).unwrap();
    assert_eq!(result, compose);
}

#[tokio::test]
async fn test_write_compose_files_rewrites_stale_security_override_with_runtime_bounds() {
    let Some(executor) = test_executor() else {
        return;
    };
    let project_dir = tempfile::tempdir().unwrap();
    let stale_override = project_dir.path().join("docker-compose.temps-security.yml");
    tokio::fs::write(
        &stale_override,
        "services:\n  webserver:\n    cap_drop:\n      - ALL\n",
    )
    .await
    .unwrap();
    let request = ComposeDeployRequest {
        output_sink: None,
        project_name: "temps-test".to_string(),
        compose_content: "services:\n  webserver:\n    image: paperlessngx/paperless-ngx:latest\n"
            .to_string(),
        env_content: None,
        work_dir: project_dir.path().to_path_buf(),
        compose_path: None,
        environment_vars: HashMap::new(),
        secrets: HashMap::new(),
        secret_compose_services: HashMap::new(),
        build_args: HashMap::new(),
        labels: HashMap::new(),
        repo_dir: None,
        compose_override: None,
        relaxed_capability_services: Vec::new(),
        unsandboxed_services: vec!["webserver".to_string()],
        ready_timeout: None,
        routed_services: Vec::new(),
    };

    executor
        .write_compose_files(project_dir.path(), &request, "test-generation")
        .await
        .unwrap();

    let rewritten = tokio::fs::read_to_string(&stale_override).await.unwrap();
    assert!(rewritten.contains("pids_limit: 512"));
    assert!(!rewritten.contains("cpus:"));
    assert!(rewritten.contains("mem_limit:"));
    assert!(rewritten.contains("logging:"));
    assert!(!rewritten.contains("cap_drop:"));
    assert!(!rewritten.contains("no-new-privileges:true"));
    assert!(!rewritten.contains("init: true"));
}

#[test]
fn test_reserved_generated_compose_file_detection() {
    for reserved in RESERVED_GENERATED_COMPOSE_FILES {
        assert!(
            is_reserved_generated_compose_file(Path::new(reserved)),
            "{reserved} must be recognised as generated"
        );
        assert!(
            is_reserved_generated_compose_file(&Path::new("./").join(reserved)),
            "{reserved} must be recognised through a './' prefix"
        );
    }
    // Case-insensitive: the stack directory may live on a case-insensitive
    // filesystem, where `Docker-Compose.Temps-Override.yml` is the same file.
    assert!(is_reserved_generated_compose_file(Path::new(
        "Docker-Compose.Temps-Override.YML"
    )));
    // Only the stack root is passed to `docker compose -f`.
    assert!(!is_reserved_generated_compose_file(Path::new(
        "config/docker-compose.temps-override.yml"
    )));
    assert!(!is_reserved_generated_compose_file(Path::new(".env")));
    assert!(!is_reserved_generated_compose_file(Path::new(
        "docker-compose.yml"
    )));
}

/// A tenant compose file must not be able to make Temps materialise an
/// `env_file` onto one of the generated Compose filenames: `compose_up`
/// passes those to the daemon unvalidated, so attacker-influenced bytes
/// there are a container-escape primitive, not just a config oddity.
#[tokio::test]
async fn test_write_compose_files_rejects_env_file_targeting_generated_override() {
    let Some(executor) = test_executor() else {
        return;
    };
    let project_dir = tempfile::tempdir().unwrap();
    let request = ComposeDeployRequest {
        output_sink: None,
        project_name: "temps-test".to_string(),
        compose_content: "services:\n  app:\n    image: nginx\n    env_file: \
                              docker-compose.temps-override.yml\n"
            .to_string(),
        env_content: None,
        work_dir: project_dir.path().to_path_buf(),
        compose_path: None,
        environment_vars: HashMap::from([("SECRET".to_string(), "value".to_string())]),
        secrets: HashMap::new(),
        secret_compose_services: HashMap::new(),
        build_args: HashMap::new(),
        labels: HashMap::new(),
        repo_dir: None,
        compose_override: None,
        relaxed_capability_services: Vec::new(),
        unsandboxed_services: vec!["app".to_string()],
        ready_timeout: None,
        routed_services: Vec::new(),
    };

    let err = executor
        .write_compose_files(project_dir.path(), &request, "test-generation")
        .await
        .unwrap_err();
    assert_eq!(violation_field(err), "env_file");

    // And the generated override must not carry tenant env-var content.
    let generated = project_dir.path().join("docker-compose.temps-override.yml");
    if generated.exists() {
        let contents = tokio::fs::read_to_string(&generated).await.unwrap();
        assert!(
            !contents.contains("SECRET"),
            "generated override must never contain env-file content: {contents}"
        );
    }
}

/// The long form (`env_file: [{{path: ...}}]`) reaches the same writer, so
/// it must be rejected identically.
#[tokio::test]
async fn test_write_compose_files_rejects_long_form_env_file_targeting_generated_file() {
    let Some(executor) = test_executor() else {
        return;
    };
    let project_dir = tempfile::tempdir().unwrap();
    let request = ComposeDeployRequest {
        output_sink: None,
        project_name: "temps-test".to_string(),
        compose_content: "services:\n  app:\n    image: nginx\n    env_file:\n      \
                              - path: docker-compose.temps-security.yml\n        required: false\n"
            .to_string(),
        env_content: None,
        work_dir: project_dir.path().to_path_buf(),
        compose_path: None,
        environment_vars: HashMap::from([("SECRET".to_string(), "value".to_string())]),
        secrets: HashMap::new(),
        secret_compose_services: HashMap::new(),
        build_args: HashMap::new(),
        labels: HashMap::new(),
        repo_dir: None,
        compose_override: None,
        relaxed_capability_services: Vec::new(),
        unsandboxed_services: vec!["app".to_string()],
        ready_timeout: None,
        routed_services: Vec::new(),
    };

    let err = executor
        .write_compose_files(project_dir.path(), &request, "test-generation")
        .await
        .unwrap_err();
    assert_eq!(violation_field(err), "env_file");
}

/// `compose_path` comes from project settings, so it is user-selected too.
#[tokio::test]
async fn test_write_compose_files_rejects_compose_path_targeting_generated_file() {
    let Some(executor) = test_executor() else {
        return;
    };
    let project_dir = tempfile::tempdir().unwrap();
    let request = ComposeDeployRequest {
        output_sink: None,
        project_name: "temps-test".to_string(),
        compose_content: "services:\n  app:\n    image: nginx\n".to_string(),
        env_content: None,
        work_dir: project_dir.path().to_path_buf(),
        compose_path: Some("docker-compose.temps-network.yml".to_string()),
        environment_vars: HashMap::new(),
        secrets: HashMap::new(),
        secret_compose_services: HashMap::new(),
        build_args: HashMap::new(),
        labels: HashMap::new(),
        repo_dir: None,
        compose_override: None,
        relaxed_capability_services: Vec::new(),
        unsandboxed_services: vec!["app".to_string()],
        ready_timeout: None,
        routed_services: Vec::new(),
    };

    let err = executor
        .write_compose_files(project_dir.path(), &request, "test-generation")
        .await
        .unwrap_err();
    assert_eq!(violation_field(err), "compose_path");
}

/// The guard must not break the ordinary case: a normal `env_file:` is
/// still materialised from the project's environment variables.
#[tokio::test]
async fn test_write_compose_files_still_materialises_normal_env_file() {
    let Some(executor) = test_executor() else {
        return;
    };
    let project_dir = tempfile::tempdir().unwrap();
    let request = ComposeDeployRequest {
        output_sink: None,
        project_name: "temps-test".to_string(),
        compose_content: "services:\n  app:\n    image: nginx\n    env_file: app.env\n".to_string(),
        env_content: None,
        work_dir: project_dir.path().to_path_buf(),
        compose_path: None,
        environment_vars: HashMap::from([("SECRET".to_string(), "value".to_string())]),
        secrets: HashMap::new(),
        secret_compose_services: HashMap::new(),
        build_args: HashMap::new(),
        labels: HashMap::new(),
        repo_dir: None,
        compose_override: None,
        relaxed_capability_services: Vec::new(),
        unsandboxed_services: vec!["app".to_string()],
        ready_timeout: None,
        routed_services: Vec::new(),
    };

    executor
        .write_compose_files(project_dir.path(), &request, "test-generation")
        .await
        .unwrap();

    let materialised = tokio::fs::read_to_string(project_dir.path().join("app.env"))
        .await
        .unwrap();
    assert!(materialised.contains("SECRET"), "got: {materialised}");
}

#[tokio::test]
async fn test_compose_secrets_removed_when_last_secret_is_deleted() {
    let Some(docker) = Docker::connect_with_defaults().ok() else {
        return;
    };
    let data_dir = tempfile::tempdir().unwrap();
    let executor = ComposeExecutor::new(Arc::new(docker), data_dir.path().to_path_buf());
    let project_dir = tempfile::tempdir().unwrap();
    let compose = "services:\n  web:\n    image: nginx\n";

    executor
        .write_compose_files(
            project_dir.path(),
            &secrets_test_request("temps-1-2", compose, one_secret("OLD_KEY", "value")),
            "test-generation",
        )
        .await
        .unwrap();
    assert!(project_dir.path().join(TEMPS_SECRETS_OVERRIDE).exists());

    // Redeploy after the user deleted every secret.
    executor
        .write_compose_files(
            project_dir.path(),
            &secrets_test_request("temps-1-2", compose, HashMap::new()),
            "test-generation",
        )
        .await
        .unwrap();

    assert!(
        !executor
            .secret_generation_dir("temps-1-2", "test-generation")
            .exists(),
        "stale plaintext survived a redeploy with no secrets"
    );
    assert!(
        !project_dir.path().join(TEMPS_SECRETS_OVERRIDE).exists(),
        "stale override would still mount an emptied directory"
    );
}

#[tokio::test]
async fn test_compose_secrets_rotated_value_replaces_previous_file() {
    let Some(docker) = Docker::connect_with_defaults().ok() else {
        return;
    };
    let data_dir = tempfile::tempdir().unwrap();
    let executor = ComposeExecutor::new(Arc::new(docker), data_dir.path().to_path_buf());
    let project_dir = tempfile::tempdir().unwrap();
    let compose = "services:\n  web:\n    image: nginx\n";

    executor
        .write_compose_files(
            project_dir.path(),
            &secrets_test_request("temps-1-2", compose, one_secret("API_KEY", "old-value")),
            "test-generation",
        )
        .await
        .unwrap();
    executor
        .write_compose_files(
            project_dir.path(),
            &secrets_test_request("temps-1-2", compose, one_secret("RENAMED", "new-value")),
            "test-generation",
        )
        .await
        .unwrap();

    let dir = executor
        .secret_generation_dir("temps-1-2", "test-generation")
        .join("web");
    assert!(
        !dir.join("API_KEY").exists(),
        "renamed key left a stale file"
    );
    assert_eq!(
        tokio::fs::read_to_string(dir.join("RENAMED"))
            .await
            .unwrap(),
        "new-value"
    );
}

#[tokio::test]
async fn test_compose_override_cannot_introduce_a_service_without_secrets() {
    let Some(docker) = Docker::connect_with_defaults().ok() else {
        return;
    };
    let data_dir = tempfile::tempdir().unwrap();
    let executor = ComposeExecutor::new(Arc::new(docker), data_dir.path().to_path_buf());
    let project_dir = tempfile::tempdir().unwrap();
    let mut request = secrets_test_request(
        "temps-1-2",
        "services:\n  web:\n    image: nginx\n",
        one_secret("TOKEN", "value"),
    );
    request.compose_override = Some("services:\n  worker:\n    image: nginx\n".to_string());

    // Secret coverage is enumerated from the compose documents, so a
    // service the enumeration cannot see would silently get no secrets.
    // `validate_compose_override` is what makes that unreachable: an
    // inline override may not introduce services at all. This test pins
    // that dependency so the guarantee cannot be removed elsewhere
    // without a failure here.
    let error = executor
        .write_compose_files(project_dir.path(), &request, "test-generation")
        .await
        .unwrap_err();

    assert!(
        matches!(error, ComposeError::InvalidOverride { ref reason, .. } if reason.contains("cannot add service")),
        "expected the override to be rejected, got {error:?}"
    );
}

#[tokio::test]
async fn test_compose_secrets_fail_loudly_when_data_dir_has_a_colon() {
    let Some(docker) = Docker::connect_with_defaults().ok() else {
        return;
    };
    let data_dir = tempfile::tempdir().unwrap();
    // Docker splits bind specs on ':', so this must not silently mount the
    // wrong path.
    let colon_dir = data_dir.path().join("has:colon");
    let executor = ComposeExecutor::new(Arc::new(docker), colon_dir);
    let project_dir = tempfile::tempdir().unwrap();
    let request = secrets_test_request(
        "temps-1-2",
        "services:\n  web:\n    image: nginx\n",
        one_secret("TOKEN", "value"),
    );

    let error = executor
        .write_compose_files(project_dir.path(), &request, "test-generation")
        .await
        .unwrap_err();

    assert!(
        matches!(error, ComposeError::FileWriteFailed { ref reason, .. } if reason.contains("colon")),
        "expected a clear colon error, got {error:?}"
    );
}

#[test]
fn test_compose_path_cannot_shadow_a_generated_override() {
    for reserved in RESERVED_GENERATED_COMPOSE_FILES.iter().copied() {
        assert!(
            ComposeExecutor::validate_compose_path_not_generated(reserved).is_err(),
            "{reserved} should be reserved"
        );
        assert!(
            ComposeExecutor::validate_compose_path_not_generated(&format!("stack/{reserved}"))
                .is_err(),
            "{reserved} should be reserved in a subdirectory too"
        );
    }
    assert!(ComposeExecutor::validate_compose_path_not_generated("docker-compose.yml").is_ok());
}

#[tokio::test]
async fn test_compose_secrets_skip_services_with_conflicting_mount() {
    let Some(docker) = Docker::connect_with_defaults().ok() else {
        return;
    };
    let data_dir = tempfile::tempdir().unwrap();
    let executor = ComposeExecutor::new(Arc::new(docker), data_dir.path().to_path_buf());
    let project_dir = tempfile::tempdir().unwrap();
    let request = secrets_test_request(
            "temps-1-2",
            "services:\n  web:\n    image: nginx\n  legacy:\n    image: nginx\n    volumes:\n      - ./s:/run/secrets\n",
            one_secret("TOKEN", "value"),
        );

    executor
        .write_compose_files(project_dir.path(), &request, "test-generation")
        .await
        .unwrap();

    let parsed: YamlValue = yaml_serde::from_str(
        &tokio::fs::read_to_string(project_dir.path().join(TEMPS_SECRETS_OVERRIDE))
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(parsed["services"]["web"].is_mapping());
    // Mounting on top would make `docker compose up` fail for the whole
    // stack on a duplicate mount target.
    assert!(parsed["services"]["legacy"].is_null());
}

#[test]
fn test_missing_relative_bind_mounts_use_stable_project_storage() {
    let checkout = tempfile::tempdir().unwrap();
    let compose_base = checkout.path().join("paperless");
    let persistent_root = checkout.path().join("temps-data/binds");
    std::fs::create_dir_all(&compose_base).unwrap();
    let compose = r#"
services:
  webserver:
    image: ghcr.io/paperless-ngx/paperless-ngx:latest
    volumes:
      - ./export:/usr/src/paperless/export
      - ./consume:/usr/src/paperless/consume:ro
"#;

    let rewritten = ComposeExecutor::rewrite_missing_relative_bind_mounts(
        compose,
        &compose_base,
        &persistent_root,
    )
    .unwrap();
    let export = persistent_root.join(ComposeExecutor::stable_bind_name("export"));
    let consume = persistent_root.join(ComposeExecutor::stable_bind_name("consume"));

    assert!(export.is_dir());
    assert!(consume.is_dir());
    assert!(rewritten.contains(&format!("{}:/usr/src/paperless/export", export.display())));
    assert!(rewritten.contains(&format!(
        "{}:/usr/src/paperless/consume:ro",
        consume.display()
    )));

    // A later checkout adding these directories must not switch the stack
    // away from its already-populated persistent storage.
    std::fs::create_dir_all(compose_base.join("export")).unwrap();
    std::fs::create_dir_all(compose_base.join("consume")).unwrap();
    let redeploy = ComposeExecutor::rewrite_missing_relative_bind_mounts(
        compose,
        &compose_base,
        &persistent_root,
    )
    .unwrap();
    assert!(redeploy.contains(&export.to_string_lossy().to_string()));
    assert!(redeploy.contains(&consume.to_string_lossy().to_string()));
}

#[test]
fn test_existing_repository_bind_is_not_rewritten_without_stable_storage() {
    let checkout = tempfile::tempdir().unwrap();
    let compose_base = checkout.path().join("app");
    let persistent_root = checkout.path().join("temps-data/binds");
    std::fs::create_dir_all(compose_base.join("checked-in-data")).unwrap();
    let compose =
        "services:\n  app:\n    image: alpine\n    volumes:\n      - ./checked-in-data:/data\n";

    let rewritten = ComposeExecutor::rewrite_missing_relative_bind_mounts(
        compose,
        &compose_base,
        &persistent_root,
    )
    .unwrap();

    assert_eq!(rewritten, compose);
    assert!(!persistent_root.exists());
}

/// Compose allows three shapes for `env_file`; a stack must not fail to
/// deploy because its author picked one of them.
#[test]
fn collects_env_file_refs_in_all_three_compose_shapes() {
    let compose = r#"
services:
  api:
    image: api:latest
    env_file: .env
  worker:
    image: worker:latest
    env_file:
      - .env
      - config/worker.env
  web:
    image: web:latest
    env_file:
      - path: config/web.env
        required: false
"#;
    let refs = ComposeExecutor::collect_env_file_refs(compose);
    // `.env` is referenced by two services but materialized once.
    assert_eq!(refs, vec![".env", "config/worker.env", "config/web.env"]);
}

/// The compose file may come from a repository the operator does not
/// control, so an `env_file` entry must never name a write target outside
/// the stack directory.
#[test]
fn drops_env_file_refs_that_escape_the_project() {
    let compose = r#"
services:
  evil:
    image: evil:latest
    env_file:
      - ../../../root/.ssh/authorized_keys
      - /etc/passwd
      - ok.env
"#;
    assert_eq!(
        ComposeExecutor::collect_env_file_refs(compose),
        vec!["ok.env"]
    );
}

#[test]
fn collects_nothing_from_compose_without_env_files() {
    let compose = "services:\n  api:\n    image: api:latest\n";
    assert!(ComposeExecutor::collect_env_file_refs(compose).is_empty());
    // Unparsable YAML must not panic or invent references.
    assert!(ComposeExecutor::collect_env_file_refs("{{{ not yaml").is_empty());
}

/// A file the repository ships is authored config and wins; one it does not
/// ship is synthesized from the project's environment variables.
#[test]
fn plans_repo_copy_when_present_and_synthesis_when_absent() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(repo.path().join("config")).unwrap();
    std::fs::write(repo.path().join("config/worker.env"), "FROM_REPO=1\n").unwrap();

    let compose = r#"
services:
  api:
    image: api:latest
    env_file:
      - .env
      - config/worker.env
"#;
    let plans = ComposeExecutor::plan_env_files(compose, Some(repo.path()));
    assert_eq!(plans.len(), 2);
    assert_eq!(plans[0].path, ".env");
    assert_eq!(plans[0].source, EnvFileSource::ProjectEnvironment);
    assert_eq!(plans[1].path, "config/worker.env");
    assert!(matches!(plans[1].source, EnvFileSource::Repository(_)));
}

/// A symlink in the repo must not let a referenced env file read outside it.
#[cfg(unix)]
#[test]
fn plan_rejects_repo_env_file_symlinked_outside_the_checkout() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.env"), "STOLEN=1\n").unwrap();
    let repo = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("secret.env"),
        repo.path().join("app.env"),
    )
    .unwrap();

    let compose = "services:\n  api:\n    image: api:latest\n    env_file: app.env\n";
    let plans = ComposeExecutor::plan_env_files(compose, Some(repo.path()));
    assert_eq!(plans[0].source, EnvFileSource::ProjectEnvironment);
}

#[test]
fn renders_env_file_deterministically() {
    let mut vars = HashMap::new();
    vars.insert("B_KEY".to_string(), "2".to_string());
    vars.insert("A_KEY".to_string(), "1".to_string());
    assert_eq!(
        render_env_file(&vars).unwrap(),
        "A_KEY=\"1\"\nB_KEY=\"2\"\n"
    );
    assert_eq!(render_env_file(&HashMap::new()).unwrap(), "");
}

#[test]
fn render_env_file_quotes_newlines_dollars_and_quotes_without_injecting_keys() {
    let vars = HashMap::from([(
        "SAFE_KEY".to_string(),
        "first\nINJECTED=bad $TOKEN \\\"quoted\\\"".to_string(),
    )]);

    let rendered = render_env_file(&vars).unwrap();

    assert_eq!(
        rendered,
        "SAFE_KEY=\"first\\nINJECTED=bad $$TOKEN \\\\\\\"quoted\\\\\\\"\"\n"
    );
    assert_eq!(rendered.lines().count(), 1);
}

#[test]
fn render_env_file_rejects_invalid_keys_and_control_characters() {
    let invalid_key = HashMap::from([("DOCKER_HOST\nPATH".to_string(), "x".to_string())]);
    assert!(matches!(
        render_env_file(&invalid_key),
        Err(ComposeError::InvalidEnvironmentVariable { .. })
    ));

    let invalid_value = HashMap::from([("SAFE_KEY".to_string(), "bad\u{0000}".to_string())]);
    assert!(matches!(
        render_env_file(&invalid_value),
        Err(ComposeError::InvalidEnvironmentVariable { .. })
    ));
}

#[test]
fn env_files_resolve_relative_to_the_compose_file_directory() {
    let compose = r#"
services:
  web:
    image: app
    env_file:
      - app.env
      - ../.env
      - ../../outside.env
      - /etc/passwd
      - ./config/web.env
"#;
    assert_eq!(
        ComposeExecutor::env_file_refs_for_compose_file(compose, "deploy/compose.yaml"),
        vec!["deploy/app.env", ".env", "deploy/config/web.env"]
    );
    // Root compose files keep their previous behaviour.
    assert_eq!(
        ComposeExecutor::env_file_refs_for_compose_file(compose, "compose.yaml"),
        vec!["app.env", "config/web.env"]
    );
}

#[tokio::test]
async fn subdirectory_compose_gets_absolute_temps_env_and_env_files_next_to_it() {
    let data_dir = tempfile::tempdir().unwrap();
    let executor = disabled_executor(data_dir.path().to_path_buf());
    let checkout = tempfile::tempdir().unwrap();
    let compose = "services:\n  web:\n    image: app\n    env_file: app.env\n";
    let mut request = deploy_request(compose, "deploy/compose.yaml");
    request.repo_dir = Some(checkout.path().to_path_buf());
    request.environment_vars = HashMap::from([("GREETING".into(), "hello".into())]);

    executor
        .write_compose_files(checkout.path(), &request, "gen-1")
        .await
        .unwrap();

    let env_override =
        std::fs::read_to_string(checkout.path().join("docker-compose.temps-env.yml")).unwrap();
    let temps_env = checkout.path().join(".env.temps");
    assert!(temps_env.is_file());
    assert!(
        env_override.contains(&temps_env.to_string_lossy().to_string()),
        "override must reference .env.temps absolutely: {env_override}"
    );
    // Synthesized where Compose will look for it: next to the compose file.
    assert!(checkout.path().join("deploy/app.env").is_file());
    assert!(!checkout.path().join("app.env").exists());
}

#[tokio::test]
async fn env_file_resolving_onto_a_generated_compose_file_is_rejected() {
    let data_dir = tempfile::tempdir().unwrap();
    let executor = disabled_executor(data_dir.path().to_path_buf());
    let checkout = tempfile::tempdir().unwrap();
    let compose =
        "services:\n  web:\n    image: app\n    env_file: ../docker-compose.temps-env.yml\n";
    let mut request = deploy_request(compose, "deploy/compose.yaml");
    request.repo_dir = Some(checkout.path().to_path_buf());
    let error = executor
        .write_compose_files(checkout.path(), &request, "gen-1")
        .await
        .unwrap_err();
    assert!(
        matches!(error, ComposeError::SecurityPolicyViolation { ref field, .. } if field == "env_file"),
        "{error}"
    );
}
