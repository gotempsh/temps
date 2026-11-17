// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Requests the control plane encrypts to the worker they are sent to.
//!
//! Environment variables and secrets must stay unreadable to anyone who can
//! read the traffic between the control plane and a worker, TLS included. A
//! request carrying them is sealed as a whole to the node's X25519 key
//! (`nodes.edge_public_key`) and only that node's private key opens it.
//!
//! The whole body is one ciphertext, so the values cannot be split from the
//! image and container name they arrived with, and the HKDF label includes the
//! route, so a captured request cannot be replayed to a different one. ECIES
//! says who can read a request, not who sent it: authenticating the sender
//! stays the job of mTLS and the node token.

use crate::ecies::{decrypt_bundle_with_info, EciesError, EncryptedBundle, EncryptionSession};
use crate::node_address::is_https_agent_address;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Content type of a sealed request. Agents from before sealing answer it with
/// 415 from their `Json` extractor, so they can never run a workload with its
/// variables missing, and the sender can tell them to upgrade. Deliberately not
/// `+json`: axum's extractor accepts that suffix, and would try to read the
/// envelope as the request itself.
pub const SEALED_REQUEST_CONTENT_TYPE: &str = "application/vnd.temps.sealed-request";

/// Largest sealed request an agent reads. Holds the largest Compose spec (16
/// MiB) after base64.
pub const MAX_SEALED_REQUEST_BYTES: usize = 32 * 1024 * 1024;

/// Largest sealed sandbox upload an agent reads. Encoding costs about 16/9 (the
/// JSON's base64, then the envelope's), so this holds about 72 MiB of file
/// content, well past the largest attachment (20 MiB). Opening one holds the
/// envelope and the plaintext together, about 224 MiB at this size, which is why
/// an agent opens these inside an upload slot and why the cap is not the 512 MiB
/// a plain upload may use.
pub const MAX_SEALED_UPLOAD_BYTES: usize = 128 * 1024 * 1024;

/// HKDF label, kept apart from certificate bundles and Shield keys.
const INFO_LABEL: &[u8] = b"temps-agent-request-v1\n";

fn request_info(path: &str) -> Vec<u8> {
    let mut info = Vec::with_capacity(INFO_LABEL.len() + path.len());
    info.extend_from_slice(INFO_LABEL);
    info.extend_from_slice(path.as_bytes());
    info
}

#[derive(Serialize, Deserialize)]
struct SealedRequest {
    ephemeral_public_key: String,
    ciphertext: String,
    nonce: String,
}

/// Why a request carrying values was not sent to a node.
#[derive(Debug, Error)]
pub enum SealRequestError {
    #[error(
        "Node '{node}' has no encryption key on record, so its environment variables and secrets \
         cannot be sent to it. Upgrade its worker agent, which registers a key on start, or \
         re-run `temps join` on it"
    )]
    NoNodeKey { node: String },

    #[error(
        "Node '{node}' is reachable only over a plain http:// address, where anyone on the path \
         can read its token and use it to read a container's environment, so its environment \
         variables and secrets are not sent to it. Re-run `temps join` on it to give it an https \
         (mTLS) address"
    )]
    PlainHttp { node: String },

    #[error("Could not encode the request to {path} for node '{node}': {source}")]
    Encode {
        node: String,
        path: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("Could not encrypt the request to {path} for node '{node}': {source}")]
    Encrypt {
        node: String,
        path: String,
        #[source]
        source: EciesError,
    },
}

/// Why an agent could not open a sealed request.
#[derive(Debug, Error)]
pub enum OpenRequestError {
    #[error("The sealed request to {path} is not a valid envelope: {source}")]
    Malformed {
        path: String,
        #[source]
        source: serde_json::Error,
    },

    #[error(
        "The sealed request to {path} cannot be opened with this node's key. It was sealed for \
         another key or route, or was altered. If this worker lost or changed its key, re-run \
         `temps join` on it: {source}"
    )]
    Decrypt {
        path: String,
        #[source]
        source: EciesError,
    },
}

/// How a client may send values to one node.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum NodeSealing {
    /// Seal to this base64 X25519 public key.
    Key(String),
    /// No key on record: values are refused.
    #[default]
    NoKey,
    /// A plain `http://` agent address: values are refused even with a key,
    /// since the node token crosses the wire readable.
    PlainHttp,
}

