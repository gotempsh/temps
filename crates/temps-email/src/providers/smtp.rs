//! Generic SMTP relay provider.
//!
//! Unlike the SES/Scaleway providers, SMTP cannot create/verify domain identities
//! (there is no admin API). Domains used with this provider are considered
//! "imported": the user is responsible for setting up DKIM/SPF/MX at the upstream
//! mail server (AWS SES console, Sendgrid dashboard, etc.). We treat such domains
//! as already-verified so the rest of the email pipeline can use them.

use async_trait::async_trait;
use lettre::{
    message::{header::ContentType, Mailbox, MultiPart, SinglePart},
    transport::smtp::{authentication::Credentials, client::TlsParametersBuilder},
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, error};
use utoipa::ToSchema;

use super::traits::{
    DomainIdentity, DomainIdentityDetails, EmailProvider, EmailProviderType, SendEmailRequest,
    SendEmailResponse, VerificationStatus,
};
use crate::errors::EmailError;

/// SMTP TLS mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum SmtpEncryption {
    /// STARTTLS — start plain, upgrade to TLS (e.g. AWS SES port 587, Sendgrid).
    #[default]
    Starttls,
    /// Implicit TLS / SMTPS — TLS from the first byte (e.g. port 465).
    Tls,
    /// No encryption at all. Intended for local-testing relays like Mailhog.
    None,
}

/// Generic SMTP credentials. Works with any SMTP relay (AWS SES SMTP,
/// Sendgrid, Mailgun, Postmark, self-hosted Postfix, …).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmtpCredentials {
    /// SMTP host, e.g. `email-smtp.eu-west-1.amazonaws.com`.
    pub host: String,
    /// SMTP port (typically 587 for STARTTLS, 465 for implicit TLS).
    pub port: u16,
    /// SMTP username. Optional — leave empty for unauthenticated relays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// SMTP password / API token. Optional — required when `username` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// TLS mode.
    #[serde(default)]
    pub encryption: SmtpEncryption,
    /// Accept invalid / self-signed certs. Only safe for local testing.
    #[serde(default)]
    pub accept_invalid_certs: bool,
}

/// Generic SMTP provider.
pub struct SmtpProvider {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    host: String,
}

impl SmtpProvider {
    /// Build a transport from the supplied credentials.
    pub fn new(credentials: &SmtpCredentials) -> Result<Self, EmailError> {
        let host = credentials.host.clone();
        // Loosen TLS on loopback for local testing (mirrors temps-notifications).
        let is_loopback = host == "localhost" || host == "127.0.0.1" || host == "::1";
        let allow_invalid = credentials.accept_invalid_certs || is_loopback;

        let transport = match credentials.encryption {
            SmtpEncryption::None => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&host)
                .port(credentials.port)
                .credentials_opt(credentials)
                .build(),
            SmtpEncryption::Starttls => {
                let tls = TlsParametersBuilder::new(host.clone())
                    .dangerous_accept_invalid_certs(allow_invalid)
                    .dangerous_accept_invalid_hostnames(allow_invalid)
                    .build()
                    .map_err(|e| {
                        EmailError::Smtp(format!("Failed to build TLS parameters: {}", e))
                    })?;
                AsyncSmtpTransport::<Tokio1Executor>::relay(&host)
                    .map_err(|e| {
                        EmailError::Smtp(format!("Failed to build SMTP relay for {}: {}", host, e))
                    })?
                    .port(credentials.port)
                    .tls(lettre::transport::smtp::client::Tls::Required(tls))
                    .credentials_opt(credentials)
                    .build()
            }
            SmtpEncryption::Tls => {
                let tls = TlsParametersBuilder::new(host.clone())
                    .dangerous_accept_invalid_certs(allow_invalid)
                    .dangerous_accept_invalid_hostnames(allow_invalid)
                    .build()
                    .map_err(|e| {
                        EmailError::Smtp(format!("Failed to build TLS parameters: {}", e))
                    })?;
                AsyncSmtpTransport::<Tokio1Executor>::relay(&host)
                    .map_err(|e| {
                        EmailError::Smtp(format!("Failed to build SMTP relay for {}: {}", host, e))
                    })?
                    .port(credentials.port)
                    .tls(lettre::transport::smtp::client::Tls::Wrapper(tls))
                    .credentials_opt(credentials)
                    .build()
            }
        };

        Ok(Self { transport, host })
    }

    /// Host the provider is configured to relay through (used for logging).
    pub fn host(&self) -> &str {
        &self.host
    }
}

/// Tiny extension trait so we can attach credentials in one place above.
trait BuilderExt: Sized {
    fn credentials_opt(self, creds: &SmtpCredentials) -> Self;
}

impl BuilderExt for lettre::transport::smtp::AsyncSmtpTransportBuilder {
    fn credentials_opt(self, creds: &SmtpCredentials) -> Self {
        match (creds.username.as_deref(), creds.password.as_deref()) {
            (Some(u), Some(p)) if !u.is_empty() => {
                self.credentials(Credentials::new(u.to_string(), p.to_string()))
            }
            _ => self,
        }
    }
}

