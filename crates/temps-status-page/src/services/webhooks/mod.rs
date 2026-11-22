// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Webhooks of status pages (ADR 108 D2).
//!
//! An owner registers HTTPS endpoints on a page (at most
//! [`service::MAX_WEBHOOKS_PER_PAGE`]); there is no public sign-up, so a
//! visitor can never make the server post to an address of their choosing.
//! Each new public event of the page (an incident update, a resolution, a
//! scheduled maintenance window) is posted once to every enabled endpoint as
//! a typed JSON body built from the published snapshot, signed with the
//! endpoint's secret.
//!
//! Delivery runs in the background: every address is resolved and checked
//! against the outbound policy at send time, redirects are not followed,
//! each attempt times out, and an event is tried
//! [`dispatcher::MAX_ATTEMPTS`] times before it is marked failed. The
//! console shows each endpoint's last outcome.

pub mod dispatcher;
pub mod error;
pub mod payload;
pub mod service;
pub mod transport;
pub mod types;

pub use error::WebhookError;
pub use service::StatusPageWebhookService;
pub use transport::{PinnedWebhookTransport, TransportError, WebhookTransport};
pub use types::*;

#[cfg(test)]
mod tests;