impl NodeSealing {
    /// The policy for an agent dialed at `dial_url` with the key on its node row.
    pub fn for_agent(dial_url: &str, public_key: Option<&str>) -> Self {
        if !is_https_agent_address(dial_url) {
            return Self::PlainHttp;
        }
        match public_key.map(str::trim).filter(|key| !key.is_empty()) {
            Some(key) => Self::Key(key.to_string()),
            None => Self::NoKey,
        }
    }

    /// Seal `plaintext` for `path`, or say why it may not be sent to `node`.
    pub fn seal(
        &self,
        node: &str,
        path: &str,
        plaintext: &[u8],
    ) -> Result<Vec<u8>, SealRequestError> {
        let public_key = match self {
            Self::Key(key) => key,
            Self::NoKey => {
                return Err(SealRequestError::NoNodeKey {
                    node: node.to_string(),
                })
            }
            Self::PlainHttp => {
                return Err(SealRequestError::PlainHttp {
                    node: node.to_string(),
                })
            }
        };
        let encrypt_failed = |source| SealRequestError::Encrypt {
            node: node.to_string(),
            path: path.to_string(),
            source,
        };
        let session = EncryptionSession::with_info(public_key, &request_info(path))
            .map_err(encrypt_failed)?;
        let bundle = session.encrypt(plaintext).map_err(encrypt_failed)?;
        serde_json::to_vec(&SealedRequest {
            ephemeral_public_key: session.ephemeral_public_key().to_string(),
            ciphertext: bundle.ciphertext,
            nonce: bundle.nonce,
        })
        .map_err(|source| SealRequestError::Encode {
            node: node.to_string(),
            path: path.to_string(),
            source,
        })
    }

    /// [`Self::seal`] for a JSON body.
    pub fn seal_json<B: Serialize + ?Sized>(
        &self,
        node: &str,
        path: &str,
        body: &B,
    ) -> Result<Vec<u8>, SealRequestError> {
        let plaintext = serde_json::to_vec(body).map_err(|source| SealRequestError::Encode {
            node: node.to_string(),
            path: path.to_string(),
            source,
        })?;
        self.seal(node, path, &plaintext)
    }
}

/// Open a request sealed by [`NodeSealing::seal`] for `path`.
pub fn open_request(
    private_key_b64: &str,
    path: &str,
    sealed: &[u8],
) -> Result<Vec<u8>, OpenRequestError> {
    let envelope: SealedRequest =
        serde_json::from_slice(sealed).map_err(|source| OpenRequestError::Malformed {
            path: path.to_string(),
            source,
        })?;
    decrypt_bundle_with_info(
        private_key_b64,
        &envelope.ephemeral_public_key,
        &EncryptedBundle {
            ciphertext: envelope.ciphertext,
            nonce: envelope.nonce,
        },
        &request_info(path),
    )
    .map_err(|source| OpenRequestError::Decrypt {
        path: path.to_string(),
        source,
    })
}

