// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

#[test]
fn a_replaced_gateway_image_that_gained_a_dev_tag_is_kept() {
    use super::replaced_gateway_image_is_disposable;
    let digest = || vec!["ghcr.io/gotempsh/temps-preview-gateway@sha256:a".to_string()];
    assert!(replaced_gateway_image_is_disposable(&[], &digest()));
    assert!(replaced_gateway_image_is_disposable(
        &["ghcr.io/gotempsh/temps-preview-gateway:0.4.1".to_string()],
        &digest()
    ));
    // Removing it by ID would also drop the `:dev` tag, which is never
    // pulled back.
    assert!(!replaced_gateway_image_is_disposable(
        &["ghcr.io/gotempsh/temps-preview-gateway:dev".to_string()],
        &digest()
    ));
    // No digest: a local build, possibly the only copy.
    assert!(!replaced_gateway_image_is_disposable(&[], &[]));
}

#[test]
fn only_a_replaced_temps_gateway_image_is_removed() {
    use super::superseded_gateway_image;
    let old = "ghcr.io/gotempsh/temps-preview-gateway@sha256:aaaa";
    assert_eq!(
        superseded_gateway_image(old, "sha256:old", "sha256:new"),
        Some("sha256:old")
    );
    // Same image: a restart or config drift, nothing was replaced.
    assert_eq!(
        superseded_gateway_image(old, "sha256:new", "sha256:new"),
        None
    );
    assert_eq!(superseded_gateway_image(old, "", "sha256:new"), None);
    // Images the operator pointed the gateway at are theirs.
    assert_eq!(
        superseded_gateway_image("temps-preview-gateway:dev", "sha256:old", "sha256:new"),
        None
    );
    // A local dev build under the official repository name is never
    // pulled, so it could not come back either.
    assert_eq!(
        superseded_gateway_image(
            "ghcr.io/gotempsh/temps-preview-gateway:dev",
            "sha256:old",
            "sha256:new"
        ),
        None
    );
    assert_eq!(
        superseded_gateway_image("sha256:old", "sha256:old", "sha256:new"),
        None
    );
    assert_eq!(
        superseded_gateway_image(
            "registry.internal/mirror/temps-preview-gateway:1",
            "sha256:old",
            "sha256:new"
        ),
        None
    );
}

use super::*;
use tempfile::TempDir;

#[test]
fn a_disabled_gateway_is_removed_only_when_it_is_this_instances() {
    assert!(created_by_another_instance(Some("other"), "own"));
    assert!(!created_by_another_instance(Some("own"), "own"));
    // Unknown on either side: the configured name is this instance's.
    assert!(!created_by_another_instance(None, "own"));
    assert!(!created_by_another_instance(Some(""), "own"));
    assert!(!created_by_another_instance(Some("other"), ""));
}

#[test]
fn disabled_settings_skip_reconciliation_operations() {
    let settings = PreviewGatewaySettings {
        enabled: false,
        ..PreviewGatewaySettings::default()
    };

    assert!(!should_reconcile(&settings));
}

#[test]
fn container_names_that_docker_and_its_dns_accept_are_kept() {
    assert_eq!(
        validated_container_name("temps-preview-gateway-2").as_deref(),
        Ok("temps-preview-gateway-2")
    );
    assert_eq!(
        validated_container_name("  gw.local_1  ").as_deref(),
        Ok("gw.local_1")
    );
    assert_eq!(
        validated_container_name("   ").as_deref(),
        Ok(PREVIEW_GATEWAY_CONTAINER)
    );
    assert!(validated_container_name(&"g".repeat(63)).is_ok());
}

#[test]
fn container_names_that_docker_or_its_dns_would_refuse_are_rejected() {
    for name in [
        "-gateway",
        "g",
        "has space",
        "slash/name",
        "gätewày",
        &"g".repeat(64),
    ] {
        let error = validated_container_name(name).expect_err(name);
        assert_eq!(error.name, name);
        assert!(error.to_string().contains(&format!("{name:?}")), "{error}");
    }
}

#[tokio::test]
async fn gateway_operations_run_one_at_a_time() {
    let running = lock_operations().await;
    let next = tokio::spawn(async {
        let _held = lock_operations().await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        !next.is_finished(),
        "a second gateway operation started while the first was running"
    );

    drop(running);
    tokio::time::timeout(std::time::Duration::from_secs(60), next)
        .await
        .expect("the waiting operation runs once the first one finishes")
        .expect("the waiting operation does not panic");
}

#[tokio::test]
async fn sandboxes_created_after_a_rename_use_the_new_gateway_name() {
    let renamer = GatewayName::default();
    let provider = renamer.clone();

    // A rename waits for a sandbox network being labelled and attached.
    let setting_up = provider.read().await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), renamer.write())
            .await
            .is_err(),
        "renamed while a sandbox network was being set up"
    );
    drop(setting_up);

    // A sandbox network set up during a rename waits for the new name.
    let mut renaming = renamer.write().await;
    let waiting = tokio::spawn({
        let provider = provider.clone();
        async move { provider.read().await.clone() }
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        !waiting.is_finished(),
        "a sandbox network was set up while the gateway was being renamed"
    );
    *renaming = "temps-preview-gateway-2".to_string();
    drop(renaming);

    let seen = tokio::time::timeout(std::time::Duration::from_secs(60), waiting)
        .await
        .expect("the waiting sandbox continues once the rename finishes")
        .expect("the waiting sandbox does not panic");
    assert_eq!(seen, "temps-preview-gateway-2");
    assert_eq!(*provider.read().await, "temps-preview-gateway-2");
}

