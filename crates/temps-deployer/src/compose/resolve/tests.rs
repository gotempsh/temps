// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tests for [`super`].

use super::*;
use crate::compose::test_support::{disabled_executor, executor_with_checks_disabled};
use crate::compose::{ComposeConfigFailureClass, ComposeError, ComposeExecutor};
use temps_entities::compose_security::ComposeSecurityPolicy;

#[tokio::test]
async fn compose_config_failure_reports_safe_validation_cause() {
    if !std::process::Command::new("docker")
        .args(["compose", "version"])
        .output()
        .is_ok_and(|result| result.status.success())
    {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let executor = executor_with_checks_disabled(&[]);
    let error = executor
        .resolve_security_configuration(
            "temps-config-diagnostic-test",
            Some(root.path()),
            "compose.yml",
            "services: {web: {image: nginx, ports: not-a-list}}",
            Some("services: {web: {environment: {TOKEN: repo-only-secret}}}"),
            &HashMap::new(),
        )
        .await
        .unwrap_err();
    let ComposeError::ConfigResolutionFailed {
        class, diagnostics, ..
    } = &error
    else {
        panic!("expected a config resolution failure, got {error}");
    };
    assert_eq!(*class, ComposeConfigFailureClass::InvalidModel);
    assert_eq!(error.class(), ComposeCheckErrorClass::ConfigResolution);
    // The exact command is reported, so the step can be reproduced.
    assert!(diagnostics.argv.iter().any(|arg| arg == "config"));
    assert!(diagnostics.argv.iter().any(|arg| arg == "--no-interpolate"));
    assert!(diagnostics.exit_code.is_some_and(|code| code != 0));
    assert!(!diagnostics.stderr.is_empty());
    assert!(!diagnostics.stderr.contains("repo-only-secret"));
    let error = error.to_string();
    assert!(
        error.contains("services.web.ports expects a value of type array"),
        "{error}"
    );
    assert!(!error.contains("repo-only-secret"), "{error}");
}

#[test]
fn remote_runtime_files_are_rejected_but_build_and_env_assets_are_allowed() {
    let executor = executor_with_checks_disabled(&[]);
    let cache = ".temps-compose-sources-example";
    for content in [
            "services: {app: {volumes: [{type: bind, source: .temps-compose-sources-example/repo/data, target: /data}]}}",
            "services: {app: {volumes: ['.temps-compose-sources-example/repo/data:/data']}}",
            "configs: {cfg: {file: .temps-compose-sources-example/repo/config.txt}}",
            "secrets: {key: {file: .temps-compose-sources-example/repo/key.txt}}",
            "volumes: {data: {driver_opts: {type: none, device: .temps-compose-sources-example/repo/data}}}",
        ] {
            assert!(matches!(executor.validate_remote_runtime_assets(content, cache), Err(ComposeError::InvalidComposePath { reason, .. }) if reason.contains("outlive")), "{content}");
        }
    assert!(executor.validate_remote_runtime_assets("services: {app: {build: {context: .temps-compose-sources-example/repo}, env_file: .temps-compose-sources-example/repo/vars.env, volumes: [data:/data]} }", cache).is_ok());
}

#[test]
fn reset_tags_require_resolution_without_policy_exceptions() {
    assert_eq!(
        ComposeExecutor::resolution_reasons(
            "services: {app: {image: alpine, shm_size: !reset null}}",
            None
        ),
        vec!["Compose merge tags"]
    );
    assert!(ComposeExecutor::resolution_reasons(
        "services: {app: {image: alpine, shm_size: 128m}}",
        None
    )
    .is_empty());
}

#[test]
fn resolution_depends_on_file_features_not_policy() {
    // Interpolation used to force `docker compose config` when its check
    // was disabled, so disabling a check could add a failing step.
    let interpolated = "services: {app: {image: '${IMG:-ghcr.io/example/app:1}'}}";
    assert!(ComposeExecutor::resolution_reasons(interpolated, None).is_empty());
    assert!(ComposeExecutor::resolution_reasons(interpolated, Some("  \n")).is_empty());
    assert_eq!(
        ComposeExecutor::resolution_reasons(
            "include: [other.yml]\nservices: {app: {extends: {file: base.yml, service: app}}}",
            Some("services: {app: {ports: ['8080:80']}}")
        ),
        vec!["project override", "include", "extends"]
    );
}

#[tokio::test]
async fn compose_snapshots_reject_cycles_and_do_not_rewrite_repository_sources() {
    let root = tempfile::tempdir().unwrap();
    let first = "services: {app: {extends: {file: second.yml, service: base}}}";
    std::fs::write(root.path().join("first.yml"), first).unwrap();
    std::fs::write(
        root.path().join("second.yml"),
        "services: {base: {extends: {file: first.yml, service: app}}}",
    )
    .unwrap();
    let executor = executor_with_checks_disabled(&[PolicyCheck::Extends]);
    let mut snapshots = ComposeReferenceSnapshots::new(root.path()).unwrap();
    let error = executor
        .snapshot_compose_document(root.path(), root.path(), first, &mut snapshots)
        .await
        .unwrap_err();
    assert!(
        matches!(error, ComposeError::InvalidComposePath { reason, .. } if reason.contains("cyclic"))
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("first.yml")).unwrap(),
        first
    );
}

