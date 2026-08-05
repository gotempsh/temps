use anyhow::Result;
use serde::Serialize;
use std::sync::{Arc, RwLock};

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

    /// Returns the user ID recorded as the audit actor. `None` when the actor
    /// has no resolvable user account, such as an unknown login identity or a
    /// deployment token.
    fn user_id(&self) -> Option<i32>;

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

/// Stable indirection for audit consumers constructed before optional
/// decorators register. Replacing the target updates every previously-captured
/// `Arc<dyn AuditLogger>`, so decorators registered later cannot be bypassed.
pub struct AuditLoggerSlot {
    target: RwLock<Arc<dyn AuditLogger>>,
}

impl AuditLoggerSlot {
    pub fn new(target: Arc<dyn AuditLogger>) -> Self {
        Self {
            target: RwLock::new(target),
        }
    }

    pub fn current(&self) -> Result<Arc<dyn AuditLogger>> {
        self.target
            .read()
            .map(|target| target.clone())
            .map_err(|_| anyhow::anyhow!("audit logger slot read lock is poisoned"))
    }

    pub fn replace(&self, target: Arc<dyn AuditLogger>) -> Result<()> {
        let mut current = self
            .target
            .write()
            .map_err(|_| anyhow::anyhow!("audit logger slot write lock is poisoned"))?;
        *current = target;
        Ok(())
    }
}

#[async_trait::async_trait]
impl AuditLogger for AuditLoggerSlot {
    async fn create_audit_log(&self, operation: &dyn AuditOperation) -> Result<()> {
        let target = self.current()?;
        target.create_audit_log(operation).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TestOperation;

    impl AuditOperation for TestOperation {
        fn operation_type(&self) -> String {
            "TEST".to_string()
        }
        fn user_id(&self) -> Option<i32> {
            None
        }
        fn ip_address(&self) -> Option<String> {
            None
        }
        fn user_agent(&self) -> &str {
            "test"
        }
        fn serialize(&self) -> Result<String> {
            Ok("{}".to_string())
        }
    }

    struct CountingLogger(Arc<AtomicUsize>);

    #[async_trait::async_trait]
    impl AuditLogger for CountingLogger {
        async fn create_audit_log(&self, _operation: &dyn AuditOperation) -> Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn captured_trait_object_routes_to_replacement() {
        let original_count = Arc::new(AtomicUsize::new(0));
        let replacement_count = Arc::new(AtomicUsize::new(0));
        let original: Arc<dyn AuditLogger> = Arc::new(CountingLogger(original_count.clone()));
        let slot = Arc::new(AuditLoggerSlot::new(original));
        let captured: Arc<dyn AuditLogger> = slot.clone();

        captured.create_audit_log(&TestOperation).await.unwrap();
        slot.replace(Arc::new(CountingLogger(replacement_count.clone())))
            .unwrap();
        captured.create_audit_log(&TestOperation).await.unwrap();

        assert_eq!(original_count.load(Ordering::SeqCst), 1);
        assert_eq!(replacement_count.load(Ordering::SeqCst), 1);
    }
}
