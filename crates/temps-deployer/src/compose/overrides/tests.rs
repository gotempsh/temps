// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tests for [`super`].

use super::*;
use crate::compose::test_support::{executor_with_checks_disabled, test_executor};
use crate::compose::{ComposeError, ComposeExecutor};
use bollard::Docker;
use std::path::PathBuf;
use std::sync::Arc;

#[test]
fn test_generate_env_override() {
    let docker = Docker::connect_with_defaults();
    if docker.is_err() {
        return;
    }
    let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

    let compose = r#"
services:
  web:
    image: nginx
    ports:
      - "8080:80"
  redis:
    image: redis:7
  postgres:
    image: postgres:17
"#;

    let override_yaml = executor.generate_env_override(compose, ".env.temps", &HashMap::new());
    assert!(override_yaml.contains("web:"));
    assert!(override_yaml.contains("redis:"));
    assert!(override_yaml.contains("postgres:"));
    assert!(override_yaml.contains(".env.temps"));
    // Each service should have env_file
    assert_eq!(override_yaml.matches("env_file:").count(), 3);
}

#[test]
fn test_generate_env_override_passes_project_vars_as_build_args() {
    let docker = Docker::connect_with_defaults();
    if docker.is_err() {
        return;
    }
    let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

    let compose = r#"
services:
  web:
    build: .
  api:
    build:
      context: ./api
      args:
        - NODE_ENV=production
  worker:
    build:
      context: ./worker
      args:
        API_URL: https://compose.example
  db:
    image: postgres:17-alpine
"#;
    let project_vars = HashMap::from([
        ("API_URL".to_string(), "https://project.example".to_string()),
        ("NODE_ENV".to_string(), "development".to_string()),
        ("PRICE".to_string(), "costs $5 or ${X}".to_string()),
    ]);

    let override_yaml = executor.generate_env_override(compose, ".env.temps", &project_vars);
    let parsed: yaml_serde::Value = yaml_serde::from_str(&override_yaml).unwrap();
    let args = |service: &str| {
        parsed
            .get("services")
            .and_then(|s| s.get(service))
            .and_then(|s| s.get("build"))
            .and_then(|b| b.get("args"))
            .cloned()
    };

    // Short-form build: every project variable becomes a build arg, with
    // `$` escaped so Compose does not interpolate the literal value.
    let web = args("web").expect("web builds, so it gets build args");
    assert_eq!(
        web.get("API_URL").and_then(|v| v.as_str()),
        Some("https://project.example")
    );
    assert_eq!(
        web.get("PRICE").and_then(|v| v.as_str()),
        Some("costs $$5 or $${X}")
    );

    // An arg the Compose file declares (list or map form) keeps its value.
    let api = args("api").expect("api builds, so it gets build args");
    assert!(api.get("NODE_ENV").is_none());
    assert!(api.get("API_URL").is_some());
    let worker = args("worker").expect("worker builds, so it gets build args");
    assert!(worker.get("API_URL").is_none());
    assert!(worker.get("NODE_ENV").is_some());

    // An image-only service must not gain a `build:` section.
    let db = parsed.get("services").and_then(|s| s.get("db")).unwrap();
    assert!(db.get("build").is_none());
    assert!(db.get("env_file").is_some());
}

#[test]
fn test_generate_env_override_beats_inline_environment_for_matching_key() {
    let docker = Docker::connect_with_defaults();
    if docker.is_err() {
        return;
    }
    let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

    let compose = r#"
services:
  postgres:
    image: postgres:17-alpine
  hub:
    image: ghcr.io/getpaseo/hub:latest
    env_file: .env
    environment:
      DATABASE_URL: postgres://paseo:paseo@postgres:5432/paseo_hub
      PORT: 3000
"#;
    let mut project_vars = HashMap::new();
    project_vars.insert(
        "DATABASE_URL".to_string(),
        "postgres://user:pass@managed-db:5432/app".to_string(),
    );
    project_vars.insert("UNRELATED_VAR".to_string(), "value".to_string());

    let override_yaml = executor.generate_env_override(compose, ".env.temps", &project_vars);
    let parsed: yaml_serde::Value = yaml_serde::from_str(&override_yaml).unwrap();
    let hub_env = parsed
        .get("services")
        .and_then(|s| s.get("hub"))
        .and_then(|h| h.get("environment"))
        .expect("hub should have an environment override");
    assert_eq!(
        hub_env.get("DATABASE_URL").and_then(|v| v.as_str()),
        Some("postgres://user:pass@managed-db:5432/app")
    );
    // PORT is inline in compose.yml but not project-configured — untouched.
    assert!(hub_env.get("PORT").is_none());
    // UNRELATED_VAR is project-configured but not inline in compose.yml —
    // never force-injected into a service that doesn't reference it.
    assert!(hub_env.get("UNRELATED_VAR").is_none());

    // postgres has no inline `environment:` referencing DATABASE_URL, so
    // it gets env_file only, no environment: override block at all.
    let postgres_service = parsed
        .get("services")
        .and_then(|s| s.get("postgres"))
        .unwrap();
    assert!(postgres_service.get("environment").is_none());
    assert!(postgres_service.get("env_file").is_some());
}

