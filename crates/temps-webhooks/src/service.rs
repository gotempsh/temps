// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Webhook service for managing webhooks and delivering events.

use crate::events::{WebhookEvent, WebhookEventType};
use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, Condition, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect,
};
use sha2::Sha256;
use std::sync::Arc;
use thiserror::Error;
use tracing::{error, info, warn};
// For URL validation

type HmacSha256 = Hmac<Sha256>;

/// Webhook service errors
#[derive(Error, Debug)]
pub enum WebhookError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("Webhook not found: {0}")]
    NotFound(i32),

    #[error(
        "Webhook delivery {delivery_id} not found for webhook {webhook_id} in project {project_id}"
    )]
    DeliveryNotInScope {
        project_id: i32,
        webhook_id: i32,
        delivery_id: i32,
    },

    #[error("HTTP request failed: {0}")]
    HttpError(#[from] reqwest::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Invalid configuration: {0}")]
    InvalidConfiguration(String),
}

/// Result of a webhook delivery attempt
#[derive(Debug, Clone)]
pub struct WebhookDeliveryResult {
    pub webhook_id: i32,
    /// Stable identity of the delivery (the `X-Webhook-Delivery` header value),
    /// shared by every attempt of it. `0` when the attempt could not be recorded.
    pub delivery_id: i32,
    /// Id of the history row recording this specific attempt. `0` when the
    /// attempt could not be recorded.
    pub attempt_id: i32,
    pub success: bool,
    pub status_code: Option<u16>,
    // SECURITY: response_body field removed to prevent data exfiltration via SSRF
    pub error_message: Option<String>,
    pub attempt_number: i32,
    pub delivered_at: DateTime<Utc>,
    /// The history row for this attempt as persisted (`None` when recording failed).
    pub record: Option<temps_entities::webhook_deliveries::Model>,
}

/// Where an attempt sits in its delivery: which delivery it belongs to
/// (`None` for a first attempt, which becomes the delivery's identity) and
/// its 1-based attempt number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AttemptSlot {
    delivery_id: Option<i32>,
    attempt_number: i32,
}

impl AttemptSlot {
    fn first() -> Self {
        Self {
            delivery_id: None,
            attempt_number: 1,
        }
    }

    /// The attempt that follows `latest_attempt_number` in delivery `delivery_id`.
    fn next(delivery_id: i32, latest_attempt_number: i32) -> Self {
        Self {
            delivery_id: Some(delivery_id),
            attempt_number: latest_attempt_number.max(0).saturating_add(1),
        }
    }
}

/// Identifying headers sent with every delivery attempt (besides
/// `Content-Type` and the optional signature):
///
/// * `X-Webhook-Event` -- the event name, identical to the subscription name
///   and to the payload's `event_type` (`deployment.succeeded`).
/// * `X-Webhook-Delivery` -- the stable delivery id; identical on every
///   attempt of one delivery, so receivers dedupe on it.
/// * `X-Webhook-Attempt` -- the 1-based attempt number within that delivery.
/// * `X-Webhook-Timestamp` -- unix seconds of *this* attempt (signed).
fn delivery_headers(
    event_type: &str,
    delivery_id: i32,
    attempt_number: i32,
    timestamp: &str,
) -> [(&'static str, String); 4] {
    [
        ("X-Webhook-Event", event_type.to_string()),
        ("X-Webhook-Delivery", delivery_id.to_string()),
        ("X-Webhook-Attempt", attempt_number.to_string()),
        ("X-Webhook-Timestamp", timestamp.to_string()),
    ]
}

/// Request to create a new webhook
#[derive(Debug, Clone)]
pub struct CreateWebhookRequest {
    pub project_id: i32,
    pub url: String,
    pub secret: Option<String>,
    pub events: Vec<WebhookEventType>,
    pub enabled: bool,
}

/// Request to update a webhook
#[derive(Debug, Clone)]
pub struct UpdateWebhookRequest {
    pub url: Option<String>,
    pub secret: Option<String>,
    pub events: Option<Vec<WebhookEventType>>,
    pub enabled: Option<bool>,
}

/// Shared reqwest settings for webhook delivery. Redirects are disabled so a
/// redirect chain cannot bounce a request to an internal target.
fn webhook_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("Temps-Webhook/1.0")
        .redirect(reqwest::redirect::Policy::none())
}

/// Webhook service for managing and delivering webhooks
pub struct WebhookService {
    db: Arc<DatabaseConnection>,
    http_client: reqwest::Client,
    encryption_service: Arc<temps_core::EncryptionService>,
}

impl WebhookService {
    /// Create a new webhook service
    pub fn new(
        db: Arc<DatabaseConnection>,
        encryption_service: Arc<temps_core::EncryptionService>,
    ) -> Self {
        let http_client = webhook_client_builder()
            .build()
            .expect("Failed to create HTTP client");

        Self {
            db,
            http_client,
            encryption_service,
        }
    }

