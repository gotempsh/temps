// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Registering, changing and removing a page's webhooks (ADR 108 D2).

use std::sync::Arc;

use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect, Set, TransactionTrait,
};
use temps_core::EncryptionService;
use temps_entities::{status_page_webhooks, status_pages};
use tracing::info;

use super::error::WebhookError;
use super::transport::WebhookTransport;
use super::types::{
    CreateWebhookRequest, UpdateWebhookRequest, WebhookCreatedResponse, WebhookListResponse,
    WebhookResponse,
};
use crate::services::public_pages::StatusSnapshotCache;

pub const MAX_WEBHOOKS_PER_PAGE: u64 = 10;
pub const MAX_URL_LEN: usize = 2048;

pub struct StatusPageWebhookService {
    pub(crate) db: Arc<DatabaseConnection>,
    pub(crate) cache: Arc<StatusSnapshotCache>,
    pub(crate) encryption: Arc<EncryptionService>,
    pub(crate) transport: Arc<dyn WebhookTransport>,
}

/// Shape rules for a webhook URL; the address itself is checked by the
/// transport.
pub fn validate_url(status_page_id: i32, raw: &str) -> Result<String, WebhookError> {
    let invalid = |reason: &str| WebhookError::Validation {
        status_page_id,
        reason: reason.to_string(),
    };
    let url = raw.trim();
    if url.is_empty() {
        return Err(invalid("the URL is empty"));
    }
    if url.len() > MAX_URL_LEN {
        return Err(invalid("the URL is longer than 2048 characters"));
    }
    let parsed = reqwest::Url::parse(url).map_err(|_| invalid("the URL is not a valid URL"))?;
    if parsed.scheme() != "https" {
        return Err(invalid(
            "the URL must use https://, so events and signatures are not sent in the clear",
        ));
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err(invalid("the URL has no host"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(invalid(
            "the URL cannot carry a user name or password; verify the signature instead",
        ));
    }
    if parsed.fragment().is_some() {
        return Err(invalid("the URL cannot have a #fragment"));
    }
    Ok(parsed.to_string())
}

/// 256 random bits from the operating system's CSPRNG.
fn new_secret() -> Result<String, WebhookError> {
    crate::services::subscribers::tokens::random_hex()
        .map(|hex| format!("whsec_{hex}"))
        .map_err(|e| WebhookError::SecretGeneration {
            reason: e.to_string(),
        })
}

impl From<status_page_webhooks::Model> for WebhookResponse {
    fn from(model: status_page_webhooks::Model) -> Self {
        Self {
            id: model.id,
            status_page_id: model.status_page_id,
            url: model.url,
            enabled: model.enabled,
            has_secret: !model.secret_encrypted.is_empty(),
            last_attempt_at: model.last_attempt_at,
            last_success_at: model.last_success_at,
            last_failure_reason: model.last_failure_reason,
            created_at: model.created_at,
            updated_at: model.updated_at,
        }
    }
}

impl StatusPageWebhookService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        cache: Arc<StatusSnapshotCache>,
        encryption: Arc<EncryptionService>,
        transport: Arc<dyn WebhookTransport>,
    ) -> Self {
        Self {
            db,
            cache,
            encryption,
            transport,
        }
    }

    async fn ensure_page(&self, status_page_id: i32) -> Result<(), WebhookError> {
        status_pages::Entity::find_by_id(status_page_id)
            .select_only()
            .column(status_pages::Column::Id)
            .into_tuple::<i32>()
            .one(self.db.as_ref())
            .await?
            .map(|_| ())
            .ok_or(WebhookError::StatusPageNotFound { status_page_id })
    }

    async fn find(
        &self,
        status_page_id: i32,
        webhook_id: i32,
    ) -> Result<status_page_webhooks::Model, WebhookError> {
        status_page_webhooks::Entity::find_by_id(webhook_id)
            .filter(status_page_webhooks::Column::StatusPageId.eq(status_page_id))
            .one(self.db.as_ref())
            .await?
            .ok_or(WebhookError::NotFound {
                status_page_id,
                webhook_id,
            })
    }

    async fn check_address(&self, status_page_id: i32, url: &str) -> Result<(), WebhookError> {
        self.transport
            .check(url)
            .await
            .map_err(|reason| WebhookError::Validation {
                status_page_id,
                reason: format!("webhooks cannot be sent to {url}: {reason}"),
            })
    }

    pub async fn list(
        &self,
        status_page_id: i32,
        page: u64,
        page_size: u64,
    ) -> Result<WebhookListResponse, WebhookError> {
        let page = page.max(1);
        let page_size = page_size.clamp(1, 100);
        self.ensure_page(status_page_id).await?;
        let paginator = status_page_webhooks::Entity::find()
            .filter(status_page_webhooks::Column::StatusPageId.eq(status_page_id))
            .order_by_desc(status_page_webhooks::Column::CreatedAt)
            .order_by_desc(status_page_webhooks::Column::Id)
            .paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let items = paginator
            .fetch_page(page - 1)
            .await?
            .into_iter()
            .map(WebhookResponse::from)
            .collect();
        Ok(WebhookListResponse {
            items,
            total,
            page,
            page_size,
        })
    }

    pub async fn create(
        &self,
        status_page_id: i32,
        request: CreateWebhookRequest,
    ) -> Result<WebhookCreatedResponse, WebhookError> {
        let url = validate_url(status_page_id, &request.url)?;
        self.ensure_page(status_page_id).await?;
        self.check_address(status_page_id, &url).await?;
        let secret = new_secret()?;
        let secret_encrypted =
            self.encryption
                .encrypt_string(&secret)
                .map_err(|e| WebhookError::Secret {
                    webhook_id: 0,
                    reason: e.to_string(),
                })?;

        let txn = self.db.begin().await?;
        // Locking the page row serializes creates against the limit.
        status_pages::Entity::find_by_id(status_page_id)
            .select_only()
            .column(status_pages::Column::Id)
            .lock_exclusive()
            .into_tuple::<i32>()
            .one(&txn)
            .await?
            .ok_or(WebhookError::StatusPageNotFound { status_page_id })?;
        let existing = status_page_webhooks::Entity::find()
            .filter(status_page_webhooks::Column::StatusPageId.eq(status_page_id))
            .count(&txn)
            .await?;
        if existing >= MAX_WEBHOOKS_PER_PAGE {
            return Err(WebhookError::TooMany {
                status_page_id,
                limit: MAX_WEBHOOKS_PER_PAGE,
            });
        }
        let model = status_page_webhooks::ActiveModel {
            status_page_id: Set(status_page_id),
            url: Set(url),
            secret_encrypted: Set(secret_encrypted),
            enabled: Set(true),
            ..Default::default()
        }
        .insert(&txn)
        .await?;
        txn.commit().await?;
        info!(
            status_page_id,
            webhook_id = model.id,
            "registered status page webhook"
        );
        Ok(WebhookCreatedResponse {
            webhook: model.into(),
            secret,
        })
    }

    pub async fn update(
        &self,
        status_page_id: i32,
        webhook_id: i32,
        request: UpdateWebhookRequest,
    ) -> Result<WebhookResponse, WebhookError> {
        let model = self.find(status_page_id, webhook_id).await?;
        let url = match request.url.as_deref() {
            Some(raw) => {
                let url = validate_url(status_page_id, raw)?;
                self.check_address(status_page_id, &url).await?;
                Some(url)
            }
            None => None,
        };
        let mut active: status_page_webhooks::ActiveModel = model.into();
        if let Some(url) = url {
            active.url = Set(url);
            active.last_failure_reason = Set(None);
        }
        if let Some(enabled) = request.enabled {
            active.enabled = Set(enabled);
        }
        let model = active.update(self.db.as_ref()).await?;
        info!(
            status_page_id,
            webhook_id,
            enabled = model.enabled,
            "updated status page webhook"
        );
        Ok(model.into())
    }

    pub async fn delete(&self, status_page_id: i32, webhook_id: i32) -> Result<(), WebhookError> {
        let deleted = status_page_webhooks::Entity::delete_many()
            .filter(status_page_webhooks::Column::Id.eq(webhook_id))
            .filter(status_page_webhooks::Column::StatusPageId.eq(status_page_id))
            .exec(self.db.as_ref())
            .await?;
        if deleted.rows_affected == 0 {
            return Err(WebhookError::NotFound {
                status_page_id,
                webhook_id,
            });
        }
        info!(status_page_id, webhook_id, "deleted status page webhook");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_https_urls_are_accepted() {
        assert_eq!(
            validate_url(3, " https://hooks.example.test/status ").expect("ok"),
            "https://hooks.example.test/status"
        );
        for (url, reason) in [
            ("", "empty"),
            ("http://hooks.example.test/", "https://"),
            ("ftp://hooks.example.test/", "https://"),
            ("https://user:pw@hooks.example.test/", "user name"),
            ("https://hooks.example.test/#x", "fragment"),
            ("not a url", "not a valid URL"),
        ] {
            let err = validate_url(3, url).expect_err(url);
            assert!(err.to_string().contains(reason), "{url}: {err}");
            assert!(err.to_string().contains("status page 3"), "{err}");
        }
        let long = format!("https://hooks.example.test/{}", "a".repeat(2100));
        assert!(validate_url(3, &long).is_err());
    }

    #[test]
    fn secrets_are_random_and_prefixed() {
        let one = new_secret().expect("secret");
        assert!(one.starts_with("whsec_") && one.len() == 6 + 64);
        assert_ne!(one, new_secret().expect("secret"));
    }
}