#[test]
fn test_generate_env_override_handles_special_characters_in_values() {
    let docker = Docker::connect_with_defaults();
    if docker.is_err() {
        return;
    }
    let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

    let compose = r#"
services:
  app:
    image: app:latest
    environment:
      SECRET: placeholder
"#;
    let mut project_vars = HashMap::new();
    project_vars.insert(
        "SECRET".to_string(),
        "va:lue with \"quotes\" and\nnewline".to_string(),
    );

    let override_yaml = executor.generate_env_override(compose, ".env.temps", &project_vars);
    let parsed: yaml_serde::Value = yaml_serde::from_str(&override_yaml).unwrap();
    let secret = parsed
        .get("services")
        .and_then(|s| s.get("app"))
        .and_then(|a| a.get("environment"))
        .and_then(|e| e.get("SECRET"))
        .and_then(|v| v.as_str());
    assert_eq!(secret, Some("va:lue with \"quotes\" and\nnewline"));
}

#[test]
fn test_generate_env_override_ignores_sequence_form_environment() {
    let docker = Docker::connect_with_defaults();
    if docker.is_err() {
        return;
    }
    let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

    let compose = r#"
services:
  app:
    image: app:latest
    environment:
      - DATABASE_URL=postgres://local/db
      - PORT
"#;
    let mut project_vars = HashMap::new();
    project_vars.insert(
        "DATABASE_URL".to_string(),
        "postgres://managed/db".to_string(),
    );

    let override_yaml = executor.generate_env_override(compose, ".env.temps", &project_vars);
    let parsed: yaml_serde::Value = yaml_serde::from_str(&override_yaml).unwrap();
    let app_env = parsed
        .get("services")
        .and_then(|s| s.get("app"))
        .and_then(|a| a.get("environment"))
        .expect("sequence-form environment keys should still be detected");
    assert_eq!(
        app_env.get("DATABASE_URL").and_then(|v| v.as_str()),
        Some("postgres://managed/db")
    );
}

#[test]
fn test_generate_env_override_empty() {
    let docker = Docker::connect_with_defaults();
    if docker.is_err() {
        return;
    }
    let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

    let compose = "version: '3'\n";
    let override_yaml = executor.generate_env_override(compose, ".env.temps", &HashMap::new());
    assert!(override_yaml.is_empty());
}

#[test]
fn test_generate_network_override_attaches_every_service() {
    let docker = Docker::connect_with_defaults();
    if docker.is_err() {
        return;
    }
    let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

    let compose = r#"
services:
  hub:
    image: ghcr.io/getpaseo/hub:latest
  postgres:
    image: postgres:17-alpine
"#;
    let override_yaml = executor.generate_network_override(compose);
    let parsed: yaml_serde::Value = yaml_serde::from_str(&override_yaml).unwrap();

    let network_name = temps_core::NETWORK_NAME.as_str();
    let top_level_networks = parsed.get("networks").expect("top-level networks: block");
    assert!(top_level_networks.get("default").is_some());
    assert_eq!(
        top_level_networks
            .get(network_name)
            .and_then(|n| n.get("external"))
            .and_then(|v| v.as_bool()),
        Some(true)
    );

    for service in ["hub", "postgres"] {
        let service_networks = parsed
            .get("services")
            .and_then(|s| s.get(service))
            .and_then(|s| s.get("networks"))
            .and_then(|n| n.as_sequence())
            .unwrap_or_else(|| panic!("{service} should have a networks: list"));
        let names: Vec<&str> = service_networks.iter().filter_map(|v| v.as_str()).collect();
        assert!(names.contains(&"default"));
        assert!(names.contains(&network_name));
    }
}

