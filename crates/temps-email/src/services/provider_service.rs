//! Provider service for managing email provider configurations

use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder,
};
use std::sync::Arc;
use temps_core::EncryptionService;
use temps_entities::email_providers;
use tracing::{debug, error};

use crate::errors::EmailError;
use crate::providers::{
    EmailProvider, EmailProviderType, ScalewayCredentials, ScalewayProvider, SesCredentials,
    SesProvider, SmtpCredentials, SmtpProvider,
};

/// Service for managing email providers
#[derive(Clone)]
pub struct ProviderService {
    db: Arc<DatabaseConnection>,
    encryption_service: Arc<EncryptionService>,
}

/// Request to create a new email provider
#[derive(Debug, Clone)]
pub struct CreateProviderRequest {
    pub name: String,
    pub provider_type: EmailProviderType,
    pub region: String,
    pub credentials: ProviderCredentials,
}

/// Provider credentials enum
#[derive(Debug, Clone)]
pub enum ProviderCredentials {
    Ses(SesCredentials),
    Scaleway(ScalewayCredentials),
    Smtp(SmtpCredentials),
}

impl ProviderCredentials {
    /// Get the provider type that owns these credentials.
    pub fn provider_type(&self) -> EmailProviderType {
        match self {
            ProviderCredentials::Ses(_) => EmailProviderType::Ses,
            ProviderCredentials::Scaleway(_) => EmailProviderType::Scaleway,
            ProviderCredentials::Smtp(_) => EmailProviderType::Smtp,
        }
    }
}

/// Request to update an existing email provider. All fields are optional —
/// `None` means "leave this field unchanged". `provider_type` cannot be
/// changed (the stored credentials format is fixed at creation time).
#[derive(Debug, Clone, Default)]
pub struct UpdateProviderRequest {
    pub name: Option<String>,
    pub region: Option<String>,
    /// New credentials. Must match the existing provider's type or the
    /// update is rejected. `None` keeps the encrypted blob as-is — this is
    /// how operators rotate `name`/`region` without re-typing secrets.
    pub credentials: Option<ProviderCredentials>,
    pub is_active: Option<bool>,
}

/// Summary of what changed during an update. Used for audit logging.
#[derive(Debug, Clone)]
pub struct UpdateProviderOutcome {
    pub provider: email_providers::Model,
    pub changed_fields: Vec<String>,
}

/// Result of sending a test email
#[derive(Debug, Clone)]
pub struct TestEmailResult {
    /// Whether the test email was sent successfully
    pub success: bool,
    /// The email address the test was sent to
    pub recipient_email: String,
    /// Provider message ID if successful
    pub provider_message_id: Option<String>,
    /// Error message if failed
    pub error: Option<String>,
}

impl ProviderService {
    pub fn new(db: Arc<DatabaseConnection>, encryption_service: Arc<EncryptionService>) -> Self {
        Self {
            db,
            encryption_service,
        }
    }

    /// Create a new email provider
    pub async fn create(
        &self,
        request: CreateProviderRequest,
    ) -> Result<email_providers::Model, EmailError> {
        debug!(
            "Creating email provider: {} ({})",
            request.name, request.provider_type
        );

        // Serialize credentials to JSON
        let credentials_json = match &request.credentials {
            ProviderCredentials::Ses(creds) => serde_json::to_string(creds)?,
            ProviderCredentials::Scaleway(creds) => serde_json::to_string(creds)?,
            ProviderCredentials::Smtp(creds) => serde_json::to_string(creds)?,
        };

        // Encrypt credentials
        let encrypted_credentials = self
            .encryption_service
            .encrypt_string(&credentials_json)
            .map_err(|e| EmailError::Encryption(e.to_string()))?;

        let provider = email_providers::ActiveModel {
            name: Set(request.name),
            provider_type: Set(request.provider_type.to_string()),
            region: Set(request.region),
            credentials: Set(encrypted_credentials),
            is_active: Set(true),
            ..Default::default()
        };

        let result = provider.insert(self.db.as_ref()).await?;

        debug!("Created email provider with id: {}", result.id);

        Ok(result)
    }

    /// Get a provider by ID
    pub async fn get(&self, id: i32) -> Result<email_providers::Model, EmailError> {
        email_providers::Entity::find_by_id(id)
            .one(self.db.as_ref())
            .await?
            .ok_or(EmailError::ProviderNotFound(id))
    }

