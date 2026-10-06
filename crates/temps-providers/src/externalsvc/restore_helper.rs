// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Identity and fencing for short-lived restore helper containers.
//!
//! Every engine's restore path launches one-shot helper containers (a WAL-G
//! fetcher, a datadir swapper, a `mongorestore` sidecar, ...) that run
//! independently of the Temps process. When Temps restarts mid-restore the
//! helper keeps writing into the target's volume while the restore run row
//! still says `running`. Before startup reconciliation releases the
//! "one active in-place restore per service" constraint it must make sure no
//! such helper is still alive, otherwise a second restore could race the
//! surviving one on the same data directory.
//!
//! Helpers created by this binary carry two labels:
//!
//! - `sh.temps.kind=restore-helper`
//! - `sh.temps.restore.target=<target container name>`
//!
//! Helpers created by older binaries carry no labels, so the fence also
//! recognises the legacy names that embed the target container name
//! (`<target>-restore-helper`, `<target>-restore-helper-<suffix>`,
//! `<target>-walg-restore-<uuid>`).

use std::collections::HashMap;

use bollard::query_parameters::{
    ListContainersOptionsBuilder, RemoveContainerOptions, StopContainerOptions,
};
use bollard::Docker;
use tracing::{info, warn};

/// Label key identifying the container's role.
pub const KIND_LABEL: &str = "sh.temps.kind";
/// Value of [`KIND_LABEL`] for restore helpers.
pub const RESTORE_HELPER_KIND: &str = "restore-helper";
/// Label key naming the service container a helper restores into.
pub const RESTORE_TARGET_LABEL: &str = "sh.temps.restore.target";

/// Seconds Docker waits after SIGTERM before SIGKILL when fencing a helper.
const FENCE_STOP_TIMEOUT_SECS: i32 = 30;

/// Labels to attach to every restore helper container.
pub fn restore_helper_labels(target_container: &str) -> HashMap<String, String> {
    HashMap::from([
        (KIND_LABEL.to_string(), RESTORE_HELPER_KIND.to_string()),
        (
            RESTORE_TARGET_LABEL.to_string(),
            target_container.to_string(),
        ),
    ])
}

/// Whether `name` (without Docker's leading `/`) is a helper that an older
/// binary created for `target_container`, before helpers were labelled.
pub fn is_legacy_restore_helper_name(target_container: &str, name: &str) -> bool {
    let name = name.trim_start_matches('/');
    let Some(rest) = name.strip_prefix(target_container) else {
        return false;
    };
    rest == "-restore-helper"
        || rest.starts_with("-restore-helper-")
        || rest.starts_with("-walg-restore-")
}

/// What the fence found and did for one target container.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoreFenceReport {
    /// Helper containers that were still running and have been stopped.
    pub stopped: Vec<String>,
    /// Exited helper containers that were removed.
    pub removed: Vec<String>,
}

impl RestoreFenceReport {
    pub fn is_empty(&self) -> bool {
        self.stopped.is_empty() && self.removed.is_empty()
    }
}

/// Error raised when a helper could not be fenced. The restore run that owns
/// it must stay active so a second restore cannot race it.
#[derive(Debug, thiserror::Error)]
pub enum RestoreFenceError {
    #[error("Failed to list restore helper containers for '{target_container}': {reason}")]
    List {
        target_container: String,
        reason: String,
    },
    #[error(
        "Failed to stop surviving restore helper '{helper}' for '{target_container}': {reason}"
    )]
    Stop {
        target_container: String,
        helper: String,
        reason: String,
    },
    #[error(
        "Could not resolve the container of {service_type} service '{service_name}' to fence \
         its restore helpers: {reason}"
    )]
    Resolve {
        service_name: String,
        service_type: String,
        reason: String,
    },
}