#[test]
fn test_generate_network_override_empty_for_no_services() {
    let docker = Docker::connect_with_defaults();
    if docker.is_err() {
        return;
    }
    let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

    let compose = "version: '3'\n";
    let override_yaml = executor.generate_network_override(compose);
    assert!(override_yaml.is_empty());
}

#[test]
fn test_services_with_ports_in_override() {
    let docker = Docker::connect_with_defaults();
    if docker.is_err() {
        return;
    }
    let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

    let override_content = r#"
services:
  clickhouse:
    ports:
      - '127.0.0.1:28123:8123'
      - '127.0.0.1:29001:9000'
"#;
    let result = executor.services_with_ports_in_override(override_content);
    assert_eq!(result, vec!["clickhouse"]);

    // No ports override
    let override_no_ports = r#"
services:
  clickhouse:
    environment:
      - FOO=bar
"#;
    let result = executor.services_with_ports_in_override(override_no_ports);
    assert!(result.is_empty());

    // Multiple services, only one with ports
    let override_mixed = r#"
services:
  web:
    ports:
      - '8080:80'
  redis:
    environment:
      - REDIS_PASSWORD=secret
"#;
    let result = executor.services_with_ports_in_override(override_mixed);
    assert_eq!(result, vec!["web"]);
}

#[test]
fn test_strip_ports_for_services() {
    let docker = Docker::connect_with_defaults();
    if docker.is_err() {
        return;
    }
    let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

    let compose = r#"version: '3.8'
services:
  clickhouse:
    image: clickhouse/clickhouse-server:23.4
    ports:
      - '8123:8123'
      - '9000:9000'
    volumes:
      - ./data:/var/lib/clickhouse
  keeper:
    image: clickhouse/clickhouse-keeper:23.4-alpine
    ports:
      - '9181:9181'
"#;

    // Strip ports only for clickhouse, keep keeper's ports
    let result = executor.strip_ports_for_services(compose, &["clickhouse".to_string()]);
    assert!(!result.contains("8123:8123"));
    assert!(!result.contains("9000:9000"));
    assert!(result.contains("9181:9181")); // keeper untouched
    assert!(result.contains("volumes:")); // other sections preserved
    assert!(result.contains("./data:/var/lib/clickhouse"));

    // Strip ports for both
    let result = executor
        .strip_ports_for_services(compose, &["clickhouse".to_string(), "keeper".to_string()]);
    assert!(!result.contains("8123:8123"));
    assert!(!result.contains("9000:9000"));
    assert!(!result.contains("9181:9181"));
}

#[test]
fn test_generate_security_override() {
    let docker = Docker::connect_with_defaults();
    if docker.is_err() {
        return;
    }
    let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

    let compose = r#"
services:
  web:
    image: nginx
  worker:
    image: alpine
"#;
    let override_yaml = executor.generate_security_override(compose, &[]);

    assert_eq!(override_yaml.matches("cap_drop:").count(), 2);
    assert_eq!(override_yaml.matches("no-new-privileges:true").count(), 2);
    assert_eq!(override_yaml.matches("pids_limit: 512").count(), 2);
    assert_eq!(override_yaml.matches("init: true").count(), 2);
    // Every sandboxed service gets the minimal safe capability set back
    // by default now — no opt-in required.
    assert_eq!(override_yaml.matches("cap_add:").count(), 2);
}

#[test]
fn test_healthcheck_loopback_override_is_generic_and_secret_safe() {
    let compose = r#"
services:
  web:
    image: example/web
    healthcheck:
      test: ["CMD-SHELL", "curl -H 'Authorization: Bearer ${TOKEN}' HTTP://LOCALHOST:8080/ready"]
  lookalike:
    image: example/lookalike
    healthcheck:
      test: ["CMD", "curl", "http://localhost.example/ready"]
  disabled:
    image: example/disabled
    healthcheck:
      test: ["CMD", "curl", "http://localhost:9000/ready"]
"#;
    let compose_override = r#"
services:
  disabled:
    healthcheck:
      disable: true
"#;

    let overrides =
        ComposeExecutor::healthcheck_loopback_overrides(compose, Some(compose_override));
    assert_eq!(overrides.len(), 1);
    let web = overrides.get("web").expect("web probe normalized");
    assert!(web.contains("http://127.0.0.1:8080/ready"));
    assert!(
        web.contains("${TOKEN}"),
        "interpolation must remain unresolved"
    );
    assert!(!overrides.contains_key("lookalike"));
    assert!(!overrides.contains_key("disabled"));

    let executor = ComposeExecutor::new(
        Arc::new(Docker::connect_with_defaults().expect("construct Docker client")),
        PathBuf::from("/tmp/test"),
    );
    let override_yaml = executor.generate_security_override_with_image_init(
        compose,
        &["web".to_string()],
        &HashSet::new(),
        &overrides,
    );
    let parsed: YamlValue = yaml_serde::from_str(&override_yaml).unwrap();
    assert_eq!(parsed["services"]["web"].get("privileged"), None);
    let test = parsed["services"]["web"]["healthcheck"]["test"]
        .as_sequence()
        .expect("healthcheck test remains a Compose sequence");
    assert_eq!(test[0].as_str(), Some("CMD-SHELL"));
    assert!(test[1]
        .as_str()
        .is_some_and(|command| command.contains("http://127.0.0.1:8080/ready")));
}

