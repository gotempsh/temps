// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The small vocabulary shared by `temps-ai-cli`, `temps-sandbox-providers` and
//! `temps-ai-agents`. It sits below all three, so neither the AI CLI code nor
//! the sandbox code has to depend on the other, or on the agent services and
//! everything they need.

pub mod error;
pub mod run_status;
pub mod sandbox_user;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Callback invoked for each line of AI CLI output (for real-time streaming)
pub type OnEventCallback =
    Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;