#[tokio::test]
async fn compose_snapshots_reject_remote_urls_before_fetch_when_extends_blocked() {
    let root = tempfile::tempdir().unwrap();
    let executor = executor_with_checks_disabled(&[]);
    let mut snapshots = ComposeReferenceSnapshots::new(root.path()).unwrap();
    let error = executor
        .snapshot_compose_document(
            root.path(),
            root.path(),
            "services: {app: {extends: {file: 'https://example.invalid/repo.git', service: base}}}",
            &mut snapshots,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, ComposeError::SecurityPolicyViolation { field, .. } if field == "extends")
    );
    assert!(snapshots.remote.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn compose_snapshots_confine_inherited_env_files_before_resolution() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.env"), "SECRET=private\n").unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("secret.env"),
        root.path().join("vars.env"),
    )
    .unwrap();
    let executor = executor_with_checks_disabled(&[PolicyCheck::Extends]);
    let source = "services: {app: {extends: {file: common.yml, service: base}}}";
    for env_file in [
        "vars.env",
        "[vars.env]",
        "[{path: vars.env, required: false}]",
    ] {
        std::fs::write(
            root.path().join("common.yml"),
            format!("services: {{base: {{image: alpine, env_file: {env_file}}}}}"),
        )
        .unwrap();
        let mut snapshots = ComposeReferenceSnapshots::new(root.path()).unwrap();
        let error = executor
            .snapshot_compose_document(root.path(), root.path(), source, &mut snapshots)
            .await
            .unwrap_err();
        assert!(
            matches!(error, ComposeError::SecurityPolicyViolation { field, reason, .. } if field == "env_file" && reason.contains("outside")),
            "{env_file}"
        );
    }
    std::fs::write(root.path().join("common.yml"), "services: {base: {image: alpine, env_file: [{path: optional/missing.env, required: false}]}}").unwrap();
    let mut snapshots = ComposeReferenceSnapshots::new(root.path()).unwrap();
    assert!(executor
        .snapshot_compose_document(root.path(), root.path(), source, &mut snapshots)
        .await
        .is_ok());
    std::fs::write(
        root.path().join("common.yml"),
        "services: {base: {image: alpine, env_file: vars.env}}",
    )
    .unwrap();
    let permitted = executor_with_checks_disabled(&[PolicyCheck::Extends, PolicyCheck::EnvFiles]);
    let mut snapshots = ComposeReferenceSnapshots::new(root.path()).unwrap();
    assert!(permitted
        .snapshot_compose_document(root.path(), root.path(), source, &mut snapshots)
        .await
        .is_ok());
}

#[cfg(unix)]
#[tokio::test]
async fn compose_include_uses_effective_project_directory_for_nested_env_guards() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let runtime = root.path().join("runtime");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&runtime).unwrap();
    std::fs::write(
        source.join("child.yml"),
        "include: [{path: grandchild.yml, env_file: vars.env}]",
    )
    .unwrap();
    std::fs::write(source.join("vars.env"), "VALUE=safe\n").unwrap();
    std::fs::write(
        runtime.join("grandchild.yml"),
        "services: {app: {image: alpine}}",
    )
    .unwrap();
    std::fs::write(outside.path().join("secret.env"), "VALUE=private\n").unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret.env"), runtime.join("vars.env"))
        .unwrap();
    let executor = executor_with_checks_disabled(&[PolicyCheck::Include]);
    let mut snapshots = ComposeReferenceSnapshots::new(root.path()).unwrap();
    let error = executor
        .snapshot_compose_document(
            root.path(),
            root.path(),
            "include: [{path: source/child.yml, project_directory: runtime}]",
            &mut snapshots,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, ComposeError::InvalidComposePath { reason, .. } if reason.contains("inside the project"))
    );
}

