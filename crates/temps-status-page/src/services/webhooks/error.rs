// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Errors of status page webhooks (ADR 108 D2).

use thiserror::Error;

#[derive(Debug, Error)]
pub enum WebhookError {
    #[error("database error in status page webhooks: {0}")]
    Database(#[from] sea_orm::DbErr),
    #[error("status page {status_page_id} not found")]
    StatusPageNotFound { status_page_id: i32 },
    #[error("webhook {webhook_id} not found on status page {status_page_id}")]
    NotFound {
        status_page_id: i32,
        webhook_id: i32,
    },
    #[error("invalid webhook for status page {status_page_id}: {reason}")]
    Validation { status_page_id: i32, reason: String },
    #[error("status page {status_page_id} already has {limit} webhooks; delete one first")]
    TooMany { status_page_id: i32, limit: u64 },
    #[error("could not generate a webhook secret from the system random source: {reason}")]
    SecretGeneration { reason: String },
    #[error("could not encrypt or decrypt the secret of webhook {webhook_id}: {reason}")]
    Secret { webhook_id: i32, reason: String },
}
