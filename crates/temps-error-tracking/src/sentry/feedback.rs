// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Feedback adapters: modern `captureFeedback` events and legacy user reports.
//! Baseline sentry-javascript 11.8.0, revision
//! c6492ef78c7bfb97931e43075763ce84ee8fda7e, core/src/feedback.ts.

use super::{envelope::EnvelopeError, logs};
use crate::services::feedback_service::{validate_submission, FeedbackSubmission};
use serde_json::Value;
use sha2::{Digest, Sha256};

fn invalid() -> EnvelopeError {
    EnvelopeError::InvalidPayload("invalid feedback submission".into())
}

pub fn decode(
    payload: &[u8],
    legacy: bool,
    envelope_id: Option<String>,
) -> Result<FeedbackSubmission, EnvelopeError> {
    if payload.len() > 128 * 1024 {
        return Err(invalid());
    }
    let raw: Value = serde_json::from_slice(payload).map_err(|_| invalid())?;
    let object = raw.as_object().ok_or_else(invalid)?;
    let feedback = if legacy {
        &raw
    } else {
        raw.pointer("/contexts/feedback").ok_or_else(invalid)?
    };
    if !feedback.is_object() {
        return Err(invalid());
    }
    let text = |key: &str| -> Result<Option<String>, EnvelopeError> {
        feedback
            .get(key)
            .filter(|v| !v.is_null())
            .map(|v| v.as_str().map(str::to_owned).ok_or_else(invalid))
            .transpose()
    };
    let id = if legacy {
        // Legacy reports have only an associated error ID. Their content-derived
        // identity allows distinct reports and makes exact retries idempotent.
        let canonical = serde_json::to_vec(&raw).map_err(|_| invalid())?;
        hex::encode(Sha256::digest(canonical))[..32].to_owned()
    } else {
        envelope_id
            .or_else(|| {
                raw.get("event_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .ok_or_else(invalid)?
    }
    .replace('-', "")
    .to_ascii_lowercase();
    if !logs::valid_id(&id, 32) {
        return Err(invalid());
    }
    let timestamp = match object.get("timestamp") {
        None | Some(Value::Null) => chrono::Utc::now(),
        Some(Value::String(s)) => chrono::DateTime::parse_from_rfc3339(s)
            .map_err(|_| invalid())?
            .to_utc(),
        Some(v) if v.is_number() => logs::timestamp(&v.to_string()).ok_or_else(invalid)?,
        _ => return Err(invalid()),
    };
    let mut context = object.clone();
    if !legacy {
        let mut extra = feedback.as_object().ok_or_else(invalid)?.clone();
        for key in [
            "message",
            "name",
            "contact_email",
            "url",
            "associated_event_id",
            "replay_id",
        ] {
            extra.remove(key);
        }
        if !extra.is_empty() {
            context.insert("feedback".into(), Value::Object(extra));
        }
    }

    // Native columns own feedback content. Keep other SDK context without a
    // parallel copy of the submission or protocol envelope.
    for key in [
        "event_id",
        "timestamp",
        "release",
        "environment",
        "type",
        "comments",
        "name",
        "email",
    ] {
        context.remove(key);
    }
    if let Some(contexts) = context.get_mut("contexts").and_then(Value::as_object_mut) {
        contexts.remove("feedback");
    }
    let field = |key: &str| -> Result<Option<String>, EnvelopeError> {
        raw.get(key)
            .filter(|v| !v.is_null())
            .map(|v| v.as_str().map(str::to_owned).ok_or_else(invalid))
            .transpose()
    };
    let normalize = |id: Option<String>| id.map(|id| id.replace('-', "").to_ascii_lowercase());
    let submission = FeedbackSubmission {
        source: "sentry".into(),
        source_id: id,
        timestamp,
        message: text(if legacy { "comments" } else { "message" })?.ok_or_else(invalid)?,
        name: text("name")?,
        email: text(if legacy { "email" } else { "contact_email" })?,
        url: text("url")?,
        associated_event_id: normalize(text(if legacy {
            "event_id"
        } else {
            "associated_event_id"
        })?),
        replay_id: normalize(text("replay_id")?),
        release: field("release")?,
        environment: field("environment")?,
        context: Value::Object(context),
    };
    validate_submission(&submission).map_err(|_| invalid())?;
    Ok(submission)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn modern_and_legacy_feedback_keep_content_and_stable_identity() {
        let payload=br#"{"event_id":"11111111111111111111111111111111","type":"feedback","contexts":{"feedback":{"message":"Synthetic feedback","name":"Synthetic user","contact_email":"synthetic@example.invalid","associated_event_id":"22222222222222222222222222222222","replay_id":"33333333333333333333333333333333","source":"api"},"custom":{"typed":true}},"tags":{"test":"feedback"}}"#;
        let f = decode(
            payload,
            false,
            Some("44444444444444444444444444444444".into()),
        )
        .unwrap();
        assert_eq!(f.source_id, "44444444444444444444444444444444");
        assert_eq!(f.message, "Synthetic feedback");
        assert_eq!(f.context["feedback"]["source"], "api");
        assert_eq!(f.context["contexts"]["custom"]["typed"], true);
        assert!(f.context.pointer("/contexts/feedback").is_none());
        let legacy=br#"{"event_id":"22222222222222222222222222222222","comments":"Synthetic legacy feedback","name":"Synthetic user","email":"synthetic@example.invalid"}"#;
        let a = decode(legacy, true, None).unwrap();
        let b = decode(legacy, true, None).unwrap();
        assert_eq!(a.source_id, b.source_id);
        assert_eq!(
            a.associated_event_id.as_deref(),
            Some("22222222222222222222222222222222")
        );
        assert!(decode(br#"{"event_id":"11111111111111111111111111111111","contexts":{"feedback":{"message":" "}}}"#,false,None).is_err());
        assert!(decode(br#"{"event_id":"11111111111111111111111111111111","contexts":{"feedback":{"message":"Synthetic","associated_event_id":"bad"}}}"#,false,None).is_err());
    }
}
