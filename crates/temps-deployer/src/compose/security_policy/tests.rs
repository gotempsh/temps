// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tests for [`super`].

use super::*;
use crate::compose::test_support::{
    disabled_executor, executor_with_checks_disabled, test_executor, violation_field,
};
use crate::compose::{ComposeError, ComposeExecutor};
use bollard::Docker;
use std::sync::Arc;

#[test]
fn test_validate_compose_override_allows_safe_service_changes() {
    let compose = r#"
services:
  web:
    image: nginx
"#;
    let override_content = r#"
services:
  web:
    ports:
      - "127.0.0.1:8080:80"
    environment:
      RUST_LOG: info
    command: ["nginx", "-g", "daemon off;"]
"#;

    ComposeExecutor::validate_compose_override("temps-test", compose, override_content).unwrap();
}

#[test]
fn test_validate_compose_override_allows_image_change_for_existing_service() {
    let compose = "services:\n  web:\n    image: example/web:1\n";
    let override_content = "services:\n  web:\n    image: example/web:2\n    ports: ['8080:80']\n";

    ComposeExecutor::validate_compose_override("temps-test", compose, override_content).unwrap();
}

#[test]
fn test_validate_compose_override_rejects_new_services() {
    let compose = r#"
services:
  web:
    image: nginx
"#;
    let override_content = r#"
services:
  attacker:
    image: alpine
"#;

    let error = ComposeExecutor::validate_compose_override("temps-test", compose, override_content)
        .unwrap_err();
    assert!(error.to_string().contains("cannot add service 'attacker'"));
    assert!(error.to_string().contains("Inline Services check"));
}

#[test]
fn test_validate_compose_override_rejects_host_escape_keys() {
    let compose = r#"
services:
  web:
    image: nginx
"#;
    let dangerous_overrides = [
        "privileged: true",
        "network_mode: host",
        "pid: host",
        "cap_add: [SYS_ADMIN]",
        "devices: ['/dev/kvm:/dev/kvm']",
        "security_opt: ['apparmor:unconfined']",
        "sysctls: {net.ipv4.ip_forward: '1'}",
        "volumes: ['/:/host:rw']",
        "volumes_from: ['container:temps-db']",
        "labels: {sh.temps.managed: 'false'}",
        "build: ./attacker",
        "env_file: ./override.env",
    ];

    for dangerous_override in dangerous_overrides {
        let override_content = format!(
            "services:
  web:
    {dangerous_override}
"
        );
        let error =
            ComposeExecutor::validate_compose_override("temps-test", compose, &override_content)
                .unwrap_err();
        let key = dangerous_override.split(':').next().unwrap();
        assert!(
            matches!(error, ComposeError::InvalidOverride { .. }),
            "expected {dangerous_override} to be rejected as an invalid override, got {error}"
        );
        assert!(
            error.to_string().contains(key),
            "expected the rejection of {dangerous_override} to name the key '{key}', got {error}"
        );
    }
}

/// The two denylists must stay disjoint: a key in both would make the
/// "never allowed" message win by ordering alone, and silently reintroduce
/// the misleading advice the split exists to remove.
#[test]
fn test_override_key_denylists_are_disjoint() {
    for key in NEVER_ALLOWED_OVERRIDE_KEYS {
        assert!(
            !REPO_ONLY_OVERRIDE_KEYS.contains(key),
            "'{key}' is classified both as never-allowed and as repo-only"
        );
        assert!(
            PolicyCheck::for_restricted_service_field(key).is_some(),
            "'{key}' needs a specific policy check"
        );
    }
}

/// A key the deploy-time policy rejects everywhere must not tell the user
/// to move it into the repository compose file — that advice costs them a
/// round trip and then fails with a different error.
#[test]
fn test_never_allowed_override_key_does_not_advise_moving_to_repository() {
    let compose = "services:\n  web:\n    image: nginx\n";
    let error = ComposeExecutor::validate_compose_override(
        "temps-test",
        compose,
        "services:\n  web:\n    privileged: true\n",
    )
    .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("disable that check"), "{message}");
    assert!(!message.contains("repository compose file"), "{message}");
}

/// A key the repository compose file legitimately accepts must point there,
/// since that is the route that actually works.
#[test]
fn test_repo_only_override_key_points_at_the_repository_compose_file() {
    let compose = "services:\n  web:\n    image: nginx\n";
    let error = ComposeExecutor::validate_compose_override(
        "temps-test",
        compose,
        "services:\n  web:\n    volumes: ['data:/var/lib/data']\n",
    )
    .unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("declare it in the repository compose file"),
        "expected the message to point at the repository compose file, got {message}"
    );
    assert!(message.contains("Inline Fields check"), "{message}");
}

#[test]
fn test_include_exception_allows_inline_include_without_inline_sections_exception() {
    let compose = "services:\n  web:\n    image: nginx\n";
    let override_content = "include:\n  - common.yml\n";
    let policy = ComposeSecurityPolicy {
        disabled_checks: std::collections::BTreeSet::from([PolicyCheck::Include]),
    };
    ComposeExecutor::validate_compose_override_with_policy(
        "temps-test",
        compose,
        override_content,
        &policy,
    )
    .unwrap();

    let Some(executor) = test_executor() else {
        return;
    };
    let error = executor
        .preflight_validate(compose, Some(override_content))
        .unwrap_err();
    assert_eq!(violation_field(error), "include");
    executor
        .with_security_policy(policy)
        .preflight_validate(compose, Some(override_content))
        .unwrap();
}

#[test]
fn test_inline_field_exception_does_not_disable_privileged_check() {
    let compose = "services:\n  web:\n    image: nginx\n";
    let override_content = "services:\n  web:\n    privileged: true\n    volumes: ['data:/data']\n";
    let policy = ComposeSecurityPolicy {
        disabled_checks: std::collections::BTreeSet::from([PolicyCheck::InlineFields]),
    };
    let error = ComposeExecutor::validate_compose_override_with_policy(
        "temps-test",
        compose,
        override_content,
        &policy,
    )
    .unwrap_err();
    assert!(error.to_string().contains("Privileged check"));

    let policy = ComposeSecurityPolicy {
        disabled_checks: std::collections::BTreeSet::from([
            PolicyCheck::InlineFields,
            PolicyCheck::Privileged,
        ]),
    };
    ComposeExecutor::validate_compose_override_with_policy(
        "temps-test",
        compose,
        override_content,
        &policy,
    )
    .unwrap();
}

#[test]
fn test_validate_compose_override_rejects_top_level_escape_keys() {
    let compose = r#"
services:
  web:
    image: nginx
"#;
    let override_content = r#"
services:
  web:
    ports:
      - "8080:80"
networks:
  hostnet:
    external: true
"#;

    let error = ComposeExecutor::validate_compose_override("temps-test", compose, override_content)
        .unwrap_err();
    assert!(error.to_string().contains("top-level key 'networks'"));
    assert!(error.to_string().contains("Inline Sections check"));
}

#[test]
fn test_has_build_directives() {
    let docker = Docker::connect_with_defaults();
    if docker.is_err() {
        return;
    }
    let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

    // No build
    assert!(!executor.has_build_directives("services:\n  web:\n    image: nginx\n"));

    // build: with context
    assert!(executor.has_build_directives("services:\n  web:\n    build: .\n"));

    // build: block
    assert!(executor.has_build_directives(
        "services:\n  web:\n    build:\n      context: .\n      dockerfile: Dockerfile\n"
    ));
}

