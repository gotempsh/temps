use anyhow::Result;
use serde::Serialize;

/// Context information common to all audit events
#[derive(Debug, Clone, Serialize)]
pub struct AuditContext {
    pub user_id: i32,
    pub ip_address: Option<String>,
    pub user_agent: String,
}

/// Trait that all audit events must implement
pub trait AuditEvent: Send + Sync {
    /// Returns the operation type (e.g., "USER_CREATED", "LOGIN_SUCCESS")
    fn operation_type(&self) -> String;

    /// Returns the audit context (user, IP, timestamp, etc.)
    fn context(&self) -> &AuditContext;
}

/// Extended trait for audit operations with serialization and additional methods
/// This is compatible with the existing AuditOperation trait pattern
pub trait AuditOperation: Send + Sync {
    /// Returns the operation type (e.g., "USER_CREATED", "LOGIN_SUCCESS")
    fn operation_type(&self) -> String;

    /// Returns the user ID who performed the operation
    fn user_id(&self) -> i32;

    /// Returns the IP address if available
    fn ip_address(&self) -> Option<String>;

    /// Returns the user agent string
    fn user_agent(&self) -> &str;

    /// Serializes the operation to JSON
    fn serialize(&self) -> Result<String>;
}

/// Trait for services that can create audit logs
#[async_trait::async_trait]
pub trait AuditLogger: Send + Sync {
    /// Creates an audit log entry for the given operation
    async fn create_audit_log(&self, operation: &dyn AuditOperation) -> Result<()>;
}
