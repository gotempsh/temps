//! AWS SNS message parsing and signature verification for SES event notifications
//!
//! Handles:
//! - SNS message parsing (SubscriptionConfirmation, Notification, UnsubscribeConfirmation)
//! - SigningCertURL validation (SSRF prevention)
//! - Signature verification (SHA1 for SignatureVersion "1", SHA256 for "2")
//! - SubscriptionConfirmation with retry
//! - SES event mapping (Delivery, Bounce, Complaint)

use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::errors::EmailTrackingError;

/// Parsed SNS message envelope
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct SnsMessage {
    #[serde(rename = "Type")]
    pub message_type: String,
    pub message_id: String,
    #[serde(default)]
    pub topic_arn: Option<String>,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub timestamp: String,
    #[serde(default)]
    pub signature: String,
    #[serde(default)]
    pub signature_version: String,
    #[serde(default, rename = "SigningCertURL")]
    pub signing_cert_url: Option<String>,
    #[serde(default, rename = "SubscribeURL")]
    pub subscribe_url: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
}

/// SES event notification parsed from the SNS message body
#[derive(Debug, Deserialize)]
pub struct SesEventNotification {
    #[serde(rename = "notificationType")]
    pub notification_type: String,
    pub mail: SesMail,
    #[serde(default)]
    pub bounce: Option<SesBounce>,
    #[serde(default)]
    pub complaint: Option<SesComplaint>,
    #[serde(default)]
    pub delivery: Option<SesDelivery>,
}

#[derive(Debug, Deserialize)]
pub struct SesMail {
    #[serde(rename = "messageId")]
    pub message_id: String,
    #[serde(default)]
    pub destination: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct SesBounce {
    #[serde(rename = "bounceType")]
    pub bounce_type: String,
    #[serde(rename = "bounceSubType")]
    pub bounce_sub_type: String,
    #[serde(default, rename = "bouncedRecipients")]
    pub bounced_recipients: Vec<SesRecipient>,
}

#[derive(Debug, Deserialize)]
pub struct SesComplaint {
    #[serde(default, rename = "complainedRecipients")]
    pub complained_recipients: Vec<SesRecipient>,
    #[serde(default, rename = "complaintFeedbackType")]
    pub complaint_feedback_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SesDelivery {
    #[serde(default)]
    pub recipients: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct SesRecipient {
    #[serde(rename = "emailAddress")]
    pub email_address: String,
}

/// Certificate cache for SNS signing certificates (LRU-like with capacity limit)
pub struct CertCache {
    cache: RwLock<HashMap<String, Vec<u8>>>,
    capacity: usize,
}

impl CertCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            cache: RwLock::new(HashMap::new()),
            capacity,
        }
    }

    pub async fn get(&self, url: &str) -> Option<Vec<u8>> {
        self.cache.read().await.get(url).cloned()
    }

    pub async fn insert(&self, url: String, cert: Vec<u8>) {
        let mut cache = self.cache.write().await;
        // Simple eviction: clear all when at capacity
        if cache.len() >= self.capacity {
            cache.clear();
        }
        cache.insert(url, cert);
    }
}

/// SNS signature verifier
pub struct SnsVerifier {
    cert_cache: Arc<CertCache>,
    http_client: reqwest::Client,
}