/// `has_build_directives` must detect a `build:` stanza that appears only
/// in the compose override, not in the base file. This is the Bug 2 scenario:
/// a user override adds `build:` to a service that only has `image:` in the
/// base, making `compose_pull --ignore-buildable` skip that service (because
/// the merged view has a build stanza), while the old code skipped
/// `compose_build` too (because it only scanned the base file). The service
/// ended up neither built nor pulled.
#[test]
fn test_has_build_in_override_only_is_detected() {
    let docker = Docker::connect_with_defaults();
    if docker.is_err() {
        return;
    }
    let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

    // Base file has no build directive.
    let base = "services:\n  web:\n    image: nginx:latest\n";
    // Override adds a build stanza to the same service.
    let override_yaml = "services:\n  web:\n    build: ./custom\n";

    // The base alone should NOT look like a build project.
    assert!(
        !executor.has_build_directives(base),
        "base without build: should not be detected as a build project"
    );
    // The override alone SHOULD be detected.
    assert!(
        executor.has_build_directives(override_yaml),
        "override with build: should be detected"
    );
    // The combined check — as used in prepare_and_pull — must fire when
    // the override has the directive even if the base does not.
    let combined =
        executor.has_build_directives(base) || executor.has_build_directives(override_yaml);
    assert!(
        combined,
        "combined base+override check must detect build: in the override"
    );
}

/// Verify that the combined base+override check does NOT produce a false
/// positive when neither document contains a `build:` directive (e.g. a
/// purely `image:`-based stack with an override that only changes env vars).
#[test]
fn test_no_build_in_base_or_override_is_not_detected() {
    let docker = Docker::connect_with_defaults();
    if docker.is_err() {
        return;
    }
    let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

    let base = "services:\n  web:\n    image: nginx:latest\n";
    let override_yaml = "services:\n  web:\n    environment:\n      - FOO=bar\n";

    let combined =
        executor.has_build_directives(base) || executor.has_build_directives(override_yaml);
    assert!(
        !combined,
        "stack with no build: directives in either document must not be flagged as a build project"
    );
}

#[test]
fn test_validate_compose_security_policy_rejects_privileged_host_escape() {
    let docker = Docker::connect_with_defaults();
    if docker.is_err() {
        return;
    }
    let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

    let compose = r#"
services:
  pwn:
    image: alpine
    privileged: true
    network_mode: host
    pid: host
    cap_add:
      - SYS_ADMIN
    devices:
      - /dev/kmsg:/dev/kmsg
    volumes:
      - /:/host:rw
"#;

    let error = executor
        .validate_compose_security_policy("compose file", compose)
        .unwrap_err();
    assert!(matches!(
        error,
        ComposeError::SecurityPolicyViolation { field, .. } if field == "privileged"
    ));
}

#[test]
fn test_validate_compose_security_policy_rejects_docker_socket_mount() {
    let docker = Docker::connect_with_defaults();
    if docker.is_err() {
        return;
    }
    let executor = ComposeExecutor::new(Arc::new(docker.unwrap()), PathBuf::from("/tmp/test"));

    let compose = r#"
services:
  worker:
    image: alpine
    volumes:
      - type: bind
        source: /var/run/docker.sock
        target: /var/run/docker.sock
"#;

    let error = executor
        .validate_compose_security_policy("compose file", compose)
        .unwrap_err();
    assert!(matches!(
        error,
        ComposeError::SecurityPolicyViolation { field, .. } if field == "volumes"
    ));
}

#[test]
fn test_validate_compose_security_policy_rejects_network_filter_bypasses() {
    let docker = Docker::connect_with_defaults().unwrap();
    let executor = ComposeExecutor::new(Arc::new(docker), PathBuf::from("/tmp/test"));

    for (field, network) in [
        ("networks.direct.driver", "driver: macvlan"),
        ("networks.direct.driver", "driver: ipvlan"),
        ("networks.direct.external", "external: true"),
    ] {
        let compose = format!(
                "services:\n  web:\n    image: alpine\n    networks: [direct]\nnetworks:\n  direct:\n    {network}\n"
            );
        let error = executor
            .validate_compose_security_policy("compose file", &compose)
            .expect_err("L2 and external networks must not bypass metadata filtering");
        assert!(matches!(
            error,
            ComposeError::SecurityPolicyViolation { field: actual, .. } if actual == field
        ));
    }
}

#[test]
fn test_validate_compose_security_policy_allows_managed_bridge_network() {
    let docker = Docker::connect_with_defaults().unwrap();
    let executor = ComposeExecutor::new(Arc::new(docker), PathBuf::from("/tmp/test"));
    let compose = r#"
services:
  web:
    image: alpine
    networks: [app]
networks:
  app:
    driver: bridge
"#;

    executor
        .validate_compose_security_policy("compose file", compose)
        .expect("managed bridge networks remain supported");
}

#[test]
fn test_validate_compose_security_policy_rejects_interpolated_bind_source() {
    let Some(executor) = test_executor() else {
        return;
    };
    let compose = r#"
services:
  pwn:
    image: alpine
    volumes:
      - "${HOST_ROOT:-/}:/host:rw"
"#;
    let err = executor
        .validate_compose_security_policy("compose file", compose)
        .unwrap_err();
    assert_eq!(violation_field(err), "volumes");
}

#[test]
fn test_validate_compose_security_policy_rejects_unsupported_long_form_mount_types() {
    let Some(executor) = test_executor() else {
        return;
    };

    for compose in [
            "services:\n  pwn:\n    image: alpine\n    volumes:\n      - type: image\n        source: temps.internal/victim\n        target: /stolen\n",
            "services:\n  pwn:\n    image: alpine\n    volumes:\n      - type: ${MOUNT_TYPE:-bind}\n        source: ./data\n        target: /data\n",
            "services:\n  pwn:\n    image: alpine\n    volumes:\n      - source: ./data\n        target: /data\n",
        ] {
            let err = executor
                .validate_compose_security_policy("compose file", compose)
                .unwrap_err();
            assert!(violation_field(err).starts_with("volumes"));
        }
}

#[test]
fn test_validate_compose_security_policy_rejects_home_relative_paths() {
    let Some(executor) = test_executor() else {
        return;
    };

    let cases = [
            (
                "volumes",
                "services:\n  pwn:\n    image: alpine\n    volumes:\n      - ~/.ssh:/stolen:ro\n",
            ),
            (
                "volumes",
                "services:\n  pwn:\n    image: alpine\n    volumes:\n      - type: bind\n        source: ~other/.ssh\n        target: /stolen\n",
            ),
            (
                "build.context",
                "services:\n  pwn:\n    build:\n      context: ~/.ssh\n",
            ),
            (
                "env_file",
                "services:\n  pwn:\n    image: alpine\n    env_file: ~/.env\n",
            ),
        ];

    for (expected_field, compose) in cases {
        let err = executor
            .validate_compose_security_policy("compose file", compose)
            .unwrap_err();
        assert_eq!(violation_field(err), expected_field);
    }

    for path in ["~", "~/audit", "~other/.ssh", "~\\secrets"] {
        assert!(ComposeExecutor::is_dangerous_host_path(path));
    }
}

#[test]
fn test_validate_compose_security_policy_rejects_extends() {
    let Some(executor) = test_executor() else {
        return;
    };
    let compose = r#"
services:
  app:
    image: alpine
    extends:
      file: malicious.yml
      service: privileged_base
"#;
    let err = executor
        .validate_compose_security_policy("compose file", compose)
        .unwrap_err();
    assert!(err.to_string().contains("Advanced security settings"));
    assert_eq!(violation_field(err), "extends");
}

#[test]
fn test_validate_compose_security_policy_rejects_use_api_socket() {
    let Some(executor) = test_executor() else {
        return;
    };
    let compose = r#"
services:
  app:
    image: alpine
    use_api_socket: true
"#;
    let err = executor
        .validate_compose_security_policy("compose file", compose)
        .unwrap_err();
    assert_eq!(violation_field(err), "use_api_socket");
}

#[test]
fn test_validate_compose_security_policy_rejects_relative_escape_bind() {
    let Some(executor) = test_executor() else {
        return;
    };
    let compose = r#"
services:
  pwn:
    image: alpine
    volumes:
      - ../../../../etc:/host:rw
"#;
    let err = executor
        .validate_compose_security_policy("compose file", compose)
        .unwrap_err();
    assert_eq!(violation_field(err), "volumes");

    // A relative path that stays inside the project dir is allowed.
    let ok = r#"
services:
  app:
    image: alpine
    volumes:
      - ./data:/data:rw
"#;
    assert!(executor
        .validate_compose_security_policy("compose file", ok)
        .is_ok());
}

