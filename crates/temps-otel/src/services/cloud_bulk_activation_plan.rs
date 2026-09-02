// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `plan_token` — the handle that binds a confirmed estimate to the job it
//! creates (ADR-042 §9).
//!
//! # What it is for
//!
//! The operator path is two calls: estimate, then submit. Between them the
//! world can move — a project can be created, deleted, have its fidelity
//! lowered, or simply accumulate another hour of spans. Without something
//! binding the two, "you confirmed this bill" is an approximation, and the
//! thing being approximated is money leaving the customer's account.
//!
//! So `POST /estimate` mints an opaque, short-lived, signed handle over the
//! **exact** project set, windows and per-project estimates it just showed, and
//! `POST /bulk-jobs` takes nothing else. The plan cannot be edited in transit
//! and cannot be assembled by a client that never estimated: the only way to
//! get a job is to have been shown its cost first.
//!
//! # Why the plan travels inside the token
//!
//! The alternative — a token that only carries a hash, with the project list
//! re-sent alongside it — needs the client to send back a list that must then
//! be re-hashed and compared. That works, but it leaves the submit endpoint
//! able to *receive* a project list at all, and the failure mode of getting the
//! comparison subtly wrong is "we shipped a different set than we quoted". Here
//! the submit endpoint has no project-list parameter to get wrong.
//!
//! # Why it is signed rather than stored
//!
//! A stored plan would need a table, an expiry sweep, and a decision about what
//! happens to rows nobody submits. An HMAC over the plan needs none of those and
//! survives a restart, because the key is derived from the instance's master
//! encryption key rather than generated per process — an operator who reads an
//! estimate, gets interrupted, and comes back inside the TTL should not have
//! their confirmation invalidated by an unrelated deploy.
//!
//! The key is a `derive_subkey` domain of its own, so this signature can never
//! be confused with an email-tracking link or any other HMAC on the instance.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as BASE64URL, Engine};
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
use temps_core::DBDateTime;

use crate::services::cloud_bulk_activation::BulkJobProjectPlan;

type HmacSha256 = Hmac<Sha256>;

/// `EncryptionService::derive_subkey` domain for the plan signing key.
///
/// Domain-separated so a signature minted here can never verify anywhere else
/// on this instance, and vice versa.
pub const PLAN_TOKEN_KEY_DOMAIN: &str = "temps-otel-cloud-bulk-activation-plan";

/// How long a minted plan stays submittable.
///
/// Long enough to read a forty-row table, think, and confirm; short enough that
/// an estimate left open in a tab overnight is re-computed rather than
/// submitted against yesterday's span counts.
pub const PLAN_TOKEN_TTL_SECONDS: i64 = 15 * 60;

/// Wire format marker. Bumped if the payload encoding ever changes, so an old
/// token is rejected with "re-estimate" rather than misparsed.
const PLAN_TOKEN_VERSION: &str = "v1";

/// Ceiling on how many projects one plan may name.
///
/// A plan is carried in a request body and re-parsed on submit, so it must be
/// bounded. Five hundred is far above any plausible instance's project count
/// (the reference deployment is 3 vCPU / 4 GB) and far below anything that
/// could be used to make the submit endpoint do unbounded work.
pub const MAX_PLAN_PROJECTS: usize = 500;

/// Advice appended to every rejection.
///
/// A self-hosted operator has nobody to ask what to do about a token they never
/// knew existed, so every failure says the same concrete next step.
const REESTIMATE: &str =
    "Re-run the estimate to get a fresh plan, then confirm that one — the estimate is free \
     and sends nothing.";

