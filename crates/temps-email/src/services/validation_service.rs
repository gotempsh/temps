//! Email validation service using check-if-email-exists library
//!
//! This service provides email validation capabilities to check if an email
//! address exists without sending any email.

use check_if_email_exists::{
    check_email, CheckEmailInput, CheckEmailInputProxy, CheckEmailOutput, Reachable,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::errors::EmailError;

/// Configuration for email validation
#[derive(Debug, Clone, Default)]
pub struct ValidationConfig {
    /// SOCKS5 proxy configuration for validation requests
    pub proxy: Option<ProxyConfig>,
    /// From email address to use for SMTP validation
    pub from_email: Option<String>,
    /// Hello name for SMTP HELO/EHLO command
    pub hello_name: Option<String>,
}

/// Proxy configuration for email validation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
}

/// Service for validating email addresses
pub struct ValidationService {
    config: ValidationConfig,
}

/// Request to validate an email address
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidateEmailRequest {
    /// Email address to validate
    pub email: String,
    /// Optional SOCKS5 proxy to use for this request
    pub proxy: Option<ProxyConfig>,
}

/// Email reachability status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReachabilityStatus {
    /// Email is safe to send to
    Safe,
    /// Email might bounce, proceed with caution
    Risky,
    /// Email is invalid and will definitely bounce
    Invalid,
    /// Unable to determine deliverability
    Unknown,
}

impl From<Reachable> for ReachabilityStatus {
    fn from(reachable: Reachable) -> Self {
        match reachable {
            Reachable::Safe => ReachabilityStatus::Safe,
            Reachable::Risky => ReachabilityStatus::Risky,
            Reachable::Invalid => ReachabilityStatus::Invalid,
            Reachable::Unknown => ReachabilityStatus::Unknown,
        }
    }
}

/// Syntax validation result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyntaxResult {
    /// Whether the email syntax is valid
    pub is_valid_syntax: bool,
    /// The domain part of the email
    pub domain: Option<String>,
    /// The username part of the email
    pub username: Option<String>,
    /// Suggested email correction if available
    pub suggestion: Option<String>,
}

/// MX (Mail Exchange) validation result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MxResult {
    /// Whether the domain accepts mail
    pub accepts_mail: bool,
    /// List of MX records for the domain
    pub records: Vec<String>,
    /// Error message if MX lookup failed
    pub error: Option<String>,
}

/// Misc validation result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiscResult {
    /// Whether the email is from a disposable email provider
    pub is_disposable: bool,
    /// Whether the email is a role-based account (e.g., admin@, info@)
    pub is_role_account: bool,
    /// Whether the email provider is a B2C (consumer) email provider
    pub is_b2c: bool,
    /// Gravatar URL if available
    pub gravatar_url: Option<String>,
}

/// SMTP validation result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmtpResult {
    /// Whether we could connect to the SMTP server
    pub can_connect_smtp: bool,
    /// Whether the mailbox appears to have a full inbox
    pub has_full_inbox: bool,
    /// Whether this is a catch-all domain
    pub is_catch_all: bool,
    /// Whether the email is deliverable
    pub is_deliverable: bool,
    /// Whether the mailbox is disabled
    pub is_disabled: bool,
    /// Error message if SMTP check failed
    pub error: Option<String>,
}

/// Complete email validation response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidateEmailResponse {
    /// The email address that was validated
    pub email: String,
    /// Overall reachability status
    pub is_reachable: ReachabilityStatus,
    /// Syntax validation result
    pub syntax: SyntaxResult,
    /// MX record validation result
    pub mx: MxResult,
    /// Miscellaneous validation result
    pub misc: MiscResult,
    /// SMTP validation result
    pub smtp: SmtpResult,
}