#[test]
fn test_validate_compose_security_policy_rejects_privileged_build_options() {
    let Some(executor) = test_executor() else {
        return;
    };
    for compose in [
            "services:\n  app:\n    build:\n      context: .\n      privileged: true\n",
            "services:\n  app:\n    build:\n      context: .\n      network: host\n",
            "services:\n  app:\n    build:\n      context: .\n      entitlements:\n        - security.insecure\n",
        ] {
            let err = executor
                .validate_compose_security_policy("compose file", compose)
                .unwrap_err();
            assert!(violation_field(err).starts_with("build."));
        }
}

#[test]
fn test_validate_compose_security_policy_rejects_interpolated_build_security_options() {
    let Some(executor) = test_executor() else {
        return;
    };

    let cases = [
            (
                "build.privileged",
                "services:\n  app:\n    build:\n      context: .\n      privileged: ${BUILD_PRIVILEGED:-true}\n",
            ),
            (
                "build.network",
                "services:\n  app:\n    build:\n      context: .\n      network: ${BUILD_NETWORK:-host}\n",
            ),
            (
                "build.privileged",
                "services:\n  app:\n    build:\n      context: .\n      privileged: \"true\"\n",
            ),
            (
                "build.privileged",
                "services:\n  app:\n    build:\n      context: .\n      privileged: \"false\"\n",
            ),
        ];

    for (expected_field, compose) in cases {
        let err = executor
            .validate_compose_security_policy("compose file", compose)
            .unwrap_err();
        assert_eq!(violation_field(err), expected_field);
    }

    let safe = "services:\n  app:\n    build:\n      context: .\n      privileged: false\n      network: default\n";
    assert!(executor
        .validate_compose_security_policy("compose file", safe)
        .is_ok());
}

#[test]
fn test_validate_compose_security_policy_rejects_named_volume_driver_device() {
    let Some(executor) = test_executor() else {
        return;
    };
    let compose = r#"
services:
  pwn:
    image: alpine
    volumes:
      - hostroot:/host
volumes:
  hostroot:
    driver_opts:
      type: none
      o: bind
      device: /
"#;
    let err = executor
        .validate_compose_security_policy("compose file", compose)
        .unwrap_err();
    assert_eq!(violation_field(err), "volumes.hostroot.driver_opts");
}

#[test]
fn test_validate_compose_security_policy_isolates_daemon_global_resources() {
    let Some(executor) = test_executor() else {
        return;
    };
    for (expected_field, compose) in [
            (
                "volumes.data.external",
                "services:\n  app:\n    image: alpine\nvolumes:\n  data:\n    external: true\n",
            ),
            (
                "volumes.data.name",
                "services:\n  app:\n    image: alpine\nvolumes:\n  data:\n    name: another-project-data\n",
            ),
            (
                "networks.shared.external",
                "services:\n  app:\n    image: alpine\nnetworks:\n  shared:\n    external: \"true\"\n",
            ),
            (
                "networks.shared.name",
                "services:\n  app:\n    image: alpine\nnetworks:\n  shared:\n    name: another-project-network\n",
            ),
            (
                "networks.shared.ipam",
                "services:\n  app:\n    image: alpine\nnetworks:\n  shared:\n    ipam:\n      config:\n        - subnet: 172.16.0.0/12\n",
            ),
            (
                "configs.app_config.external",
                "services:\n  app:\n    image: alpine\nconfigs:\n  app_config:\n    external: true\n",
            ),
            (
                "secrets.api_key.name",
                "services:\n  app:\n    image: alpine\nsecrets:\n  api_key:\n    name: another-project-secret\n",
            ),
            (
                "external_links",
                "services:\n  app:\n    image: alpine\n    external_links:\n      - victim-db:db\n",
            ),
            (
                "label_file",
                "services:\n  app:\n    image: alpine\n    label_file: /etc/passwd\n",
            ),
            (
                "post_start",
                "services:\n  app:\n    image: alpine\n    post_start:\n      - command: id\n        privileged: true\n",
            ),
            (
                "provider",
                "services:\n  app:\n    provider:\n      type: attacker\n",
            ),
            (
                "container_name",
                "services:\n  app:\n    image: alpine\n    container_name: victim\n",
            ),
        ] {
            let err = executor
                .validate_compose_security_policy("compose file", compose)
                .unwrap_err();
            assert_eq!(violation_field(err), expected_field);
        }

    let safe = "services:\n  app:\n    image: alpine\nvolumes:\n  data:\n    external: false\nnetworks:\n  private:\n    driver: bridge\n    external: false\n";
    assert!(executor
        .validate_compose_security_policy("compose file", safe)
        .is_ok());
}

#[test]
fn test_validate_compose_security_policy_rejects_unbounded_build_inputs() {
    let Some(executor) = test_executor() else {
        return;
    };
    for field in ["additional_contexts", "cache_from", "cache_to", "tags"] {
        let compose =
            format!("services:\n  app:\n    build:\n      context: .\n      {field}: []\n");
        let err = executor
            .validate_compose_security_policy("compose file", &compose)
            .unwrap_err();
        assert_eq!(violation_field(err), format!("build.{field}"));
    }
}

#[test]
fn test_validate_compose_security_policy_limits_networking_and_replicas() {
    let Some(executor) = test_executor() else {
        return;
    };
    for (expected_field, compose) in [
        (
            "network_mode",
            "services:\n  app:\n    image: alpine\n    network_mode: bridge\n",
        ),
        (
            "network_mode",
            "services:\n  app:\n    image: alpine\n    network_mode: service:missing\n",
        ),
        (
            "scale",
            "services:\n  app:\n    image: alpine\n    scale: 2\n",
        ),
        (
            "deploy.replicas",
            "services:\n  app:\n    image: alpine\n    deploy:\n      replicas: ${REPLICAS:-100}\n",
        ),
        (
            "deploy.mode",
            "services:\n  app:\n    image: alpine\n    deploy:\n      mode: global\n",
        ),
    ] {
        let err = executor
            .validate_compose_security_policy("compose file", compose)
            .unwrap_err();
        assert_eq!(violation_field(err), expected_field);
    }

    let safe = "services:\n  app:\n    image: alpine\n    network_mode: service:sidecar\n    scale: 1\n    deploy:\n      mode: replicated\n      replicas: 1\n  sidecar:\n    image: alpine\n    network_mode: none\n";
    assert!(executor
        .validate_compose_security_policy("compose file", safe)
        .is_ok());
}

#[test]
fn test_validate_compose_security_policy_isolates_image_selection() {
    let Some(executor) = test_executor() else {
        return;
    };
    for (expected_field, compose) in [
            (
                "image",
                "services:\n  app:\n    image: temps.internal/victim:latest\n",
            ),
            (
                "image",
                "services:\n  app:\n    image: sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
            ),
            (
                "image",
                "services:\n  app:\n    image: alpine\n    build: .\n",
            ),
            (
                "pull_policy",
                "services:\n  app:\n    image: alpine\n    pull_policy: never\n",
            ),
        ] {
            let err = executor
                .validate_compose_security_policy("compose file", compose)
                .unwrap_err();
            assert_eq!(violation_field(err), expected_field);
        }
    assert!(executor
            .validate_compose_security_policy(
                "compose file",
                "services:\n  app:\n    image: registry.example/app@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
            )
            .is_ok());
}