#[test]
fn test_unsandboxed_service_keeps_its_image_owned_init_process() {
    let Some(executor) = test_executor() else {
        return;
    };
    let compose = r#"
services:
  webserver:
    image: ghcr.io/paperless-ngx/paperless-ngx:latest
    restart: unless-stopped
"#;

    let override_yaml = executor.generate_security_override(compose, &["webserver".to_string()]);

    let override_value: Value = yaml_serde::from_str(&override_yaml).unwrap();
    let webserver = override_value["services"]["webserver"]
        .as_mapping()
        .unwrap();
    assert_eq!(
        webserver.get("pids_limit").and_then(Value::as_u64),
        Some(512)
    );
    assert_eq!(webserver.get("init"), None);
    assert_eq!(webserver.get("privileged"), None);
    assert!(!webserver.contains_key("cap_drop"));
    assert!(!webserver.contains_key("security_opt"));
}

#[test]
fn test_explicit_init_false_keeps_sandbox_but_does_not_wrap_image_init() {
    // Constructing the Bollard client does not contact the daemon. Keep
    // this pure YAML-generation regression mandatory in Docker-less CI.
    let executor = ComposeExecutor::new(
        Arc::new(Docker::connect_with_defaults().expect("construct Docker client")),
        PathBuf::from("/tmp/test"),
    );
    let compose = r#"
x-image-init: &image-init
  init: false
services:
  budge:
    image: lscr.io/linuxserver/budge:latest
    <<: *image-init
  worker:
    image: alpine:latest
"#;

    let override_yaml = executor.generate_security_override(compose, &[]);
    let override_value: Value = yaml_serde::from_str(&override_yaml).unwrap();
    let budge = override_value["services"]["budge"].as_mapping().unwrap();
    let worker = override_value["services"]["worker"].as_mapping().unwrap();

    assert_eq!(budge.get("init"), None);
    assert_eq!(budge.get("pids_limit").and_then(Value::as_u64), Some(512));
    assert!(budge.contains_key("cap_drop"));
    assert!(budge.contains_key("cap_add"));
    assert_eq!(worker.get("init").and_then(Value::as_bool), Some(true));
}

#[test]
fn test_detects_image_owned_init_entrypoints_without_image_name_rules() {
    for entrypoint in [
        vec!["/init"],
        vec!["/usr/bin/tini", "--"],
        vec!["/usr/local/bin/dumb-init", "--"],
        vec!["/package/admin/s6/command/s6-svscan"],
        vec!["/usr/libexec/s6-overlay/s6-overlay-suexec"],
        vec!["/usr/bin/catatonit", "--"],
        vec!["/usr/bin/runsvdir"],
    ] {
        let entrypoint = entrypoint
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert!(
            ComposeExecutor::entrypoint_owns_pid_one(&entrypoint),
            "expected {entrypoint:?} to own PID 1"
        );
    }

    for entrypoint in [
        vec!["/bin/sh", "-c", "exec /init"],
        vec!["/app/initialize"],
        vec!["/app/my-tini-wrapper"],
        vec!["node", "server.js"],
    ] {
        let entrypoint = entrypoint
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert!(
            !ComposeExecutor::entrypoint_owns_pid_one(&entrypoint),
            "expected {entrypoint:?} to keep Docker init"
        );
    }
    assert!(!ComposeExecutor::entrypoint_owns_pid_one(&[]));
}

