//! # temps-webhooks
//!
//! Webhook delivery system for Temps platform events.
//!
//! This crate provides functionality for:
//! - Configuring webhooks for projects
//! - Delivering webhook payloads to user-configured URLs
//! - Retry logic with exponential backoff
//! - Webhook delivery logging and history

mod events;
mod handlers;
mod listener;
mod plugin;
mod service;

pub use events::{WebhookEvent, WebhookEventType, WebhookPayload};
pub use handlers::{configure_routes, WebhookState, WebhooksApiDoc};
pub use listener::WebhookEventListener;
pub use plugin::WebhooksPlugin;
pub use service::{WebhookDeliveryResult, WebhookError, WebhookService};
