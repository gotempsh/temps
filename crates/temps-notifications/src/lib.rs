//! notifications services and utilities

pub mod plugin;
pub mod services;
pub use handlers::{configure_routes, NotificationProvidersApiDoc};
pub use plugin::NotificationsPlugin;
pub use services::*;
pub use services::{
    NotificationPreferences, NotificationPreferencesService, NotificationProvider,
    NotificationService,
};
mod handlers;
mod types;