/// Parse `"name <addr@host>"` or `"addr@host"` into a lettre `Mailbox`.
fn parse_mailbox(address: &str, name: Option<&str>) -> Result<Mailbox, EmailError> {
    let parsed = address
        .parse::<Mailbox>()
        .map_err(|e| EmailError::Smtp(format!("Invalid email address '{}': {}", address, e)))?;
    Ok(Mailbox::new(
        name.map(|n| n.to_string()).or(parsed.name),
        parsed.email,
    ))
}

#[async_trait]
impl EmailProvider for SmtpProvider {
    /// SMTP has no domain-management API. We accept the domain as-is and treat
    /// it as "imported" — no records to set up, no provider identity to track.
    async fn create_identity(&self, domain: &str) -> Result<DomainIdentity, EmailError> {
        debug!(
            "SMTP provider: registering domain '{}' as imported (no DNS provisioning)",
            domain
        );
        Ok(DomainIdentity {
            provider_identity_id: domain.to_string(),
            spf_record: None,
            dkim_records: Vec::new(),
            dkim_selector: None,
            mx_record: None,
            mail_from_subdomain: None,
        })
    }

    /// Imported SMTP domains have no records we can probe via the provider, so
    /// we report them as verified. The user is responsible for DNS upstream.
    async fn verify_identity(&self, _domain: &str) -> Result<VerificationStatus, EmailError> {
        Ok(VerificationStatus::Verified)
    }

    async fn get_identity_details(
        &self,
        _domain: &str,
    ) -> Result<DomainIdentityDetails, EmailError> {
        Ok(DomainIdentityDetails {
            overall_status: VerificationStatus::Verified,
            spf_record: None,
            dkim_records: Vec::new(),
            mx_record: None,
            mail_from_subdomain: None,
        })
    }

    /// Nothing to delete upstream — caller still removes the row locally.
    async fn delete_identity(&self, _domain: &str) -> Result<(), EmailError> {
        Ok(())
    }

    async fn send(&self, email: &SendEmailRequest) -> Result<SendEmailResponse, EmailError> {
        debug!(
            "Sending email via SMTP relay {} from: {}",
            self.host, email.from
        );

        let from = parse_mailbox(&email.from, email.from_name.as_deref())?;

        if email.to.is_empty() {
            return Err(EmailError::Smtp(
                "At least one recipient is required".to_string(),
            ));
        }

        let mut builder = Message::builder()
            .from(from.clone())
            .subject(&email.subject);

        for to in &email.to {
            builder = builder.to(parse_mailbox(to, None)?);
        }
        if let Some(cc) = &email.cc {
            for addr in cc {
                builder = builder.cc(parse_mailbox(addr, None)?);
            }
        }
        if let Some(bcc) = &email.bcc {
            for addr in bcc {
                builder = builder.bcc(parse_mailbox(addr, None)?);
            }
        }
        if let Some(reply_to) = &email.reply_to {
            builder = builder.reply_to(parse_mailbox(reply_to, None)?);
        }
        if let Some(headers) = &email.headers {
            for (name, value) in headers {
                // lettre rejects malformed header names with an Err — surface that
                // as a typed SMTP error instead of swallowing.
                let header_name = lettre::message::header::HeaderName::new_from_ascii(name.clone())
                    .map_err(|e| {
                        EmailError::Smtp(format!("Invalid header name '{}': {}", name, e))
                    })?;
                builder = builder.raw_header(lettre::message::header::HeaderValue::new(
                    header_name,
                    value.clone(),
                ));
            }
        }

        let message = match (email.html.as_deref(), email.text.as_deref()) {
            (Some(html), Some(text)) => builder
                .multipart(
                    MultiPart::alternative()
                        .singlepart(
                            SinglePart::builder()
                                .header(ContentType::TEXT_PLAIN)
                                .body(text.to_string()),
                        )
                        .singlepart(
                            SinglePart::builder()
                                .header(ContentType::TEXT_HTML)
                                .body(html.to_string()),
                        ),
                )
                .map_err(|e| EmailError::Smtp(format!("Failed to build MIME message: {}", e)))?,
            (Some(html), None) => builder
                .header(ContentType::TEXT_HTML)
                .body(html.to_string())
                .map_err(|e| EmailError::Smtp(format!("Failed to build HTML message: {}", e)))?,
            (None, Some(text)) => builder
                .header(ContentType::TEXT_PLAIN)
                .body(text.to_string())
                .map_err(|e| EmailError::Smtp(format!("Failed to build text message: {}", e)))?,
            (None, None) => {
                return Err(EmailError::Smtp(
                    "Email must contain either an HTML or text body".to_string(),
                ));
            }
        };

        let message_id = message
            .headers()
            .get_raw("Message-ID")
            .map(|s| s.trim_matches(|c: char| c == '<' || c == '>').to_string())
            .unwrap_or_else(|| format!("smtp-{}", uuid::Uuid::new_v4()));

        self.transport.send(message).await.map_err(|e| {
            error!("Failed to send email via SMTP: {}", e);
            EmailError::Smtp(format!("Failed to send email: {}", e))
        })?;

        debug!("Email sent via SMTP, message_id: {}", message_id);
        Ok(SendEmailResponse { message_id })
    }

