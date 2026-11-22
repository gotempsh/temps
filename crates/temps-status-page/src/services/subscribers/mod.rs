// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Email subscribers of public status pages (ADR 108 D2): double opt-in
//! subscription from the public page, notices derived from the public
//! snapshot, and a bounded background dispatcher.

pub mod dispatcher;
pub mod error;
pub mod mailer;
pub mod notices;
pub mod service;
pub mod tokens;
pub mod types;

pub use error::SubscriptionError;
pub use mailer::{SubscriberMailer, TempsEmailMailer};
pub use service::StatusPageSubscriberService;
pub use types::*;

#[cfg(test)]
mod tests;
