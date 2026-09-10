// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Error types for the Kubernetes importer

use temps_import_types::ImportError;
use thiserror::Error;

/// Errors specific to Kubernetes cluster access and kubeconfig handling
#[derive(Error, Debug)]
pub enum KubernetesImportError {
    #[error("No kubeconfig provided: pass the kubeconfig YAML in credentials.extra[\"kubeconfig\"] (or base64-encoded in extra[\"kubeconfig_base64\"])")]
    MissingKubeconfig,

    #[error("Failed to parse kubeconfig: {reason}")]
    InvalidKubeconfig { reason: String },

    #[error("Kubeconfig context '{context}' not found (available: {available})")]
    ContextNotFound { context: String, available: String },

    #[error("Kubeconfig context '{context}' references cluster '{cluster}' which is not defined")]
    ClusterNotFound { context: String, cluster: String },

    #[error("Kubeconfig context '{context}' references user '{user}' which is not defined")]
    UserNotFound { context: String, user: String },

    #[error("Kubeconfig user '{user}' uses an exec credential plugin, which cannot run inside the temps server. Create a long-lived ServiceAccount token instead: kubectl -n kube-system create token <serviceaccount> --duration=24h (or use a kubeconfig with embedded client-certificate-data)")]
    ExecAuthUnsupported { user: String },

    #[error("Kubeconfig field '{field}' references a local file path, which is not available inside the temps server. Use a kubeconfig with embedded base64 data fields ({field}-data)")]
    FileRefUnsupported { field: String },

    #[error("Kubeconfig user '{user}' has no usable credentials (expected token, client-certificate-data + client-key-data, or username/password)")]
    NoUsableCredentials { user: String },

    #[error("Invalid base64 in kubeconfig field '{field}': {reason}")]
    InvalidBase64 { field: String, reason: String },

    #[error("Cluster API server URL '{url}' rejected: {reason}")]
    ServerUrlRejected { url: String, reason: String },

    #[error("Failed to build HTTP client for cluster '{server}': {reason}")]
    ClientBuild { server: String, reason: String },

    #[error("Kubernetes API request to '{path}' failed: {reason}")]
    ApiRequest { path: String, reason: String },

    #[error("Kubernetes API returned HTTP {status} for '{path}': {body}")]
    ApiStatus {
        path: String,
        status: u16,
        body: String,
    },

    #[error("Failed to decode Kubernetes API response from '{path}': {reason}")]
    ApiDecode { path: String, reason: String },

    #[error("Invalid workload id '{id}': expected '<namespace>/<kind>/<name>' with kind one of deployment|statefulset|cronjob")]
    InvalidWorkloadId { id: String },

    #[error("Workload '{kind}/{name}' not found in namespace '{namespace}'")]
    WorkloadNotFound {
        namespace: String,
        kind: String,
        name: String,
    },
}

impl From<KubernetesImportError> for ImportError {
    fn from(error: KubernetesImportError) -> Self {
        match &error {
            KubernetesImportError::MissingKubeconfig
            | KubernetesImportError::InvalidKubeconfig { .. }
            | KubernetesImportError::ContextNotFound { .. }
            | KubernetesImportError::ClusterNotFound { .. }
            | KubernetesImportError::UserNotFound { .. }
            | KubernetesImportError::InvalidBase64 { .. }
            | KubernetesImportError::NoUsableCredentials { .. } => {
                ImportError::InvalidCredentials(error.to_string())
            }
            KubernetesImportError::ExecAuthUnsupported { .. }
            | KubernetesImportError::FileRefUnsupported { .. } => {
                ImportError::UnsupportedFeature(error.to_string())
            }
            KubernetesImportError::ServerUrlRejected { .. } => {
                ImportError::InvalidConfiguration(error.to_string())
            }
            KubernetesImportError::ClientBuild { .. }
            | KubernetesImportError::ApiRequest { .. } => {
                ImportError::SourceNotAccessible(error.to_string())
            }
            KubernetesImportError::ApiStatus { status, .. } => match *status {
                401 | 403 => ImportError::AuthenticationError(error.to_string()),
                404 => ImportError::ContainerNotFound(error.to_string()),
                429 => ImportError::RateLimitExceeded(error.to_string()),
                _ => ImportError::SourceNotAccessible(error.to_string()),
            },
            KubernetesImportError::ApiDecode { .. } => {
                ImportError::InspectionFailed(error.to_string())
            }
            KubernetesImportError::InvalidWorkloadId { .. } => {
                ImportError::InvalidConfiguration(error.to_string())
            }
            KubernetesImportError::WorkloadNotFound { .. } => {
                ImportError::ContainerNotFound(error.to_string())
            }
        }
    }
}
