// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pure SDK translation. This module cannot sign tokens, open listeners or
//! activate saved intents. A future control-plane coordinator must reauthorize
//! fresh consent and resolve the current node/incarnation before calling it.
mod binding;
pub use binding::{PreparedBinding, MAX_BINDING_GRANTS};
mod policy;
pub use policy::compile_policy;
pub mod delivery;

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use tokenless_core::{
    ids::{NodeId, RuleId},
    rule::RuleScope,
    scope::Scope,
    session::SessionClaims,
};
use uuid::Uuid;

use crate::{error::AccessError, types::SandboxAccessPolicy};

/// The immutable SDK contract this adapter is compiled against. Upgrading the
/// dependency does not by itself enable live access.
pub const SDK_REVISION: &str = "03c94edcd6ec214164863540d7b372dac219b3ac";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationBlocker {
    HardStreamExpiry,
    IndependentGrantUpdates,
    RevocationAcknowledgement,
    TrustedHostTransport,
    FreshConsentCoordinator,
}

/// Deliberately no operator override: these are implementation requirements,
/// not configuration switches. Nothing reports connected while they remain.
pub const BLOCKERS: &[IntegrationBlocker] = &[
    IntegrationBlocker::HardStreamExpiry,
    IntegrationBlocker::IndependentGrantUpdates,
    IntegrationBlocker::RevocationAcknowledgement,
    IntegrationBlocker::TrustedHostTransport,
    IntegrationBlocker::FreshConsentCoordinator,
];

/// Host-resolved identity, never supplied by a sandbox header or guest IP.
/// A fresh incarnation UUID is required after every start/restore/move. Both
/// Docker and microVM transports must resolve this before attaching streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingTarget {
    pub project_id: Option<i32>,
    pub sandbox_id: i32,
    pub incarnation_id: Uuid,
    pub node_id: Uuid,
}

/// Inputs from a freshly authorized request. This is not an HTTP DTO and is
/// intentionally not deserializable; it is not evidence of authorization.
#[derive(Debug, Clone)]
pub struct PrepareSession {
    pub target: BindingTarget,
    pub grant_id: String,
    pub service_id: String,
    pub service_version: i32,
    pub policy: SandboxAccessPolicy,
    pub expires_at: DateTime<Utc>,
    pub max_ttl_seconds: i32,
}

#[derive(Debug, thiserror::Error)]
pub enum PreparationError {
    #[error("Cannot prepare Tokenless grant {grant_id}: {reason}")]
    Invalid {
        grant_id: String,
        reason: &'static str,
    },
    #[error("Cannot compile policy for Tokenless grant {grant_id}: {source}")]
    Policy {
        grant_id: String,
        source: AccessError,
    },
    #[error("Cannot prepare Tokenless session for grant {grant_id} during {operation}: {source}")]
    Sdk {
        grant_id: String,
        operation: &'static str,
        source: tokenless_core::CoreError,
    },
}

/// An unsigned, secret-free plan, not a readiness or enforcement receipt.
/// One rule per grant keeps later revocation independent of other grants to
/// the same service. The coordinator must persist/fence rule versions before
/// publishing snapshots; this plan makes no network or database changes.
#[derive(Debug)]
pub struct PreparedSession {
    target: BindingTarget,
    grant_id: String,
    delivery: Option<crate::types::CredentialDelivery>,
    rule: RuleScope,
    claims: SessionClaims,
}

fn parse_record_id(value: &str, prefix: &str) -> Option<Uuid> {
    let raw = value.strip_prefix(prefix)?;
    // The database generates a 32-character UUID in simple form.
    if raw.len() != 32 || !raw.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Uuid::parse_str(raw).ok().filter(|id| !id.is_nil())
}

