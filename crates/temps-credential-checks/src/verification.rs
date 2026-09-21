// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Healthy,
    Warning,
    Error,
    Unknown,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Finding {
    pub code: String,
    pub status: CheckStatus,
    pub message: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VerificationResult {
    pub status: CheckStatus,
    pub findings: Vec<Finding>,
    #[schema(value_type = String, format = DateTime)]
    pub checked_at: DateTime<Utc>,
}
impl VerificationResult {
    pub fn single(status: CheckStatus, code: &str, message: &str, now: DateTime<Utc>) -> Self {
        Self {
            status: status.clone(),
            findings: vec![Finding {
                code: code.into(),
                status,
                message: message.into(),
            }],
            checked_at: now,
        }
    }
    /// Stable across daily probes: changing numeric values do not cause alert storms.
    pub fn fingerprint(&self) -> String {
        let mut codes: Vec<_> = self
            .findings
            .iter()
            .filter(|f| f.status != CheckStatus::Healthy)
            .map(|f| f.code.as_str())
            .collect();
        codes.sort_unstable();
        codes.join(",")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HttpCheckMethod {
    Get,
    Head,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ResponseField {
    Header(String),
    JsonPointer(String),
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ExpirationRule {
    pub field: ResponseField,
    pub warning_days: Vec<u16>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Comparison {
    Below,
    Above,
}
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NumericRule {
    pub name: String,
    pub field: ResponseField,
    pub comparison: Comparison,
    pub warning: f64,
    pub critical: Option<f64>,
}
/// This is a declarative HTTP recipe, never executable code.
/// Headers may contain secrets: hosts must encrypt this specification at rest.
#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct HttpCheckSpec {
    pub url: String,
    pub method: HttpCheckMethod,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Credential injection is separate from detection and always explicitly selected.
    pub credential_header: Option<String>,
    #[serde(default)]
    pub credential_prefix: String,
    pub accepted_statuses: Vec<u16>,
    pub expiration: Option<ExpirationRule>,
    #[serde(default)]
    pub numeric_rules: Vec<NumericRule>,
}
impl std::fmt::Debug for HttpCheckSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HttpCheckSpec([redacted])")
    }
}
#[derive(Debug, thiserror::Error)]
pub enum VerificationError {
    #[error("Invalid HTTP check configuration: {reason}")]
    Configuration { reason: &'static str },
    #[error("HTTP check transport failed: {kind}")]
    Transport { kind: TransportFailure },
}
#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum TransportFailure {
    #[error("destination is not permitted")]
    Destination,
    #[error("DNS resolution failed")]
    Dns,
    #[error("request timed out")]
    Timeout,
    #[error("connection or TLS negotiation failed")]
    Connection,
    #[error("response exceeded the size limit")]
    ResponseTooLarge,
    #[error("request header is invalid")]
    Header,
}
/// Neither Debug nor serialization exposes request credentials or response bodies.
pub struct HttpCheckRequest {
    pub url: String,
    pub method: HttpCheckMethod,
    pub headers: BTreeMap<String, String>,
}
pub struct HttpCheckResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

/// Implementations must enforce a timeout, body limit, DNS/IP policy, TLS, and no redirects.
#[async_trait]
pub trait HttpCheckTransport: Send + Sync {
    async fn execute(
        &self,
        request: HttpCheckRequest,
    ) -> Result<HttpCheckResponse, VerificationError>;
}
#[async_trait]
pub trait CredentialVerifier: Send + Sync {
    async fn verify(
        &self,
        credential: Option<&str>,
        transport: &dyn HttpCheckTransport,
        now: DateTime<Utc>,
    ) -> Result<VerificationResult, VerificationError>;
}
#[derive(Debug, Clone)]
pub struct HttpCredentialVerifier {
    spec: HttpCheckSpec,
}
impl HttpCredentialVerifier {
    pub fn new(spec: HttpCheckSpec) -> Result<Self, VerificationError> {
        let invalid = |reason| VerificationError::Configuration { reason };
        if spec.url.len() > 2048 || !spec.url.starts_with("https://") {
            return Err(invalid(
                "a HTTPS endpoint of at most 2048 characters is required",
            ));
        }
        if spec.headers.len() > 16
            || spec.headers.iter().any(|(k, v)| {
                k.len() > 128
                    || v.len() > 4096
                    || k.contains(['\r', '\n'])
                    || v.contains(['\r', '\n'])
            })
        {
            return Err(invalid("headers exceed limits or contain newlines"));
        }
        if spec.credential_prefix.len() > 64 || spec.credential_prefix.contains(['\r', '\n']) {
            return Err(invalid("credential prefix is invalid"));
        }
        if spec
            .credential_header
            .as_ref()
            .is_some_and(|h| h.is_empty() || h.len() > 128 || h.contains(['\r', '\n']))
        {
            return Err(invalid("credential header is invalid"));
        }
        if spec.accepted_statuses.is_empty()
            || spec.accepted_statuses.len() > 16
            || spec
                .accepted_statuses
                .iter()
                .any(|s| !(200..300).contains(s))
        {
            return Err(invalid("accepted statuses must be successful HTTP codes"));
        }
        if spec.numeric_rules.len() > 8 {
            return Err(invalid("at most 8 numeric rules are allowed"));
        }
        if matches!(spec.method, HttpCheckMethod::Head)
            && (spec
                .numeric_rules
                .iter()
                .any(|r| matches!(r.field, ResponseField::JsonPointer(_)))
                || spec
                    .expiration
                    .as_ref()
                    .is_some_and(|r| matches!(r.field, ResponseField::JsonPointer(_))))
        {
            return Err(invalid("HEAD requests cannot inspect a JSON body"));
        }
        for field in spec
            .expiration
            .iter()
            .map(|r| &r.field)
            .chain(spec.numeric_rules.iter().map(|r| &r.field))
        {
            match field {
                ResponseField::Header(h) if h.is_empty() || h.len() > 128 => {
                    return Err(invalid("response header name is invalid"))
                }
                ResponseField::JsonPointer(p) if !p.starts_with('/') || p.len() > 256 => {
                    return Err(invalid(
                        "JSON selectors must be JSON pointers beginning with /",
                    ))
                }
                _ => {}
            }
        }
        if spec.expiration.as_ref().is_some_and(|r| {
            r.warning_days.is_empty()
                || r.warning_days.len() > 8
                || r.warning_days.iter().any(|d| *d == 0 || *d > 365)
        }) {
            return Err(invalid(
                "expiration warnings must contain 1–8 thresholds between 1 and 365 days",
            ));
        }
        for rule in &spec.numeric_rules {
            if rule.name.is_empty()
                || rule.name.len() > 80
                || !rule.warning.is_finite()
                || rule.critical.is_some_and(|c| {
                    !c.is_finite()
                        || match rule.comparison {
                            Comparison::Below => c > rule.warning,
                            Comparison::Above => c < rule.warning,
                        }
                })
            {
                return Err(invalid(
                    "numeric rules need a name, finite thresholds, and critical beyond warning",
                ));
            }
        }
        Ok(Self { spec })
    }
    pub fn spec(&self) -> &HttpCheckSpec {
        &self.spec
    }
    pub fn evaluate(&self, response: &HttpCheckResponse, now: DateTime<Utc>) -> VerificationResult {
        if !self.spec.accepted_statuses.contains(&response.status) {
            let (status, code, message) = match response.status {
                401 => (
                    CheckStatus::Error,
                    "authentication_rejected",
                    "The endpoint rejected the credential.",
                ),
                403 => (
                    CheckStatus::Warning,
                    "access_denied",
                    "The endpoint denied access. Check permissions or account restrictions.",
                ),
                429 => (
                    CheckStatus::Unknown,
                    "rate_limited",
                    "The endpoint rate-limited this check; validity could not be confirmed.",
                ),
                500..=599 => (
                    CheckStatus::Unknown,
                    "provider_unavailable",
                    "The provider is unavailable; validity could not be confirmed.",
                ),
                _ => (
                    CheckStatus::Unknown,
                    "unexpected_status",
                    "The endpoint returned an unexpected HTTP status.",
                ),
            };
            return VerificationResult::single(status, code, message, now);
        }
        let json: serde_json::Value =
            serde_json::from_slice(&response.body).unwrap_or(serde_json::Value::Null);
        let mut findings = vec![Finding {
            code: "http_success".into(),
            status: CheckStatus::Healthy,
            message: "The endpoint returned an expected HTTP status.".into(),
        }];
        if let Some(rule) = &self.spec.expiration {
            let finding =
                match read_field(&rule.field, response, &json).and_then(|s| parse_expiry(&s)) {
                    None => Finding {
                        code: "expiration_unknown".into(),
                        status: CheckStatus::Unknown,
                        message: "The endpoint did not provide a usable expiration date.".into(),
                    },
                    Some(expiry) if expiry <= now => Finding {
                        code: "expired".into(),
                        status: CheckStatus::Error,
                        message: "The credential has expired.".into(),
                    },
                    Some(expiry) => {
                        let days = (expiry - now).num_seconds() as f64 / 86400.0;
                        let threshold = rule
                            .warning_days
                            .iter()
                            .filter(|d| days <= **d as f64)
                            .min();
                        match threshold {
                            Some(d) => Finding {
                                code: format!("expires_within_{d}_days"),
                                status: CheckStatus::Warning,
                                message: format!(
                                    "The credential expires within {d} days ({}).",
                                    expiry.to_rfc3339()
                                ),
                            },
                            None => Finding {
                                code: "expiration_healthy".into(),
                                status: CheckStatus::Healthy,
                                message: format!("Expires {}.", expiry.to_rfc3339()),
                            },
                        }
                    }
                };
            findings.push(finding);
        }
        for (index, rule) in self.spec.numeric_rules.iter().enumerate() {
            let value = read_field(&rule.field, response, &json)
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|v| v.is_finite());
            let breached = |value: f64, threshold: f64| match rule.comparison {
                Comparison::Below => value < threshold,
                Comparison::Above => value > threshold,
            };
            let (status, suffix, message) = match value {
                None => (
                    CheckStatus::Unknown,
                    "unknown",
                    format!("{}: no usable numeric value was returned.", rule.name),
                ),
                Some(v) if rule.critical.is_some_and(|c| breached(v, c)) => (
                    CheckStatus::Error,
                    "critical",
                    format!("{} crossed its critical threshold.", rule.name),
                ),
                Some(v) if breached(v, rule.warning) => (
                    CheckStatus::Warning,
                    "warning",
                    format!("{} crossed its warning threshold.", rule.name),
                ),
                Some(_) => (
                    CheckStatus::Healthy,
                    "healthy",
                    format!("{} is within the configured thresholds.", rule.name),
                ),
            };
            findings.push(Finding {
                code: format!("numeric_{index}_{suffix}"),
                status,
                message,
            });
        }
        let status = if findings.iter().any(|f| f.status == CheckStatus::Error) {
            CheckStatus::Error
        } else if findings.iter().any(|f| f.status == CheckStatus::Warning) {
            CheckStatus::Warning
        } else if findings.iter().any(|f| f.status == CheckStatus::Unknown) {
            CheckStatus::Unknown
        } else {
            CheckStatus::Healthy
        };
        VerificationResult {
            status,
            findings,
            checked_at: now,
        }
    }
}
#[async_trait]
impl CredentialVerifier for HttpCredentialVerifier {
    async fn verify(
        &self,
        credential: Option<&str>,
        transport: &dyn HttpCheckTransport,
        now: DateTime<Utc>,
    ) -> Result<VerificationResult, VerificationError> {
        let mut headers = self.spec.headers.clone();
        if let Some(header) = &self.spec.credential_header {
            let value =
                credential
                    .filter(|s| !s.is_empty())
                    .ok_or(VerificationError::Configuration {
                        reason: "this check requires a credential",
                    })?;
            if value.len() > 16_384 || value.contains(['\r', '\n']) {
                return Err(VerificationError::Configuration {
                    reason: "credential is too long or contains newlines",
                });
            }
            headers.retain(|name, _| !name.eq_ignore_ascii_case(header));
            headers.insert(
                header.clone(),
                format!("{}{value}", self.spec.credential_prefix),
            );
        }
        let response = transport
            .execute(HttpCheckRequest {
                url: self.spec.url.clone(),
                method: self.spec.method.clone(),
                headers,
            })
            .await?;
        Ok(self.evaluate(&response, now))
    }
}
fn read_field(
    field: &ResponseField,
    response: &HttpCheckResponse,
    json: &serde_json::Value,
) -> Option<String> {
    match field {
        ResponseField::Header(name) => response
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone()),
        ResponseField::JsonPointer(pointer) => json.pointer(pointer).and_then(|v| match v {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        }),
    }
}
fn parse_expiry(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|d| d.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            DateTime::parse_from_rfc2822(value)
                .map(|d| d.with_timezone(&Utc))
                .ok()
        })
        .or_else(|| {
            NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S UTC")
                .ok()
                .map(|d| d.and_utc())
        })
        .or_else(|| {
            NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .map(|d| d.and_utc())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn spec() -> HttpCheckSpec {
        HttpCheckSpec {
            url: "https://api.example.com/me".into(),
            method: HttpCheckMethod::Get,
            headers: BTreeMap::new(),
            credential_header: Some("Authorization".into()),
            credential_prefix: "Bearer ".into(),
            accepted_statuses: vec![200],
            expiration: None,
            numeric_rules: vec![],
        }
    }
    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-21T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }
    fn response(status: u16, body: &str) -> HttpCheckResponse {
        HttpCheckResponse {
            status,
            headers: BTreeMap::new(),
            body: body.as_bytes().to_vec(),
        }
    }
    #[test]
    fn distinguishes_rejected_permissions_rate_limits_and_outages() {
        let verifier = HttpCredentialVerifier::new(spec()).unwrap();
        for (http, status) in [
            (200, CheckStatus::Healthy),
            (401, CheckStatus::Error),
            (403, CheckStatus::Warning),
            (429, CheckStatus::Unknown),
            (503, CheckStatus::Unknown),
            (302, CheckStatus::Unknown),
        ] {
            assert_eq!(
                verifier
                    .evaluate(&response(http, "remote-secret"), now())
                    .status,
                status
            );
        }
        assert!(
            !serde_json::to_string(&verifier.evaluate(&response(401, "remote-secret"), now()))
                .unwrap()
                .contains("remote-secret")
        );
    }
    #[test]
    fn expiry_thresholds_unknown_and_midnight_are_correct() {
        let mut spec = spec();
        spec.expiration = Some(ExpirationRule {
            field: ResponseField::JsonPointer("/expires_at".into()),
            warning_days: vec![30, 7, 1],
        });
        let v = HttpCredentialVerifier::new(spec).unwrap();
        for (date, code) in [
            ("2026-09-21", "expired"),
            ("2026-09-22", "expires_within_1_days"),
            ("2026-09-28", "expires_within_7_days"),
            ("2026-10-20", "expires_within_30_days"),
            ("2027-01-01", "expiration_healthy"),
            ("invalid", "expiration_unknown"),
        ] {
            assert_eq!(
                v.evaluate(
                    &response(200, &format!(r#"{{"expires_at":"{date}"}}"#)),
                    now()
                )
                .findings[1]
                    .code,
                code
            );
        }
        assert_eq!(
            v.evaluate(&response(200, "{}"), now()).status,
            CheckStatus::Unknown
        );
    }
    #[test]
    fn numeric_thresholds_require_actual_finite_values() {
        let mut s = spec();
        s.numeric_rules = vec![NumericRule {
            name: "Credits".into(),
            field: ResponseField::JsonPointer("/balance".into()),
            comparison: Comparison::Below,
            warning: 20.0,
            critical: Some(5.0),
        }];
        let v = HttpCredentialVerifier::new(s).unwrap();
        for (body, status) in [
            (r#"{"balance":30}"#, CheckStatus::Healthy),
            (r#"{"balance":10}"#, CheckStatus::Warning),
            (r#"{"balance":0}"#, CheckStatus::Error),
            (r#"{"balance":"NaN"}"#, CheckStatus::Unknown),
            ("{}", CheckStatus::Unknown),
        ] {
            assert_eq!(v.evaluate(&response(200, body), now()).status, status);
        }
        assert_eq!(
            v.evaluate(&response(200, r#"{"balance":10}"#), now())
                .fingerprint(),
            v.evaluate(&response(200, r#"{"balance":11}"#), now())
                .fingerprint()
        );
    }
    struct MockTransport;
    #[async_trait]
    impl HttpCheckTransport for MockTransport {
        async fn execute(
            &self,
            request: HttpCheckRequest,
        ) -> Result<HttpCheckResponse, VerificationError> {
            assert_eq!(
                request.headers.get("Authorization").unwrap(),
                "Bearer synthetic"
            );
            Ok(response(200, "{}"))
        }
    }
    #[tokio::test]
    async fn verifies_with_injected_transport_and_requires_credential() {
        let v = HttpCredentialVerifier::new(spec()).unwrap();
        assert!(v.verify(None, &MockTransport, now()).await.is_err());
        assert!(v
            .verify(Some("bad\r\nheader"), &MockTransport, now())
            .await
            .is_err());
        assert_eq!(
            v.verify(Some("synthetic"), &MockTransport, now())
                .await
                .unwrap()
                .status,
            CheckStatus::Healthy
        );
    }
    #[test]
    fn rejects_invalid_specs_and_redacts_debug() {
        let mut s = spec();
        s.accepted_statuses = vec![401];
        assert!(HttpCredentialVerifier::new(s).is_err());
        let mut s = spec();
        s.url = "http://example.com".into();
        assert!(HttpCredentialVerifier::new(s).is_err());
        let mut s = spec();
        s.headers.insert("X-Token".into(), "sensitive".into());
        assert!(!format!("{s:?}").contains("sensitive"));
    }
}
