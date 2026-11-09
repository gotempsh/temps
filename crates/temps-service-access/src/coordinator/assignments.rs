// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::{future::Future, time::Duration};

// At most eight claimed sandbox groups, two in flight, three seconds each:
// network failures consume at most twelve seconds of a renewal sweep. Each
// group shares one inspection; cancelled database transactions roll back.
pub(super) async fn run_pairs<T, F, Fut>(groups: Vec<T>, renew: F)
where
    F: Fn(T) -> Fut,
    Fut: Future<Output = ()>,
{
    let renew = &renew;
    let bounded = |group| async move {
        if tokio::time::timeout(Duration::from_secs(3), renew(group))
            .await
            .is_err()
        {
            tracing::warn!("Sandbox service approval renewal exceeded its three-second budget; retrying on the next due claim");
        }
    };
    let mut groups = groups.into_iter();
    while let Some(first) = groups.next() {
        if let Some(second) = groups.next() {
            tokio::join!(bounded(first), bounded(second));
        } else {
            bounded(first).await;
        }
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
        run_pairs((0..8).collect(), |id| async move {
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
        assert_eq!(peak.load(Ordering::SeqCst), 2);
        assert!(start.elapsed() <= Duration::from_secs(12));
    }
}