    /// Create a new webhook for a project
    pub async fn create_webhook(
        &self,
        request: CreateWebhookRequest,
    ) -> Result<temps_entities::webhooks::Model, WebhookError> {
        // SECURITY: Validate URL to prevent SSRF attacks
        self.validate_webhook_url(&request.url).await?;

        // Encrypt the secret if provided
        let encrypted_secret = if let Some(secret) = &request.secret {
            Some(
                self.encryption_service
                    .encrypt(secret.as_bytes())
                    .map_err(|e| WebhookError::InvalidConfiguration(e.to_string()))?,
            )
        } else {
            None
        };

        // Serialize events to JSON
        let events_json = serde_json::to_string(&request.events)?;

        let webhook = temps_entities::webhooks::ActiveModel {
            project_id: Set(request.project_id),
            url: Set(request.url),
            secret: Set(encrypted_secret),
            events: Set(events_json),
            enabled: Set(request.enabled),
            ..Default::default()
        };

        let result = webhook.insert(self.db.as_ref()).await?;
        info!(
            "Created webhook {} for project {}",
            result.id, result.project_id
        );
        Ok(result)
    }

    /// Get a webhook by ID
    pub async fn get_webhook(
        &self,
        webhook_id: i32,
    ) -> Result<Option<temps_entities::webhooks::Model>, WebhookError> {
        let webhook = temps_entities::webhooks::Entity::find_by_id(webhook_id)
            .one(self.db.as_ref())
            .await?;
        Ok(webhook)
    }

    /// List all webhooks for a project
    pub async fn list_webhooks(
        &self,
        project_id: i32,
    ) -> Result<Vec<temps_entities::webhooks::Model>, WebhookError> {
        let webhooks = temps_entities::webhooks::Entity::find()
            .filter(temps_entities::webhooks::Column::ProjectId.eq(project_id))
            .order_by_desc(temps_entities::webhooks::Column::CreatedAt)
            .all(self.db.as_ref())
            .await?;
        Ok(webhooks)
    }

    /// List webhooks for a project with pagination
    pub async fn list_webhooks_paginated(
        &self,
        project_id: i32,
        page: u64,
        page_size: u64,
    ) -> Result<Vec<temps_entities::webhooks::Model>, WebhookError> {
        let webhooks = temps_entities::webhooks::Entity::find()
            .filter(temps_entities::webhooks::Column::ProjectId.eq(project_id))
            .order_by_desc(temps_entities::webhooks::Column::CreatedAt)
            .paginate(self.db.as_ref(), page_size)
            .fetch_page(page - 1)
            .await?;
        Ok(webhooks)
    }

    /// Update a webhook
    pub async fn update_webhook(
        &self,
        webhook_id: i32,
        request: UpdateWebhookRequest,
    ) -> Result<Option<temps_entities::webhooks::Model>, WebhookError> {
        let webhook = temps_entities::webhooks::Entity::find_by_id(webhook_id)
            .one(self.db.as_ref())
            .await?;

        let Some(webhook) = webhook else {
            return Ok(None);
        };

        let mut active_model: temps_entities::webhooks::ActiveModel = webhook.into();

        if let Some(url) = request.url {
            // SECURITY: Validate URL to prevent SSRF attacks
            self.validate_webhook_url(&url).await?;
            active_model.url = Set(url);
        }

        if let Some(secret) = request.secret {
            let encrypted = self
                .encryption_service
                .encrypt(secret.as_bytes())
                .map_err(|e| WebhookError::InvalidConfiguration(e.to_string()))?;
            active_model.secret = Set(Some(encrypted));
        }

        if let Some(events) = request.events {
            let events_json = serde_json::to_string(&events)?;
            active_model.events = Set(events_json);
        }

        if let Some(enabled) = request.enabled {
            active_model.enabled = Set(enabled);
        }

        let result = active_model.update(self.db.as_ref()).await?;
        info!("Updated webhook {}", webhook_id);
        Ok(Some(result))
    }

    /// Delete a webhook
    pub async fn delete_webhook(&self, webhook_id: i32) -> Result<bool, WebhookError> {
        let result = temps_entities::webhooks::Entity::delete_by_id(webhook_id)
            .exec(self.db.as_ref())
            .await?;
        Ok(result.rows_affected > 0)
    }

    /// Trigger webhooks for an event
    pub async fn trigger_event(
        &self,
        event: WebhookEvent,
    ) -> Result<Vec<WebhookDeliveryResult>, WebhookError> {
        let project_id = match event.project_id {
            Some(id) => id,
            None => {
                warn!("Cannot trigger webhook without project_id");
                return Ok(vec![]);
            }
        };

        // Find all enabled webhooks for this project that listen to this event type
        let webhooks = temps_entities::webhooks::Entity::find()
            .filter(temps_entities::webhooks::Column::ProjectId.eq(project_id))
            .filter(temps_entities::webhooks::Column::Enabled.eq(true))
            .all(self.db.as_ref())
            .await?;

        let mut results = Vec::new();

        for webhook in webhooks {
            // Check if this webhook is subscribed to this event type
            let events: Vec<WebhookEventType> =
                serde_json::from_str(&webhook.events).unwrap_or_default();
            if !events.contains(&event.event_type) {
                continue;
            }

            // Deliver the webhook
            let result = self.deliver_webhook(&webhook, &event).await;
            results.push(result);
        }

        Ok(results)
    }

