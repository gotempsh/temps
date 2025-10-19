//! notifications services and utilities

pub mod services;
pub mod plugin;
pub use services::{NotificationProvider, NotificationService, NotificationPreferencesService, NotificationPreferences};
pub use handlers::{NotificationProvidersApiDoc, configure_routes};
pub use plugin::NotificationsPlugin;
pub use services::*;
mod types;
mod handlers;