#[test]
fn test_image_rejection_explains_each_cause_without_echoing_resolved_values() {
    let Some(executor) = test_executor() else {
        return;
    };
    // (compose, fragments the message must contain, fragment it must not)
    for (compose, expected, absent) in [
            (
                "services:\n  app:\n    image: ${IMAGE}\n    build: .\n",
                ["'${IMAGE}'", "Remove image:"],
                None,
            ),
            (
                "services:\n  app:\n    image: ghcr.io/org/app:${TAG:-latest}\n",
                [
                    "'ghcr.io/org/app:${TAG:-latest}'",
                    "still contains a variable",
                ],
                None,
            ),
            (
                "services:\n  app:\n    image: temps.internal/victim:latest\n",
                ["Temps-internal", "registry reference"],
                Some("victim"),
            ),
            (
                "services:\n  app:\n    image: sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
                ["raw image ID", "registry reference"],
                Some("aaaaaaaa"),
            ),
            // With the interpolation check relaxed this runs on the resolved
            // model, where the literal may be a project variable's value.
            (
                "services:\n  app:\n    image: resolved-private-value\n    build: .\n",
                ["Remove image:", "service 'app'"],
                Some("resolved-private-value"),
            ),
        ] {
            let message = executor
                .validate_compose_security_policy("compose file", compose)
                .unwrap_err()
                .to_string();
            for fragment in expected {
                assert!(
                    message.contains(fragment),
                    "rejection of {compose:?} should mention {fragment:?}: {message}"
                );
            }
            if let Some(absent) = absent {
                assert!(
                    !message.contains(absent),
                    "rejection of {compose:?} must not echo {absent:?}: {message}"
                );
            }
        }
}

#[test]
fn test_validate_compose_security_policy_confines_env_files_and_ports() {
    let Some(executor) = test_executor() else {
        return;
    };
    for (expected_field, compose) in [
            (
                "env_file",
                "services:\n  app:\n    image: alpine\n    env_file: /etc/passwd\n",
            ),
            (
                "env_file",
                "services:\n  app:\n    image: alpine\n    env_file: ${HOST_ENV:-/etc/passwd}\n",
            ),
            (
                "ports",
                "services:\n  app:\n    image: alpine\n    ports: [\"8080:80\"]\n",
            ),
            (
                "ports",
                "services:\n  app:\n    image: alpine\n    ports:\n      - target: 80\n        published: 8080\n        host_ip: 0.0.0.0\n",
            ),
        ] {
            let err = executor
                .validate_compose_security_policy("compose file", compose)
                .unwrap_err();
            assert_eq!(violation_field(err), expected_field);
        }

    let safe = "services:\n  app:\n    image: alpine\n    env_file: ./app.env\n    ports:\n      - \"127.0.0.1:8080:80\"\n      - target: 443\n        published: 8443\n        host_ip: 127.0.0.1\n";
    assert!(executor
        .validate_compose_security_policy("compose file", safe)
        .is_ok());
}

#[test]
fn test_validate_compose_security_policy_rejects_configs_and_secrets_files() {
    let Some(executor) = test_executor() else {
        return;
    };
    let configs = r#"
services:
  app:
    image: alpine
configs:
  hostfile:
    file: /etc/passwd
"#;
    assert_eq!(
        violation_field(
            executor
                .validate_compose_security_policy("compose file", configs)
                .unwrap_err()
        ),
        "configs.file"
    );

    let secrets = r#"
services:
  app:
    image: alpine
secrets:
  hostsecret:
    file: ../../../../etc/shadow
"#;
    assert_eq!(
        violation_field(
            executor
                .validate_compose_security_policy("compose file", secrets)
                .unwrap_err()
        ),
        "secrets.file"
    );
}

#[test]
fn test_validate_compose_security_policy_rejects_remaining_host_namespaces() {
    let Some(executor) = test_executor() else {
        return;
    };
    for (field, compose) in [
        (
            "cgroup",
            "services:\n  app:\n    image: alpine\n    cgroup: host\n",
        ),
        (
            "userns_mode",
            "services:\n  app:\n    image: alpine\n    userns_mode: \"host\"\n",
        ),
        (
            "uts",
            "services:\n  app:\n    image: alpine\n    uts: \"host\"\n",
        ),
    ] {
        let err = executor
            .validate_compose_security_policy("compose file", compose)
            .unwrap_err();
        assert_eq!(violation_field(err), field);
    }
}

#[test]
fn test_validate_compose_security_policy_rejects_gpus() {
    let Some(executor) = test_executor() else {
        return;
    };
    let compose = "services:\n  app:\n    image: alpine\n    gpus: all\n";
    let err = executor
        .validate_compose_security_policy("compose file", compose)
        .unwrap_err();
    assert_eq!(violation_field(err), "gpus");
}

#[test]
fn test_validate_compose_security_policy_resolves_merge_keys() {
    let Some(executor) = test_executor() else {
        return;
    };
    // The privileged setting is inherited via a `<<` merge key from an anchor.
    let compose = r#"
x-base: &base
  privileged: true
services:
  app:
    image: alpine
    <<: *base
"#;
    let err = executor
        .validate_compose_security_policy("compose file", compose)
        .unwrap_err();
    assert_eq!(violation_field(err), "privileged");
}

#[test]
fn test_lexically_normalize() {
    assert_eq!(
        ComposeExecutor::lexically_normalize("../../../../etc"),
        "../../../../etc"
    );
    assert_eq!(ComposeExecutor::lexically_normalize("/tmp/../etc"), "/etc");
    assert_eq!(ComposeExecutor::lexically_normalize("./data"), "data");
    assert_eq!(ComposeExecutor::lexically_normalize("/"), "/");
    assert!(ComposeExecutor::is_dangerous_host_path("/tmp/../etc"));
    assert!(ComposeExecutor::is_dangerous_host_path("../escape"));
    assert!(!ComposeExecutor::is_dangerous_host_path("./data"));
    // All absolute host paths are rejected — including /tmp, which is
    // world-writable and can hold other tenants' project artifacts.
    assert!(ComposeExecutor::is_dangerous_host_path("/tmp/ok"));
    assert!(ComposeExecutor::is_dangerous_host_path(
        "/tmp/test/compose/victim"
    ));
    assert!(ComposeExecutor::is_dangerous_host_path("/etc/passwd"));
    assert!(ComposeExecutor::is_dangerous_host_path("/"));
}

#[test]
fn test_validate_compose_security_policy_rejects_interpolation_bypass() {
    let Some(executor) = test_executor() else {
        return;
    };

    // network_mode via env default would resolve to `host` at runtime but
    // the literal value is `${NET_MODE:-host}`, bypassing the `host` check.
    let net = "services:\n  web:\n    image: alpine\n    network_mode: ${NET_MODE:-host}\n";
    let err = executor
        .validate_compose_security_policy("compose file", net)
        .unwrap_err();
    assert_eq!(violation_field(err), "network_mode");

    // privileged via env default bypasses the `as_bool()` check.
    let priv_compose = "services:\n  web:\n    image: alpine\n    privileged: ${P:-true}\n";
    let err = executor
        .validate_compose_security_policy("compose file", priv_compose)
        .unwrap_err();
    assert_eq!(violation_field(err), "privileged");

    // Compose coerces this quoted string to a true boolean. Static policy
    // validation must therefore reject non-boolean values rather than
    // treating them as a harmless false value.
    let quoted_privileged = "services:\n  web:\n    image: alpine\n    privileged: \"true\"\n";
    let err = executor
        .validate_compose_security_policy("compose file", quoted_privileged)
        .unwrap_err();
    assert_eq!(violation_field(err), "privileged");

    // $(...) command-substitution form inside a guarded sequence field.
    let grp = "services:\n  web:\n    image: alpine\n    group_add:\n      - $(id -g docker)\n";
    let err = executor
        .validate_compose_security_policy("compose file", grp)
        .unwrap_err();
    assert_eq!(violation_field(err), "group_add");

    // userns_mode via interpolation.
    let userns = "services:\n  web:\n    image: alpine\n    userns_mode: ${U:-host}\n";
    let err = executor
        .validate_compose_security_policy("compose file", userns)
        .unwrap_err();
    assert_eq!(violation_field(err), "userns_mode");
}