#[tokio::test]
async fn remote_compose_snapshots_rebase_assets_and_confine_nested_files() {
    let root = tempfile::tempdir().unwrap();
    let executor = executor_with_checks_disabled(&[PolicyCheck::Extends, PolicyCheck::Include]);
    let mut snapshots = ComposeReferenceSnapshots::new(root.path()).unwrap();
    let remote = snapshots.cache.path().join("repository");
    std::fs::create_dir(&remote).unwrap();
    std::fs::write(remote.join("vars.env"), "SETTING=value\n").unwrap();
    std::fs::write(
        remote.join("compose.yml"),
        "services: {base: {image: alpine, env_file: vars.env}}",
    )
    .unwrap();
    let reference = "https://github.com/example/stack.git";
    snapshots.remote.insert(
        reference.to_string(),
        (remote.join("compose.yml"), remote.clone()),
    );
    let source = format!("services: {{app: {{extends: {{file: '{reference}', service: base}}}}}}");
    let input = executor
        .snapshot_compose_document(root.path(), root.path(), &source, &mut snapshots)
        .await
        .unwrap();
    assert_eq!(snapshots.remote.len(), 1);
    let original = std::fs::read_to_string(remote.join("compose.yml")).unwrap();
    assert_eq!(
        original,
        "services: {base: {image: alpine, env_file: vars.env}}"
    );
    if std::process::Command::new("docker")
        .args(["compose", "version"])
        .output()
        .is_ok_and(|result| result.status.success())
    {
        let compose_base = std::fs::canonicalize(root.path()).unwrap();
        // Match the resolver's working directory: Compose versions that
        // check relative env files during config resolve them against cwd.
        let output = std::process::Command::new("docker")
            .args([
                "compose",
                "-p",
                "temps-remote-snapshot-test",
                "--project-directory",
            ])
            .arg(&compose_base)
            .arg("-f")
            .arg(&input)
            .current_dir(&compose_base)
            .args([
                "config",
                "--no-normalize",
                "--no-path-resolution",
                "--no-env-resolution",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let yaml: Value = yaml_serde::from_slice(&output.stdout).unwrap();
        if let Some(env_path) = yaml["services"]["app"]["env_file"][0]["path"].as_str() {
            assert_eq!(
                std::fs::canonicalize(compose_base.join(env_path)).unwrap(),
                std::fs::canonicalize(remote.join("vars.env")).unwrap()
            );
        } else {
            // Older Compose versions eagerly resolve inherited env files
            // even with --no-env-resolution; assert the same effective data.
            assert_eq!(yaml["services"]["app"]["environment"]["SETTING"], "value");
        }
    }
    std::fs::write(
        root.path().join("private.yml"),
        "services: {private: {image: alpine}}",
    )
    .unwrap();
    let escape = format!("include: ['{}']", root.path().join("private.yml").display());
    let error = executor
        .snapshot_compose_document(&remote, &remote, &escape, &mut snapshots)
        .await
        .unwrap_err();
    assert!(
        matches!(error, ComposeError::InvalidComposePath { reason, .. } if reason.contains("inside the project"))
    );
}

#[tokio::test]
async fn compose_reset_removes_inherited_shm_and_keeps_effective_policy_checks() {
    if !std::process::Command::new("docker")
        .args(["compose", "version"])
        .output()
        .is_ok_and(|result| result.status.success())
    {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("common.yml"), "services: {base: {image: alpine, shm_size: 1gb, mem_limit: 3g}, unused: {image: alpine, privileged: true}}").unwrap();
    let executor = executor_with_checks_disabled(&[PolicyCheck::Extends]);
    let content = "services: {app: {extends: {file: common.yml, service: base}, shm_size: !reset null, mem_limit: !reset null}}";
    let (resolved, _) = executor
        .resolve_security_configuration(
            "temps-reset-test",
            Some(root.path()),
            "compose.yml",
            content,
            None,
            &HashMap::new(),
        )
        .await
        .unwrap();
    let yaml: Value = yaml_serde::from_str(&resolved).unwrap();
    assert!(yaml["services"]["app"].get("shm_size").is_none());
    assert!(yaml["services"]["app"].get("mem_limit").is_none());
    assert!(yaml["services"].get("unused").is_none());
    let inherited = "services: {app: {extends: {file: common.yml, service: base}}}";
    let error = executor
        .resolve_security_configuration(
            "temps-reset-test",
            Some(root.path()),
            "compose.yml",
            inherited,
            None,
            &HashMap::new(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, ComposeError::SecurityPolicyViolation { field, .. } if field == "shm_size")
    );
    let default_executor = executor_with_checks_disabled(&[]);
    assert!(default_executor
        .resolve_security_configuration(
            "temps-reset-test",
            Some(root.path()),
            "compose.yml",
            "services: {app: {image: alpine, shm_size: !reset null}}",
            None,
            &HashMap::new()
        )
        .await
        .is_ok());
}

#[tokio::test]
async fn extends_exception_resolves_inherited_services_and_still_rejects_privileged() {
    if !std::process::Command::new("docker")
        .args(["compose", "version"])
        .output()
        .is_ok_and(|result| result.status.success())
    {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("common.yml"),
        "services: {base: {image: nginx:alpine, environment: {MODE: shared}}}",
    )
    .unwrap();
    let content = "services: {web: {extends: {file: common.yml, service: base}}}";
    let executor = executor_with_checks_disabled(&[PolicyCheck::Extends]);
    let (resolved, overrides) = executor
        .resolve_security_configuration(
            "temps-policy-test",
            Some(root.path()),
            "compose.yml",
            content,
            None,
            &HashMap::new(),
        )
        .await
        .unwrap();
    let value: Value = yaml_serde::from_str(&resolved).unwrap();
    assert_eq!(
        value["services"]["web"]["image"].as_str(),
        Some("nginx:alpine")
    );
    assert!(value["services"]["web"].get("extends").is_none());
    assert!(overrides.is_none());
    std::fs::write(
        root.path().join("common.yml"),
        "services: {base: {image: nginx:alpine, privileged: true}}",
    )
    .unwrap();
    assert!(
        matches!(executor.resolve_security_configuration("temps-policy-test", Some(root.path()), "compose.yml", content, None, &HashMap::new()).await, Err(ComposeError::SecurityPolicyViolation { field, .. }) if field == "privileged")
    );
}

#[tokio::test]
async fn inline_ports_replace_inherited_and_source_ports_during_resolution() {
    if !std::process::Command::new("docker")
        .args(["compose", "version"])
        .output()
        .is_ok_and(|result| result.status.success())
    {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("common.yml"),
        "services: {base: {image: alpine, ports: ['8191:8191']}}",
    )
    .unwrap();
    let inline = "services: {app: {ports: ['127.0.0.1:8194:8191']}}";
    for (source, disabled_checks) in [
            (
                "services: {app: {image: alpine, ports: ['127.0.0.1:8191:8191']}}",
                Vec::new(),
            ),
            (
                "services: {app: {extends: {file: common.yml, service: base}, ports: ['127.0.0.1:8192:8191']}}",
                vec![PolicyCheck::Extends],
            ),
        ] {
            let executor = executor_with_checks_disabled(&disabled_checks);
            let (resolved, override_content) = executor
                .resolve_security_configuration(
                    "temps-port-override-test",
                    Some(root.path()),
                    "compose.yml",
                    source,
                    Some(inline),
                    &HashMap::new(),
                )
                .await
                .unwrap();
            assert!(override_content.is_none());
            let yaml: Value = yaml_serde::from_str(&resolved).unwrap();
            let ports = yaml["services"]["app"]["ports"].as_sequence().unwrap();
            assert_eq!(ports.len(), 1, "{resolved}");
            assert_eq!(ports[0]["published"].as_str(), Some("8194"));
            assert_eq!(ports[0]["host_ip"].as_str(), Some("127.0.0.1"));

            let clear_inline = "services: {app: {ports: !reset []}}";
            let (cleared, _) = executor
                .resolve_security_configuration(
                    "temps-port-override-test",
                    Some(root.path()),
                    "compose.yml",
                    source,
                    Some(clear_inline),
                    &HashMap::new(),
                )
                .await
                .unwrap();
            let cleared: Value = yaml_serde::from_str(&cleared).unwrap();
            assert!(cleared["services"]["app"].get("ports").is_none());

            let public_inline = "services: {app: {ports: ['0.0.0.0:8194:8191']}}";
            assert!(matches!(
                executor
                    .resolve_security_configuration(
                        "temps-port-override-test",
                        Some(root.path()),
                        "compose.yml",
                        source,
                        Some(public_inline),
                        &HashMap::new(),
                    )
                    .await,
                Err(ComposeError::SecurityPolicyViolation { field, .. }) if field == "ports"
            ));
        }
}

#[tokio::test]
async fn duplicate_extends_in_inline_override_keeps_repository_dependency_override() {
    if !std::process::Command::new("docker")
        .args(["compose", "version"])
        .output()
        .is_ok_and(|result| result.status.success())
    {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("common.yml"),
        "services: {base: {image: alpine, depends_on: [redis]}}",
    )
    .unwrap();
    let source = "services: {app: {extends: {file: common.yml, service: base}, depends_on: !override {warp: {condition: service_started}}}, warp: {image: alpine}}";
    let inline = "services: {app: {extends: {file: common.yml, service: base}, ports: ['127.0.0.1:8191:8191']}}";
    let executor = executor_with_checks_disabled(&[PolicyCheck::Extends]);
    let (resolved, override_content) = executor
        .resolve_security_configuration(
            "temps-duplicate-extends-test",
            Some(root.path()),
            "compose.yml",
            source,
            Some(inline),
            &HashMap::new(),
        )
        .await
        .unwrap();
    assert!(override_content.is_none());
    let yaml: Value = yaml_serde::from_str(&resolved).unwrap();
    assert!(yaml["services"]["app"]["depends_on"].get("redis").is_none());
    assert!(yaml["services"]["app"]["depends_on"].get("warp").is_some());
    assert_eq!(
        yaml["services"]["app"]["ports"][0]["published"].as_str(),
        Some("8191")
    );

    std::fs::write(
        root.path().join("common.yml"),
        "services: {base: {image: alpine, depends_on: [redis], privileged: true}}",
    )
    .unwrap();
    let error = executor
        .resolve_security_configuration(
            "temps-duplicate-extends-test",
            Some(root.path()),
            "compose.yml",
            source,
            Some(inline),
            &HashMap::new(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, ComposeError::SecurityPolicyViolation { field, .. } if field == "privileged")
    );
}

#[tokio::test]
async fn interpolation_exception_validates_the_resolved_value() {
    if !std::process::Command::new("docker")
        .args(["compose", "version"])
        .output()
        .is_ok_and(|result| result.status.success())
    {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let executor = executor_with_checks_disabled(&[PolicyCheck::Interpolation]);
    let content = "services: {app: {image: nginx:alpine, privileged: '${PRIVILEGED:-true}'}}";
    assert!(executor
        .resolve_security_configuration(
            "temps-policy-test",
            Some(root.path()),
            "compose.yml",
            content,
            None,
            &HashMap::new()
        )
        .await
        .is_err());
    std::fs::write(root.path().join(".env"), "PRIVILEGED=true\n").unwrap();
    let env = HashMap::from([("PRIVILEGED".to_string(), "false".to_string())]);
    assert!(executor
        .resolve_security_configuration(
            "temps-policy-test",
            Some(root.path()),
            "compose.yml",
            content,
            None,
            &env
        )
        .await
        .is_ok());
}

#[cfg(unix)]
#[tokio::test]
async fn include_env_symlinks_cannot_escape_the_repository() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("sub")).unwrap();
    std::fs::write(
        root.path().join("sub/compose.yml"),
        "services: {app: {image: nginx}}",
    )
    .unwrap();
    std::fs::write(outside.path().join("secret.env"), "SECRET=hidden").unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("secret.env"),
        root.path().join("sub/.env"),
    )
    .unwrap();
    let result = executor_with_checks_disabled(&[PolicyCheck::Include])
        .resolve_security_configuration(
            "test",
            Some(root.path()),
            "compose.yml",
            "include: [sub/compose.yml]",
            None,
            &HashMap::new(),
        )
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn include_exception_resolves_shared_stack_without_waiving_host_network() {
    if !std::process::Command::new("docker")
        .args(["compose", "version"])
        .output()
        .is_ok_and(|result| result.status.success())
    {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("shared.yml"),
        "services: {proxy: {image: nginx:alpine}}",
    )
    .unwrap();
    let executor = executor_with_checks_disabled(&[PolicyCheck::Include]);
    let content = "include: [shared.yml]";
    let (resolved, _) = executor
        .resolve_security_configuration(
            "include-test",
            Some(root.path()),
            "compose.yml",
            content,
            None,
            &HashMap::new(),
        )
        .await
        .unwrap();
    let value: Value = yaml_serde::from_str(&resolved).unwrap();
    assert_eq!(
        value["services"]["proxy"]["image"].as_str(),
        Some("nginx:alpine")
    );
    std::fs::write(
        root.path().join("shared.yml"),
        "services: {proxy: {image: nginx:alpine, network_mode: host}}",
    )
    .unwrap();
    assert!(matches!(
        executor
            .resolve_security_configuration(
                "include-test",
                Some(root.path()),
                "compose.yml",
                content,
                None,
                &HashMap::new()
            )
            .await,
        Err(ComposeError::SecurityPolicyViolation { .. })
    ));
}

fn compose_cli_available() -> bool {
    std::process::Command::new("docker")
        .args(["compose", "version"])
        .output()
        .is_ok_and(|result| result.status.success())
}

#[tokio::test]
async fn included_files_are_interpolated_with_their_own_env_file() {
    if !compose_cli_available() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("child")).unwrap();
    std::fs::write(
            root.path().join("child/compose.yml"),
            "services:\n  child:\n    image: nginx:alpine\n    environment:\n      GREETING: ${GREETING:-fallback}\n      REQUIRED: ${REQUIRED:?set it}\n      SHARED: ${SHARED}\n",
        )
        .unwrap();
    std::fs::write(
        root.path().join("child/.env"),
        "GREETING=from-child\nREQUIRED=present\nSHARED=from-child\n",
    )
    .unwrap();
    let executor = executor_with_checks_disabled(&[PolicyCheck::Include]);
    let environment = HashMap::from([("SHARED".to_string(), "from-project".to_string())]);
    for content in [
        "include: [child/compose.yml]",
        "include:\n  - path: child/compose.yml\n",
        "include:\n  - path: child/compose.yml\n    env_file: child/.env\n",
    ] {
        let (resolved, _) = executor
            .resolve_security_configuration(
                "include-env-test",
                Some(root.path()),
                "compose.yml",
                content,
                None,
                &environment,
            )
            .await
            .unwrap_or_else(|error| panic!("{content}: {error}"));
        let value: Value = yaml_serde::from_str(&resolved).unwrap();
        let env = &value["services"]["child"]["environment"];
        assert_eq!(env["GREETING"].as_str(), Some("from-child"), "{content}");
        assert_eq!(env["REQUIRED"].as_str(), Some("present"), "{content}");
        // The including project's variables win over the include's file.
        assert_eq!(env["SHARED"].as_str(), Some("from-project"), "{content}");
    }
}

