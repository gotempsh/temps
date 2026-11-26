// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Request and response shapes of status page webhooks.

use serde::{Deserialize, Serialize};
use temps_core::UtcDateTime;
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StatusPageWebhookResponse {
    pub id: i32,
    pub status_page_id: i32,
    pub url: String,
    pub enabled: bool,
    /// The signing secret is stored encrypted and shown only when the
    /// webhook is created.
    pub has_secret: bool,
    #[schema(value_type = Option<String>, format = "date-time")]
    pub last_attempt_at: Option<UtcDateTime>,
    #[schema(value_type = Option<String>, format = "date-time")]
    pub last_success_at: Option<UtcDateTime>,
    /// Why the last delivery failed, until one succeeds.
    pub last_failure_reason: Option<String>,
    #[schema(value_type = String, format = "date-time")]
    pub created_at: UtcDateTime,
    #[schema(value_type = String, format = "date-time")]
    pub updated_at: UtcDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StatusPageWebhookListResponse {
    /// Newest first.
    pub items: Vec<StatusPageWebhookResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

/// The new webhook and its signing secret. The secret is shown only here.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StatusPageWebhookCreatedResponse {
    pub webhook: StatusPageWebhookResponse,
    /// Verify `X-Temps-Signature` with it. Store it now: it is never shown
    /// again.
    pub secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateStatusPageWebhookRequest {
    /// `https://` URL with a public address, at most 2048 characters.
    pub url: String,
    /// Whether events are posted to it (default `true`). A webhook created
    /// switched off receives nothing until it is enabled.
    #[serde(default = "enabled_by_default")]
    #[schema(default = true)]
    pub enabled: bool,
}

fn enabled_by_default() -> bool {
    true
}

/// Fields left out are unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct UpdateStatusPageWebhookRequest {
    pub url: Option<String>,
    pub enabled: Option<bool>,
}