#[test]
fn interpolation_in_volumes_names_the_security_check() {
    let Some(executor) = test_executor() else {
        return;
    };

    let compose =
        "services:\n  trawl:\n    image: alpine\n    volumes:\n      - ${HOST_PATH:-/tmp}:/data\n";
    let error = executor
        .validate_compose_security_policy("compose file", compose)
        .expect_err("interpolated volume must be rejected");
    let message = error.to_string();
    let interpolation_label = PolicyCheck::catalog()
        .into_iter()
        .find(|definition| definition.id == PolicyCheck::Interpolation)
        .expect("interpolation check is in the settings catalog")
        .label;
    assert!(
        message.contains("rejected volumes for service 'trawl'"),
        "{message}"
    );
    assert!(
        message.contains(&format!("'{interpolation_label}' security check")),
        "{message}"
    );
    assert!(message.contains("Advanced security settings"), "{message}");
}

#[test]
fn test_validate_compose_security_policy_rejects_volumes_from() {
    let Some(executor) = test_executor() else {
        return;
    };

    // `volumes_from: container:X` inherits every volume of an arbitrary host
    // container (other tenants', Temps infra) — a full host-escape vector.
    let container_form =
            "services:\n  pwn:\n    image: alpine\n    volumes_from:\n      - container:temps-db-1a2b3c\n";
    let err = executor
        .validate_compose_security_policy("compose file", container_form)
        .unwrap_err();
    assert_eq!(violation_field(err), "volumes_from");

    // The `service:X` intra-project form is blocked too — the field is
    // rejected outright rather than trying to distinguish safe targets.
    let service_form =
        "services:\n  pwn:\n    image: alpine\n    volumes_from:\n      - service:web\n";
    let err = executor
        .validate_compose_security_policy("compose file", service_form)
        .unwrap_err();
    assert_eq!(violation_field(err), "volumes_from");

    // A benign service with no volumes_from still validates.
    let clean = "services:\n  web:\n    image: alpine\n    volumes:\n      - ./data:/data\n";
    assert!(executor
        .validate_compose_security_policy("compose file", clean)
        .is_ok());
}

#[test]
fn test_validate_compose_security_policy_rejects_absolute_tmp_bind() {
    let Some(executor) = test_executor() else {
        return;
    };

    // Absolute host bind sources are rejected even under /tmp, which is
    // world-writable and can hold another project's data-dir artifacts.
    let tmp_bind =
            "services:\n  pwn:\n    image: alpine\n    volumes:\n      - /tmp/test/compose/victim:/stolen:ro\n";
    let err = executor
        .validate_compose_security_policy("compose file", tmp_bind)
        .unwrap_err();
    assert_eq!(violation_field(err), "volumes");
}

#[test]
fn test_validate_compose_security_policy_rejects_host_control_keys() {
    let Some(executor) = test_executor() else {
        return;
    };

    // Each host-affecting key must be rejected in isolation (not only when
    // it appears alongside another violation that short-circuits first).
    let cases = [
        (
            "sysctls",
            "services:\n  a:\n    image: alpine\n    sysctls:\n      net.ipv4.ip_forward: \"1\"\n",
        ),
        (
            "group_add",
            "services:\n  a:\n    image: alpine\n    group_add:\n      - docker\n",
        ),
        (
            "cgroup_parent",
            "services:\n  a:\n    image: alpine\n    cgroup_parent: /custom.slice\n",
        ),
    ];
    for (field, yaml) in cases {
        let err = executor
            .validate_compose_security_policy("compose file", yaml)
            .unwrap_err();
        assert_eq!(
            violation_field(err),
            field,
            "expected {field} to be rejected"
        );
    }
}

#[test]
fn test_validate_compose_security_policy_rejects_deploy_devices() {
    let Some(executor) = test_executor() else {
        return;
    };

    // Long-form device reservation — the equivalent of the blocked `gpus:`.
    let yaml = "services:\n  gpu:\n    image: alpine\n    deploy:\n      resources:\n        reservations:\n          devices:\n            - driver: nvidia\n              count: all\n              capabilities: [gpu]\n";
    let err = executor
        .validate_compose_security_policy("compose file", yaml)
        .unwrap_err();
    assert_eq!(
        violation_field(err),
        "deploy.resources.reservations.devices"
    );

    // A benign deploy block (replicas only) is still allowed.
    let benign = "services:\n  web:\n    image: alpine\n    deploy:\n      replicas: 1\n";
    assert!(executor
        .validate_compose_security_policy("compose file", benign)
        .is_ok());
}

#[test]
fn test_validate_compose_security_policy_rejects_build_context_and_ssh() {
    let Some(executor) = test_executor() else {
        return;
    };

    // Absolute build context would send an arbitrary host dir into the build.
    let ctx = "services:\n  app:\n    build:\n      context: /etc\n";
    let err = executor
        .validate_compose_security_policy("compose file", ctx)
        .unwrap_err();
    assert_eq!(violation_field(err), "build.context");

    // Project-escaping dockerfile path.
    let dockerfile =
        "services:\n  app:\n    build:\n      context: .\n      dockerfile: ../../../etc/x\n";
    let err = executor
        .validate_compose_security_policy("compose file", dockerfile)
        .unwrap_err();
    assert_eq!(violation_field(err), "build.dockerfile");

    // SSH agent forwarding during build.
    let ssh = "services:\n  app:\n    build:\n      context: .\n      ssh:\n        - default\n";
    let err = executor
        .validate_compose_security_policy("compose file", ssh)
        .unwrap_err();
    assert_eq!(violation_field(err), "build.ssh");

    for remote_context in [
        "https://example.test/source.tar.gz",
        "http://127.0.0.1/internal",
        "git://example.test/repository.git",
        "git@example.test:repository.git",
    ] {
        let compose = format!("services:\n  app:\n    build:\n      context: {remote_context}\n");
        let err = executor
            .validate_compose_security_policy("compose file", &compose)
            .unwrap_err();
        assert_eq!(violation_field(err), "build.context", "{remote_context}");
    }

    // A confined relative build context is accepted.
    let ok = "services:\n  app:\n    build:\n      context: ./app\n      dockerfile: Dockerfile\n";
    assert!(executor
        .validate_compose_security_policy("compose file", ok)
        .is_ok());
}

#[test]
fn test_validate_compose_security_policy_rejects_network_volume_driver() {
    let Some(executor) = test_executor() else {
        return;
    };

    // Non-local driver invokes an external volume plugin.
    let plugin = "services:\n  app:\n    image: alpine\n    volumes:\n      - vol:/data\nvolumes:\n  vol:\n    driver: some-plugin\n";
    let err = executor
        .validate_compose_security_policy("compose file", plugin)
        .unwrap_err();
    assert_eq!(violation_field(err), "volumes");

    // Local driver with an NFS type mounts an off-host filesystem.
    let nfs = "services:\n  app:\n    image: alpine\n    volumes:\n      - vol:/data\nvolumes:\n  vol:\n    driver: local\n    driver_opts:\n      type: nfs\n      o: addr=attacker.example.com,rw\n      device: \":/exports/root\"\n";
    let err = executor
        .validate_compose_security_policy("compose file", nfs)
        .unwrap_err();
    assert_eq!(violation_field(err), "volumes.vol.driver_opts");

    // A plain local named volume is fine.
    let ok = "services:\n  app:\n    image: alpine\n    volumes:\n      - vol:/data\nvolumes:\n  vol: {}\n";
    assert!(executor
        .validate_compose_security_policy("compose file", ok)
        .is_ok());
}

#[test]
fn test_validate_compose_security_policy_rejects_unsafe_service_names() {
    let Some(executor) = test_executor() else {
        return;
    };

    // A service name carrying newlines/YAML structure would corrupt the
    // generated security override; reject it up front.
    let yaml =
        "services:\n  ? \"evil:\\n  cap_drop:\\n  - ALL\\n  legit\"\n  :\n    image: alpine\n";
    let err = executor
        .validate_compose_security_policy("compose file", yaml)
        .unwrap_err();
    assert_eq!(violation_field(err), "services");

    // Normal names pass the character-set check.
    assert!(ComposeExecutor::is_valid_service_name("web-1.api_v2"));
    assert!(!ComposeExecutor::is_valid_service_name("evil:\n  x"));
    assert!(!ComposeExecutor::is_valid_service_name("-leading-dash"));
    assert!(!ComposeExecutor::is_valid_service_name(""));
}

