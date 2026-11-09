// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

pub(crate) const DISPATCH_CONCURRENCY: usize = 4;

/// Four RPCs, but only two may wait beyond one second. This reserves progress
/// for new workers even when the slow allowance is occupied by first attempts.
/// Excess slow calls are cancelled and retried by the durable queue. Separate
/// inspection admission prevents an unavailable worker from starving delivery.
pub(super) struct DeliveryBudget {
    slots: Semaphore,
    slow: Semaphore,
}

impl Default for DeliveryBudget {
    fn default() -> Self {
        Self {
            slots: Semaphore::new(DISPATCH_CONCURRENCY),
            slow: Semaphore::new(2),
        }
    }
}

impl DeliveryBudget {
    pub(super) async fn run<T>(
        &self,
        sandbox_id: &str,
        operation: &'static str,
        seconds: u64,
        work: impl Future<Output = Result<T, TransportError>>,
    ) -> Result<T, TransportError> {
        let _slot = self.slots.try_acquire().map_err(|_| TransportError::Busy {
            sandbox_id: sandbox_id.into(),
            operation,
        })?;
        let started = tokio::time::Instant::now();
        tokio::pin!(work);
        if let Ok(result) = tokio::time::timeout(Duration::from_secs(1), &mut work).await {
            return result;
        }
        let timeout = |seconds| TransportError::Timeout {
            sandbox_id: sandbox_id.into(),
            operation,
            seconds,
        };
        let _slow = self.slow.try_acquire().map_err(|_| timeout(1))?;
        tokio::time::timeout_at(started + Duration::from_secs(seconds), work)
            .await
            .map_err(|_| timeout(seconds))?
    }
}

#[cfg(test)]
mod tests;
