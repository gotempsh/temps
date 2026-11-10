// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Mutex;

#[tokio::test(start_paused = true)]
async fn healthy_delivery_passes_eight_stalled_workers_before_lease_expiry() {
    let budget = DeliveryBudget::default();
    let next = AtomicUsize::new(0);
    let healthy_at = Mutex::new(None);
    let start = tokio::time::Instant::now();
    // The same fixed number of dispatchers used by the reconciler. Failed
    // attempts leave the immediate queue (durable retries have backoff).
    let dispatch = || async {
        loop {
            let worker = next.fetch_add(1, Ordering::SeqCst);
            if worker > 8 {
                return;
            }
            let result = budget
                .run("fixture", "apply", 10, async {
                    if worker < 8 {
                        std::future::pending::<()>().await;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    *healthy_at.lock().await = Some(start.elapsed());
                    Ok(())
                })
                .await;
            assert_eq!(result.is_ok(), worker == 8);
        }
    };
    // Scoped futures avoid spawning borrowed state; the assertion also guards
    // drift between this regression and the production concurrency limit.
    assert_eq!(DISPATCH_CONCURRENCY, 4);
    tokio::join!(dispatch(), dispatch(), dispatch(), dispatch());
    assert!(healthy_at.lock().await.unwrap() < Duration::from_secs(4));
    assert_eq!(budget.slots.available_permits(), DISPATCH_CONCURRENCY);
    assert_eq!(budget.slow.available_permits(), 2);
    // All outstanding slow operations also end at their total deadline.
    assert_eq!(start.elapsed(), Duration::from_secs(10));
}

#[tokio::test(start_paused = true)]
async fn slow_allowance_preserves_long_calls_and_cancellation_releases_capacity() {
    let budget = DeliveryBudget::default();
    let mut operations = Box::pin(async {
        let slow = || {
            budget.run("fixture", "apply", 10, async {
                tokio::time::sleep(Duration::from_secs(3)).await;
                Ok(())
            })
        };
        tokio::join!(slow(), slow())
    });
    tokio::select! {
        _ = &mut operations => panic!("calls completed early"),
        _ = tokio::time::sleep(Duration::from_secs(2)) => {},
    }
    assert_eq!(budget.slow.available_permits(), 0);
    // Other slow calls cannot steal the remaining dispatcher slots for ten
    // seconds. Fast calls still work, and cancelled owners return all permits.
    let started = tokio::time::Instant::now();
    assert!(matches!(
        budget
            .run::<()>("fixture", "retire", 5, std::future::pending())
            .await,
        Err(TransportError::Timeout { seconds: 1, .. })
    ));
    assert_eq!(started.elapsed(), Duration::from_secs(1));
    budget
        .run("healthy", "apply", 10, async { Ok(()) })
        .await
        .unwrap();
    drop(operations);
    assert_eq!(budget.slots.available_permits(), DISPATCH_CONCURRENCY);
    assert_eq!(budget.slow.available_permits(), 2);
    let slow = || {
        budget.run("fixture", "apply", 10, async {
            tokio::time::sleep(Duration::from_secs(3)).await;
            Ok(())
        })
    };
    let (a, b) = tokio::join!(slow(), slow());
    a.unwrap();
    b.unwrap();
}
