// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Retry identities from protocol identities, never anonymous content alone.
use crate::providers::AuthContext;
use sha2::{Digest, Sha256};
use temps_otel::types::IngestIdentity;

fn scoped(auth: &AuthContext, domain: &[u8]) -> Sha256 {
    let mut hash = Sha256::new();
    hash.update(b"temps-sentry-retry-v1");
    hash.update(domain);
    hash.update(auth.project_id.to_be_bytes());
    // Zero is not a valid database entity ID. The authenticated DSN scope,
    // not a payload environment string, controls retry ownership.
    hash.update(auth.environment_id.unwrap_or(0).to_be_bytes());
    hash.update(auth.deployment_id.unwrap_or(0).to_be_bytes());
    hash
}

pub fn span(auth: &AuthContext, trace: &str, span: &str) -> IngestIdentity {
    let mut hash = scoped(auth, b"span");
    hash.update(trace.as_bytes());
    hash.update(span.as_bytes());
    IngestIdentity::from_digest(hash.finalize().into())
}

pub fn item(
    auth: &AuthContext,
    event_id: Option<&str>,
    item_index: usize,
    payload: &[u8],
    record_index: usize,
) -> Option<IngestIdentity> {
    let id = uuid::Uuid::parse_str(event_id?)
        .ok()
        .filter(|id| !id.is_nil())?;
    let mut hash = scoped(auth, b"identified-item");
    hash.update(id.as_bytes());
    hash.update((item_index as u64).to_be_bytes());
    hash.update(Sha256::digest(payload));
    hash.update((record_index as u64).to_be_bytes());
    Some(IngestIdentity::from_digest(hash.finalize().into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn anonymous_observations_stay_independent_and_identified_positions_are_distinct() {
        let mut auth = AuthContext {
            project_id: 1,
            environment_id: None,
            deployment_id: None,
            rate_limit_per_minute: None,
        };
        assert!(item(&auth, None, 0, b"same", 0).is_none());
        assert!(item(
            &auth,
            Some("00000000000000000000000000000000"),
            0,
            b"same",
            0
        )
        .is_none());
        let id = "11111111111111111111111111111111";
        let key = item(&auth, Some(id), 0, b"same", 0);
        assert_eq!(key, item(&auth, Some(id), 0, b"same", 0));
        assert_ne!(key, item(&auth, Some(id), 1, b"same", 0));
        assert_ne!(key, item(&auth, Some(id), 0, b"same", 1));
        assert_ne!(key, item(&auth, Some(id), 0, b"updated", 0));
        auth.environment_id = Some(2);
        assert_ne!(key, item(&auth, Some(id), 0, b"same", 0));
    }
}
