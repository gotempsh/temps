pub mod analytics;
pub mod handler;
pub mod plugin;
pub mod traits;
pub mod types;

#[cfg(test)]
pub mod testing;

// Re-export main types, service, and plugin
pub use analytics::AnalyticsService;
pub use plugin::AnalyticsPlugin;
pub use traits::Analytics;
pub use types::*;