#[derive(Debug, thiserror::Error)]
pub enum PlanTokenError {
    #[error(
        "This activation plan token is not in the form this instance issues ({reason}). {REESTIMATE}"
    )]
    Malformed { reason: String },

    #[error(
        "This activation plan token was not issued by this instance, or its contents were \
         altered after it was issued, so the projects, windows and estimated cost it names \
         cannot be trusted. Nothing has been switched and nothing has been shipped. {REESTIMATE}"
    )]
    SignatureMismatch,

    #[error(
        "This activation plan token expired at {expired_at} (plans are valid for \
         {ttl_seconds} seconds). The span counts it quoted are no longer the counts that \
         would be shipped, so it is refused rather than billed against a stale estimate. \
         {REESTIMATE}"
    )]
    Expired {
        expired_at: String,
        ttl_seconds: i64,
    },

    #[error(
        "This activation plan token uses format `{version}`, which this instance does not \
         understand — it was most likely issued by a newer or older Temps build. {REESTIMATE}"
    )]
    UnsupportedVersion { version: String },

    #[error(
        "An activation plan may name at most {max} project(s); this one names {count}. \
         Split the activation into smaller batches."
    )]
    TooManyProjects { count: usize, max: usize },

    #[error(
        "An activation plan must name at least one project. This one names none, so there \
         would be nothing to switch and nothing to ship."
    )]
    NoProjects,

    #[error(
        "This instance could not sign the activation plan, so no plan token was issued. \
         Nothing has been switched and nothing has been shipped."
    )]
    SigningFailed,
}

/// A freshly minted plan, and everything the caller has to show or store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintedPlan {
    /// The opaque handle the operator sends back to create the job.
    pub token: String,
    /// Stable identity of the project set and windows, stored on the job row so
    /// an invoice dispute can be tied back to the estimate that authorized it.
    pub plan_hash: String,
    pub expires_at: DBDateTime,
}

/// A plan recovered from a token whose signature and expiry both checked out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedPlan {
    pub plan_hash: String,
    pub expires_at: DBDateTime,
    /// Exactly the projects, windows and estimates that were quoted, in
    /// ascending project id.
    pub projects: Vec<BulkJobProjectPlan>,
}

/// Stable identity of a project set and its windows.
///
/// Deliberately **not** over the estimates: two estimates of the same projects
/// over the same windows are the same plan even if the span counts moved
/// between them, and an operator retrying a plan should be able to see that
/// they are looking at the same activation.
pub fn plan_hash(projects: &[BulkJobProjectPlan]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(PLAN_TOKEN_VERSION.as_bytes());
    for plan in sorted(projects) {
        hasher.update(
            format!(
                "|{}:{}:{}",
                plan.project_id,
                plan.window_from.timestamp_millis(),
                plan.window_to.timestamp_millis()
            )
            .as_bytes(),
        );
    }
    hex::encode(hasher.finalize())
}

/// Mint a token over `projects`, valid for [`PLAN_TOKEN_TTL_SECONDS`].
pub fn mint_plan_token(
    key: &[u8; 32],
    projects: &[BulkJobProjectPlan],
    now: DBDateTime,
) -> Result<MintedPlan, PlanTokenError> {
    if projects.is_empty() {
        return Err(PlanTokenError::NoProjects);
    }
    if projects.len() > MAX_PLAN_PROJECTS {
        return Err(PlanTokenError::TooManyProjects {
            count: projects.len(),
            max: MAX_PLAN_PROJECTS,
        });
    }

    let expires_at = now + chrono::Duration::seconds(PLAN_TOKEN_TTL_SECONDS);
    let payload = encode_payload(expires_at, projects);
    let signature = sign(key, payload.as_bytes()).ok_or(PlanTokenError::SigningFailed)?;

    Ok(MintedPlan {
        token: format!("{}.{signature}", BASE64URL.encode(payload.as_bytes())),
        plan_hash: plan_hash(projects),
        expires_at,
    })
}

/// Recover a plan from a token, or say precisely why it cannot be trusted.
///
/// The signature is checked **before** the payload is parsed. Parsing
/// attacker-controlled bytes and then deciding whether to believe them is the
/// standard way this kind of check goes wrong.
pub fn verify_plan_token(
    key: &[u8; 32],
    token: &str,
    now: DBDateTime,
) -> Result<VerifiedPlan, PlanTokenError> {
    let (encoded, signature) = token
        .rsplit_once('.')
        .ok_or_else(|| PlanTokenError::Malformed {
            reason: "it has no `.` separating the plan from its signature".to_string(),
        })?;

    let payload =
        BASE64URL
            .decode(encoded.as_bytes())
            .map_err(|error| PlanTokenError::Malformed {
                reason: format!("its plan section is not valid base64url: {error}"),
            })?;

    // An absent signature can never be correct, and checking it here means a
    // signing failure can only ever fail closed.
    let expected = sign(key, &payload).ok_or(PlanTokenError::SignatureMismatch)?;
    if signature.is_empty() || !constant_time_eq(expected.as_bytes(), signature.as_bytes()) {
        return Err(PlanTokenError::SignatureMismatch);
    }

    // Authenticated from here down: these bytes were produced by this instance.
    let payload = String::from_utf8(payload).map_err(|error| PlanTokenError::Malformed {
        reason: format!("its plan section is not valid UTF-8: {error}"),
    })?;
    let (expires_at, projects) = decode_payload(&payload)?;

    if now > expires_at {
        return Err(PlanTokenError::Expired {
            expired_at: expires_at.to_rfc3339(),
            ttl_seconds: PLAN_TOKEN_TTL_SECONDS,
        });
    }

    Ok(VerifiedPlan {
        plan_hash: plan_hash(&projects),
        expires_at,
        projects,
    })
}