#[test]
fn test_detected_image_init_omits_only_init_wrapper() {
    let executor = ComposeExecutor::new(
        Arc::new(Docker::connect_with_defaults().expect("construct Docker client")),
        PathBuf::from("/tmp/test"),
    );
    let compose = r#"
services:
  image-init:
    image: example.com/custom/runtime:latest
  ordinary-app:
    image: example.com/custom/app:latest
"#;
    let detected = HashSet::from(["image-init".to_string()]);

    let override_yaml = executor.generate_security_override_with_image_init(
        compose,
        &[],
        &detected,
        &HashMap::new(),
    );
    let override_value: Value = yaml_serde::from_str(&override_yaml).unwrap();
    let image_init = override_value["services"]["image-init"]
        .as_mapping()
        .unwrap();
    let ordinary_app = override_value["services"]["ordinary-app"]
        .as_mapping()
        .unwrap();

    assert_eq!(image_init.get("init").and_then(Value::as_bool), Some(false));
    assert_eq!(
        image_init.get("pids_limit").and_then(Value::as_u64),
        Some(512)
    );
    assert!(image_init.contains_key("cap_drop"));
    assert!(image_init.contains_key("security_opt"));
    assert_eq!(
        ordinary_app.get("init").and_then(Value::as_bool),
        Some(true)
    );
}

#[tokio::test]
async fn test_resolved_compose_detects_init_from_pulled_image_metadata() {
    let Some(executor) = test_executor() else {
        return;
    };
    let compose_available = tokio::process::Command::new("docker")
        .args(["compose", "version"])
        .output()
        .await
        .is_ok_and(|output| output.status.success());
    for image in ["ghcr.io/advplyr/audiobookshelf:2.34.0", "alpine:latest"] {
        if !compose_available
            || executor
                .docker
                .require()
                .expect("test_executor only builds an available handle")
                .inspect_image(image)
                .await
                .is_err()
        {
            println!("Docker Compose or {image} is unavailable; skipping runtime test");
            return;
        }
    }

    let project_dir = tempfile::tempdir().unwrap();
    let compose = r#"
services:
  custom-image-with-init:
    image: ghcr.io/advplyr/audiobookshelf:2.34.0
  ordinary-image:
    image: alpine:latest
"#;
    tokio::fs::write(project_dir.path().join("docker-compose.yml"), compose)
        .await
        .unwrap();
    tokio::fs::write(
        project_dir.path().join("docker-compose.temps-security.yml"),
        executor.generate_security_override(compose, &[]),
    )
    .await
    .unwrap();

    let detected = executor
        .detect_image_owned_init_services(
            project_dir.path(),
            "temps-image-init-metadata-test",
            "docker-compose.yml",
            &[],
        )
        .await
        .unwrap();

    assert_eq!(
        detected,
        HashSet::from(["custom-image-with-init".to_string()])
    );
}

#[test]
fn test_security_override_keeps_bounds_for_explicitly_unsandboxed_services() {
    let Some(executor) = test_executor() else {
        return;
    };
    let compose = r#"
services:
  webserver:
    image: ghcr.io/paperless-ngx/paperless-ngx:latest
  worker:
    image: alpine:latest
"#;

    let override_yaml = executor.generate_security_override(compose, &["webserver".to_string()]);

    assert!(override_yaml.contains("  webserver:"));
    assert!(override_yaml.contains("  worker:"));
    assert_eq!(override_yaml.matches("cap_drop:").count(), 1);
    assert_eq!(override_yaml.matches("no-new-privileges:true").count(), 1);
    assert_eq!(override_yaml.matches("pids_limit: 512").count(), 2);
    assert_eq!(override_yaml.matches("init: true").count(), 1);

    let override_value: Value = yaml_serde::from_str(&override_yaml).unwrap();
    let webserver = override_value["services"]["webserver"]
        .as_mapping()
        .unwrap();
    assert_eq!(webserver.get("init"), None);
    assert_eq!(webserver.get("privileged"), None);
    assert!(!webserver.contains_key("cap_drop"));
    assert!(!webserver.contains_key("security_opt"));
}

