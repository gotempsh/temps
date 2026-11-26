// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Ownership of the gateway's recurring background workers.

use tokio::task::JoinHandle;

/// The gateway's recurring workers (the model catalog refresher, the
/// coding-agent updater), aborted when this is dropped.
///
/// The plugin holds them while it initializes, so a later initialization
/// step that fails stops them with it instead of leaving loops that keep the
/// database and services alive; once initialization succeeds the plugin
/// registers this in its context, which keeps them running for as long as
/// the registered services exist.
#[derive(Default)]
pub struct GatewayBackgroundTasks {
    handles: Vec<JoinHandle<()>>,
}

impl GatewayBackgroundTasks {
    pub fn push(&mut self, handle: JoinHandle<()>) {
        self.handles.push(handle);
    }

    pub fn len(&self) -> usize {
        self.handles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.handles.is_empty()
    }
}

impl Drop for GatewayBackgroundTasks {
    fn drop(&mut self) {
        for handle in &self.handles {
            handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn dropping_the_tasks_stops_every_worker() {
        let alive = Arc::new(());
        let mut tasks = GatewayBackgroundTasks::default();
        for _ in 0..2 {
            let held = alive.clone();
            tasks.push(tokio::spawn(async move {
                let _held = held;
                loop {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }));
        }
        assert_eq!(tasks.len(), 2);
        assert_eq!(Arc::strong_count(&alive), 3);

        drop(tasks);
        // An aborted task drops its future, and with it what it held.
        tokio::time::timeout(Duration::from_secs(5), async {
            while Arc::strong_count(&alive) > 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("both workers stopped");
    }
}
