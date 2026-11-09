// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Six owned tasks, no per-sandbox sidecars or unbounded task spawning. The
//! durable queue survives shutdown; worker leases expire without renewal.
use crate::{coordinator::ServiceAccessCoordinator, transport::DISPATCH_CONCURRENCY};
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
            let mut tasks = Vec::with_capacity(2 + DISPATCH_CONCURRENCY);
            let scheduler = Arc::clone(&coordinator);
            tasks.push(tokio::spawn(async move {
                loop {
                    match tokio::time::timeout(Duration::from_secs(12), scheduler.maintain()).await {
                        Ok(Ok(batch)) => {
                            wait_after_batch(batch.processed, scheduler.wait_for_work()).await;
                            continue;
                        },
                        Ok(Err(error)) => tracing::warn!(%error, "Cannot schedule sandbox service-access reconciliation"),
                        Err(_) => tracing::warn!("Sandbox service-access maintenance deadline exceeded"),
                    }
                    scheduler.wait_for_work().await;
                }
            }));
            let approvals = Arc::clone(&coordinator);
            tasks.push(tokio::spawn(async move {
                loop {
                    match tokio::time::timeout(Duration::from_secs(90), approvals.maintain_assignments()).await {
                        Ok(Ok(count)) => { wait_after_batch(count, tokio::time::sleep(Duration::from_secs(5))).await; continue; },
                        Ok(Err(error)) => tracing::warn!(%error, "Cannot maintain sandbox service approvals"),
                        Err(_) => tracing::warn!("Sandbox service approval maintenance timed out"),
                    }
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }));
            for dispatcher in 0..DISPATCH_CONCURRENCY {
                let coordinator = Arc::clone(&coordinator);
                tasks.push(tokio::spawn(async move {
                    loop {
                        match tokio::time::timeout(Duration::from_secs(25), coordinator.dispatch_next()).await {
                            Ok(Ok(Some(result))) => {
                                if let Err(error) = result {
                                    tracing::warn!(dispatcher, %error, "Sandbox service-access delivery failed; retry scheduled");
                                }
                                wait_after_batch(1, coordinator.wait_for_work()).await;
                                continue;
                            },
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

// Yield between bounded batches; only an empty queue needs the polling delay.
async fn wait_after_batch(count: usize, idle: impl std::future::Future<Output = ()>) {
    if count == 0 {
        idle.await;
    } else {
        tokio::task::yield_now().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn backlog_does_not_spend_short_grant_lifetimes_sleeping() {
        let start = tokio::time::Instant::now();
        for _ in 0..16 {
            wait_after_batch(8, tokio::time::sleep(Duration::from_secs(5))).await;
        }
        assert_eq!(start.elapsed(), Duration::ZERO);
        wait_after_batch(0, tokio::time::sleep(Duration::from_secs(5))).await;
        assert_eq!(start.elapsed(), Duration::from_secs(5));
    }
    #[tokio::test(start_paused = true)]
    async fn worker_backlog_drains_before_policy_leases_expire() {
        let start = tokio::time::Instant::now();
        let mut workers = 97;
        while workers > 0 {
            let processed = workers.min(16);
            workers -= processed;
            wait_after_batch(processed, tokio::time::sleep(Duration::from_secs(5))).await;
        }
        assert_eq!(start.elapsed(), Duration::ZERO);
        wait_after_batch(0, tokio::time::sleep(Duration::from_secs(5))).await;
        assert_eq!(start.elapsed(), Duration::from_secs(5));
    }
}
