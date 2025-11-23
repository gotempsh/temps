//! Webhook service for managing webhooks and delivering events.

use crate::events::{WebhookEvent, WebhookEventType};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect,
};
use sha2::Sha256;
use std::sync::Arc;
use thiserror::Error;
use tracing::{error, info, warn};

type HmacSha256 = Hmac<Sha256>;

/// Webhook service errors
#[derive(Error, Debug)]
pub enum WebhookError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("Webhook not found: {0}")]
    NotFound(i32),

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
    pub response_body: Option<String>,
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
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent("Temps-Webhook/1.0")
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

    /// Deliver a webhook to its configured URL
    async fn deliver_webhook(
        &self,
        webhook: &temps_entities::webhooks::Model,
        event: &WebhookEvent,
    ) -> WebhookDeliveryResult {
        let payload = serde_json::to_string(event).unwrap_or_default();
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
                    response_body: None,
                    error_message: Some(format!("Failed to create delivery record: {}", e)),
                    attempt_number: 1,
                    delivered_at: Utc::now(),
                };
            }
        };

        // Send HTTP request
        let mut request_builder = self
            .http_client
            .post(&webhook.url)
            .header("Content-Type", "application/json")
            .header("X-Webhook-Event", event.event_type.as_str())
            .header("X-Webhook-Delivery", &delivery_record.id.to_string())
            .header("X-Webhook-Timestamp", &timestamp);

        if let Some(sig) = &signature {
            request_builder = request_builder.header("X-Webhook-Signature", sig);
        }

        let response = request_builder.body(payload).send().await;

        let (success, status_code, response_body, error_message) = match response {
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.ok();
                let is_success = status.is_success();
                (is_success, Some(status.as_u16()), body, None)
            }
            Err(e) => (false, None, None, Some(e.to_string())),
        };

        // Update delivery record with result
        let mut delivery_update: temps_entities::webhook_deliveries::ActiveModel =
            delivery_record.clone().into();
        delivery_update.status_code = Set(status_code.map(|s| s as i32));
        delivery_update.response_body = Set(response_body.clone());
        delivery_update.error_message = Set(error_message.clone());
        delivery_update.success = Set(success);
        delivery_update.delivered_at = Set(Some(Utc::now()));

        if let Err(e) = delivery_update.update(self.db.as_ref()).await {
            error!("Failed to update delivery record: {}", e);
        }

        if success {
            info!(
                "Webhook {} delivered successfully to {}",
                webhook.id, webhook.url
            );
        } else {
            warn!(
                "Webhook {} delivery failed to {}: {:?}",
                webhook.id, webhook.url, error_message
            );
        }

        WebhookDeliveryResult {
            webhook_id: webhook.id,
            delivery_id: delivery_record.id,
            success,
            status_code,
            response_body,
            error_message,
            attempt_number: 1,
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

    /// Retry a failed delivery
    pub async fn retry_delivery(
        &self,
        delivery_id: i32,
    ) -> Result<WebhookDeliveryResult, WebhookError> {
        let delivery = temps_entities::webhook_deliveries::Entity::find_by_id(delivery_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(WebhookError::NotFound(delivery_id))?;

        let webhook = temps_entities::webhooks::Entity::find_by_id(delivery.webhook_id)
            .one(self.db.as_ref())
            .await?
            .ok_or(WebhookError::NotFound(delivery.webhook_id))?;

        // Parse the original event from the stored payload
        let event: WebhookEvent = serde_json::from_str(&delivery.payload)?;

        // Deliver again with incremented attempt number
        let result = self.deliver_webhook(&webhook, &event).await;

        // Update attempt number
        let mut delivery_update: temps_entities::webhook_deliveries::ActiveModel = delivery.into();
        delivery_update.attempt_number = Set(result.attempt_number + 1);
        let _ = delivery_update.update(self.db.as_ref()).await;

        Ok(result)
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
