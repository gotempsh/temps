// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Kubeconfig parsing and resolution
//!
//! Parses a kubeconfig YAML (the standard `~/.kube/config` format) and
//! resolves the selected context into a connectable configuration. Only
//! embedded credentials are supported — file paths and exec plugins cannot
//! work inside the temps server and produce actionable errors instead.

use base64::Engine;
use serde::Deserialize;

use crate::error::KubernetesImportError;

// ---------------------------------------------------------------------------
// Raw kubeconfig file structure (subset)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct KubeconfigFile {
    #[serde(default)]
    clusters: Vec<NamedCluster>,
    #[serde(default)]
    contexts: Vec<NamedContext>,
    #[serde(default)]
    users: Vec<NamedUser>,
    #[serde(rename = "current-context")]
    current_context: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NamedCluster {
    name: String,
    cluster: ClusterEntry,
}

#[derive(Debug, Deserialize)]
struct ClusterEntry {
    server: String,
    #[serde(rename = "certificate-authority-data")]
    certificate_authority_data: Option<String>,
    #[serde(rename = "certificate-authority")]
    certificate_authority: Option<String>,
    #[serde(rename = "insecure-skip-tls-verify", default)]
    insecure_skip_tls_verify: bool,
}

#[derive(Debug, Deserialize)]
struct NamedContext {
    name: String,
    context: ContextEntry,
}