#[test]
fn test_contains_interpolation_covers_braceless_and_escapes() {
    // Braced, command-substitution, and braceless forms are all caught.
    assert!(ComposeExecutor::contains_interpolation("${VAR}"));
    assert!(ComposeExecutor::contains_interpolation("$(id -g docker)"));
    assert!(ComposeExecutor::contains_interpolation("$VAR"));
    assert!(ComposeExecutor::contains_interpolation(
        "prefix-$HOST_ROOT/x"
    ));
    assert!(ComposeExecutor::contains_interpolation("${NET:-host}"));
    // `$$` escapes a literal dollar, and a bare/trailing `$` is not a var.
    assert!(!ComposeExecutor::contains_interpolation("$$HOME"));
    assert!(!ComposeExecutor::contains_interpolation("no dollars here"));
    assert!(!ComposeExecutor::contains_interpolation("trailing$"));
    assert!(!ComposeExecutor::contains_interpolation("cost is $ 5"));
}

#[test]
fn test_validate_compose_security_policy_rejects_braceless_interpolation() {
    let Some(executor) = test_executor() else {
        return;
    };
    // Braceless $VAR in network_mode would resolve to `host` from a
    // repo-controlled .env at runtime, bypassing the literal `host` check.
    let net = "services:\n  web:\n    image: alpine\n    network_mode: $NET\n";
    assert_eq!(
        violation_field(
            executor
                .validate_compose_security_policy("compose file", net)
                .unwrap_err()
        ),
        "network_mode"
    );

    // Braceless $SRC in a bind mount source.
    let vol = "services:\n  web:\n    image: alpine\n    volumes:\n      - $SRC:/host:rw\n";
    assert_eq!(
        violation_field(
            executor
                .validate_compose_security_policy("compose file", vol)
                .unwrap_err()
        ),
        "volumes"
    );
}

#[test]
fn test_validate_compose_security_policy_rejects_top_level_include() {
    let Some(executor) = test_executor() else {
        return;
    };
    // `include` merges repo-controlled compose files that never flow through
    // this validator.
    let compose = "include:\n  - ./evil.yml\nservices:\n  web:\n    image: nginx\n";
    assert_eq!(
        violation_field(
            executor
                .validate_compose_security_policy("compose file", compose)
                .unwrap_err()
        ),
        "include"
    );
}

#[test]
fn test_validate_compose_security_policy_rejects_container_namespace() {
    let Some(executor) = test_executor() else {
        return;
    };
    for (field, compose) in [
        (
            "network_mode",
            "services:\n  web:\n    image: alpine\n    network_mode: \"container:other\"\n",
        ),
        (
            "pid",
            "services:\n  web:\n    image: alpine\n    pid: \"container:other\"\n",
        ),
    ] {
        assert_eq!(
            violation_field(
                executor
                    .validate_compose_security_policy("compose file", compose)
                    .unwrap_err()
            ),
            field
        );
    }

    // Intra-project `service:` sharing stays within the deployment and is
    // allowed.
    let ok = "services:\n  web:\n    image: alpine\n    network_mode: \"service:db\"\n  db:\n    image: postgres\n";
    assert!(executor
        .validate_compose_security_policy("compose file", ok)
        .is_ok());
}

#[test]
fn test_validate_compose_security_policy_rejects_non_string_service_names() {
    let Some(executor) = test_executor() else {
        return;
    };
    // A bare boolean/null/numeric key is a non-string scalar that would be
    // dropped by the service-name enumerator and skip the security override.
    for compose in [
        "services:\n  true:\n    image: alpine\n",
        "services:\n  null:\n    image: alpine\n",
        "services:\n  8080:\n    image: alpine\n",
    ] {
        assert_eq!(
            violation_field(
                executor
                    .validate_compose_security_policy("compose file", compose)
                    .unwrap_err()
            ),
            "services"
        );
    }

    // A normal quoted/bareword string service name is still accepted.
    let ok = "services:\n  web:\n    image: nginx\n";
    assert!(executor
        .validate_compose_security_policy("compose file", ok)
        .is_ok());
}

#[test]
fn test_validate_compose_security_policy_rejects_runtime_and_oom_disable() {
    let Some(executor) = test_executor() else {
        return;
    };
    for (field, yaml) in [
        (
            "runtime",
            "services:\n  app:\n    image: alpine\n    runtime: kata-runtime\n",
        ),
        (
            "oom_kill_disable",
            "services:\n  app:\n    image: alpine\n    oom_kill_disable: true\n",
        ),
    ] {
        let err = executor
            .validate_compose_security_policy("compose file", yaml)
            .unwrap_err();
        assert_eq!(violation_field(err), field);
    }
}

#[test]
fn test_validate_compose_security_policy_rejects_unbounded_resource_controls() {
    let Some(executor) = test_executor() else {
        return;
    };
    for (field, yaml) in [
            (
                "tmpfs",
                "services:\n  app:\n    image: alpine\n    tmpfs:\n      - /run\n",
            ),
            (
                "volumes.type",
                "services:\n  app:\n    image: alpine\n    volumes:\n      - type: tmpfs\n        target: /run\n",
            ),
            (
                "ulimits",
                "services:\n  app:\n    image: alpine\n    ulimits:\n      nofile: 1024\n",
            ),
            (
                "build.shm_size",
                "services:\n  app:\n    build:\n      context: .\n      shm_size: 64m\n",
            ),
        ] {
            let err = executor
                .validate_compose_security_policy("compose file", yaml)
                .unwrap_err();
            assert_eq!(violation_field(err), field, "expected rejection for {yaml}");
        }
}

#[test]
fn test_validate_compose_security_policy_accepts_bounded_shm_sizes() {
    let Some(executor) = test_executor() else {
        return;
    };
    for yaml in [
        "services:\n  immich:\n    image: alpine\n    shm_size: 128mb\n",
        "services:\n  gitlab:\n    image: alpine\n    shm_size: 256m\n",
        "services:\n  app:\n    image: alpine\n    shm_size: 536870912\n",
    ] {
        executor
            .validate_compose_security_policy("compose file", yaml)
            .unwrap_or_else(|error| panic!("expected bounded shm_size to pass: {error}"));
    }
}

#[test]
fn test_validate_compose_security_policy_rejects_oversized_or_invalid_shm_sizes() {
    let executor = disabled_executor(PathBuf::from("/tmp/test"));
    for (yaml, expected_check) in [
        (
            "services:\n  app:\n    image: alpine\n    shm_size: 513m\n",
            Some("Limit service shared memory"),
        ),
        (
            "services:\n  app:\n    image: alpine\n    shm_size: unlimited\n",
            None,
        ),
        (
            "services:\n  app:\n    image: alpine\n    shm_size: 0\n",
            None,
        ),
    ] {
        let err = executor
            .validate_compose_security_policy("compose file", yaml)
            .unwrap_err();
        let message = err.to_string();
        assert!(
                message.contains("Project Settings → Build & deploy → Build → Docker Compose → Advanced security settings"),
                "{message}"
            );
        match expected_check {
            Some(check) => assert!(message.contains(check), "{message}"),
            None => assert!(
                !message.contains("Limit service shared memory"),
                "{message}"
            ),
        }
        assert_eq!(violation_field(err), "shm_size");
    }
}

#[test]
fn test_validate_compose_security_policy_caps_aggregate_shm_size() {
    let executor = disabled_executor(PathBuf::from("/tmp/test"));
    let yaml = "services:\n  first:\n    image: alpine\n    shm_size: 400m\n  second:\n    image: alpine\n    shm_size: 400m\n  third:\n    image: alpine\n    shm_size: 400m\n";

    let err = executor
        .validate_compose_security_policy("compose file", yaml)
        .unwrap_err();
    let message = err.to_string();

    assert_eq!(violation_field(err), "shm_size");
    assert!(message.contains("'Limit aggregate shared memory' (Resources)"));
    assert!(message.contains("lower the stack total or disable that check"));
}

