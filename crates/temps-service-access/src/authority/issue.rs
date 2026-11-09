// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use crate::tokenless::PreparedBinding;
use std::collections::{BTreeMap, HashSet};
use temps_core::secret_string::SecretString;
use temps_service_access_runtime::{
    PolicyEnvelope, SessionDelivery, MAX_POLICY_LEASE_SECONDS, MAX_SNAPSHOT_BYTES,
    MAX_WORKER_BINDINGS,
};
use tokenless_core::{
    egress::EgressPolicy,
    ids::{CredentialId, NodeId},
    injection::{InjectedHeader, InjectedHeaders},
    seal::{seal, SealPublicKey},
    session::SessionSigner,
    snapshot::{
        ca_aad, format_for, rule_aad, InterceptionCa, PublishedKey, SignedSnapshot, SnapshotBody,
        SnapshotRule, UnmatchedHostPolicy,
    },
};

/// Server-only input. The coordinator must reauthorize the persisted grants,
/// resolve current service versions and decrypt their credentials before this
/// call. Neither this type nor PreparedBinding is a guest/API input.
#[derive(Debug)]
pub struct AuthorizedBinding {
    pub plan: PreparedBinding,
    /// Exact grant ID -> upstream bearer credential. No extra entries allowed.
    pub credentials: BTreeMap<String, SecretString>,
}

struct IssuedBinding {
    version: u64,
    session: SecretString,
    github_git: bool,
    grant_ids: Vec<String>,
}

/// One bounded snapshot shared by the worker's bindings. It deliberately has
/// no Serialize implementation: callers deliver one binding at a time, never
/// allocate 128 copies of a 4 MiB snapshot or expose sessions in a response.
pub struct IssuedPolicy {
    authority: WorkerAuthority,
    snapshot_version: u64,
    lease_expires_at: chrono::DateTime<chrono::Utc>,
    policy: PolicyEnvelope,
    bindings: BTreeMap<Uuid, IssuedBinding>,
}
impl std::fmt::Debug for IssuedPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedPolicy")
            .field("authority", &self.authority)
            .field("snapshot_version", &self.snapshot_version)
            .field("bindings", &self.bindings.len())
            .finish_non_exhaustive()
    }
}
impl IssuedPolicy {
    pub fn snapshot_version(&self) -> u64 {
        self.snapshot_version
    }
    /// Absolute signed lease deadline; receipt latency cannot extend it.
    pub fn lease_expires_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.lease_expires_at
    }
    pub(crate) fn binding_version(&self, incarnation: Uuid) -> Option<u64> {
        self.bindings.get(&incarnation).map(|b| b.version)
    }
    pub(crate) fn grant_ids(&self, incarnation: Uuid) -> &[String] {
        self.bindings
            .get(&incarnation)
            .map(|b| b.grant_ids.as_slice())
            .unwrap_or_default()
    }
    pub(crate) fn binding_count(&self) -> usize {
        self.bindings.len()
    }
    pub fn delivery(&self, incarnation: Uuid) -> Result<SessionDelivery, AuthorityError> {
        self.binding_delivery(incarnation, false)
    }
    pub(crate) fn binding_delivery(
        &self,
        incarnation: Uuid,
        reference: bool,
    ) -> Result<SessionDelivery, AuthorityError> {
        let binding = self.bindings.get(&incarnation).ok_or_else(|| {
            invalid(
                self.authority.boot_id,
                "select binding delivery",
                "incarnation is not included in this policy",
            )
        })?;
        Ok(SessionDelivery {
            authority: self.authority.clone(),
            policy: if reference {
                PolicyEnvelope {
                    body: String::new(),
                    signature: self.policy.signature.clone(),
                    key_id: self.policy.key_id.clone(),
                }
            } else {
                self.policy.clone()
            },
            version: binding.version,
            session: binding.session.expose().into(),
            github_git: binding.github_git,
        })
    }
}

