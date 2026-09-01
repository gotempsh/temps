// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Service layer for the standalone sandbox API. All business logic lives
//! here; HTTP handlers are thin wrappers that call into these services.

pub mod agent_run_bridge;
pub mod exec;
pub mod expiration_sweeper;
pub mod fs;
pub mod job_tracker;
pub mod preview_password;
pub mod preview_urls;
pub mod public_id;
pub mod registry;
pub mod sandbox_service;
pub mod snapshot_service;

pub use expiration_sweeper::SandboxExpirationSweeper;
pub use job_tracker::{Job, JobLogEvent, JobState, JobStatus, JobTracker};
pub use preview_urls::PreviewUrlParts;
pub use registry::StandaloneSandboxRegistry;
pub use sandbox_service::{CreateSandboxRequest, SandboxService, SandboxSource, SandboxSummary};
pub use snapshot_service::{SnapshotService, StorageSummary};
