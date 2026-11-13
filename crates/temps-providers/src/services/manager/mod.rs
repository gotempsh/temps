// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `ExternalServiceManager` methods, grouped by concern. Each submodule adds
//! an `impl ExternalServiceManager` block.

mod backup_support;
mod cluster_auth;
mod cluster_backup;
mod cluster_lifecycle;
mod cluster_members;
mod cluster_provisioning;
mod crud;
mod env_vars;
mod health;
mod healthcheck;
#[cfg(test)]
pub(crate) use healthcheck::HEALTHCHECK_REGISTRY_TEST_LOCK;
pub(crate) use healthcheck::{HEALTHCHECK_CHANNEL, HEALTHCHECK_WRITES};
mod import;
mod lifecycle;
mod managed_s3;
mod networking;
mod persistence;
mod project_links;
mod readiness;
mod remote;
mod runtime_limits;
mod server_tls;
mod stats_poll;
mod wal_health;

pub use healthcheck::{
    service_overrides, HealthcheckApplied, HealthcheckCapability, ServiceHealthcheck,
    ServiceHealthcheckUpdate,
};
pub(super) use server_tls::ServiceTlsLocks;
pub use stats_poll::{
    cpu_utilization_percent, ContainerStatsBaseline, CpuLimit, PolledContainerStats,
    CPU_LIMIT_REFRESH,
};
pub(crate) use wal_health::record_wal_outcome;
pub use wal_health::{WalHealthReading, WalHealthUnchecked, WalProbeOutcome};