    fn provider_type(&self) -> EmailProviderType {
        EmailProviderType::Smtp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_smtp_credentials_serialization_roundtrip() {
        let creds = SmtpCredentials {
            host: "email-smtp.eu-west-1.amazonaws.com".to_string(),
            port: 587,
            username: Some("AKIAEXAMPLE".to_string()),
            password: Some("smtp-secret".to_string()),
            encryption: SmtpEncryption::Starttls,
            accept_invalid_certs: false,
        };

        let json = serde_json::to_string(&creds).unwrap();
        let back: SmtpCredentials = serde_json::from_str(&json).unwrap();
        assert_eq!(back.host, creds.host);
        assert_eq!(back.port, creds.port);
        assert_eq!(back.username, creds.username);
        assert_eq!(back.password, creds.password);
        assert_eq!(back.encryption, creds.encryption);
        assert!(!back.accept_invalid_certs);
    }

    #[test]
    fn test_smtp_credentials_skips_none_auth() {
        let creds = SmtpCredentials {
            host: "mailhog.local".to_string(),
            port: 1025,
            username: None,
            password: None,
            encryption: SmtpEncryption::None,
            accept_invalid_certs: false,
        };
        let json = serde_json::to_string(&creds).unwrap();
        assert!(!json.contains("username"));
        assert!(!json.contains("password"));
    }

    #[test]
    fn test_smtp_encryption_default_is_starttls() {
        assert_eq!(SmtpEncryption::default(), SmtpEncryption::Starttls);
    }

    #[tokio::test]
    async fn test_new_builds_transport_for_each_encryption_mode() {
        // The transport is built lazily but `relay()` constructs a tokio-aware
        // pool — run inside a tokio runtime so its Drop impl has somewhere to land.
        for enc in [
            SmtpEncryption::Starttls,
            SmtpEncryption::Tls,
            SmtpEncryption::None,
        ] {
            let creds = SmtpCredentials {
                host: "smtp.example.com".to_string(),
                port: 587,
                username: Some("u".to_string()),
                password: Some("p".to_string()),
                encryption: enc,
                accept_invalid_certs: false,
            };
            let provider = SmtpProvider::new(&creds);
            assert!(
                provider.is_ok(),
                "Expected SmtpProvider::new to succeed for {:?}",
                enc
            );
            assert_eq!(provider.unwrap().host(), "smtp.example.com");
        }
    }

    #[tokio::test]
    async fn test_imported_domain_is_auto_verified() {
        let creds = SmtpCredentials {
            host: "smtp.example.com".to_string(),
            port: 587,
            username: None,
            password: None,
            encryption: SmtpEncryption::Starttls,
            accept_invalid_certs: false,
        };
        let provider = SmtpProvider::new(&creds).unwrap();

        let identity = provider.create_identity("example.com").await.unwrap();
        assert_eq!(identity.provider_identity_id, "example.com");
        assert!(identity.spf_record.is_none());
        assert!(identity.dkim_records.is_empty());
        assert!(identity.mx_record.is_none());

        assert!(matches!(
            provider.verify_identity("example.com").await.unwrap(),
            VerificationStatus::Verified
        ));

        let details = provider.get_identity_details("example.com").await.unwrap();
        assert!(matches!(
            details.overall_status,
            VerificationStatus::Verified
        ));
        assert!(details.dkim_records.is_empty());

        provider.delete_identity("example.com").await.unwrap();
    }

    #[tokio::test]
    async fn test_send_rejects_empty_recipients() {
        let creds = SmtpCredentials {
            host: "smtp.example.com".to_string(),
            port: 587,
            username: None,
            password: None,
            encryption: SmtpEncryption::Starttls,
            accept_invalid_certs: false,
        };
        let provider = SmtpProvider::new(&creds).unwrap();
        let req = SendEmailRequest {
            from: "from@example.com".to_string(),
            from_name: None,
            to: Vec::new(),
            cc: None,
            bcc: None,
            reply_to: None,
            subject: "x".to_string(),
            html: None,
            text: Some("hi".to_string()),
            headers: None,
        };
        let err = provider.send(&req).await.unwrap_err();
        assert!(matches!(err, EmailError::Smtp(_)));
    }

    #[tokio::test]
    async fn test_send_requires_html_or_text_body() {
        let creds = SmtpCredentials {
            host: "smtp.example.com".to_string(),
            port: 587,
            username: None,
            password: None,
            encryption: SmtpEncryption::Starttls,
            accept_invalid_certs: false,
        };
        let provider = SmtpProvider::new(&creds).unwrap();
        let req = SendEmailRequest {
            from: "from@example.com".to_string(),
            from_name: None,
            to: vec!["to@example.com".to_string()],
            cc: None,
            bcc: None,
            reply_to: None,
            subject: "x".to_string(),
            html: None,
            text: None,
            headers: None,
        };
        let err = provider.send(&req).await.unwrap_err();
        match err {
            EmailError::Smtp(msg) => assert!(msg.contains("body")),
            other => panic!("expected Smtp body error, got {:?}", other),
        }
    }
}