    /// Build the HTTP client to use for a single delivery.
    ///
    /// For domain hosts, re-resolve at delivery time, reject any non-public IP,
    /// and pin the client to the validated addresses so reqwest dials exactly
    /// those IPs. This closes the DNS-rebinding window between create-time
    /// validation and delivery (the host cannot re-resolve to 127.0.0.1 /
    /// 169.254.169.254 / RFC1918 at send time). IP-literal hosts involve no DNS,
    /// so the shared client is reused.
    async fn delivery_client_for(&self, url: &str) -> Result<reqwest::Client, WebhookError> {
        let parsed = url::Url::parse(url).map_err(|e| {
            WebhookError::InvalidConfiguration(format!("Invalid webhook URL: {}", e))
        })?;

        match parsed.host() {
            Some(url::Host::Domain(domain)) => {
                let port = parsed.port_or_known_default().unwrap_or(443);
                let addrs = temps_core::url_validation::resolve_and_validate_domain(domain, port)
                    .await
                    .map_err(|e| {
                        WebhookError::InvalidConfiguration(format!(
                            "Webhook URL failed SSRF re-validation at delivery: {}",
                            e
                        ))
                    })?;
                webhook_client_builder()
                    .resolve_to_addrs(domain, &addrs)
                    .build()
                    .map_err(WebhookError::from)
            }
            // IP-literal (or no) host: no DNS resolution happens, so there is no
            // rebinding window. The create-time IP validation still applies.
            _ => Ok(self.http_client.clone()),
        }
    }

    /// Deliver the first attempt of a webhook event to its configured URL.
    async fn deliver_webhook(
        &self,
        webhook: &temps_entities::webhooks::Model,
        event: &WebhookEvent,
    ) -> WebhookDeliveryResult {
        let payload = serde_json::to_string(event).unwrap_or_default();
        self.send_attempt(
            webhook,
            event.event_type.as_str(),
            &event.id,
            payload,
            AttemptSlot::first(),
        )
        .await
    }

    /// Send one attempt of a delivery and record it.
    ///
    /// This is the only place a delivery attempt is sent, so the original send
    /// and every retry share the same recording and header rules: one
    /// `webhook_deliveries` row per attempt, all attempts of a delivery
    /// carrying the same `delivery_id` (the id of the first attempt's row) and
    /// consecutive `attempt_number`s.
    async fn send_attempt(
        &self,
        webhook: &temps_entities::webhooks::Model,
        event_type: &str,
        event_id: &str,
        payload: String,
        slot: AttemptSlot,
    ) -> WebhookDeliveryResult {
        let timestamp = Utc::now().timestamp().to_string();

        // Generate signature if secret is configured
        let signature = if let Some(encrypted_secret) = &webhook.secret {
            match self.encryption_service.decrypt(encrypted_secret) {
                Ok(secret_bytes) => match String::from_utf8(secret_bytes) {
                    Ok(secret) => Some(self.generate_signature(&secret, &timestamp, &payload)),
                    Err(e) => {
                        error!("Failed to convert secret to string: {}", e);
                        None
                    }
                },
                Err(e) => {
                    error!("Failed to decrypt webhook secret: {}", e);
                    None
                }
            }
        } else {
            None
        };

        // Record this attempt. A retry reuses the delivery's identity; a first
        // attempt learns its own identity from the inserted row below.
        let delivery = temps_entities::webhook_deliveries::ActiveModel {
            webhook_id: Set(webhook.id),
            event_type: Set(event_type.to_string()),
            event_id: Set(event_id.to_string()),
            payload: Set(payload.clone()),
            attempt_number: Set(slot.attempt_number),
            delivery_id: Set(slot.delivery_id),
            ..Default::default()
        };

        let delivery_record = match delivery.insert(self.db.as_ref()).await {
            Ok(record) => record,
            Err(e) => {
                error!(
                    "Failed to create delivery record for webhook {} event {} attempt {}: {}",
                    webhook.id, event_id, slot.attempt_number, e
                );
                return WebhookDeliveryResult {
                    webhook_id: webhook.id,
                    delivery_id: slot.delivery_id.unwrap_or(0),
                    attempt_id: 0,
                    success: false,
                    status_code: None,
                    error_message: Some(format!("Failed to create delivery record: {}", e)),
                    attempt_number: slot.attempt_number,
                    delivered_at: Utc::now(),
                    record: None,
                };
            }
        };
        let stable_delivery_id = delivery_record.stable_delivery_id();

        // Build a delivery client pinned to the validated IP (SSRF/rebinding
        // guard). A re-validation failure aborts the send and is recorded as a
        // failed delivery rather than silently connecting to an internal target.
        let (success, status_code, error_message) =
            match self.delivery_client_for(&webhook.url).await {
                Err(e) => (false, None, Some(e.to_string())),
                Ok(http_client) => {
                    // Send HTTP request
                    let mut request_builder = http_client
                        .post(&webhook.url)
                        .header("Content-Type", "application/json");
                    for (name, value) in delivery_headers(
                        event_type,
                        stable_delivery_id,
                        slot.attempt_number,
                        &timestamp,
                    ) {
                        request_builder = request_builder.header(name, value);
                    }

                    if let Some(sig) = &signature {
                        request_builder = request_builder.header("X-Webhook-Signature", sig);
                    }

                    let response = request_builder.body(payload).send().await;

                    match response {
                        Ok(resp) => {
                            let status = resp.status();
                            // SECURITY: Do NOT store response body to prevent data exfiltration via SSRF
                            // Response bodies could contain sensitive data from internal services
                            let is_success = status.is_success();
                            (is_success, Some(status.as_u16()), None)
                        }
                        Err(e) => {
                            let error_msg = Self::format_webhook_error(&e, &webhook.url);
                            (false, None, Some(error_msg))
                        }
                    }
                }
            };

        // Update delivery record with result
        let delivered_at = Utc::now();
        let mut delivery_update: temps_entities::webhook_deliveries::ActiveModel =
            delivery_record.clone().into();
        delivery_update.delivery_id = Set(Some(stable_delivery_id));
        delivery_update.status_code = Set(status_code.map(|s| s as i32));
        // SECURITY: response_body field removed - do not store response data
        delivery_update.error_message = Set(error_message.clone());
        delivery_update.success = Set(success);
        delivery_update.delivered_at = Set(Some(delivered_at));

        let record = match delivery_update.update(self.db.as_ref()).await {
            Ok(updated) => updated,
            Err(e) => {
                error!(
                    "Failed to update delivery record {} (delivery {}, attempt {}): {}",
                    delivery_record.id, stable_delivery_id, slot.attempt_number, e
                );
                let mut fallback = delivery_record.clone();
                fallback.delivery_id = Some(stable_delivery_id);
                fallback.status_code = status_code.map(|s| s as i32);
                fallback.error_message = error_message.clone();
                fallback.success = success;
                fallback.delivered_at = Some(delivered_at);
                fallback
            }
        };

        if success {
            info!(
                "Webhook {} delivered successfully to {} (delivery {}, attempt {})",
                webhook.id, webhook.url, stable_delivery_id, slot.attempt_number
            );
        } else {
            warn!(
                "Webhook {} delivery failed to {} (delivery {}, attempt {}): {:?}",
                webhook.id, webhook.url, stable_delivery_id, slot.attempt_number, error_message
            );
        }

        WebhookDeliveryResult {
            webhook_id: webhook.id,
            delivery_id: stable_delivery_id,
            attempt_id: delivery_record.id,
            success,
            status_code,
            error_message,
            attempt_number: slot.attempt_number,
            delivered_at,
            record: Some(record),
        }
    }

