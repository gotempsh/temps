// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Typed errors for notification delivery, provider management and digests.
//!
//! Messages here reach logs and, through `POST /notification-providers/{id}/test`,
//! the API. They name providers, hosts and recipients, never a credential:
//! webhook URLs appear in their scheme-and-host form, and a stored config that
//! fails to parse is described without quoting its values.

use crate::routing::NotificationRouteError;
use crate::services::{
    CloudNotificationError, EmailDeliveryError, NotificationProviderConfigMergeError,
    WebhookDeliveryError,
};
use thiserror::Error;

pub type Result<T, E = NotificationError> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum NotificationError {
    #[error("Notification provider with ID {provider_id} not found")]
    ProviderNotFound { provider_id: i32 },

    #[error("Notification provider {provider_id} has unsupported type '{provider_type}'")]
    UnsupportedProviderType {
        provider_id: i32,
        provider_type: String,
    },

    #[error("Failed to {operation}: {source}")]
    Database {
        operation: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },

    #[error("Failed to {operation} for notification provider {provider_id}: {source}")]
    ProviderDatabase {
        provider_id: i32,
        operation: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },

    #[error("Failed to fetch project {project_id}: {source}")]
    ProjectLookup {
        project_id: i32,
        #[source]
        source: sea_orm::DbErr,
    },

    #[error("Project {project_id} not found")]
    ProjectNotFound { project_id: i32 },

    #[error("Failed to fetch vulnerability scan {scan_id}: {source}")]
    ScanLookup {
        scan_id: i32,
        #[source]
        source: sea_orm::DbErr,
    },

    #[error("Vulnerability scan {scan_id} not found")]
    ScanNotFound { scan_id: i32 },

    #[error("Failed to query {metric} of project {project_id} for the weekly digest: {source}")]
    DigestProjectQuery {
        project_id: i32,
        metric: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },

    #[error("Failed to query {metric} of funnel {funnel_id} for the weekly digest: {source}")]
    DigestFunnelQuery {
        funnel_id: i32,
        metric: &'static str,
        #[source]
        source: sea_orm::DbErr,
    },

    #[error("Admin role not found")]
    AdminRoleNotFound,

    #[error("Failed to get providers {source}")]
    ProviderResolution {
        #[source]
        source: NotificationRouteError,
    },

    #[error(transparent)]
    Routing(#[from] NotificationRouteError),

    #[error("Failed to decrypt config{}: {reason}", provider_suffix(*.provider_id))]
    ConfigDecryption {
        provider_id: Option<i32>,
        reason: String,
    },

    #[error("Failed to parse decrypted config{}: {reason}", provider_suffix(*.provider_id))]
    ConfigParse {
        provider_id: Option<i32>,
        reason: String,
    },

    #[error("Failed to serialize config for notification provider {provider_id}: {reason}")]
    ConfigSerialization { provider_id: i32, reason: String },

    #[error("Failed to encrypt config for notification provider {provider_id}: {reason}")]
    ConfigEncryption { provider_id: i32, reason: String },

    #[error(transparent)]
    ConfigMerge(#[from] NotificationProviderConfigMergeError),

    #[error("Failed to serialize {what} for notification batch '{batch_key}': {source}")]
    MetadataSerialization {
        what: &'static str,
        batch_key: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("Cloudflare {field} cannot be empty")]
    CloudflareFieldEmpty { field: &'static str },

    #[error("Cloudflare provider requires at least one recipient in to_addresses")]
    CloudflareNoRecipientsConfigured,

    #[error("Cloudflare provider had no recipients to deliver to")]
    CloudflareNoRecipients,

    #[error("Cloudflare email send to {to} failed (request error): {reason}")]
    CloudflareRequest { to: String, reason: String },

    #[error("Cloudflare email send to {to} failed with status {status}: {body}")]
    CloudflareRejected {
        to: String,
        status: u16,
        body: String,
    },

    #[error("Failed to build the HTTP client for the {provider} provider: {reason}")]
    HttpClientBuild {
        provider: &'static str,
        reason: String,
    },

    #[error("Invalid Slack webhook URL")]
    InvalidSlackWebhook,

    #[error("Invalid HTTP method: {method}. Must be POST, PUT, or PATCH")]
    InvalidWebhookMethod { method: String },

    #[error("Webhook request failed with status {status}: {body}")]
    WebhookRejected { status: u16, body: String },

    #[error(transparent)]
    Webhook(#[from] WebhookDeliveryError),

    #[error(transparent)]
    Cloud(#[from] CloudNotificationError),

    #[error("Email provider not initialized")]
    EmailProviderNotInitialized,

    #[error("Invalid sender address '{address}': {reason}")]
    InvalidSender { address: String, reason: String },

    #[error("Failed to build the email for {recipient}: {reason}")]
    EmailBuild { recipient: String, reason: String },

    #[error("Failed to configure the SMTP transport for {smtp_host}:{smtp_port}: {reason}")]
    SmtpTransport {
        smtp_host: String,
        smtp_port: u16,
        reason: String,
    },

    #[error(transparent)]
    EmailDelivery(#[from] EmailDeliveryError),

    #[error("No recipient for transactional email")]
    NoTransactionalRecipient,

    #[error("Invalid recipient {address}: {reason}")]
    InvalidRecipient { address: String, reason: String },

    #[error("SMTP send failed for {address}: {reason}")]
    SmtpSend { address: String, reason: String },

    #[error("Transactional email could not be delivered")]
    TransactionalUndelivered,

    #[error("Failed to send weekly digest: {source}")]
    DigestSend {
        #[source]
        source: Box<NotificationError>,
    },

    #[error("Failed to send vulnerability notification for scan {scan_id}: {source}")]
    VulnerabilitySend {
        scan_id: i32,
        #[source]
        source: Box<NotificationError>,
    },
}

impl NotificationError {
    /// Build the `map_err` argument for a query with no single provider to name.
    pub(crate) fn database(operation: &'static str) -> impl FnOnce(sea_orm::DbErr) -> Self {
        move |source| Self::Database { operation, source }
    }

    /// Build the `map_err` argument for a query about one provider.
    pub(crate) fn provider_database(
        provider_id: i32,
        operation: &'static str,
    ) -> impl FnOnce(sea_orm::DbErr) -> Self {
        move |source| Self::ProviderDatabase {
            provider_id,
            operation,
            source,
        }
    }
}

fn provider_suffix(provider_id: Option<i32>) -> String {
    provider_id
        .map(|id| format!(" for notification provider {id}"))
        .unwrap_or_default()
}

/// Describe why a decrypted provider config failed to parse. A wrong-typed
/// value makes serde quote it (`invalid type: string "..."`), and config values
/// are SMTP passwords and API tokens, so only messages that name a field alone
/// pass through.
pub(crate) fn config_parse_reason(error: &serde_json::Error) -> String {
    let message = error.to_string();
    if ["missing field", "unknown field", "duplicate field"]
        .iter()
        .any(|prefix| message.starts_with(prefix))
    {
        return message;
    }
    format!(
        "a value has the wrong type or syntax at line {} column {}",
        error.line(),
        error.column()
    )
}

/// What an error may say about a webhook URL: its scheme and host. The path,
/// query and userinfo of a webhook URL routinely carry the credential.
pub(crate) fn webhook_url_display(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return "<invalid URL>".to_string();
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    format!("{scheme}://{host}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhook_urls_are_displayed_without_their_credentials() {
        assert_eq!(
            webhook_url_display("https://user:hunter2@hooks.example.test:8443/t/SECRET?k=SECRET"),
            "https://hooks.example.test:8443"
        );
        assert_eq!(
            webhook_url_display("https://hooks.example.test?token=SECRET#SECRET"),
            "https://hooks.example.test"
        );
        assert_eq!(webhook_url_display("SECRET"), "<invalid URL>");
    }

    #[test]
    fn a_wrong_typed_config_value_is_not_quoted() {
        #[derive(serde::Deserialize, Debug)]
        struct Config {
            #[allow(dead_code, reason = "only deserialization is exercised")]
            port: u16,
        }
        let error = serde_json::from_str::<Config>(r#"{"port":"hunter2"}"#)
            .expect_err("a string is not a port");
        assert!(error.to_string().contains("hunter2"), "serde quotes values");

        let reason = config_parse_reason(&error);
        assert!(!reason.contains("hunter2"), "{reason}");
        assert!(reason.contains("line 1"), "{reason}");
    }

    #[test]
    fn a_missing_config_field_is_named() {
        #[derive(serde::Deserialize, Debug)]
        struct Config {
            #[allow(dead_code, reason = "only deserialization is exercised")]
            smtp_host: String,
        }
        let error = serde_json::from_str::<Config>("{}").expect_err("the field is required");
        assert!(config_parse_reason(&error).contains("smtp_host"));
    }

    #[test]
    fn wrapped_sends_keep_the_cause_and_the_scan() {
        let digest = NotificationError::DigestSend {
            source: Box::new(NotificationError::AdminRoleNotFound),
        };
        assert_eq!(
            digest.to_string(),
            "Failed to send weekly digest: Admin role not found"
        );
        let scan = NotificationError::VulnerabilitySend {
            scan_id: 11,
            source: Box::new(NotificationError::AdminRoleNotFound),
        };
        assert_eq!(
            scan.to_string(),
            "Failed to send vulnerability notification for scan 11: Admin role not found"
        );
    }

    #[test]
    fn provider_errors_carry_the_provider_and_operation() {
        let error = NotificationError::provider_database(7, "delete the provider")(
            sea_orm::DbErr::Custom("connection lost".to_string()),
        );
        let message = error.to_string();
        assert!(message.contains("delete the provider"), "{message}");
        assert!(message.contains("provider 7"), "{message}");
        assert!(message.contains("connection lost"), "{message}");

        let missing = NotificationError::ProviderNotFound { provider_id: 7 }.to_string();
        assert_eq!(missing, "Notification provider with ID 7 not found");

        let parse = NotificationError::ConfigParse {
            provider_id: Some(7),
            reason: "bad".to_string(),
        };
        assert_eq!(
            parse.to_string(),
            "Failed to parse decrypted config for notification provider 7: bad"
        );
        let parse = NotificationError::ConfigParse {
            provider_id: None,
            reason: "bad".to_string(),
        };
        assert_eq!(parse.to_string(), "Failed to parse decrypted config: bad");
    }
}
