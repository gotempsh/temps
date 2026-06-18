//! Append-only audit log of on-demand TLS issuance attempts (ADR-018 §5).
//!
//! Every on-demand HTTP-01 issuance attempt — successful, failed, or skipped —
//! writes exactly one row here. This is the design's first-class observability
//! pillar: a misconfigured domain, an exhausted Let's Encrypt rate limit, a
//! blocked port 80, or a DNS failure must each produce a queryable record with
//! the full error chain rather than an opaque TLS handshake error at the client.
//!
//! This table is NOT the authoritative cert state — that lives on the `domains`
//! row (`status`, `last_error`, `on_demand_backoff_until`). This is the audit
//! log: multiple rows per hostname are expected (retries, renewals). It is
//! written only by the proxy process; the console reads it for the
//! "Certificates" UI.

use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "on_demand_cert_attempts")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// SNI hostname that triggered the attempt.
    pub hostname: String,
    /// What triggered the attempt. Always `"tls_callback"` in this feature.
    pub trigger: String,
    /// Did the proxy serve the `/.well-known/acme-challenge/` request? `None`
    /// when the attempt was skipped before any challenge was served.
    pub challenge_served: Option<bool>,
    /// Did we reach the Let's Encrypt API? `None` when skipped before any ACME
    /// request was made.
    pub acme_request_sent: Option<bool>,
    /// HTTP status or ACME error type returned by Let's Encrypt, when known.
    pub acme_response_status: Option<String>,
    /// Final outcome: `"issued"`, `"failed"`, `"skipped_duplicate"`,
    /// `"skipped_gate"`, `"skipped_rate_limit"`, or `"skipped_no_route"`.
    pub outcome: String,
    /// Full `Display` chain of the error (all `source()` levels), when failed.
    pub error_chain: Option<String>,
    /// Coarse error category for UI labelling: `"rate_limited"`,
    /// `"dns_failure"`, `"acme_order_expired"`, `"challenge_mismatch"`,
    /// `"timeout"`, `"internal"`, or `None`.
    pub error_category: Option<String>,
    /// End-to-end issuance duration in milliseconds (0/None for skipped).
    pub duration_ms: Option<i32>,
    pub created_at: DBDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

#[async_trait]
impl ActiveModelBehavior for ActiveModel {
    async fn before_save<C>(mut self, _db: &C, insert: bool) -> Result<Self, DbErr>
    where
        C: ConnectionTrait,
    {
        // Append-only: only stamp `created_at` on insert. There is no
        // `updated_at` — rows are never mutated after they are written.
        if insert && self.created_at.is_not_set() {
            self.created_at = Set(chrono::Utc::now());
        }
        Ok(self)
    }
}