/// Contents of every snapshot written so far.
fn snapshot_contents(snapshots: &ComposeReferenceSnapshots) -> Vec<String> {
    snapshots
        .files
        .iter()
        .map(|file| std::fs::read_to_string(file.path()).unwrap())
        .collect()
}

#[tokio::test]
async fn later_include_env_files_build_on_earlier_ones() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("child.yml"),
        "services: {child: {image: alpine, environment: {RESULT: '${RESULT}'}}}",
    )
    .unwrap();
    std::fs::write(root.path().join("first.env"), "BASE=from-first\n").unwrap();
    std::fs::write(root.path().join("second.env"), "RESULT=${BASE}-second\n").unwrap();
    let executor = executor_with_checks_disabled(&[PolicyCheck::Include]);
    let mut snapshots = ComposeReferenceSnapshots::new(root.path()).unwrap();
    snapshots.interpolation = SnapshotInterpolation::new(HashMap::new(), false);
    executor
        .snapshot_compose_document(
            root.path(),
            root.path(),
            "include:\n  - path: child.yml\n    env_file: [first.env, second.env]\n",
            &mut snapshots,
        )
        .await
        .unwrap();
    let contents = snapshot_contents(&snapshots).join("\n");
    assert!(contents.contains("from-first-second"), "{contents}");
}

