// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Job-scoped credentials for the managed backup source (ADR-093).
//!
//! A worker node never receives the managed source's bucket token. For each
//! worker job the control plane asks Cloud, with its instance token, for a
//! credential limited to the job's prefixes, to reading or writing, and to a
//! lifetime; Cloud mints it from R2's temporary credentials, whose parent is
//! the tenant's token.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use temps_cloud_client::{CloudError, CloudLink};
use temps_cloud_protocol::ManagedBackupScopedCredentialsRequest;
use temps_core::{
    ManagedBackupScopedCredentials, ScopedS3Credential, ScopedS3CredentialError,
    ScopedS3CredentialRequest,
};

/// The longest lifetime Cloud mints (its token rotation overlap).
pub const MAX_SCOPED_CREDENTIAL_LIFETIME: Duration =
    temps_core::MANAGED_SCOPED_CREDENTIAL_MAX_LIFETIME;

pub struct CloudScopedCredentialMinter {
    link: Arc<CloudLink>,
}

impl CloudScopedCredentialMinter {
    pub fn new(link: Arc<CloudLink>) -> Self {
        Self { link }
    }
}

/// The wire request for `request`: its lifetime capped at what Cloud mints.
fn wire_request(request: &ScopedS3CredentialRequest) -> ManagedBackupScopedCredentialsRequest {
    ManagedBackupScopedCredentialsRequest {
        prefixes: request.prefixes.clone(),
        access: request.access.as_str().to_string(),
        ttl_seconds: request
            .lifetime
            .min(MAX_SCOPED_CREDENTIAL_LIFETIME)
            .as_secs()
            .max(1),
    }
}

/// What a Cloud client error means for a scoped credential: whether it can
/// be retried, and whether Cloud can do this at all.
fn classify(error: CloudError, request: &ScopedS3CredentialRequest) -> ScopedS3CredentialError {
    let access = request.access.as_str();
    let prefixes = request.prefixes.clone();
    match error {
        CloudError::NotEnrolled | CloudError::LinkStateUnreadable { .. } => {
            ScopedS3CredentialError::NotConnected
        }
        CloudError::RouteUnavailable { status, .. } => {
            ScopedS3CredentialError::Unsupported { status }
        }
        CloudError::Unreachable { reason, .. } => ScopedS3CredentialError::Transient {
            access,
            prefixes,
            reason,
        },
        error @ CloudError::BackupUploadIdleTimeout { .. } => ScopedS3CredentialError::Transient {
            access,
            prefixes,
            reason: error.to_string(),
        },
        CloudError::Rejected { detail } => ScopedS3CredentialError::Refused {
            access,
            prefixes,
            reason: detail,
        },
        error @ (CloudError::CredentialRejected
        | CloudError::ConfigurationBlocked { .. }
        | CloudError::FeatureDisabled { .. }
        | CloudError::EnrollmentRefused { .. }
        | CloudError::InvalidBackendUrl { .. }
        | CloudError::ClientConfiguration { .. }
        | CloudError::InvalidBackupTarget { .. }
        | CloudError::InvalidAcknowledgement { .. }) => ScopedS3CredentialError::Refused {
            access,
            prefixes,
            reason: error.to_string(),
        },
    }
}

#[async_trait]
impl ManagedBackupScopedCredentials for CloudScopedCredentialMinter {
    async fn mint(
        &self,
        request: ScopedS3CredentialRequest,
    ) -> Result<ScopedS3Credential, ScopedS3CredentialError> {
        let minted = self
            .link
            .managed_backup_scoped_credentials(&wire_request(&request))
            .await
            .map_err(|error| classify(error, &request))?;
        if minted.access_key_id.is_empty()
            || minted.secret_key.is_empty()
            || minted.session_token.is_empty()
        {
            return Err(ScopedS3CredentialError::Refused {
                access: request.access.as_str(),
                prefixes: request.prefixes,
                reason: "Temps Cloud answered an incomplete credential".to_string(),
            });
        }
        tracing::info!(
            access = request.access.as_str(),
            prefixes = ?request.prefixes,
            expires_at = %minted.expires_at,
            "Temps Cloud minted a scoped managed backup credential for a worker job"
        );
        Ok(ScopedS3Credential {
            access_key_id: minted.access_key_id,
            secret_access_key: minted.secret_key,
            session_token: Some(minted.session_token),
            expires_at: minted.expires_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_core::ScopedS3Access;

    fn request(lifetime: Duration) -> ScopedS3CredentialRequest {
        ScopedS3CredentialRequest {
            prefixes: vec!["t/managed-backups/external_services/redis/cache/".to_string()],
            access: ScopedS3Access::Write,
            lifetime,
        }
    }

    #[test]
    fn the_lifetime_is_capped_at_what_cloud_mints() {
        let wire = wire_request(&request(Duration::from_secs(7 * 24 * 60 * 60)));
        assert_eq!(wire.ttl_seconds, 6 * 60 * 60);
        assert_eq!(wire.access, "write");
        assert_eq!(
            wire.prefixes,
            vec!["t/managed-backups/external_services/redis/cache/".to_string()]
        );
        assert_eq!(
            wire_request(&request(Duration::from_secs(3600))).ttl_seconds,
            3600
        );
    }

    #[test]
    fn an_older_cloud_is_unsupported_not_transient() {
        let error = classify(
            CloudError::RouteUnavailable {
                route: "/v1/backups/managed/scoped-credentials",
                status: 404,
            },
            &request(Duration::from_secs(60)),
        );
        assert_eq!(error, ScopedS3CredentialError::Unsupported { status: 404 });
    }

    #[test]
    fn an_unreachable_cloud_is_transient_and_a_refusal_is_not() {
        let transient = classify(
            CloudError::Unreachable {
                reason: "connection reset".to_string(),
                spooled_bytes: 0,
            },
            &request(Duration::from_secs(60)),
        );
        assert!(matches!(
            transient,
            ScopedS3CredentialError::Transient { .. }
        ));
        let refused = classify(
            CloudError::Rejected {
                detail: "prefix outside this tenant".to_string(),
            },
            &request(Duration::from_secs(60)),
        );
        assert!(matches!(
            refused,
            ScopedS3CredentialError::Refused { ref reason, .. } if reason == "prefix outside this tenant"
        ));
        assert_eq!(
            classify(CloudError::NotEnrolled, &request(Duration::from_secs(60))),
            ScopedS3CredentialError::NotConnected
        );
    }
}