#[tokio::test]
async fn the_provider_records_the_sandbox_networks_it_sets_up_until_removed() {
    let renamer = GatewayName::default();
    let provider = renamer.clone();

    provider.network_set_up("temps-sandbox-net-v3-b").await;
    provider.network_set_up("temps-sandbox-net-v3-a").await;
    // Recovering a sandbox sets its network up again.
    provider.network_set_up("temps-sandbox-net-v3-a").await;
    assert_eq!(
        renamer.networks_set_up().await,
        vec!["temps-sandbox-net-v3-a", "temps-sandbox-net-v3-b"]
    );

    provider.network_removed("temps-sandbox-net-v3-a").await;
    provider
        .network_removed("temps-sandbox-net-v3-never-set-up")
        .await;
    assert_eq!(
        renamer.networks_set_up().await,
        vec!["temps-sandbox-net-v3-b"]
    );
}

#[test]
fn networks_removed_outside_temps_do_not_block_a_rename() {
    let network = |name: &str| bollard::models::Network {
        name: Some(name.to_string()),
        ..Default::default()
    };
    let docker_has = [
        network("temps-sandbox-net-v3-a"),
        network("bridge"),
        bollard::models::Network::default(),
    ];

    assert_eq!(
        still_present(
            vec![
                "temps-sandbox-net-v3-a".to_string(),
                "temps-sandbox-net-v3-removed-by-hand".to_string(),
            ],
            &docker_has,
        ),
        vec!["temps-sandbox-net-v3-a"]
    );
    assert!(still_present(Vec::new(), &docker_has).is_empty());
}

