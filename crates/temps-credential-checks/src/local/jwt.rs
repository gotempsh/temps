// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! JSON Web Tokens. The signature is never verified: only `exp`, `nbf` and
//! `iat` are read, and no other claim is returned, logged or stored, because
//! claims can be personal data. The check is an expiry reminder, never proof
//! that a token is valid.

use super::{timestamp, ArtifactKind, ExpiringArtifact, Found};
use crate::verification::{CheckStatus, Finding};
use base64::Engine;
use serde::Deserialize;

const SHORT_LIVED_SECONDS: f64 = 86_400.0;

#[derive(Deserialize)]
struct Header {
    #[allow(dead_code)]
    alg: String,
}
/// Every other claim is ignored by serde and never materialized.
#[derive(Deserialize)]
struct TimeClaims {
    exp: Option<f64>,
    nbf: Option<f64>,
    iat: Option<f64>,
}

/// Recognizes a value that is exactly one compact JWS. Tokens without `exp`
/// never expire and yield nothing.
pub(super) fn from_value(value: &str, label: &str) -> Found {
    let mut found = Found::default();
    let Some(claims) = time_claims(value.trim()) else {
        return found;
    };
    let Some(expires_at) = claims.exp.and_then(seconds) else {
        return found;
    };
    found.artifacts.push(ExpiringArtifact {
        kind: ArtifactKind::Jwt,
        label: label.to_owned(),
        not_before: claims.nbf.and_then(seconds),
        expires_at,
    });
    if let (Some(exp), Some(iat)) = (claims.exp, claims.iat) {
        if exp > iat && exp - iat < SHORT_LIVED_SECONDS {
            found.findings.push(Finding {
                code: "short_lived_token".into(),
                status: CheckStatus::Warning,
                message: format!("{label} is valid for less than a day; a session token stored as a long-lived credential will stop working."),
            });
        }
    }
    found
}

fn time_claims(token: &str) -> Option<TimeClaims> {
    let mut parts = token.split('.');
    let (header, payload, signature) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || header.is_empty() || payload.is_empty() {
        return None;
    }
    if ![header, payload, signature].iter().all(|part| {
        part.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'=')
    }) {
        return None;
    }
    let decode = |part: &str| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(part.trim_end_matches('='))
    };
    serde_json::from_slice::<Header>(&decode(header).ok()?).ok()?;
    serde_json::from_slice(&decode(payload).ok()?).ok()
}

fn seconds(value: f64) -> Option<chrono::DateTime<chrono::Utc>> {
    if !value.is_finite() || value < 0.0 || value > i64::MAX as f64 {
        return None;
    }
    timestamp(value.floor() as i64)
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::tests::{at, verify};
    use super::super::*;
    use crate::verification::CheckStatus;
    use base64::Engine;

    /// Unsigned test tokens; the reader never checks signatures.
    pub(in crate::local) fn token(claims: serde_json::Value) -> String {
        let encode = |json: &serde_json::Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.to_string())
        };
        format!(
            "{}.{}.c2lnbmF0dXJl",
            encode(&serde_json::json!({"alg": "HS256", "typ": "JWT"})),
            encode(&claims)
        )
    }

    #[test]
    fn reads_expiry_and_not_before() {
        let exp = at("2026-09-25").timestamp();
        let value = token(
            serde_json::json!({"sub": "user-42", "exp": exp, "nbf": at("2026-09-01").timestamp()}),
        );
        let inspection = inspect(&value);
        assert_eq!(inspection.artifacts.len(), 1);
        let artifact = &inspection.artifacts[0];
        assert_eq!(artifact.kind, ArtifactKind::Jwt);
        assert_eq!(artifact.label, "JWT");
        assert_eq!(artifact.expires_at, at("2026-09-25"));
        assert_eq!(artifact.not_before, Some(at("2026-09-01")));
        let result = verify(&value);
        assert_eq!(result.status, CheckStatus::Warning);
        assert_eq!(
            result.findings.last().unwrap().code,
            "expires_within_7_days"
        );
    }

    #[test]
    fn tokens_without_expiry_or_shape_yield_nothing() {
        for value in [
            token(serde_json::json!({"sub": "service"})),
            token(serde_json::json!({"exp": "tomorrow"})),
            "eyJhbGciOiJIUzI1NiJ9.not json.sig".into(),
            "a.b".into(),
            "a.b.c.d".into(),
            format!(
                "Bearer {}",
                token(serde_json::json!({"exp": 1_900_000_000}))
            ),
        ] {
            assert!(inspect(&value).artifacts.is_empty(), "{value}");
        }
    }

    #[test]
    fn short_lived_tokens_warn_alongside_expiry() {
        let iat = at("2026-09-20").timestamp();
        let value = token(serde_json::json!({"iat": iat, "exp": iat + 3600}));
        let result = verify(&value);
        assert!(result
            .findings
            .iter()
            .any(|f| f.code == "short_lived_token" && f.status == CheckStatus::Warning));
        assert_eq!(result.findings.last().unwrap().code, "expired");
    }

    #[test]
    fn no_other_claim_reaches_any_finding() {
        let value = token(serde_json::json!({
            "sub": "subject-marker",
            "email": "person@example.test",
            "iat": at("2026-09-20").timestamp(),
            "exp": at("2026-09-20").timestamp() + 60,
        }));
        let serialized = serde_json::to_string(&verify(&value)).unwrap();
        assert!(!serialized.contains("subject-marker"));
        assert!(!serialized.contains("person@example.test"));
        let artifacts = serde_json::to_string(&inspect(&value).artifacts).unwrap();
        assert!(!artifacts.contains("subject-marker"));
    }
}
