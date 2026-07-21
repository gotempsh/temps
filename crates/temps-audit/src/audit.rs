//! Audit operations emitted by the audit subsystem itself.

use anyhow::Result;
use serde::Serialize;
use temps_core::{AuditContext, AuditOperation};

/// Recorded BEFORE a scrub pass begins mutating rows, so that a scrub
/// interrupted mid-scan (DB error, crash) still has attribution — without
/// this, pages already rewritten would carry `[REDACTED]` markers with no
/// trail saying who started the operation. The completion entry
/// ([`AuditDataScrubbedAudit`]) is the matching receipt; a start entry with
/// no receipt means the scrub did not finish and should be re-run.
#[derive(Debug, Clone, Serialize)]
pub struct AuditDataScrubStartedAudit {
    pub context: AuditContext,
    /// Which request fields were provided (e.g. `["email", "username"]`) —
    /// never the identifier values themselves.
    pub identifier_fields: Vec<String>,
}

impl AuditOperation for AuditDataScrubStartedAudit {
    fn operation_type(&self) -> String {
        "AUDIT_DATA_SCRUB_STARTED".to_string()
    }

    fn user_id(&self) -> i32 {
        self.context.user_id
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}

/// Recorded when an operator scrubs PII identifier values out of audit
/// `data` payloads (typically after a user-account deletion, to satisfy an
/// erasure request without destroying the structural audit trail).
///
/// Deliberately records only which kinds of identifiers were provided and
/// how many rows changed — never the identifier values themselves, which are
/// exactly the data being erased.
#[derive(Debug, Clone, Serialize)]
pub struct AuditDataScrubbedAudit {
    pub context: AuditContext,
    /// Which request fields were provided (e.g. `["email", "username"]`).
    pub identifier_fields: Vec<String>,
    /// Number of audit rows whose payload was inspected.
    pub rows_scanned: u64,
    /// Number of audit rows that had at least one value redacted.
    pub rows_scrubbed: u64,
}

impl AuditOperation for AuditDataScrubbedAudit {
    fn operation_type(&self) -> String {
        "AUDIT_DATA_SCRUBBED".to_string()
    }

    fn user_id(&self) -> i32 {
        self.context.user_id
    }

    fn ip_address(&self) -> Option<String> {
        self.context.ip_address.clone()
    }

    fn user_agent(&self) -> &str {
        &self.context.user_agent
    }

    fn serialize(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| anyhow::anyhow!("Failed to serialize audit operation {}", e))
    }
}
