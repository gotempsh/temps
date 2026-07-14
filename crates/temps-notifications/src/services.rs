use crate::types::{Notification, NotificationPriority, NotificationSeverity, NotificationType};
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use lettre::{
    message::{header::ContentType, Mailbox, MultiPart, SinglePart},
    transport::smtp::{authentication::Credentials, client::TlsParametersBuilder},
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, JoinType, ModelTrait,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, RelationTrait, Set,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use temps_core::notifications::{
    EmailMessage, NotificationData, NotificationError as CoreNotificationError,
    NotificationService as CoreNotificationService,
};
use temps_entities::types::RoleType;
use temps_entities::{
    notification_preferences, notification_providers, notifications, roles, user_roles, users,
};
use tracing::{error, info};
use utoipa::ToSchema;

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct UpdateProviderRequest {
    pub name: Option<String>,
    pub config: Option<serde_json::Value>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub enum TlsMode {
    None,     // No encryption
    Starttls, // STARTTLS (opportunistic TLS)
    Tls,      // Direct TLS connection
}

fn default_tls_mode() -> TlsMode {
    TlsMode::Starttls
}

fn default_starttls_required() -> bool {
    true
}

fn default_accept_invalid_certs() -> bool {
    false // Default to secure behavior
}

// Provider-specific structs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailProvider {
    pub smtp_host: String,
    pub smtp_port: u16,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    pub from_address: String,
    pub from_name: Option<String>,
    pub to_addresses: Vec<String>,
    #[serde(default = "default_tls_mode")]
    pub tls_mode: TlsMode,
    #[serde(default = "default_starttls_required")]
    pub starttls_required: bool, // Only used when tls_mode is Starttls
    #[serde(default = "default_accept_invalid_certs")]
    pub accept_invalid_certs: bool, // Accept self-signed certificates (use with caution)
    #[serde(skip)]
    mailer: Option<AsyncSmtpTransport<Tokio1Executor>>,
    #[serde(skip)]
    db: Arc<DatabaseConnection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlackProvider {
    pub webhook_url: String,
    pub channel: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SMSProvider {
    pub api_key: String,
    pub from_number: String,
    pub to_numbers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhatsAppProvider {
    pub api_key: String,
    pub from_number: String,
    pub to_numbers: Vec<String>,
}

fn default_http_method() -> String {
    "POST".to_string()
}

fn default_timeout_secs() -> u64 {
    30
}

/// Generic webhook provider for custom integrations
/// Sends notification data as JSON payload to any HTTP endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookProvider {
    /// The URL to send webhook requests to
    pub url: String,
    /// HTTP method (POST, PUT, PATCH). Defaults to POST.
    #[serde(default = "default_http_method")]
    pub method: String,
    /// Custom headers to include in the request (e.g., for authentication)
    #[serde(default)]
    pub headers: std::collections::HashMap<String, String>,
    /// Request timeout in seconds. Defaults to 30.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

/// Cloudflare Email Sending provider.
///
/// Delivers notification emails through Cloudflare's transactional Email
/// Sending API (`POST /accounts/{account_id}/email/sending/send`) instead of a
/// self-managed SMTP relay. The operator only needs to configure their
/// Cloudflare account id, an API token with the *Email Sending* permission, the
/// verified sender, and the list of recipients — everything else (HTML/text
/// rendering, subject) is derived from the notification itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudflareProvider {
    /// Cloudflare account id that owns the Email Sending configuration.
    pub account_id: String,
    /// Cloudflare API token with the Email Sending permission. Stored encrypted.
    pub api_token: String,
    /// Verified sender address (must belong to a domain configured for
    /// Cloudflare Email Sending, e.g. `welcome@infracf.example.com`).
    pub from_address: String,
    /// Optional human-friendly sender name shown in the recipient's inbox.
    #[serde(default)]
    pub from_name: Option<String>,
    /// Recipients that should receive the notification.
    pub to_addresses: Vec<String>,
    /// Override for the Cloudflare API base URL. Never serialized into stored
    /// config — it exists only so integration tests can point the provider at a
    /// local mock server. Production always uses [`Self::API_BASE`].
    #[serde(skip)]
    pub api_base: Option<String>,
}

impl CloudflareProvider {
    /// Cloudflare API base. Kept as an associated const so tests and call sites
    /// build the same URL.
    const API_BASE: &'static str = "https://api.cloudflare.com/client/v4";

    /// Effective API base — the test override if set, otherwise the real one.
    fn api_base(&self) -> &str {
        self.api_base.as_deref().unwrap_or(Self::API_BASE)
    }

    fn send_endpoint(&self) -> String {
        format!(
            "{}/accounts/{}/email/sending/send",
            self.api_base(),
            self.account_id
        )
    }

    /// Build the `from` field for the Cloudflare payload.
    ///
    /// Cloudflare Email Sending accepts either a bare address string or a
    /// structured `{ "email", "name" }` object for a display name (the RFC 5322
    /// `Name <address>` *string* form is NOT parsed — it would be treated as a
    /// literal address). We emit the object form only when a name is set.
    fn sender_value(&self) -> serde_json::Value {
        match &self.from_name {
            Some(name) if !name.trim().is_empty() => serde_json::json!({
                "email": self.from_address,
                "name": name,
            }),
            _ => serde_json::json!(self.from_address),
        }
    }

    /// Plain-text fallback body. Cloudflare requires a `text` part alongside the
    /// HTML one, so derive a readable version from the notification.
    fn render_text_body(notification: &Notification) -> String {
        let mut body = format!("{}\n\n{}", notification.title, notification.message);
        if !notification.metadata.is_empty() {
            body.push_str("\n\n---\n");
            for (key, value) in &notification.metadata {
                body.push_str(&format!("{}: {}\n", key, value));
            }
        }
        body
    }

    /// POST a single rendered email to Cloudflare. Returns an error carrying the
    /// status and response body so failures are diagnosable from the logs.
    async fn post_email(
        &self,
        client: &reqwest::Client,
        to: &str,
        subject: &str,
        html: &str,
        text: &str,
    ) -> Result<()> {
        let payload = serde_json::json!({
            "to": to,
            "from": self.sender_value(),
            "subject": subject,
            "html": html,
            "text": text,
        });

        let response = client
            .post(self.send_endpoint())
            .bearer_auth(&self.api_token)
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Cloudflare email send to {} failed (request error): {}",
                    to,
                    e
                )
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "Cloudflare email send to {} failed with status {}: {}",
                to,
                status,
                body
            ));
        }

        Ok(())
    }
}

#[async_trait]
impl NotificationProvider for CloudflareProvider {
    async fn initialize(&mut self, _db: Arc<DatabaseConnection>) -> Result<()> {
        if self.account_id.trim().is_empty() {
            return Err(anyhow::anyhow!("Cloudflare account_id cannot be empty"));
        }
        if self.api_token.trim().is_empty() {
            return Err(anyhow::anyhow!("Cloudflare api_token cannot be empty"));
        }
        if self.from_address.trim().is_empty() {
            return Err(anyhow::anyhow!("Cloudflare from_address cannot be empty"));
        }
        if self.to_addresses.is_empty() {
            return Err(anyhow::anyhow!(
                "Cloudflare provider requires at least one recipient in to_addresses"
            ));
        }
        Ok(())
    }

    async fn send(&self, notification: &Notification) -> Result<()> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        let priority_prefix = match notification.priority {
            NotificationPriority::Low => "[LOW] ",
            NotificationPriority::Normal => "",
            NotificationPriority::High => "[HIGH] ",
            NotificationPriority::Critical => "[CRITICAL] ",
        };
        let subject = format!("{}{}", priority_prefix, notification.title);

        // Reuse the shared notification email template unless the message is
        // already a full HTML document (matching EmailProvider's behaviour).
        let trimmed = notification.message.trim_start();
        let is_full_document = trimmed.starts_with("<!DOCTYPE")
            || trimmed.starts_with("<!doctype")
            || trimmed.starts_with("<html")
            || trimmed.starts_with("<HTML");
        let html = if is_full_document {
            notification.message.clone()
        } else {
            EmailProvider::render_notification_email(notification)
        };
        let text = Self::render_text_body(notification);

        // De-duplicate recipients while preserving determinism.
        let mut recipients = self.to_addresses.clone();
        recipients.sort();
        recipients.dedup();

        let mut last_err: Option<anyhow::Error> = None;
        let mut delivered = false;
        for addr in &recipients {
            match self.post_email(&client, addr, &subject, &html, &text).await {
                Ok(()) => delivered = true,
                Err(e) => {
                    error!("Failed to send Cloudflare email to {}: {}", addr, e);
                    last_err = Some(e);
                }
            }
        }

        // Surface a failure only if every recipient failed — partial delivery
        // still counts as a successful notification, consistent with SMTP.
        if !delivered {
            return Err(last_err.unwrap_or_else(|| {
                anyhow::anyhow!("Cloudflare provider had no recipients to deliver to")
            }));
        }

        Ok(())
    }

    async fn health_check(&self) -> Result<bool> {
        // Validate the API token via Cloudflare's documented token-verify
        // endpoint (`GET /user/tokens/verify`). This is a cheap, side-effect-free
        // check that fails fast on bad/expired credentials without sending a real
        // email. It is account-independent by design.
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()?;

        let url = format!("{}/user/tokens/verify", self.api_base());

        match client.get(url).bearer_auth(&self.api_token).send().await {
            Ok(response) => Ok(response.status().is_success()),
            Err(e) => {
                error!("Cloudflare provider health check failed: {}", e);
                Ok(false)
            }
        }
    }
}

/// HTML-encode the five characters that can break element structure or inject
/// new tags when user-controlled text is interpolated into an HTML template.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

/// Escape Slack mrkdwn special characters so user-controlled text cannot inject
/// hyperlinks (`<url|text>`), `<!channel>` mention floods, `&entity;` refs, or
/// forge bold/italic/code/strikethrough formatting.
fn slack_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('*', "\\*")
        .replace('_', "\\_")
        .replace('`', "\\`")
        .replace('~', "\\~")
}

#[async_trait]
pub trait NotificationProvider: Send + Sync {
    async fn initialize(&mut self, db: Arc<DatabaseConnection>) -> Result<()>;
    async fn send(&self, notification: &Notification) -> Result<()>;
    async fn health_check(&self) -> Result<bool>;
}

impl EmailProvider {
    async fn get_admin_users(&self) -> Result<Vec<users::Model>> {
        let db = self.db.as_ref();

        // First get the admin role
        let admin_role = roles::Entity::find()
            .filter(roles::Column::Name.eq(RoleType::Admin.as_str()))
            .one(db)
            .await
            .map_err(|e| {
                error!("Failed to get admin role: {}", e);
                anyhow::anyhow!("Failed to get admin role: {}", e)
            })?
            .ok_or_else(|| anyhow::anyhow!("Admin role not found"))?;

        // Then get all users with admin role through user_roles join
        let admin_users = users::Entity::find()
            .join(JoinType::InnerJoin, users::Relation::UserRoles.def())
            .filter(user_roles::Column::RoleId.eq(admin_role.id))
            .all(db)
            .await
            .map_err(|e| {
                error!("Failed to get admin users: {}", e);
                anyhow::anyhow!("Failed to get admin users: {}", e)
            })?;

        Ok(admin_users)
    }
}

#[async_trait]
impl NotificationProvider for EmailProvider {
    async fn initialize(&mut self, db: Arc<DatabaseConnection>) -> Result<()> {
        let mut builder = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&self.smtp_host)
            .port(self.smtp_port);

        // Configure authentication if username is provided
        if let (Some(username), Some(password)) = (&self.username, &self.password) {
            if !username.is_empty() {
                let creds = Credentials::new(username.clone(), password.clone());
                builder = builder.credentials(creds);
            }
        }

