// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Concrete job implementations for deployment workflows
//!
//! This module provides ready-to-use job implementations for common deployment tasks.

use std::sync::{Arc, LazyLock};
use temps_core::WorkflowError;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

static ARCHIVE_EXTRACTION_SEMAPHORE: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(2)));

pub(crate) async fn acquire_archive_extraction_permit(
) -> Result<OwnedSemaphorePermit, WorkflowError> {
    ARCHIVE_EXTRACTION_SEMAPHORE
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| {
            WorkflowError::JobExecutionFailed(
                "Archive extraction concurrency limiter was closed".to_string(),
            )
        })
}

#[cfg(test)]
mod extraction_limit_tests {
    use super::*;

    #[tokio::test]
    async fn extraction_limit_holds_at_two_until_work_releases_a_permit() {
        let first = acquire_archive_extraction_permit().await.unwrap();
        let second = acquire_archive_extraction_permit().await.unwrap();
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(20),
            acquire_archive_extraction_permit()
        )
        .await
        .is_err());
        drop(first);
        let third = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            acquire_archive_extraction_permit(),
        )
        .await
        .expect("released extraction slot")
        .unwrap();
        drop((second, third));
    }
}

pub mod build_image;
pub mod capture_source_files;
pub mod capture_source_maps;
pub mod configure_agents;
pub mod configure_crons;
pub mod configure_metric_alerts;
pub mod deploy_compose;
pub mod deploy_image;
pub mod deploy_static;
pub mod deploy_static_bundle;
pub mod deploy_static_from_source;
pub mod download_repo;
pub mod mark_deployment_complete;
pub mod node_health_check;
pub mod npmrc;
pub mod persist_static_assets;
pub mod pipeline_validation;
pub mod prepare_source_bundle;
pub mod pull_external_image;
pub mod scan_vulnerabilities;
pub mod take_screenshot;
pub mod verify_local_image;

pub use build_image::*;
pub use capture_source_files::*;
pub use capture_source_maps::*;
pub use configure_agents::*;
pub use configure_crons::*;
pub use configure_metric_alerts::*;
pub use deploy_compose::*;
pub use deploy_image::*;
pub use deploy_static::*;
pub use deploy_static_bundle::*;
pub use deploy_static_from_source::*;
pub use download_repo::*;
pub use mark_deployment_complete::*;
pub use persist_static_assets::*;
pub use prepare_source_bundle::*;
pub use pull_external_image::*;
pub use scan_vulnerabilities::*;
pub use take_screenshot::*;
pub use verify_local_image::*;
