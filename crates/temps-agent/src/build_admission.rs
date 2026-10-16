// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Live build admission. Lowering a cap drains existing work without cancelling it.
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::Notify;

pub(crate) struct BuildAdmission {
    active: AtomicUsize,
    limit: AtomicUsize,
    changed: Notify,
}
impl Default for BuildAdmission {
    fn default() -> Self {
        Self {
            active: AtomicUsize::new(0),
            limit: AtomicUsize::new(1),
            changed: Notify::new(),
        }
    }
}
impl BuildAdmission {
    pub(crate) fn concurrency_limit(&self) -> u32 {
        self.limit.load(Ordering::Acquire) as u32
    }
    pub(crate) fn serial(&self) -> bool {
        self.limit.load(Ordering::Acquire) == 1
    }
    pub(crate) fn available(&self) -> u32 {
        self.limit
            .load(Ordering::Acquire)
            .saturating_sub(self.active.load(Ordering::Acquire)) as u32
    }
    pub(crate) async fn acquire(self: &Arc<Self>, limit: u16) -> BuildPermit {
        self.limit.store(usize::from(limit), Ordering::Release);
        self.changed.notify_waiters();
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let active = self.active.load(Ordering::Acquire);
            if active < self.limit.load(Ordering::Acquire)
                && self
                    .active
                    .compare_exchange(active, active + 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                return BuildPermit(self.clone());
            }
            notified.await;
        }
    }
}
pub(crate) struct BuildPermit(Arc<BuildAdmission>);
impl Drop for BuildPermit {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
        self.0.changed.notify_waiters();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn lowering_cap_drains_and_cancelled_waiters_do_not_leak() {
        let gate = Arc::new(BuildAdmission::default());
        let a = gate.acquire(2).await;
        let b = gate.acquire(2).await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), gate.acquire(1))
                .await
                .is_err()
        );
        drop(a);
        assert_eq!(gate.available(), 0);
        drop(b);
        let c = gate.acquire(1).await;
        assert_eq!(gate.available(), 0);
        drop(c);
        assert_eq!(gate.available(), 1);
        let _a = gate.acquire(3).await;
        let _b = gate.acquire(3).await;
        let _c = gate.acquire(3).await;
        assert_eq!(gate.available(), 0);
    }
}