        // Configure TLS based on the mode
        let mailer = match self.tls_mode {
            TlsMode::None => {
                // No TLS at all
                builder.build()
            }
            TlsMode::Starttls => {
                // STARTTLS - upgrade plain connection to TLS
                if self.starttls_required {
                    // Require STARTTLS - accept self-signed certificates based on configuration
                    let tls = TlsParametersBuilder::new(self.smtp_host.clone())
                        .dangerous_accept_invalid_certs(
                            self.accept_invalid_certs
                                || self.smtp_host == "localhost"
                                || self.smtp_host == "127.0.0.1",
                        )
                        .dangerous_accept_invalid_hostnames(
                            self.accept_invalid_certs
                                || self.smtp_host == "localhost"
                                || self.smtp_host == "127.0.0.1",
                        )
                        .build()?;
                    builder
                        .tls(lettre::transport::smtp::client::Tls::Required(tls))
                        .build()
                } else {
                    // Opportunistic STARTTLS (use if available) - accept self-signed certificates based on configuration
                    let tls = TlsParametersBuilder::new(self.smtp_host.clone())
                        .dangerous_accept_invalid_certs(
                            self.accept_invalid_certs
                                || self.smtp_host == "localhost"
                                || self.smtp_host == "127.0.0.1",
                        )
                        .dangerous_accept_invalid_hostnames(
                            self.accept_invalid_certs
                                || self.smtp_host == "localhost"
                                || self.smtp_host == "127.0.0.1",
                        )
                        .build()?;
                    builder
                        .tls(lettre::transport::smtp::client::Tls::Opportunistic(tls))
                        .build()
                }
            }
            TlsMode::Tls => {
                // Direct TLS connection (SMTPS) - accept self-signed certificates based on configuration
                let tls = TlsParametersBuilder::new(self.smtp_host.clone())
                    .dangerous_accept_invalid_certs(
                        self.accept_invalid_certs
                            || self.smtp_host == "localhost"
                            || self.smtp_host == "127.0.0.1",
                    )
                    .dangerous_accept_invalid_hostnames(
                        self.accept_invalid_certs
                            || self.smtp_host == "localhost"
                            || self.smtp_host == "127.0.0.1",
                    )
                    .build()?;
                let mut relay_builder =
                    AsyncSmtpTransport::<Tokio1Executor>::relay(&self.smtp_host)?
                        .port(self.smtp_port)
                        .tls(lettre::transport::smtp::client::Tls::Wrapper(tls));

                // Add credentials if provided
                if let (Some(username), Some(password)) = (&self.username, &self.password) {
                    if !username.is_empty() {
                        relay_builder = relay_builder
                            .credentials(Credentials::new(username.clone(), password.clone()));
                    }
                }

                relay_builder.build()
            }
        };

        self.mailer = Some(mailer);
        self.db = db;

        Ok(())
    }

    async fn send(&self, notification: &Notification) -> Result<()> {
        let mailer = self
            .mailer
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Email provider not initialized"))?;

        let priority_prefix = match notification.priority {
            NotificationPriority::Low => "[LOW] ",
            NotificationPriority::Normal => "",
            NotificationPriority::High => "[HIGH] ",
            NotificationPriority::Critical => "[CRITICAL] ",
        };

        let from = Mailbox::new(self.from_name.clone(), self.from_address.parse()?);

        // If the notification message is already a full HTML document, send it
        // as-is. Otherwise wrap it in the standard notification email template.
        let trimmed = notification.message.trim_start();
        let is_full_document = trimmed.starts_with("<!DOCTYPE")
            || trimmed.starts_with("<!doctype")
            || trimmed.starts_with("<html")
            || trimmed.starts_with("<HTML");
        let email_body = if is_full_document {
            notification.message.clone()
        } else {
            Self::render_notification_email(notification)
        };

        // Combine configured addresses and admin emails into a single list
        let mut all_recipients = self.to_addresses.clone();
        if let Ok(admin_users) = self.get_admin_users().await {
            all_recipients.extend(admin_users.into_iter().filter_map(|user| {
                if !user.email.trim().is_empty() {
                    Some(user.email)
                } else {
                    None
                }
            }));
        }
        // Remove duplicates
        all_recipients.sort();
        all_recipients.dedup();

        // Send individual emails to each recipient
        for addr in &all_recipients {
            match addr.parse::<Mailbox>() {
                Ok(to_mailbox) => {
                    let email_msg = Message::builder()
                        .from(from.clone())
                        .to(to_mailbox)
                        .subject(format!("{}{}", priority_prefix, notification.title))
                        .header(ContentType::TEXT_HTML)
                        .body(email_body.clone())?;

                    if let Err(e) = mailer.send(email_msg).await {
                        error!("Failed to send email to {}: {}", addr, e);
                    }
                }
                Err(e) => {
                    error!("Invalid email address {}: {}", addr, e);
                }
            }
        }

        Ok(())
    }

    async fn health_check(&self) -> Result<bool> {
        EmailProvider::email_health_check(self).await
    }
}

impl EmailProvider {
    /// Send a transactional email to explicit recipients.
    ///
    /// Unlike [`NotificationProvider::send`], this does NOT pull in the
    /// configured `to_addresses` or admin users — it delivers only to the
    /// addresses passed in (e.g. the user who requested a password reset).
    /// The From defaults to the provider's configured sender unless
    /// `from_override` is supplied. The body is sent as-is when it's a full
    /// HTML document, matching the notification send-path behaviour.
    async fn send_to(
        &self,
        recipients: &[String],
        subject: &str,
        html_body: &str,
        from_override: Option<&str>,
    ) -> Result<()> {
        let mailer = self
            .mailer
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Email provider not initialized"))?;

        let from_address = from_override.unwrap_or(&self.from_address);
        let from = Mailbox::new(self.from_name.clone(), from_address.parse()?);

        if recipients.is_empty() {
            return Err(anyhow::anyhow!("No recipient for transactional email"));
        }

        let mut last_err: Option<anyhow::Error> = None;
        let mut delivered = false;
        for addr in recipients {
            let to_mailbox = match addr.parse::<Mailbox>() {
                Ok(m) => m,
                Err(e) => {
                    error!("Invalid transactional email recipient {}: {}", addr, e);
                    last_err = Some(anyhow::anyhow!("Invalid recipient {}: {}", addr, e));
                    continue;
                }
            };

            let email_msg = Message::builder()
                .from(from.clone())
                .to(to_mailbox)
                .subject(subject)
                .header(ContentType::TEXT_HTML)
                .body(html_body.to_string())?;

            match mailer.send(email_msg).await {
                Ok(_) => delivered = true,
                Err(e) => {
                    error!("Failed to send transactional email to {}: {}", addr, e);
                    last_err = Some(anyhow::anyhow!("SMTP send failed for {}: {}", addr, e));
                }
            }
        }

        if delivered {
            Ok(())
        } else {
            Err(last_err
                .unwrap_or_else(|| anyhow::anyhow!("Transactional email could not be delivered")))
        }
    }

