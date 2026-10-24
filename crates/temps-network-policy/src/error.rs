// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use thiserror::Error;

#[derive(Debug, Error)]
pub enum NetworkPolicyError {
    #[error("Network policy rule {rule_id} not found")]
    RuleNotFound { rule_id: i64 },

    #[error("Node {node_id} not found; node 0 is the control plane, other ids are `nodes` rows")]
    NodeNotFound { node_id: i32 },

    #[error("Invalid network policy rule: {message}")]
    Validation { message: String },

    #[error(
        "Rule {rule_id} was created by the {link_table} link {link_id} and is removed by \
         unlinking, not by deleting the rule"
    )]
    LinkOwned {
        rule_id: i64,
        link_table: String,
        link_id: i64,
    },

    #[error(
        "Cannot create the hostname rule for '{host}' on {subject}: hostname rules are enforced \
         only by the shared egress proxy, and no node runs it yet. Packet rules (nftables) \
         cannot match hostnames, so the rule would be accepted without being enforced. Use a \
         'cidr' rule for the destination's addresses, or the workload's egress profile, instead"
    )]
    HostRuleNotEnforceable { subject: String, host: String },

    #[error(
        "Cannot set node {node_id} to '{mode}': no network policy enforcer runs on worker nodes \
         yet, so the mode would be reported without any policy being computed or applied there. \
         Only the control plane (node 0) supports 'shadow' and 'enforce'; set worker nodes to \
         'legacy'"
    )]
    WorkerEnforcementUnavailable { node_id: i32, mode: String },

    #[error("Stored network policy rule {rule_id} is invalid: {reason}")]
    InvalidStoredRule { rule_id: i64, reason: String },

    #[error("Stored network policy profile for {subject} is invalid: {reason}")]
    InvalidStoredProfile { subject: String, reason: String },

    #[error("Network policy database operation '{operation}' failed: {source}")]
    Database {
        operation: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },
}

impl NetworkPolicyError {
    pub(crate) fn db(operation: &'static str) -> impl FnOnce(sea_orm::DbErr) -> Self {
        move |source| NetworkPolicyError::Database { operation, source }
    }

    pub(crate) fn validation(message: impl Into<String>) -> Self {
        NetworkPolicyError::Validation {
            message: message.into(),
        }
    }
}