impl PreparedSession {
    pub fn prepare(request: PrepareSession, now: DateTime<Utc>) -> Result<Self, PreparationError> {
        let invalid = |reason| PreparationError::Invalid {
            grant_id: request.grant_id.clone(),
            reason,
        };
        if request.target.project_id.is_some_and(|id| id <= 0)
            || request.target.sandbox_id <= 0
            || request.service_version <= 0
        {
            return Err(invalid(
                "sandbox ID, service version and any project ID must be positive",
            ));
        }
        if [request.target.incarnation_id, request.target.node_id]
            .iter()
            .any(Uuid::is_nil)
        {
            return Err(invalid(
                "incarnation and Tokenless node IDs must be non-nil",
            ));
        }
        let grant_uuid = parse_record_id(&request.grant_id, "grant_")
            .ok_or_else(|| invalid("grant ID must be a grant_ prefixed UUID"))?;
        if parse_record_id(&request.service_id, "svc_").is_none() {
            return Err(invalid("service ID must be a svc_ prefixed UUID"));
        }
        if !(60..=86400).contains(&request.max_ttl_seconds) {
            return Err(invalid(
                "service lifetime cap must be between 60 and 86400 seconds",
            ));
        }
        let remaining = request.expires_at.signed_duration_since(now);
        if remaining < chrono::Duration::seconds(1)
            || remaining > chrono::Duration::seconds(i64::from(request.max_ttl_seconds))
        {
            return Err(invalid(
                "grant must have at least one second remaining and fit the service lifetime cap",
            ));
        }
        // Floor, never round up: preparation and later renewal cannot extend
        // the absolute deadline. This does not claim the SDK cuts open streams.
        let ttl = u64::try_from(remaining.num_seconds())
            .map_err(|_| invalid("remaining lifetime cannot be represented"))?;
        let (policy, rule) =
            compile_policy(&request.policy).map_err(|source| PreparationError::Policy {
                grant_id: request.grant_id.clone(),
                source,
            })?;
        let sdk = |operation, source| PreparationError::Sdk {
            grant_id: request.grant_id.clone(),
            operation,
            source,
        };
        let scope_name = match request.target.project_id {
            Some(id) => format!("temps:project:{id}"),
            None => format!("temps:sandbox:{}", request.target.sandbox_id),
        };
        let scope = Scope::parse(&scope_name).map_err(|source| sdk("scope compilation", source))?;
        let binding_id = request.target.incarnation_id.to_string();
        let labels = BTreeMap::from([
            ("grant_id".into(), request.grant_id.to_string()),
            ("service_id".into(), request.service_id.to_string()),
            (
                "service_version".into(),
                request.service_version.to_string(),
            ),
        ]);
        let claims = SessionClaims::new(
            scope,
            vec![RuleId(grant_uuid)],
            ttl,
            None,
            labels,
            now,
            86400,
        )
        .map_err(|source| sdk("claims compilation", source))?
        .strict()
        .for_node(NodeId(request.target.node_id))
        // The incarnation is already part of the binding ID, so generation
        // is fixed within that incarnation; it is never reused on restart.
        .for_binding(&binding_id, 1)
        .map_err(|source| sdk("binding compilation", source))?;
        Ok(Self {
            target: request.target,
            grant_id: request.grant_id.clone(),
            delivery: policy.delivery,
            rule,
            claims,
        })
    }

    pub fn target(&self) -> &BindingTarget {
        &self.target
    }
    pub fn grant_id(&self) -> &str {
        &self.grant_id
    }
    /// How this grant's credential is sent; `None` is a plain bearer token.
    pub fn delivery(&self) -> Option<&crate::types::CredentialDelivery> {
        self.delivery.as_ref()
    }
    pub fn rule(&self) -> &RuleScope {
        &self.rule
    }
    pub fn claims(&self) -> &SessionClaims {
        &self.claims
    }
}

/// A revocation addresses the same host-resolved incarnation as provisioning.
/// Returning from an eventual runtime implementation must mean enforcement,
/// not merely that a message was queued or a database row was changed.
#[derive(Debug, Clone)]
pub struct RevokeSession {
    pub target: BindingTarget,
    pub grant_id: String,
}

#[cfg(test)]
pub(crate) mod tests;
