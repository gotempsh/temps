//! Funnels analytics module
//! 
//! Provides funnel analysis capabilities for tracking user conversion paths.

pub mod services;
pub mod handlers;
pub mod plugin;
pub mod types;

// Re-export plugin
pub use plugin::FunnelsPlugin;