#[test]
fn test_validate_compose_override_expands_merge_keys() {
    let compose = "services:\n  web:\n    image: nginx\n";
    let override_content = r#"
services:
  web:
    x-runtime: &runtime
      runtime: kata-runtime
    <<: *runtime
"#;
    let error = ComposeExecutor::validate_compose_override("temps-test", compose, override_content)
        .unwrap_err();
    assert!(error.to_string().contains("'runtime'"), "{error}");
    assert!(error.to_string().contains("Advanced security settings"));
}

#[test]
fn policy_exceptions_are_independent_for_service_fields() {
    let cases = [
        (PolicyCheck::Privileged, "privileged: true"),
        (PolicyCheck::DockerSocket, "use_api_socket: true"),
        (PolicyCheck::Capabilities, "cap_add: [NET_ADMIN]"),
        (PolicyCheck::Devices, "devices: [/dev/fuse:/dev/fuse]"),
        (
            PolicyCheck::DeviceRules,
            "device_cgroup_rules: ['c 1:3 rwm']",
        ),
        (
            PolicyCheck::SecurityOptions,
            "security_opt: [seccomp:unconfined]",
        ),
        (PolicyCheck::Gpu, "gpus: all"),
        (PolicyCheck::Sysctls, "sysctls: {net.ipv4.ip_forward: '1'}"),
        (PolicyCheck::Groups, "group_add: ['44']"),
        (PolicyCheck::CgroupParent, "cgroup_parent: custom.slice"),
        (PolicyCheck::Runtime, "runtime: nvidia"),
        (
            PolicyCheck::LifecycleHooks,
            "post_start: [{command: echo ready}]",
        ),
        (PolicyCheck::Provider, "provider: {type: custom}"),
        (PolicyCheck::ContainerName, "container_name: custom-name"),
        (PolicyCheck::HostNetwork, "network_mode: host"),
        (PolicyCheck::HostPid, "pid: host"),
        (PolicyCheck::HostIpc, "ipc: host"),
        (PolicyCheck::HostUts, "uts: host"),
        (PolicyCheck::HostCgroup, "cgroup: host"),
        (PolicyCheck::HostUser, "userns_mode: host"),
        (
            PolicyCheck::ContainerNamespace,
            "network_mode: container:other",
        ),
        (PolicyCheck::NetworkMode, "network_mode: bridge"),
        (PolicyCheck::ExternalLinks, "external_links: [other]"),
        (PolicyCheck::PublishedPorts, "ports: ['8080:80']"),
        (PolicyCheck::BindMounts, "volumes: ['/srv/app:/data']"),
        (
            PolicyCheck::DockerSocket,
            "volumes: ['/var/run/docker.sock:/var/run/docker.sock']",
        ),
        (PolicyCheck::VolumesFrom, "volumes_from: [other]"),
        (PolicyCheck::EnvFiles, "env_file: /srv/app.env"),
        (PolicyCheck::LabelFiles, "label_file: labels.txt"),
        (PolicyCheck::StorageOptions, "storage_opt: {size: 20G}"),
        (PolicyCheck::OomKiller, "oom_kill_disable: true"),
        (PolicyCheck::ServiceShm, "shm_size: 768m"),
        (PolicyCheck::Tmpfs, "tmpfs: [/tmp]"),
        (PolicyCheck::Tmpfs, "volumes: [{type: tmpfs, target: /tmp}]"),
        (PolicyCheck::Ulimits, "ulimits: {nofile: 65536}"),
        (PolicyCheck::Blkio, "blkio_config: {weight: 500}"),
        (PolicyCheck::Swap, "memswap_limit: 2g"),
        (PolicyCheck::Replicas, "scale: 2"),
        (PolicyCheck::PullPolicy, "pull_policy: never"),
    ];
    for (check, field) in cases {
        let compose = format!("services:\n  app:\n    image: nginx:alpine\n    {field}\n");
        assert!(
            executor_with_checks_disabled(&[])
                .validate_compose_security_policy("test", &compose)
                .is_err(),
            "default must reject {field}"
        );
        let executor = executor_with_checks_disabled(&[check]);
        assert!(
            executor
                .validate_compose_security_policy("test", &compose)
                .is_ok(),
            "exception {check:?} did not permit {field}"
        );
        let other = if check == PolicyCheck::Privileged {
            "    network_mode: host\n"
        } else {
            "    privileged: true\n"
        };
        assert!(
            executor
                .validate_compose_security_policy("test", &(compose + other))
                .is_err(),
            "{check:?} waived an unrelated check"
        );
    }
}

#[test]
fn policy_exceptions_apply_to_build_and_top_level_checks() {
    let cases = [
        (PolicyCheck::BuildPrivileged, "privileged: true"),
        (
            PolicyCheck::BuildEntitlements,
            "entitlements: [security.insecure]",
        ),
        (PolicyCheck::BuildNetwork, "network: host"),
        (PolicyCheck::BuildSsh, "ssh: [default]"),
        (PolicyCheck::BuildShm, "shm_size: 2g"),
        (PolicyCheck::BuildUlimits, "ulimits: {nofile: 65536}"),
        (
            PolicyCheck::BuildAdditionalContexts,
            "additional_contexts: {assets: /srv/assets}",
        ),
        (
            PolicyCheck::BuildCacheFrom,
            "cache_from: [type=local,src=/tmp/cache]",
        ),
        (
            PolicyCheck::BuildCacheTo,
            "cache_to: [type=local,dest=/tmp/cache]",
        ),
        (PolicyCheck::BuildTags, "tags: [custom:latest]"),
        (PolicyCheck::BuildContext, "context: /srv/app"),
        (PolicyCheck::Dockerfile, "dockerfile: /srv/Dockerfile"),
    ];
    for (check, field) in cases {
        let compose = format!("services:\n  app:\n    build:\n      {field}\n");
        assert!(
            executor_with_checks_disabled(&[])
                .validate_compose_security_policy("test", &compose)
                .is_err(),
            "default accepted {field}"
        );
        assert!(
            executor_with_checks_disabled(&[check])
                .validate_compose_security_policy("test", &compose)
                .is_ok(),
            "exception {check:?} did not permit {field}"
        );
    }
    for (check, definition) in [
        (
            PolicyCheck::ExternalNetworks,
            "networks: {shared: {external: true}}",
        ),
        (
            PolicyCheck::NetworkNames,
            "networks: {shared: {name: shared}}",
        ),
        (
            PolicyCheck::NetworkDrivers,
            "networks: {shared: {driver: macvlan}}",
        ),
        (
            PolicyCheck::NetworkOptions,
            "networks: {shared: {driver_opts: {parent: eth0}}}",
        ),
        (
            PolicyCheck::NetworkIpam,
            "networks: {shared: {ipam: {driver: default}}}",
        ),
        (
            PolicyCheck::ExternalVolumes,
            "volumes: {data: {external: true}}",
        ),
        (
            PolicyCheck::VolumeNames,
            "volumes: {data: {name: shared-data}}",
        ),
        (
            PolicyCheck::VolumeHostPaths,
            "volumes: {data: {driver_opts: {type: none, o: bind, device: /srv/data}}}",
        ),
        (
            PolicyCheck::VolumeNetworkFilesystems,
            "volumes: {data: {driver_opts: {type: nfs, device: 'server:/data'}}}",
        ),
        (
            PolicyCheck::VolumeOptions,
            "volumes: {data: {driver_opts: {custom: value}}}",
        ),
        (
            PolicyCheck::ConfigPaths,
            "configs: {config: {file: /etc/config}}",
        ),
        (
            PolicyCheck::SecretPaths,
            "secrets: {secret: {file: /etc/secret}}",
        ),
        (
            PolicyCheck::ExternalConfigs,
            "configs: {config: {external: true}}",
        ),
        (
            PolicyCheck::ExternalSecrets,
            "secrets: {secret: {external: true}}",
        ),
    ] {
        let compose = format!("services: {{app: {{image: 'nginx:alpine'}}}}\n{definition}\n");
        assert!(
            executor_with_checks_disabled(&[])
                .validate_compose_security_policy("test", &compose)
                .is_err(),
            "default accepted {definition}"
        );
        assert!(
            executor_with_checks_disabled(&[check])
                .validate_compose_security_policy("test", &compose)
                .is_ok(),
            "exception {check:?} did not permit {definition}"
        );
    }
}

