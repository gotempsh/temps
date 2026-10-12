// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Webhook service for managing webhooks and delivering events.

use crate::events::{WebhookEvent, WebhookEventType};
use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait,
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
    pub delivery_id: i32,
    pub success: bool,
    pub status_code: Option<u16>,
    // SECURITY: response_body field removed to prevent data exfiltration via SSRF
    pub error_message: Option<String>,
    pub attempt_number: i32,
    pub delivered_at: DateTime<Utc>,
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

    /// Deliver a webhook to its configured URL as a brand-new delivery
    /// (attempt 1). Re-sending an existing delivery goes through
    /// [`Self::retry_delivery`], which reuses its record.
    async fn deliver_webhook(
        &self,
        webhook: &temps_entities::webhooks::Model,
        event: &WebhookEvent,
    ) -> WebhookDeliveryResult {
        let payload = serde_json::to_string(event).unwrap_or_default();

        // Create delivery record
        let delivery = temps_entities::webhook_deliveries::ActiveModel {
            webhook_id: Set(webhook.id),
            event_type: Set(event.event_type.to_string()),
            event_id: Set(event.id.clone()),
            payload: Set(payload.clone()),
            attempt_number: Set(1),
            ..Default::default()
        };

        let delivery_record = match delivery.insert(self.db.as_ref()).await {
            Ok(record) => record,
            Err(e) => {
                error!("Failed to create delivery record: {}", e);
                return WebhookDeliveryResult {
                    webhook_id: webhook.id,
                    delivery_id: 0,
                    success: false,
                    status_code: None,
                    error_message: Some(format!("Failed to create delivery record: {}", e)),
                    attempt_number: 1,
                    delivered_at: Utc::now(),
                };
            }
        };

        self.send_delivery(webhook, event.event_type.as_str(), payload, delivery_record)
            .await
    }

    /// Send `payload` for an existing delivery record and write the outcome
    /// (status code, error, success, delivered_at) back to that same record.
    ///
    /// The record's `id` is what the receiver sees in `X-Webhook-Delivery`, and
    /// its `attempt_number` is what the result reports, so a retry that reuses
    /// the record is the same delivery, one attempt later.
    async fn send_delivery(
        &self,
        webhook: &temps_entities::webhooks::Model,
        event_type: &str,
        payload: String,
        delivery_record: temps_entities::webhook_deliveries::Model,
    ) -> WebhookDeliveryResult {
        let timestamp = Utc::now().timestamp().to_string();
        let attempt_number = delivery_record.attempt_number;

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
                        .header("Content-Type", "application/json")
                        .header("X-Webhook-Event", event_type)
                        .header("X-Webhook-Delivery", &delivery_record.id.to_string())
                        .header("X-Webhook-Timestamp", &timestamp);

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

        // Update delivery record with result. Only the attempt that owns the
        // row's current attempt number may write its outcome: if a newer retry
        // already incremented it, this (older, slower) attempt must not
        // overwrite the newer outcome.
        let delivery_id = delivery_record.id;
        // SECURITY: response_body is never written - do not store response data
        let write = temps_entities::webhook_deliveries::Entity::update_many()
            .col_expr(
                temps_entities::webhook_deliveries::Column::StatusCode,
                sea_orm::sea_query::Expr::value(status_code.map(i32::from)),
            )
            .col_expr(
                temps_entities::webhook_deliveries::Column::ErrorMessage,
                sea_orm::sea_query::Expr::value(error_message.clone()),
            )
            .col_expr(
                temps_entities::webhook_deliveries::Column::Success,
                sea_orm::sea_query::Expr::value(success),
            )
            .col_expr(
                temps_entities::webhook_deliveries::Column::DeliveredAt,
                sea_orm::sea_query::Expr::value(Some(Utc::now())),
            )
            .filter(temps_entities::webhook_deliveries::Column::Id.eq(delivery_id))
            .filter(temps_entities::webhook_deliveries::Column::AttemptNumber.eq(attempt_number))
            .exec(self.db.as_ref())
            .await;

        if let Err(e) = write {
            error!(
                "Failed to update webhook delivery {} (webhook {}): {}",
                delivery_id, webhook.id, e
            );
        }

        if success {
            info!(
                "Webhook {} delivered successfully to {} (delivery {}, attempt {})",
                webhook.id, webhook.url, delivery_id, attempt_number
            );
        } else {
            warn!(
                "Webhook {} delivery {} (attempt {}) failed to {}: {:?}",
                webhook.id, delivery_id, attempt_number, webhook.url, error_message
            );
        }

        WebhookDeliveryResult {
            webhook_id: webhook.id,
            delivery_id,
            success,
            status_code,
            error_message,
            attempt_number,
            delivered_at: Utc::now(),
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

    /// Retry a failed delivery after verifying its full project/webhook scope.
    pub async fn retry_delivery(
        &self,
        project_id: i32,
        webhook_id: i32,
        delivery_id: i32,
    ) -> Result<WebhookDeliveryResult, WebhookError> {
        let (delivery, webhook) = self
            .load_retry_scope(project_id, webhook_id, delivery_id)
            .await?;

        // The stored payload is replayed byte-for-byte, so the receiver gets the
        // same body (and event id) as the failed attempt. Parsing it validates
        // it and yields the event type for the `X-Webhook-Event` header.
        let event: WebhookEvent = serde_json::from_str(&delivery.payload)?;

        // A retry is another attempt at the SAME delivery: keep its record (and
        // therefore its `X-Webhook-Delivery` id) and count the attempt on it.
        // The increment happens in the database and returns the new count, so
        // two overlapping retries get distinct attempt numbers instead of both
        // reading N and writing N + 1. Recording it before sending also keeps
        // the history right even if the send outcome cannot be written back.
        let payload = delivery.payload.clone();
        let incremented = temps_entities::webhook_deliveries::Entity::update_many()
            .col_expr(
                temps_entities::webhook_deliveries::Column::AttemptNumber,
                sea_orm::sea_query::Expr::col(
                    temps_entities::webhook_deliveries::Column::AttemptNumber,
                )
                .add(1),
            )
            .filter(temps_entities::webhook_deliveries::Column::Id.eq(delivery_id))
            .filter(temps_entities::webhook_deliveries::Column::WebhookId.eq(webhook_id))
            .exec_with_returning(self.db.as_ref())
            .await?;
        let delivery = incremented
            .into_iter()
            .next()
            .ok_or(WebhookError::DeliveryNotInScope {
                project_id,
                webhook_id,
                delivery_id,
            })?;

        Ok(self
            .send_delivery(&webhook, event.event_type.as_str(), payload, delivery)
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
    use sea_orm::{DatabaseBackend, DbErr, MockDatabase, MockExecResult};

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
            created_at: Utc::now(),
            delivered_at: None,
        }
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

    /// One-shot HTTP receiver on a loopback port. Replies `200` to every
    /// request and reports the lower-cased `x-webhook-delivery` and
    /// `x-webhook-event` header values it saw.
    async fn spawn_receiver() -> (
        String,
        tokio::sync::mpsc::UnboundedReceiver<(Option<String>, Option<String>)>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/hook", listener.local_addr().unwrap());
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut buffer = vec![0u8; 8192];
                let mut read = 0;
                // Read until the end of the header block; the body is ignored.
                while !buffer[..read].windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buffer[read..]).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => read += n,
                    }
                }
                let head = String::from_utf8_lossy(&buffer[..read]).to_lowercase();
                let header = |name: &str| {
                    head.lines()
                        .find_map(|line| line.strip_prefix(name))
                        .map(|value| value.trim().to_string())
                };
                let _ = tx.send((header("x-webhook-delivery:"), header("x-webhook-event:")));
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await;
            }
        });
        (url, rx)
    }

    /// A manual retry is a second attempt at the SAME delivery: the failed
    /// delivery's record is reused (no new row, same `X-Webhook-Delivery`),
    /// its attempt number goes from 1 to 2, and the result reports attempt 2.
    /// Before the fix the retry inserted a brand-new delivery (attempt 1, new
    /// id) and rewrote the failed delivery's attempt count instead.
    #[tokio::test]
    async fn retry_reuses_the_failed_delivery_and_counts_the_attempt_on_it() {
        let (url, mut received) = spawn_receiver().await;

        let mut target = webhook(10, 1);
        target.url = url;
        let event = WebhookEvent::new(
            WebhookEventType::DeploymentSucceeded,
            Some(1),
            crate::events::WebhookPayload::Project(crate::events::ProjectPayload {
                project_id: 1,
                project_name: "demo".to_string(),
                slug: "demo".to_string(),
                repo_url: None,
            }),
        );
        let mut failed = delivery(20, 10);
        failed.payload = serde_json::to_string(&event).unwrap();
        failed.event_id = event.id.clone();
        let mut after_attempt_bump = failed.clone();
        after_attempt_bump.attempt_number = 2;

        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results(vec![vec![target]])
                .append_query_results(vec![vec![failed]])
                // `UPDATE … SET attempt_number = attempt_number + 1 RETURNING *`
                .append_query_results(vec![vec![after_attempt_bump]])
                // the outcome write
                .append_exec_results(vec![MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                }])
                .into_connection(),
        );
        let service = service_with_db(db.clone());

        let result = service.retry_delivery(1, 10, 20).await.unwrap();

        assert!(result.success);
        assert_eq!(result.status_code, Some(200));
        assert_eq!(
            result.delivery_id, 20,
            "the retry must keep the delivery id"
        );
        assert_eq!(result.attempt_number, 2);

        let (delivery_header, event_header) = received.recv().await.unwrap();
        assert_eq!(delivery_header.as_deref(), Some("20"));
        assert_eq!(event_header.as_deref(), Some("deployment.succeeded"));

        drop(service);
        let log = Arc::try_unwrap(db).unwrap().into_transaction_log();
        let statements: Vec<String> = log
            .iter()
            .flat_map(|transaction| transaction.statements().iter().map(|s| s.sql.clone()))
            .collect();
        assert!(
            statements.iter().all(|sql| !sql.starts_with("INSERT")),
            "a retry must not record a new delivery: {statements:?}"
        );
        let updates: Vec<&String> = statements
            .iter()
            .filter(|sql| sql.starts_with("UPDATE"))
            .collect();
        assert_eq!(updates.len(), 2, "attempt bump + outcome: {statements:?}");
        // The count is incremented by the database, not computed from a read,
        // so overlapping retries cannot both write the same number.
        assert!(
            updates[0].contains("\"attempt_number\" = \"attempt_number\" + "),
            "{}",
            updates[0]
        );
        // The outcome is only written by the attempt that owns the current count.
        assert!(
            updates[1].contains("\"attempt_number\" ="),
            "{}",
            updates[1]
        );
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