#[tokio::test]
async fn only_live_sandboxes_on_this_host_block_a_rename() {
    use sea_orm::{DatabaseBackend, MockDatabase, Value};

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results(vec![vec![std::collections::BTreeMap::from([(
            "num_items".to_string(),
            Value::BigInt(Some(2)),
        )])]])
        .into_connection();
    assert_eq!(count_live_sandboxes(&db).await.ok(), Some(2));

    let log = db.into_transaction_log();
    let statement = log
        .iter()
        .flat_map(|transaction| transaction.statements())
        .next()
        .expect("the count query was sent");
    assert!(
        statement.sql.contains(r#""sandboxes"."status" <> $1"#),
        "{}",
        statement.sql
    );
    assert!(
        statement.sql.contains(r#""sandboxes"."node_id" IS NULL"#),
        "{}",
        statement.sql
    );
    let values = format!("{:?}", statement.values);
    assert!(values.contains("destroyed"), "{values}");
}

#[tokio::test]
async fn a_failed_sandbox_count_says_what_it_was_for() {
    use sea_orm::{DatabaseBackend, DbErr, MockDatabase};

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_errors(vec![DbErr::Custom("connection reset".to_string())])
        .into_connection();
    let error = count_live_sandboxes(&db)
        .await
        .expect_err("the count query failed");

    assert!(matches!(
        error,
        PreviewGatewayOwnershipError::CountSandboxes { .. }
    ));
    let chain = error_chain(&error);
    assert!(
        chain.starts_with(
            "failed to count this instance's sandboxes before renaming the preview gateway"
        ),
        "{chain}"
    );
    assert!(chain.contains("connection reset"), "{chain}");
}

#[test]
fn gateway_errors_name_the_container_and_keep_the_docker_error() {
    let error = PreviewGatewayError::RemoveContainer {
        container: "temps-preview-gateway-ingress".to_string(),
        source: bollard::errors::Error::DockerResponseServerError {
            status_code: 409,
            message: "removal of container is already in progress".to_string(),
        },
    };

    assert_eq!(
            error_chain(&error),
            "failed to remove preview gateway container temps-preview-gateway-ingress: Docker responded with status code 409: removal of container is already in progress"
        );
    assert!(matches!(
        error,
        PreviewGatewayError::RemoveContainer {
            source: bollard::errors::Error::DockerResponseServerError {
                status_code: 409,
                ..
            },
            ..
        }
    ));
}

#[test]
fn a_gateway_that_cannot_be_inspected_reports_why_and_whether_it_was_removed() {
    let inspection = || {
        Box::new(PreviewGatewayError::InspectContainer {
            container: "temps-preview-gateway".to_string(),
            source: bollard::errors::Error::DockerResponseServerError {
                status_code: 500,
                message: "inspect timed out".to_string(),
            },
        })
    };

    let removed = PreviewGatewayError::RemovedUninspectable {
        container: "temps-preview-gateway".to_string(),
        inspection: inspection(),
    };
    assert_eq!(
            error_chain(&removed),
            "removed preview gateway container temps-preview-gateway because it could not be inspected: failed to inspect preview gateway container temps-preview-gateway: Docker responded with status code 500: inspect timed out"
        );

    let not_removed = PreviewGatewayError::UninspectableNotRemoved {
        container: "temps-preview-gateway".to_string(),
        inspection: inspection(),
        removal: Box::new(PreviewGatewayError::RemoveContainer {
            container: "temps-preview-gateway-ingress".to_string(),
            source: bollard::errors::Error::DockerResponseServerError {
                status_code: 500,
                message: "removal failed".to_string(),
            },
        }),
    };
    assert_eq!(
            error_chain(&not_removed),
            "preview gateway container temps-preview-gateway could not be inspected (failed to inspect preview gateway container temps-preview-gateway: Docker responded with status code 500: inspect timed out), and removing it failed: failed to remove preview gateway container temps-preview-gateway-ingress: Docker responded with status code 500: removal failed"
        );
}

#[tokio::test]
async fn an_unreachable_daemon_reports_the_failed_inspection_and_removal() {
    let docker = Docker::connect_with_http("http://127.0.0.1:1", 1, bollard::API_DEFAULT_VERSION)
        .expect("a client for an unreachable daemon");

    let error = disable_unsafe_existing_gateway(&docker, "temps-preview-gateway")
        .await
        .expect_err("nothing can be inspected or removed");

    match &error {
        PreviewGatewayError::UninspectableNotRemoved {
            container,
            inspection,
            removal,
        } => {
            assert_eq!(container, "temps-preview-gateway");
            assert!(
                matches!(**inspection, PreviewGatewayError::ListContainers { .. }),
                "{error:?}"
            );
            assert!(
                matches!(**removal, PreviewGatewayError::RemoveContainer { .. }),
                "{error:?}"
            );
        }
        other => panic!("an unreachable daemon gave an unexpected error: {other:?}"),
    }
}

#[test]
fn docker_operation_error_preserves_bollard_source() {
    let source = bollard::errors::Error::DockerResponseServerError {
        status_code: 500,
        message: "daemon rejected the requested port mapping".to_string(),
    };
    let error = docker_operation_error("failed to start preview gateway", source);

    assert_eq!(
            format!("{error:#}"),
            "failed to start preview gateway: Docker responded with status code 500: daemon rejected the requested port mapping"
        );
}

#[test]
fn gateway_diagnostic_redacts_structured_and_registry_credentials() {
    let diagnostic = concat!(
        "registry user:password@example.test rejected ",
        r#"{"token":"json-value","client_secret":"client-value"} "#,
        "Authorization: Basic basic-value; X-Registry-Auth: registry-value; Bearer bearer-value"
    );
    let sanitized = sanitize_gateway_diagnostic(diagnostic, &[]);

    assert!(sanitized.contains("[redacted]@example.test"));
    assert!(sanitized.contains(r#""token":[redacted]"#));
    assert!(sanitized.contains(r#""client_secret":[redacted]"#));
    assert!(sanitized.contains("Authorization: [redacted]"));
    assert!(sanitized.contains("X-Registry-Auth: [redacted]"));
    assert!(sanitized.contains("Bearer [redacted]"));
    for secret in [
        "user:password",
        "json-value",
        "client-value",
        "basic-value",
        "registry-value",
        "bearer-value",
    ] {
        assert!(!sanitized.contains(secret));
    }
}

#[test]
fn default_gateway_image_is_immutable() {
    assert!(PreviewGatewaySettings::default().image.is_empty());
    assert!(PREVIEW_GATEWAY_IMAGE.contains("@sha256:"));
    assert_eq!(resolved_gateway_image(""), PREVIEW_GATEWAY_IMAGE);
    assert_eq!(resolved_gateway_image(" \t\n "), PREVIEW_GATEWAY_IMAGE);
    assert_eq!(
        resolved_gateway_image(temps_core::release_images::LOCAL_PREVIEW_GATEWAY_IMAGE),
        temps_core::release_images::LOCAL_PREVIEW_GATEWAY_IMAGE
    );
    assert_eq!(
        resolved_gateway_image("ghcr.io/operator/custom@sha256:1234"),
        "ghcr.io/operator/custom@sha256:1234"
    );
    assert_eq!(
        resolved_gateway_image(" ghcr.io/operator/custom@sha256:1234 "),
        " ghcr.io/operator/custom@sha256:1234 "
    );
}

#[test]
fn gateway_control_network_is_internal_and_has_no_outbound_route() {
    let request = preview_gateway_network_request("preview-test");

    assert_eq!(request.driver.as_deref(), Some("bridge"));
    assert_eq!(request.internal, Some(true));
    assert_eq!(request.enable_ipv6, Some(false));
    assert_eq!(
        request
            .labels
            .as_ref()
            .and_then(|labels| labels.get(PREVIEW_GATEWAY_NETWORK_LABEL))
            .map(String::as_str),
        Some(PREVIEW_GATEWAY_NETWORK_POLICY_VERSION)
    );
    assert!(request
        .options
        .as_ref()
        .is_some_and(|options| !options.contains_key(BRIDGE_ENABLE_ICC_OPTION)));
    assert_eq!(
        request
            .options
            .as_ref()
            .and_then(|options| options.get(BRIDGE_ENABLE_MASQUERADE_OPTION))
            .map(String::as_str),
        Some("false")
    );
    assert!(has_host_isolation(request.options.as_ref()));
}

#[test]
fn existing_gateway_control_network_accepts_only_complete_host_isolation_policy() {
    let request = preview_gateway_network_request("preview-test");
    let modern = bollard::models::NetworkInspect {
        name: Some(request.name),
        driver: request.driver,
        internal: request.internal,
        enable_ipv6: request.enable_ipv6,
        labels: request.labels,
        options: request.options,
        ..Default::default()
    };
    assert!(preview_gateway_network_matches_policy(&modern));

    let mut legacy = modern.clone();
    let legacy_options = legacy.options.get_or_insert_with(HashMap::new);
    legacy_options.remove(BRIDGE_GATEWAY_MODE_IPV4_OPTION);
    legacy_options.insert(BRIDGE_INHIBIT_IPV4_OPTION.to_string(), "true".to_string());
    assert!(preview_gateway_network_matches_policy(&legacy));

    let mut masquerading = legacy.clone();
    masquerading
        .options
        .get_or_insert_with(HashMap::new)
        .insert(
            BRIDGE_ENABLE_MASQUERADE_OPTION.to_string(),
            "true".to_string(),
        );
    assert!(!preview_gateway_network_matches_policy(&masquerading));

    let mut externally_routed = legacy.clone();
    externally_routed.internal = Some(false);
    assert!(!preview_gateway_network_matches_policy(&externally_routed));

    let mut disabled_isolation = legacy;
    disabled_isolation
        .options
        .get_or_insert_with(HashMap::new)
        .insert(BRIDGE_INHIBIT_IPV4_OPTION.to_string(), "false".to_string());
    assert!(!preview_gateway_network_matches_policy(&disabled_isolation));
}

#[test]
fn gateway_image_protocol_requires_exact_security_attestation() {
    let compatible = HashMap::from([(
        PREVIEW_GATEWAY_SECURITY_PROTOCOL_LABEL.to_string(),
        PREVIEW_GATEWAY_SECURITY_PROTOCOL_VERSION.to_string(),
    )]);
    let legacy = HashMap::from([(
        PREVIEW_GATEWAY_SECURITY_PROTOCOL_LABEL.to_string(),
        "legacy".to_string(),
    )]);

    assert!(has_required_gateway_protocol(Some(&compatible)));
    assert!(!has_required_gateway_protocol(Some(&legacy)));
    assert!(!has_required_gateway_protocol(None));
}

#[test]
fn existing_image_is_preserved_only_when_upgrades_are_off_and_protocol_is_verified() {
    assert!(should_preserve_existing_image(false, true));
    assert!(!should_preserve_existing_image(false, false));
    assert!(!should_preserve_existing_image(true, true));
}

#[test]
fn gateway_reconnects_only_to_managed_per_sandbox_networks() {
    let legacy_default = bollard::models::Network {
        name: Some("temps-sandbox-net-v3-temps-sandbox-workspace".to_string()),
        labels: Some(HashMap::from([(
            SANDBOX_NETWORK_OWNER_LABEL.to_string(),
            "temps-sandbox-workspace".to_string(),
        )])),
        ..Default::default()
    };
    let scoped_custom = bollard::models::Network {
        name: legacy_default.name.clone(),
        labels: Some(HashMap::from([
            (
                SANDBOX_NETWORK_OWNER_LABEL.to_string(),
                "temps-sandbox-workspace".to_string(),
            ),
            (
                crate::sandbox::docker::SANDBOX_PREVIEW_GATEWAY_LABEL.to_string(),
                "temps-preview-gateway-custom".to_string(),
            ),
        ])),
        ..Default::default()
    };
    let spoofed_name = bollard::models::Network {
        name: Some("application-production".to_string()),
        labels: legacy_default.labels.clone(),
        ..Default::default()
    };
    let missing_owner = bollard::models::Network {
        name: legacy_default.name.clone(),
        ..Default::default()
    };

    assert_eq!(
        managed_sandbox_network_name(&legacy_default, PREVIEW_GATEWAY_CONTAINER, false),
        Some("temps-sandbox-net-v3-temps-sandbox-workspace")
    );
    assert_eq!(
        managed_sandbox_network_name(&legacy_default, "temps-preview-gateway-custom", false),
        None
    );
    assert_eq!(
        managed_sandbox_network_name(&legacy_default, "temps-preview-gateway-custom", true),
        Some("temps-sandbox-net-v3-temps-sandbox-workspace")
    );
    assert_eq!(
        managed_sandbox_network_name(&scoped_custom, "temps-preview-gateway-custom", false),
        Some("temps-sandbox-net-v3-temps-sandbox-workspace")
    );
    assert_eq!(
        managed_sandbox_network_name(&scoped_custom, PREVIEW_GATEWAY_CONTAINER, true),
        None
    );
    assert_eq!(
        managed_sandbox_network_name(&spoofed_name, PREVIEW_GATEWAY_CONTAINER, false),
        None
    );
    assert_eq!(
        managed_sandbox_network_name(&missing_owner, PREVIEW_GATEWAY_CONTAINER, false),
        None
    );
}

#[tokio::test]
async fn disable_removes_the_gateway_and_its_ingress_companion() {
    let docker = match Docker::connect_with_local_defaults() {
        Ok(docker) if docker.ping().await.is_ok() => docker,
        _ => {
            println!("Docker not available, skipping test");
            return;
        }
    };
    let image = crate::sandbox::docker::image_name_for_runtime("node");
    if docker.inspect_image(&image).await.is_err() {
        println!("Managed node sandbox image not present, skipping test");
        return;
    }

    let suffix = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos()
    );
    let gateway_name = format!("temps-preview-gateway-disable-test-{suffix}");
    let settings = PreviewGatewaySettings {
        enabled: false,
        container_name: gateway_name.clone(),
        ..PreviewGatewaySettings::default()
    };
    let names = [gateway_name.clone(), ingress_container_name(&gateway_name)];
    let created: Result<()> = async {
        for name in &names {
            docker
                .create_container(
                    Some(CreateContainerOptionsBuilder::new().name(name).build()),
                    ContainerCreateBody {
                        image: Some(image.clone()),
                        ..Default::default()
                    },
                )
                .await?;
        }
        Ok(())
    }
    .await;
    let held = lock_operations().await;
    let disabled = match created {
        Ok(()) => disable(&held, &docker, &settings)
            .await
            .map_err(anyhow::Error::from),
        Err(error) => Err(error),
    };
    let mut remaining = Vec::new();
    for name in &names {
        if inspect(&docker, name).await.ok().flatten().is_some() {
            remaining.push(name.clone());
        }
    }
    let _ = remove_gateway_pair(&docker, &gateway_name).await;

    disabled.expect("disabling removes the gateway pair");
    assert!(remaining.is_empty(), "still present: {remaining:?}");
    disable(&held, &docker, &settings)
        .await
        .expect("disabling a gateway that is already gone succeeds");

    // Renaming removes the pair under the previous name the same way.
    let recreated: Result<()> = async {
        for name in &names {
            docker
                .create_container(
                    Some(CreateContainerOptionsBuilder::new().name(name).build()),
                    ContainerCreateBody {
                        image: Some(image.clone()),
                        ..Default::default()
                    },
                )
                .await?;
        }
        Ok(())
    }
    .await;
    let renamed = match recreated {
        Ok(()) => remove_renamed(&held, &docker, &settings)
            .await
            .map_err(anyhow::Error::from),
        Err(error) => Err(error),
    };
    let mut left_behind = Vec::new();
    for name in &names {
        if inspect(&docker, name).await.ok().flatten().is_some() {
            left_behind.push(name.clone());
        }
    }
    let _ = remove_gateway_pair(&docker, &gateway_name).await;
    renamed.expect("renaming removes the pair under the previous name");
    assert!(left_behind.is_empty(), "still present: {left_behind:?}");

    // A gateway another instance created under this name stays.
    let foreign = docker
        .create_container(
            Some(
                CreateContainerOptionsBuilder::new()
                    .name(&gateway_name)
                    .build(),
            ),
            ContainerCreateBody {
                image: Some(image.clone()),
                env: Some(vec![
                    "PREVIEW_GATEWAY_SHARED_SECRET=other-instance".to_string()
                ]),
                ..Default::default()
            },
        )
        .await;
    let own = PreviewGatewaySettings {
        shared_secret: "this-instance".to_string(),
        ..settings.clone()
    };
    let kept = match foreign {
        Ok(_) => disable(&held, &docker, &own)
            .await
            .map_err(anyhow::Error::from),
        Err(error) => Err(error.into()),
    };
    let still_present = inspect(&docker, &gateway_name)
        .await
        .ok()
        .flatten()
        .is_some();
    let _ = remove_gateway_pair(&docker, &gateway_name).await;
    kept.expect("disabling leaves another instance's gateway alone");
    assert!(still_present, "another instance's gateway was removed");
}

#[tokio::test]
async fn reconcile_leaves_another_instances_gateway_and_reports_the_conflict() {
    let docker = match Docker::connect_with_local_defaults() {
        Ok(docker) if docker.ping().await.is_ok() => docker,
        _ => {
            println!("Docker not available, skipping test");
            return;
        }
    };
    let image = crate::sandbox::docker::image_name_for_runtime("node");
    if docker.inspect_image(&image).await.is_err() {
        println!("Managed node sandbox image not present, skipping test");
        return;
    }

    let suffix = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos()
    );
    let gateway_name = format!("temps-preview-gateway-foreign-test-{suffix}");
    let spec = PreviewGatewaySpec {
        image: image.clone(),
        container_name: gateway_name.clone(),
        network: format!("temps-preview-gateway-foreign-test-net-{suffix}"),
        host_port: 0,
        shared_secret: "this-instance".to_string(),
    };
    // No query is expected: the conflict is found before the database.
    let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection();
    let docker = Arc::new(docker);

    docker
        .create_container(
            Some(
                CreateContainerOptionsBuilder::new()
                    .name(&gateway_name)
                    .build(),
            ),
            ContainerCreateBody {
                image: Some(image.clone()),
                env: Some(vec![
                    "PREVIEW_GATEWAY_SHARED_SECRET=other-instance".to_string()
                ]),
                ..Default::default()
            },
        )
        .await
        .expect("create the other instance's gateway");

    // Held throughout: the failure record is shared with the other tests
    // that run a gateway operation.
    let held = lock_operations().await;
    let reconciled = reconcile(&held, docker.clone(), &db, spec.clone()).await;
    let reported = last_reconcile_failure();
    LAST_FAILURE.clear();
    let still_present = inspect(&docker, &gateway_name)
        .await
        .ok()
        .flatten()
        .is_some();
    let own_spec = PreviewGatewaySpec {
        shared_secret: "other-instance".to_string(),
        ..spec
    };
    let own_gateway = refuse_another_instances_gateway(&docker, &own_spec).await;
    let _ = remove_gateway_pair(&docker, &gateway_name).await;
    drop(held);

    let error = reconciled.expect_err("the conflict fails the reconciliation");
    assert!(
        matches!(
            error.downcast_ref::<PreviewGatewayError>(),
            Some(PreviewGatewayError::OwnedByAnotherInstance { container })
                if *container == gateway_name
        ),
        "{error:#}"
    );
    assert!(still_present, "another instance's gateway was removed");
    let reported = reported.expect("the conflict is on the gateway's status");
    assert!(reported.error.contains(&gateway_name), "{}", reported.error);
    assert!(
        reported.error.contains("another Temps instance"),
        "{}",
        reported.error
    );
    assert!(
        !reported.error.contains("other-instance"),
        "the status must not carry the other instance's secret: {}",
        reported.error
    );
    own_gateway.expect("this instance's own gateway is not a conflict");
}

#[tokio::test]
async fn recreated_custom_gateway_preserves_db_owned_legacy_sandbox_network() {
    let docker = match Docker::connect_with_local_defaults() {
        Ok(docker) if docker.ping().await.is_ok() => docker,
        _ => {
            println!("Docker not available, skipping test");
            return;
        }
    };
    let image = crate::sandbox::docker::image_name_for_runtime("node");
    if docker.inspect_image(&image).await.is_err() {
        println!("Managed node sandbox image not present, skipping test");
        return;
    }

    let suffix = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos()
    );
    let gateway_name = format!("temps-preview-gateway-test-{suffix}");
    let control_network = format!("temps-preview-gateway-control-test-{suffix}");
    let ingress_network = format!("temps-preview-gateway-ingress-test-{suffix}");
    let sandbox_container = format!("temps-sandbox-test-{suffix}");
    let sandbox_network = crate::sandbox::docker::sandbox_network_name(&sandbox_container);
    let spec = PreviewGatewaySpec {
        image: image.clone(),
        container_name: gateway_name.clone(),
        network: control_network.clone(),
        host_port: 0,
        shared_secret: String::new(),
    };
    let test_result: Result<bool> = async {
        ensure_network(&docker, &control_network).await?;
        ensure_ingress_network_named(&docker, &ingress_network).await?;
        create_host_isolated_network(
            &docker,
            with_host_isolation(NetworkCreateRequest {
                name: sandbox_network.clone(),
                labels: Some(HashMap::from([(
                    SANDBOX_NETWORK_OWNER_LABEL.to_string(),
                    sandbox_container,
                )])),
                ..Default::default()
            }),
        )
        .await?;

        create_and_start_on_ingress(&docker, &spec, &image, &HashSet::new(), &ingress_network)
            .await?;
        docker
            .connect_network(
                &sandbox_network,
                bollard::models::NetworkConnectRequest {
                    container: gateway_name.clone(),
                    endpoint_config: None,
                },
            )
            .await?;
        let preserved = HashSet::from([sandbox_network.clone()]);

        remove_gateway_pair(&docker, &gateway_name).await?;
        create_and_start_on_ingress(&docker, &spec, &image, &preserved, &ingress_network).await?;
        Ok(docker
            .inspect_container(&gateway_name, None::<InspectContainerOptions>)
            .await
            .ok()
            .and_then(|container| container.network_settings)
            .and_then(|settings| settings.networks)
            .is_some_and(|networks| networks.contains_key(&sandbox_network)))
    }
    .await;

    let _ = remove_gateway_pair(&docker, &gateway_name).await;
    let _ = docker.remove_network(&sandbox_network).await;
    let _ = docker.remove_network(&ingress_network).await;
    let _ = docker.remove_network(&control_network).await;

    let attached = test_result.expect("gateway recreation should succeed");
    assert!(
        attached,
        "the recreated custom gateway must preserve its DB-owned legacy sandbox network"
    );
}

#[test]
fn local_gateway_images_are_inspected_without_registry_pull() {
    assert!(!should_pull_gateway_image("temps-preview-gateway:dev"));
    assert!(!should_pull_gateway_image("sha256:verified-local-image"));
    assert!(should_pull_gateway_image(
        "ghcr.io/gotempsh/temps-preview-gateway:beta"
    ));
    assert!(should_pull_gateway_image(
        "ghcr.io/gotempsh/temps-preview-gateway@sha256:manifest"
    ));
}

#[test]
fn mutable_tag_does_not_hide_a_stale_running_image_id() {
    let old_running_image_id = "sha256:legacy";
    let newly_pulled_image_id = "sha256:strip-token-v1";

    assert!(!running_image_matches(
        old_running_image_id,
        newly_pulled_image_id
    ));
    assert!(running_image_matches(
        newly_pulled_image_id,
        newly_pulled_image_id
    ));
    assert!(!running_image_matches("", newly_pulled_image_id));
}

#[test]
fn gateway_ingress_network_publishes_loopback_without_container_peering() {
    let request = preview_gateway_ingress_network_request();

    assert_eq!(request.name, PREVIEW_GATEWAY_INGRESS_NETWORK);
    assert_eq!(request.driver.as_deref(), Some("bridge"));
    assert_eq!(request.internal, Some(false));
    assert_eq!(request.enable_ipv6, Some(false));
    assert_eq!(
        request
            .labels
            .as_ref()
            .and_then(|labels| labels.get(PREVIEW_GATEWAY_INGRESS_LABEL))
            .map(String::as_str),
        Some(PREVIEW_GATEWAY_NETWORK_POLICY_VERSION)
    );
    for option in [BRIDGE_ENABLE_ICC_OPTION, BRIDGE_ENABLE_MASQUERADE_OPTION] {
        assert_eq!(
            request
                .options
                .as_ref()
                .and_then(|options| options.get(option))
                .map(String::as_str),
            Some("false")
        );
    }
}

#[test]
fn ensure_shared_secret_creates_file_on_first_call() {
    let dir = TempDir::new().unwrap();
    let secret = ensure_shared_secret(dir.path()).unwrap();
    // 32 random bytes → 64 hex chars
    assert_eq!(secret.len(), 64, "secret should be 64 hex chars");
    // File should exist with the same content
    let on_disk = std::fs::read_to_string(dir.path().join(PREVIEW_GATEWAY_SECRET_FILE)).unwrap();
    assert_eq!(on_disk, secret);
}

#[test]
fn ensure_shared_secret_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let first = ensure_shared_secret(dir.path()).unwrap();
    let second = ensure_shared_secret(dir.path()).unwrap();
    assert_eq!(first, second, "second call should return the same secret");
}

#[test]
fn ensure_shared_secret_rejects_empty_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join(PREVIEW_GATEWAY_SECRET_FILE);
    std::fs::write(&path, "").unwrap();
    let result = ensure_shared_secret(dir.path());
    assert!(result.is_err(), "empty secret file should be an error");
}

#[cfg(unix)]
#[test]
fn ensure_shared_secret_sets_restrictive_perms() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new().unwrap();
    let _ = ensure_shared_secret(dir.path()).unwrap();
    let meta = std::fs::metadata(dir.path().join(PREVIEW_GATEWAY_SECRET_FILE)).unwrap();
    let mode = meta.permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "secret file should be 0600, got {:o}", mode);
}