#[test]
fn source_policy_defers_runtime_values_but_keeps_read_guards() {
    let executor = executor_with_checks_disabled(&[PolicyCheck::Extends]);
    for value in ["1gb", "!reset null"] {
        assert!(executor
            .validate_source_security(&format!(
                "services: {{app: {{image: alpine, shm_size: {value}}}}}"
            ))
            .is_ok());
    }
    assert!(executor
        .validate_source_security("services: {app: {label_file: /etc/secret}}")
        .is_err());
    assert!(executor
        .validate_source_security("services: {app: {shm_size: !override '${SIZE}'}}")
        .is_err());
    assert!(executor
        .validate_source_security("include: [other.yml]")
        .is_err());
}

#[test]
fn identical_extends_is_removed_from_inline_override_only() {
    let source = "services: {app: {extends: {file: common.yml, service: base}}}";
    let same = "services: {app: {extends: {file: common.yml, service: base}, ports: ['127.0.0.1:8191:8191']}}";
    let normalized = ComposeExecutor::without_duplicate_extends("test", source, same).unwrap();
    let yaml: Value = yaml_serde::from_str(&normalized).unwrap();
    assert!(yaml["services"]["app"].get("extends").is_none());
    assert_eq!(
        yaml["services"]["app"]["ports"][0].as_str(),
        Some("127.0.0.1:8191:8191")
    );

    let different = "services: {app: {extends: {file: other.yml, service: base}}}";
    assert_eq!(
        ComposeExecutor::without_duplicate_extends("test", source, different).unwrap(),
        different
    );
}

#[test]
fn port_reset_layer_targets_only_services_with_inline_ports() {
    let inline =
        "services: {app: {ports: ['127.0.0.1:8194:8191']}, worker: {environment: {MODE: safe}}}";
    let reset = ComposeExecutor::port_reset_override("test", inline)
        .unwrap()
        .unwrap();
    assert!(reset.contains("!reset []"));
    let yaml: Value = yaml_serde::from_str(&reset).unwrap();
    assert!(yaml["services"]["app"].get("ports").is_some());
    assert!(yaml["services"].get("worker").is_none());
    assert!(
        ComposeExecutor::port_reset_override("test", "services: {worker: {image: alpine}}")
            .unwrap()
            .is_none()
    );
}

#[test]
fn inline_exceptions_allow_sections_and_new_services_independently() {
    let base = "services: {app: {image: nginx}}";
    let sections = "volumes: {data: {}}";
    let policy = ComposeSecurityPolicy {
        disabled_checks: [PolicyCheck::InlineSections].into(),
    };
    assert!(ComposeExecutor::validate_compose_override_with_policy(
        "test", base, sections, &policy
    )
    .is_ok());
    assert!(ComposeExecutor::validate_compose_override_with_policy(
        "test",
        base,
        "services: []",
        &policy
    )
    .is_err());
    assert!(ComposeExecutor::validate_compose_override_with_policy(
        "test",
        base,
        "services: {extra: {image: nginx}}",
        &policy
    )
    .is_err());
    let policy = ComposeSecurityPolicy {
        disabled_checks: [PolicyCheck::InlineServices, PolicyCheck::InlineFields].into(),
    };
    assert!(ComposeExecutor::validate_compose_override_with_policy(
        "test",
        "include: [base.yml]",
        "services: {extra: {image: nginx}}",
        &policy
    )
    .is_ok());
    assert!(ComposeExecutor::validate_compose_override_with_policy(
        "test", base, sections, &policy
    )
    .is_err());
}

#[test]
fn shared_external_certificate_volume_requires_both_explicit_grants() {
    let compose = "services:\n  proxy:\n    image: nginx:alpine\n    volumes: [certificates:/etc/certificates:ro]\nvolumes:\n  certificates:\n    external: true\n    name: shared-certificates\n";
    for checks in [
        vec![],
        vec![PolicyCheck::ExternalVolumes],
        vec![PolicyCheck::VolumeNames],
    ] {
        assert!(executor_with_checks_disabled(&checks)
            .validate_compose_security_policy("certificates", compose)
            .is_err());
    }
    assert!(executor_with_checks_disabled(&[
        PolicyCheck::ExternalVolumes,
        PolicyCheck::VolumeNames
    ])
    .validate_compose_security_policy("certificates", compose)
    .is_ok());
    let ordinary = "services: {app: {image: nginx, volumes: ['data:/data']}}\nvolumes: {data: {}}";
    assert!(executor_with_checks_disabled(&[])
        .validate_compose_security_policy("ordinary", ordinary)
        .is_ok());
}

#[test]
fn anonymous_short_syntax_volume_is_not_a_host_bind() {
    let executor = executor_with_checks_disabled(&[]);
    let anonymous = "services:\n  web:\n    image: node:22\n    volumes:\n      - /app/node_modules\n      - cache:/cache\nvolumes:\n  cache: {}\n";
    executor.preflight_validate(anonymous, None).unwrap();

    let long_form = "services:\n  web:\n    image: node:22\n    volumes:\n      - type: volume\n        target: /app/node_modules\n";
    executor.preflight_validate(long_form, None).unwrap();

    // Real host binds and interpolated entries stay guarded.
    for rejected in [
            "services:\n  web:\n    image: node:22\n    volumes:\n      - /etc:/host-etc\n",
            "services:\n  web:\n    image: node:22\n    volumes:\n      - ${MOUNT}\n",
            "services:\n  web:\n    image: node:22\n    volumes:\n      - type: bind\n        target: /data\n",
        ] {
            assert!(
                matches!(
                    executor.preflight_validate(rejected, None),
                    Err(ComposeError::SecurityPolicyViolation { .. })
                ),
                "{rejected}"
            );
        }
    assert_eq!(
        ComposeExecutor::volume_source(&YamlValue::String("/app/node_modules".into())),
        None
    );
    assert_eq!(
        ComposeExecutor::volume_source(&YamlValue::String("./data:/data:ro".into())),
        Some("./data".to_string())
    );
}

#[test]
fn non_loopback_port_rejection_names_the_exact_fix() {
    let executor = executor_with_checks_disabled(&[]);
    let cases = [
        (
            "- \"3000:3000\"",
            "change \"3000:3000\" to \"127.0.0.1::3000\"",
        ),
        (
            "- \"0.0.0.0:8080:80/udp\"",
            "change \"0.0.0.0:8080:80/udp\" to \"127.0.0.1::80/udp\"",
        ),
        ("- 5432", "change \"5432\" to \"127.0.0.1::5432\""),
        ("- \"${PORT}:3000\"", "uses a variable"),
        (
            "- target: 80\n        published: 8080",
            "host_ip: 127.0.0.1",
        ),
    ];
    for (entry, expected) in cases {
        let compose = format!("services:\n  web:\n    image: app\n    ports:\n      {entry}\n");
        let Err(ComposeError::SecurityPolicyViolation { reason, .. }) =
            executor.preflight_validate(&compose, None)
        else {
            panic!("{entry} must be rejected");
        };
        assert!(reason.contains(expected), "{entry}: {reason}");
        assert!(reason.contains("select the service as public"), "{reason}");
        assert!(reason.contains("127.0.0.1"), "{reason}");
    }
    executor
        .preflight_validate(
            "services:\n  web:\n    image: app\n    ports:\n      - \"127.0.0.1::3000\"\n",
            None,
        )
        .unwrap();
}
