// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Recording a workload's outbound connections (opt-in, Linux nodes), for
//! every workload kind.
//!
//! Telemetry a workload sends about itself is optional and can be omitted
//! or falsified. The per-node egress proxy sees every outbound connection
//! that goes through it without the workload's cooperation, so when a
//! workload's owner turns recording on, the proxy records each connection
//! and this crate stores the records as OpenTelemetry log records owned by
//! the workload, and by its project when it has one.
//!
//! **Written once, for every kind.** A sandbox and an environment are
//! different products with their own tables, lifecycle and permissions,
//! and they stay that way. What they share is implemented here against the
//! workload identity ([`temps_workload::WorkloadRef`]):
//!
//! - **the opt-in flow** ([`EgressRecordingService::status`],
//!   [`EgressRecordingService::set_enabled`]): whether recording is on and
//!   whether it can work, refused with a typed error naming the workload
//!   and the node when it cannot;
//! - **ingest** ([`ingest`]): batches from a node, attributed by its proxy
//!   from the source address. The control plane trusts a node only for
//!   workloads placed on it and re-checks every opt-in, so a workload can
//!   neither forge nor suppress its records;
//! - **records** ([`records`]): one log record per connection, with OTel
//!   semantic-convention attributes, marked as platform-observed;
//! - **activity** ([`activity`]): one workload's records, newest first;
//! - **the node-0 recorder** ([`local`]): the control plane's own proxy for
//!   the recorded workloads that run on this host;
//! - **the node endpoint and the API types** ([`handlers`]).
//!
//! **What a kind provides** is one [`WorkloadEgressAdapter`]: where its
//! opt-in column is, which project owns it, which nodes run it, how it is
//! named. Authorization stays with the kind: its handlers resolve the
//! caller's workload in their own terms and hand this crate a
//! [`RecordedWorkload`]. A kind whose workloads run on the control plane
//! also provides a [`local::LocalTraffic`], which says how its traffic
//! reaches the recorder; one whose containers reach the network directly
//! steers them there with [`steering`].

pub mod activity;
pub mod adapter;
pub mod error;
pub mod handlers;
pub mod ingest;
pub mod local;
pub mod plugin;
pub mod records;
pub mod service;
pub mod sink;
pub mod steering;

#[cfg(test)]
pub(crate) mod tests;

pub use activity::{EgressActivityPage, EgressActivityRecord, EgressOutcomeFilter};
pub use adapter::{RecordedWorkload, WorkloadEgressAdapter};
pub use error::EgressRecordingError;
pub use ingest::EgressIngestOutcome;
pub use plugin::WorkloadEgressPlugin;
pub use service::{
    EgressLogStore, EgressRecordingNode, EgressRecordingService, EgressRecordingStatus,
    NodeRecordingCapabilities, CONTROL_PLANE_NODE_ID, NODES_SETUP_PATH,
};
