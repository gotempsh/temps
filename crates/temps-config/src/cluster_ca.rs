// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Transactional per-cluster CA initialization shared by enrollment-token
//! minting and node registration.

use crate::{ClusterCaRotationResult, ConfigService};
use temps_core::EncryptionService;

/// A resolved cluster CA: the public certificate and decrypted private key.
pub struct ClusterCa {
    pub cert_pem: String,
    pub key_pem: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ClusterCaError {
    #[error(transparent)]
    Settings(#[from] crate::ConfigServiceError),
    #[error("Cluster CA generation failed: {0}")]
    Pki(String),
    #[error("Cluster CA encryption failed: {0}")]
    Encryption(String),
}

fn decrypt_key(
    encryption_service: &EncryptionService,
    encrypted: &str,
) -> Result<String, ClusterCaError> {
    let bytes = encryption_service
        .decrypt(encrypted)
        .map_err(|error| ClusterCaError::Encryption(error.to_string()))?;
    String::from_utf8(bytes).map_err(|error| ClusterCaError::Encryption(error.to_string()))
}

/// Return the existing cluster CA or initialize it transactionally.
///
/// The generated candidate is persisted through a locked settings-row update.
/// If another request wins the race, both callers receive the winner's CA.
pub async fn ensure_cluster_ca(
    config_service: &ConfigService,
    encryption_service: &EncryptionService,
) -> Result<ClusterCa, ClusterCaError> {
    let settings = config_service.get_settings().await?;

    match (
        settings.multi_node.cluster_ca_cert_pem.as_deref(),
        settings.multi_node.cluster_ca_key_encrypted.as_deref(),
    ) {
        (Some(cert), Some(encrypted_key)) => {
            return Ok(ClusterCa {
                cert_pem: cert.to_string(),
                key_pem: decrypt_key(encryption_service, encrypted_key)?,
            });
        }
        (Some(_), None) | (None, Some(_)) => {
            return Err(crate::ConfigServiceError::InvalidConfiguration {
                details: "cluster CA settings are incomplete; refusing automatic replacement"
                    .to_string(),
            }
            .into());
        }
        (None, None) => {}
    }

    let generated = temps_core::node_pki::generate_cluster_ca()
        .map_err(|error| ClusterCaError::Pki(error.to_string()))?;
    let encrypted_key = encryption_service
        .encrypt(generated.key_pem.as_bytes())
        .map_err(|error| ClusterCaError::Encryption(error.to_string()))?;
    let (cert_pem, encrypted_key) = config_service
        .initialize_cluster_ca_material(generated.cert_pem, encrypted_key)
        .await?;

    Ok(ClusterCa {
        cert_pem,
        key_pem: decrypt_key(encryption_service, &encrypted_key)?,
    })
}

/// Generate and atomically activate a replacement cluster trust root.
pub async fn rotate_cluster_ca(
    config_service: &ConfigService,
    encryption_service: &EncryptionService,
    expected_fingerprint: &str,
) -> Result<(ClusterCa, ClusterCaRotationResult), ClusterCaError> {
    let generated = temps_core::node_pki::generate_cluster_ca()
        .map_err(|error| ClusterCaError::Pki(error.to_string()))?;
    let encrypted_key = encryption_service
        .encrypt(generated.key_pem.as_bytes())
        .map_err(|error| ClusterCaError::Encryption(error.to_string()))?;
    let result = config_service
        .rotate_cluster_ca_material(
            expected_fingerprint,
            generated.cert_pem.clone(),
            encrypted_key,
        )
        .await?;

    Ok((
        ClusterCa {
            cert_pem: generated.cert_pem,
            key_pem: generated.key_pem,
        },
        result,
    ))
}
