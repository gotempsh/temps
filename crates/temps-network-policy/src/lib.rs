// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Network policy for every workload kind (ADR-052 §4, ADR-055 phase 7).
//!
//! * `network_policy_rules`: link rules (written by database triggers when a
//!   service is linked to a project or a sandbox) and operator allow/deny
//!   rules (written through the API).
//! * `network_policy_profiles`: a subject's egress/ingress profile when it
//!   differs from its kind's default.
//! * `nodes.network_policy_mode` (`control_plane_node_settings` for node 0):
//!   `legacy` (default, today's behaviour), `shadow`, `enforce`.
//! * The node-0 enforcer: `temps-workload-netpol`'s [`NodeEnforcer`] with the
//!   database as its policy source.
//!
//! [`NodeEnforcer`]: temps_workload_netpol::NodeEnforcer

pub mod error;
pub mod handlers;
pub mod model;
pub mod plugin;
pub mod service;
pub mod source;

pub use error::NetworkPolicyError;
pub use handlers::NetworkPolicyApiDoc;
pub use plugin::NetworkPolicyPlugin;
pub use service::NetworkPolicyService;
pub use source::DbPolicySource;
