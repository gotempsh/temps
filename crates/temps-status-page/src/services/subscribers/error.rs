// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Errors of status page subscriptions (ADR 108 D2).

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SubscriptionError {
    #[error("database error in status page subscriptions: {0}")]
    Database(#[from] sea_orm::DbErr),
    #[error("could not generate a subscription token from the system random source: {reason}")]
    TokenGeneration { reason: String },
    #[error("status page {status_page_id} not found")]
    StatusPageNotFound { status_page_id: i32 },
    #[error("subscriber {subscriber_id} not found on status page {status_page_id}")]
    SubscriberNotFound {
        status_page_id: i32,
        subscriber_id: i32,
    },
    #[error("sender address '{sender}' cannot be used: {reason}")]
    InvalidSender { sender: String, reason: String },
    #[error("subscriptions on status page {status_page_id} cannot be enabled: {reason}")]
    NotReady { status_page_id: i32, reason: String },
    #[error("could not read the email setup for status page subscriptions: {reason}")]
    EmailSetup { reason: String },
    #[error("could not send status page mail to subscriber {subscriber_id}: {reason}")]
    Send { subscriber_id: i32, reason: String },
    #[error("could not read the settings that hold the public URL: {reason}")]
    Settings { reason: String },
}