#[derive(Debug, Deserialize)]
struct ContextEntry {
    cluster: String,
    user: String,
    namespace: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NamedUser {
    name: String,
    user: UserEntry,
}

#[derive(Debug, Default, Deserialize)]
struct UserEntry {
    token: Option<String>,
    #[serde(rename = "tokenFile")]
    token_file: Option<String>,
    #[serde(rename = "client-certificate-data")]
    client_certificate_data: Option<String>,
    #[serde(rename = "client-key-data")]
    client_key_data: Option<String>,
    #[serde(rename = "client-certificate")]
    client_certificate: Option<String>,
    #[serde(rename = "client-key")]
    client_key: Option<String>,
    username: Option<String>,
    password: Option<String>,
    exec: Option<serde_json::Value>,
    #[serde(rename = "auth-provider")]
    auth_provider: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Resolved configuration
// ---------------------------------------------------------------------------

/// Authentication method resolved from the kubeconfig
#[derive(Debug, Clone)]
pub enum KubeAuth {
    /// Bearer token (ServiceAccount token, static token)
    Token(String),
    /// Embedded client certificate + key (PEM)
    ClientCert { cert_pem: Vec<u8>, key_pem: Vec<u8> },
    /// HTTP basic auth
    Basic { username: String, password: String },
}

/// A kubeconfig context resolved into everything needed to connect
#[derive(Debug, Clone)]
pub struct ResolvedKubeconfig {
    /// API server URL (e.g. `https://1.2.3.4:6443`)
    pub server: String,
    /// Cluster CA certificate (PEM), decoded from `certificate-authority-data`
    pub ca_pem: Option<Vec<u8>>,
    /// Whether TLS verification is disabled in the kubeconfig
    pub skip_tls_verify: bool,
    /// Resolved authentication
    pub auth: KubeAuth,
    /// Default namespace from the context (if any)
    pub namespace: Option<String>,
    /// Name of the resolved context (for display)
    pub context_name: String,
    /// Name of the resolved cluster (for display)
    pub cluster_name: String,
}

fn decode_b64(field: &str, value: &str) -> Result<Vec<u8>, KubernetesImportError> {
    base64::engine::general_purpose::STANDARD
        .decode(value.trim())
        .map_err(|e| KubernetesImportError::InvalidBase64 {
            field: field.to_string(),
            reason: e.to_string(),
        })
}

/// Parse a kubeconfig YAML and resolve the given (or current) context.
pub fn resolve_kubeconfig(
    yaml: &str,
    context_override: Option<&str>,
) -> Result<ResolvedKubeconfig, KubernetesImportError> {
    let file: KubeconfigFile =
        serde_yaml::from_str(yaml).map_err(|e| KubernetesImportError::InvalidKubeconfig {
            reason: e.to_string(),
        })?;

    let context_name = context_override
        .map(|s| s.to_string())
        .or(file.current_context.clone())
        .ok_or_else(|| KubernetesImportError::InvalidKubeconfig {
            reason: "no current-context set and no context override provided".to_string(),
        })?;

    let available = || {
        file.contexts
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };

    let context = file
        .contexts
        .iter()
        .find(|c| c.name == context_name)
        .ok_or_else(|| KubernetesImportError::ContextNotFound {
            context: context_name.clone(),
            available: available(),
        })?;

    let cluster = file
        .clusters
        .iter()
        .find(|c| c.name == context.context.cluster)
        .ok_or_else(|| KubernetesImportError::ClusterNotFound {
            context: context_name.clone(),
            cluster: context.context.cluster.clone(),
        })?;

    let user = file
        .users
        .iter()
        .find(|u| u.name == context.context.user)
        .ok_or_else(|| KubernetesImportError::UserNotFound {
            context: context_name.clone(),
            user: context.context.user.clone(),
        })?;

    // Reject configurations that cannot work server-side, with actionable messages
    if user.user.exec.is_some() || user.user.auth_provider.is_some() {
        return Err(KubernetesImportError::ExecAuthUnsupported {
            user: user.name.clone(),
        });
    }
    if user.user.token_file.is_some() {
        return Err(KubernetesImportError::FileRefUnsupported {
            field: "tokenFile".to_string(),
        });
    }
    if user.user.client_certificate.is_some() || user.user.client_key.is_some() {
        return Err(KubernetesImportError::FileRefUnsupported {
            field: "client-certificate".to_string(),
        });
    }
    if cluster.cluster.certificate_authority.is_some() {
        return Err(KubernetesImportError::FileRefUnsupported {
            field: "certificate-authority".to_string(),
        });
    }

    let auth = if let Some(token) = &user.user.token {
        KubeAuth::Token(token.clone())
    } else if let (Some(cert), Some(key)) = (
        &user.user.client_certificate_data,
        &user.user.client_key_data,
    ) {
        KubeAuth::ClientCert {
            cert_pem: decode_b64("client-certificate-data", cert)?,
            key_pem: decode_b64("client-key-data", key)?,
        }
    } else if let (Some(username), Some(password)) = (&user.user.username, &user.user.password) {
        KubeAuth::Basic {
            username: username.clone(),
            password: password.clone(),
        }
    } else {
        return Err(KubernetesImportError::NoUsableCredentials {
            user: user.name.clone(),
        });
    };

    let ca_pem = cluster
        .cluster
        .certificate_authority_data
        .as_deref()
        .map(|d| decode_b64("certificate-authority-data", d))
        .transpose()?;

    Ok(ResolvedKubeconfig {
        server: cluster.cluster.server.trim_end_matches('/').to_string(),
        ca_pem,
        skip_tls_verify: cluster.cluster.insecure_skip_tls_verify,
        auth,
        namespace: context.context.namespace.clone(),
        context_name,
        cluster_name: cluster.name.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const K3S_STYLE: &str = r#"
apiVersion: v1
clusters:
- cluster:
    certificate-authority-data: Q0EgUEVNIGRhdGE=
    server: https://203.0.113.10:6443
  name: default
contexts:
- context:
    cluster: default
    user: default
  name: default
current-context: default
kind: Config
users:
- name: default
  user:
    client-certificate-data: Q0VSVCBQRU0=
    client-key-data: S0VZIFBFTQ==
"#;

    #[test]
    fn test_resolve_k3s_style_client_cert() {
        let cfg = resolve_kubeconfig(K3S_STYLE, None).unwrap();
        assert_eq!(cfg.server, "https://203.0.113.10:6443");
        assert_eq!(cfg.context_name, "default");
        assert_eq!(cfg.ca_pem.as_deref(), Some(b"CA PEM data".as_slice()));
        match cfg.auth {
            KubeAuth::ClientCert { cert_pem, key_pem } => {
                assert_eq!(cert_pem, b"CERT PEM");
                assert_eq!(key_pem, b"KEY PEM");
            }
            other => panic!("expected client cert auth, got {:?}", other),
        }
    }

    #[test]
    fn test_resolve_token_auth() {
        let yaml = r#"
clusters:
- cluster: { server: "https://example.com:6443" }
  name: c
contexts:
- context: { cluster: c, user: u, namespace: apps }
  name: ctx
current-context: ctx
users:
- name: u
  user: { token: "sa-token-abc" }
"#;
        let cfg = resolve_kubeconfig(yaml, None).unwrap();
        assert!(matches!(cfg.auth, KubeAuth::Token(ref t) if t == "sa-token-abc"));
        assert_eq!(cfg.namespace.as_deref(), Some("apps"));
        assert!(cfg.ca_pem.is_none());
    }

    #[test]
    fn test_exec_auth_rejected_with_actionable_message() {
        let yaml = r#"
clusters:
- cluster: { server: "https://eks.example.com" }
  name: c
contexts:
- context: { cluster: c, user: u }
  name: ctx
current-context: ctx
users:
- name: u
  user:
    exec:
      apiVersion: client.authentication.k8s.io/v1beta1
      command: aws
"#;
        let err = resolve_kubeconfig(yaml, None).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("exec credential plugin"), "{}", msg);
        assert!(
            msg.contains("kubectl -n kube-system create token"),
            "{}",
            msg
        );
    }

    #[test]
    fn test_missing_context_lists_available() {
        let err = resolve_kubeconfig(K3S_STYLE, Some("staging")).unwrap_err();
        match err {
            KubernetesImportError::ContextNotFound { context, available } => {
                assert_eq!(context, "staging");
                assert_eq!(available, "default");
            }
            other => panic!("unexpected error: {:?}", other),
        }
    }

    #[test]
    fn test_file_path_refs_rejected() {
        let yaml = r#"
clusters:
- cluster: { server: "https://example.com" }
  name: c
contexts:
- context: { cluster: c, user: u }
  name: ctx
current-context: ctx
users:
- name: u
  user:
    client-certificate: /home/user/.certs/client.crt
    client-key: /home/user/.certs/client.key
"#;
        let err = resolve_kubeconfig(yaml, None).unwrap_err();
        assert!(matches!(
            err,
            KubernetesImportError::FileRefUnsupported { .. }
        ));
    }

    #[test]
    fn test_garbage_yaml_is_invalid_kubeconfig() {
        let err = resolve_kubeconfig("{not valid yaml: [", None).unwrap_err();
        assert!(matches!(
            err,
            KubernetesImportError::InvalidKubeconfig { .. }
        ));
    }
}