#[tokio::test]
async fn test_security_override_runtime_matches_per_service_selection() {
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

    let compose = r#"
services:
  image-owned-init:
    image: alpine:latest
    command: ["sleep", "30"]
    network_mode: none
  sandboxed:
    image: alpine:latest
    command: ["sleep", "30"]
    network_mode: none
"#;
    let override_yaml =
        executor.generate_security_override(compose, &["image-owned-init".to_string()]);
    let project_dir = tempfile::tempdir().unwrap();
    let compose_path = project_dir.path().join("compose.yml");
    let override_path = project_dir.path().join("security.yml");
    tokio::fs::write(&compose_path, compose).await.unwrap();
    tokio::fs::write(&override_path, override_yaml)
        .await
        .unwrap();
    let project_name = format!("temps-security-runtime-{}", std::process::id());
    let compose_args = |action: &str| {
        vec![
            "compose".to_string(),
            "-p".to_string(),
            project_name.clone(),
            "-f".to_string(),
            compose_path.to_string_lossy().into_owned(),
            "-f".to_string(),
            override_path.to_string_lossy().into_owned(),
            action.to_string(),
        ]
    };

    let up = tokio::process::Command::new("docker")
        .args(compose_args("up"))
        .args(["-d", "--no-build"])
        .output()
        .await
        .unwrap();
    assert!(
        up.status.success(),
        "Docker Compose runtime setup failed: {}",
        String::from_utf8_lossy(&up.stderr)
    );

    let docker = executor
        .docker
        .require()
        .expect("test_executor only builds an available handle");
    let inspect_service = |service: &'static str| {
        let args = compose_args("ps");
        let docker = docker.clone();
        async move {
            let output = tokio::process::Command::new("docker")
                .args(args)
                .args(["-q", service])
                .output()
                .await
                .unwrap();
            let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
            docker.inspect_container(&id, None).await.unwrap()
        }
    };
    let image_owned = inspect_service("image-owned-init").await;
    let sandboxed = inspect_service("sandboxed").await;

    let down = tokio::process::Command::new("docker")
        .args(compose_args("down"))
        .args(["--remove-orphans", "--volumes"])
        .output()
        .await
        .unwrap();
    assert!(
        down.status.success(),
        "Docker Compose cleanup failed: {}",
        String::from_utf8_lossy(&down.stderr)
    );

    let image_owned_host = image_owned.host_config.unwrap();
    assert_ne!(image_owned_host.init, Some(true));
    assert!(image_owned_host.cap_drop.unwrap_or_default().is_empty());
    assert!(image_owned_host.security_opt.unwrap_or_default().is_empty());
    assert_eq!(image_owned_host.pids_limit, Some(512));

    let sandboxed_host = sandboxed.host_config.unwrap();
    assert_eq!(sandboxed_host.init, Some(true));
    assert_eq!(sandboxed_host.cap_drop.unwrap_or_default(), ["ALL"]);
    assert!(sandboxed_host
        .security_opt
        .unwrap_or_default()
        .iter()
        .any(|option| option == "no-new-privileges:true"));
    assert_eq!(sandboxed_host.pids_limit, Some(512));
}

#[test]
fn labels_override_supports_inline_service_mappings() {
    let Some(executor) = test_executor() else {
        return;
    };
    let compose = r#"services: {web: {image: nginx:alpine}, worker: {image: alpine}}"#;
    let labels = HashMap::from([
        ("sh.temps.managed".to_string(), "true".to_string()),
        ("sh.temps.project_id".to_string(), "42".to_string()),
    ]);

    let rendered = executor.generate_labels_override(compose, &labels);
    let parsed: yaml_serde::Value = yaml_serde::from_str(&rendered).unwrap();
    let services = parsed
        .get("services")
        .and_then(yaml_serde::Value::as_mapping)
        .unwrap();

    for service in ["web", "worker"] {
        let service_config = services
            .get(yaml_serde::Value::String(service.to_string()))
            .and_then(yaml_serde::Value::as_mapping)
            .unwrap();
        let generated_labels = service_config
            .get(yaml_serde::Value::String("labels".to_string()))
            .and_then(yaml_serde::Value::as_mapping)
            .unwrap();
        assert_eq!(
            generated_labels.get(yaml_serde::Value::String("sh.temps.managed".to_string())),
            Some(&yaml_serde::Value::String("true".to_string()))
        );
        assert_eq!(
            generated_labels.get(yaml_serde::Value::String("sh.temps.service".to_string())),
            Some(&yaml_serde::Value::String(service.to_string()))
        );
    }
}

