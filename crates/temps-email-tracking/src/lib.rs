// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Email tracking crate for temps
//!
//! Provides open tracking (pixel), click tracking (link rewriting + redirect),
//! and SES bounce/complaint/delivery event processing via SNS webhooks.

pub mod errors;
pub mod event_service;
pub mod handlers;
pub mod hmac;
pub mod html_rewriter;
pub mod plugin;
pub mod sns;

pub use errors::EmailTrackingError;
pub use event_service::EmailEventService;
pub use handlers::TrackingState;
pub use html_rewriter::HtmlTrackingRewriter;
pub use plugin::EmailTrackingPlugin;
pub use sns::SnsVerifier;