#[test]
fn ensure_shared_secret_creates_parent_dirs() {
    let dir = TempDir::new().unwrap();
    let nested = dir.path().join("a").join("b").join("c");
    let secret = ensure_shared_secret(&nested).unwrap();
    assert_eq!(secret.len(), 64);
    assert!(nested.join(PREVIEW_GATEWAY_SECRET_FILE).exists());
}

#[test]
fn a_failed_setup_is_retried_soon_then_at_most_every_five_minutes() {
    let delays: Vec<u64> = (1..=9).map(|n| retry_delay(n).as_secs()).collect();
    assert_eq!(delays, vec![5, 10, 20, 40, 80, 160, 300, 300, 300]);
    // However long it has been failing.
    assert_eq!(retry_delay(u32::MAX).as_secs(), 300);
    assert_eq!(retry_delay(0).as_secs(), 5);
}

#[test]
fn a_lasting_failure_warns_once_an_hour_not_on_every_retry() {
    let pull = "reconcile failed: failed to pull image example/sandbox:0.0.0: manifest unknown";
    let start = std::time::Instant::now();
    let mut warnings = FailureWarnings::default();

    // An idle hour of retries at the capped delay: the first warns, the
    // eleven repeats after it do not.
    let warned: Vec<bool> = (0..12u64)
        .map(|n| warnings.should_warn(pull, start + std::time::Duration::from_secs(n * 300)))
        .collect();
    assert_eq!(warned.iter().filter(|w| **w).count(), 1, "{warned:?}");
    assert!(warned[0]);

    // An hour after the last warning, a reminder.
    assert!(warnings.should_warn(pull, start + REPEATED_FAILURE_WARN_INTERVAL));
    assert!(!warnings.should_warn(
        pull,
        start + REPEATED_FAILURE_WARN_INTERVAL + std::time::Duration::from_secs(300)
    ));
}