    /// List all providers
    pub async fn list(&self) -> Result<Vec<email_providers::Model>, EmailError> {
        let providers = email_providers::Entity::find()
            .order_by_desc(email_providers::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;

        Ok(providers)
    }

    /// List only active providers
    pub async fn list_active(&self) -> Result<Vec<email_providers::Model>, EmailError> {
        let providers = email_providers::Entity::find()
            .filter(email_providers::Column::IsActive.eq(true))
            .order_by_desc(email_providers::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;

        Ok(providers)
    }

    /// Delete a provider
    pub async fn delete(&self, id: i32) -> Result<(), EmailError> {
        let provider = self.get(id).await?;

        email_providers::Entity::delete_by_id(provider.id)
            .exec(self.db.as_ref())
            .await?;

        debug!("Deleted email provider with id: {}", id);

        Ok(())
    }

    /// Update provider active status
    pub async fn set_active(
        &self,
        id: i32,
        is_active: bool,
    ) -> Result<email_providers::Model, EmailError> {
        let provider = self.get(id).await?;

        let mut active_model: email_providers::ActiveModel = provider.into();
        active_model.is_active = Set(is_active);

        let result = active_model.update(self.db.as_ref()).await?;

        debug!(
            "Updated email provider {} active status to: {}",
            id, is_active
        );

        Ok(result)
    }

    /// Update an existing provider. Returns the updated row plus the list of
    /// fields that actually changed (used by handlers for audit logging).
    ///
    /// `provider_type` is immutable. If `request.credentials` is `Some` and the
    /// variant doesn't match the existing provider's type, the call fails with
    /// `EmailError::Validation` rather than silently corrupting the row.
    pub async fn update(
        &self,
        id: i32,
        request: UpdateProviderRequest,
    ) -> Result<UpdateProviderOutcome, EmailError> {
        debug!("Updating email provider {}", id);

        let existing = self.get(id).await?;
        let existing_type = EmailProviderType::from_str(&existing.provider_type)?;
        let mut changed_fields: Vec<String> = Vec::new();

        let mut active: email_providers::ActiveModel = existing.clone().into();

        if let Some(name) = request.name {
            if name.trim().is_empty() {
                return Err(EmailError::Validation(
                    "Provider name cannot be empty".to_string(),
                ));
            }
            if name != existing.name {
                active.name = Set(name);
                changed_fields.push("name".to_string());
            }
        }

        if let Some(region) = request.region {
            if region != existing.region {
                active.region = Set(region);
                changed_fields.push("region".to_string());
            }
        }

        if let Some(is_active) = request.is_active {
            if is_active != existing.is_active {
                active.is_active = Set(is_active);
                changed_fields.push("is_active".to_string());
            }
        }

        if let Some(new_credentials) = request.credentials {
            let new_type = new_credentials.provider_type();
            if new_type != existing_type {
                return Err(EmailError::Validation(format!(
                    "Cannot change provider type from {} to {} on existing provider (id={})",
                    existing_type, new_type, id
                )));
            }
            let credentials_json = match &new_credentials {
                ProviderCredentials::Ses(c) => serde_json::to_string(c)?,
                ProviderCredentials::Scaleway(c) => serde_json::to_string(c)?,
                ProviderCredentials::Smtp(c) => serde_json::to_string(c)?,
            };
            let encrypted = self
                .encryption_service
                .encrypt_string(&credentials_json)
                .map_err(|e| EmailError::Encryption(e.to_string()))?;
            active.credentials = Set(encrypted);
            changed_fields.push("credentials".to_string());
        }

        // Skip the DB roundtrip if nothing changed.
        if changed_fields.is_empty() {
            return Ok(UpdateProviderOutcome {
                provider: existing,
                changed_fields,
            });
        }

        let updated = active.update(self.db.as_ref()).await?;
        debug!(
            "Updated email provider {} (changed fields: {:?})",
            id, changed_fields
        );
        Ok(UpdateProviderOutcome {
            provider: updated,
            changed_fields,
        })
    }

    /// Create an email provider instance from a database model
    pub async fn create_provider_instance(
        &self,
        provider: &email_providers::Model,
    ) -> Result<Box<dyn EmailProvider>, EmailError> {
        // Decrypt credentials
        let credentials_json = self
            .encryption_service
            .decrypt_string(&provider.credentials)
            .map_err(|e| EmailError::Decryption(e.to_string()))?;

        let provider_type = EmailProviderType::from_str(&provider.provider_type)?;

        match provider_type {
            EmailProviderType::Ses => {
                let credentials: SesCredentials = serde_json::from_str(&credentials_json)?;
                let ses_provider = SesProvider::new(&credentials, &provider.region)
                    .await
                    .map_err(|e| {
                        error!("Failed to create SES provider: {}", e);
                        e
                    })?;
                Ok(Box::new(ses_provider))
            }
            EmailProviderType::Scaleway => {
                let credentials: ScalewayCredentials = serde_json::from_str(&credentials_json)?;
                let scaleway_provider = ScalewayProvider::new(&credentials, &provider.region)
                    .map_err(|e| {
                        error!("Failed to create Scaleway provider: {}", e);
                        e
                    })?;
                Ok(Box::new(scaleway_provider))
            }
            EmailProviderType::Smtp => {
                let credentials: SmtpCredentials = serde_json::from_str(&credentials_json)?;
                let smtp_provider = SmtpProvider::new(&credentials).map_err(|e| {
                    error!("Failed to create SMTP provider: {}", e);
                    e
                })?;
                Ok(Box::new(smtp_provider))
            }
        }
    }

    /// Send a test email to verify provider configuration
    ///
    /// This sends a simple test email to the specified recipient to verify
    /// that the provider credentials are valid and the provider can send emails.
    ///
    /// Note: This bypasses domain verification and sends directly through the provider.
    /// The provider must have the ability to send from any address (e.g., SES sandbox mode
    /// may require verified sender addresses).
    ///
    /// # Arguments
    /// * `provider_id` - The ID of the provider to test
    /// * `recipient_email` - The email address to send the test email to
    /// * `from_address` - The sender email address (must be verified with the provider)
    /// * `from_name` - Optional sender display name
    pub async fn send_test_email(
        &self,
        provider_id: i32,
        recipient_email: &str,
        from_address: &str,
        from_name: Option<&str>,
    ) -> Result<TestEmailResult, EmailError> {
        use crate::providers::SendEmailRequest as ProviderSendRequest;

        debug!(
            "Sending test email from provider {} ({}) to {}",
            provider_id, from_address, recipient_email
        );

        // Get the provider
        let provider = self.get(provider_id).await?;

        // Create provider instance
        let provider_instance = self.create_provider_instance(&provider).await?;

        // Create a branded test email matching the notification provider design.
        let timestamp = chrono::Utc::now()
            .format("%Y-%m-%d %H:%M:%S UTC")
            .to_string();
        let provider_type_label = pretty_provider_type(&provider.provider_type);
        let test_request = ProviderSendRequest {
            from: from_address.to_string(),
            from_name: from_name.map(|s| s.to_string()),
            to: vec![recipient_email.to_string()],
            cc: None,
            bcc: None,
            reply_to: None,
            subject: format!("[Temps] Email provider test — {}", provider.name),
            html: Some(render_test_email_html(
                &provider.name,
                &provider_type_label,
                &provider.region,
                &timestamp,
            )),
            text: Some(render_test_email_text(
                &provider.name,
                &provider_type_label,
                &provider.region,
                &timestamp,
            )),
            headers: None,
        };

        // Try to send the email
        match provider_instance.send(&test_request).await {
            Ok(response) => {
                debug!(
                    "Test email sent successfully, message_id: {}",
                    response.message_id
                );
                Ok(TestEmailResult {
                    success: true,
                    recipient_email: recipient_email.to_string(),
                    provider_message_id: Some(response.message_id),
                    error: None,
                })
            }
            Err(e) => {
                error!("Failed to send test email: {}", e);
                Ok(TestEmailResult {
                    success: false,
                    recipient_email: recipient_email.to_string(),
                    provider_message_id: None,
                    error: Some(e.to_string()),
                })
            }
        }
    }

    /// Get decrypted credentials for a provider (for display purposes, masked)
    pub fn get_masked_credentials(
        &self,
        provider: &email_providers::Model,
    ) -> Result<serde_json::Value, EmailError> {
        let credentials_json = self
            .encryption_service
            .decrypt_string(&provider.credentials)
            .map_err(|e| EmailError::Decryption(e.to_string()))?;

        let provider_type = EmailProviderType::from_str(&provider.provider_type)?;

        match provider_type {
            EmailProviderType::Ses => {
                let credentials: SesCredentials = serde_json::from_str(&credentials_json)?;
                Ok(serde_json::json!({
                    "access_key_id": mask_string(&credentials.access_key_id),
                    "secret_access_key": "***"
                }))
            }
            EmailProviderType::Scaleway => {
                let credentials: ScalewayCredentials = serde_json::from_str(&credentials_json)?;
                Ok(serde_json::json!({
                    "api_key": "***",
                    "project_id": credentials.project_id
                }))
            }
            EmailProviderType::Smtp => {
                let credentials: SmtpCredentials = serde_json::from_str(&credentials_json)?;
                Ok(serde_json::json!({
                    "host": credentials.host,
                    "port": credentials.port,
                    "username": credentials
                        .username
                        .as_deref()
                        .map(mask_string)
                        .unwrap_or_default(),
                    "password": credentials.password.as_ref().map(|_| "***".to_string()),
                    "encryption": credentials.encryption,
                    "accept_invalid_certs": credentials.accept_invalid_certs,
                }))
            }
        }
    }
}

/// Mask a string, showing only first 4 and last 4 characters
fn mask_string(s: &str) -> String {
    if s.len() <= 8 {
        "***".to_string()
    } else {
        format!("{}...{}", &s[..4], &s[s.len() - 4..])
    }
}

/// Human-readable label for a stored provider_type string. Falls back to the
/// uppercased raw value for unknown variants so future additions still render.
fn pretty_provider_type(raw: &str) -> String {
    match raw.to_lowercase().as_str() {
        "ses" => "AWS SES".to_string(),
        "scaleway" => "Scaleway".to_string(),
        "smtp" => "SMTP".to_string(),
        other => other.to_uppercase(),
    }
}

/// HTML-escape the small set of characters that matter inside an email body.
/// Values shown in the test email come from the database (provider name,
/// region) and are user-controlled, so we escape them to keep the message
/// HTML-safe.
fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Render the HTML body of the "test email provider" message. Visually
/// matches the notification template (header bar, success badge, title,
/// details table, footer) so it looks like a real Temps email rather than
/// a debug ping. Kept as a free function so unit tests can call it without
/// a `ProviderService` instance.
pub(crate) fn render_test_email_html(
    provider_name: &str,
    provider_type_label: &str,
    region: &str,
    timestamp: &str,
) -> String {
    // "Success" palette — same accent/background as a positive notification.
    let accent_color = "#16a34a";
    let bg_color = "#ecfdf5";
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Email provider test</title>
</head>
<body style="margin: 0; padding: 0; background-color: #f3f4f6; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif; -webkit-font-smoothing: antialiased;">
    <table width="100%" cellpadding="0" cellspacing="0" style="background-color: #f3f4f6; padding: 32px 16px; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif;">
        <tr><td align="center">
            <table width="600" cellpadding="0" cellspacing="0" style="max-width: 600px; width: 100%;">
                <!-- Header -->
                <tr><td style="padding: 24px 32px; background: #0f172a; border-radius: 8px 8px 0 0;">
                    <table width="100%" cellpadding="0" cellspacing="0">
                        <tr>
                            <td style="font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; font-size: 18px; font-weight: 700; color: #ffffff; letter-spacing: -0.02em;">Temps</td>
                            <td align="right" style="font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; font-size: 12px; color: #94a3b8;">{timestamp}</td>
                        </tr>
                    </table>
                </td></tr>

                <!-- Status Badge -->
                <tr><td style="padding: 24px 32px 0; background: #ffffff;">
                    <table cellpadding="0" cellspacing="0">
                        <tr><td style="padding: 4px 12px; background: {bg_color}; border: 1px solid {accent_color}22; border-radius: 100px;">
                            <span style="font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; font-size: 12px; font-weight: 600; color: {accent_color};">&#10003; Test email</span>
                        </td></tr>
                    </table>
                </td></tr>

                <!-- Title -->
                <tr><td style="padding: 12px 32px 0; background: #ffffff;">
                    <h1 style="margin: 0; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; font-size: 20px; font-weight: 600; color: #111827; line-height: 1.4;">Email provider is working</h1>
                </td></tr>

                <!-- Message -->
                <tr><td style="padding: 16px 32px 8px; background: #ffffff;">
                    <p style="margin: 0; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; font-size: 14px; color: #374151; line-height: 1.7;">
                        Your Temps email provider just delivered this message successfully. The provider is configured correctly and ready to send transactional and notification email. No action is required.
                    </p>
                </td></tr>

                <!-- Provider details -->
                <tr><td style="padding: 12px 32px 24px; background: #ffffff;">
                    <table width="100%" cellpadding="0" cellspacing="0" style="background: #f9fafb; border: 1px solid #e5e7eb; border-radius: 6px;">
                        <tr>
                            <td style="padding: 10px 14px; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; font-size: 12px; color: #6b7280; white-space: nowrap; vertical-align: top; width: 130px;">Provider name</td>
                            <td style="padding: 10px 14px; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; font-size: 13px; color: #111827; font-weight: 600; word-break: break-all;">{provider_name}</td>
                        </tr>
                        <tr>
                            <td style="padding: 10px 14px; border-top: 1px solid #e5e7eb; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; font-size: 12px; color: #6b7280; white-space: nowrap; vertical-align: top;">Provider type</td>
                            <td style="padding: 10px 14px; border-top: 1px solid #e5e7eb; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; font-size: 13px; color: #111827; font-weight: 600;">{provider_type_label}</td>
                        </tr>
                        <tr>
                            <td style="padding: 10px 14px; border-top: 1px solid #e5e7eb; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; font-size: 12px; color: #6b7280; white-space: nowrap; vertical-align: top;">Region</td>
                            <td style="padding: 10px 14px; border-top: 1px solid #e5e7eb; font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; font-size: 13px; color: #111827; word-break: break-all;">{region}</td>
                        </tr>
                        <tr>
                            <td style="padding: 10px 14px; border-top: 1px solid #e5e7eb; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; font-size: 12px; color: #6b7280; white-space: nowrap; vertical-align: top;">Test time</td>
                            <td style="padding: 10px 14px; border-top: 1px solid #e5e7eb; font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; font-size: 13px; color: #111827;">{timestamp}</td>
                        </tr>
                    </table>
                </td></tr>

                <!-- Footer -->
                <tr><td style="padding: 16px 32px; background: #f9fafb; border-top: 1px solid #e5e7eb; border-radius: 0 0 8px 8px;">
                    <table width="100%" cellpadding="0" cellspacing="0">
                        <tr>
                            <td style="font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; font-size: 12px; color: #9ca3af;">Sent by Temps &middot; Self-hosted PaaS</td>
                            <td align="right" style="font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; font-size: 12px; color: #9ca3af;">Email provider test</td>
                        </tr>
                    </table>
                </td></tr>
            </table>
        </td></tr>
    </table>
</body>
</html>"#,
        provider_name = escape_html(provider_name),
        provider_type_label = escape_html(provider_type_label),
        region = escape_html(region),
        timestamp = escape_html(timestamp),
        accent_color = accent_color,
        bg_color = bg_color,
    )
}

/// Plain-text alternative for the test email. Kept terse on purpose —
/// the HTML body carries the full design; the plaintext is the fallback.
pub(crate) fn render_test_email_text(
    provider_name: &str,
    provider_type_label: &str,
    region: &str,
    timestamp: &str,
) -> String {
    format!(
        "Email provider test successful\n\
         \n\
         Your Temps email provider just delivered this message successfully. \
         The provider is configured correctly and ready to send transactional \
         and notification email. No action is required.\n\
         \n\
         Provider name:  {provider_name}\n\
         Provider type:  {provider_type_label}\n\
         Region:         {region}\n\
         Test time:      {timestamp}\n\
         \n\
         Sent by Temps · Self-hosted PaaS\n",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use temps_database::test_utils::TestDatabase;

    // Helper to create a test encryption service
    fn create_test_encryption_service() -> Arc<EncryptionService> {
        // 32-byte hex key for testing
        let key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        Arc::new(EncryptionService::new(key).unwrap())
    }

    // Helper to setup test environment with real database
    async fn setup_test_env() -> (TestDatabase, ProviderService) {
        let db = TestDatabase::with_migrations().await.unwrap();
        let encryption_service = create_test_encryption_service();
        let service = ProviderService::new(db.db.clone(), encryption_service);
        (db, service)
    }

    // ========== Unit Tests (no database required) ==========

    #[test]
    fn test_mask_string() {
        assert_eq!(mask_string("short"), "***");
        assert_eq!(mask_string("AKIAIOSFODNN7EXAMPLE"), "AKIA...MPLE");
        assert_eq!(mask_string("12345678"), "***"); // Exactly 8 chars
        assert_eq!(mask_string("123456789"), "1234...6789"); // 9 chars
    }

    #[test]
    fn test_pretty_provider_type() {
        assert_eq!(pretty_provider_type("ses"), "AWS SES");
        assert_eq!(pretty_provider_type("SES"), "AWS SES");
        assert_eq!(pretty_provider_type("scaleway"), "Scaleway");
        assert_eq!(pretty_provider_type("smtp"), "SMTP");
        // Unknown variants render as uppercase — future providers still look OK.
        assert_eq!(pretty_provider_type("postmark"), "POSTMARK");
    }

    #[test]
    fn test_escape_html_handles_dangerous_characters() {
        assert_eq!(
            escape_html("<script>alert('x')</script>"),
            "&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt;"
        );
        assert_eq!(escape_html("AT&T \"prod\""), "AT&amp;T &quot;prod&quot;");
        // Plain ASCII passes through unchanged.
        assert_eq!(escape_html("us-east-1"), "us-east-1");
    }

    #[test]
    fn test_render_test_email_html_is_branded_and_safe() {
        let html = render_test_email_html(
            "Production SES",
            "AWS SES",
            "eu-west-1",
            "2026-05-27 12:23:51 UTC",
        );

        // Brand: header bar + footer attribution must be present so it looks
        // like a real Temps email rather than a debug ping.
        assert!(html.contains(">Temps<"), "expected Temps header");
        assert!(
            html.contains("Sent by Temps"),
            "expected footer attribution"
        );
        assert!(html.contains("Email provider is working"), "expected title");
        assert!(html.contains("Test email"), "expected success badge");

        // Provider details surface — operator needs all four lines.
        assert!(html.contains("Production SES"));
        assert!(html.contains("AWS SES"));
        assert!(html.contains("eu-west-1"));
        assert!(html.contains("2026-05-27 12:23:51 UTC"));

        // Inline-styled <table> layout — Outlook-safe.
        assert!(html.contains("<table"));
        assert!(!html.contains("display: flex"));
        assert!(!html.contains("display: grid"));
    }

    #[test]
    fn test_render_test_email_html_escapes_provider_name() {
        let html = render_test_email_html(
            "<script>alert(1)</script>",
            "SMTP",
            "us-east-1",
            "2026-05-27 12:00:00 UTC",
        );
        // The raw script tag must never appear in the body — would otherwise
        // execute in any email client that renders HTML inline (most don't,
        // but we don't rely on that).
        assert!(
            !html.contains("<script>alert(1)</script>"),
            "raw user-controlled HTML must be escaped"
        );
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    }

    #[test]
    fn test_render_test_email_text_includes_provider_details() {
        let text = render_test_email_text(
            "Production SES",
            "AWS SES",
            "eu-west-1",
            "2026-05-27 12:23:51 UTC",
        );
        assert!(text.contains("Email provider test successful"));
        assert!(text.contains("Production SES"));
        assert!(text.contains("AWS SES"));
        assert!(text.contains("eu-west-1"));
        assert!(text.contains("2026-05-27 12:23:51 UTC"));
        assert!(text.contains("Sent by Temps"));
    }

    #[test]
    fn test_create_provider_request_ses() {
        let credentials = ProviderCredentials::Ses(SesCredentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
            endpoint_url: None,
        });

        let request = CreateProviderRequest {
            name: "My SES Provider".to_string(),
            provider_type: EmailProviderType::Ses,
            region: "us-east-1".to_string(),
            credentials,
        };

        assert_eq!(request.name, "My SES Provider");
        assert_eq!(request.provider_type, EmailProviderType::Ses);
        assert_eq!(request.region, "us-east-1");
    }

    #[test]
    fn test_create_provider_request_scaleway() {
        let credentials = ProviderCredentials::Scaleway(ScalewayCredentials {
            api_key: "scw-api-key-example".to_string(),
            project_id: "project-123".to_string(),
        });

        let request = CreateProviderRequest {
            name: "My Scaleway Provider".to_string(),
            provider_type: EmailProviderType::Scaleway,
            region: "fr-par".to_string(),
            credentials,
        };

        assert_eq!(request.name, "My Scaleway Provider");
        assert_eq!(request.provider_type, EmailProviderType::Scaleway);
        assert_eq!(request.region, "fr-par");
    }

    #[test]
    fn test_email_provider_type_display() {
        assert_eq!(format!("{}", EmailProviderType::Ses), "ses");
        assert_eq!(format!("{}", EmailProviderType::Scaleway), "scaleway");
    }

    #[test]
    fn test_email_provider_type_from_str() {
        assert_eq!(
            EmailProviderType::from_str("ses").unwrap(),
            EmailProviderType::Ses
        );
        assert_eq!(
            EmailProviderType::from_str("scaleway").unwrap(),
            EmailProviderType::Scaleway
        );
        assert!(EmailProviderType::from_str("invalid").is_err());
    }

    #[test]
    fn test_get_masked_credentials_ses() {
        let encryption_service = create_test_encryption_service();

        // Create and encrypt real credentials
        let credentials = SesCredentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
            endpoint_url: None,
        };
        let credentials_json = serde_json::to_string(&credentials).unwrap();
        let encrypted = encryption_service
            .encrypt_string(&credentials_json)
            .unwrap();

        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = ProviderService::new(Arc::new(db), encryption_service);

        let provider = email_providers::Model {
            id: 1,
            name: "Test".to_string(),
            provider_type: "ses".to_string(),
            region: "us-east-1".to_string(),
            credentials: encrypted,
            is_active: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        let result = service.get_masked_credentials(&provider);

        assert!(result.is_ok());
        let masked = result.unwrap();
        assert_eq!(masked["access_key_id"], "AKIA...MPLE");
        assert_eq!(masked["secret_access_key"], "***");
    }

    #[test]
    fn test_get_masked_credentials_scaleway() {
        let encryption_service = create_test_encryption_service();

        // Create and encrypt real credentials
        let credentials = ScalewayCredentials {
            api_key: "scw-api-key-example-12345".to_string(),
            project_id: "project-123".to_string(),
        };
        let credentials_json = serde_json::to_string(&credentials).unwrap();
        let encrypted = encryption_service
            .encrypt_string(&credentials_json)
            .unwrap();

        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let service = ProviderService::new(Arc::new(db), encryption_service);

        let provider = email_providers::Model {
            id: 1,
            name: "Test".to_string(),
            provider_type: "scaleway".to_string(),
            region: "fr-par".to_string(),
            credentials: encrypted,
            is_active: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        let result = service.get_masked_credentials(&provider);

        assert!(result.is_ok());
        let masked = result.unwrap();
        assert_eq!(masked["api_key"], "***");
        assert_eq!(masked["project_id"], "project-123");
    }

    // ========== Integration Tests (require Docker) ==========

    #[tokio::test]
    async fn test_create_provider() {
        let (_db, service) = setup_test_env().await;

        let request = CreateProviderRequest {
            name: "Test SES Provider".to_string(),
            provider_type: EmailProviderType::Ses,
            region: "us-east-1".to_string(),
            credentials: ProviderCredentials::Ses(SesCredentials {
                access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
                secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
                endpoint_url: None,
            }),
        };

        let result = service.create(request).await;

        assert!(result.is_ok());
        let provider = result.unwrap();
        assert!(provider.id > 0);
        assert_eq!(provider.name, "Test SES Provider");
        assert_eq!(provider.provider_type, "ses");
        assert_eq!(provider.region, "us-east-1");
        assert!(provider.is_active);
    }

    #[tokio::test]
    async fn test_get_provider() {
        let (_db, service) = setup_test_env().await;

        // Create a provider first
        let request = CreateProviderRequest {
            name: "Test Provider".to_string(),
            provider_type: EmailProviderType::Ses,
            region: "us-east-1".to_string(),
            credentials: ProviderCredentials::Ses(SesCredentials {
                access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
                secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
                endpoint_url: None,
            }),
        };
        let created = service.create(request).await.unwrap();

        // Get the provider
        let result = service.get(created.id).await;

        assert!(result.is_ok());
        let provider = result.unwrap();
        assert_eq!(provider.id, created.id);
        assert_eq!(provider.name, "Test Provider");
    }

    #[tokio::test]
    async fn test_get_provider_not_found() {
        let (_db, service) = setup_test_env().await;

        let result = service.get(999999).await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            EmailError::ProviderNotFound(999999)
        ));
    }

