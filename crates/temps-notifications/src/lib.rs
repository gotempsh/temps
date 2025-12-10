//! notifications services and utilities

pub mod digest;
pub mod plugin;
pub mod services;
pub mod vulnerability_notifications;
pub use digest::{DigestSections, DigestService, WeeklyDigestData};
pub use handlers::{configure_routes, NotificationProvidersApiDoc};
pub use plugin::NotificationsPlugin;
pub use services::*;
pub use services::{
    NotificationPreferences, NotificationPreferencesService, NotificationProvider,
    NotificationService,
};
pub use vulnerability_notifications::VulnerabilityNotificationHandler;
mod handlers;
mod types;
