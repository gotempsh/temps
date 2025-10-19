// Re-export audit traits from core for convenience
pub use temps_core::{AuditEvent, AuditOperation, AuditContext};

pub mod handlers;
pub mod services;

// Re-export the AuditService for convenience
pub use services::*;

pub mod plugin;
pub use plugin::AuditPlugin;