/// What a client reports when `node` answers a sealed request with 415.
pub fn older_agent_message(node: &str) -> String {
    format!(
        "The worker agent on node '{node}' is too old to receive encrypted environment variables \
         and secrets, so nothing was sent to it. Upgrade its worker agent"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD, Engine};

    fn keypair() -> (String, String) {
        let secret = crate::ecies::generate_x25519_static_secret().unwrap();
        let public = x25519_dalek::PublicKey::from(&secret);
        (
            STANDARD.encode(secret.as_bytes()),
            STANDARD.encode(public.as_bytes()),
        )
    }

    const PATH: &str = "/agent/containers/deploy";
    const BODY: &[u8] = br#"{"secrets":{"API_KEY":"value-only-the-node-may-read"}}"#;

    #[test]
    fn only_the_node_key_opens_a_request_and_the_value_is_never_on_the_wire() {
        let (private, public) = keypair();
        let sealed = NodeSealing::Key(public).seal("worker", PATH, BODY).unwrap();

        let wire = String::from_utf8(sealed.clone()).unwrap();
        assert!(!wire.contains("value-only-the-node-may-read"));
        assert!(!wire.contains("API_KEY"));
        assert_eq!(open_request(&private, PATH, &sealed).unwrap(), BODY);

        let (other_private, _) = keypair();
        assert!(matches!(
            open_request(&other_private, PATH, &sealed),
            Err(OpenRequestError::Decrypt { .. })
        ));
    }

    #[test]
    fn a_request_cannot_move_to_another_route() {
        let (private, public) = keypair();
        let sealed = NodeSealing::Key(public).seal("worker", PATH, BODY).unwrap();
        let error = open_request(&private, "/agent/services", &sealed).unwrap_err();
        assert!(error.to_string().contains("/agent/services"), "{error}");
        assert!(matches!(error, OpenRequestError::Decrypt { .. }));
    }

    #[test]
    fn an_altered_request_is_refused() {
        let (private, public) = keypair();
        let sealed = NodeSealing::Key(public).seal("worker", PATH, BODY).unwrap();
        let mut envelope: SealedRequest = serde_json::from_slice(&sealed).unwrap();
        let mut ciphertext = STANDARD.decode(&envelope.ciphertext).unwrap();
        ciphertext[0] ^= 1;
        envelope.ciphertext = STANDARD.encode(ciphertext);
        assert!(matches!(
            open_request(&private, PATH, &serde_json::to_vec(&envelope).unwrap()),
            Err(OpenRequestError::Decrypt { .. })
        ));
    }

    #[test]
    fn a_request_is_not_readable_with_another_purposes_label() {
        let (private, public) = keypair();
        let sealed = NodeSealing::Key(public).seal("worker", PATH, BODY).unwrap();
        let envelope: SealedRequest = serde_json::from_slice(&sealed).unwrap();
        assert!(crate::ecies::decrypt_bundle(
            &private,
            &envelope.ephemeral_public_key,
            &EncryptedBundle {
                ciphertext: envelope.ciphertext,
                nonce: envelope.nonce,
            },
        )
        .is_err());
    }

    #[test]
    fn each_request_uses_a_fresh_ephemeral_key() {
        let (_, public) = keypair();
        let sealing = NodeSealing::Key(public);
        let first: SealedRequest =
            serde_json::from_slice(&sealing.seal("worker", PATH, BODY).unwrap()).unwrap();
        let second: SealedRequest =
            serde_json::from_slice(&sealing.seal("worker", PATH, BODY).unwrap()).unwrap();
        assert_ne!(first.ephemeral_public_key, second.ephemeral_public_key);
    }

    #[test]
    fn garbage_is_a_malformed_envelope() {
        let (private, _) = keypair();
        assert!(matches!(
            open_request(&private, PATH, b"{\"image_name\":\"plain\"}"),
            Err(OpenRequestError::Malformed { .. })
        ));
    }

    #[test]
    fn values_are_refused_without_a_key_or_on_plain_http_and_the_error_names_the_node() {
        let (_, public) = keypair();
        assert_eq!(
            NodeSealing::for_agent("https://10.0.0.1:3100", Some(&public)),
            NodeSealing::Key(public.clone())
        );
        for no_key in [None, Some(""), Some("   ")] {
            assert_eq!(
                NodeSealing::for_agent("https://10.0.0.1:3100", no_key),
                NodeSealing::NoKey
            );
        }
        let plain = NodeSealing::for_agent("http://10.0.0.1:3100", Some(&public));
        assert_eq!(plain, NodeSealing::PlainHttp);

        let error = plain.seal("worker-a", PATH, BODY).unwrap_err();
        assert!(matches!(error, SealRequestError::PlainHttp { .. }));
        assert!(error.to_string().contains("worker-a"), "{error}");
        let error = NodeSealing::default()
            .seal("worker-b", PATH, BODY)
            .unwrap_err();
        assert!(matches!(error, SealRequestError::NoNodeKey { .. }));
        assert!(error.to_string().contains("worker-b"), "{error}");
        assert!(error.to_string().contains("temps join"), "{error}");
    }

    #[test]
    fn an_invalid_key_is_an_error_not_a_panic() {
        let error = NodeSealing::Key("not-a-key".to_string())
            .seal("worker", PATH, BODY)
            .unwrap_err();
        assert!(matches!(error, SealRequestError::Encrypt { .. }));
    }
}
