// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Functions (ADR-055 §14, phase 10): upload a WASM module or name an OCI
//! image, and it is served on its own hostname as a `ScaleToZero` workload
//! that the activator wakes on the first request and puts back to sleep
//! when idle.

pub mod backend;
pub mod error;
pub mod handlers;
pub mod plugin;
pub mod service;

pub use backend::FunctionActivationBackend;
pub use error::FunctionError;
pub use handlers::{configure_routes, FunctionsApiDoc, FunctionsAppState};
pub use plugin::FunctionsPlugin;
pub use service::{
    digest_of, CreateFunction, FunctionArtifact, FunctionDomain, FunctionService, FunctionView,
    FunctionsCapability, SettingsDomain, FUNCTION_OWNER_KIND,
};
