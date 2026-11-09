// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Three owned tasks, no per-sandbox sidecars or unbounded task spawning. The
//! durable queue survives shutdown; worker leases expire without renewal.
use crate::coordinator::ServiceAccessCoordinator;
use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::task::JoinHandle;

#[derive(Default)]
pub struct ServiceAccessReconciler {
    tasks: OnceLock<Vec<JoinHandle<()>>>,
}
impl ServiceAccessReconciler {
    /// Idempotent startup after all provider services have initialized.
    pub fn start(&self, coordinator: Arc<ServiceAccessCoordinator>) {
        self.tasks.get_or_init(|| {
            let mut tasks = Vec::with_capacity(3);
            let scheduler = Arc::clone(&coordinator);
            tasks.push(tokio::spawn(async move {
                loop {
                    match tokio::time::timeout(Duration::from_secs(12), scheduler.maintain()).await {
                        Ok(Ok(_)) => {},
                        Ok(Err(error)) => tracing::warn!(%error, "Cannot schedule sandbox service-access reconciliation"),
                        Err(_) => tracing::warn!("Sandbox service-access maintenance deadline exceeded"),
                    }
                    scheduler.wait_for_work().await;
                }
            }));
            for dispatcher in 0..2 {
                let coordinator = Arc::clone(&coordinator);
                tasks.push(tokio::spawn(async move {
                    loop {
                        match tokio::time::timeout(Duration::from_secs(25), coordinator.dispatch_one()).await {
                            Ok(Ok(Some(_))) => continue,
                            Ok(Ok(None)) => {},
                            Ok(Err(error)) => tracing::warn!(dispatcher, %error, "Sandbox service-access delivery failed"),
                            Err(_) => tracing::warn!(dispatcher, "Sandbox service-access dispatch deadline exceeded; claim remains recoverable"),
                        }
                        coordinator.wait_for_work().await;
                    }
                }));
            }
            tasks
        });
    }
}
impl Drop for ServiceAccessReconciler {
    fn drop(&mut self) {
        if let Some(tasks) = self.tasks.get_mut() {
            for task in tasks {
                task.abort();
            }
        }
    }
}
