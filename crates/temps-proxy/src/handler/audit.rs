// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared pieces of the audit entries the instance-wide proxy settings write
//! (custom routes, the IP block list).

use serde::Serialize;
use temps_core::{AuditLogger, AuditOperation, RequestMetadata};
use tracing::error;

/// Who made the request and from where, stored with every entry.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AuditContext {
    pub user_id: Option<i32>,
    pub ip_address: Option<String>,
    pub user_agent: String,
}

impl AuditContext {
    pub fn new(user_id: Option<i32>, metadata: &RequestMetadata) -> Self {
        Self {
            user_id,
            ip_address: Some(metadata.ip_address.clone()),
            user_agent: metadata.user_agent.clone(),
        }
    }
}

/// Implement [`AuditOperation`] for a struct with a `context: AuditContext`
/// field that serialises to JSON.
macro_rules! impl_audit_operation {
    ($ty:ty, $name:literal) => {
        impl temps_core::AuditOperation for $ty {
            fn operation_type(&self) -> String {
                $name.to_string()
            }

            fn user_id(&self) -> Option<i32> {
                self.context.user_id
            }

            fn ip_address(&self) -> Option<String> {
                self.context.ip_address.clone()
            }

            fn user_agent(&self) -> &str {
                &self.context.user_agent
            }

            fn serialize(&self) -> temps_core::AuditResult<String> {
                serde_json::to_string(self)
                    .map_err(|source| temps_core::AuditError::serialize($name, source))
            }
        }
    };
}
pub(crate) use impl_audit_operation;

/// Record an entry after the write succeeded. A failed audit write is logged
/// and never fails the request.
pub(crate) async fn write_audit(logger: &dyn AuditLogger, audit: &dyn AuditOperation) {
    if let Err(e) = logger.create_audit_log(audit).await {
        error!(
            "Failed to create audit log for {}: {}",
            audit.operation_type(),
            e
        );
    }
}

/// An audit sink that keeps what it was given, so handler tests can assert
/// that a write was recorded (and a refused one was not).
#[cfg(test)]
#[derive(Default)]
pub(crate) struct RecordingAuditLogger {
    entries: parking_lot::Mutex<Vec<(String, String)>>,
}

#[cfg(test)]
impl RecordingAuditLogger {
    /// `(operation_type, serialised payload)` for every entry, in order.
    pub fn entries(&self) -> Vec<(String, String)> {
        self.entries.lock().clone()
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl AuditLogger for RecordingAuditLogger {
    async fn create_audit_log(
        &self,
        operation: &dyn AuditOperation,
    ) -> temps_core::AuditResult<()> {
        self.entries
            .lock()
            .push((operation.operation_type(), operation.serialize()?));
        Ok(())
    }
}

/// A request's metadata as the console middleware would build it.
#[cfg(test)]
pub(crate) fn test_metadata(ip_address: &str) -> RequestMetadata {
    RequestMetadata {
        ip_address: ip_address.to_string(),
        user_agent: "test".to_string(),
        headers: axum::http::HeaderMap::new(),
        visitor_id_cookie: None,
        session_id_cookie: None,
        base_url: "http://localhost".to_string(),
        scheme: "http".to_string(),
        host: "localhost".to_string(),
        is_secure: false,
    }
}

/// A signed-in user for building an `AuthContext` in tests.
#[cfg(test)]
pub(crate) fn test_user() -> temps_entities::users::Model {
    let now = chrono::Utc::now();
    temps_entities::users::Model {
        id: 1,
        name: "Test User".to_string(),
        email: "test@example.com".to_string(),
        password_hash: None,
        email_verified: true,
        email_verification_token: None,
        email_verification_expires: None,
        password_reset_token: None,
        password_reset_expires: None,
        must_change_password: false,
        deleted_at: None,
        mfa_secret: None,
        mfa_enabled: false,
        mfa_recovery_codes: None,
        oidc_subject: None,
        oidc_provider_id: None,
        created_at: now,
        updated_at: now,
    }
}