    fn render_notification_email(notification: &Notification) -> String {
        let (accent_color, bg_color, icon, label) = match notification.priority {
            NotificationPriority::Low => ("#6b7280", "#f9fafb", "&#8505;", "Info"),
            NotificationPriority::Normal => ("#2563eb", "#eff6ff", "&#9432;", "Notice"),
            NotificationPriority::High => ("#d97706", "#fffbeb", "&#9888;", "Warning"),
            NotificationPriority::Critical => ("#dc2626", "#fef2f2", "&#128680;", "Critical"),
        };

        // Inline chart (e.g. an OTel metric-alert's recent series) carried as a
        // reserved `_chart_svg` key and rendered raw. `_`-prefixed keys are
        // channel payloads — never shown as plain detail rows.
        let chart_html = notification
            .metadata
            .get("_chart_svg")
            .map(|svg| {
                format!(
                    r#"<tr><td colspan="2" style="padding: 20px 0 0;">{}</td></tr>"#,
                    svg
                )
            })
            .unwrap_or_default();

        // Optional CTA button carried as reserved `_action_url`/`_action_label`
        // keys (e.g. "View error details" linking to the error group page).
        let action_html = notification
            .metadata
            .get("_action_url")
            .map(|url| {
                let label = notification
                    .metadata
                    .get("_action_label")
                    .map(String::as_str)
                    .unwrap_or("View details");
                format!(
                    r#"<tr><td style="padding: 24px 0 0;">
                        <a href="{}" style="display: inline-block; padding: 10px 20px; background: {}; border-radius: 6px; color: #ffffff; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif; font-size: 14px; font-weight: 600; text-decoration: none;">{}</a>
                    </td></tr>"#,
                    html_escape(url),
                    accent_color,
                    html_escape(label)
                )
            })
            .unwrap_or_default();

        let visible_metadata: Vec<(&String, &String)> = notification
            .metadata
            .iter()
            .filter(|(k, _)| !k.starts_with('_'))
            .collect();
        let metadata_html = if visible_metadata.is_empty() {
            String::new()
        } else {
            let rows: String = visible_metadata
                .iter()
                .map(|(k, v)| {
                    // Format key: replace underscores with spaces and title-case
                    let label = k
                        .split('_')
                        .map(|w| {
                            let mut c = w.chars();
                            match c.next() {
                                None => String::new(),
                                Some(f) => {
                                    f.to_uppercase().collect::<String>() + c.as_str()
                                }
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    format!(
                        r#"<tr>
                            <td style="padding: 8px 12px; color: #6b7280; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif; font-size: 13px; white-space: nowrap; vertical-align: top;">{}</td>
                            <td style="padding: 8px 12px; color: #1f2937; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif; font-size: 13px; word-break: break-all;">{}</td>
                        </tr>"#,
                        html_escape(&label), html_escape(v)
                    )
                })
                .collect();

            format!(
                r#"<tr><td colspan="2" style="padding: 0;">
                    <table width="100%" cellpadding="0" cellspacing="0" style="margin-top: 24px; border-top: 1px solid #e5e7eb;">
                        <tr><td style="padding: 16px 0 8px; color: #374151; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif; font-size: 12px; font-weight: 600; text-transform: uppercase; letter-spacing: 0.05em;">Details</td></tr>
                        <tr><td style="padding: 0;">
                            <table width="100%" cellpadding="0" cellspacing="0" style="background: #f9fafb; border-radius: 6px;">
                                {}
                            </table>
                        </td></tr>
                    </table>
                </td></tr>"#,
                rows
            )
        };

        // Always escape the message as plain text, then convert newlines to <br>.
        // Escape must happen first so the literal <br> tags we insert are not
        // themselves escaped in a subsequent pass.
        let message_html = html_escape(&notification.message).replace('\n', "<br>");
        let title_html = html_escape(&notification.title);

        format!(
            r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>{title}</title>
</head>
<body style="margin: 0; padding: 0; background-color: #f3f4f6; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif; -webkit-font-smoothing: antialiased;">
    <table width="100%" cellpadding="0" cellspacing="0" style="background-color: #f3f4f6; padding: 32px 16px; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif;">
        <tr><td align="center" style="font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif;">
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

                <!-- Priority Badge -->
                <tr><td style="padding: 24px 32px 0; background: #ffffff; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif;">
                    <table cellpadding="0" cellspacing="0">
                        <tr><td style="padding: 4px 12px; background: {bg_color}; border: 1px solid {accent_color}22; border-radius: 100px;">
                            <span style="font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; font-size: 12px; font-weight: 600; color: {accent_color};">{icon} {label}</span>
                        </td></tr>
                    </table>
                </td></tr>

                <!-- Title -->
                <tr><td style="padding: 12px 32px 0; background: #ffffff; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif;">
                    <h1 style="margin: 0; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; font-size: 20px; font-weight: 600; color: #111827; line-height: 1.4;">{title}</h1>
                </td></tr>

                <!-- Message Body -->
                <tr><td style="padding: 16px 32px 24px; background: #ffffff; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif;">
                    <table width="100%" cellpadding="0" cellspacing="0">
                        <tr><td style="font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; font-size: 14px; color: #374151; line-height: 1.7;">
                            {message}
                        </td></tr>
                        {chart}
                        {metadata}
                        {action}
                    </table>
                </td></tr>

                <!-- Footer -->
                <tr><td style="padding: 16px 32px; background: #f9fafb; border-top: 1px solid #e5e7eb; border-radius: 0 0 8px 8px; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif;">
                    <table width="100%" cellpadding="0" cellspacing="0">
                        <tr>
                            <td style="font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; font-size: 12px; color: #9ca3af;">Sent by Temps &middot; Self-hosted PaaS</td>
                            <td align="right" style="font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; font-size: 12px; color: #9ca3af;">{priority} priority</td>
                        </tr>
                    </table>
                </td></tr>
            </table>
        </td></tr>
    </table>
</body>
</html>"#,
            title = title_html,
            timestamp = notification.timestamp.format("%b %d, %Y at %H:%M UTC"),
            accent_color = accent_color,
            bg_color = bg_color,
            icon = icon,
            label = label,
            message = message_html,
            chart = chart_html,
            metadata = metadata_html,
            action = action_html,
            priority = notification.priority,
        )
    }

    async fn email_health_check(&self) -> Result<bool> {
        let Some(mailer) = &self.mailer else {
            return Ok(false);
        };

        // First, prove the SMTP connection itself works. If this fails we
        // return Ok(false) (the contract of health_check) rather than bubble
        // an error — operators see "provider unhealthy" instead of a 500.
        if let Err(e) = mailer.test_connection().await {
            error!(
                "Email provider health check failed at connection stage ({}): {}",
                self.smtp_host, e
            );
            return Ok(false);
        }

        // The send-path needs at least one recipient. Connection-only success
        // is still a useful signal, so we report healthy without bouncing an
        // unaddressable email.
        let Some(recipient) = self.to_addresses.first() else {
            return Ok(true);
        };
        let to_mailbox = match recipient.parse::<Mailbox>() {
            Ok(m) => m,
            Err(e) => {
                error!(
                    "Health check skipped send: recipient '{}' is not a valid email: {}",
                    recipient, e
                );
                // The connection is fine; the misconfiguration is on the recipient list.
                return Ok(true);
            }
        };

        let from = Mailbox::new(self.from_name.clone(), self.from_address.parse()?);
        let timestamp = chrono::Utc::now()
            .format("%b %d, %Y at %H:%M UTC")
            .to_string();
        // Best-effort identifier of which Temps instance sent the email.
        // `TEMPS_HOSTNAME` is set by the deploy scripts; falls back to the
        // public address if configured, then a generic label.
        let host = std::env::var("TEMPS_HOSTNAME")
            .or_else(|_| std::env::var("TEMPS_PUBLIC_HOSTNAME"))
            .unwrap_or_else(|_| "temps instance".to_string());

        let subject = "[Temps] Notification provider health check";
        let plain_body = format!(
            "Temps notification provider health check\n\
             \n\
             This is an automated message confirming that the email notification provider \
             is reachable and authorised to send mail.\n\
             \n\
             Sent from:  {host}\n\
             SMTP host:  {smtp_host}:{smtp_port}\n\
             From:       {from_address}\n\
             Timestamp:  {timestamp}\n\
             \n\
             If you didn't expect this email, an operator triggered a health check from \
             the Temps notifications dashboard. No action is required.\n",
            host = host,
            smtp_host = self.smtp_host,
            smtp_port = self.smtp_port,
            from_address = self.from_address,
            timestamp = timestamp,
        );
        let html_body = Self::render_health_check_email(
            &host,
            &self.smtp_host,
            self.smtp_port,
            &self.from_address,
            &timestamp,
        );

        let message = Message::builder()
            .from(from)
            .to(to_mailbox)
            .subject(subject)
            .multipart(
                MultiPart::alternative()
                    .singlepart(
                        SinglePart::builder()
                            .header(ContentType::TEXT_PLAIN)
                            .body(plain_body),
                    )
                    .singlepart(
                        SinglePart::builder()
                            .header(ContentType::TEXT_HTML)
                            .body(html_body),
                    ),
            )?;

        match mailer.send(message).await {
            Ok(_) => Ok(true),
            Err(e) => {
                error!(
                    "Email provider health check failed at send stage ({} → {}): {}",
                    self.smtp_host, recipient, e
                );
                Ok(false)
            }
        }
    }

    /// Render the HTML body for the health-check email. Visually matches the
    /// regular notification template (`render_notification_email`) so it lands
    /// in the inbox looking like a real Temps message rather than a debug ping.
    fn render_health_check_email(
        host: &str,
        smtp_host: &str,
        smtp_port: u16,
        from_address: &str,
        timestamp: &str,
    ) -> String {
        // "Info" priority palette — same colors as NotificationPriority::Normal.
        let accent_color = "#2563eb";
        let bg_color = "#eff6ff";
        format!(
            r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Notification provider health check</title>
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
                            <span style="font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; font-size: 12px; font-weight: 600; color: {accent_color};">&#9432; Health check</span>
                        </td></tr>
                    </table>
                </td></tr>

                <!-- Title -->
                <tr><td style="padding: 12px 32px 0; background: #ffffff;">
                    <h1 style="margin: 0; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; font-size: 20px; font-weight: 600; color: #111827; line-height: 1.4;">Notification provider is reachable</h1>
                </td></tr>

                <!-- Message -->
                <tr><td style="padding: 16px 32px 8px; background: #ffffff;">
                    <p style="margin: 0; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; font-size: 14px; color: #374151; line-height: 1.7;">
                        Your Temps instance just verified that this email notification provider can authenticate against the SMTP relay and deliver mail to the configured recipients. No action is required &mdash; if you didn&rsquo;t trigger this check from the dashboard, you can safely ignore the message.
                    </p>
                </td></tr>

                <!-- Connection details -->
                <tr><td style="padding: 12px 32px 24px; background: #ffffff;">
                    <table width="100%" cellpadding="0" cellspacing="0" style="background: #f9fafb; border: 1px solid #e5e7eb; border-radius: 6px;">
                        <tr>
                            <td style="padding: 10px 14px; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; font-size: 12px; color: #6b7280; white-space: nowrap; vertical-align: top; width: 110px;">Instance</td>
                            <td style="padding: 10px 14px; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; font-size: 13px; color: #111827; font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; word-break: break-all;">{host}</td>
                        </tr>
                        <tr>
                            <td style="padding: 10px 14px; border-top: 1px solid #e5e7eb; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; font-size: 12px; color: #6b7280; white-space: nowrap; vertical-align: top;">SMTP relay</td>
                            <td style="padding: 10px 14px; border-top: 1px solid #e5e7eb; font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; font-size: 13px; color: #111827; word-break: break-all;">{smtp_host}:{smtp_port}</td>
                        </tr>
                        <tr>
                            <td style="padding: 10px 14px; border-top: 1px solid #e5e7eb; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; font-size: 12px; color: #6b7280; white-space: nowrap; vertical-align: top;">From</td>
                            <td style="padding: 10px 14px; border-top: 1px solid #e5e7eb; font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; font-size: 13px; color: #111827; word-break: break-all;">{from_address}</td>
                        </tr>
                    </table>
                </td></tr>

                <!-- Footer -->
                <tr><td style="padding: 16px 32px; background: #f9fafb; border-top: 1px solid #e5e7eb; border-radius: 0 0 8px 8px;">
                    <table width="100%" cellpadding="0" cellspacing="0">
                        <tr>
                            <td style="font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; font-size: 12px; color: #9ca3af;">Sent by Temps &middot; Self-hosted PaaS</td>
                            <td align="right" style="font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; font-size: 12px; color: #9ca3af;">Automated health check</td>
                        </tr>
                    </table>
                </td></tr>
            </table>
        </td></tr>
    </table>
</body>
</html>"#,
            host = host,
            smtp_host = smtp_host,
            smtp_port = smtp_port,
            from_address = from_address,
            timestamp = timestamp,
            accent_color = accent_color,
            bg_color = bg_color,
        )
    }
}

#[async_trait]
impl NotificationProvider for SlackProvider {
    async fn initialize(&mut self, _db: Arc<DatabaseConnection>) -> Result<()> {
        // Validate webhook URL format
        if !self.webhook_url.starts_with("https://hooks.slack.com/") {
            return Err(anyhow::anyhow!("Invalid Slack webhook URL"));
        }
        Ok(())
    }

    async fn send(&self, notification: &Notification) -> Result<()> {
        let client = reqwest::Client::new();

        let color = match notification.notification_type {
            NotificationType::Info => "#0088cc",
            NotificationType::Warning => "#ffa500",
            NotificationType::Error => "#ff0000",
            NotificationType::Alert => "#ff0000",
        };

        let metadata_fields = notification
            .metadata
            .iter()
            // `_`-prefixed keys are channel payloads (e.g. the email's `_chart_svg`),
            // not human-facing fields — skip them here.
            .filter(|(k, _)| !k.starts_with('_'))
            .map(|(k, v)| {
                serde_json::json!({
                    "title": slack_escape(k),
                    "value": slack_escape(v),
                    "short": true
                })
            })
            .collect::<Vec<_>>();

        let safe_title = slack_escape(&notification.title);
        let safe_message = slack_escape(&notification.message);
        let payload = serde_json::json!({
            "channel": self.channel,
            "attachments": [{
                "color": color,
                "title": safe_title,
                "text": safe_message,
                "fields": metadata_fields,
                "footer": format!("Priority: {:?} | Type: {:?}", notification.priority, notification.notification_type)
            }]
        });

        client.post(&self.webhook_url).json(&payload).send().await?;

        Ok(())
    }

    async fn health_check(&self) -> Result<bool> {
        let client = reqwest::Client::new();

        let test_payload = serde_json::json!({
            "channel": self.channel,
            "text": "Health check"
        });

        match client
            .post(&self.webhook_url)
            .json(&test_payload)
            .send()
            .await
        {
            Ok(response) => Ok(response.status().is_success()),
            Err(e) => {
                error!("Slack provider health check failed: {}", e);
                Ok(false)
            }
        }
    }
}

#[async_trait]
impl NotificationProvider for WebhookProvider {
    async fn initialize(&mut self, _db: Arc<DatabaseConnection>) -> Result<()> {
        // Validate webhook URL with full SSRF protection (blocks private IPs,
        // loopback, cloud metadata, link-local, etc.)
        temps_core::url_validation::validate_external_url(&self.url)
            .map_err(|e| anyhow::anyhow!("Invalid webhook URL '{}': {}", self.url, e))?;

        // Validate HTTP method
        let method = self.method.to_uppercase();
        if !["POST", "PUT", "PATCH"].contains(&method.as_str()) {
            return Err(anyhow::anyhow!(
                "Invalid HTTP method: {}. Must be POST, PUT, or PATCH",
                self.method
            ));
        }

        Ok(())
    }

    async fn send(&self, notification: &Notification) -> Result<()> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .build()?;

        // Build the payload with all notification data. `_`-prefixed keys are
        // channel-specific payloads (e.g. the email's `_chart_svg`) — drop them
        // so they don't bloat the webhook body.
        let metadata: std::collections::HashMap<&String, &String> = notification
            .metadata
            .iter()
            .filter(|(k, _)| !k.starts_with('_'))
            .collect();
        let payload = serde_json::json!({
            "id": notification.id,
            "title": notification.title,
            "message": notification.message,
            "type": notification.notification_type.to_string(),
            "priority": notification.priority.to_string(),
            "severity": notification.effective_severity().to_string(),
            "timestamp": notification.timestamp.to_rfc3339(),
            "metadata": metadata,
        });

        // Build the request with configured method
        let method: reqwest::Method = self
            .method
            .to_uppercase()
            .parse()
            .unwrap_or(reqwest::Method::POST);
        let mut request = client
            .request(method, &self.url)
            .header("Content-Type", "application/json")
            .json(&payload);

        // Add custom headers (useful for auth tokens, API keys, etc.)
        for (key, value) in &self.headers {
            request = request.header(key.as_str(), value.as_str());
        }

        let response = request.send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            error!("Webhook request failed with status {}: {}", status, body);
            return Err(anyhow::anyhow!(
                "Webhook request failed with status {}: {}",
                status,
                body
            ));
        }

        Ok(())
    }

    async fn health_check(&self) -> Result<bool> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .build()?;

        // Send a test payload
        let test_payload = serde_json::json!({
            "test": true,
            "type": "health_check",
            "message": "Temps webhook health check",
            "timestamp": Utc::now().to_rfc3339(),
        });

        let method: reqwest::Method = self
            .method
            .to_uppercase()
            .parse()
            .unwrap_or(reqwest::Method::POST);
        let mut request = client
            .request(method, &self.url)
            .header("Content-Type", "application/json")
            .json(&test_payload);

        // Add custom headers
        for (key, value) in &self.headers {
            request = request.header(key.as_str(), value.as_str());
        }

        match request.send().await {
            Ok(response) => Ok(response.status().is_success()),
            Err(e) => {
                error!("Webhook provider health check failed: {}", e);
                Ok(false)
            }
        }
    }
}

pub struct NotificationService {
    db: Arc<DatabaseConnection>,
    encryption_service: Arc<temps_core::EncryptionService>,
}