#[tokio::test]
async fn remote_includes_are_interpolated_with_their_own_environment() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("local.env"), "EXPLICIT=from-local\n").unwrap();
    let executor = executor_with_checks_disabled(&[PolicyCheck::Include]);
    for (content, expected) in [
        // The materialized source's own `.env`.
        (
            "include: ['https://github.com/example/stack.git']",
            "from-remote",
        ),
        // An explicit env_file, relative to the including file.
        (
            "include:\n  - path: https://github.com/example/stack.git\n    env_file: local.env\n",
            "from-local",
        ),
    ] {
        let mut snapshots = ComposeReferenceSnapshots::new(root.path()).unwrap();
        snapshots.interpolation = SnapshotInterpolation::new(HashMap::new(), false);
        let remote = snapshots.cache.path().join("repository");
        std::fs::create_dir(&remote).unwrap();
        std::fs::write(
                remote.join("compose.yml"),
                "services: {child: {image: alpine, environment: {GREETING: '${GREETING:-fallback}', EXPLICIT: '${EXPLICIT:-fallback}', REQUIRED: '${REQUIRED:?set it}'}}}",
            )
            .unwrap();
        std::fs::write(
            remote.join(".env"),
            "GREETING=from-remote\nEXPLICIT=from-remote\nREQUIRED=present\n",
        )
        .unwrap();
        if content.contains("env_file") {
            // With an explicit env_file the source's `.env` is not read.
            std::fs::write(remote.join(".env"), "").unwrap();
            std::fs::write(
                root.path().join("local.env"),
                "EXPLICIT=from-local\nREQUIRED=present\n",
            )
            .unwrap();
        }
        snapshots.remote.insert(
            "https://github.com/example/stack.git".to_string(),
            (remote.join("compose.yml"), remote.clone()),
        );
        executor
            .snapshot_compose_document(root.path(), root.path(), content, &mut snapshots)
            .await
            .unwrap_or_else(|error| panic!("{content}: {error}"));
        let contents = snapshot_contents(&snapshots).join("\n");
        assert!(
            contents.contains(&format!("EXPLICIT: {expected}")),
            "{content}: {contents}"
        );
        assert!(
            contents.contains("REQUIRED: present"),
            "{content}: {contents}"
        );
    }
}

#[tokio::test]
async fn repeated_includes_reuse_one_bounded_environment() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("child")).unwrap();
    std::fs::write(
        root.path().join("child/compose.yml"),
        "services: {child: {image: alpine}}",
    )
    .unwrap();
    // Each line doubles the previous value: ~2 MiB of values per parse.
    let mut doubling = String::from("A0=xxxxxxxxxxxxxxxx\n");
    for index in 1..=16 {
        doubling.push_str(&format!(
            "A{index}=${{A{}}}${{A{}}}\n",
            index - 1,
            index - 1
        ));
    }
    std::fs::write(root.path().join("child/.env"), &doubling).unwrap();
    let executor = executor_with_checks_disabled(&[PolicyCheck::Include]);

    // The same include repeated many times parses its env file once and
    // holds its values once.
    let mut snapshots = ComposeReferenceSnapshots::new(root.path()).unwrap();
    snapshots.interpolation = SnapshotInterpolation::new(HashMap::new(), false);
    let repeated = format!("include:\n{}", "  - child/compose.yml\n".repeat(512));
    executor
        .snapshot_compose_document(root.path(), root.path(), &repeated, &mut snapshots)
        .await
        .unwrap();
    assert_eq!(snapshots.interpolation.include_scopes.len(), 1);
    assert_eq!(snapshots.interpolation.loaded_values.len(), 17);

    // Distinct environments are charged to one budget for the whole
    // resolution.
    for index in 0..8 {
        std::fs::write(root.path().join(format!("child/{index}.env")), &doubling).unwrap();
    }
    let mut snapshots = ComposeReferenceSnapshots::new(root.path()).unwrap();
    snapshots.interpolation = SnapshotInterpolation::new(HashMap::new(), false);
    let distinct: String = (0..8)
        .map(|index| format!("  - path: child/compose.yml\n    env_file: child/{index}.env\n"))
        .collect();
    let error = executor
        .snapshot_compose_document(
            root.path(),
            root.path(),
            &format!("include:\n{distinct}"),
            &mut snapshots,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            ComposeError::Interpolation(InterpolationError::TooLarge { .. })
        ),
        "{error}"
    );
}

#[tokio::test]
async fn resolution_returns_the_referenced_files_as_written() {
    if !compose_cli_available() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("stack")).unwrap();
    let child = "services:\n  web:\n    image: nginx:alpine\n    environment:\n      NAME: ${APP_NAME}\n    healthcheck:\n      test: [CMD, curl, -f, 'http://localhost:8080/api/health']\n";
    std::fs::write(root.path().join("stack/web.yaml"), child).unwrap();
    let executor = executor_with_checks_disabled(&[PolicyCheck::Include]);
    let environment = HashMap::from([("APP_NAME".to_string(), "api".to_string())]);
    let resolved = executor
        .resolve_configuration(&ComposeCheckInput {
            project_name: "temps-referenced-sources-test",
            project_dir: Some(root.path()),
            compose_file: "compose.yml",
            content: "include: [stack/web.yaml]\n",
            override_content: None,
            excluded_services: &[],
            environment: &environment,
            repository_env: None,
            filesystem_checks: true,
            allow_cli: true,
            defer_unknown_required: false,
        })
        .await
        .unwrap();
    // The included file before substitution, so a literal health path
    // there can be told apart from a substituted value.
    assert_eq!(resolved.referenced_sources, vec![child.to_string()]);
    assert!(
        resolved.content.contains("NAME: api"),
        "{}",
        resolved.content
    );
}

fn excluded_db_input<'a>(
    project_dir: Option<&'a Path>,
    content: &'a str,
    override_content: Option<&'a str>,
    excluded_services: &'a [String],
    environment: &'a HashMap<String, String>,
    allow_cli: bool,
) -> ComposeCheckInput<'a> {
    ComposeCheckInput {
        project_name: "temps-excluded-required-test",
        project_dir,
        compose_file: "compose.yml",
        content,
        override_content,
        excluded_services,
        environment,
        repository_env: None,
        filesystem_checks: false,
        allow_cli,
        defer_unknown_required: false,
    }
}

