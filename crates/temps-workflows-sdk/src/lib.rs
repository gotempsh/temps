// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Durable steps, sleeps, events and child workflows for Rust applications.
//! Step bodies must be idempotent: effects are at least once, journal results
//! exactly once. Mount [`Executor::router`] in the application's HTTP server.
mod client;
mod executor;
mod runtime;
mod wire;
pub use client::{Client, HttpTransport, Transport};
pub use executor::{Executor, Workflow};
pub use runtime::{Context, StepOptions};
pub use wire::*;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Workflow API operation {operation} failed with HTTP {status}")]
    Api { operation: String, status: u16 },
    #[error("Invalid workflow configuration: {reason}")]
    Configuration { reason: String },
    #[error("Workflow step '{name}' failed: {message}")]
    Step { name: String, message: String },
    #[error("Workflow step '{name}' failed: {message}")]
    RecordedStep {
        sequence: i32,
        name: String,
        message: String,
    },
    #[error("Workflow invocation suspended or fenced")]
    Control,
    #[error("Invalid workflow invocation token")]
    Authentication,
    #[error("Invalid JSON for workflow operation {operation}")]
    Encoding { operation: String },
}

#[cfg(test)]
mod tests;
