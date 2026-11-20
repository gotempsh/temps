// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The body and signature of a webhook delivery.
//!
//! The body is built from a [`PageEvent`] only, so it carries exactly what
//! the public page shows. The signature header is
//! `X-Temps-Signature: t=<unix seconds>,v1=<hex HMAC-SHA256>` over
//! `"<t>.<body>"` with the endpoint's secret; a receiver recomputes it and
//! rejects old timestamps to stop replays.

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use temps_core::UtcDateTime;
use utoipa::ToSchema;

use crate::services::public_pages::events::{PageEvent, PageEventKind, PageEventStatus};
use crate::services::public_pages::feed::opaque_id;

pub const SIGNATURE_HEADER: &str = "x-temps-signature";
pub const EVENT_HEADER: &str = "x-temps-event";
pub const EVENT_ID_HEADER: &str = "x-temps-event-id";
pub const USER_AGENT: &str = "Temps-Status-Page-Webhook/1.0";

/// The page an event belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WebhookPage {
    pub title: String,
}

/// One public happening on the page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WebhookEvent {
    /// Stable per page and event, the same as the Atom entry id: use it to
    /// drop a repeated delivery.
    pub id: String,
    pub title: String,
    #[schema(value_type = String)]
    pub status: String,
    pub message: String,
    pub affected_components: Vec<String>,
    #[schema(value_type = String, format = "date-time")]
    pub published_at: UtcDateTime,
    /// When a maintenance window starts; absent for incident events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = "date-time")]
    pub starts_at: Option<UtcDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = "date-time")]
    pub ends_at: Option<UtcDateTime>,
}

/// The JSON body of every webhook delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WebhookPayload {
    /// `incident_update`, `incident_resolved` or `maintenance_scheduled`.
    #[serde(rename = "type")]
    #[schema(value_type = String)]
    pub kind: String,
    pub page: WebhookPage,
    pub event: WebhookEvent,
}

fn kind_name(kind: PageEventKind) -> &'static str {
    match kind {
        PageEventKind::IncidentUpdate => "incident_update",
        PageEventKind::IncidentResolved => "incident_resolved",
        PageEventKind::MaintenanceScheduled => "maintenance_scheduled",
    }
}

fn status_name(status: PageEventStatus) -> &'static str {
    match status {
        PageEventStatus::Investigating => "investigating",
        PageEventStatus::Identified => "identified",
        PageEventStatus::Monitoring => "monitoring",
        PageEventStatus::Resolved => "resolved",
        PageEventStatus::Scheduled => "scheduled",
    }
}

/// The body for `event` of the page `slug` titled `page_title`. The slug
/// only seeds the opaque event id; it is never sent.
pub fn payload_for(slug: &str, page_title: &str, event: &PageEvent) -> WebhookPayload {
    WebhookPayload {
        kind: kind_name(event.kind).to_string(),
        page: WebhookPage {
            title: page_title.to_string(),
        },
        event: WebhookEvent {
            id: opaque_id(&[slug, &event.key]),
            title: event.title.clone(),
            status: status_name(event.status).to_string(),
            message: event.message_or_default().to_string(),
            affected_components: event.affected_components.clone(),
            published_at: event.published_at,
            starts_at: event.window.map(|w| w.starts_at),
            ends_at: event.window.map(|w| w.ends_at),
        },
    }
}

/// The `X-Temps-Signature` value for `body` sent at `timestamp`.
pub fn signature(secret: &str, timestamp: i64, body: &str) -> String {
    #[allow(
        clippy::expect_used,
        reason = "HMAC accepts keys of any length, so new_from_slice cannot fail"
    )]
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes any key");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body.as_bytes());
    format!(
        "t={timestamp},v1={}",
        hex::encode(mac.finalize().into_bytes())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::public_pages::events::EventWindow;
    use chrono::TimeZone;

    fn at(hour: u32) -> UtcDateTime {
        chrono::Utc
            .with_ymd_and_hms(2026, 3, 2, hour, 0, 0)
            .single()
            .expect("valid time")
    }

    #[test]
    fn the_body_carries_the_public_event_and_never_the_slug_or_key() {
        let event = PageEvent {
            key: "m4".to_string(),
            kind: PageEventKind::MaintenanceScheduled,
            title: "Database upgrade".to_string(),
            status: PageEventStatus::Scheduled,
            message: None,
            affected_components: vec!["API".to_string()],
            published_at: at(3),
            window: Some(EventWindow {
                starts_at: at(20),
                ends_at: at(22),
            }),
        };
        let payload = payload_for("secret-slug", "Example status", &event);
        let body = serde_json::to_string(&payload).expect("json");
        assert!(
            body.contains("\"type\":\"maintenance_scheduled\""),
            "{body}"
        );
        assert!(body.contains("\"status\":\"scheduled\""));
        assert!(body.contains("\"starts_at\":\"2026-03-02T20:00:00Z\""));
        assert!(
            !body.contains("secret-slug") && !body.contains("\"m4\""),
            "{body}"
        );
        assert_eq!(payload.event.message, "Maintenance has been scheduled.");
    }

    #[test]
    fn the_signature_covers_the_timestamp_and_body() {
        let one = signature("whsec_a", 1_700_000_000, "{}");
        assert!(one.starts_with("t=1700000000,v1="));
        assert_eq!(one.len(), "t=1700000000,v1=".len() + 64);
        assert_ne!(one, signature("whsec_a", 1_700_000_001, "{}"));
        assert_ne!(one, signature("whsec_a", 1_700_000_000, "{ }"));
        assert_ne!(one, signature("whsec_b", 1_700_000_000, "{}"));
    }
}