#[test]
fn test_security_override_preserves_bounds_when_every_service_is_unsandboxed() {
    let Some(executor) = test_executor() else {
        return;
    };
    let compose = "services:\n  webserver:\n    image: paperless:latest\n";

    let override_yaml = executor.generate_security_override(compose, &["webserver".to_string()]);

    let override_value: Value = yaml_serde::from_str(&override_yaml).unwrap();
    let webserver = override_value["services"]["webserver"]
        .as_mapping()
        .unwrap();
    assert_eq!(
        webserver.get("pids_limit").and_then(Value::as_u64),
        Some(512)
    );
    assert!(!webserver.contains_key("cpus"));
    assert!(webserver.contains_key("mem_limit"));
    assert!(webserver.contains_key("logging"));
    assert_eq!(webserver.get("init"), None);
    assert_eq!(webserver.get("privileged"), None);
    assert!(!webserver.contains_key("cap_drop"));
    assert!(!webserver.contains_key("security_opt"));
}

#[test]
fn test_security_exemptions_reject_overlapping_modes_at_deploy_time() {
    let error = ComposeExecutor::validate_security_exemptions(
        &["webserver".to_string()],
        &["webserver".to_string()],
    )
    .unwrap_err();

    assert!(matches!(
        error,
        ComposeError::SecurityPolicyViolation {
            ref service,
            ref field,
            ..
        } if service == "webserver" && field == "sandbox"
    ));
}

#[test]
fn test_generate_security_override_grants_baseline_capabilities_to_every_service() {
    let docker = Docker::connect_with_defaults();
    if docker.is_err() {
        return;
    }
    let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

    // A plain web server (nginx) needs the exact same baseline as a
    // database entrypoint to `chown` its cache dir and drop from root —
    // both must get it without any opt-in.
    let compose = r#"
services:
  db:
    image: postgres:18
  web:
    image: nginx
"#;
    let override_yaml = executor.generate_security_override(compose, &[]);

    // Both services get the strict cap_drop baseline...
    assert_eq!(override_yaml.matches("cap_drop:").count(), 2);
    assert_eq!(override_yaml.matches("no-new-privileges:true").count(), 2);

    // ...and both get cap_add back, with exactly the minimal safe set.
    assert_eq!(override_yaml.matches("cap_add:").count(), 2);
    for cap in ["CHOWN", "DAC_OVERRIDE", "FOWNER", "SETUID", "SETGID"] {
        assert_eq!(
            override_yaml.matches(cap).count(),
            2,
            "expected exactly one occurrence of {cap} per service"
        );
    }

    let parsed: yaml_serde::Value =
        yaml_serde::from_str(&override_yaml).expect("generated override is valid YAML");
    for service in ["db", "web"] {
        let caps = parsed["services"][service]["cap_add"]
            .as_sequence()
            .unwrap_or_else(|| panic!("{service} has cap_add sequence"))
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            caps,
            vec!["CHOWN", "DAC_OVERRIDE", "FOWNER", "SETUID", "SETGID"]
        );
    }
}

#[test]
fn test_generate_security_override_unsandboxed_service_gets_no_cap_add() {
    let docker = Docker::connect_with_defaults();
    if docker.is_err() {
        return;
    }
    let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

    let compose = "services:\n  db:\n    image: postgres:18\n";
    let override_yaml = executor.generate_security_override(compose, &["db".to_string()]);

    // Fully unsandboxed services skip the sandbox entirely, so they
    // don't get cap_drop/cap_add at all — Docker's own defaults apply.
    assert!(!override_yaml.contains("cap_drop"));
    assert!(!override_yaml.contains("cap_add"));
}

#[test]
fn test_security_override_preserves_memory_and_log_bounds_without_cpu_cap() {
    let Some(executor) = test_executor() else {
        return;
    };
    let compose = "services:\n  image_app:\n    image: alpine\n  built_app:\n    build: .\n";
    let override_yaml = executor.generate_security_override(compose, &[]);
    let parsed: YamlValue = yaml_serde::from_str(&override_yaml).unwrap();

    for service in ["image_app", "built_app"] {
        // A platform-injected four-core cap prevents deployment on two-core hosts.
        let service_config = parsed["services"][service].as_mapping().unwrap();
        for field in [
            "cpus",
            "cpu_count",
            "cpu_quota",
            "cpu_period",
            "cpuset",
            "deploy",
        ] {
            assert!(
                !service_config.contains_key(field),
                "unexpected CPU override: {field}"
            );
        }
        assert_eq!(
            parsed["services"][service]["mem_limit"].as_str(),
            Some("4g")
        );
        assert_eq!(
            parsed["services"][service]["logging"]["driver"].as_str(),
            Some("json-file")
        );
        assert_eq!(
            parsed["services"][service]["logging"]["options"]["max-size"].as_str(),
            Some("50m")
        );
        assert_eq!(
            parsed["services"][service]["logging"]["options"]["max-file"].as_str(),
            Some("3")
        );
    }
    assert_eq!(
        parsed["services"]["image_app"]["pull_policy"].as_str(),
        Some("always")
    );
    assert!(parsed["services"]["built_app"]["pull_policy"].is_null());
}

