// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Request and response shapes of status page subscriptions (ADR 108 D2).

use serde::{Deserialize, Serialize};
use temps_core::UtcDateTime;
use utoipa::ToSchema;

/// Whether a prerequisite of subscriptions is in place, and if not, what is
/// missing and where to fix it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SubscriptionCapability {
    pub configured: bool,
    /// What is missing, in words an operator can act on.
    pub reason: Option<String>,
    /// Console path that fixes it; `None` when the fix is on the same form.
    pub setup_path: Option<String>,
}

impl SubscriptionCapability {
    pub fn ready() -> Self {
        Self {
            configured: true,
            reason: None,
            setup_path: None,
        }
    }

    pub fn missing(reason: impl Into<String>, setup_path: Option<String>) -> Self {
        Self {
            configured: false,
            reason: Some(reason.into()),
            setup_path,
        }
    }
}

/// Email subscription settings of one status page.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SubscriptionSettingsResponse {
    pub status_page_id: i32,
    /// Visitors see a subscribe form on the public page.
    pub enabled: bool,
    /// When subscriptions were last switched on; only updates published
    /// after it are announced.
    #[schema(value_type = Option<String>, format = "date-time")]
    pub enabled_at: Option<UtcDateTime>,
    /// From address of subscriber mail.
    pub sender: Option<String>,
    /// Whether mail can be sent from `sender`.
    pub email: SubscriptionCapability,
    /// Whether confirmation and unsubscribe links have an address people
    /// outside this network can open.
    pub links: SubscriptionCapability,
    /// Verified email domains with an active provider: valid sender domains.
    pub verified_domains: Vec<String>,
    pub confirmed_count: u64,
    pub pending_count: u64,
    /// Most subscribers a page can have, confirmed and pending together.
    pub max_subscribers: u64,
}

/// Change the email subscription settings of a status page. Absent fields are
/// left as they are.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct UpdateSubscriptionSettingsRequest {
    /// Show the subscribe form. Switching on needs a working sender.
    pub enabled: Option<bool>,
    /// From address at a verified email domain; an empty string clears it.
    pub sender: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SubscriberState {
    /// Asked to subscribe; has not followed the confirmation link.
    Pending,
    /// Confirmed; receives notices.
    Confirmed,
}

impl SubscriberState {
    pub fn from_column(value: &str) -> Self {
        if value == temps_entities::status_page_subscribers::STATE_CONFIRMED {
            Self::Confirmed
        } else {
            Self::Pending
        }
    }
}

/// One subscriber, as the page's administrators see it. Never shown to
/// anyone else.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SubscriberResponse {
    pub id: i32,
    pub email: String,
    pub state: SubscriberState,
    #[schema(value_type = String, format = "date-time")]
    pub created_at: UtcDateTime,
    #[schema(value_type = Option<String>, format = "date-time")]
    pub confirmed_at: Option<UtcDateTime>,
}

impl From<temps_entities::status_page_subscribers::Model> for SubscriberResponse {
    fn from(model: temps_entities::status_page_subscribers::Model) -> Self {
        Self {
            id: model.id,
            email: model.email,
            state: SubscriberState::from_column(&model.state),
            created_at: model.created_at,
            confirmed_at: model.confirmed_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SubscriberListResponse {
    pub items: Vec<SubscriberResponse>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
}

/// What a public request to a subscription route did. The answer a visitor
/// sees never says whether an address was already subscribed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicOutcome {
    /// Subscribe accepted (new, repeated or already confirmed: all alike).
    Accepted,
    /// The address is not a valid email address.
    InvalidEmail,
    /// The page cannot take more subscribers.
    Full,
    Confirmed,
    /// Confirmation link unknown, expired or already used.
    LinkInvalid,
    Unsubscribed,
}
