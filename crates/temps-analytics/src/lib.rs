pub mod analytics;
pub mod traits;
pub mod types;
pub mod handler;
pub mod plugin;

#[cfg(test)]
pub mod testing;

// Re-export main types, service, and plugin
pub use analytics::AnalyticsService;
pub use traits::Analytics;
pub use types::*;
pub use plugin::AnalyticsPlugin;