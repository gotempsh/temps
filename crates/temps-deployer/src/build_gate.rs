// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Build concurrency gate whose limit can change while builds are running.
//!
//! A `tokio::sync::Semaphore` cannot be resized: permits can be added, but
//! taking them back means acquiring them, which queues behind (and blocks)
//! every later build. [`BuildGate`] keeps the limit and the number of running
//! builds as plain counters instead, so:
//!
//! * raising the limit starts queued builds immediately;
//! * lowering it never touches a running build — the gate simply admits
//!   nobody until enough of them finish to get under the new limit;
//! * waiting builds are served in arrival order, whatever the limit does;
//! * the running count is exact, for the "other builds in flight" diagnostics.
//!
//! The gate sits on the control-plane build path (one acquire per build), so
//! a short `std::sync::Mutex` section is plenty: no await is ever held across
//! it, and the number of waiters is bounded by the number of queued builds.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::oneshot;

/// Admits at most `limit` holders of a [`BuildPermit`] at a time.
#[derive(Debug)]
pub struct BuildGate {
    state: Mutex<GateState>,
}

#[derive(Debug)]
struct GateState {
    /// Maximum permits outstanding. Never 0: a zero limit would block every
    /// build forever.
    limit: usize,
    /// Permits currently held. May exceed `limit` right after the limit was
    /// lowered, until enough holders finish.
    running: usize,
    /// Builds waiting for a permit, oldest first.
    waiters: VecDeque<oneshot::Sender<BuildPermit>>,
}

/// Proof that a build holds a slot; the slot is released when this is dropped,
/// whether the build succeeded, failed, or was cancelled.
#[derive(Debug)]
pub struct BuildPermit {
    gate: Arc<BuildGate>,
}

impl Drop for BuildPermit {
    fn drop(&mut self) {
        self.gate.release();
    }
}

