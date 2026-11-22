// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::{
    collections::HashMap,
    sync::{Arc, Weak},
};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

/// Shared by inspection, apply and retirement. The outer transport budgets
/// admit at most eight operations, bounding both waiters and live worker keys.
/// Weak entries are pruned on acquisition so historical nodes cannot accumulate.
#[derive(Default)]
pub(super) struct WorkerBudget {
    workers: Mutex<HashMap<Option<i32>, Weak<Semaphore>>>,
}

impl WorkerBudget {
    pub(super) async fn acquire(&self, node_id: Option<i32>) -> OwnedSemaphorePermit {
        let slots = {
            let mut workers = self.workers.lock().await;
            workers.retain(|_, slots| slots.strong_count() > 0);
            if let Some(slots) = workers.get(&node_id).and_then(Weak::upgrade) {
                slots
            } else {
                let slots = Arc::new(Semaphore::new(2));
                workers.insert(node_id, Arc::downgrade(&slots));
                slots
            }
        };
        // This private semaphore is never closed. Cancellation drops the
        // waiter or owned permit, releasing capacity without a cleanup task.
        #[allow(
            clippy::expect_used,
            reason = "private worker semaphores are never closed"
        )]
        slots
            .acquire_owned()
            .await
            .expect("worker semaphore remains open")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn worker_capacity_is_shared_isolated_and_released_on_cancellation() {
        let budget = WorkerBudget::default();
        let first = budget.acquire(Some(1)).await;
        let second = budget.acquire(Some(1)).await;
        // Cancel a waiter while both permits are held; it must not consume
        // the next permit or obstruct the next caller.
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(10),
            budget.acquire(Some(1))
        )
        .await
        .is_err());
        let queued = budget.acquire(Some(1));
        tokio::pin!(queued);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut queued)
                .await
                .is_err()
        );
        let other = budget.acquire(Some(2)).await;
        drop(first);
        let third = queued.await;
        drop((second, third, other));
        let _next = budget.acquire(Some(3)).await;
        assert_eq!(budget.workers.lock().await.len(), 1);
    }
}