impl AuthorityStore {
    /// All active bindings for ONE authenticated worker, from a consistent
    /// authorization read. `expected_revision` must be read in the same
    /// database snapshot as the grants, so concurrent revocation rejects a
    /// stale plan before it can reserve a newer signing version.
    /// Omitted rules disappear on installation. This signs
    /// policy, not a connected receipt; the dispatcher must verify worker ACKs.
    pub async fn issue(
        &self,
        node_id: Option<i32>,
        identity: &WorkerIdentity,
        expected_revision: i64,
        bindings: Vec<AuthorizedBinding>,
    ) -> Result<IssuedPolicy, AuthorityError> {
        validate_bindings(identity.boot_id, &bindings, chrono::Utc::now())?;
        let row = self.reserve(node_id, identity, expected_revision).await?;
        let authority = self.public_authority(&row, node_id, identity)?;
        if bindings
            .iter()
            .any(|binding| binding.plan.target().node_id != row.sdk_node_id)
        {
            return Err(invalid(
                row.id,
                "issue policy",
                "binding belongs to another Tokenless node",
            ));
        }
        let snapshot_key = self.signing_key(row.id, &row.snapshot_key_encrypted)?;
        let session_signer =
            SessionSigner::new(self.signing_key(row.id, &row.session_key_encrypted)?);
        let seal_key = SealPublicKey::from(identity.seal_public_key);
        let sdk_node = NodeId(row.sdk_node_id);
        let sdk = |operation, source| AuthorityError::Sdk {
            boot_id: row.id,
            operation,
            source,
        };
        let ca_key = self.decrypt(row.id, &row.ca_key_encrypted)?;
        let snapshot_version = u64::try_from(row.snapshot_version).map_err(|_| {
            invalid(
                row.id,
                "issue policy",
                "stored snapshot version is negative",
            )
        })?;
        let mut body = SnapshotBody {
            format: format_for(&[]),
            version: snapshot_version,
            node_id: sdk_node,
            issued_at: chrono::Utc::now(),
            lease_secs: MAX_POLICY_LEASE_SECONDS,
            session_keys: vec![PublishedKey::from_key(&session_signer.verifying_key())],
            revoked_sessions: Vec::new(),
            limited_sessions: Vec::new(),
            unmatched_host_policy: UnmatchedHostPolicy::Deny,
            egress: EgressPolicy::default(),
            passthrough_hosts: Vec::new(),
            interception_ca: InterceptionCa {
                cert_der: row.ca_certificate,
                key: seal(&seal_key, &ca_key, &ca_aad(&sdk_node))
                    .map_err(|source| sdk("seal interception key", source))?,
            },
            rules: Vec::new(),
        };
        let mut snapshot_bytes = serde_json::to_vec(&body)
            .map_err(|source| AuthorityError::Encoding {
                boot_id: row.id,
                source,
            })?
            .len();
        let mut issued = BTreeMap::new();
        for binding in bindings {
            let mut rules = Vec::new();
            for grant in binding.plan.grants() {
                let credential = binding.credentials.get(grant.grant_id()).ok_or_else(|| {
                    invalid(
                        row.id,
                        "seal grant credential",
                        "grant credential is missing",
                    )
                })?;
                let rule_id = *grant.claims().rules.first().ok_or_else(|| {
                    invalid(row.id, "seal grant credential", "grant rule is missing")
                })?;
                let credential_id = CredentialId(rule_id.0);
                let headers = InjectedHeaders {
                    headers: vec![InjectedHeader {
                        name: "authorization".into(),
                        value: if grant.rule().git.is_some() {
                            use base64::Engine;
                            let basic =
                                Zeroizing::new(format!("x-access-token:{}", credential.expose()));
                            format!(
                                "Basic {}",
                                base64::engine::general_purpose::STANDARD.encode(basic.as_bytes())
                            )
                        } else {
                            format!("Bearer {}", credential.expose())
                        },
                    }],
                    substitutions: Vec::new(),
                };
                let encoded = Zeroizing::new(serde_json::to_vec(&headers).map_err(|source| {
                    AuthorityError::Encoding {
                        boot_id: row.id,
                        source,
                    }
                })?);
                let sealed = seal(
                    &seal_key,
                    &encoded,
                    &rule_aad(&sdk_node, &rule_id, &credential_id),
                )
                .map_err(|source| sdk("seal upstream credential", source))?;
                rules.push(SnapshotRule {
                    rule_id,
                    scope: grant.claims().scope.clone(),
                    credential_id,
                    allow: grant.rule().clone(),
                    strip_headers: vec![
                        "authorization".into(),
                        "proxy-authorization".into(),
                        "cookie".into(),
                    ],
                    headers: sealed,
                    not_after: Some(grant.claims().exp),
                });
            }
            let binding_bytes = rules
                .iter()
                .try_fold(0usize, |size, rule| {
                    serde_json::to_vec(rule).map(|encoded| size + encoded.len() + 1)
                })
                .map_err(|source| AuthorityError::Encoding {
                    boot_id: row.id,
                    source,
                })?;
            // Admit whole bindings only. An oversized addition must not prevent
            // the already admitted worker bindings from renewing their leases.
            // Reserve a small allowance for a format-version change.
            if snapshot_bytes + binding_bytes + 128 > MAX_SNAPSHOT_BYTES {
                tracing::warn!(boot_id=%row.id, runtime_id=%binding.plan.target().incarnation_id,
                    binding_bytes, "Worker snapshot capacity exceeded; omitting binding from this lease");
                continue;
            }
            snapshot_bytes += binding_bytes;
            body.rules.extend(rules);
            let session = session_signer
                .sign(binding.plan.claims())
                .map_err(|source| sdk("sign binding session", source))?;
            if session.len() > 32 * 1024 {
                return Err(invalid(
                    row.id,
                    "sign binding session",
                    "session exceeds delivery limit",
                ));
            }
            issued.insert(
                binding.plan.target().incarnation_id,
                IssuedBinding {
                    grant_ids: binding
                        .plan
                        .grants()
                        .iter()
                        .map(|g| g.grant_id().to_owned())
                        .collect(),
                    github_git: binding.plan.grants().iter().any(|g| g.rule().git.is_some()),
                    version: binding.plan.version(),
                    session: SecretString::new(session),
                },
            );
        }
        body.format = format_for(&body.rules);
        let snapshot = SignedSnapshot::sign(&body, &snapshot_key)
            .map_err(|source| sdk("sign policy snapshot", source))?;
        if snapshot.body.len() > MAX_SNAPSHOT_BYTES {
            return Err(invalid(
                row.id,
                "issue policy",
                "snapshot exceeds 4 MiB delivery limit",
            ));
        }
        Ok(IssuedPolicy {
            authority,
            snapshot_version,
            lease_expires_at: body.issued_at
                + chrono::Duration::seconds(i64::from(body.lease_secs)),
            policy: snapshot.into(),
            bindings: issued,
        })
    }
}