#[tokio::test]
async fn excluded_services_never_block_on_their_required_variables() {
    let executor = executor_with_checks_disabled(&[]);
    let content = "services:\n  app:\n    image: nginx:alpine\n  db:\n    image: postgres:16\n    environment:\n      POSTGRES_PASSWORD: ${DB_PASSWORD:?required}\n";
    let excluded = vec!["db".to_string()];
    let environment = HashMap::new();
    let checked = executor
        .check_configuration(&excluded_db_input(
            None,
            content,
            None,
            &excluded,
            &environment,
            false,
        ))
        .await
        .unwrap();
    assert!(!checked.content.contains("postgres"), "{}", checked.content);
    // Discovery still sees every service.
    assert!(checked.resolved_content.contains("postgres:16"));
    assert!(checked.resolution.unset_variables.is_empty());
    assert!(!checked
        .resolution
        .referenced_variables
        .contains("DB_PASSWORD"));

    // A service that runs still requires its variables.
    let error = executor
        .check_configuration(&excluded_db_input(
            None,
            content,
            None,
            &[],
            &environment,
            false,
        ))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("DB_PASSWORD"), "{error}");

    // An excluded service a kept one inherits from still runs inside it,
    // whether the file or the override adds the `extends`.
    let inherited = content.replace(
        "image: nginx:alpine\n",
        "image: nginx:alpine\n    extends: db\n",
    );
    let override_extends = "services:\n  app:\n    extends:\n      service: db\n";
    for (content, override_content) in [
        (inherited.as_str(), None),
        (content, Some(override_extends)),
    ] {
        let error = executor
            .check_configuration(&excluded_db_input(
                None,
                content,
                override_content,
                &excluded,
                &environment,
                false,
            ))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("DB_PASSWORD"), "{error}");
    }
}

#[tokio::test]
async fn excluded_included_services_never_block_but_extends_sources_still_do() {
    if !compose_cli_available() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("stack")).unwrap();
    std::fs::write(
            root.path().join("stack/db.yaml"),
            "services:\n  db:\n    image: postgres:16\n    environment:\n      POSTGRES_PASSWORD: ${DB_PASSWORD:?required}\n",
        )
        .unwrap();
    let executor = executor_with_checks_disabled(&[PolicyCheck::Include, PolicyCheck::Extends]);
    let excluded = vec!["db".to_string()];
    let environment = HashMap::new();
    let included = "include: [stack/db.yaml]\nservices:\n  app:\n    image: nginx:alpine\n";
    let checked = executor
        .check_configuration(&excluded_db_input(
            Some(root.path()),
            included,
            None,
            &excluded,
            &environment,
            true,
        ))
        .await
        .unwrap();
    assert!(!checked.content.contains("postgres"), "{}", checked.content);

    // The override may configure the excluded service too.
    let override_content =
        "services:\n  db:\n    environment:\n      EXTRA: ${DB_EXTRA:?required}\n";
    let checked = executor
        .check_configuration(&excluded_db_input(
            Some(root.path()),
            "services:\n  app:\n    image: nginx:alpine\n  db:\n    image: postgres:16\n",
            Some(override_content),
            &excluded,
            &environment,
            true,
        ))
        .await
        .unwrap();
    assert!(!checked.content.contains("postgres"), "{}", checked.content);

    // `app` inherits the extended service's values, so they must resolve
    // even though a project service of the same name is excluded.
    let extended = "services:\n  app:\n    image: nginx:alpine\n    extends:\n      file: stack/db.yaml\n      service: db\n";
    let error = executor
        .check_configuration(&excluded_db_input(
            Some(root.path()),
            extended,
            None,
            &excluded,
            &environment,
            true,
        ))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("DB_PASSWORD"), "{error}");
}

#[tokio::test]
async fn repository_env_values_never_reach_config_diagnostics() {
    if !compose_cli_available() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join(".env"), "PASSWORD=repo-only-secret\n").unwrap();
    let executor = executor_with_checks_disabled(&[]);
    let error = executor
        .resolve_security_configuration(
            "temps-config-redaction-test",
            Some(root.path()),
            "compose.yml",
            "services: {web: {image: nginx, volumes: ['${PASSWORD}::']}}",
            Some("services: {web: {environment: {CHECK: 'yes'}}}"),
            &HashMap::from([("UNRELATED".to_string(), "platform-value".to_string())]),
        )
        .await
        .unwrap_err();
    let rendered = format!("{error} {error:?}");
    assert!(!rendered.contains("repo-only-secret"), "{rendered}");
}

#[tokio::test]
async fn resolution_keeps_compose_consistency_checks() {
    if !compose_cli_available() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let executor = executor_with_checks_disabled(&[]);
    let error = executor
        .resolve_security_configuration(
            "temps-config-consistency-test",
            Some(root.path()),
            "compose.yml",
            "services: {app: {image: nginx, depends_on: [missing]}}",
            Some("services: {app: {environment: {CHECK: 'yes'}}}"),
            &HashMap::new(),
        )
        .await
        .unwrap_err();
    let ComposeError::ConfigResolutionFailed {
        class, diagnostics, ..
    } = &error
    else {
        panic!("expected a config resolution failure, got {error}");
    };
    assert_eq!(*class, ComposeConfigFailureClass::UndefinedDependency);
    assert!(diagnostics.argv.iter().any(|arg| arg == "--quiet"));
}

// ── Deploy-success regressions: anonymous volumes, one-shot services,
//    subdirectory compose files, shared command inputs ─────────────────

/// The idioms an ordinary Compose file uses: an image with a default, an
/// optional-in-practice `.env` that is not committed, and defaults in
/// environment and ports.
const IDIOMATIC_COMPOSE: &str = "services:\n  app:\n    image: ${IMG:-ghcr.io/example/app:1}\n    env_file: .env\n    environment:\n      GREETING: ${GREETING:-x}\n    ports:\n      - \"127.0.0.1:${APP_PORT:-8080}:80\"\n";

fn idiomatic_input<'a>(
    root: &'a Path,
    environment: &'a HashMap<String, String>,
    override_content: Option<&'a str>,
) -> ComposeCheckInput<'a> {
    ComposeCheckInput {
        project_name: "temps-idiom-test",
        project_dir: Some(root),
        compose_file: "docker-compose.yml",
        content: IDIOMATIC_COMPOSE,
        override_content,
        excluded_services: &[],
        environment,
        repository_env: None,
        filesystem_checks: true,
        allow_cli: true,
        defer_unknown_required: false,
    }
}

