// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! notifications services and utilities

pub mod digest;
pub mod plugin;
pub mod routing;
pub mod services;
pub mod vulnerability_notifications;
pub use digest::{DigestSections, DigestService, WeeklyDigestData};
pub use handlers::{configure_routes, NotificationProvidersApiDoc};
pub use plugin::NotificationsPlugin;
pub use routing::*;
pub use services::*;
pub use services::{
    NotificationPreferences, NotificationPreferencesService, NotificationProvider,
    NotificationService,
};
pub use vulnerability_notifications::VulnerabilityNotificationHandler;
mod handlers;
pub mod types;