impl BuildGate {
    /// A gate admitting `limit` builds at once. `0` is raised to 1.
    pub fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(GateState {
                limit: limit.max(1),
                running: 0,
                waiters: VecDeque::new(),
            }),
        })
    }

    fn lock(&self) -> MutexGuard<'_, GateState> {
        // The counters stay consistent across a panic elsewhere (every
        // critical section is a few integer updates), so a poisoned lock is
        // still safe to use.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Current limit.
    pub fn limit(&self) -> usize {
        self.lock().limit
    }

    /// Builds currently holding a permit.
    pub fn running(&self) -> usize {
        self.lock().running
    }

    /// Builds currently waiting for a permit. A build whose future was
    /// dropped is not counted, even though its entry stays in the queue until
    /// the next grant sweeps it out.
    pub fn queued(&self) -> usize {
        self.lock()
            .waiters
            .iter()
            .filter(|waiter| !waiter.is_closed())
            .count()
    }

    /// Whether a build arriving now would have to wait. Waiters only exist
    /// while the gate is at its limit (every release and every limit change
    /// grants to them first), so the running count alone answers it.
    pub fn is_full(&self) -> bool {
        let state = self.lock();
        state.running >= state.limit
    }

    /// Change the limit, returning the previous one. `0` is raised to 1.
    ///
    /// Raising it admits queued builds right away. Lowering it leaves running
    /// builds alone; new builds wait until the running count drops below the
    /// new limit.
    pub fn set_limit(self: &Arc<Self>, limit: usize) -> usize {
        let (previous, grants) = {
            let mut state = self.lock();
            let previous = state.limit;
            state.limit = limit.max(1);
            (previous, self.grant_waiting(&mut state))
        };
        Self::deliver(grants);
        previous
    }

    /// Wait for a slot. Waiting builds are served oldest first.
    pub async fn acquire(self: &Arc<Self>) -> BuildPermit {
        loop {
            let receiver = {
                let mut state = self.lock();
                if state.waiters.is_empty() && state.running < state.limit {
                    state.running += 1;
                    return BuildPermit { gate: self.clone() };
                }
                let (sender, receiver) = oneshot::channel();
                state.waiters.push_back(sender);
                receiver
            };
            // The sender lives in `waiters`, which this gate (kept alive by
            // `self`) owns, so a closed channel is not expected; if it ever
            // happens the build just queues again rather than failing.
            if let Ok(permit) = receiver.await {
                return permit;
            }
        }
    }

    /// Hand slots to the oldest waiters while there is room. Builds that gave
    /// up waiting (their future was dropped) are skipped. The permits are
    /// returned, not sent: dropping one re-enters the gate, so sending must
    /// happen after the lock is released (see [`Self::deliver`]).
    fn grant_waiting(
        self: &Arc<Self>,
        state: &mut GateState,
    ) -> Vec<(oneshot::Sender<BuildPermit>, BuildPermit)> {
        let mut grants = Vec::new();
        while state.running < state.limit {
            let Some(sender) = state.waiters.pop_front() else {
                break;
            };
            if sender.is_closed() {
                continue;
            }
            state.running += 1;
            grants.push((sender, BuildPermit { gate: self.clone() }));
        }
        grants
    }

    /// Send granted permits. A waiter that vanished between the grant and now
    /// hands the permit back, and dropping it frees the slot for the next one.
    fn deliver(grants: Vec<(oneshot::Sender<BuildPermit>, BuildPermit)>) {
        for (sender, permit) in grants {
            drop(sender.send(permit));
        }
    }

    fn release(self: &Arc<Self>) {
        let grants = {
            let mut state = self.lock();
            state.running = state.running.saturating_sub(1);
            self.grant_waiting(&mut state)
        };
        Self::deliver(grants);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Long enough for a task that can run to have run, short enough to keep
    /// the suite fast. Used to assert that something did NOT happen.
    const SETTLE: Duration = Duration::from_millis(50);

    async fn expect_pending<F: std::future::Future>(future: F) {
        assert!(
            tokio::time::timeout(SETTLE, future).await.is_err(),
            "expected the build to still be waiting for a slot"
        );
    }

    #[test]
    fn zero_limit_is_raised_to_one() {
        let gate = BuildGate::new(0);
        assert_eq!(gate.limit(), 1);
        assert_eq!(gate.set_limit(0), 1);
        assert_eq!(gate.limit(), 1);
    }

    #[tokio::test]
    async fn admits_up_to_the_limit_then_queues() {
        let gate = BuildGate::new(2);
        let first = gate.acquire().await;
        let _second = gate.acquire().await;
        assert_eq!(gate.running(), 2);
        assert!(gate.is_full());

        expect_pending(gate.acquire()).await;
        // The abandoned attempt must not leave a slot reserved.
        drop(first);
        assert_eq!(gate.running(), 1);
        let _third = gate.acquire().await;
        assert_eq!(gate.running(), 2);
    }

    #[tokio::test]
    async fn raising_the_limit_starts_queued_builds() {
        let gate = BuildGate::new(1);
        let _running = gate.acquire().await;

        let waiting = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.acquire().await })
        };
        tokio::time::sleep(SETTLE).await;
        assert_eq!(gate.queued(), 1);
        assert!(!waiting.is_finished());

        assert_eq!(gate.set_limit(2), 1);
        let permit = tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .expect("queued build proceeds once the limit is raised")
            .expect("task joins");
        assert_eq!(gate.running(), 2);
        assert_eq!(gate.queued(), 0);
        drop(permit);
    }

    #[tokio::test]
    async fn lowering_the_limit_keeps_running_builds_and_gates_new_ones() {
        let gate = BuildGate::new(3);
        let a = gate.acquire().await;
        let b = gate.acquire().await;
        let c = gate.acquire().await;

        assert_eq!(gate.set_limit(1), 3);
        // Nothing was revoked.
        assert_eq!(gate.running(), 3);
        assert_eq!(gate.limit(), 1);

        let waiting = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.acquire().await })
        };

        // Still above the new limit after one finishes, and after two.
        drop(a);
        tokio::time::sleep(SETTLE).await;
        assert!(!waiting.is_finished());
        drop(b);
        tokio::time::sleep(SETTLE).await;
        assert!(!waiting.is_finished());
        assert_eq!(gate.running(), 1);

        // Only when the last old build finishes is there room for a new one.
        drop(c);
        let permit = tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .expect("queued build proceeds once under the new limit")
            .expect("task joins");
        assert_eq!(gate.running(), 1);
        drop(permit);
        assert_eq!(gate.running(), 0);
    }

    #[tokio::test]
    async fn waiters_are_served_in_arrival_order() {
        let gate = BuildGate::new(1);
        let held = gate.acquire().await;

        let (order_tx, mut order_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut handles = Vec::new();
        for id in 0..3 {
            let task_gate = gate.clone();
            let order_tx = order_tx.clone();
            handles.push(tokio::spawn(async move {
                let permit = task_gate.acquire().await;
                let _ = order_tx.send(id);
                // Hold briefly so the next waiter cannot overtake.
                tokio::time::sleep(Duration::from_millis(10)).await;
                drop(permit);
            }));
            // Make arrival order deterministic: wait until this one is queued.
            while gate.queued() != id + 1 {
                tokio::task::yield_now().await;
            }
        }
        drop(order_tx);

        drop(held);
        for handle in handles {
            handle.await.expect("task joins");
        }
        let mut seen = Vec::new();
        while let Some(id) = order_rx.recv().await {
            seen.push(id);
        }
        assert_eq!(seen, vec![0, 1, 2]);
    }

    #[tokio::test]
    async fn a_cancelled_waiter_does_not_leak_a_slot() {
        let gate = BuildGate::new(1);
        let held = gate.acquire().await;

        let cancelled = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.acquire().await })
        };
        tokio::time::sleep(SETTLE).await;
        cancelled.abort();
        drop(cancelled.await);

        drop(held);
        assert_eq!(gate.running(), 0);
        // The slot is free for the next build rather than reserved for the
        // abandoned one.
        let permit = tokio::time::timeout(Duration::from_secs(5), gate.acquire())
            .await
            .expect("slot is free after the waiter gave up");
        assert_eq!(gate.running(), 1);
        drop(permit);
    }

    #[tokio::test]
    async fn raising_after_lowering_restores_capacity() {
        let gate = BuildGate::new(2);
        let _a = gate.acquire().await;
        let _b = gate.acquire().await;

        gate.set_limit(1);
        gate.set_limit(3);

        let _c = tokio::time::timeout(Duration::from_secs(5), gate.acquire())
            .await
            .expect("capacity is back after the limit is raised again");
        assert_eq!(gate.running(), 3);
    }
}