    /// Generate HMAC-SHA256 signature for webhook payload
    fn generate_signature(&self, secret: &str, timestamp: &str, payload: &str) -> String {
        let message = format!("{}.{}", timestamp, payload);
        let mut mac =
            HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC can take key of any size");
        mac.update(message.as_bytes());
        let result = mac.finalize();
        format!("sha256={}", hex::encode(result.into_bytes()))
    }

    /// Format webhook delivery error with detailed, actionable message
    fn format_webhook_error(error: &reqwest::Error, url: &str) -> String {
        if error.is_timeout() {
            return format!(
                "Request timeout after 30 seconds. The endpoint at {} did not respond in time. \
                Check if the service is running and responding quickly enough.",
                url
            );
        }

        if error.is_connect() {
            return format!(
                "Connection failed to {}. The service may not be running or the URL is incorrect. \
                Please verify the endpoint is accessible and listening on the correct port.",
                url
            );
        }

        if let Some(status) = error.status() {
            return format!(
                "HTTP {} error from {}: {}. The endpoint rejected the webhook request.",
                status.as_u16(),
                url,
                status.canonical_reason().unwrap_or("Unknown error")
            );
        }

        if error.is_request() {
            return format!(
                "Failed to send request to {}. The URL may be malformed or the network is unreachable. \
                Original error: {}",
                url,
                error
            );
        }

        if error.is_decode() || error.is_body() {
            return format!(
                "Failed to read response from {}. The endpoint may have sent invalid data. \
                Original error: {}",
                url, error
            );
        }

        // Fallback for any other error types
        format!(
            "Unexpected error delivering webhook to {}: {}. \
            Please check the endpoint configuration and network connectivity.",
            url, error
        )
    }

    /// Get delivery history for a webhook
    pub async fn get_deliveries(
        &self,
        webhook_id: i32,
        limit: u64,
    ) -> Result<Vec<temps_entities::webhook_deliveries::Model>, WebhookError> {
        let deliveries = temps_entities::webhook_deliveries::Entity::find()
            .filter(temps_entities::webhook_deliveries::Column::WebhookId.eq(webhook_id))
            .order_by_desc(temps_entities::webhook_deliveries::Column::CreatedAt)
            .limit(limit)
            .all(self.db.as_ref())
            .await?;
        Ok(deliveries)
    }

    /// Get a specific delivery by ID
    pub async fn get_delivery(
        &self,
        delivery_id: i32,
    ) -> Result<Option<temps_entities::webhook_deliveries::Model>, WebhookError> {
        let delivery = temps_entities::webhook_deliveries::Entity::find_by_id(delivery_id)
            .one(self.db.as_ref())
            .await?;
        Ok(delivery)
    }

