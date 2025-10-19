//! Session replay analytics module
//! 
//! Provides session recording and replay capabilities for user interaction analysis.

pub mod services;
pub mod handlers;
pub mod plugin;
pub mod types;

// Re-export plugin
pub use plugin::SessionReplayPlugin;