#[test]
fn a_new_failure_reason_or_a_recovery_makes_the_next_failure_warn() {
    let start = std::time::Instant::now();
    let mut warnings = FailureWarnings::default();
    assert!(warnings.should_warn("reconcile failed: pull", start));
    assert!(!warnings.should_warn("reconcile failed: pull", start));
    // A different fault is news.
    assert!(warnings.should_warn("reconcile failed: docker unavailable", start));
    // So is the same fault coming back after the gateway recovered.
    warnings.reset();
    assert!(warnings.should_warn("reconcile failed: docker unavailable", start));
}

#[test]
fn only_a_missing_or_exited_container_needs_setting_up_again() {
    use bollard::models::ContainerState;
    let state = |running: bool, restarting: bool| ContainerState {
        running: Some(running),
        restarting: Some(restarting),
        ..Default::default()
    };
    assert!(container_serves(Some(&state(true, false))));
    // Docker's restart policy is already bringing it back.
    assert!(container_serves(Some(&state(false, true))));
    assert!(!container_serves(Some(&state(false, false))));
    assert!(!container_serves(Some(&ContainerState::default())));
    assert!(!container_serves(None));
}

#[test]
fn a_failure_is_kept_with_its_retry_until_the_next_success() {
    let record = FailureRecord::new();
    let failed_at = chrono::DateTime::parse_from_rfc3339("2026-01-02T03:04:05Z")
        .expect("time")
        .with_timezone(&chrono::Utc);
    // A retry is scheduled for a failure only.
    record.retry_at(failed_at);
    assert_eq!(record.get(), None);

    record.failed("failed to pull image node: timeout".into(), failed_at);
    record.retry_at(failed_at + chrono::Duration::seconds(5));
    let failure = record.get().expect("recorded");
    assert_eq!(failure.error, "failed to pull image node: timeout");
    assert_eq!(
        setup_failure_fields(Some(failure)),
        (
            Some("failed to pull image node: timeout".to_string()),
            Some("2026-01-02T03:04:05Z".to_string()),
            Some("2026-01-02T03:04:10Z".to_string()),
        )
    );

    // A new failure has not been scheduled yet.
    record.failed("failed to start container".into(), failed_at);
    assert_eq!(record.get().and_then(|f| f.next_attempt_at), None);

    record.clear();
    assert_eq!(record.get(), None);
    assert_eq!(setup_failure_fields(None), (None, None, None));
}

#[test]
fn a_recorded_setup_error_never_carries_the_shared_secret() {
    let spec = PreviewGatewaySpec {
        shared_secret: "s3cr3t-gateway-token".into(),
        ..PreviewGatewaySpec::default()
    };
    let error = anyhow!("Docker said: PREVIEW_GATEWAY_SHARED_SECRET=s3cr3t-gateway-token")
        .context("failed to create container temps-preview-gateway");
    // Its own record: the gateway's Docker tests in this module write the
    // process-wide one concurrently.
    let record = FailureRecord::new();
    record_outcome_in(&record, &Err(error), &spec);
    let failure = record.get().expect("recorded");
    assert!(
        failure
            .error
            .starts_with("failed to create container temps-preview-gateway: "),
        "{}",
        failure.error
    );
    assert!(!failure.error.contains("s3cr3t"), "{}", failure.error);

    record_outcome_in(&record, &Ok(()), &spec);
    assert_eq!(record.get(), None);
}