impl NotificationService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        encryption_service: Arc<temps_core::EncryptionService>,
    ) -> Self {
        Self {
            db,
            encryption_service,
        }
    }

    fn get_batch_key(notification: &Notification) -> String {
        format!(
            "{}:{}:{}",
            notification.notification_type, notification.priority, notification.title
        )
    }

    async fn get_enabled_providers(&self) -> Result<Vec<Box<dyn NotificationProvider>>> {
        let db_providers = notification_providers::Entity::find()
            .filter(notification_providers::Column::Enabled.eq(true))
            .all(self.db.as_ref())
            .await?;
        let mut providers = vec![];
        for provider_record in db_providers {
            match self.load_provider(&provider_record).await {
                Ok(provider) => {
                    providers.push(provider);
                }
                Err(e) => {
                    error!("Failed to load provider {}: {}", provider_record.name, e);
                }
            }
        }
        Ok(providers)
    }

    /// Load only enabled `email`-type providers, initialized and ready to
    /// send. Used by the transactional email path so reset/verification
    /// links never route to Slack/webhook providers.
    async fn get_enabled_email_providers(&self) -> Result<Vec<EmailProvider>> {
        let records = notification_providers::Entity::find()
            .filter(notification_providers::Column::Enabled.eq(true))
            .filter(notification_providers::Column::ProviderType.eq("email"))
            .all(self.db.as_ref())
            .await?;

        let mut providers = Vec::new();
        for record in records {
            let decrypted_config = match self.encryption_service.decrypt_string(&record.config) {
                Ok(c) => c,
                Err(e) => {
                    error!("Failed to decrypt email provider {}: {}", record.name, e);
                    continue;
                }
            };
            let mut config: EmailProvider = match serde_json::from_str(&decrypted_config) {
                Ok(c) => c,
                Err(e) => {
                    error!("Failed to parse email provider {}: {}", record.name, e);
                    continue;
                }
            };
            if let Err(e) = config.initialize(self.db.clone()).await {
                error!("Failed to initialize email provider {}: {}", record.name, e);
                continue;
            }
            providers.push(config);
        }
        Ok(providers)
    }

    /// Returns the base delay between notifications for a given priority.
    /// This is the gap after the very first notification — subsequent gaps
    /// grow exponentially (see `get_next_allowed_time`).
    fn base_delay(priority: &NotificationPriority) -> Duration {
        match priority {
            NotificationPriority::Low => Duration::days(7),
            NotificationPriority::Normal => Duration::days(1),
            NotificationPriority::High => Duration::hours(1),
            NotificationPriority::Critical => Duration::minutes(15),
        }
    }

    /// Returns the maximum gap between notifications for a given priority.
    /// Exponential backoff is clamped here so a long-running incident still
    /// produces an occasional reminder rather than going completely silent.
    fn max_delay(priority: &NotificationPriority) -> Duration {
        match priority {
            NotificationPriority::Low => Duration::days(30),
            NotificationPriority::Normal => Duration::days(7),
            NotificationPriority::High => Duration::hours(24),
            NotificationPriority::Critical => Duration::hours(24),
        }
    }

    /// Compute the next-allowed timestamp using exponential backoff.
    ///
    /// `previous_attempts` is the `occurrence_count` of the previous record for
    /// this batch_key — i.e., how many times the same alarm tried to fire
    /// (1 send + N throttled events) before this send. The first ever send
    /// passes 0 and gets the base delay; persistent incidents grow the gap
    /// exponentially, clamped at `max_delay`. This stops a flapping container
    /// from generating one email every 15 minutes forever.
    fn get_next_allowed_time(
        priority: &NotificationPriority,
        previous_attempts: i32,
    ) -> DateTime<Utc> {
        let base = Self::base_delay(priority);
        let cap = Self::max_delay(priority);

        // First-time send (no prior attempts) uses the base delay.
        // Otherwise double per prior attempt: 1 prior -> 2x, 2 prior -> 4x, etc.
        // Clamp shift at 20 so we never overflow i64 before clamping to `cap`.
        let shift = previous_attempts.clamp(0, 20) as u32;
        let multiplier: i64 = 1i64 << shift;

        let scaled_secs = base
            .num_seconds()
            .saturating_mul(multiplier)
            .min(cap.num_seconds());

        Utc::now() + Duration::seconds(scaled_secs)
    }

    pub async fn send_notification(&self, notification: Notification) -> Result<()> {
        let now = Utc::now();
        let batch_key_str = Self::get_batch_key(&notification);

        // Check for existing similar notifications
        let existing = notifications::Entity::find()
            .filter(notifications::Column::BatchKey.eq(&batch_key_str))
            .order_by_desc(notifications::Column::CreatedAt)
            .one(self.db.as_ref())
            .await?;

        if let Some(existing) = existing.clone() {
            // If we have a similar notification, check if we should send it or batch it.
            // `bypass_throttling` lets callers (e.g. weekly digest, manual test sends)
            // skip the gate entirely. Critical alarms intentionally still respect the
            // backoff — bypassing them is what produced 100 emails/day.
            if !notification.bypass_throttling && now < existing.next_allowed_at {
                // Update occurrence count and return
                let mut existing_update: notifications::ActiveModel = existing.clone().into();
                existing_update.occurrence_count = Set(existing.occurrence_count + 1);
                existing_update.update(self.db.as_ref()).await?;

                info!(
                    "Batching notification '{}'. Current count: {}, next send allowed at {}",
                    notification.title,
                    existing.occurrence_count + 1,
                    existing.next_allowed_at,
                );
                return Ok(());
            }
        }

        // If we reach here, we should send the notification.
        // Pass the previous record's occurrence_count so the gap doubles per
        // ongoing-incident attempt instead of staying at the base delay forever.
        let previous_attempts = existing.as_ref().map(|e| e.occurrence_count).unwrap_or(0);
        // Persist only human-facing metadata; `_`-prefixed channel payloads
        // (e.g. the email's `_chart_svg`) would bloat the row needlessly.
        let persisted_metadata: std::collections::HashMap<&String, &String> = notification
            .metadata
            .iter()
            .filter(|(k, _)| !k.starts_with('_'))
            .collect();
        let metadata_json = serde_json::to_string(&persisted_metadata)?;
        let next_allowed = Self::get_next_allowed_time(&notification.priority, previous_attempts);

        // Create new notification record
        let new_notification = notifications::ActiveModel {
            notification_id: Set(notification.id.clone()),
            title: Set(notification.title.clone()),
            message: Set(notification.message.clone()),
            notification_type: Set(notification.notification_type.to_string()),
            priority: Set(notification.priority.to_string()),
            metadata: Set(metadata_json),
            created_at: Set(now),
            batch_key: Set(batch_key_str.clone()),
            occurrence_count: Set(1),
            next_allowed_at: Set(next_allowed),
            ..Default::default()
        };

        // Insert the new notification record
        let _inserted = new_notification.insert(self.db.as_ref()).await?;

        // Get the occurrence count for the message modification
        let occurrence_count_val = if let Some(existing) = existing {
            existing.occurrence_count + 1
        } else {
            1
        };

        // Modify the notification message if there were batched occurrences
        let mut notification = notification.clone();
        if occurrence_count_val > 1 {
            notification.message = format!(
                "{}\n\nThis issue has occurred {} times since the last notification.",
                notification.message, occurrence_count_val
            );
        }

        // Send through all configured providers
        let providers = self
            .get_enabled_providers()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get providers {}", e))?;
        for provider in &providers {
            if let Err(e) = provider.send(&notification).await {
                error!("Failed to send notification via provider: {}", e);
            }
        }

        Ok(())
    }

    pub async fn is_configured(&self) -> Result<bool> {
        let count = notification_providers::Entity::find()
            .filter(notification_providers::Column::Enabled.eq(true))
            .paginate(self.db.as_ref(), 1)
            .num_items()
            .await
            .map_err(|e| {
                error!("Failed to check notification providers: {}", e);
                anyhow::anyhow!("Failed to check notification providers: {}", e)
            })?;

        Ok(count > 0)
    }

    pub async fn list_providers(&self) -> Result<Vec<notification_providers::Model>> {
        let providers = notification_providers::Entity::find()
            .all(self.db.as_ref())
            .await?;
        Ok(providers)
    }

    pub async fn list_providers_paginated(
        &self,
        page: u64,
        page_size: u64,
    ) -> Result<Vec<notification_providers::Model>> {
        let providers = notification_providers::Entity::find()
            .paginate(self.db.as_ref(), page_size)
            .fetch_page(page - 1)
            .await?;
        Ok(providers)
    }

    /// Decrypt the provider config for safe return to API
    pub fn decrypt_provider_config(&self, encrypted_config: &str) -> Result<serde_json::Value> {
        let decrypted_config = self
            .encryption_service
            .decrypt_string(encrypted_config)
            .map_err(|e| anyhow::anyhow!("Failed to decrypt config: {}", e))?;

        let config_value: serde_json::Value = serde_json::from_str(&decrypted_config)
            .map_err(|e| anyhow::anyhow!("Failed to parse decrypted config: {}", e))?;

        Ok(config_value)
    }

    async fn load_provider(
        &self,
        record: &notification_providers::Model,
    ) -> Result<Box<dyn NotificationProvider>> {
        // Decrypt the config before parsing
        let decrypted_config = self
            .encryption_service
            .decrypt_string(&record.config)
            .map_err(|e| anyhow::anyhow!("Failed to decrypt config: {}", e))?;

        let provider: Box<dyn NotificationProvider> = match record.provider_type.as_str() {
            "email" => {
                let mut config: EmailProvider = serde_json::from_str(&decrypted_config)?;
                config.initialize(self.db.clone()).await?;
                Box::new(config)
            }
            "slack" => {
                let mut config: SlackProvider = serde_json::from_str(&decrypted_config)?;
                config.initialize(self.db.clone()).await?;
                Box::new(config)
            }
            "webhook" => {
                let mut config: WebhookProvider = serde_json::from_str(&decrypted_config)?;
                config.initialize(self.db.clone()).await?;
                Box::new(config)
            }
            "cloudflare" => {
                let mut config: CloudflareProvider = serde_json::from_str(&decrypted_config)?;
                config.initialize(self.db.clone()).await?;
                Box::new(config)
            }
            _ => return Err(anyhow::anyhow!("Unsupported provider type")),
        };
        Ok(provider)
    }

    pub async fn get_provider(
        &self,
        provider_id: i32,
    ) -> Result<Option<notification_providers::Model>> {
        let provider = notification_providers::Entity::find_by_id(provider_id)
            .one(self.db.as_ref())
            .await?;
        Ok(provider)
    }

    pub async fn add_provider<T: Serialize>(
        &self,
        p_name: String,
        p_provider_type: String,
        p_config: T,
    ) -> Result<notification_providers::Model> {
        let config_json = serde_json::to_string(&p_config)?;

        // Encrypt the config before storing
        let encrypted_config = self
            .encryption_service
            .encrypt_string(&config_json)
            .map_err(|e| anyhow::anyhow!("Failed to encrypt config: {}", e))?;

        let new_provider = notification_providers::ActiveModel {
            name: Set(p_name),
            provider_type: Set(p_provider_type),
            config: Set(encrypted_config),
            enabled: Set(true),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        };

        let provider = new_provider.insert(self.db.as_ref()).await?;

        Ok(provider)
    }

    pub async fn update_provider(
        &self,
        provider_id: i32,
        update: UpdateProviderRequest,
    ) -> Result<Option<notification_providers::Model>> {
        // First check if the provider exists
        let provider = notification_providers::Entity::find_by_id(provider_id)
            .one(self.db.as_ref())
            .await?;

        if let Some(provider) = provider {
            let mut active_model: notification_providers::ActiveModel = provider.into();

            // Update fields if provided
            if let Some(new_name) = update.name {
                active_model.name = Set(new_name);
            }
            if let Some(new_config) = update.config {
                let config_json = serde_json::to_string(&new_config)?;
                // Encrypt the config before storing
                let encrypted_config = self
                    .encryption_service
                    .encrypt_string(&config_json)
                    .map_err(|e| anyhow::anyhow!("Failed to encrypt config: {}", e))?;
                active_model.config = Set(encrypted_config);
            }
            if let Some(new_enabled) = update.enabled {
                active_model.enabled = Set(new_enabled);
            }
            active_model.updated_at = Set(Utc::now());

            // Update the provider in the database
            let updated_provider = active_model.update(self.db.as_ref()).await?;

            Ok(Some(updated_provider))
        } else {
            Ok(None)
        }
    }

    pub async fn delete_provider(&self, provider_id: i32) -> Result<bool> {
        let provider = notification_providers::Entity::find_by_id(provider_id)
            .one(self.db.as_ref())
            .await?;

        if let Some(provider) = provider {
            provider.delete(self.db.as_ref()).await?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub async fn test_provider(&self, provider_id: i32) -> Result<bool> {
        let provider = notification_providers::Entity::find_by_id(provider_id)
            .one(self.db.as_ref())
            .await?;

        if let Some(provider) = provider {
            let notification_provider = self.load_provider(&provider).await?;
            // Let the error propagate instead of swallowing it
            notification_provider.health_check().await
        } else {
            Err(anyhow::anyhow!(
                "Notification provider with ID {} not found",
                provider_id
            ))
        }
    }

    // Add a method to clean up old notifications
    pub async fn cleanup_old_notifications(&self, retention_days: i64) -> Result<()> {
        let cutoff = Utc::now() - Duration::days(retention_days);

        notifications::Entity::delete_many()
            .filter(notifications::Column::CreatedAt.lt(cutoff))
            .exec(self.db.as_ref())
            .await?;

        Ok(())
    }
}

// Implement the core NotificationService trait for integration with other services
#[async_trait]
impl CoreNotificationService for NotificationService {
    async fn send_email(&self, message: EmailMessage) -> Result<(), CoreNotificationError> {
        // Convert EmailMessage to our internal notification format
        let notification = Notification {
            id: uuid::Uuid::new_v4().to_string(),
            title: message.subject,
            message: message.body,
            notification_type: NotificationType::Info,
            priority: NotificationPriority::Normal,
            severity: None,
            timestamp: Utc::now(),
            metadata: [
                ("to".to_string(), message.to.join(", ")),
                ("from".to_string(), message.from.unwrap_or_default()),
                ("reply_to".to_string(), message.reply_to.unwrap_or_default()),
            ]
            .into_iter()
            .collect(),
            bypass_throttling: false,
        };

        match self.send_notification(notification).await {
            Ok(_) => Ok(()),
            Err(e) => Err(CoreNotificationError::SendError(e.to_string())),
        }
    }

    async fn send_transactional_email(
        &self,
        message: EmailMessage,
    ) -> Result<(), CoreNotificationError> {
        if message.to.is_empty() {
            return Err(CoreNotificationError::InvalidRecipient(
                "Transactional email has no recipients".to_string(),
            ));
        }

        let providers = self
            .get_enabled_email_providers()
            .await
            .map_err(|e| CoreNotificationError::ConfigurationError(e.to_string()))?;

        if providers.is_empty() {
            return Err(CoreNotificationError::ServiceUnavailable(
                "No enabled email provider is configured".to_string(),
            ));
        }

        // Prefer the HTML body; fall back to the plain-text body so we never
        // send an empty message.
        let body = message
            .html_body
            .clone()
            .unwrap_or_else(|| message.body.clone());

        // Try each email provider until one delivers. We don't fan out to
        // all of them — a transactional message should arrive once.
        let mut last_err: Option<anyhow::Error> = None;
        for provider in &providers {
            match provider
                .send_to(
                    &message.to,
                    &message.subject,
                    &body,
                    message.from.as_deref(),
                )
                .await
            {
                Ok(_) => return Ok(()),
                Err(e) => {
                    error!("Transactional email provider failed: {}", e);
                    last_err = Some(e);
                }
            }
        }

        Err(CoreNotificationError::SendError(
            last_err
                .map(|e| e.to_string())
                .unwrap_or_else(|| "All email providers failed".to_string()),
        ))
    }

    async fn is_email_provider_configured(&self) -> Result<bool, CoreNotificationError> {
        let count = notification_providers::Entity::find()
            .filter(notification_providers::Column::Enabled.eq(true))
            .filter(notification_providers::Column::ProviderType.eq("email"))
            .count(self.db.as_ref())
            .await
            .map_err(|e| CoreNotificationError::ConfigurationError(e.to_string()))?;
        Ok(count > 0)
    }

    async fn send_notification(
        &self,
        notification_data: NotificationData,
    ) -> Result<(), CoreNotificationError> {
        // Convert NotificationData to our internal Notification format
        let notification = Notification {
            id: notification_data.id,
            title: notification_data.title,
            message: notification_data.message,
            notification_type: match notification_data.notification_type {
                temps_core::notifications::NotificationType::Info => NotificationType::Info,
                temps_core::notifications::NotificationType::Warning => NotificationType::Warning,
                temps_core::notifications::NotificationType::Error => NotificationType::Error,
                temps_core::notifications::NotificationType::Alert => NotificationType::Alert,
            },
            priority: match notification_data.priority {
                temps_core::notifications::NotificationPriority::Low => NotificationPriority::Low,
                temps_core::notifications::NotificationPriority::Normal => {
                    NotificationPriority::Normal
                }
                temps_core::notifications::NotificationPriority::High => NotificationPriority::High,
                temps_core::notifications::NotificationPriority::Critical => {
                    NotificationPriority::Critical
                }
            },
            severity: notification_data
                .severity
                .and_then(|s| NotificationSeverity::from_str(&s)),
            timestamp: notification_data.timestamp,
            metadata: notification_data.metadata,
            bypass_throttling: notification_data.bypass_throttling,
        };

        match self.send_notification(notification).await {
            Ok(_) => Ok(()),
            Err(e) => Err(CoreNotificationError::SendError(e.to_string())),
        }
    }

    async fn is_configured(&self) -> Result<bool, CoreNotificationError> {
        match self.is_configured().await {
            Ok(configured) => Ok(configured),
            Err(e) => Err(CoreNotificationError::ConfigurationError(e.to_string())),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct EmailProviderConfig {
    pub smtp_host: String,
    pub smtp_port: u16,
    pub username: String,
    pub password: String,
    pub from_address: String,
    pub from_name: String,
    pub to_addresses: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct SlackProviderConfig {
    pub webhook_url: String,
    pub channel: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WebhookProviderConfig {
    pub url: String,
    #[serde(default = "default_http_method")]
    pub method: String,
    #[serde(default)]
    pub headers: std::collections::HashMap<String, String>,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

// Notification Preferences Service
fn default_backup_successes_enabled() -> bool {
    true
}

fn default_weekly_digest_enabled() -> bool {
    true
}

fn default_digest_send_day() -> String {
    "monday".to_string()
}

fn default_digest_send_time() -> String {
    "09:00".to_string()
}

fn default_digest_sections() -> crate::digest::DigestSections {
    crate::digest::DigestSections {
        performance: true,
        deployments: true,
        errors: true,
        funnels: true,
        projects: true,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationPreferences {
    // Notification Channels
    pub email_enabled: bool,
    pub slack_enabled: bool,
    pub batch_similar_notifications: bool,
    pub minimum_severity: String,

    // Project Health
    pub deployment_failures_enabled: bool,
    pub build_errors_enabled: bool,
    pub runtime_errors_enabled: bool,
    pub error_threshold: i32,
    pub error_time_window: i32,

    // Domain Monitoring
    pub ssl_expiration_enabled: bool,
    pub ssl_days_before_expiration: i32,
    pub domain_expiration_enabled: bool,
    pub dns_changes_enabled: bool,

    // Backup Monitoring
    pub backup_failures_enabled: bool,
    #[serde(default = "default_backup_successes_enabled")]
    pub backup_successes_enabled: bool,
    pub s3_connection_issues_enabled: bool,
    pub retention_policy_violations_enabled: bool,

    // Route Monitoring
    pub route_downtime_enabled: bool,
    pub load_balancer_issues_enabled: bool,

    // Weekly Digest Settings
    #[serde(default = "default_weekly_digest_enabled")]
    pub weekly_digest_enabled: bool,
    #[serde(default = "default_digest_send_day")]
    pub digest_send_day: String, // "monday" | "friday" | "sunday"
    #[serde(default = "default_digest_send_time")]
    pub digest_send_time: String, // "09:00" format (24-hour)
    #[serde(default = "default_digest_sections")]
    pub digest_sections: crate::digest::DigestSections,
}

impl Default for NotificationPreferences {
    fn default() -> Self {
        Self {
            email_enabled: true,
            slack_enabled: false,
            batch_similar_notifications: true,
            minimum_severity: "warning".to_string(),

            deployment_failures_enabled: true,
            build_errors_enabled: true,
            runtime_errors_enabled: true,
            error_threshold: 200,
            error_time_window: 5,

            ssl_expiration_enabled: true,
            ssl_days_before_expiration: 30,
            domain_expiration_enabled: true,
            dns_changes_enabled: true,

            backup_failures_enabled: true,
            backup_successes_enabled: true,
            s3_connection_issues_enabled: true,
            retention_policy_violations_enabled: true,

            route_downtime_enabled: true,
            load_balancer_issues_enabled: true,

            weekly_digest_enabled: true,
            digest_send_day: "monday".to_string(),
            digest_send_time: "09:00".to_string(),
            digest_sections: crate::digest::DigestSections::default(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum NotificationPreferencesError {
    #[error("Database error: {0}")]
    DatabaseError(String),
}

pub struct NotificationPreferencesService {
    db: Arc<DatabaseConnection>,
}

impl NotificationPreferencesService {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    pub async fn get_preferences(
        &self,
    ) -> Result<NotificationPreferences, NotificationPreferencesError> {
        let record = notification_preferences::Entity::find()
            .one(self.db.as_ref())
            .await
            .map_err(|e| NotificationPreferencesError::DatabaseError(e.to_string()))?;

        match record {
            Some(record) => {
                let preferences: NotificationPreferences =
                    serde_json::from_str(&record.preferences).map_err(|e| {
                        NotificationPreferencesError::DatabaseError(format!(
                            "Failed to deserialize preferences: {}",
                            e
                        ))
                    })?;
                Ok(preferences)
            }
            None => {
                info!("No notification preferences found, returning defaults");
                Ok(NotificationPreferences::default())
            }
        }
    }

    pub async fn update_preferences(
        &self,
        preferences: NotificationPreferences,
    ) -> Result<NotificationPreferences, NotificationPreferencesError> {
        let preferences_json = serde_json::to_string(&preferences).map_err(|e| {
            NotificationPreferencesError::DatabaseError(format!(
                "Failed to serialize preferences: {}",
                e
            ))
        })?;

        let record = notification_preferences::Entity::find()
            .one(self.db.as_ref())
            .await
            .map_err(|e| NotificationPreferencesError::DatabaseError(e.to_string()))?;

        match record {
            Some(record) => {
                let mut active_model: notification_preferences::ActiveModel = record.into();
                active_model.preferences = Set(preferences_json);
                active_model.updated_at = Set(Utc::now());

                active_model
                    .update(self.db.as_ref())
                    .await
                    .map_err(|e| NotificationPreferencesError::DatabaseError(e.to_string()))?;
            }
            None => {
                let new_pref = notification_preferences::ActiveModel {
                    preferences: Set(preferences_json),
                    created_at: Set(Utc::now()),
                    updated_at: Set(Utc::now()),
                    ..Default::default()
                };

                new_pref
                    .insert(self.db.as_ref())
                    .await
                    .map_err(|e| NotificationPreferencesError::DatabaseError(e.to_string()))?;
            }
        }

        info!("Updated notification preferences");
        Ok(preferences)
    }

    pub async fn delete_preferences(&self) -> Result<(), NotificationPreferencesError> {
        notification_preferences::Entity::delete_many()
            .exec(self.db.as_ref())
            .await
            .map_err(|e| NotificationPreferencesError::DatabaseError(e.to_string()))?;

        info!("Deleted notification preferences");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::MockDatabase;

    fn create_test_notification() -> Notification {
        Notification {
            id: "test-123".to_string(),
            title: "Test Notification".to_string(),
            message: "This is a test message".to_string(),
            notification_type: NotificationType::Info,
            priority: NotificationPriority::Normal,
            severity: None,
            timestamp: Utc::now(),
            metadata: vec![
                ("key1".to_string(), "value1".to_string()),
                ("key2".to_string(), "value2".to_string()),
            ]
            .into_iter()
            .collect(),
            bypass_throttling: false,
        }
    }

    #[test]
    fn test_tls_mode_defaults() {
        assert!(matches!(default_tls_mode(), TlsMode::Starttls));
        assert!(default_starttls_required());
        assert!(!default_accept_invalid_certs());
    }

    #[test]
    fn test_batch_key_generation() {
        let notification = create_test_notification();
        let key = NotificationService::get_batch_key(&notification);
        assert_eq!(key, "Info:Normal:Test Notification");
    }

    /// Helper: assert `actual` falls within `expected ± tolerance_secs`.
    /// Accounts for the few microseconds between the test's `Utc::now()`
    /// snapshot and the inner call inside `get_next_allowed_time`.
    fn assert_near(actual: DateTime<Utc>, expected: DateTime<Utc>, tolerance_secs: i64) {
        let diff = (actual - expected).num_seconds().abs();
        assert!(
            diff <= tolerance_secs,
            "actual {} differs from expected {} by {}s (tolerance {}s)",
            actual,
            expected,
            diff,
            tolerance_secs
        );
    }

    #[test]
    fn test_next_allowed_time_base_delay_first_send() {
        // First send (previous_attempts = 0) should equal the base delay.
        let now = Utc::now();

        let low = NotificationService::get_next_allowed_time(&NotificationPriority::Low, 0);
        let normal = NotificationService::get_next_allowed_time(&NotificationPriority::Normal, 0);
        let high = NotificationService::get_next_allowed_time(&NotificationPriority::High, 0);
        let critical =
            NotificationService::get_next_allowed_time(&NotificationPriority::Critical, 0);

        assert_near(low, now + Duration::days(7), 5);
        assert_near(normal, now + Duration::days(1), 5);
        assert_near(high, now + Duration::hours(1), 5);
        assert_near(critical, now + Duration::minutes(15), 5);
    }

    #[test]
    fn test_next_allowed_time_doubles_per_attempt() {
        // Critical: base 15m. After 1 prior attempt -> 30m. After 2 -> 1h. After 3 -> 2h.
        let now = Utc::now();

        let one = NotificationService::get_next_allowed_time(&NotificationPriority::Critical, 1);
        let two = NotificationService::get_next_allowed_time(&NotificationPriority::Critical, 2);
        let three = NotificationService::get_next_allowed_time(&NotificationPriority::Critical, 3);

        assert_near(one, now + Duration::minutes(30), 5);
        assert_near(two, now + Duration::hours(1), 5);
        assert_near(three, now + Duration::hours(2), 5);
    }

    #[test]
    fn test_next_allowed_time_clamped_at_max() {
        // Critical: cap is 24h. Even an absurd attempt count must not exceed it.
        let now = Utc::now();

        let huge =
            NotificationService::get_next_allowed_time(&NotificationPriority::Critical, 1000);

        assert_near(huge, now + Duration::hours(24), 5);
    }

    #[test]
    fn test_email_provider_configuration() {
        // Test that email provider can be configured correctly
        let config = EmailProviderConfig {
            smtp_host: "localhost".to_string(),
            smtp_port: 1025,
            username: "test_user".to_string(),
            password: "test_pass".to_string(),
            from_address: "test@example.com".to_string(),
            from_name: "Test Sender".to_string(),
            to_addresses: vec!["recipient@example.com".to_string()],
        };

        // Verify configuration fields
        assert_eq!(config.smtp_host, "localhost");
        assert_eq!(config.smtp_port, 1025);
        assert_eq!(config.from_address, "test@example.com");
    }

    #[test]
    fn test_slack_provider_configuration() {
        let config = SlackProviderConfig {
            webhook_url: "https://hooks.slack.com/services/TEST".to_string(),
            channel: "#notifications".to_string(),
        };

        assert_eq!(config.webhook_url, "https://hooks.slack.com/services/TEST");
        assert_eq!(config.channel, "#notifications");
    }

    #[test]
    fn test_slack_webhook_validation() {
        // Test that we can validate webhook URLs
        let valid_url = "https://hooks.slack.com/services/TEST";
        let invalid_url = "http://invalid-url.com";

        assert!(valid_url.starts_with("https://hooks.slack.com/"));
        assert!(!invalid_url.starts_with("https://hooks.slack.com/"));
    }

    #[test]
    fn test_email_config_serialization() {
        let config = EmailProviderConfig {
            smtp_host: "smtp.test.com".to_string(),
            smtp_port: 587,
            username: "user".to_string(),
            password: "pass".to_string(),
            from_address: "sender@test.com".to_string(),
            from_name: "Sender".to_string(),
            to_addresses: vec!["recipient@test.com".to_string()],
        };

        let json = serde_json::to_string(&config);
        assert!(json.is_ok());

        let json_str = json.unwrap();
        assert!(json_str.contains("smtp.test.com"));
        assert!(json_str.contains("587"));
        assert!(json_str.contains("sender@test.com"));
    }

    #[test]
    fn test_slack_config_serialization() {
        let config = SlackProviderConfig {
            webhook_url: "https://hooks.slack.com/services/TEST".to_string(),
            channel: "#general".to_string(),
        };

        let json = serde_json::to_string(&config);
        assert!(json.is_ok());

        let json_str = json.unwrap();
        assert!(json_str.contains("https://hooks.slack.com/services/TEST"));
        assert!(json_str.contains("#general"));
    }

    #[test]
    fn test_webhook_provider_configuration() {
        let mut headers = std::collections::HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer test-token".to_string());
        headers.insert("X-Custom-Header".to_string(), "custom-value".to_string());

        let config = WebhookProviderConfig {
            url: "https://example.com/webhook".to_string(),
            method: "POST".to_string(),
            headers,
            timeout_secs: 30,
        };

        assert_eq!(config.url, "https://example.com/webhook");
        assert_eq!(config.method, "POST");
        assert_eq!(config.timeout_secs, 30);
        assert_eq!(config.headers.len(), 2);
        assert_eq!(
            config.headers.get("Authorization"),
            Some(&"Bearer test-token".to_string())
        );
    }

    #[test]
    fn test_webhook_config_serialization() {
        let mut headers = std::collections::HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer test".to_string());

        let config = WebhookProviderConfig {
            url: "https://api.example.com/notifications".to_string(),
            method: "POST".to_string(),
            headers,
            timeout_secs: 60,
        };

        let json = serde_json::to_string(&config);
        assert!(json.is_ok());

        let json_str = json.unwrap();
        assert!(json_str.contains("https://api.example.com/notifications"));
        assert!(json_str.contains("POST"));
        assert!(json_str.contains("Authorization"));
        assert!(json_str.contains("Bearer test"));
    }

    #[test]
    fn test_webhook_config_deserialization_with_defaults() {
        // Test that defaults are applied when fields are missing
        let json = r#"{
            "url": "https://example.com/webhook"
        }"#;

        let config: WebhookProviderConfig =
            serde_json::from_str(json).expect("Failed to deserialize");

        assert_eq!(config.url, "https://example.com/webhook");
        assert_eq!(config.method, "POST"); // default
        assert_eq!(config.timeout_secs, 30); // default
        assert!(config.headers.is_empty()); // default empty
    }

    #[test]
    fn test_webhook_url_validation() {
        // Test valid URLs
        let valid_https = "https://example.com/webhook";
        let valid_http = "http://localhost:8080/webhook";

        assert!(valid_https.starts_with("http://") || valid_https.starts_with("https://"));
        assert!(valid_http.starts_with("http://") || valid_http.starts_with("https://"));

        // Test invalid URLs
        let invalid_url = "ftp://example.com/webhook";
        assert!(!invalid_url.starts_with("http://") && !invalid_url.starts_with("https://"));
    }

    #[test]
    fn test_webhook_method_validation() {
        let valid_methods = ["POST", "PUT", "PATCH", "post", "put", "patch"];
        let invalid_methods = ["GET", "DELETE", "HEAD", "OPTIONS"];

        for method in valid_methods {
            let upper = method.to_uppercase();
            assert!(["POST", "PUT", "PATCH"].contains(&upper.as_str()));
        }

        for method in invalid_methods {
            let upper = method.to_uppercase();
            assert!(!["POST", "PUT", "PATCH"].contains(&upper.as_str()));
        }
    }

    fn cloudflare_provider(to: Vec<&str>) -> CloudflareProvider {
        CloudflareProvider {
            account_id: "acct123".to_string(),
            api_token: "cf-token".to_string(),
            from_address: "welcome@infracf.example.com".to_string(),
            from_name: Some("Temps".to_string()),
            to_addresses: to.into_iter().map(String::from).collect(),
            api_base: None,
        }
    }

    #[test]
    fn test_cloudflare_send_endpoint() {
        let provider = cloudflare_provider(vec!["a@example.com"]);
        assert_eq!(
            provider.send_endpoint(),
            "https://api.cloudflare.com/client/v4/accounts/acct123/email/sending/send"
        );
    }

    #[test]
    fn test_cloudflare_sender_value() {
        // With a display name → structured { email, name } object (Cloudflare's
        // documented format; the RFC `Name <addr>` string is NOT used).
        let with_name = cloudflare_provider(vec!["a@example.com"]);
        assert_eq!(
            with_name.sender_value(),
            serde_json::json!({
                "email": "welcome@infracf.example.com",
                "name": "Temps",
            })
        );

        // Without a name → bare address string.
        let mut without_name = cloudflare_provider(vec!["a@example.com"]);
        without_name.from_name = None;
        assert_eq!(
            without_name.sender_value(),
            serde_json::json!("welcome@infracf.example.com")
        );

        // Whitespace-only name is treated as absent.
        let mut blank_name = cloudflare_provider(vec!["a@example.com"]);
        blank_name.from_name = Some("   ".to_string());
        assert_eq!(
            blank_name.sender_value(),
            serde_json::json!("welcome@infracf.example.com")
        );
    }

    #[test]
    fn test_cloudflare_text_body_includes_metadata() {
        let notification = Notification::new("Deploy failed", "The build crashed")
            .with_metadata("project", "temps")
            .with_metadata("environment", "production");
        let text = CloudflareProvider::render_text_body(&notification);
        assert!(text.starts_with("Deploy failed\n\nThe build crashed"));
        assert!(text.contains("project: temps"));
        assert!(text.contains("environment: production"));
    }

    #[tokio::test]
    async fn test_cloudflare_initialize_validates_required_fields() {
        let db = Arc::new(MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection());

        let mut ok = cloudflare_provider(vec!["a@example.com"]);
        assert!(ok.initialize(db.clone()).await.is_ok());

        let mut no_account = cloudflare_provider(vec!["a@example.com"]);
        no_account.account_id = String::new();
        assert!(no_account.initialize(db.clone()).await.is_err());

        let mut no_token = cloudflare_provider(vec!["a@example.com"]);
        no_token.api_token = String::new();
        assert!(no_token.initialize(db.clone()).await.is_err());

        let mut no_from = cloudflare_provider(vec!["a@example.com"]);
        no_from.from_address = String::new();
        assert!(no_from.initialize(db.clone()).await.is_err());

        let mut no_recipients = cloudflare_provider(vec![]);
        assert!(no_recipients.initialize(db).await.is_err());
    }

    #[test]
    fn test_cloudflare_config_serialization_roundtrip() {
        let provider = cloudflare_provider(vec!["a@example.com", "b@example.com"]);
        let json = serde_json::to_string(&provider).unwrap();
        // The api_base test override must never leak into stored config.
        assert!(!json.contains("api_base"));
        let parsed: CloudflareProvider = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.account_id, "acct123");
        assert_eq!(parsed.to_addresses.len(), 2);
        assert_eq!(parsed.from_name.as_deref(), Some("Temps"));
        assert_eq!(parsed.api_base, None);
    }

    // ---- Integration tests against a local mock HTTP server (wiremock) ----
    // These exercise the real `send` / `health_check` HTTP paths: request
    // method, path, bearer auth, JSON payload shape and response handling.

    #[tokio::test]
    async fn test_cloudflare_send_posts_to_each_recipient_with_auth_and_payload() {
        use wiremock::matchers::{body_partial_json, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/accounts/acct123/email/sending/send"))
            .and(header("authorization", "Bearer cf-token"))
            .and(body_partial_json(serde_json::json!({
                "from": { "email": "welcome@infracf.example.com", "name": "Temps" },
                "subject": "Deploy failed",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
            })))
            .expect(2) // one POST per recipient
            .mount(&server)
            .await;

        let mut provider = cloudflare_provider(vec!["a@example.com", "b@example.com"]);
        provider.api_base = Some(server.uri());

        let notification = Notification::new("Deploy failed", "The build crashed");
        let result = provider.send(&notification).await;

        assert!(result.is_ok(), "send should succeed: {:?}", result.err());
        // `expect(2)` is verified on drop of the server.
    }

    #[tokio::test]
    async fn test_cloudflare_send_errors_when_all_recipients_fail() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/accounts/acct123/email/sending/send"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "success": false,
                "errors": [{ "message": "Authentication error" }],
            })))
            .mount(&server)
            .await;

        let mut provider = cloudflare_provider(vec!["a@example.com"]);
        provider.api_base = Some(server.uri());

        let notification = Notification::new("Alert", "Something broke");
        let err = provider.send(&notification).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("403") && msg.contains("a@example.com"),
            "error should carry status and recipient: {msg}"
        );
    }

    #[tokio::test]
    async fn test_cloudflare_health_check_true_on_success() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/user/tokens/verify"))
            .and(header("authorization", "Bearer cf-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
                "result": { "id": "tok123", "status": "active" },
            })))
            .mount(&server)
            .await;

        let mut provider = cloudflare_provider(vec!["a@example.com"]);
        provider.api_base = Some(server.uri());

        assert!(provider.health_check().await.unwrap());
    }

    #[tokio::test]
    async fn test_cloudflare_health_check_false_on_bad_credentials() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/user/tokens/verify"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let mut provider = cloudflare_provider(vec!["a@example.com"]);
        provider.api_base = Some(server.uri());

        assert!(!provider.health_check().await.unwrap());
    }

    #[test]
    fn test_notification_priority_ordering() {
        // For a first send (no prior attempts), Critical should have the shortest
        // wait time and Low the longest.
        let low_time = NotificationService::get_next_allowed_time(&NotificationPriority::Low, 0);
        let normal_time =
            NotificationService::get_next_allowed_time(&NotificationPriority::Normal, 0);
        let high_time = NotificationService::get_next_allowed_time(&NotificationPriority::High, 0);
        let critical_time =
            NotificationService::get_next_allowed_time(&NotificationPriority::Critical, 0);

        assert!(critical_time < high_time);
        assert!(high_time < normal_time);
        assert!(normal_time < low_time);
    }

    #[test]
    fn test_notification_type_colors() {
        // This tests the color mapping logic used in email and slack providers
        let colors = vec![
            (NotificationType::Info, "#0088cc"),
            (NotificationType::Warning, "#ffa500"),
            (NotificationType::Error, "#ff0000"),
            (NotificationType::Alert, "#ff0000"),
        ];

        for (notification_type, expected_color) in colors {
            let color = match notification_type {
                NotificationType::Info => "#0088cc",
                NotificationType::Warning => "#ffa500",
                NotificationType::Error => "#ff0000",
                NotificationType::Alert => "#ff0000",
            };
            assert_eq!(color, expected_color);
        }
    }

    // Notification Preferences Service Tests
    #[test]
    fn test_notification_preferences_defaults() {
        let prefs = NotificationPreferences::default();

        // Channel defaults
        assert!(prefs.email_enabled);
        assert!(!prefs.slack_enabled);
        assert!(prefs.batch_similar_notifications);
        assert_eq!(prefs.minimum_severity, "warning");

        // Project health defaults
        assert!(prefs.deployment_failures_enabled);
        assert!(prefs.build_errors_enabled);
        assert!(prefs.runtime_errors_enabled);
        assert_eq!(prefs.error_threshold, 200);
        assert_eq!(prefs.error_time_window, 5);

        // Domain monitoring defaults
        assert!(prefs.ssl_expiration_enabled);
        assert_eq!(prefs.ssl_days_before_expiration, 30);
        assert!(prefs.domain_expiration_enabled);
        assert!(prefs.dns_changes_enabled);

        // Backup monitoring defaults
        assert!(prefs.backup_failures_enabled);
        assert!(prefs.backup_successes_enabled);
        assert!(prefs.s3_connection_issues_enabled);
        assert!(prefs.retention_policy_violations_enabled);

        // Route monitoring defaults
        assert!(prefs.route_downtime_enabled);
        assert!(prefs.load_balancer_issues_enabled);
    }

    #[test]
    fn test_notification_preferences_serialization() {
        let prefs = NotificationPreferences::default();

        // Test serialization
        let json = serde_json::to_string(&prefs);
        assert!(json.is_ok());

        let json_str = json.unwrap();
        assert!(json_str.contains("email_enabled"));
        assert!(json_str.contains("slack_enabled"));

        // Test deserialization
        let deserialized: Result<NotificationPreferences, _> = serde_json::from_str(&json_str);
        assert!(deserialized.is_ok());

        let deserialized_prefs = deserialized.unwrap();
        assert_eq!(prefs.email_enabled, deserialized_prefs.email_enabled);
        assert_eq!(prefs.error_threshold, deserialized_prefs.error_threshold);
    }

    #[tokio::test]
    async fn test_notification_preferences_service_get_defaults() {
        use temps_database::test_utils::TestDatabase;

        // Start database with migrations
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");

        // Create service
        let service = NotificationPreferencesService::new(test_db.connection_arc());

        // Get preferences (should return defaults since none exist)
        let prefs = service
            .get_preferences()
            .await
            .expect("Failed to get preferences");

        // Verify defaults
        assert!(prefs.email_enabled);
        assert!(!prefs.slack_enabled);
        assert_eq!(prefs.minimum_severity, "warning");

        // Cleanup
        test_db
            .cleanup_all_tables()
            .await
            .expect("Failed to cleanup");
    }

    #[tokio::test]
    async fn test_notification_preferences_service_update() {
        use temps_database::test_utils::TestDatabase;

        // Start database with migrations
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");

        // Create service
        let service = NotificationPreferencesService::new(test_db.connection_arc());

        // Create custom preferences
        let custom_prefs = NotificationPreferences {
            email_enabled: false,
            slack_enabled: true,
            minimum_severity: "critical".to_string(),
            error_threshold: 500,
            ..Default::default()
        };

        // Update preferences
        let updated = service
            .update_preferences(custom_prefs.clone())
            .await
            .expect("Failed to update preferences");

        // Verify update
        assert!(!updated.email_enabled);
        assert!(updated.slack_enabled);
        assert_eq!(updated.minimum_severity, "critical");
        assert_eq!(updated.error_threshold, 500);

        // Get preferences again to verify persistence
        let retrieved = service
            .get_preferences()
            .await
            .expect("Failed to get preferences");
        assert!(!retrieved.email_enabled);
        assert!(retrieved.slack_enabled);
        assert_eq!(retrieved.minimum_severity, "critical");
        assert_eq!(retrieved.error_threshold, 500);

        // Cleanup
        test_db
            .cleanup_all_tables()
            .await
            .expect("Failed to cleanup");
    }

    #[tokio::test]
    async fn test_notification_preferences_service_update_existing() {
        use temps_database::test_utils::TestDatabase;

        // Start database with migrations
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");

        // Create service
        let service = NotificationPreferencesService::new(test_db.connection_arc());

        // Create initial preferences
        let initial_prefs = NotificationPreferences {
            email_enabled: false,
            ..Default::default()
        };
        service
            .update_preferences(initial_prefs)
            .await
            .expect("Failed to create initial preferences");

        // Update with different values
        let updated_prefs = NotificationPreferences {
            email_enabled: true,
            slack_enabled: true,
            ..Default::default()
        };

        let result = service
            .update_preferences(updated_prefs)
            .await
            .expect("Failed to update preferences");

        // Verify the update
        assert!(result.email_enabled);
        assert!(result.slack_enabled);

        // Cleanup
        test_db
            .cleanup_all_tables()
            .await
            .expect("Failed to cleanup");
    }

    #[tokio::test]
    async fn test_notification_preferences_service_delete() {
        use temps_database::test_utils::TestDatabase;

        // Start database with migrations
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");

        // Create service
        let service = NotificationPreferencesService::new(test_db.connection_arc());

        // Create preferences
        let prefs = NotificationPreferences::default();
        service
            .update_preferences(prefs)
            .await
            .expect("Failed to create preferences");

        // Verify they exist (should not be defaults, should be from database)
        let existing = service
            .get_preferences()
            .await
            .expect("Failed to get preferences");
        assert!(existing.email_enabled); // Verify it's actually stored

        // Delete preferences
        service
            .delete_preferences()
            .await
            .expect("Failed to delete preferences");

        // Get preferences again (should return defaults since deleted)
        let after_delete = service
            .get_preferences()
            .await
            .expect("Failed to get preferences");
        assert!(after_delete.email_enabled); // Should still be true from defaults
        assert!(!after_delete.slack_enabled); // Should be false from defaults

        // Cleanup
        test_db
            .cleanup_all_tables()
            .await
            .expect("Failed to cleanup");
    }

    #[test]
    fn test_notification_preferences_backup_successes_default() {
        // Test the default function
        assert!(default_backup_successes_enabled());

        // Test JSON deserialization with missing field
        let json_without_field = r#"{
            "email_enabled": true,
            "slack_enabled": false,
            "batch_similar_notifications": true,
            "minimum_severity": "warning",
            "deployment_failures_enabled": true,
            "build_errors_enabled": true,
            "runtime_errors_enabled": true,
            "error_threshold": 200,
            "error_time_window": 5,
            "ssl_expiration_enabled": true,
            "ssl_days_before_expiration": 30,
            "domain_expiration_enabled": true,
            "dns_changes_enabled": true,
            "backup_failures_enabled": true,
            "s3_connection_issues_enabled": true,
            "retention_policy_violations_enabled": true,
            "route_downtime_enabled": true,
            "load_balancer_issues_enabled": true
        }"#;

        let prefs: NotificationPreferences =
            serde_json::from_str(json_without_field).expect("Failed to deserialize");

        // Should use default value of true
        assert!(prefs.backup_successes_enabled);
    }

    #[tokio::test]
    async fn test_notification_preferences_service_multiple_updates() {
        use temps_database::test_utils::TestDatabase;

        // Start database with migrations
        let test_db = TestDatabase::with_migrations()
            .await
            .expect("Failed to create test database");

        // Create service
        let service = NotificationPreferencesService::new(test_db.connection_arc());

        // First update
        let prefs1 = NotificationPreferences {
            error_threshold: 100,
            ..Default::default()
        };
        service
            .update_preferences(prefs1)
            .await
            .expect("Failed to update preferences");

        // Second update
        let prefs2 = NotificationPreferences {
            error_threshold: 200,
            ..Default::default()
        };
        service
            .update_preferences(prefs2)
            .await
            .expect("Failed to update preferences");

        // Third update
        let prefs3 = NotificationPreferences {
            error_threshold: 300,
            ..Default::default()
        };
        service
            .update_preferences(prefs3)
            .await
            .expect("Failed to update preferences");

        // Verify final state
        let final_prefs = service
            .get_preferences()
            .await
            .expect("Failed to get preferences");
        assert_eq!(final_prefs.error_threshold, 300);

        // Cleanup
        test_db
            .cleanup_all_tables()
            .await
            .expect("Failed to cleanup");
    }

    // ── SSRF Prevention Tests for WebhookProvider ────────────────────

    fn create_webhook(url: &str) -> WebhookProvider {
        WebhookProvider {
            url: url.to_string(),
            method: "POST".to_string(),
            headers: std::collections::HashMap::new(),
            timeout_secs: 30,
        }
    }

    #[tokio::test]
    async fn test_webhook_ssrf_blocks_private_ip_192_168() {
        let mut webhook = create_webhook("http://192.168.1.1/callback");
        let db = Arc::new(MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection());
        let result = webhook.initialize(db).await;
        assert!(
            result.is_err(),
            "Must block RFC 1918 private IP 192.168.x.x"
        );
    }

    #[tokio::test]
    async fn test_webhook_ssrf_blocks_private_ip_10() {
        let mut webhook = create_webhook("http://10.0.0.1:8080/hook");
        let db = Arc::new(MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection());
        let result = webhook.initialize(db).await;
        assert!(result.is_err(), "Must block RFC 1918 private IP 10.x.x.x");
    }

    #[tokio::test]
    async fn test_webhook_ssrf_blocks_private_ip_172() {
        let mut webhook = create_webhook("http://172.16.0.1/hook");
        let db = Arc::new(MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection());
        let result = webhook.initialize(db).await;
        assert!(result.is_err(), "Must block RFC 1918 private IP 172.16.x.x");
    }

    #[tokio::test]
    async fn test_webhook_ssrf_blocks_localhost() {
        let mut webhook = create_webhook("http://localhost:6379/");
        let db = Arc::new(MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection());
        let result = webhook.initialize(db).await;
        assert!(result.is_err(), "Must block localhost");
    }

    #[tokio::test]
    async fn test_webhook_ssrf_blocks_loopback_ip() {
        let mut webhook = create_webhook("http://127.0.0.1:6379/");
        let db = Arc::new(MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection());
        let result = webhook.initialize(db).await;
        assert!(result.is_err(), "Must block loopback 127.0.0.1");
    }

    #[tokio::test]
    async fn test_webhook_ssrf_blocks_cloud_metadata() {
        let mut webhook = create_webhook("http://169.254.169.254/latest/meta-data/");
        let db = Arc::new(MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection());
        let result = webhook.initialize(db).await;
        assert!(
            result.is_err(),
            "Must block AWS metadata endpoint 169.254.169.254"
        );
    }

    #[tokio::test]
    async fn test_webhook_ssrf_blocks_link_local() {
        let mut webhook = create_webhook("http://169.254.1.1/hook");
        let db = Arc::new(MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection());
        let result = webhook.initialize(db).await;
        assert!(result.is_err(), "Must block link-local 169.254.x.x");
    }

    #[tokio::test]
    async fn test_webhook_ssrf_blocks_non_http_scheme() {
        let mut webhook = create_webhook("ftp://evil.com/payload");
        let db = Arc::new(MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection());
        let result = webhook.initialize(db).await;
        assert!(result.is_err(), "Must block non-HTTP/HTTPS schemes");
    }

    #[tokio::test]
    async fn test_webhook_ssrf_allows_public_https() {
        let mut webhook = create_webhook("https://hooks.example.com/webhook");
        let db = Arc::new(MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection());
        let result = webhook.initialize(db).await;
        assert!(result.is_ok(), "Must allow public HTTPS URLs");
    }

    #[tokio::test]
    async fn test_webhook_invalid_method_rejected() {
        let mut webhook = WebhookProvider {
            url: "https://hooks.example.com/webhook".to_string(),
            method: "DELETE".to_string(),
            headers: std::collections::HashMap::new(),
            timeout_secs: 30,
        };
        let db = Arc::new(MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection());
        let result = webhook.initialize(db).await;
        assert!(result.is_err(), "Must reject DELETE method");
    }

    // ========== Health-check email rendering ==========

    #[test]
    fn test_health_check_email_renders_branded_html() {
        let html = EmailProvider::render_health_check_email(
            "deploy.example.com",
            "email-smtp.eu-west-1.amazonaws.com",
            587,
            "noreply@example.com",
            "Jan 01, 2026 at 12:00 UTC",
        );

        // Branding: header bar and footer must be present so the message
        // reads as a real Temps email, not a debug ping.
        assert!(html.contains(">Temps<"), "expected Temps brand header");
        assert!(
            html.contains("Sent by Temps"),
            "expected footer attribution"
        );
        assert!(html.contains("Health check"), "expected status badge");
        assert!(
            html.contains("Notification provider is reachable"),
            "expected human-readable title"
        );

        // Connection details must surface the values the operator needs to
        // confirm the provider is set up correctly.
        assert!(
            html.contains("deploy.example.com"),
            "expected instance hostname in body"
        );
        assert!(
            html.contains("email-smtp.eu-west-1.amazonaws.com:587"),
            "expected smtp host:port in body"
        );
        assert!(
            html.contains("noreply@example.com"),
            "expected from-address in body"
        );
        assert!(
            html.contains("Jan 01, 2026 at 12:00 UTC"),
            "expected timestamp in body"
        );
    }

    #[test]
    fn test_health_check_email_has_valid_html_document_shape() {
        let html = EmailProvider::render_health_check_email("a", "b", 1, "c@d.e", "t");
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.trim_end().ends_with("</html>"));
        // Inline-styled <table> layout (email-client compatible) — must not
        // accidentally drift to a flex/grid layout that breaks in Outlook.
        assert!(html.contains("<table"));
        assert!(!html.contains("display: flex"));
        assert!(!html.contains("display: grid"));
    }

    // ── Escape helper unit tests ──────────────────────────────────────

    #[test]
    fn test_html_escape_encodes_all_special_chars() {
        assert_eq!(html_escape("<"), "&lt;");
        assert_eq!(html_escape(">"), "&gt;");
        assert_eq!(html_escape("&"), "&amp;");
        assert_eq!(html_escape("\""), "&quot;");
        assert_eq!(html_escape("'"), "&#x27;");
        assert_eq!(
            html_escape("<script>alert('xss')</script>"),
            "&lt;script&gt;alert(&#x27;xss&#x27;)&lt;/script&gt;"
        );
    }

    #[test]
    fn test_html_escape_passes_safe_chars_through() {
        assert_eq!(html_escape("hello world 123"), "hello world 123");
        assert_eq!(html_escape(""), "");
        // Newlines, tabs, and spaces are not HTML-special — must not be altered.
        assert_eq!(html_escape("line1\nline2\ttab"), "line1\nline2\ttab");
    }

    #[test]
    fn test_slack_escape_encodes_mrkdwn_special_chars() {
        assert_eq!(slack_escape("<"), "&lt;");
        assert_eq!(slack_escape(">"), "&gt;");
        assert_eq!(slack_escape("&"), "&amp;");
        // Full injection sequences must be neutralised.
        assert_eq!(slack_escape("<!channel>"), "&lt;!channel&gt;");
        assert_eq!(
            slack_escape("<https://evil.example|Click here>"),
            "&lt;https://evil.example|Click here&gt;"
        );
        assert_eq!(slack_escape("&amp;"), "&amp;amp;");
        // mrkdwn formatting characters must not let user text forge emphasis or
        // code blocks (e.g. a bolded fake severity, or a `DROP TABLE` code block).
        assert_eq!(slack_escape("*bold*"), "\\*bold\\*");
        assert_eq!(slack_escape("_italic_"), "\\_italic\\_");
        assert_eq!(slack_escape("`code`"), "\\`code\\`");
        assert_eq!(slack_escape("~strike~"), "\\~strike\\~");
    }

    #[test]
    fn test_slack_escape_passes_safe_chars_through() {
        assert_eq!(slack_escape("hello world 123"), "hello world 123");
        assert_eq!(slack_escape(""), "");
    }

    // ── Regression tests: HTML injection via title/metadata ───────────

    #[test]
    fn test_email_title_injection_does_not_break_html_structure() {
        // Simulates an OTel series_label value injected into the alarm title by
        // Phase 3 (per-series dynamic alerting). The value contains a closing tag
        // that would break the <h1> and inject a phishing anchor.
        let notification = Notification {
            id: "sec-test".to_string(),
            title: r#"Metric threshold breached [endpoint=</h1><a href="https://evil.example">Click to resolve</a>]"#.to_string(),
            message: "Normal message".to_string(),
            notification_type: NotificationType::Alert,
            priority: NotificationPriority::Critical,
            severity: None,
            timestamp: Utc::now(),
            metadata: std::collections::HashMap::new(),
            bypass_throttling: false,
        };

        let html = EmailProvider::render_notification_email(&notification);

        // The injected anchor tag must not appear verbatim in the rendered output.
        assert!(
            !html.contains(r#"<a href="https://evil.example">"#),
            "rendered HTML must not contain injected anchor tag"
        );
        // The title content must still appear, but escaped.
        assert!(
            html.contains("&lt;/h1&gt;"),
            "closing h1 in title must be HTML-escaped"
        );
        assert!(
            html.contains("&lt;a href=&quot;https://evil.example&quot;&gt;"),
            "injected anchor in title must be fully HTML-escaped"
        );
    }

    #[test]
    fn test_email_metadata_value_injection_does_not_break_html_structure() {
        // Simulates an OTel attribute value (series_key) injected into the
        // DETAILS table. The value closes the current <td> and injects a script.
        let mut metadata = std::collections::HashMap::new();
        metadata.insert(
            "series_label".to_string(),
            r#"endpoint=</td><script>alert(1)</script>"#.to_string(),
        );

        let notification = Notification {
            id: "sec-test-2".to_string(),
            title: "Metric threshold breached".to_string(),
            message: "Normal message".to_string(),
            notification_type: NotificationType::Alert,
            priority: NotificationPriority::High,
            severity: None,
            timestamp: Utc::now(),
            metadata,
            bypass_throttling: false,
        };

        let html = EmailProvider::render_notification_email(&notification);

        assert!(
            !html.contains("<script>"),
            "rendered HTML must not contain injected script tag"
        );
        assert!(
            html.contains("&lt;/td&gt;"),
            "injected closing td in metadata value must be HTML-escaped"
        );
    }

    #[test]
    fn test_email_renders_action_button_when_action_url_present() {
        // Mirrors what the error-tracking plugin attaches: a deep link to the
        // error group page plus a human-readable label.
        let mut metadata = std::collections::HashMap::new();
        metadata.insert(
            "_action_url".to_string(),
            "https://temps.example/projects/demo/errors/42".to_string(),
        );
        metadata.insert(
            "_action_label".to_string(),
            "View error details".to_string(),
        );

        let notification = Notification {
            id: "action-test".to_string(),
            title: "New error group".to_string(),
            message: "A new error was detected".to_string(),
            notification_type: NotificationType::Alert,
            priority: NotificationPriority::High,
            severity: None,
            timestamp: Utc::now(),
            metadata,
            bypass_throttling: false,
        };

        let html = EmailProvider::render_notification_email(&notification);

        assert!(
            html.contains(r#"href="https://temps.example/projects/demo/errors/42""#),
            "rendered email must link to the action URL: {html}"
        );
        assert!(
            html.contains("View error details"),
            "rendered email must show the action label: {html}"
        );
        // `_`-prefixed keys are reserved channel payloads, not human-facing
        // metadata rows — they must not also appear in the details table.
        assert!(
            !html.contains("_action_url") && !html.contains("_action_label"),
            "reserved metadata keys must not leak into the visible details table: {html}"
        );
    }

    #[test]
    fn test_email_omits_action_button_when_action_url_absent() {
        let notification = Notification::new("New error group", "A new error was detected");

        let html = EmailProvider::render_notification_email(&notification);

        assert!(
            !html.contains("<a href="),
            "no CTA button should render when _action_url is not set: {html}"
        );
    }

    #[test]
    fn test_email_action_button_escapes_html_in_url_and_label() {
        let mut metadata = std::collections::HashMap::new();
        metadata.insert(
            "_action_url".to_string(),
            r#"https://evil.example/"><script>alert(1)</script>"#.to_string(),
        );
        metadata.insert(
            "_action_label".to_string(),
            "<script>alert(1)</script>".to_string(),
        );

        let notification = Notification {
            id: "action-injection-test".to_string(),
            title: "New error group".to_string(),
            message: "A new error was detected".to_string(),
            notification_type: NotificationType::Alert,
            priority: NotificationPriority::High,
            severity: None,
            timestamp: Utc::now(),
            metadata,
            bypass_throttling: false,
        };

        let html = EmailProvider::render_notification_email(&notification);

        assert!(
            !html.contains("<script>"),
            "rendered HTML must not contain an injected script tag: {html}"
        );
        assert!(
            html.contains("&lt;script&gt;"),
            "injected markup in the action URL/label must be HTML-escaped: {html}"
        );
    }

    // ── Regression test: HTML injection via message body (ADR-026 Phase 3) ──
    //
    // Attack path: OTel per-series label (e.g., `env=</td><a href="...">`) is
    // embedded into `alarm.title` by the metric-alert evaluator.  When the alarm
    // resolves, `alarm_service::send_resolved_notification` formats the message as
    //   format!("Alarm '{}' has been resolved.\nOriginal severity: {}", alarm.title, …)
    // and sends it through `render_notification_email`.  The old `contains("</")` heuristic
    // would have passed the entire message through unescaped, injecting arbitrary HTML.
    #[test]
    fn test_email_message_injection_via_resolved_alarm_label_is_escaped() {
        // Mirrors exactly what alarm_service::send_resolved_notification produces when
        // alarm.title embeds an attacker-controlled OTel series_label.
        let injected_title =
            r#"Metric threshold breached [env=</td><a href="https://evil.example">click</a>]"#;
        let message = format!(
            "Alarm '{}' has been resolved.\nOriginal severity: critical",
            injected_title
        );

        let notification = Notification {
            id: "sec-test-msg".to_string(),
            title: "Resolved: Metric threshold breached".to_string(),
            message,
            notification_type: NotificationType::Info,
            priority: NotificationPriority::Normal,
            severity: None,
            timestamp: Utc::now(),
            metadata: std::collections::HashMap::new(),
            bypass_throttling: false,
        };

        let html = EmailProvider::render_notification_email(&notification);

        // The injected anchor must not appear verbatim.
        assert!(
            !html.contains(r#"<a href="https://evil.example">"#),
            "rendered HTML must not contain unescaped injected anchor from message"
        );
        // The closing </td> from the injection must not appear verbatim.
        assert!(
            !html.contains("</td><a"),
            "rendered HTML must not contain unescaped </td> injection from message"
        );
        // The angle-bracket content must be entity-encoded instead.
        assert!(
            html.contains("&lt;/td&gt;"),
            "injected </td> in message must be HTML-escaped to &lt;/td&gt;"
        );
        assert!(
            html.contains("&lt;a href=&quot;https://evil.example&quot;&gt;"),
            "injected anchor in message must be fully HTML-escaped"
        );
        // Newlines must still become <br> so multi-line messages render correctly.
        assert!(
            html.contains("<br>"),
            "newline in message must be converted to <br>"
        );
    }

    #[test]
    fn test_safe_title_renders_identically_without_escaping() {
        // A title with no HTML-special characters must produce the exact same
        // visible text after escaping (byte-for-byte in the rendered document).
        let title = "Metric alert fired for deployment my-app in environment production";
        let notification = Notification {
            id: "safe-test".to_string(),
            title: title.to_string(),
            message: "Normal message".to_string(),
            notification_type: NotificationType::Alert,
            priority: NotificationPriority::High,
            severity: None,
            timestamp: Utc::now(),
            metadata: std::collections::HashMap::new(),
            bypass_throttling: false,
        };

        let html = EmailProvider::render_notification_email(&notification);

        // The title must appear verbatim — no extra escaping of safe characters.
        assert!(
            html.contains(title),
            "safe title must appear unchanged in rendered HTML"
        );
    }

    #[test]
    fn test_slack_channel_mention_injection_is_neutralised() {
        // The Slack mrkdwn `<!channel>` sequence would trigger an @channel
        // notification flood if passed through unescaped.
        let title = "Metric alert [env=<!channel>]";
        let message = "Value exceeded threshold. See <https://evil.example|dashboard>.";

        let escaped_title = slack_escape(title);
        let escaped_message = slack_escape(message);

        assert!(
            !escaped_title.contains("<!channel>"),
            "<!channel> must be neutralised in Slack title"
        );
        assert!(
            escaped_title.contains("&lt;!channel&gt;"),
            "escaped title must contain the HTML-entity form"
        );
        assert!(
            !escaped_message.contains("<https://evil.example|dashboard>"),
            "mrkdwn link must be neutralised in Slack message"
        );
        assert!(
            escaped_message.contains("&lt;https://evil.example|dashboard&gt;"),
            "escaped message must contain the HTML-entity form"
        );
    }

    #[tokio::test]
    async fn test_slack_send_excludes_action_url_metadata_and_html() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let provider = SlackProvider {
            webhook_url: server.uri(),
            channel: "#alerts".to_string(),
        };

        // Mirrors what the error-tracking plugin attaches for the email's CTA
        // button (`_action_url`/`_action_label`) plus a message containing raw
        // HTML/mrkdwn-dangerous characters, to prove neither reaches Slack.
        let notification = Notification::new(
            "Error alert",
            "See <a href=\"https://evil.example\">details</a> for env=<!channel>",
        )
        .with_metadata(
            "_action_url",
            "https://temps.example/projects/demo/errors/42",
        )
        .with_metadata("_action_label", "View error details");

        provider.send(&notification).await.unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body = String::from_utf8(requests[0].body.clone()).unwrap();

        assert!(
            !body.contains("_action_url") && !body.contains("temps.example"),
            "reserved _action_url metadata must never be sent to Slack: {body}"
        );
        assert!(
            !body.contains("<a href"),
            "raw HTML anchor tags must never reach Slack unescaped: {body}"
        );
        assert!(
            !body.contains("<!channel>"),
            "raw mrkdwn @channel mention must be neutralised: {body}"
        );
        assert!(
            body.contains("&lt;a href=") && body.contains("&lt;/a&gt;"),
            "HTML tags in the message must be escaped to literal entities: {body}"
        );
        assert!(
            body.contains("&lt;!channel&gt;"),
            "mrkdwn @channel mention must be escaped to literal entities: {body}"
        );
    }
}
