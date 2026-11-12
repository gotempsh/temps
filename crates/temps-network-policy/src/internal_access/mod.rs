// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Internal access grants (ADR 101): which other projects, or environments of
//! them, may call a project's `*.temps.local` addresses through the internal
//! proxy.
//!
//! Stored in `internal_access_grants`, not in `network_policy_rules`: rules are
//! instance-admin L3/L4 policy over workloads, grants are project-owned L7
//! permissions over projects and environments (ADR 101 D1). The route sync
//! (`temps_routes::internal_access_grants`) carries them to workers.

pub mod error;
pub mod handlers;
pub mod service;

pub use error::InternalAccessError;
pub use handlers::{InternalAccessApiDoc, InternalAccessAppState};
pub use service::InternalAccessService;
