// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Host-only delivery DTOs. Never serve these through a guest-facing API.
use crate::{RuntimeError, WorkerAuthority, MAX_SNAPSHOT_BYTES};
use serde::{Deserialize, Serialize};
use tokenless_core::{
    audit::{AuditEvent, AuditOutcome},
    snapshot::SignedSnapshot,
};
use tokenless_dataplane::{audit::AuditShipper, BindingSpec, BindingUpdate, DataPlaneError};

#[derive(Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PolicyEnvelope {
    pub body: String,
    pub signature: String,
    pub key_id: String,
}

impl std::fmt::Debug for PolicyEnvelope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PolicyEnvelope")
            .field("body", &"[redacted]")
            .finish_non_exhaustive()
    }
}

impl From<SignedSnapshot> for PolicyEnvelope {
    fn from(value: SignedSnapshot) -> Self {
        Self {
            body: value.body,
            signature: value.signature,
            key_id: value.kid,
        }
    }
}

impl PolicyEnvelope {
    pub fn into_snapshot(self) -> Result<SignedSnapshot, RuntimeError> {
        if self.body.len() > MAX_SNAPSHOT_BYTES
            || self.signature.len() > 128
            || self.key_id.len() > 128
        {
            return Err(RuntimeError::Invalid {
                operation: "decode delivery",
                binding: "worker".into(),
                reason: "policy envelope exceeds delivery limits",
            });
        }
        Ok(SignedSnapshot {
            body: self.body,
            signature: self.signature,
            kid: self.key_id,
        })
    }
}

/// One grouped, strict session. Signature and scope are verified by the runtime;
/// the provider supplies the binding identity, never this payload.
#[derive(Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionDelivery {
    pub authority: WorkerAuthority,
    pub policy: PolicyEnvelope,
    #[schema(write_only)]
    pub session: String,
    pub version: u64,
}

impl std::fmt::Debug for SessionDelivery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionDelivery")
            .field("authority", &self.authority)
            .field("version", &self.version)
            .field("session", &"[redacted]")
            .finish_non_exhaustive()
    }
}

impl SessionDelivery {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self.version == 0
            || self.session.is_empty()
            || self.session.len() > 32 * 1024
            || self.policy.body.len() > MAX_SNAPSHOT_BYTES
            || self.policy.signature.len() > 128
            || self.policy.key_id.len() > 128
        {
            return Err(RuntimeError::Invalid {
                operation: "validate delivery",
                binding: self.authority.boot_id.to_string(),
                reason:
                    "delivery requires a positive version, a bounded session and policy envelope",
            });
        }
        Ok(())
    }
    pub fn binding_spec(id: String, session: String, version: u64) -> BindingSpec {
        BindingSpec {
            binding_id: id,
            generation: 1,
            sessions: vec![session],
            version,
            max_connections: crate::MAX_SANDBOX_CONNECTIONS,
        }
    }
    pub fn binding_update(session: String, version: u64) -> BindingUpdate {
        BindingUpdate {
            version,
            sessions: vec![session],
        }
    }
}

/// SDK audit batches are drained off the request path. Record only trusted
/// outcome counts here; request metadata and credential material never enter
/// worker logs. User grant mutations have separate control-plane audit records.
pub struct HostAudit;
#[async_trait::async_trait]
impl AuditShipper for HostAudit {
    async fn ship(&self, events: &[AuditEvent]) -> Result<(), DataPlaneError> {
        let (mut success, mut denied, mut failed) = (0_u32, 0_u32, 0_u32);
        for event in events {
            match event.outcome {
                AuditOutcome::Success => success = success.saturating_add(1),
                AuditOutcome::Denied => denied = denied.saturating_add(1),
                AuditOutcome::Failure => failed = failed.saturating_add(1),
            }
        }
        tracing::info!(
            success,
            denied,
            failed,
            "Sandbox credential proxy audit batch"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn delivery() -> SessionDelivery {
        SessionDelivery {
            authority: WorkerAuthority {
                boot_id: uuid::Uuid::new_v4(),
                node_id: uuid::Uuid::new_v4(),
                snapshot_verify_key: [1; 32],
            },
            policy: PolicyEnvelope {
                body: "sealed-policy-sentinel".into(),
                signature: "signature-sentinel".into(),
                key_id: "key".into(),
            },
            session: "session-sentinel".into(),
            version: 1,
        }
    }
    #[test]
    fn delivery_debug_never_formats_policy_or_session() {
        let input = delivery();
        let debug = format!("{input:?} {:?}", input.policy);
        for secret in [&input.session, &input.policy.body, &input.policy.signature] {
            assert!(!debug.contains(secret));
        }
    }
    #[test]
    fn delivery_limits_reject_empty_zero_and_oversized_values() {
        delivery().validate().unwrap();
        for variant in 0..6 {
            let mut input = delivery();
            match variant {
                0 => input.version = 0,
                1 => input.session.clear(),
                2 => input.session = "x".repeat(32 * 1024 + 1),
                3 => input.policy.body = "x".repeat(MAX_SNAPSHOT_BYTES + 1),
                4 => input.policy.signature = "x".repeat(129),
                _ => input.policy.key_id = "x".repeat(129),
            }
            assert!(input.validate().is_err(), "variant {variant}");
            if variant >= 3 {
                assert!(input.policy.into_snapshot().is_err());
            }
        }
        let mut boundary = delivery();
        boundary.session = "x".repeat(32 * 1024);
        boundary.policy.body = "x".repeat(MAX_SNAPSHOT_BYTES);
        boundary.policy.signature = "x".repeat(128);
        boundary.policy.key_id = "x".repeat(128);
        boundary.validate().unwrap();
        boundary.policy.into_snapshot().unwrap();
    }
}
