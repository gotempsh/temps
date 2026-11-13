// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What every managed-service engine builds on: the `ExternalService` trait and
//! its types, `EngineError`, and the machinery engines share (container
//! recreation, restore staging, data import, the workload spec).
//!
//! The engines and the manager that dispatches to them sit above this crate.

#![cfg_attr(
    any(test, feature = "test-support"),
    allow(
        clippy::disallowed_methods,
        reason = "tests read fixture configuration (database/service URLs) from the environment"
    )
)]
pub mod data_import;
pub mod externalsvc;
pub mod healthcheck;
pub mod query;
pub mod readiness;
pub mod remote_params;
pub mod sandbox_namespace;
pub mod service_errors;
pub mod utils;
pub mod workload;