    #[tokio::test]
    async fn test_list_providers() {
        let (_db, service) = setup_test_env().await;

        // Create multiple providers
        let request1 = CreateProviderRequest {
            name: "Provider 1".to_string(),
            provider_type: EmailProviderType::Ses,
            region: "us-east-1".to_string(),
            credentials: ProviderCredentials::Ses(SesCredentials {
                access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
                secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
                endpoint_url: None,
            }),
        };
        service.create(request1).await.unwrap();

        let request2 = CreateProviderRequest {
            name: "Provider 2".to_string(),
            provider_type: EmailProviderType::Scaleway,
            region: "fr-par".to_string(),
            credentials: ProviderCredentials::Scaleway(ScalewayCredentials {
                api_key: "scw-api-key".to_string(),
                project_id: "project-123".to_string(),
            }),
        };
        service.create(request2).await.unwrap();

        // List all providers
        let result = service.list().await;

        assert!(result.is_ok());
        let providers = result.unwrap();
        assert_eq!(providers.len(), 2);
    }

    #[tokio::test]
    async fn test_list_active_providers() {
        let (_db, service) = setup_test_env().await;

        // Create a provider
        let request = CreateProviderRequest {
            name: "Active Provider".to_string(),
            provider_type: EmailProviderType::Ses,
            region: "us-east-1".to_string(),
            credentials: ProviderCredentials::Ses(SesCredentials {
                access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
                secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
                endpoint_url: None,
            }),
        };
        let created = service.create(request).await.unwrap();

        // Create another and deactivate it
        let request2 = CreateProviderRequest {
            name: "Inactive Provider".to_string(),
            provider_type: EmailProviderType::Ses,
            region: "us-west-2".to_string(),
            credentials: ProviderCredentials::Ses(SesCredentials {
                access_key_id: "AKIAIOSFODNN7EXAMPLE2".to_string(),
                secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY2".to_string(),
                endpoint_url: None,
            }),
        };
        let created2 = service.create(request2).await.unwrap();
        service.set_active(created2.id, false).await.unwrap();

        // List only active providers
        let result = service.list_active().await;

        assert!(result.is_ok());
        let providers = result.unwrap();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].id, created.id);
        assert!(providers[0].is_active);
    }

    #[tokio::test]
    async fn test_delete_provider() {
        let (_db, service) = setup_test_env().await;

        // Create a provider
        let request = CreateProviderRequest {
            name: "To Delete".to_string(),
            provider_type: EmailProviderType::Ses,
            region: "us-east-1".to_string(),
            credentials: ProviderCredentials::Ses(SesCredentials {
                access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
                secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
                endpoint_url: None,
            }),
        };
        let created = service.create(request).await.unwrap();

        // Delete it
        let result = service.delete(created.id).await;
        assert!(result.is_ok());

        // Verify it's gone
        let get_result = service.get(created.id).await;
        assert!(get_result.is_err());
    }

    #[tokio::test]
    async fn test_set_active() {
        let (_db, service) = setup_test_env().await;

        // Create a provider (active by default)
        let request = CreateProviderRequest {
            name: "Test Provider".to_string(),
            provider_type: EmailProviderType::Ses,
            region: "us-east-1".to_string(),
            credentials: ProviderCredentials::Ses(SesCredentials {
                access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
                secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
                endpoint_url: None,
            }),
        };
        let created = service.create(request).await.unwrap();
        assert!(created.is_active);

        // Deactivate it
        let result = service.set_active(created.id, false).await;
        assert!(result.is_ok());
        let updated = result.unwrap();
        assert!(!updated.is_active);

        // Reactivate it
        let result = service.set_active(created.id, true).await;
        assert!(result.is_ok());
        let updated = result.unwrap();
        assert!(updated.is_active);
    }

    // ========== Tests for update() ==========

    fn ses_creds() -> ProviderCredentials {
        ProviderCredentials::Ses(SesCredentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
            endpoint_url: None,
        })
    }

    #[test]
    fn test_provider_credentials_type_helper() {
        assert_eq!(ses_creds().provider_type(), EmailProviderType::Ses);
        assert_eq!(
            ProviderCredentials::Scaleway(ScalewayCredentials {
                api_key: "k".to_string(),
                project_id: "p".to_string(),
            })
            .provider_type(),
            EmailProviderType::Scaleway,
        );
        assert_eq!(
            ProviderCredentials::Smtp(crate::providers::SmtpCredentials {
                host: "h".to_string(),
                port: 587,
                username: None,
                password: None,
                encryption: crate::providers::SmtpEncryption::Starttls,
                accept_invalid_certs: false,
            })
            .provider_type(),
            EmailProviderType::Smtp,
        );
    }

    #[tokio::test]
    async fn test_update_renames_provider() {
        let (_db, service) = setup_test_env().await;
        let created = service
            .create(CreateProviderRequest {
                name: "Original".to_string(),
                provider_type: EmailProviderType::Ses,
                region: "us-east-1".to_string(),
                credentials: ses_creds(),
            })
            .await
            .unwrap();

        let outcome = service
            .update(
                created.id,
                UpdateProviderRequest {
                    name: Some("Renamed".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(outcome.provider.name, "Renamed");
        assert_eq!(outcome.changed_fields, vec!["name".to_string()]);
        // Credentials blob must not change when only name is updated.
        assert_eq!(outcome.provider.credentials, created.credentials);
    }

    #[tokio::test]
    async fn test_update_with_no_changes_is_noop() {
        let (_db, service) = setup_test_env().await;
        let created = service
            .create(CreateProviderRequest {
                name: "Same".to_string(),
                provider_type: EmailProviderType::Ses,
                region: "us-east-1".to_string(),
                credentials: ses_creds(),
            })
            .await
            .unwrap();

        let outcome = service
            .update(
                created.id,
                UpdateProviderRequest {
                    name: Some("Same".to_string()),
                    region: Some("us-east-1".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert!(outcome.changed_fields.is_empty());
        assert_eq!(outcome.provider.id, created.id);
    }

    #[tokio::test]
    async fn test_update_rejects_empty_name() {
        let (_db, service) = setup_test_env().await;
        let created = service
            .create(CreateProviderRequest {
                name: "Original".to_string(),
                provider_type: EmailProviderType::Ses,
                region: "us-east-1".to_string(),
                credentials: ses_creds(),
            })
            .await
            .unwrap();

        let err = service
            .update(
                created.id,
                UpdateProviderRequest {
                    name: Some("   ".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, EmailError::Validation(_)));
    }

    #[tokio::test]
    async fn test_update_rejects_provider_type_mismatch() {
        let (_db, service) = setup_test_env().await;
        let created = service
            .create(CreateProviderRequest {
                name: "SES provider".to_string(),
                provider_type: EmailProviderType::Ses,
                region: "us-east-1".to_string(),
                credentials: ses_creds(),
            })
            .await
            .unwrap();

        // Try to swap SES credentials for Scaleway ones — must fail.
        let err = service
            .update(
                created.id,
                UpdateProviderRequest {
                    credentials: Some(ProviderCredentials::Scaleway(ScalewayCredentials {
                        api_key: "k".to_string(),
                        project_id: "p".to_string(),
                    })),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, EmailError::Validation(_)));
    }

    #[tokio::test]
    async fn test_update_rotates_credentials_when_supplied() {
        let (_db, service) = setup_test_env().await;
        let created = service
            .create(CreateProviderRequest {
                name: "SES".to_string(),
                provider_type: EmailProviderType::Ses,
                region: "us-east-1".to_string(),
                credentials: ses_creds(),
            })
            .await
            .unwrap();

        let new_creds = ProviderCredentials::Ses(SesCredentials {
            access_key_id: "AKIAROTATEDKEY123456".to_string(),
            secret_access_key: "newsecretrotatedvaluevaluevaluevaluevalue".to_string(),
            endpoint_url: None,
        });

        let outcome = service
            .update(
                created.id,
                UpdateProviderRequest {
                    credentials: Some(new_creds),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(outcome.changed_fields, vec!["credentials".to_string()]);
        assert_ne!(
            outcome.provider.credentials, created.credentials,
            "rotated credentials should re-encrypt to a different ciphertext"
        );

        // The masked response should expose the new prefix.
        let masked = service.get_masked_credentials(&outcome.provider).unwrap();
        assert_eq!(masked["access_key_id"], "AKIA...3456");
    }

    #[tokio::test]
    async fn test_update_preserves_credentials_when_omitted() {
        let (_db, service) = setup_test_env().await;
        let created = service
            .create(CreateProviderRequest {
                name: "SES".to_string(),
                provider_type: EmailProviderType::Ses,
                region: "us-east-1".to_string(),
                credentials: ses_creds(),
            })
            .await
            .unwrap();

        let outcome = service
            .update(
                created.id,
                UpdateProviderRequest {
                    region: Some("eu-west-1".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert!(outcome.changed_fields.contains(&"region".to_string()));
        assert!(
            !outcome.changed_fields.contains(&"credentials".to_string()),
            "credentials must not be listed when caller didn't supply them"
        );
        assert_eq!(
            outcome.provider.credentials, created.credentials,
            "encrypted blob must be byte-identical when caller omits credentials"
        );
    }

    // ========== Unit Tests for TestEmailResult ==========

    #[test]
    fn test_email_result_success() {
        let result = TestEmailResult {
            success: true,
            recipient_email: "test@example.com".to_string(),
            provider_message_id: Some("msg-123".to_string()),
            error: None,
        };

        assert!(result.success);
        assert_eq!(result.recipient_email, "test@example.com");
        assert_eq!(result.provider_message_id, Some("msg-123".to_string()));
        assert!(result.error.is_none());
    }

    #[test]
    fn test_email_result_failure() {
        let result = TestEmailResult {
            success: false,
            recipient_email: "test@example.com".to_string(),
            provider_message_id: None,
            error: Some("Connection refused".to_string()),
        };

        assert!(!result.success);
        assert_eq!(result.recipient_email, "test@example.com");
        assert!(result.provider_message_id.is_none());
        assert_eq!(result.error, Some("Connection refused".to_string()));
    }

    // ========== Integration Tests for send_test_email ==========

    #[tokio::test]
    async fn test_send_test_email_provider_not_found() {
        let (_db, service) = setup_test_env().await;

        // Attempt to send test email for non-existent provider
        let result = service
            .send_test_email(
                999999,
                "test@example.com",
                "sender@example.com",
                Some("Test Sender"),
            )
            .await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            EmailError::ProviderNotFound(999999)
        ));
    }

    #[tokio::test]
    async fn test_send_test_email_with_invalid_credentials() {
        let (_db, service) = setup_test_env().await;

        // Create a provider with fake credentials
        let request = CreateProviderRequest {
            name: "Test Provider".to_string(),
            provider_type: EmailProviderType::Ses,
            region: "us-east-1".to_string(),
            credentials: ProviderCredentials::Ses(SesCredentials {
                access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
                secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
                endpoint_url: None,
            }),
        };
        let provider = service.create(request).await.unwrap();

        // Attempt to send test email - this will create a provider instance
        // but the send will fail because the credentials are fake
        // The function should return a result with success=false, not an error
        let result = service
            .send_test_email(
                provider.id,
                "test@example.com",
                "sender@example.com",
                Some("Test Sender"),
            )
            .await;

        // The function should succeed (return Ok) but the result should indicate failure
        // This is because we gracefully handle send errors as failed test results
        assert!(result.is_ok());
        let test_result = result.unwrap();
        assert!(!test_result.success); // Email send failed due to invalid credentials
        assert_eq!(test_result.recipient_email, "test@example.com");
        assert!(test_result.error.is_some()); // Should have an error message
    }

    // ========== LocalStack Integration Tests ==========
    //
    // These tests use LocalStack to test actual AWS SES integration without
    // requiring a real AWS account. They require Docker to be running.
    //
    // To run these tests:
    //   cargo test --lib -p temps-email test_localstack -- --nocapture
    //
    // The tests will be skipped if Docker is not available.

    /// Helper to check if Docker is available
    fn is_docker_available() -> bool {
        std::process::Command::new("docker")
            .arg("info")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Helper struct to hold LocalStack container and connection details
    struct LocalStackTestEnv {
        _container: testcontainers::ContainerAsync<testcontainers::GenericImage>,
        endpoint_url: String,
        #[allow(dead_code)]
        port: u16,
    }

    impl LocalStackTestEnv {
        async fn new() -> anyhow::Result<Self> {
            use testcontainers::{runners::AsyncRunner, GenericImage, ImageExt};

            // Start LocalStack container with SES service
            let container = GenericImage::new("localstack/localstack", "latest")
                .with_env_var("SERVICES", "ses")
                .with_env_var("DEBUG", "1")
                .with_env_var("LOCALSTACK_HOST", "localhost.localstack.cloud")
                .start()
                .await?;

            // Get the mapped port for LocalStack (default internal port is 4566)
            let port = container.get_host_port_ipv4(4566).await?;
            let endpoint_url = format!("http://localhost:{}", port);

            // Wait for LocalStack to be ready
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

            Ok(Self {
                _container: container,
                endpoint_url,
                port,
            })
        }
    }

    /// Test sending email via LocalStack SES
    ///
    /// This test verifies that the SES provider can send emails through LocalStack.
    /// LocalStack simulates SES and accepts all emails without validation.
    #[tokio::test]
    async fn test_localstack_ses_send_email() {
        // Skip if Docker is not available
        if !is_docker_available() {
            eprintln!("Skipping test_localstack_ses_send_email: Docker not available");
            return;
        }

        // Start LocalStack
        let localstack = match LocalStackTestEnv::new().await {
            Ok(env) => env,
            Err(e) => {
                eprintln!("Skipping test: Failed to start LocalStack: {}", e);
                return;
            }
        };

        // Setup test database and provider service
        let (_db, service) = setup_test_env().await;

        // Create a provider pointing to LocalStack
        let request = CreateProviderRequest {
            name: "LocalStack SES Provider".to_string(),
            provider_type: EmailProviderType::Ses,
            region: "us-east-1".to_string(),
            credentials: ProviderCredentials::Ses(SesCredentials {
                // LocalStack accepts any credentials
                access_key_id: "test".to_string(),
                secret_access_key: "test".to_string(),
                endpoint_url: Some(localstack.endpoint_url.clone()),
            }),
        };
        let provider = service.create(request).await.unwrap();

        // Verify the provider was created
        assert!(provider.id > 0);
        assert_eq!(provider.name, "LocalStack SES Provider");
        assert_eq!(provider.provider_type, "ses");

        // LocalStack requires email identity to be verified first
        // Let's verify an identity before sending
        let provider_model = service.get(provider.id).await.unwrap();
        let provider_instance = service
            .create_provider_instance(&provider_model)
            .await
            .unwrap();

        // Create/verify a test identity (LocalStack auto-verifies)
        let domain = "test.example.com";
        match provider_instance.create_identity(domain).await {
            Ok(_identity) => {
                debug!("Created identity for {}", domain);
            }
            Err(e) => {
                // LocalStack might not support all SES operations
                debug!("Could not create identity (may be expected): {}", e);
            }
        }

        // Send a test email
        let result = service
            .send_test_email(
                provider.id,
                "recipient@test.example.com",
                "sender@test.example.com",
                Some("Test Sender"),
            )
            .await;

        // Verify the result
        assert!(result.is_ok(), "send_test_email should not return error");
        let test_result = result.unwrap();

        // LocalStack should accept the email
        // Note: The result depends on LocalStack's SES implementation
        // Some versions may return success, others may return specific errors
        println!(
            "LocalStack test email result: success={}, error={:?}",
            test_result.success, test_result.error
        );

        assert_eq!(test_result.recipient_email, "recipient@test.example.com");
    }

    /// Test creating SES provider with LocalStack endpoint
    ///
    /// This test verifies that the SES provider can be created with a custom
    /// endpoint URL pointing to LocalStack.
    #[tokio::test]
    async fn test_localstack_ses_provider_creation() {
        // Skip if Docker is not available
        if !is_docker_available() {
            eprintln!("Skipping test_localstack_ses_provider_creation: Docker not available");
            return;
        }

        // Start LocalStack
        let localstack = match LocalStackTestEnv::new().await {
            Ok(env) => env,
            Err(e) => {
                eprintln!("Skipping test: Failed to start LocalStack: {}", e);
                return;
            }
        };

        // Setup test database and provider service
        let (_db, service) = setup_test_env().await;

        // Create a provider with LocalStack endpoint
        let request = CreateProviderRequest {
            name: "LocalStack Test Provider".to_string(),
            provider_type: EmailProviderType::Ses,
            region: "us-east-1".to_string(),
            credentials: ProviderCredentials::Ses(SesCredentials {
                access_key_id: "test-key".to_string(),
                secret_access_key: "test-secret".to_string(),
                endpoint_url: Some(localstack.endpoint_url.clone()),
            }),
        };

        // Create the provider
        let result = service.create(request).await;
        assert!(result.is_ok());
        let provider = result.unwrap();

        // Verify provider was stored correctly
        assert!(provider.id > 0);
        assert_eq!(provider.name, "LocalStack Test Provider");
        assert_eq!(provider.provider_type, "ses");
        assert_eq!(provider.region, "us-east-1");
        assert!(provider.is_active);

        // Verify we can retrieve it
        let retrieved = service.get(provider.id).await;
        assert!(retrieved.is_ok());
        let retrieved_provider = retrieved.unwrap();
        assert_eq!(retrieved_provider.id, provider.id);

        // Verify we can create a provider instance (which creates the AWS client)
        let instance_result = service.create_provider_instance(&retrieved_provider).await;
        assert!(
            instance_result.is_ok(),
            "Should be able to create provider instance: {:?}",
            instance_result.err()
        );

        // Verify the provider instance has the correct type
        let instance = instance_result.unwrap();
        assert_eq!(instance.provider_type(), EmailProviderType::Ses);
    }

    /// Test SES identity operations with LocalStack
    ///
    /// This test verifies that the SES provider can create and verify domain
    /// identities through LocalStack.
    #[tokio::test]
    async fn test_localstack_ses_identity_operations() {
        // Skip if Docker is not available
        if !is_docker_available() {
            eprintln!("Skipping test_localstack_ses_identity_operations: Docker not available");
            return;
        }

        // Start LocalStack
        let localstack = match LocalStackTestEnv::new().await {
            Ok(env) => env,
            Err(e) => {
                eprintln!("Skipping test: Failed to start LocalStack: {}", e);
                return;
            }
        };

        // Setup test database and provider service
        let (_db, service) = setup_test_env().await;

        // Create a provider with LocalStack endpoint
        let request = CreateProviderRequest {
            name: "LocalStack Identity Test".to_string(),
            provider_type: EmailProviderType::Ses,
            region: "us-east-1".to_string(),
            credentials: ProviderCredentials::Ses(SesCredentials {
                access_key_id: "test-key".to_string(),
                secret_access_key: "test-secret".to_string(),
                endpoint_url: Some(localstack.endpoint_url.clone()),
            }),
        };
        let provider = service.create(request).await.unwrap();

        // Get provider instance
        let provider_model = service.get(provider.id).await.unwrap();
        let provider_instance = service
            .create_provider_instance(&provider_model)
            .await
            .unwrap();

        // Test domain identity creation
        let test_domain = "localstack-test.example.com";
        let identity_result = provider_instance.create_identity(test_domain).await;

        // LocalStack should accept the identity creation
        // The result depends on LocalStack's SES implementation
        match identity_result {
            Ok(identity) => {
                println!("Created identity for {}: {:?}", test_domain, identity);
                assert_eq!(identity.provider_identity_id, test_domain);

                // Verify the identity (LocalStack auto-verifies)
                let verify_result = provider_instance.verify_identity(test_domain).await;
                match verify_result {
                    Ok(status) => {
                        println!("Verification status for {}: {:?}", test_domain, status);
                        // LocalStack may return different statuses
                    }
                    Err(e) => {
                        println!("Verification check failed (may be expected): {}", e);
                    }
                }

                // Clean up - delete the identity
                let delete_result = provider_instance.delete_identity(test_domain).await;
                match delete_result {
                    Ok(_) => println!("Deleted identity for {}", test_domain),
                    Err(e) => println!("Delete failed (may be expected): {}", e),
                }
            }
            Err(e) => {
                // Some LocalStack versions may not fully support SESv2
                println!("Identity creation failed (may be expected): {}", e);
            }
        }
    }
}
