// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Live build admission. Lowering a cap drains existing work without cancelling it.
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::Notify;

struct AdmissionState {
    active: u32,
    limit: u16,
    version: u64,
}
pub(crate) struct BuildAdmission {
    state: Mutex<AdmissionState>,
    changed: Notify,
}
impl Default for BuildAdmission {
    fn default() -> Self {
        Self {
            state: Mutex::new(AdmissionState {
                active: 0,
                limit: 1,
                version: 0,
            }),
            changed: Notify::new(),
        }
    }
}
impl BuildAdmission {
    fn state(&self) -> MutexGuard<'_, AdmissionState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
    pub(crate) fn concurrency_limit(&self) -> u32 {
        u32::from(self.state().limit)
    }
    pub(crate) fn serial(&self) -> bool {
        self.state().limit == 1
    }
    pub(crate) fn available(&self) -> u32 {
        let state = self.state();
        u32::from(state.limit).saturating_sub(state.active)
    }
    pub(crate) async fn acquire(self: &Arc<Self>, limit: u16, version: u64) -> BuildPermit {
        {
            let mut state = self.state();
            // Legacy requests may update a legacy worker until the first
            // versioned setting arrives. Equal revisions cannot change a cap.
            if version > state.version || (version == 0 && state.version == 0) {
                state.limit = limit;
                state.version = version;
            }
        }
        self.changed.notify_waiters();
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                // Config publication and slot admission share one short control-
                // plane lock, so lowering a cap cannot race an old admission.
                let mut state = self.state();
                if state.active < u32::from(state.limit) {
                    state.active += 1;
                    return BuildPermit(self.clone());
                }
            }
            notified.await;
        }
    }
}
pub(crate) struct BuildPermit(Arc<BuildAdmission>);
impl Drop for BuildPermit {
    fn drop(&mut self) {
        self.0.state().active -= 1;
        self.0.changed.notify_waiters();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn lowering_cap_drains_and_cancelled_waiters_do_not_leak() {
        let gate = Arc::new(BuildAdmission::default());
        let a = gate.acquire(2, 1).await;
        let b = gate.acquire(2, 1).await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), gate.acquire(1, 2))
                .await
                .is_err()
        );
        drop(a);
        assert_eq!(gate.available(), 0);
        drop(b);
        let c = gate.acquire(1, 2).await;
        assert_eq!(gate.available(), 0);
        drop(c);
        assert_eq!(gate.available(), 1);
        let _a = gate.acquire(3, 3).await;
        let _b = gate.acquire(3, 3).await;
        let _c = gate.acquire(3, 3).await;
        assert_eq!(gate.available(), 0);
    }
    #[tokio::test(start_paused = true)]
    async fn stale_and_legacy_requests_cannot_restore_a_lowered_cap() {
        let gate = Arc::new(BuildAdmission::default());
        let active = gate.acquire(8, 10).await;
        for (cap, version) in [(1, 11), (8, 10), (8, 0), (8, 11)] {
            assert!(tokio::time::timeout(
                std::time::Duration::from_secs(1),
                gate.acquire(cap, version)
            )
            .await
            .is_err());
            assert_eq!(gate.concurrency_limit(), 1);
        }
        drop(active);
        let a = gate.acquire(8, 10).await;
        assert_eq!(gate.available(), 0);
        let b = gate.acquire(2, 12).await;
        assert_eq!(gate.concurrency_limit(), 2);
        drop((a, b));
        assert_eq!(gate.available(), 2);
    }
    #[tokio::test(start_paused = true)]
    async fn legacy_cap_updates_work_only_until_versioned_settings_arrive() {
        let gate = Arc::new(BuildAdmission::default());
        let a = gate.acquire(3, 0).await;
        let b = gate.acquire(2, 0).await;
        assert_eq!(gate.concurrency_limit(), 2);
        drop((a, b));
        let active = gate.acquire(1, 1).await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), gate.acquire(8, 0))
                .await
                .is_err()
        );
        assert_eq!(gate.concurrency_limit(), 1);
        drop(active);
    }
}
