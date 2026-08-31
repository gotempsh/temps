// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::{sync::Arc, time::Duration};

use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::service::{CloudService, ManagedBackupOutcome};

/// How often to re-fetch and rotate the Cloud-managed backup credential.
///
/// Cloud-issued credentials expire at least daily by contract (see the ADR
/// on the managed-backup-source feature). This interval gives a 4x safety
/// margin under that minimum, so a transient Cloud outage during one tick
/// still leaves multiple retry opportunities before the credential a linked
/// instance is actually using could expire.
const ROTATION_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Keep the Cloud-managed `s3_sources` credential from ever going stale.
///
/// Enrollment provisions it once; without this loop, a linked instance would
/// silently start failing every backup once that credential's TTL elapsed —
/// hours or days after the operator last touched Cloud settings, with no
/// action of theirs to point at.
pub async fn run(service: Arc<CloudService>, mut cancel: watch::Receiver<bool>) {
    info!("Cloud backup credential rotation started");
    loop {
        tokio::select! {
            changed = cancel.changed() => {
                if changed.is_err() {
                    warn!("Cloud backup credential rotation stopped because its owner was dropped");
                    return;
                }
                if *cancel.borrow() {
                    info!("Cloud backup credential rotation stopped after shutdown request");
                    return;
                }
            }
            _ = tokio::time::sleep(ROTATION_INTERVAL) => {
                if !service.link().is_linked() {
                    debug!("Cloud is not linked; skipping backup credential rotation tick");
                    continue;
                }
                match service.provision_managed_backup_source().await {
                    // `provision_managed_backup_source` already logs the
                    // specifics (including the loud error for a bucket
                    // change) at every outcome; the loop itself only needs
                    // to keep ticking.
                    ManagedBackupOutcome::Provisioned
                    | ManagedBackupOutcome::ProvisionedBucketChanged { .. }
                    | ManagedBackupOutcome::NotConfigured
                    | ManagedBackupOutcome::Unavailable(_) => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stops_immediately_on_shutdown_request_without_waiting_for_a_tick() {
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let link = Arc::new(
            temps_cloud_client::CloudLink::load_for_loopback_development(
                tempfile::tempdir().expect("state dir").path().to_path_buf(),
                "test-agent",
            ),
        );
        let config = Arc::new(temps_config::ConfigService::new(
            Arc::new(
                temps_config::ServerConfig::new(
                    "127.0.0.1:3000".to_string(),
                    "postgresql://test".to_string(),
                    None,
                    Some("127.0.0.1:8000".to_string()),
                )
                .expect("ServerConfig::new"),
            ),
            Arc::new(
                sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
            ),
        ));
        let db = Arc::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        let encryption = Arc::new(temps_core::EncryptionService::new_from_password(
            "rotation-test",
        ));
        let service = Arc::new(CloudService::new(link, config, db, encryption, true));

        let handle = tokio::spawn(run(service, cancel_rx));
        cancel_tx.send(true).expect("send shutdown signal");
        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("rotation loop should stop promptly, not wait for ROTATION_INTERVAL")
            .expect("rotation task should not panic");
    }
}
