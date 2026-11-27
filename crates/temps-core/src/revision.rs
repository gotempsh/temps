// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Optimistic-concurrency revisions of settings documents.
//!
//! A settings form reads a whole document, changes some fields and writes
//! the whole document back. Its `GET` carries a revision of what it read and
//! its `PUT` sends it back as `expected_revision`; the write is refused with
//! `409 Conflict` when the stored document has changed since, so a save made
//! in between is never silently reverted.
//!
//! A revision derived from content ([`document_revision`]) must only be
//! computed over what the reader is allowed to see (masked secrets): a hash
//! of a stored credential would let anyone who can read the settings guess
//! it offline.

use sha2::{Digest, Sha256};

/// Revision of a JSON document: the first 16 hex digits of the SHA-256 of
/// its canonical form (object keys sorted at every level), so it does not
/// depend on the order the fields were serialized in.
pub fn document_revision(document: &serde_json::Value) -> String {
    let mut hasher = Sha256::new();
    feed_canonical(document, &mut hasher);
    let digest = hasher.finalize();
    hex::encode(&digest[..8])
}

fn feed_canonical(value: &serde_json::Value, hasher: &mut Sha256) {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            hasher.update(b"{");
            for key in keys {
                feed_canonical(&Value::String(key.clone()), hasher);
                hasher.update(b":");
                if let Some(child) = map.get(key) {
                    feed_canonical(child, hasher);
                }
                hasher.update(b",");
            }
            hasher.update(b"}");
        }
        Value::Array(items) => {
            hasher.update(b"[");
            for item in items {
                feed_canonical(item, hasher);
                hasher.update(b",");
            }
            hasher.update(b"]");
        }
        // Scalars serialize the same way whatever the map order.
        scalar => hasher.update(scalar.to_string().as_bytes()),
    }
}

/// The document changed after the client read it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "{resource} changed since it was read (revision {expected} was read, the stored revision is {current}); nothing was saved"
)]
pub struct StaleRevision {
    /// What was being written, for example "build limits of node 4".
    pub resource: String,
    pub expected: String,
    pub current: String,
}

/// `Ok` when no revision was sent (the write is unconditional, as it was
/// before revisions existed) or when it matches `current`.
pub fn check_expected_revision(
    resource: impl FnOnce() -> String,
    expected: Option<&str>,
    current: &str,
) -> Result<(), StaleRevision> {
    match expected {
        Some(expected) if expected != current => Err(StaleRevision {
            resource: resource(),
            expected: expected.to_string(),
            current: current.to_string(),
        }),
        _ => Ok(()),
    }
}

/// The `409 Conflict` Problem for a [`StaleRevision`], carrying both
/// revisions so a client can tell its user the settings changed and re-read.
pub fn stale_revision_problem(error: &StaleRevision) -> crate::problemdetails::Problem {
    crate::error_builder::ErrorBuilder::new(axum::http::StatusCode::CONFLICT)
        .type_("https://temps.sh/probs/stale-revision")
        .title("Settings Changed Since They Were Read")
        .detail(format!(
            "{error}. Read them again, reapply your change and save."
        ))
        .value("expected_revision", &error.expected)
        .value("current_revision", &error.current)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_revision_ignores_key_order_and_follows_every_value() {
        let a = json!({"b": 1, "a": {"y": [1, 2], "x": null}});
        let b = json!({"a": {"x": null, "y": [1, 2]}, "b": 1});
        assert_eq!(document_revision(&a), document_revision(&b));
        assert_eq!(document_revision(&a).len(), 16);
        assert_ne!(
            document_revision(&a),
            document_revision(&json!({"b": 2, "a": {"y": [1, 2], "x": null}}))
        );
        assert_ne!(
            document_revision(&json!({"a": [1, 2]})),
            document_revision(&json!({"a": [2, 1]}))
        );
        // A string that looks like a number is not that number.
        assert_ne!(
            document_revision(&json!({"a": "1"})),
            document_revision(&json!({"a": 1}))
        );
    }

    #[test]
    fn a_missing_revision_writes_and_a_stale_one_is_refused() {
        assert!(check_expected_revision(|| "x".into(), None, "abc").is_ok());
        assert!(check_expected_revision(|| "x".into(), Some("abc"), "abc").is_ok());
        let stale = check_expected_revision(|| "build limits of node 4".into(), Some("1"), "2")
            .unwrap_err();
        assert_eq!(stale.expected, "1");
        assert_eq!(stale.current, "2");
        let problem = stale_revision_problem(&stale);
        assert_eq!(problem.status_code, axum::http::StatusCode::CONFLICT);
        assert_eq!(problem.body["expected_revision"], "1");
        assert_eq!(problem.body["current_revision"], "2");
        assert!(problem.body["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("build limits of node 4")));
    }
}