#[test]
fn test_generate_security_override_inline_and_anchor_services() {
    let Some(executor) = test_executor() else {
        return;
    };
    // Inline mapping and anchor service definitions that the old
    // line-based parser missed.
    let compose = r#"
services:
  web: { image: nginx }
  worker: &app
    image: alpine
"#;
    let override_yaml = executor.generate_security_override(compose, &[]);
    assert!(override_yaml.contains("web:"));
    assert!(override_yaml.contains("worker:"));
    assert_eq!(override_yaml.matches("cap_drop:").count(), 2);
    assert_eq!(override_yaml.matches("init: true").count(), 2);
}

#[test]
fn test_strip_ports_no_services_matched() {
    let docker = Docker::connect_with_defaults();
    if docker.is_err() {
        return;
    }
    let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

    let compose = r#"services:
  web:
    image: nginx
    ports:
      - '80:80'
"#;

    // No services to strip — output should be identical
    let result = executor.strip_ports_for_services(compose, &[]);
    assert!(result.contains("80:80"));
}

#[test]
fn test_generate_security_override_sets_privileged_false() {
    let Some(executor) = test_executor() else {
        return;
    };
    let compose = "services:\n  web:\n    image: nginx\n  worker:\n    image: alpine\n";
    let override_yaml = executor.generate_security_override(compose, &[]);
    assert_eq!(override_yaml.matches("privileged: false").count(), 2);
}

#[test]
fn runtime_exceptions_remove_only_the_selected_injected_setting() {
    let compose = "services: {app: {image: nginx:alpine}}";
    for (check, field) in [
        (PolicyCheck::Privileged, "privileged"),
        (PolicyCheck::DropCapabilities, "cap_drop"),
        (PolicyCheck::NoNewPrivileges, "security_opt"),
        (PolicyCheck::Pids, "pids_limit"),
        (PolicyCheck::Memory, "mem_limit"),
        (PolicyCheck::Logging, "logging"),
        (PolicyCheck::Init, "init"),
        (PolicyCheck::PullPolicy, "pull_policy"),
    ] {
        let strict: Value = yaml_serde::from_str(
            &executor_with_checks_disabled(&[]).generate_security_override(compose, &[]),
        )
        .unwrap();
        assert!(strict["services"]["app"].get(field).is_some());
        let relaxed: Value = yaml_serde::from_str(
            &executor_with_checks_disabled(&[check]).generate_security_override(compose, &[]),
        )
        .unwrap();
        assert!(
            relaxed["services"]["app"].get(field).is_none(),
            "{check:?} was re-injected"
        );
        let other = if field == "privileged" {
            "pids_limit"
        } else {
            "privileged"
        };
        assert!(relaxed["services"]["app"].get(other).is_some());
    }
}

#[test]
fn allowed_host_network_does_not_receive_a_conflicting_network_override() {
    let executor = executor_with_checks_disabled(&[PolicyCheck::HostNetwork]);
    let yaml: Value = yaml_serde::from_str(&executor.generate_network_override(
        "services: {host: {image: nginx, network_mode: host}, web: {image: nginx}}",
    ))
    .unwrap();
    assert!(yaml["services"].get("host").is_none());
    assert!(yaml["services"].get("web").is_some());
}

#[test]
fn acknowledged_policy_overrides_legacy_sandbox_grants() {
    let executor = executor_with_checks_disabled(&[]);
    let yaml: Value = yaml_serde::from_str(
        &executor
            .generate_security_override("services: {app: {image: nginx}}", &["app".to_string()]),
    )
    .unwrap();
    let app = &yaml["services"]["app"];
    assert_eq!(app["privileged"], Value::Bool(false));
    assert!(app.get("cap_drop").is_some());
    assert!(app.get("security_opt").is_some());
    assert_eq!(app["init"], Value::Bool(true));
}