pub(super) fn validate_bindings(
    boot_id: Uuid,
    bindings: &[AuthorizedBinding],
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), AuthorityError> {
    if bindings.len() > MAX_WORKER_BINDINGS {
        return Err(invalid(
            boot_id,
            "validate policy",
            "worker exceeds 128 bindings",
        ));
    }
    let mut incarnations = HashSet::new();
    let mut grants = HashSet::new();
    for binding in bindings {
        if !incarnations.insert(binding.plan.target().incarnation_id)
            || binding.credentials.len() != binding.plan.grants().len()
        {
            return Err(invalid(
                boot_id,
                "validate policy",
                "duplicate incarnation or incorrect credential count",
            ));
        }
        for grant in binding.plan.grants() {
            if !grants.insert(grant.grant_id()) || grant.claims().exp <= now {
                return Err(invalid(
                    boot_id,
                    "validate policy",
                    "duplicate or expired grant",
                ));
            }
            let credential = binding.credentials.get(grant.grant_id()).ok_or_else(|| {
                invalid(boot_id, "validate policy", "grant credential is missing")
            })?;
            let value = credential.expose();
            if value.is_empty()
                || value.len() > 16384
                || !value.bytes().all(|b| (33..=126).contains(&b))
            {
                return Err(invalid(
                    boot_id,
                    "validate credential",
                    "bearer credential must be 1 to 16384 printable non-space ASCII bytes",
                ));
            }
        }
    }
    Ok(())
}