fn resolved_service(content: &str) -> Value {
    let model: Value = yaml_serde::from_str(content).unwrap();
    model["services"]["app"].clone()
}

#[tokio::test]
async fn standard_idioms_pass_with_the_default_policy() {
    let root = tempfile::tempdir().unwrap();
    let environment = HashMap::new();
    let executor =
        disabled_executor(root.path().join("data")).with_security_policy(Default::default());
    let checked = executor
        .check_configuration(&idiomatic_input(root.path(), &environment, None))
        .await
        .unwrap();
    // No merge feature is used, so no `docker compose config` step runs.
    assert!(checked.resolution.resolution_reasons.is_empty());
    assert!(checked.resolution.command.is_none());
    let app = resolved_service(&checked.content);
    assert_eq!(app["image"], "ghcr.io/example/app:1");
    assert_eq!(app["environment"]["GREETING"], "x");
    assert_eq!(app["ports"][0], "127.0.0.1:8080:80");
    assert_eq!(app["env_file"], ".env");
    // Every reference has a default, so nothing is reported as unset.
    assert!(checked.resolution.unset_variables.is_empty());
    assert_eq!(
        checked.resolution.referenced_variables,
        BTreeSet::from(["APP_PORT", "GREETING", "IMG"].map(str::to_string))
    );
}

#[tokio::test]
async fn standard_idioms_behave_the_same_with_the_interpolation_check_disabled() {
    let root = tempfile::tempdir().unwrap();
    let environment = HashMap::new();
    let enforced = disabled_executor(root.path().join("data"))
        .with_security_policy(Default::default())
        .check_configuration(&idiomatic_input(root.path(), &environment, None))
        .await
        .unwrap();
    let relaxed = executor_with_checks_disabled(&[PolicyCheck::Interpolation])
        .check_configuration(&idiomatic_input(root.path(), &environment, None))
        .await
        .unwrap();
    assert_eq!(enforced.content, relaxed.content);
    assert_eq!(
        enforced.resolution.resolution_reasons,
        relaxed.resolution.resolution_reasons
    );
    assert!(relaxed.resolution.command.is_none());
}

#[tokio::test]
async fn standard_idioms_resolve_through_compose_config_with_an_override() {
    if !std::process::Command::new("docker")
        .args(["compose", "version"])
        .output()
        .is_ok_and(|result| result.status.success())
    {
        println!("Docker Compose not available, skipping");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let environment = HashMap::from([("IMG".to_string(), "ghcr.io/example/app:2".to_string())]);
    // Missing `.env` and a project variable overriding the image default.
    let override_content = "services:\n  app:\n    environment:\n      EXTRA: '${GREETING:-y}'\n";
    for checks in [&[][..], &[PolicyCheck::Interpolation][..]] {
        let checked = executor_with_checks_disabled(checks)
            .check_configuration(&idiomatic_input(
                root.path(),
                &environment,
                Some(override_content),
            ))
            .await
            .unwrap_or_else(|error| panic!("checks disabled {checks:?}: {error}"));
        assert_eq!(
            checked.resolution.resolution_reasons,
            vec!["project override"]
        );
        let command = checked.resolution.command.as_ref().unwrap();
        assert_eq!(command.exit_code, Some(0));
        let app = resolved_service(&checked.content);
        assert_eq!(app["image"], "ghcr.io/example/app:2", "{}", checked.content);
        // GREETING is unset, so the override's own default applies.
        assert!(
            app["environment"]
                .as_sequence()
                .is_some_and(|env| env.contains(&Value::from("EXTRA=y"))
                    && env.contains(&Value::from("GREETING=x"))),
            "{}",
            checked.content
        );
    }
}

#[tokio::test]
async fn substituted_values_are_still_held_to_the_policy_without_echoing_them() {
    let root = tempfile::tempdir().unwrap();
    let environment = HashMap::from([
        ("HOST_DIR".to_string(), "/srv/private-host-dir".to_string()),
        ("PRIV".to_string(), "yes".to_string()),
    ]);
    let executor =
        disabled_executor(root.path().join("data")).with_security_policy(Default::default());
    for (content, field) in [
            (
                "services: {app: {image: 'ghcr.io/example/app:1', volumes: ['${HOST_DIR}:/data']}}",
                "volumes",
            ),
            (
                "services: {app: {image: 'ghcr.io/example/app:1', privileged: '${PRIV:-false}'}}",
                "privileged",
            ),
            (
                "services: {app: {image: '${IMG:-sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa}'}}",
                "image",
            ),
            (
                "services: {app: {image: '${IMG:-temps.internal/other-project:latest}'}}",
                "image",
            ),
        ] {
            let error = executor
                .check_configuration(&ComposeCheckInput {
                    content,
                    ..idiomatic_input(root.path(), &environment, None)
                })
                .await
                .unwrap_err();
            assert_eq!(error.class(), ComposeCheckErrorClass::PolicyViolation, "{error}");
            assert!(
                matches!(&error, ComposeError::SecurityPolicyViolation { field: f, .. } if f == field),
                "{content}: {error}"
            );
            assert!(!error.to_string().contains("private-host-dir"), "{error}");
        }
}

#[tokio::test]
async fn required_variables_fail_with_their_name_only() {
    let root = tempfile::tempdir().unwrap();
    let environment = HashMap::new();
    let error = executor_with_checks_disabled(&[])
        .check_configuration(&ComposeCheckInput {
            content: "services: {app: {image: '${IMG:?set the image}'}}",
            ..idiomatic_input(root.path(), &environment, None)
        })
        .await
        .unwrap_err();
    assert_eq!(error.class(), ComposeCheckErrorClass::Interpolation);
    assert!(error.to_string().contains("IMG"), "{error}");
}

#[test]
fn missing_env_files_become_optional_for_the_config_step() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("present.env"), "A=1\n").unwrap();
    let mut definition: Value = yaml_serde::from_str(
        "env_file: [.env, present.env, {path: other.env}, {path: present.env, required: true}]",
    )
    .unwrap();
    ComposeExecutor::tolerate_missing_env_files(root.path(), &mut definition);
    let expected: Value = yaml_serde::from_str(
            "env_file: [{path: .env, required: false}, present.env, {path: other.env, required: false}, {path: present.env, required: true}]",
        )
        .unwrap();
    assert_eq!(definition, expected);

    let mut single: Value = yaml_serde::from_str("env_file: .env").unwrap();
    ComposeExecutor::tolerate_missing_env_files(root.path(), &mut single);
    assert_eq!(single["env_file"][0]["required"], Value::Bool(false));
}