impl SnsVerifier {
    pub fn new() -> Result<Self, EmailTrackingError> {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| {
                EmailTrackingError::Configuration(format!(
                    "Failed to build SNS HTTP client: {error}"
                ))
            })?;
        Ok(Self {
            cert_cache: Arc::new(CertCache::new(100)),
            http_client,
        })
    }

    fn sns_hostname_for_topic(topic_arn: &str) -> Result<String, EmailTrackingError> {
        let parts: Vec<&str> = topic_arn.split(':').collect();
        if parts.len() != 6
            || parts[0] != "arn"
            || parts[2] != "sns"
            || parts[3].is_empty()
            || parts[4].len() != 12
            || !parts[4].bytes().all(|byte| byte.is_ascii_digit())
            || parts[5].is_empty()
        {
            return Err(EmailTrackingError::SnsValidation(format!(
                "Invalid SNS TopicArn: {topic_arn}"
            )));
        }
        let suffix = match parts[1] {
            "aws" | "aws-us-gov" => "amazonaws.com",
            "aws-cn" => "amazonaws.com.cn",
            partition => {
                return Err(EmailTrackingError::SnsValidation(format!(
                    "Unsupported SNS ARN partition: {partition}"
                )))
            }
        };
        Ok(format!("sns.{}.{}", parts[3], suffix))
    }

    pub fn validate_topic<'a>(
        &self,
        message: &'a SnsMessage,
    ) -> Result<&'a str, EmailTrackingError> {
        let topic = message
            .topic_arn
            .as_deref()
            .ok_or_else(|| EmailTrackingError::SnsValidation("Missing TopicArn".to_string()))?;
        Self::sns_hostname_for_topic(topic)?;
        Ok(topic)
    }

    /// Validate that a SigningCertURL is a legitimate AWS SNS URL.
    /// Prevents SSRF attacks where an attacker supplies their own cert URL.
    pub fn validate_signing_cert_url(url_str: &str) -> Result<(), EmailTrackingError> {
        let url = url::Url::parse(url_str).map_err(|_| {
            EmailTrackingError::SnsValidation(format!("Invalid SigningCertURL: {}", url_str))
        })?;

        if url.scheme() != "https" {
            return Err(EmailTrackingError::SnsValidation(
                "SigningCertURL must use HTTPS".to_string(),
            ));
        }

        let host = url.host_str().ok_or_else(|| {
            EmailTrackingError::SnsValidation("SigningCertURL missing host".to_string())
        })?;

        if (!host.ends_with(".amazonaws.com") && !host.ends_with(".amazonaws.com.cn"))
            || !host.starts_with("sns.")
        {
            return Err(EmailTrackingError::SnsValidation(format!(
                "SigningCertURL host must be sns.{{region}}.amazonaws.com, got: {}",
                host
            )));
        }

        if url.port().is_some() {
            return Err(EmailTrackingError::SnsValidation(
                "SigningCertURL must not specify a port".to_string(),
            ));
        }

        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(EmailTrackingError::SnsValidation(
                "SigningCertURL must not contain credentials, query, or fragment".to_string(),
            ));
        }

        let filename = url.path().strip_prefix('/').ok_or_else(|| {
            EmailTrackingError::SnsValidation("Invalid SigningCertURL path".to_string())
        })?;
        let digest = filename
            .strip_prefix("SimpleNotificationService-")
            .and_then(|value| value.strip_suffix(".pem"));
        if digest.is_none_or(|value| {
            value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_alphanumeric())
        }) {
            return Err(EmailTrackingError::SnsValidation(
                "SigningCertURL must name an AWS SimpleNotificationService certificate".to_string(),
            ));
        }

        Ok(())
    }

    /// Fetch a signing certificate, using cache when available.
    async fn fetch_cert(&self, url: &str) -> Result<Vec<u8>, EmailTrackingError> {
        if let Some(cert) = self.cert_cache.get(url).await {
            return Ok(cert);
        }

        Self::validate_signing_cert_url(url)?;

        let mut resp = self.http_client.get(url).send().await.map_err(|e| {
            EmailTrackingError::SnsValidation(format!("Failed to fetch cert: {}", e))
        })?;

        if !resp.status().is_success() {
            return Err(EmailTrackingError::SnsValidation(format!(
                "Cert fetch returned HTTP {}",
                resp.status()
            )));
        }

        const MAX_CERT_BYTES: usize = 64 * 1024;
        if resp
            .content_length()
            .is_some_and(|length| length > MAX_CERT_BYTES as u64)
        {
            return Err(EmailTrackingError::SnsValidation(
                "SNS signing certificate exceeded 64 KiB".to_string(),
            ));
        }
        let mut cert_pem = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| {
            EmailTrackingError::SnsValidation(format!("Failed to read cert body: {e}"))
        })? {
            if cert_pem.len() + chunk.len() > MAX_CERT_BYTES {
                return Err(EmailTrackingError::SnsValidation(
                    "SNS signing certificate exceeded 64 KiB".to_string(),
                ));
            }
            cert_pem.extend_from_slice(&chunk);
        }

        self.cert_cache
            .insert(url.to_string(), cert_pem.clone())
            .await;

        Ok(cert_pem)
    }

    /// Build the string-to-sign for an SNS message based on message type.
    /// AWS SNS requires specific field ordering depending on message type.
    fn build_string_to_sign(message: &SnsMessage) -> String {
        let mut parts = Vec::new();

        match message.message_type.as_str() {
            "Notification" => {
                parts.push(("Message", message.message.as_str()));
                parts.push(("MessageId", message.message_id.as_str()));
                if let Some(ref subject) = message.subject {
                    parts.push(("Subject", subject.as_str()));
                }
                parts.push(("Timestamp", message.timestamp.as_str()));
                if let Some(ref topic_arn) = message.topic_arn {
                    parts.push(("TopicArn", topic_arn.as_str()));
                }
                parts.push(("Type", message.message_type.as_str()));
            }
            "SubscriptionConfirmation" | "UnsubscribeConfirmation" => {
                parts.push(("Message", message.message.as_str()));
                parts.push(("MessageId", message.message_id.as_str()));
                if let Some(ref subscribe_url) = message.subscribe_url {
                    parts.push(("SubscribeURL", subscribe_url.as_str()));
                }
                parts.push(("Timestamp", message.timestamp.as_str()));
                if let Some(ref token) = message.token {
                    parts.push(("Token", token.as_str()));
                }
                if let Some(ref topic_arn) = message.topic_arn {
                    parts.push(("TopicArn", topic_arn.as_str()));
                }
                parts.push(("Type", message.message_type.as_str()));
            }
            _ => {}
        }

        let mut string_to_sign = String::new();
        for (key, value) in parts {
            string_to_sign.push_str(key);
            string_to_sign.push('\n');
            string_to_sign.push_str(value);
            string_to_sign.push('\n');
        }
        string_to_sign
    }

    /// Extract the RSA public key from a PEM-encoded X.509 certificate body.
    fn extract_public_key(cert_pem: &[u8]) -> Result<rsa::RsaPublicKey, EmailTrackingError> {
        use rsa::pkcs8::DecodePublicKey;

        let (_, pem) = x509_parser::pem::parse_x509_pem(cert_pem).map_err(|e| {
            EmailTrackingError::SnsValidation(format!("Failed to parse cert PEM: {}", e))
        })?;
        let cert = pem.parse_x509().map_err(|e| {
            EmailTrackingError::SnsValidation(format!("Failed to parse X.509 certificate: {}", e))
        })?;

        rsa::RsaPublicKey::from_public_key_der(cert.tbs_certificate.subject_pki.raw).map_err(|e| {
            EmailTrackingError::SnsValidation(format!(
                "Failed to extract RSA public key from certificate: {}",
                e
            ))
        })
    }

    /// Verify a PKCS#1v1.5 RSA signature over `string_to_sign` against
    /// `public_key`, for the given AWS `SignatureVersion` ("1" = SHA1, "2" =
    /// SHA256). Pure/sync so it's unit-testable without a network fetch —
    /// `verify_signature` below is the thin async wrapper that fetches the
    /// cert and calls this.
    fn verify_pkcs1v15(
        public_key: rsa::RsaPublicKey,
        signature_version: &str,
        string_to_sign: &str,
        signature_bytes: &[u8],
    ) -> Result<(), EmailTrackingError> {
        use rsa::pkcs1v15;
        use rsa::signature::hazmat::PrehashVerifier;
        use sha2_rsa::Digest;

        let result = match signature_version {
            "1" => {
                let hashed = sha1::Sha1::digest(string_to_sign.as_bytes());
                let signature = pkcs1v15::Signature::try_from(signature_bytes).map_err(|e| {
                    EmailTrackingError::SnsValidation(format!("Invalid signature: {}", e))
                })?;
                pkcs1v15::VerifyingKey::<sha1::Sha1>::new(public_key)
                    .verify_prehash(&hashed, &signature)
            }
            "2" => {
                let hashed = sha2_rsa::Sha256::digest(string_to_sign.as_bytes());
                let signature = pkcs1v15::Signature::try_from(signature_bytes).map_err(|e| {
                    EmailTrackingError::SnsValidation(format!("Invalid signature: {}", e))
                })?;
                pkcs1v15::VerifyingKey::<sha2_rsa::Sha256>::new(public_key)
                    .verify_prehash(&hashed, &signature)
            }
            other => {
                return Err(EmailTrackingError::SnsValidation(format!(
                    "Unsupported SignatureVersion: {}",
                    other
                )))
            }
        };

        result.map_err(|e| {
            EmailTrackingError::SnsValidation(format!("Signature verification failed: {}", e))
        })?;
        Ok(())
    }

    /// Verify the signature of an SNS message.
    ///
    /// Supports SignatureVersion "1" (SHA1 with RSA — AWS SNS default) and
    /// "2" (SHA256 with RSA — newer SNS topics). Fetches the signing
    /// certificate (already SSRF-guarded via `validate_signing_cert_url` in
    /// `fetch_cert`), extracts its RSA public key, and verifies the
    /// PKCS#1v1.5 signature against the canonical string-to-sign — this is
    /// what actually proves the notification came from AWS, not just that
    /// the cert URL looked legitimate.
    pub async fn verify_signature(&self, message: &SnsMessage) -> Result<(), EmailTrackingError> {
        let topic_arn = self.validate_topic(message)?;
        let cert_url = message.signing_cert_url.as_deref().ok_or_else(|| {
            EmailTrackingError::SnsValidation("Missing SigningCertURL".to_string())
        })?;

        let cert_host = url::Url::parse(cert_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned));
        let expected_host = Self::sns_hostname_for_topic(topic_arn)?;
        if cert_host.as_deref() != Some(expected_host.as_str()) {
            return Err(EmailTrackingError::SnsValidation(format!(
                "SigningCertURL host does not match TopicArn region: expected {expected_host}"
            )));
        }

        let cert_pem = self.fetch_cert(cert_url).await?;
        let public_key = Self::extract_public_key(&cert_pem)?;

        let string_to_sign = Self::build_string_to_sign(message);
        let signature_bytes = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            &message.signature,
        )
        .map_err(|e| {
            EmailTrackingError::SnsValidation(format!("Invalid signature base64: {}", e))
        })?;

        Self::verify_pkcs1v15(
            public_key,
            &message.signature_version,
            &string_to_sign,
            &signature_bytes,
        )?;

        debug!(
            "SNS signature verified (SignatureVersion {})",
            message.signature_version
        );
        Ok(())
    }

    /// Handle SubscriptionConfirmation by confirming the subscription with retry.
    pub async fn confirm_subscription(
        &self,
        message: &SnsMessage,
    ) -> Result<(), EmailTrackingError> {
        let subscribe_url = message
            .subscribe_url
            .as_deref()
            .ok_or_else(|| EmailTrackingError::SnsValidation("Missing SubscribeURL".to_string()))?;

        let topic_arn = self.validate_topic(message)?;
        let token = message.token.as_deref().ok_or_else(|| {
            EmailTrackingError::SnsValidation("Missing subscription Token".to_string())
        })?;

        // Bind the confirmation request to the exact authorized topic, region
        // and signed token. No redirects are followed by the shared client.
        let url = url::Url::parse(subscribe_url)
            .map_err(|_| EmailTrackingError::SnsValidation("Invalid SubscribeURL".to_string()))?;
        let expected_host = Self::sns_hostname_for_topic(topic_arn)?;
        if url.scheme() != "https"
            || url.host_str() != Some(expected_host.as_str())
            || url.port().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(EmailTrackingError::SnsValidation(format!(
                "SubscribeURL must use the authorized SNS endpoint {expected_host}"
            )));
        }
        let pairs: HashMap<String, String> = url.query_pairs().into_owned().collect();
        if pairs.len() != 3
            || pairs.get("Action").map(String::as_str) != Some("ConfirmSubscription")
            || pairs.get("TopicArn").map(String::as_str) != Some(topic_arn)
            || pairs.get("Token").map(String::as_str) != Some(token)
        {
            return Err(EmailTrackingError::SnsValidation(
                "SubscribeURL action, topic, or token does not match the signed envelope"
                    .to_string(),
            ));
        }

        // Retry with exponential backoff: 3 attempts, 1s/2s/4s
        let mut last_error = None;
        for attempt in 0..3u32 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(1 << attempt)).await;
            }
            match self.http_client.get(subscribe_url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    info!(
                        "SNS subscription confirmed for topic: {:?}",
                        message.topic_arn
                    );
                    return Ok(());
                }
                Ok(resp) => {
                    last_error = Some(format!("HTTP {}", resp.status()));
                }
                Err(e) => {
                    last_error = Some(e.to_string());
                }
            }
        }

        Err(EmailTrackingError::SnsValidation(format!(
            "Failed to confirm SNS subscription after 3 attempts: {:?}",
            last_error
        )))
    }

    /// Parse an SES event notification from an SNS message body.
    pub fn parse_ses_event(message_body: &str) -> Result<SesEventNotification, EmailTrackingError> {
        serde_json::from_str(message_body).map_err(|e| {
            EmailTrackingError::SnsValidation(format!("Failed to parse SES event: {}", e))
        })
    }

    /// Map an SES event to (event_type, metadata, recipients).
    pub fn map_ses_event(
        event: &SesEventNotification,
    ) -> (String, Option<serde_json::Value>, Vec<String>) {
        match event.notification_type.as_str() {
            "Delivery" => {
                let recipients = event
                    .delivery
                    .as_ref()
                    .map(|d| d.recipients.clone())
                    .unwrap_or_default();
                ("delivered".to_string(), None, recipients)
            }
            "Bounce" => {
                let (metadata, recipients) = if let Some(ref bounce) = event.bounce {
                    let meta = serde_json::json!({
                        "bounce_type": bounce.bounce_type,
                        "bounce_sub_type": bounce.bounce_sub_type,
                    });
                    let recips: Vec<String> = bounce
                        .bounced_recipients
                        .iter()
                        .map(|r| r.email_address.clone())
                        .collect();
                    (Some(meta), recips)
                } else {
                    (None, vec![])
                };
                ("bounced".to_string(), metadata, recipients)
            }
            "Complaint" => {
                let (metadata, recipients) = if let Some(ref complaint) = event.complaint {
                    let meta = serde_json::json!({
                        "complaint_feedback_type": complaint.complaint_feedback_type,
                    });
                    let recips: Vec<String> = complaint
                        .complained_recipients
                        .iter()
                        .map(|r| r.email_address.clone())
                        .collect();
                    (Some(meta), recips)
                } else {
                    (None, vec![])
                };
                ("complained".to_string(), metadata, recipients)
            }
            other => {
                warn!("Unknown SES notification type: {}", other);
                (other.to_lowercase(), None, vec![])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real self-signed RSA-2048 keypair + cert, generated once with:
    //   openssl genrsa -out key.pem 2048
    //   openssl req -new -x509 -key key.pem -out cert.pem -days 3650 \
    //     -subj "/CN=sns.us-east-1.amazonaws.com"
    // Signatures are over the literal bytes "hello sns test", produced with:
    //   openssl dgst -sha1/-sha256 -sign key.pem message.txt | base64
    // Exercises the actual crypto path (extract_public_key + verify_pkcs1v15)
    // against known-good vectors, not just error-path plumbing.
    // spellchecker:disable
    const TEST_CERT_PEM: &str = "-----BEGIN CERTIFICATE-----\n\
MIIDLTCCAhWgAwIBAgIUXj+rcBQ6uYyv/nRRRUamAvHUbBUwDQYJKoZIhvcNAQEL\n\
BQAwJjEkMCIGA1\x55\x45Awwbc25zLnVzLWVhc3QtMS5hbWF6b25hd3MuY29tMB4XDTI2\n\
MDcxMTA4NTU1MloXDTM2MDcwODA4NTU1MlowJjEkMCIGA1\x55\x45Awwbc25zLnVzLWVh\n\
c3QtMS5hbWF6b25hd3MuY29tMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKC\n\
AQEAsu8cvSBCdR/7h2dRj92q/9lcPOvJcwxN9ltYepB8Yo2Am+OA7BAkZKpSDJBQ\n\
snjTMdRcl0YyXIUZC3S2+pJQwJOfYHGx+Aj6uO20E03GtmFtjhT7phx2Z0SfvVjd\n\
1swvqAiz12WRFENJI9KjIpRUM0fZNFCyk0GM6gXkt4+1AW3+vWsaK/sHBqDCOx68\n\
zO6IDVnQWN9Fst9OO7vGNATlGctX6KCFJ+wbcTyWShaOmfQv4B1rnkn8x46Ks2e8\n\
yxTWxzzagcyN7DdqnrHUtRROho7vGNJvY5ym4W5N7SNz8puymE6yubCqY/Rk+bNL\n\
uMSRQKkYluO1wN8YH2CQtWEUpQIDAQABo1MwUTAdBgNVHQ4EFgQUXLZS/RfDvnB3\n\
z+9ixrxPqt2YRawwHwYDVR0jBBgwFoAUXLZS/RfDvnB3z+9ixrxPqt2YRawwDwYD\n\
VR0TAQH/BAUwAwEB/zANBgkqhkiG9w0BAQsFAAOCAQEAU4SSAcweqT9dEswEO2Q9\n\
A8/wYz4UGA6yD4HfSPSFVzdaXUVli5iFegaJM2nfBwXb0RhBE31smMyxNZAEjFcS\n\
FvojwUzVDSFbnR5m80h4M8EpJ9b1UbojX8xmZ696/ZX1PySbNRQwt5reS9RK1z1P\n\
mW3aiPGIh1X30h5tIBcnlNk99vL+2VD+fmGw6FdyXP8VmDPOXa/lBzw4LGm9mijn\n\
k4YZJ3XZqxeS5/0tAqqj+XzacraM6mm92nZxQNrF9UkPFwQWxxxBYfKQyU+8bWdO\n\
NzhDwvguWhmGlUoSFrzbyr3JbHTQCA+zhE5VxqYlcXCPap0dtfw1JxE0gUGJ/WdS\n\
EQ==\n\
-----END CERTIFICATE-----\n";
    // spellchecker:enable

    const TEST_MESSAGE: &str = "hello sns test";

    const TEST_SIG_SHA1_B64: &str = "SXLlWLT4D0tiG/G2gR3sl22QAKuV6CqbbbWy6FWVPAKQv0SmdBU5ck6CspGYGYmB360QAu+zv4nVKJITaiK3GIIindCreDNblABnIMZQdvxgRwIt8ihLwZV0UB1Ont6ex8+hp3s0SaP3YHpUOctz7LxD5ROOBespWgzsam7NH+R9BJvPgykkSVkwpYeK98SX7+5YUBQ5LmMIXm9z4JYFaMGd3YCy0T6EgSPlTNLggTcVq+dkT0al7NkiKv0ysWh8+gsZzq0tSfKhnKBxaJN5S3NkEEmKfGgVFgWOxUa/1tR7Cl+j45OXXPWRKwMMwlC2Q7CAo/b95FUkx9VvPWn+9g==";

    const TEST_SIG_SHA256_B64: &str = "q/jObO/8qo6HPN1FzvllsHpCLr3nEvW3r+HglGD0SfEjj3cng9jNj7xVvt0vFR3YN6YgyZn+Ss7kv8w/Yi90IUGHgx0RPvF1s3Nu9BqONNn8LUFzng+ixJESzGgZi9Czkgk5gob/jBTVoK+CT4sc/37ZJFw3o9rS8BrRm5uvCxP0hJJhlwkpIlMBwVILWJVk+bkGGnfuDF5Pn1OM1/S3L+FK+i3dS7V6pHovp0cINEjw/I3VOXeSuw5JTR6imQB8/OyIjaI3nd1oFuA7Tf9ynFmTMdBdMbpg1D0YyCJUJDOI1n5aEwuV/FIzppEsORYOyuUjB4pkxzVyQ5/UX/jsqA==";

    fn decode_b64(s: &str) -> Vec<u8> {
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, s).unwrap()
    }

    const ALLOWED_TOPIC: &str = "arn:aws:sns:us-east-1:123456789012:temps-ses";

    fn sns_message(topic_arn: Option<&str>) -> SnsMessage {
        SnsMessage {
            message_type: "Notification".to_string(),
            message_id: "sns-message-1".to_string(),
            topic_arn: topic_arn.map(str::to_string),
            message: "{}".to_string(),
            timestamp: "2026-07-14T10:00:00Z".to_string(),
            signature: String::new(),
            signature_version: "2".to_string(),
            signing_cert_url: Some(
                "https://sns.us-east-1.amazonaws.com/SimpleNotificationService-abc123.pem"
                    .to_string(),
            ),
            subscribe_url: None,
            subject: None,
            token: None,
        }
    }

    #[test]
    fn verifier_rejects_malformed_or_unsupported_topic_arns() {
        let verifier = SnsVerifier::new().unwrap();
        for topic in [
            "not-an-arn",
            "arn:aws:sqs:us-east-1:123456789012:temps-ses",
            "arn:aws:sns::123456789012:temps-ses",
            "arn:aws:sns:us-east-1:123:temps-ses",
            "arn:aws:sns:us-east-1:123456789012:",
            "arn:unknown:sns:us-east-1:123456789012:temps-ses",
        ] {
            assert!(verifier.validate_topic(&sns_message(Some(topic))).is_err());
        }
    }

    #[test]
    fn topic_validation_requires_a_well_formed_arn() {
        let verifier = SnsVerifier::new().unwrap();

        assert_eq!(
            verifier
                .validate_topic(&sns_message(Some(ALLOWED_TOPIC)))
                .unwrap(),
            ALLOWED_TOPIC
        );
        assert!(verifier.validate_topic(&sns_message(None)).is_err());
    }

    #[test]
    fn signature_verification_rejects_cert_host_from_another_region_before_fetch() {
        let verifier = SnsVerifier::new().unwrap();
        let mut message = sns_message(Some(ALLOWED_TOPIC));
        message.signing_cert_url = Some(
            "https://sns.eu-west-1.amazonaws.com/SimpleNotificationService-abc123.pem".to_string(),
        );

        let result = tokio_test::block_on(verifier.verify_signature(&message));
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn subscription_confirmation_rejects_url_not_bound_to_signed_envelope() {
        let verifier = SnsVerifier::new().unwrap();
        let token = "signed-token";
        let encoded_topic = urlencoding::encode(ALLOWED_TOPIC);

        for subscribe_url in [
            format!(
                "http://sns.us-east-1.amazonaws.com/?Action=ConfirmSubscription&TopicArn={encoded_topic}&Token={token}"
            ),
            format!(
                "https://sns.us-east-1.amazonaws.com:8443/?Action=ConfirmSubscription&TopicArn={encoded_topic}&Token={token}"
            ),
            format!(
                "https://sns.eu-west-1.amazonaws.com/?Action=ConfirmSubscription&TopicArn={encoded_topic}&Token={token}"
            ),
            format!(
                "https://sns.us-east-1.amazonaws.com/?Action=ConfirmSubscription&TopicArn={encoded_topic}&Token=wrong-token"
            ),
            format!(
                "https://sns.us-east-1.amazonaws.com/?Action=DeleteTopic&TopicArn={encoded_topic}&Token={token}"
            ),
        ] {
            let mut message = sns_message(Some(ALLOWED_TOPIC));
            message.message_type = "SubscriptionConfirmation".to_string();
            message.token = Some(token.to_string());
            message.subscribe_url = Some(subscribe_url.clone());
            assert!(
                verifier.confirm_subscription(&message).await.is_err(),
                "unbound SubscribeURL must be rejected: {subscribe_url}"
            );
        }
    }

    #[test]
    fn extract_public_key_parses_real_cert() {
        assert!(SnsVerifier::extract_public_key(TEST_CERT_PEM.as_bytes()).is_ok());
    }

    #[test]
    fn extract_public_key_rejects_garbage() {
        assert!(SnsVerifier::extract_public_key(b"not a certificate").is_err());
    }

    #[test]
    fn verify_pkcs1v15_accepts_valid_sha1_signature() {
        let key = SnsVerifier::extract_public_key(TEST_CERT_PEM.as_bytes()).unwrap();
        let sig = decode_b64(TEST_SIG_SHA1_B64);
        assert!(SnsVerifier::verify_pkcs1v15(key, "1", TEST_MESSAGE, &sig).is_ok());
    }

    #[test]
    fn verify_pkcs1v15_accepts_valid_sha256_signature() {
        let key = SnsVerifier::extract_public_key(TEST_CERT_PEM.as_bytes()).unwrap();
        let sig = decode_b64(TEST_SIG_SHA256_B64);
        assert!(SnsVerifier::verify_pkcs1v15(key, "2", TEST_MESSAGE, &sig).is_ok());
    }

    #[test]
    fn verify_pkcs1v15_rejects_tampered_message() {
        let key = SnsVerifier::extract_public_key(TEST_CERT_PEM.as_bytes()).unwrap();
        let sig = decode_b64(TEST_SIG_SHA256_B64);
        assert!(SnsVerifier::verify_pkcs1v15(key, "2", "tampered message", &sig).is_err());
    }

    #[test]
    fn verify_pkcs1v15_rejects_sha1_signature_claimed_as_sha256() {
        // A valid SHA1 signature must not verify under a "2" (SHA256) claim —
        // this is exactly the kind of algorithm-confusion an attacker would
        // try if SignatureVersion in the payload weren't cryptographically
        // bound to the signature itself.
        let key = SnsVerifier::extract_public_key(TEST_CERT_PEM.as_bytes()).unwrap();
        let sig = decode_b64(TEST_SIG_SHA1_B64);
        assert!(SnsVerifier::verify_pkcs1v15(key, "2", TEST_MESSAGE, &sig).is_err());
    }

    #[test]
    fn verify_pkcs1v15_rejects_garbage_signature() {
        let key = SnsVerifier::extract_public_key(TEST_CERT_PEM.as_bytes()).unwrap();
        assert!(SnsVerifier::verify_pkcs1v15(key, "2", TEST_MESSAGE, b"not-a-signature").is_err());
    }

    #[test]
    fn verify_pkcs1v15_rejects_unsupported_signature_version() {
        let key = SnsVerifier::extract_public_key(TEST_CERT_PEM.as_bytes()).unwrap();
        let sig = decode_b64(TEST_SIG_SHA256_B64);
        assert!(SnsVerifier::verify_pkcs1v15(key, "3", TEST_MESSAGE, &sig).is_err());
    }

    #[test]
    fn test_validate_signing_cert_url_valid() {
        assert!(SnsVerifier::validate_signing_cert_url(
            "https://sns.us-east-1.amazonaws.com/SimpleNotificationService-abc123.pem"
        )
        .is_ok());
        assert!(SnsVerifier::validate_signing_cert_url(
            "https://sns.cn-north-1.amazonaws.com.cn/SimpleNotificationService-abc123.pem"
        )
        .is_ok());
    }

    #[test]
    fn test_validate_signing_cert_url_http_rejected() {
        assert!(SnsVerifier::validate_signing_cert_url(
            "http://sns.us-east-1.amazonaws.com/SimpleNotificationService-abc123.pem"
        )
        .is_err());
    }

    #[test]
    fn test_validate_signing_cert_url_wrong_host() {
        assert!(SnsVerifier::validate_signing_cert_url(
            "https://evil.example.com/SimpleNotificationService-abc123.pem"
        )
        .is_err());
    }

    #[test]
    fn test_validate_signing_cert_url_wrong_path() {
        assert!(SnsVerifier::validate_signing_cert_url(
            "https://sns.us-east-1.amazonaws.com/some-other-path.pem"
        )
        .is_err());
    }

    #[test]
    fn test_validate_signing_cert_url_with_port() {
        assert!(SnsVerifier::validate_signing_cert_url(
            "https://sns.us-east-1.amazonaws.com:8443/SimpleNotificationService-abc123.pem"
        )
        .is_err());
    }

    #[test]
    fn test_validate_signing_cert_url_not_sns_subdomain() {
        assert!(SnsVerifier::validate_signing_cert_url(
            "https://s3.us-east-1.amazonaws.com/SimpleNotificationService-abc123.pem"
        )
        .is_err());
    }

    #[test]
    fn test_validate_signing_cert_url_rejects_url_smuggling_components() {
        for url in [
            "https://user@sns.us-east-1.amazonaws.com/SimpleNotificationService-abc123.pem",
            "https://sns.us-east-1.amazonaws.com/SimpleNotificationService-abc123.pem?next=https://evil.example",
            "https://sns.us-east-1.amazonaws.com/SimpleNotificationService-abc123.pem#fragment",
            "https://sns.us-east-1.amazonaws.com/nested/SimpleNotificationService-abc123.pem",
            "https://sns.us-east-1.amazonaws.com/SimpleNotificationService-abc123.pem.exe",
        ] {
            assert!(
                SnsVerifier::validate_signing_cert_url(url).is_err(),
                "invalid signing certificate URL must be rejected: {url}"
            );
        }
    }

    #[test]
    fn test_parse_ses_delivery() {
        let json = r#"{
            "notificationType": "Delivery",
            "mail": {
                "messageId": "msg-123",
                "destination": ["user@example.com"]
            },
            "delivery": {
                "recipients": ["user@example.com"]
            }
        }"#;

        let event = SnsVerifier::parse_ses_event(json).unwrap();
        assert_eq!(event.notification_type, "Delivery");
        assert_eq!(event.mail.message_id, "msg-123");
        assert!(event.delivery.is_some());
    }

    #[test]
    fn test_parse_ses_bounce() {
        let json = r#"{
            "notificationType": "Bounce",
            "mail": {
                "messageId": "msg-456",
                "destination": ["bounced@example.com"]
            },
            "bounce": {
                "bounceType": "Permanent",
                "bounceSubType": "General",
                "bouncedRecipients": [
                    {"emailAddress": "bounced@example.com"}
                ]
            }
        }"#;

        let event = SnsVerifier::parse_ses_event(json).unwrap();
        assert_eq!(event.notification_type, "Bounce");
        let bounce = event.bounce.unwrap();
        assert_eq!(bounce.bounce_type, "Permanent");
        assert_eq!(bounce.bounced_recipients.len(), 1);
    }

    #[test]
    fn test_parse_ses_complaint() {
        let json = r#"{
            "notificationType": "Complaint",
            "mail": {
                "messageId": "msg-789",
                "destination": ["user@example.com"]
            },
            "complaint": {
                "complainedRecipients": [
                    {"emailAddress": "user@example.com"}
                ],
                "complaintFeedbackType": "abuse"
            }
        }"#;

        let event = SnsVerifier::parse_ses_event(json).unwrap();
        assert_eq!(event.notification_type, "Complaint");
        let complaint = event.complaint.unwrap();
        assert_eq!(complaint.complaint_feedback_type, Some("abuse".to_string()));
    }

    #[test]
    fn test_map_ses_delivery() {
        let event = SesEventNotification {
            notification_type: "Delivery".to_string(),
            mail: SesMail {
                message_id: "msg-123".to_string(),
                destination: vec!["user@example.com".to_string()],
            },
            bounce: None,
            complaint: None,
            delivery: Some(SesDelivery {
                recipients: vec!["user@example.com".to_string()],
            }),
        };

        let (event_type, metadata, recipients) = SnsVerifier::map_ses_event(&event);
        assert_eq!(event_type, "delivered");
        assert!(metadata.is_none());
        assert_eq!(recipients, vec!["user@example.com"]);
    }

    #[test]
    fn test_map_ses_bounce() {
        let event = SesEventNotification {
            notification_type: "Bounce".to_string(),
            mail: SesMail {
                message_id: "msg-456".to_string(),
                destination: vec![],
            },
            bounce: Some(SesBounce {
                bounce_type: "Permanent".to_string(),
                bounce_sub_type: "General".to_string(),
                bounced_recipients: vec![SesRecipient {
                    email_address: "bad@example.com".to_string(),
                }],
            }),
            complaint: None,
            delivery: None,
        };

        let (event_type, metadata, recipients) = SnsVerifier::map_ses_event(&event);
        assert_eq!(event_type, "bounced");
        assert!(metadata.is_some());
        let meta = metadata.unwrap();
        assert_eq!(meta["bounce_type"], "Permanent");
        assert_eq!(recipients, vec!["bad@example.com"]);
    }

    #[test]
    fn test_build_string_to_sign_notification() {
        let msg = SnsMessage {
            message_type: "Notification".to_string(),
            message_id: "id-1".to_string(),
            topic_arn: Some("arn:aws:sns:us-east-1:123:topic".to_string()),
            message: "Hello".to_string(),
            timestamp: "2026-01-01T00:00:00.000Z".to_string(),
            signature: String::new(),
            signature_version: "1".to_string(),
            signing_cert_url: None,
            subscribe_url: None,
            subject: None,
            token: None,
        };

        let sts = SnsVerifier::build_string_to_sign(&msg);
        assert!(sts.contains("Message\nHello\n"));
        assert!(sts.contains("MessageId\nid-1\n"));
        assert!(sts.contains("Type\nNotification\n"));
    }
}
