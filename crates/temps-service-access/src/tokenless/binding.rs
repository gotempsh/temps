// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One strict session per sandbox binding. Rules retain their own deadlines,
//! including when multiple services share a destination. Preparation grants no
//! authority: the coordinator must authorize every input before signing.
use super::*;
use std::collections::HashSet;

pub const MAX_BINDING_GRANTS: usize = 16;

#[derive(Debug)]
pub struct PreparedBinding {
    target: BindingTarget,
    version: u64,
    grants: Vec<PreparedSession>,
    claims: SessionClaims,
}

impl PreparedBinding {
    pub fn prepare(
        requests: Vec<PrepareSession>,
        version: u64,
        now: DateTime<Utc>,
    ) -> Result<Self, PreparationError> {
        let invalid = |reason| PreparationError::Invalid {
            grant_id: "binding".into(),
            reason,
        };
        if version == 0 || requests.is_empty() || requests.len() > MAX_BINDING_GRANTS {
            return Err(invalid(
                "binding version must be positive and contain 1 to 16 grants",
            ));
        }
        let grants = requests
            .into_iter()
            .map(|request| PreparedSession::prepare(request, now))
            .collect::<Result<Vec<_>, _>>()?;
        let first = grants
            .first()
            .ok_or_else(|| invalid("binding needs a grant"))?;
        let target = first.target.clone();
        let mut ids = HashSet::new();
        let mut rules = Vec::new();
        let mut deadlines = BTreeMap::new();
        let mut expires_at = first.claims.exp;
        for grant in &grants {
            if grant.target != target {
                return Err(invalid(
                    "all grants must address the same project, sandbox, node and incarnation",
                ));
            }
            if !ids.insert(grant.grant_id.clone()) {
                return Err(invalid("a grant cannot occur twice in a binding"));
            }
            for rule in &grant.claims.rules {
                rules.push(*rule);
                deadlines.insert(*rule, grant.claims.exp);
            }
            expires_at = expires_at.max(grant.claims.exp);
        }
        let ttl = u64::try_from((expires_at - now).num_seconds())
            .map_err(|_| invalid("binding deadline cannot be represented"))?;
        let sdk = |operation, source| PreparationError::Sdk {
            grant_id: "binding".into(),
            operation,
            source,
        };
        let claims = SessionClaims::new(
            first.claims.scope.clone(),
            rules,
            ttl,
            None,
            BTreeMap::new(),
            now,
            86400,
        )
        .map_err(|source| sdk("binding claims", source))?
        .strict()
        .for_node(NodeId(target.node_id))
        .for_binding(&target.incarnation_id.to_string(), 1)
        .map_err(|source| sdk("binding identity", source))?
        .with_rule_deadlines(&deadlines, now)
        .map_err(|source| sdk("grant deadlines", source))?;
        Ok(Self {
            target,
            version,
            grants,
            claims,
        })
    }

    pub fn target(&self) -> &BindingTarget {
        &self.target
    }
    pub fn version(&self) -> u64 {
        self.version
    }
    pub fn grants(&self) -> &[PreparedSession] {
        &self.grants
    }
    pub fn claims(&self) -> &SessionClaims {
        &self.claims
    }
}