    async fn load_retry_scope(
        &self,
        project_id: i32,
        webhook_id: i32,
        delivery_id: i32,
    ) -> Result<
        (
            temps_entities::webhook_deliveries::Model,
            temps_entities::webhooks::Model,
        ),
        WebhookError,
    > {
        let not_in_scope = || WebhookError::DeliveryNotInScope {
            project_id,
            webhook_id,
            delivery_id,
        };

        let webhook = temps_entities::webhooks::Entity::find_by_id(webhook_id)
            .filter(temps_entities::webhooks::Column::ProjectId.eq(project_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(not_in_scope)?;

        let delivery = temps_entities::webhook_deliveries::Entity::find_by_id(delivery_id)
            .filter(temps_entities::webhook_deliveries::Column::WebhookId.eq(webhook_id))
            .one(self.db.as_ref())
            .await?
            .ok_or_else(not_in_scope)?;

        Ok((delivery, webhook))
    }

    /// Highest attempt number recorded so far for the delivery whose stable
    /// identity is `stable_delivery_id` (the id of its first attempt's row).
    /// Rows written before `delivery_id` existed, or whose result update
    /// failed, have no `delivery_id` and are matched by their own id.
    async fn latest_attempt_number(
        &self,
        webhook_id: i32,
        stable_delivery_id: i32,
    ) -> Result<i32, WebhookError> {
        use temps_entities::webhook_deliveries::Column;

        let latest = temps_entities::webhook_deliveries::Entity::find()
            .filter(Column::WebhookId.eq(webhook_id))
            .filter(
                Condition::any()
                    .add(Column::DeliveryId.eq(stable_delivery_id))
                    .add(
                        Condition::all()
                            .add(Column::DeliveryId.is_null())
                            .add(Column::Id.eq(stable_delivery_id)),
                    ),
            )
            .order_by_desc(Column::AttemptNumber)
            .one(self.db.as_ref())
            .await?;
        Ok(latest.map(|row| row.attempt_number).unwrap_or(0))
    }

    /// Retry a delivery after verifying its full project/webhook scope.
    ///
    /// The retry is recorded as the next attempt of the *same* delivery: it
    /// shares the delivery's identity (`X-Webhook-Delivery`) and event id, the
    /// stored payload is re-sent verbatim, and `delivery_id` may be the row id
    /// of any attempt of that delivery. The earlier attempts keep their own
    /// result rows untouched.
    pub async fn retry_delivery(
        &self,
        project_id: i32,
        webhook_id: i32,
        delivery_id: i32,
    ) -> Result<WebhookDeliveryResult, WebhookError> {
        let (delivery, webhook) = self
            .load_retry_scope(project_id, webhook_id, delivery_id)
            .await?;

        let stable_id = delivery.stable_delivery_id();
        let latest = self
            .latest_attempt_number(webhook.id, stable_id)
            .await?
            .max(delivery.attempt_number);
        let slot = AttemptSlot::next(stable_id, latest);

        Ok(self
            .send_attempt(
                &webhook,
                &delivery.event_type,
                &delivery.event_id,
                delivery.payload,
                slot,
            )
            .await)
    }

    /// Decrypt webhook secret for display (masked)
    pub fn decrypt_secret(&self, encrypted_secret: &str) -> Result<String, WebhookError> {
        let secret_bytes = self
            .encryption_service
            .decrypt(encrypted_secret)
            .map_err(|e| WebhookError::InvalidConfiguration(e.to_string()))?;
        String::from_utf8(secret_bytes)
            .map_err(|e| WebhookError::InvalidConfiguration(e.to_string()))
    }

    /// Validate a webhook URL to prevent SSRF attacks
    ///
    /// This method performs comprehensive validation to prevent Server-Side Request Forgery:
    /// - Only allows HTTP and HTTPS schemes
    /// - Blocks private IP ranges (RFC 1918)
    /// - Blocks loopback addresses
    /// - Blocks link-local addresses
    /// - Blocks cloud metadata services (AWS, GCP, Azure, etc.)
    /// - For domains, resolves DNS and validates all resolved IPs
    ///
    /// # Security
    ///
    /// This is a critical security function. Any webhook URL that fails validation
    /// should be rejected to prevent attackers from:
    /// - Accessing internal services (Redis, PostgreSQL, etc.)
    /// - Stealing cloud credentials from metadata services
    /// - Port scanning the internal network
    /// - Exfiltrating data via DNS rebinding attacks
    async fn validate_webhook_url(&self, url: &str) -> Result<(), WebhookError> {
        // Basic URL format and scheme validation
        let parsed = temps_core::url_validation::validate_external_url(url).map_err(|e| {
            WebhookError::InvalidConfiguration(format!("Invalid webhook URL: {}", e))
        })?;

        // For domain names, perform DNS resolution and validate resolved IPs
        if let Some(url::Host::Domain(domain)) = parsed.host() {
            temps_core::url_validation::validate_domain_async(domain)
                .await
                .map_err(|e| {
                    WebhookError::InvalidConfiguration(format!(
                        "Webhook URL domain validation failed: {}",
                        e
                    ))
                })?;
        }

        info!("Validated webhook URL: {}", url);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use sea_orm::{DatabaseBackend, DbErr, MockDatabase};

    fn service_with_db(db: Arc<DatabaseConnection>) -> WebhookService {
        let encryption_service = Arc::new(
            temps_core::EncryptionService::new(
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .unwrap(),
        );
        WebhookService::new(db, encryption_service)
    }

    fn webhook(id: i32, project_id: i32) -> temps_entities::webhooks::Model {
        temps_entities::webhooks::Model {
            id,
            project_id,
            url: "https://example.com/webhook".to_string(),
            secret: None,
            events: "[]".to_string(),
            enabled: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[allow(deprecated)]
    fn delivery(id: i32, webhook_id: i32) -> temps_entities::webhook_deliveries::Model {
        temps_entities::webhook_deliveries::Model {
            id,
            webhook_id,
            event_type: "deployment.succeeded".to_string(),
            event_id: "event-1".to_string(),
            payload: "{}".to_string(),
            success: false,
            status_code: None,
            response_body: None,
            error_message: None,
            attempt_number: 1,
            delivery_id: None,
            created_at: Utc::now(),
            delivered_at: None,
        }
    }

    /// A history row for one attempt of a delivery.
    #[allow(deprecated)]
    fn attempt_row(
        id: i32,
        webhook_id: i32,
        delivery_id: Option<i32>,
        attempt_number: i32,
        payload: &str,
        success: bool,
    ) -> temps_entities::webhook_deliveries::Model {
        temps_entities::webhook_deliveries::Model {
            payload: payload.to_string(),
            attempt_number,
            delivery_id,
            success,
            ..delivery(id, webhook_id)
        }
    }

    /// A request as the test receiver saw it.
    #[derive(Debug)]
    struct SeenRequest {
        headers: std::collections::HashMap<String, String>,
        body: String,
    }

    /// Start a local HTTP receiver that answers its Nth connection with
    /// `statuses[N]` and records every request. Returns its address and a
    /// handle yielding the recorded requests once all were answered.
    async fn spawn_receiver(
        statuses: Vec<u16>,
    ) -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<Vec<SeenRequest>>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let mut seen = Vec::new();
            for status in statuses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 4096];
                let (head_end, content_length) = loop {
                    let read = stream.read(&mut chunk).await.unwrap();
                    assert!(read > 0, "receiver connection closed before headers ended");
                    buffer.extend_from_slice(&chunk[..read]);
                    if let Some(pos) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buffer[..pos]).to_lowercase();
                        let length = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        break (pos + 4, length);
                    }
                };
                while buffer.len() < head_end + content_length {
                    let read = stream.read(&mut chunk).await.unwrap();
                    assert!(read > 0, "receiver connection closed before body ended");
                    buffer.extend_from_slice(&chunk[..read]);
                }
                let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
                let headers = head
                    .lines()
                    .skip(1)
                    .filter_map(|l| l.split_once(':'))
                    .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))
                    .collect();
                let body = String::from_utf8_lossy(&buffer[head_end..head_end + content_length])
                    .to_string();
                seen.push(SeenRequest { headers, body });
                let response = format!(
                    "HTTP/1.1 {status} Test\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                stream.write_all(response.as_bytes()).await.unwrap();
                stream.shutdown().await.ok();
            }
            seen
        });
        (addr, handle)
    }

    fn webhook_at(addr: std::net::SocketAddr) -> temps_entities::webhooks::Model {
        temps_entities::webhooks::Model {
            url: format!("http://{addr}/hook"),
            ..webhook(10, 1)
        }
    }

    fn statements(log: &[sea_orm::Transaction]) -> Vec<String> {
        log.iter()
            .flat_map(|t| t.statements().iter().map(|s| s.sql.clone()))
            .collect()
    }

    #[test]
    fn delivery_headers_carry_stable_delivery_id_and_attempt_number() {
        let first = delivery_headers("deployment.succeeded", 7, 1, "100");
        let retry = delivery_headers("deployment.succeeded", 7, 2, "200");

        let value = |headers: &[(&'static str, String); 4], name: &str| {
            headers
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(value(&first, "X-Webhook-Delivery").as_deref(), Some("7"));
        assert_eq!(
            value(&first, "X-Webhook-Delivery"),
            value(&retry, "X-Webhook-Delivery"),
            "the delivery id must not change between attempts"
        );
        assert_eq!(value(&first, "X-Webhook-Attempt").as_deref(), Some("1"));
        assert_eq!(value(&retry, "X-Webhook-Attempt").as_deref(), Some("2"));
        assert_eq!(
            value(&retry, "X-Webhook-Event").as_deref(),
            Some("deployment.succeeded")
        );
        assert_eq!(value(&retry, "X-Webhook-Timestamp").as_deref(), Some("200"));
    }

    #[test]
    fn attempt_slot_first_starts_a_new_delivery_and_next_continues_it() {
        assert_eq!(
            AttemptSlot::first(),
            AttemptSlot {
                delivery_id: None,
                attempt_number: 1
            }
        );
        assert_eq!(
            AttemptSlot::next(20, 1),
            AttemptSlot {
                delivery_id: Some(20),
                attempt_number: 2
            }
        );
        // A legacy corrupted row may report 0 or a negative count.
        assert_eq!(AttemptSlot::next(20, 0).attempt_number, 1);
        assert_eq!(AttemptSlot::next(20, i32::MAX).attempt_number, i32::MAX);
    }

    #[test]
    fn stable_delivery_id_falls_back_to_the_row_id() {
        assert_eq!(delivery(20, 10).stable_delivery_id(), 20);
        let retry = attempt_row(21, 10, Some(20), 2, "{}", true);
        assert_eq!(retry.stable_delivery_id(), 20);
    }

    #[tokio::test]
    async fn first_attempt_becomes_its_own_delivery_identity() {
        let (addr, received) = spawn_receiver(vec![200]).await;
        let payload = r#"{"id":"event-1"}"#;
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // insert of attempt 1 (no delivery id yet)
                .append_query_results(vec![vec![attempt_row(5, 10, None, 1, payload, false)]])
                // result update records the identity and the outcome
                .append_query_results(vec![vec![attempt_row(5, 10, Some(5), 1, payload, true)]])
                .into_connection(),
        );
        let service = service_with_db(db.clone());

        let result = service
            .send_attempt(
                &webhook_at(addr),
                "deployment.succeeded",
                "event-1",
                payload.to_string(),
                AttemptSlot::first(),
            )
            .await;

        assert!(result.success);
        assert_eq!((result.delivery_id, result.attempt_id), (5, 5));
        assert_eq!(result.attempt_number, 1);
        let seen = received.await.unwrap();
        assert_eq!(seen[0].headers["x-webhook-delivery"], "5");
        assert_eq!(seen[0].headers["x-webhook-attempt"], "1");
        assert_eq!(seen[0].headers["x-webhook-event"], "deployment.succeeded");
        assert_eq!(seen[0].body, payload);

        drop(service);
        let log = Arc::try_unwrap(db).unwrap().into_transaction_log();
        let sql = statements(&log);
        assert_eq!(sql.len(), 2, "insert + result update only: {sql:?}");
        assert!(sql[0].starts_with(r#"INSERT INTO "webhook_deliveries""#));
        assert!(sql[1].starts_with(r#"UPDATE "webhook_deliveries""#));
        assert!(
            format!("{:?}", log[1].statements()[0].values).contains("Int(Some(5))"),
            "the update must store delivery_id = the first attempt's own id"
        );
    }

    #[tokio::test]
    async fn manual_retry_is_the_next_attempt_of_the_same_delivery() {
        let (addr, received) = spawn_receiver(vec![200]).await;
        let payload = r#"{"id":"event-1","event_type":"deployment.succeeded"}"#;
        let failed_first_attempt = attempt_row(20, 10, Some(20), 1, payload, false);
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![webhook_at(addr)]])
                // the delivery being retried: attempt 1 of delivery 20, failed
                .append_query_results(vec![vec![failed_first_attempt.clone()]])
                // latest recorded attempt of delivery 20
                .append_query_results(vec![vec![failed_first_attempt]])
                // insert of the retry row, then its result update
                .append_query_results(vec![vec![attempt_row(21, 10, Some(20), 2, payload, false)]])
                .append_query_results(vec![vec![attempt_row(21, 10, Some(20), 2, payload, true)]])
                .into_connection(),
        );
        let service = service_with_db(db.clone());

        let result = service.retry_delivery(1, 10, 20).await.unwrap();

        assert!(result.success);
        assert_eq!(result.delivery_id, 20, "same delivery identity");
        assert_eq!(result.attempt_id, 21, "new history row for the new attempt");
        assert_eq!(result.attempt_number, 2);
        let record = result.record.expect("persisted attempt row");
        assert_eq!(record.stable_delivery_id(), 20);
        assert_eq!(record.attempt_number, 2);

        let seen = received.await.unwrap();
        assert_eq!(seen[0].headers["x-webhook-delivery"], "20");
        assert_eq!(seen[0].headers["x-webhook-attempt"], "2");
        assert_eq!(seen[0].body, payload, "stored payload is re-sent verbatim");

        drop(service);
        let log = Arc::try_unwrap(db).unwrap().into_transaction_log();
        let sql = statements(&log);
        let updates = sql
            .iter()
            .filter(|s| s.starts_with(r#"UPDATE "webhook_deliveries""#))
            .count();
        assert_eq!(
            updates, 1,
            "only the new attempt row is written; the failed attempt keeps its result: {sql:?}"
        );
        let update_values = format!("{:?}", log.last().unwrap().statements()[0].values);
        assert!(
            update_values.contains("Int(Some(21))"),
            "the update targets the retry row, not the original: {update_values}"
        );
        let insert_values = format!("{:?}", log[3].statements()[0].values);
        assert!(
            insert_values.contains("Int(Some(2))") && insert_values.contains("Int(Some(20))"),
            "insert carries attempt 2 and delivery 20: {insert_values}"
        );
    }

    #[tokio::test]
    async fn failed_send_then_manual_retry_share_identity_and_event_id() {
        let (addr, received) = spawn_receiver(vec![500, 200]).await;
        let payload = r#"{"id":"event-9","event_type":"deployment.succeeded"}"#;
        let failed_first_attempt = attempt_row(30, 10, Some(30), 1, payload, false);
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                // --- original send (attempt 1) ---
                .append_query_results(vec![vec![attempt_row(30, 10, None, 1, payload, false)]])
                .append_query_results(vec![vec![failed_first_attempt.clone()]])
                // --- manual retry ---
                .append_query_results(vec![vec![webhook_at(addr)]])
                .append_query_results(vec![vec![failed_first_attempt.clone()]])
                .append_query_results(vec![vec![failed_first_attempt]])
                .append_query_results(vec![vec![attempt_row(31, 10, Some(30), 2, payload, false)]])
                .append_query_results(vec![vec![attempt_row(31, 10, Some(30), 2, payload, true)]])
                .into_connection(),
        );
        let service = service_with_db(db);
        let webhook = webhook_at(addr);

        let first = service
            .send_attempt(
                &webhook,
                "deployment.succeeded",
                "event-9",
                payload.to_string(),
                AttemptSlot::first(),
            )
            .await;
        assert!(!first.success);
        assert_eq!(first.status_code, Some(500));
        assert_eq!((first.delivery_id, first.attempt_number), (30, 1));

        let retried = service
            .retry_delivery(1, 10, first.attempt_id)
            .await
            .unwrap();
        assert!(retried.success);
        assert_eq!(retried.delivery_id, first.delivery_id);
        assert_eq!(retried.attempt_number, 2);
        assert_ne!(retried.attempt_id, first.attempt_id);

        let seen = received.await.unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(
            seen[0].headers["x-webhook-delivery"], seen[1].headers["x-webhook-delivery"],
            "X-Webhook-Delivery is stable across attempts"
        );
        assert_eq!(seen[0].headers["x-webhook-attempt"], "1");
        assert_eq!(seen[1].headers["x-webhook-attempt"], "2");
        assert_eq!(seen[0].body, seen[1].body, "same payload, same event id");
    }

    #[test]
    fn test_signature_generation() {
        let _encryption_service = Arc::new(
            temps_core::EncryptionService::new(
                "0000000000000000000000000000000000000000000000000000000000000000",
            )
            .unwrap(),
        );

        // We can't easily test the full service without a database,
        // but we can test the signature generation logic
        let secret = "test_secret";
        let timestamp = "1234567890";
        let payload = r#"{"test":"data"}"#;

        let message = format!("{}.{}", timestamp, payload);
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(message.as_bytes());
        let result = mac.finalize();
        let signature = format!("sha256={}", hex::encode(result.into_bytes()));

        assert!(signature.starts_with("sha256="));
        assert_eq!(signature.len(), 71); // "sha256=" (7) + 64 hex chars
    }

    #[tokio::test]
    async fn retry_scope_loads_delivery_only_from_requested_project_and_webhook() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![webhook(10, 1)]])
                .append_query_results(vec![vec![delivery(20, 10)]])
                .into_connection(),
        );
        let service = service_with_db(db.clone());

        let (found_delivery, found_webhook) = service.load_retry_scope(1, 10, 20).await.unwrap();

        assert_eq!(found_webhook.id, 10);
        assert_eq!(found_webhook.project_id, 1);
        assert_eq!(found_delivery.id, 20);
        assert_eq!(found_delivery.webhook_id, 10);

        drop(service);
        let log = Arc::try_unwrap(db).unwrap().into_transaction_log();
        assert_eq!(log.len(), 2);
        assert!(log[0].statements()[0]
            .sql
            .contains(r#""webhooks"."project_id" = $2"#));
        assert!(log[1].statements()[0]
            .sql
            .contains(r#""webhook_deliveries"."webhook_id" = $2"#));
    }

    #[tokio::test]
    async fn retry_scope_rejects_cross_project_webhook() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![Vec::<temps_entities::webhooks::Model>::new()])
                .into_connection(),
        );
        let service = service_with_db(db.clone());

        let error = service.retry_delivery(1, 10, 20).await.unwrap_err();

        assert!(matches!(
            error,
            WebhookError::DeliveryNotInScope {
                project_id: 1,
                webhook_id: 10,
                delivery_id: 20
            }
        ));

        drop(service);
        let log = Arc::try_unwrap(db).unwrap().into_transaction_log();
        assert_eq!(
            log.len(),
            1,
            "retry must stop before loading or sending a delivery"
        );
        let statement = &log[0].statements()[0];
        assert!(statement.sql.contains(r#""webhooks"."id" = $1"#));
        assert!(statement.sql.contains(r#""webhooks"."project_id" = $2"#));
        assert_eq!(
            format!("{:?}", statement.values),
            "Some(Values([Int(Some(10)), Int(Some(1)), BigUnsigned(Some(1))]))"
        );
    }

    #[tokio::test]
    async fn retry_scope_rejects_delivery_from_another_webhook() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![webhook(10, 1)]])
                .append_query_results(vec![Vec::<temps_entities::webhook_deliveries::Model>::new()])
                .into_connection(),
        );
        let service = service_with_db(db.clone());

        let error = service.retry_delivery(1, 10, 20).await.unwrap_err();

        assert!(matches!(
            error,
            WebhookError::DeliveryNotInScope {
                project_id: 1,
                webhook_id: 10,
                delivery_id: 20
            }
        ));

        drop(service);
        let log = Arc::try_unwrap(db).unwrap().into_transaction_log();
        assert_eq!(
            log.len(),
            2,
            "retry must stop before parsing or sending the payload"
        );
        let statement = &log[1].statements()[0];
        assert!(statement.sql.contains(r#""webhook_deliveries"."id" = $1"#));
        assert!(statement
            .sql
            .contains(r#""webhook_deliveries"."webhook_id" = $2"#));
        assert_eq!(
            format!("{:?}", statement.values),
            "Some(Values([Int(Some(20)), Int(Some(10)), BigUnsigned(Some(1))]))"
        );
    }

    #[tokio::test]
    async fn retry_scope_preserves_database_errors() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_errors(vec![DbErr::Custom("webhook lookup failed".to_string())])
                .into_connection(),
        );
        let service = service_with_db(db);

        let error = service.load_retry_scope(1, 10, 20).await.unwrap_err();

        assert!(
            matches!(error, WebhookError::Database(DbErr::Custom(message)) if message == "webhook lookup failed")
        );
    }

    // Note: Testing format_webhook_error requires constructing reqwest::Error instances,
    // which is complex as the error types are not directly constructible.
    // The error formatting logic has been manually verified for:
    // - Connection errors (is_connect)
    // - Timeout errors (is_timeout)
    // - HTTP status errors (status)
    // - Request errors (is_request)
    // - Decode/body errors (is_decode/is_body)
}
