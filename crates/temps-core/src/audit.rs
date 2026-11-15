// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use serde::Serialize;
use std::sync::{Arc, RwLock};
use thiserror::Error;

/// Context information common to all audit events
#[derive(Debug, Clone, Serialize)]
pub struct AuditContext {
    pub user_id: i32,
    pub ip_address: Option<String>,
    pub user_agent: String,
}

/// Failure to record an audit log entry.
///
/// Whether a failure stops the operation being audited is the caller's decision:
/// most log it and carry on, and security-sensitive paths deliberately fail closed.
#[derive(Debug, Error)]
pub enum AuditError {
    #[error("Failed to serialize audit operation {operation}: {source}")]
    Serialize {
        /// The operation being serialized, with the IDs that identify it.
        operation: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("Failed to create audit log for {operation}: {source}")]
    Write {
        /// The operation type being recorded.
        operation: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error(transparent)]
    Slot(#[from] AuditLoggerSlotError),
}

impl AuditError {
    pub fn serialize(operation: impl std::fmt::Display, source: serde_json::Error) -> Self {
        Self::Serialize {
            operation: operation.to_string(),
            source,
        }
    }

    pub fn write(
        operation: impl std::fmt::Display,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self::Write {
            operation: operation.to_string(),
            source: source.into(),
        }
    }
}

/// Result of recording or serializing an audit log entry.
pub type AuditResult<T = ()> = Result<T, AuditError>;

/// Serializes an operation to JSON, naming its operation type on failure.
pub fn serialize_audit_operation<T: AuditOperation + Serialize>(
    operation: &T,
) -> AuditResult<String> {
    serde_json::to_string(operation)
        .map_err(|source| AuditError::serialize(operation.operation_type(), source))
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
    fn serialize(&self) -> AuditResult<String>;
}

/// Trait for services that can create audit logs
#[async_trait::async_trait]
pub trait AuditLogger: Send + Sync {
    /// Creates an audit log entry for the given operation
    async fn create_audit_log(&self, operation: &dyn AuditOperation) -> AuditResult<()>;
}

/// An audit sink that records nothing, for tests and tools that construct a
/// service requiring an [`AuditLogger`] but have no audit trail to write to.
pub struct NoopAuditLogger;

#[async_trait::async_trait]
impl AuditLogger for NoopAuditLogger {
    async fn create_audit_log(&self, _operation: &dyn AuditOperation) -> AuditResult<()> {
        Ok(())
    }
}

/// Stable indirection for audit consumers constructed before optional
/// decorators register. Replacing the target updates every previously-captured
/// `Arc<dyn AuditLogger>`, so decorators registered later cannot be bypassed.
pub struct AuditLoggerSlot {
    target: RwLock<Arc<dyn AuditLogger>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AuditLoggerSlotError {
    #[error("audit logger slot read lock is poisoned")]
    ReadLockPoisoned,
    #[error("audit logger slot write lock is poisoned")]
    WriteLockPoisoned,
}

impl AuditLoggerSlot {
    pub fn new(target: Arc<dyn AuditLogger>) -> Self {
        Self {
            target: RwLock::new(target),
        }
    }

    pub fn current(&self) -> std::result::Result<Arc<dyn AuditLogger>, AuditLoggerSlotError> {
        self.target
            .read()
            .map(|target| target.clone())
            .map_err(|_| AuditLoggerSlotError::ReadLockPoisoned)
    }

    pub fn replace(
        &self,
        target: Arc<dyn AuditLogger>,
    ) -> std::result::Result<(), AuditLoggerSlotError> {
        let mut current = self
            .target
            .write()
            .map_err(|_| AuditLoggerSlotError::WriteLockPoisoned)?;
        *current = target;
        Ok(())
    }
}

#[async_trait::async_trait]
impl AuditLogger for AuditLoggerSlot {
    async fn create_audit_log(&self, operation: &dyn AuditOperation) -> AuditResult<()> {
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
        fn serialize(&self) -> AuditResult<String> {
            Ok("{}".to_string())
        }
    }

    struct CountingLogger(Arc<AtomicUsize>);

    #[async_trait::async_trait]
    impl AuditLogger for CountingLogger {
        async fn create_audit_log(&self, _operation: &dyn AuditOperation) -> AuditResult<()> {
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

    /// An operation whose JSON encoding always fails.
    struct UnserializableOperation;

    impl Serialize for UnserializableOperation {
        fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("field cannot be encoded"))
        }
    }

    impl AuditOperation for UnserializableOperation {
        fn operation_type(&self) -> String {
            "UNSERIALIZABLE".to_string()
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
        fn serialize(&self) -> AuditResult<String> {
            serialize_audit_operation(self)
        }
    }

    #[test]
    fn serialize_failure_names_the_operation_and_keeps_the_cause() {
        let error = AuditOperation::serialize(&UnserializableOperation).unwrap_err();

        assert_eq!(
            error.to_string(),
            "Failed to serialize audit operation UNSERIALIZABLE: field cannot be encoded"
        );
        assert!(matches!(
            &error,
            AuditError::Serialize { operation, .. } if operation == "UNSERIALIZABLE"
        ));
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn write_failure_names_the_operation_and_keeps_the_cause() {
        let error = AuditError::write("USER_CREATED", std::io::Error::other("connection reset"));

        assert_eq!(
            error.to_string(),
            "Failed to create audit log for USER_CREATED: connection reset"
        );
        assert!(std::error::Error::source(&error).is_some());
    }

    #[tokio::test]
    async fn poisoned_slot_write_reports_the_slot_error_unchanged() {
        let logger: Arc<dyn AuditLogger> = Arc::new(CountingLogger(Arc::new(AtomicUsize::new(0))));
        let slot = Arc::new(AuditLoggerSlot::new(logger));
        let worker_slot = slot.clone();
        let _ = std::thread::spawn(move || {
            let _write_guard = worker_slot.target.write().unwrap();
            panic!("poison audit logger slot for test");
        })
        .join();

        let error = slot.create_audit_log(&TestOperation).await.unwrap_err();

        assert!(matches!(
            error,
            AuditError::Slot(AuditLoggerSlotError::ReadLockPoisoned)
        ));
        assert_eq!(error.to_string(), "audit logger slot read lock is poisoned");
    }

    #[test]
    fn poisoned_slot_returns_typed_errors() {
        fn poisoned_slot() -> Arc<AuditLoggerSlot> {
            let logger: Arc<dyn AuditLogger> =
                Arc::new(CountingLogger(Arc::new(AtomicUsize::new(0))));
            let slot = Arc::new(AuditLoggerSlot::new(logger));
            let worker_slot = slot.clone();
            let _ = std::thread::spawn(move || {
                let _write_guard = worker_slot.target.write().unwrap();
                panic!("poison audit logger slot for test");
            })
            .join();
            slot
        }

        let read_slot = poisoned_slot();
        assert!(matches!(
            read_slot.current(),
            Err(AuditLoggerSlotError::ReadLockPoisoned)
        ));

        let write_slot = poisoned_slot();
        let replacement: Arc<dyn AuditLogger> =
            Arc::new(CountingLogger(Arc::new(AtomicUsize::new(0))));
        assert!(matches!(
            write_slot.replace(replacement),
            Err(AuditLoggerSlotError::WriteLockPoisoned)
        ));
    }
}