// ── Encoding ───────────────────────────────────────────────────────────────

/// `v1|<expires_ms>|<pid>:<from_ms>:<to_ms>:<spans>:<bytes>;…`
///
/// A flat, deterministic text encoding rather than JSON: the bytes that are
/// signed and the bytes that are parsed must be identical, and a serializer
/// whose field order or number formatting can change between versions makes
/// that a property of the serializer rather than of this file.
fn encode_payload(expires_at: DBDateTime, projects: &[BulkJobProjectPlan]) -> String {
    let entries: Vec<String> = sorted(projects)
        .into_iter()
        .map(|plan| {
            format!(
                "{}:{}:{}:{}:{}",
                plan.project_id,
                plan.window_from.timestamp_millis(),
                plan.window_to.timestamp_millis(),
                plan.estimated_spans,
                plan.estimated_bytes
            )
        })
        .collect();
    format!(
        "{PLAN_TOKEN_VERSION}|{}|{}",
        expires_at.timestamp_millis(),
        entries.join(";")
    )
}

fn decode_payload(payload: &str) -> Result<(DBDateTime, Vec<BulkJobProjectPlan>), PlanTokenError> {
    let mut parts = payload.splitn(3, '|');
    let version = parts.next().unwrap_or_default();
    if version != PLAN_TOKEN_VERSION {
        return Err(PlanTokenError::UnsupportedVersion {
            version: version.to_string(),
        });
    }

    let expires_ms: i64 = parts
        .next()
        .ok_or_else(|| PlanTokenError::Malformed {
            reason: "it carries no expiry".to_string(),
        })?
        .parse()
        .map_err(|error| PlanTokenError::Malformed {
            reason: format!("its expiry is not a millisecond timestamp: {error}"),
        })?;
    let expires_at = chrono::DateTime::from_timestamp_millis(expires_ms).ok_or_else(|| {
        PlanTokenError::Malformed {
            reason: format!("its expiry {expires_ms} is not a representable timestamp"),
        }
    })?;

    let body = parts.next().unwrap_or_default();
    if body.is_empty() {
        return Err(PlanTokenError::NoProjects);
    }

    let entries: Vec<&str> = body.split(';').collect();
    if entries.len() > MAX_PLAN_PROJECTS {
        return Err(PlanTokenError::TooManyProjects {
            count: entries.len(),
            max: MAX_PLAN_PROJECTS,
        });
    }

    let mut projects = Vec::with_capacity(entries.len());
    for entry in entries {
        projects.push(decode_project(entry)?);
    }
    Ok((expires_at, projects))
}

fn decode_project(entry: &str) -> Result<BulkJobProjectPlan, PlanTokenError> {
    let fields: Vec<&str> = entry.split(':').collect();
    if fields.len() != 5 {
        return Err(PlanTokenError::Malformed {
            reason: format!(
                "one of its project entries has {} field(s) rather than 5",
                fields.len()
            ),
        });
    }

    let malformed = |what: &str, error: std::num::ParseIntError| PlanTokenError::Malformed {
        reason: format!("one of its project entries has an unreadable {what}: {error}"),
    };

    let project_id: i32 = fields[0].parse().map_err(|e| malformed("project id", e))?;
    let from_ms: i64 = fields[1]
        .parse()
        .map_err(|e| malformed("window start", e))?;
    let to_ms: i64 = fields[2].parse().map_err(|e| malformed("window end", e))?;
    let estimated_spans: u64 = fields[3].parse().map_err(|e| malformed("span count", e))?;
    let estimated_bytes: u64 = fields[4].parse().map_err(|e| malformed("byte count", e))?;

    let window_from = chrono::DateTime::from_timestamp_millis(from_ms).ok_or_else(|| {
        PlanTokenError::Malformed {
            reason: format!(
                "project {project_id}'s window start {from_ms} is not a representable timestamp"
            ),
        }
    })?;
    let window_to = chrono::DateTime::from_timestamp_millis(to_ms).ok_or_else(|| {
        PlanTokenError::Malformed {
            reason: format!(
                "project {project_id}'s window end {to_ms} is not a representable timestamp"
            ),
        }
    })?;

    Ok(BulkJobProjectPlan {
        project_id,
        window_from,
        window_to,
        estimated_spans,
        estimated_bytes,
    })
}

