// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The S3-compatible object storage engines: RustFS, Garage and legacy MinIO, with their backup, restore and bucket mirroring.

#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        reason = "tests read fixture configuration (database/service URLs) from the environment"
    )
)]
pub mod garage;
pub mod query;
pub mod rc_client;
pub mod rustfs;
pub mod s3;
pub(crate) mod s3_mirror;

// The engine code addresses the contract it implements through these paths.
pub(crate) use temps_providers_core::externalsvc::*;
#[allow(unused_imports, reason = "not every engine uses all of them")]
pub(crate) use temps_providers_core::{data_import, externalsvc, readiness, utils, workload};
