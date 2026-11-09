// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::{future::Future, time::Duration};

// At most eight claimed sandbox groups, four in flight. A group gets the
// transport's ten-second inspection allowance plus five seconds for database
// renewal. Two waves bound a sweep to thirty seconds; a reachable slow worker
// can complete instead of being cancelled at the same point on every retry.
pub(super) async fn run_groups<T, F, Fut>(groups: Vec<T>, renew: F)
where
    F: Fn(T) -> Fut,
    Fut: Future<Output = ()>,
{
    let renew = &renew;
    let bounded = |group| async move {
        if let Some(group) = group {
            if tokio::time::timeout(Duration::from_secs(15), renew(group))
                .await
                .is_err()
            {
                tracing::warn!("Sandbox service approval renewal exceeded its fifteen-second budget; retrying on the next due claim");
            }
        }
    };
    let mut groups = groups.into_iter();
    while let Some(first) = groups.next() {
        tokio::join!(
            bounded(Some(first)),
            bounded(groups.next()),
            bounded(groups.next()),
            bounded(groups.next()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test(start_paused = true)]
    async fn stalled_sandboxes_cannot_exhaust_healthy_renewal_window() {
        let start = tokio::time::Instant::now();
        let completed = AtomicUsize::new(0);
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        struct Active<'a>(&'a AtomicUsize);
        impl Drop for Active<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let (completed, active, peak) = (&completed, &active, &peak);
        run_groups((0..8).collect(), |id| async move {
            let current = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(current, Ordering::SeqCst);
            let _active = Active(active);
            if id < 7 {
                std::future::pending::<()>().await;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
            completed.fetch_add(1, Ordering::SeqCst);
        })
        .await;
        assert_eq!(completed.load(Ordering::SeqCst), 1);
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert_eq!(peak.load(Ordering::SeqCst), 4);
        assert!(start.elapsed() <= Duration::from_secs(30));
    }
    #[tokio::test(start_paused = true)]
    async fn reachable_slow_workers_can_finish_inspection_and_renewal() {
        let completed = AtomicUsize::new(0);
        let start = tokio::time::Instant::now();
        run_groups((0..8).collect(), |_| async {
            tokio::time::sleep(Duration::from_secs(4)).await;
            completed.fetch_add(1, Ordering::SeqCst);
        })
        .await;
        assert_eq!(completed.load(Ordering::SeqCst), 8);
        assert_eq!(start.elapsed(), Duration::from_secs(8));
    }
}