/// Ascending project id — the documented processing order, and what makes the
/// hash independent of the order the caller happened to assemble the list in.
fn sorted(projects: &[BulkJobProjectPlan]) -> Vec<&BulkJobProjectPlan> {
    let mut sorted: Vec<&BulkJobProjectPlan> = projects.iter().collect();
    sorted.sort_by_key(|plan| plan.project_id);
    sorted
}

/// HMAC-SHA256 over the payload, hex-encoded.
///
/// `None` is unreachable — HMAC accepts a key of any length and this one is a
/// fixed 32 bytes — but it is returned rather than unwrapped so that the
/// impossible case fails closed at both call sites (no token is minted, and no
/// token verifies) instead of panicking inside a request.
fn sign(key: &[u8; 32], payload: &[u8]) -> Option<String> {
    let mut mac = HmacSha256::new_from_slice(key).ok()?;
    mac.update(payload);
    Some(hex::encode(mac.finalize().into_bytes()))
}

/// Length-independent comparison, so a signature cannot be recovered a byte at
/// a time from response timing.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut differences = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        differences |= x ^ y;
    }
    differences == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [7u8; 32];
    const OTHER_KEY: [u8; 32] = [9u8; 32];

    fn at(rfc3339: &str) -> DBDateTime {
        chrono::DateTime::parse_from_rfc3339(rfc3339)
            .expect("test timestamp must parse")
            .with_timezone(&chrono::Utc)
    }

    fn plan(project_id: i32) -> BulkJobProjectPlan {
        BulkJobProjectPlan {
            project_id,
            window_from: at("2026-08-01T00:00:00Z"),
            window_to: at("2026-09-01T00:00:00Z"),
            estimated_spans: 1_000 * project_id as u64,
            estimated_bytes: 250_000 * project_id as u64,
        }
    }

    #[test]
    fn a_minted_plan_round_trips_every_project_window_and_estimate() {
        // This is the whole guarantee: what the operator was quoted is what the
        // job is created from, byte for byte.
        let now = at("2026-09-01T12:00:00Z");
        let projects = vec![plan(4), plan(9), plan(1)];

        let minted = mint_plan_token(&KEY, &projects, now).expect("mint");
        let verified = verify_plan_token(&KEY, &minted.token, now).expect("verify");

        assert_eq!(verified.plan_hash, minted.plan_hash);
        assert_eq!(verified.expires_at, minted.expires_at);
        assert_eq!(
            verified.projects,
            vec![plan(1), plan(4), plan(9)],
            "the plan comes back in ascending project id, the processing order"
        );
    }

    #[test]
    fn the_hash_ignores_the_order_the_caller_listed_projects_in() {
        let a = [plan(9), plan(1), plan(4)];
        let b = [plan(1), plan(4), plan(9)];
        assert_eq!(plan_hash(&a), plan_hash(&b));
    }

    #[test]
    fn adding_or_removing_a_project_changes_the_hash() {
        // "The project set changed between estimate and submit" is exactly what
        // this has to detect.
        let three = [plan(1), plan(4), plan(9)];
        let two = [plan(1), plan(4)];
        assert_ne!(plan_hash(&three), plan_hash(&two));
    }

    #[test]
    fn moving_a_window_changes_the_hash() {
        let mut moved = plan(1);
        moved.window_to = at("2026-09-02T00:00:00Z");
        assert_ne!(plan_hash(&[plan(1)]), plan_hash(&[moved]));
    }

    #[test]
    fn re_estimating_the_same_projects_and_windows_keeps_the_same_hash() {
        // The hash identifies the activation, not the numbers, so an operator
        // retrying after a transient failure sees the same plan identity even
        // though another hour of spans arrived in between.
        let mut restated = plan(1);
        restated.estimated_spans += 12_345;
        restated.estimated_bytes += 999;
        assert_eq!(plan_hash(&[plan(1)]), plan_hash(&[restated]));
    }

    #[test]
    fn a_token_signed_with_another_key_is_refused_and_says_to_re_estimate() {
        let now = at("2026-09-01T12:00:00Z");
        let minted = mint_plan_token(&OTHER_KEY, &[plan(1)], now).expect("mint");

        let error = verify_plan_token(&KEY, &minted.token, now).expect_err("must refuse");
        assert!(matches!(error, PlanTokenError::SignatureMismatch));
        let message = error.to_string();
        assert!(message.contains("Re-run the estimate"), "{message}");
        assert!(
            message.contains("nothing has been shipped"),
            "the operator must be told no money was spent: {message}"
        );
    }

    #[test]
    fn editing_the_plan_inside_the_token_invalidates_it() {
        // Without this, "you confirmed this bill" would be a claim about the
        // client rather than about the server.
        let now = at("2026-09-01T12:00:00Z");
        let minted = mint_plan_token(&KEY, &[plan(1)], now).expect("mint");
        let (encoded, signature) = minted.token.rsplit_once('.').expect("well-formed token");

        let decoded = String::from_utf8(BASE64URL.decode(encoded.as_bytes()).expect("base64"))
            .expect("utf-8");
        // Add a second project the operator was never shown.
        let tampered = format!("{decoded};2:0:0:0:0");
        let forged = format!("{}.{signature}", BASE64URL.encode(tampered.as_bytes()));

        assert!(matches!(
            verify_plan_token(&KEY, &forged, now),
            Err(PlanTokenError::SignatureMismatch)
        ));
    }

    #[test]
    fn an_expired_token_is_refused_with_the_moment_it_expired() {
        let issued = at("2026-09-01T12:00:00Z");
        let minted = mint_plan_token(&KEY, &[plan(1)], issued).expect("mint");

        let just_inside = issued + chrono::Duration::seconds(PLAN_TOKEN_TTL_SECONDS);
        assert!(verify_plan_token(&KEY, &minted.token, just_inside).is_ok());

        let just_outside = just_inside + chrono::Duration::milliseconds(1);
        let error = verify_plan_token(&KEY, &minted.token, just_outside).expect_err("expired");
        match error {
            PlanTokenError::Expired { ref expired_at, .. } => {
                assert_eq!(expired_at, &minted.expires_at.to_rfc3339());
            }
            other => panic!("expected an expiry refusal, got {other}"),
        }
        assert!(error.to_string().contains("Re-run the estimate"));
    }

    #[test]
    fn garbage_is_rejected_as_malformed_rather_than_panicking() {
        let now = at("2026-09-01T12:00:00Z");
        for token in ["", "no-separator", "!!!.aabb", "."] {
            let error = verify_plan_token(&KEY, token, now).expect_err(token);
            assert!(
                matches!(
                    error,
                    PlanTokenError::Malformed { .. } | PlanTokenError::SignatureMismatch
                ),
                "{token} produced {error}"
            );
        }
    }

    #[test]
    fn an_empty_plan_is_refused_at_mint_time() {
        let now = at("2026-09-01T12:00:00Z");
        assert!(matches!(
            mint_plan_token(&KEY, &[], now),
            Err(PlanTokenError::NoProjects)
        ));
    }

    #[test]
    fn an_oversized_plan_is_refused_rather_than_signed() {
        let now = at("2026-09-01T12:00:00Z");
        let projects: Vec<BulkJobProjectPlan> =
            (1..=(MAX_PLAN_PROJECTS as i32 + 1)).map(plan).collect();

        assert!(matches!(
            mint_plan_token(&KEY, &projects, now),
            Err(PlanTokenError::TooManyProjects {
                max: MAX_PLAN_PROJECTS,
                ..
            })
        ));
    }

    #[test]
    fn the_key_domain_is_specific_to_this_use() {
        // Sharing a derived subkey with another feature would let a signature
        // minted there authorize a spend here.
        assert_eq!(
            PLAN_TOKEN_KEY_DOMAIN,
            "temps-otel-cloud-bulk-activation-plan"
        );
    }

    #[test]
    fn constant_time_equality_matches_ordinary_equality() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abx"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
