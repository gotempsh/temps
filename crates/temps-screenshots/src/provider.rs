// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Screenshot Provider Trait
//!
//! Defines the interface for screenshot providers (local, remote, etc.)

use crate::error::ScreenshotResult;
use async_trait::async_trait;

/// Screenshot provider trait - implement this for different screenshot backends
#[async_trait]
pub trait ScreenshotProvider: Send + Sync {
    /// Capture a screenshot of the given URL and return the image bytes
    async fn capture_screenshot(&self, url: &str) -> ScreenshotResult<Vec<u8>>;

    /// Get the name of this provider (for logging/debugging)
    fn provider_name(&self) -> &'static str;

    /// Check if the provider is available/configured, returning *why* it is not.
    ///
    /// Implementations must surface an actionable reason (missing binary, missing
    /// shared libraries, unreachable remote service) rather than a bare boolean —
    /// self-hosted operators have no support channel and the reason is what ends
    /// up in the deployment job's error message.
    async fn check_availability(&self) -> ScreenshotResult<()>;

    /// Convenience wrapper over [`check_availability`](Self::check_availability)
    /// for call sites that only need a yes/no answer.
    async fn is_available(&self) -> bool {
        self.check_availability().await.is_ok()
    }
}