/// Stop and remove every restore helper that targets `target_container`.
///
/// Running helpers are stopped first (the caller is reconciling a restore
/// whose owning process is gone, so the helper's work can no longer be
/// observed or verified). A helper that cannot be stopped is an error;
/// failing to *remove* an already-stopped helper is only logged, because a
/// stopped container can no longer touch the target's data.
pub async fn fence_restore_helpers(
    docker: &Docker,
    target_container: &str,
) -> Result<RestoreFenceReport, RestoreFenceError> {
    let list = |filters: HashMap<String, Vec<String>>| {
        docker.list_containers(Some(
            ListContainersOptionsBuilder::new()
                .all(true)
                .filters(&filters)
                .build(),
        ))
    };

    let labelled = list(HashMap::from([(
        "label".to_string(),
        vec![format!("{}={}", RESTORE_TARGET_LABEL, target_container)],
    )]))
    .await
    .map_err(|e| RestoreFenceError::List {
        target_container: target_container.to_string(),
        reason: e.to_string(),
    })?;
    // Docker's `name` filter is a substring match; the exact legacy shapes
    // are checked below.
    let by_name = list(HashMap::from([(
        "name".to_string(),
        vec![target_container.to_string()],
    )]))
    .await
    .map_err(|e| RestoreFenceError::List {
        target_container: target_container.to_string(),
        reason: e.to_string(),
    })?;

    let mut helpers: Vec<(String, bool)> = Vec::new();
    for container in labelled.into_iter().chain(by_name) {
        let names = container.names.clone().unwrap_or_default();
        let labelled_for_target = container
            .labels
            .as_ref()
            .and_then(|labels| labels.get(RESTORE_TARGET_LABEL))
            .is_some_and(|target| target == target_container);
        let legacy = names
            .iter()
            .any(|name| is_legacy_restore_helper_name(target_container, name));
        if !labelled_for_target && !legacy {
            continue;
        }
        let Some(id) = container.id.clone() else {
            continue;
        };
        if helpers.iter().any(|(seen, _)| seen == &id) {
            continue;
        }
        let running = matches!(
            container.state,
            Some(bollard::models::ContainerSummaryStateEnum::RUNNING)
                | Some(bollard::models::ContainerSummaryStateEnum::RESTARTING)
                | Some(bollard::models::ContainerSummaryStateEnum::PAUSED)
        );
        helpers.push((id, running));
    }

    let mut report = RestoreFenceReport::default();
    for (id, running) in helpers {
        if running {
            docker
                .stop_container(
                    &id,
                    Some(StopContainerOptions {
                        t: Some(FENCE_STOP_TIMEOUT_SECS),
                        ..Default::default()
                    }),
                )
                .await
                .map_err(|e| RestoreFenceError::Stop {
                    target_container: target_container.to_string(),
                    helper: id.clone(),
                    reason: e.to_string(),
                })?;
            info!(
                target_container,
                helper = %id,
                "stopped surviving restore helper left by an interrupted restore"
            );
            report.stopped.push(id.clone());
        }
        match docker
            .remove_container(
                &id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
        {
            Ok(()) => {
                if !running {
                    report.removed.push(id);
                }
            }
            Err(e) => warn!(
                target_container,
                helper = %id,
                "restore helper is stopped but could not be removed: {}",
                e
            ),
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_name_the_target_container() {
        let labels = restore_helper_labels("postgres-orders");
        assert_eq!(
            labels.get(KIND_LABEL).map(String::as_str),
            Some("restore-helper")
        );
        assert_eq!(
            labels.get(RESTORE_TARGET_LABEL).map(String::as_str),
            Some("postgres-orders")
        );
    }

    #[test]
    fn legacy_names_match_only_their_target() {
        assert!(is_legacy_restore_helper_name(
            "postgres-orders",
            "/postgres-orders-restore-helper"
        ));
        assert!(is_legacy_restore_helper_name(
            "redis-cache",
            "redis-cache-restore-helper-1a2b3c4d"
        ));
        assert!(is_legacy_restore_helper_name(
            "mariadb-shop",
            "/mariadb-shop-walg-restore-0f1e2d3c"
        ));
        // The service container itself is never a helper.
        assert!(!is_legacy_restore_helper_name(
            "postgres-orders",
            "/postgres-orders"
        ));
        // A service whose name extends the target's is a different target.
        assert!(!is_legacy_restore_helper_name(
            "postgres-orders",
            "/postgres-orders-eu-restore-helper"
        ));
        assert!(!is_legacy_restore_helper_name(
            "postgres-orders",
            "/temps-redis-rdb-restore-1234"
        ));
    }

    #[tokio::test]
    async fn fence_stops_running_helpers_and_spares_the_service() {
        let Ok(docker) = Docker::connect_with_local_defaults() else {
            println!("Docker not available, skipping");
            return;
        };
        if docker.ping().await.is_err() {
            println!("Docker not available, skipping");
            return;
        }
        let image = "busybox:latest";
        if crate::utils::pull_image_with_retry(&docker, image, None)
            .await
            .is_err()
        {
            println!("Could not pull {image}, skipping");
            return;
        }
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let target = format!("temps-fence-target-{}", &suffix[..8]);
        let create = |name: String, labels: HashMap<String, String>| {
            let docker = docker.clone();
            async move {
                docker
                    .create_container(
                        Some(
                            bollard::query_parameters::CreateContainerOptionsBuilder::new()
                                .name(&name)
                                .build(),
                        ),
                        bollard::models::ContainerCreateBody {
                            image: Some(image.to_string()),
                            cmd: Some(vec!["sleep".into(), "300".into()]),
                            labels: Some(labels),
                            ..Default::default()
                        },
                    )
                    .await
                    .map(|c| c.id)
            }
        };
        let service = create(target.clone(), HashMap::new()).await;
        let labelled = create(
            format!("temps-fence-helper-{}", &suffix[..8]),
            restore_helper_labels(&target),
        )
        .await;
        let legacy = create(format!("{}-restore-helper", target), HashMap::new()).await;
        let (Ok(service), Ok(labelled), Ok(legacy)) = (service, labelled, legacy) else {
            println!("Could not create fence test containers, skipping");
            return;
        };
        for id in [&service, &labelled, &legacy] {
            let _ = docker
                .start_container(id, None::<bollard::query_parameters::StartContainerOptions>)
                .await;
        }

        let report = fence_restore_helpers(&docker, &target).await;

        let service_running = docker
            .inspect_container(
                &service,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .ok()
            .and_then(|c| c.state)
            .and_then(|s| s.running);
        let mut helpers_left = Vec::new();
        for id in [&labelled, &legacy] {
            if docker
                .inspect_container(
                    id,
                    None::<bollard::query_parameters::InspectContainerOptions>,
                )
                .await
                .is_ok()
            {
                helpers_left.push(id.to_string());
                let _ = docker
                    .remove_container(
                        id,
                        Some(RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await;
            }
        }
        let _ = docker
            .remove_container(
                &service,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;

        let report = report.expect("fence should succeed against a reachable daemon");
        assert_eq!(report.stopped.len(), 2, "both helpers were running");
        assert_eq!(service_running, Some(true), "service must not be fenced");
        assert!(helpers_left.is_empty(), "helpers must be removed");
    }
}