impl From<CheckEmailOutput> for ValidateEmailResponse {
    fn from(output: CheckEmailOutput) -> Self {
        // Extract syntax result
        let syntax = SyntaxResult {
            is_valid_syntax: output.syntax.is_valid_syntax,
            domain: Some(output.syntax.domain.to_string()),
            username: Some(output.syntax.username.to_string()),
            suggestion: output.syntax.suggestion.clone(),
        };

        // Extract MX result
        let mx = match &output.mx {
            Ok(mx_details) => {
                // MxDetails.lookup is Result<MxLookup, ResolveError>
                // When Ok, iterate over MxLookup using .iter() to get MX records
                let records: Vec<String> = mx_details
                    .lookup
                    .as_ref()
                    .map(|lookup| {
                        lookup
                            .iter()
                            .map(|host| host.exchange().to_string())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_else(|_| Vec::new());
                let accepts_mail = !records.is_empty();
                MxResult {
                    accepts_mail,
                    records,
                    error: None,
                }
            }
            Err(e) => MxResult {
                accepts_mail: false,
                records: Vec::new(),
                error: Some(format!("{:?}", e)),
            },
        };

        // Extract misc result
        let misc = match &output.misc {
            Ok(misc_details) => MiscResult {
                is_disposable: misc_details.is_disposable,
                is_role_account: misc_details.is_role_account,
                is_b2c: false, // This field may not exist in v0.9
                gravatar_url: misc_details.gravatar_url.clone(),
            },
            Err(_) => MiscResult {
                is_disposable: false,
                is_role_account: false,
                is_b2c: false,
                gravatar_url: None,
            },
        };

        // Extract SMTP result
        let smtp = match &output.smtp {
            Ok(smtp_details) => SmtpResult {
                can_connect_smtp: smtp_details.can_connect_smtp,
                has_full_inbox: smtp_details.has_full_inbox,
                is_catch_all: smtp_details.is_catch_all,
                is_deliverable: smtp_details.is_deliverable,
                is_disabled: smtp_details.is_disabled,
                error: None,
            },
            Err(e) => SmtpResult {
                can_connect_smtp: false,
                has_full_inbox: false,
                is_catch_all: false,
                is_deliverable: false,
                is_disabled: false,
                error: Some(format!("{:?}", e)),
            },
        };

        ValidateEmailResponse {
            email: output.input,
            is_reachable: output.is_reachable.into(),
            syntax,
            mx,
            misc,
            smtp,
        }
    }
}

impl ValidationService {
    /// Create a new validation service with the given configuration
    pub fn new(config: ValidationConfig) -> Self {
        Self { config }
    }

    /// Create a new validation service with default configuration
    pub fn with_default_config() -> Self {
        Self {
            config: ValidationConfig::default(),
        }
    }

    /// Validate a single email address
    pub async fn validate(
        &self,
        request: ValidateEmailRequest,
    ) -> Result<ValidateEmailResponse, EmailError> {
        info!("Validating email: {}", request.email);

        // Build the check email input
        let mut input = CheckEmailInput::new(request.email.clone());

        // Configure from email if available
        if let Some(from_email) = &self.config.from_email {
            input.set_from_email(from_email.clone());
        }

        // Configure hello name if available
        if let Some(hello_name) = &self.config.hello_name {
            input.set_hello_name(hello_name.clone());
        }

        // Configure proxy (request proxy takes precedence over service config)
        let proxy_config = request.proxy.as_ref().or(self.config.proxy.as_ref());
        if let Some(proxy) = proxy_config {
            let proxy_input = CheckEmailInputProxy {
                host: proxy.host.clone(),
                port: proxy.port,
                username: proxy.username.clone(),
                password: proxy.password.clone(),
            };

            input.set_proxy(proxy_input);
        }

        debug!("Calling check_email for: {}", request.email);

        // Perform the validation - returns a single CheckEmailOutput
        let output = check_email(&input).await;

        debug!(
            "Email validation result for {}: is_reachable={:?}",
            request.email, output.is_reachable
        );

        Ok(ValidateEmailResponse::from(output))
    }

    /// Validate multiple email addresses
    pub async fn validate_batch(
        &self,
        emails: Vec<String>,
    ) -> Result<Vec<ValidateEmailResponse>, EmailError> {
        let mut results = Vec::with_capacity(emails.len());

        for email in emails {
            let request = ValidateEmailRequest { email, proxy: None };
            let result = self.validate(request).await?;
            results.push(result);
        }

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reachability_status_from_reachable() {
        assert_eq!(
            ReachabilityStatus::from(Reachable::Safe),
            ReachabilityStatus::Safe
        );
        assert_eq!(
            ReachabilityStatus::from(Reachable::Risky),
            ReachabilityStatus::Risky
        );
        assert_eq!(
            ReachabilityStatus::from(Reachable::Invalid),
            ReachabilityStatus::Invalid
        );
        assert_eq!(
            ReachabilityStatus::from(Reachable::Unknown),
            ReachabilityStatus::Unknown
        );
    }

    #[test]
    fn test_validation_config_default() {
        let config = ValidationConfig::default();
        assert!(config.proxy.is_none());
        assert!(config.from_email.is_none());
        assert!(config.hello_name.is_none());
    }

    #[test]
    fn test_validation_config_with_proxy() {
        let config = ValidationConfig {
            proxy: Some(ProxyConfig {
                host: "proxy.example.com".to_string(),
                port: 1080,
                username: Some("user".to_string()),
                password: Some("pass".to_string()),
            }),
            from_email: Some("test@example.com".to_string()),
            hello_name: Some("mail.example.com".to_string()),
        };

        assert!(config.proxy.is_some());
        let proxy = config.proxy.unwrap();
        assert_eq!(proxy.host, "proxy.example.com");
        assert_eq!(proxy.port, 1080);
        assert_eq!(proxy.username, Some("user".to_string()));
        assert_eq!(proxy.password, Some("pass".to_string()));
    }

    #[test]
    fn test_validate_email_request() {
        let request = ValidateEmailRequest {
            email: "test@example.com".to_string(),
            proxy: None,
        };

        assert_eq!(request.email, "test@example.com");
        assert!(request.proxy.is_none());
    }

    #[test]
    fn test_syntax_result() {
        let syntax = SyntaxResult {
            is_valid_syntax: true,
            domain: Some("example.com".to_string()),
            username: Some("test".to_string()),
            suggestion: None,
        };

        assert!(syntax.is_valid_syntax);
        assert_eq!(syntax.domain, Some("example.com".to_string()));
        assert_eq!(syntax.username, Some("test".to_string()));
        assert!(syntax.suggestion.is_none());
    }

    #[test]
    fn test_mx_result() {
        let mx = MxResult {
            accepts_mail: true,
            records: vec!["mx1.example.com".to_string(), "mx2.example.com".to_string()],
            error: None,
        };

        assert!(mx.accepts_mail);
        assert_eq!(mx.records.len(), 2);
        assert!(mx.error.is_none());
    }

    #[test]
    fn test_misc_result() {
        let misc = MiscResult {
            is_disposable: false,
            is_role_account: true,
            is_b2c: false,
            gravatar_url: Some("https://gravatar.com/avatar/xxx".to_string()),
        };

        assert!(!misc.is_disposable);
        assert!(misc.is_role_account);
        assert!(!misc.is_b2c);
        assert!(misc.gravatar_url.is_some());
    }

    #[test]
    fn test_smtp_result() {
        let smtp = SmtpResult {
            can_connect_smtp: true,
            has_full_inbox: false,
            is_catch_all: false,
            is_deliverable: true,
            is_disabled: false,
            error: None,
        };

        assert!(smtp.can_connect_smtp);
        assert!(!smtp.has_full_inbox);
        assert!(!smtp.is_catch_all);
        assert!(smtp.is_deliverable);
        assert!(!smtp.is_disabled);
        assert!(smtp.error.is_none());
    }

    #[test]
    fn test_validate_email_response() {
        let response = ValidateEmailResponse {
            email: "test@example.com".to_string(),
            is_reachable: ReachabilityStatus::Safe,
            syntax: SyntaxResult {
                is_valid_syntax: true,
                domain: Some("example.com".to_string()),
                username: Some("test".to_string()),
                suggestion: None,
            },
            mx: MxResult {
                accepts_mail: true,
                records: vec!["mx.example.com".to_string()],
                error: None,
            },
            misc: MiscResult {
                is_disposable: false,
                is_role_account: false,
                is_b2c: false,
                gravatar_url: None,
            },
            smtp: SmtpResult {
                can_connect_smtp: true,
                has_full_inbox: false,
                is_catch_all: false,
                is_deliverable: true,
                is_disabled: false,
                error: None,
            },
        };

        assert_eq!(response.email, "test@example.com");
        assert_eq!(response.is_reachable, ReachabilityStatus::Safe);
        assert!(response.syntax.is_valid_syntax);
        assert!(response.mx.accepts_mail);
        assert!(!response.misc.is_disposable);
        assert!(response.smtp.is_deliverable);
    }

    #[test]
    fn test_validation_service_with_default_config() {
        let service = ValidationService::with_default_config();
        assert!(service.config.proxy.is_none());
        assert!(service.config.from_email.is_none());
        assert!(service.config.hello_name.is_none());
    }

    #[test]
    fn test_validation_service_with_config() {
        let config = ValidationConfig {
            proxy: None,
            from_email: Some("validator@example.com".to_string()),
            hello_name: Some("mail.example.com".to_string()),
        };

        let service = ValidationService::new(config);
        assert_eq!(
            service.config.from_email,
            Some("validator@example.com".to_string())
        );
        assert_eq!(
            service.config.hello_name,
            Some("mail.example.com".to_string())
        );
    }
}
