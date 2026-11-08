// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The seam between the Cloud link and the worker credential scoper (ADR-093).
//!
//! A worker node never receives a Temps Cloud managed source's own bucket
//! token: it receives a credential Cloud mints for one job, limited to the
//! job's key prefixes, to reading or writing, and to a lifetime. Only Cloud
//! can mint one (it holds the Cloudflare account API), and only the Cloud
//! plugin (`temps-cloud`) can ask it; the scoper (`temps-providers`) knows
//! what a job needs. Neither crate depends on the other, so the Cloud plugin
//! registers an implementation of this trait and the scoper resolves it.

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use thiserror::Error;

/// The longest a credential Temps Cloud mints lives: its token rotation
/// overlap. R2 ends a temporary credential when its parent token is deleted,
/// and Cloud deletes a rotated token once that overlap has passed (Cloud
/// ADR 0119), so a longer request is clamped to this.
pub const MANAGED_SCOPED_CREDENTIAL_MAX_LIFETIME: Duration = Duration::from_secs(6 * 60 * 60);

/// What a scoped credential may do inside its prefixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopedS3Access {
    /// Read and list: a restore.
    Read,
    /// Write, read back and list: a backup or continuous archiving.
    Write,
}

impl ScopedS3Access {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
        }
    }
}

/// One credential to mint for the managed source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedS3CredentialRequest {
    /// Object-key prefixes inside the managed bucket, each ending in `/`.
    pub prefixes: Vec<String>,
    pub access: ScopedS3Access,
    pub lifetime: Duration,
}

/// A minted credential. Secret: `Debug` prints neither the secret nor the
/// session token.
#[derive(Clone, PartialEq, Eq)]
pub struct ScopedS3Credential {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
    pub expires_at: DateTime<Utc>,
}

impl std::fmt::Debug for ScopedS3Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScopedS3Credential")
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"[REDACTED]")
            .field(
                "session_token",
                &self.session_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum ScopedS3CredentialError {
    /// This instance is not linked to Temps Cloud, so nothing can mint for
    /// its managed source.
    #[error("this instance is not connected to Temps Cloud, which mints the managed source's scoped credentials")]
    NotConnected,
    /// The Cloud this instance talks to has no scoped-credential route yet.
    #[error("Temps Cloud cannot scope managed backup credentials yet (it answered {status} to the scoped-credentials request)")]
    Unsupported { status: u16 },
    /// Cloud refused this request and retrying will not change that: a
    /// prefix outside the tenant, no paid plan for a write, revoked quota.
    #[error("Temps Cloud refused a scoped {access} credential for {prefixes:?}: {reason}")]
    Refused {
        access: &'static str,
        prefixes: Vec<String>,
        reason: String,
    },
    /// Cloud could not be reached or failed; a retry may succeed.
    #[error("Temps Cloud could not mint a scoped {access} credential for {prefixes:?}: {reason}")]
    Transient {
        access: &'static str,
        prefixes: Vec<String>,
        reason: String,
    },
}

/// Registered by the Cloud plugin as `Arc<dyn ManagedBackupScopedCredentials>`.
#[async_trait]
pub trait ManagedBackupScopedCredentials: Send + Sync {
    async fn mint(
        &self,
        request: ScopedS3CredentialRequest,
    ) -> Result<ScopedS3Credential, ScopedS3CredentialError>;
}