#[tokio::test]
async fn preview_and_deploy_report_the_same_policy_error() {
    let root = tempfile::tempdir().unwrap();
    let content = "services: {app: {image: 'ghcr.io/example/app:1', privileged: true}}";
    let environment = HashMap::new();
    let executor =
        disabled_executor(root.path().join("data")).with_security_policy(Default::default());
    let deploy_error = executor
        .check_configuration(&ComposeCheckInput {
            content,
            ..idiomatic_input(root.path(), &environment, None)
        })
        .await
        .unwrap_err();
    let report = executor
        .check_report(ComposeCheckRequest {
            project_name: "temps-idiom-test".to_string(),
            compose_file: "docker-compose.yml".to_string(),
            content: content.to_string(),
            ..Default::default()
        })
        .await;
    assert!(!report.passed);
    assert_eq!(report.error_class, Some(deploy_error.class()));
    assert_eq!(
        report.error_class,
        Some(ComposeCheckErrorClass::PolicyViolation)
    );
    assert_eq!(
        report.error.as_deref(),
        Some(deploy_error.to_string().as_str())
    );
}

#[tokio::test]
async fn checks_without_a_checkout_resolve_fetched_reference_files() {
    if !compose_cli_available() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let executor = disabled_executor(root.path().join("data"));
    let request =
        |reference_files: std::collections::BTreeMap<String, String>| ComposeCheckRequest {
            project_name: "temps-reference-check".to_string(),
            compose_file: "compose.yaml".to_string(),
            content: "include: [stack/shared.yaml]\n".to_string(),
            policy: ComposeSecurityPolicy {
                disabled_checks: [PolicyCheck::Include].into_iter().collect(),
            },
            reference_files,
            ..Default::default()
        };
    let files = std::collections::BTreeMap::from([
            (
                "stack/shared.yaml".to_string(),
                "services:\n  proxy:\n    image: nginx:alpine\n    environment:\n      GREETING: ${GREETING:?set in stack/.env}\n".to_string(),
            ),
            ("stack/.env".to_string(), "GREETING=hello\n".to_string()),
        ]);
    let report = executor.check_report(request(files)).await;
    assert!(report.passed, "{:?}", report.error);
    assert!(report.resolution_ran);
    // Without the referenced file the check fails exactly as a checkout
    // missing it would.
    let report = executor.check_report(request(Default::default())).await;
    assert!(!report.passed);
    assert_eq!(
        report.error_class,
        Some(ComposeCheckErrorClass::InvalidPath)
    );
}

#[tokio::test]
async fn checks_outside_a_deployment_defer_variables_only_it_can_supply() {
    let root = tempfile::tempdir().unwrap();
    let executor = disabled_executor(root.path().join("data"));
    let content = "services:\n  app:\n    image: ghcr.io/example/app:1\n    environment:\n      DATABASE_URL: ${POSTGRES_URL:?database required}\n";
    let report = executor
        .check_report(ComposeCheckRequest {
            project_name: "temps-deferred-check".to_string(),
            compose_file: "compose.yaml".to_string(),
            content: content.to_string(),
            ..Default::default()
        })
        .await;
    assert!(report.passed, "{:?}", report.error);
    assert!(report.unset_variables.contains(&"POSTGRES_URL".to_string()));
    assert!(report
        .notes
        .iter()
        .any(|note| note.contains("POSTGRES_URL") && note.contains("linked services")));
    // The deployment itself still fails on it.
    let environment = HashMap::new();
    let error = executor
        .check_configuration(&ComposeCheckInput {
            content,
            allow_cli: false,
            ..idiomatic_input(root.path(), &environment, None)
        })
        .await
        .unwrap_err();
    assert_eq!(error.class(), ComposeCheckErrorClass::Interpolation);
}

#[tokio::test]
async fn resolution_carries_every_substituted_value_for_redaction() {
    let root = tempfile::tempdir().unwrap();
    let environment = HashMap::from([("PROJECT_TOKEN".to_string(), "project-secret".to_string())]);
    let content = "services:\n  app:\n    image: registry.example.com/${REPO_TOKEN}/app:1\n    env_file: ${PROJECT_TOKEN}.env\n";
    let resolved = executor_with_checks_disabled(&[])
        .resolve_configuration(&ComposeCheckInput {
            content,
            allow_cli: false,
            filesystem_checks: false,
            repository_env: Some("REPO_TOKEN=repository-secret\n"),
            ..idiomatic_input(root.path(), &environment, None)
        })
        .await
        .unwrap();
    // Both sources were substituted into the model...
    assert!(resolved.content.contains("project-secret.env"));
    assert!(resolved.content.contains("repository-secret/app"));
    // ...and text derived from it can be masked for either.
    let line = format!(
        "env file '{}' for image {}",
        "project-secret.env", "registry.example.com/repository-secret/app:1"
    );
    let redacted = resolved.substituted_values.redact(&line);
    assert!(!redacted.contains("project-secret"), "{redacted}");
    assert!(!redacted.contains("repository-secret"), "{redacted}");
    // Debug output never carries them.
    let debug = format!("{:?}", resolved.substituted_values);
    assert!(!debug.contains("secret"), "{debug}");
}

#[tokio::test]
async fn override_lists_repeating_the_repository_file_are_not_duplicated() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join(".env"), "A=1\n").unwrap();
    let environment = HashMap::new();
    let content = "services:\n  app:\n    image: ghcr.io/example/app:1\n    env_file: .env\n    volumes: [data:/data]\nvolumes: {data: {}}\n";
    let override_content = "services:\n  app:\n    env_file: [.env]\n    volumes: [data:/data, cache:/cache]\nvolumes: {cache: {}}\n";
    let resolved = executor_with_checks_disabled(&[])
        .resolve_configuration(&ComposeCheckInput {
            content,
            allow_cli: false,
            defer_unknown_required: false,
            ..idiomatic_input(root.path(), &environment, Some(override_content))
        })
        .await
        .unwrap();
    let overlay: Value =
        yaml_serde::from_str(resolved.override_content.as_deref().unwrap()).unwrap();
    assert!(
        overlay["services"]["app"].get("env_file").is_none(),
        "{overlay:?}"
    );
    assert_eq!(
        overlay["services"]["app"]["volumes"],
        yaml_serde::from_str::<Value>("[cache:/cache]").unwrap()
    );
    assert_eq!(resolved.notes.len(), 1);
}
