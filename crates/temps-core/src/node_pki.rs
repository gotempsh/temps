//! Per-cluster certificate authority and per-node certificate issuance for
//! multi-node mTLS (ADR-020 WS-2.1 / WS-1.1).
//!
//! The control plane mints a single per-cluster CA when multi-node is first
//! enabled, storing the CA key encrypted at rest (via `EncryptionService`).
//! At enrollment a worker generates its **own** keypair and a CSR; the control
//! plane signs it with the cluster CA and returns a per-node leaf certificate.
//! The node's private key never leaves the node. Thereafter the agent serves
//! TLS with its leaf and the control plane presents a client certificate the
//! agent pins to the cluster CA — so the bearer token stops being the only
//! secret, the channel is encrypted+mutually-authenticated even in Direct mode,
//! and a malicious relay cannot substitute identities (the enrollment token
//! carries the CA fingerprint, which the node verifies out of band).
//!
//! This module is transport-agnostic: it only produces/consumes PEM material.
//! Wiring it into the agent's TLS listener, the control-plane client, and the
//! enrollment handshake happens in the respective crates.

use rcgen::string::Ia5String;
use rcgen::{
    BasicConstraints, CertificateParams, CertificateSigningRequestParams, DistinguishedName,
    DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose, SanType,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Classify a SAN string as an IP address or DNS name (rcgen needs typed SANs).
fn san_from_str(s: &str) -> Result<SanType, PkiError> {
    if let Ok(ip) = s.parse::<std::net::IpAddr>() {
        Ok(SanType::IpAddress(ip))
    } else {
        Ia5String::try_from(s.to_string())
            .map(SanType::DnsName)
            .map_err(|e| PkiError::CertBuild(format!("invalid DNS SAN '{s}': {e}")))
    }
}

/// Errors from CA / certificate operations.
#[derive(Debug, Error)]
pub enum PkiError {
    #[error("Failed to generate key pair: {0}")]
    KeyGen(String),

    #[error("Failed to build certificate: {0}")]
    CertBuild(String),

    #[error("Failed to parse PEM ({context}): {reason}")]
    PemParse { context: String, reason: String },

    #[error("Failed to sign node CSR: {reason}")]
    CsrSign { reason: String },
}

/// A freshly minted per-cluster certificate authority. PEM-encoded.
#[derive(Debug, Clone)]
pub struct ClusterCa {
    /// CA certificate (distribute to nodes and the control-plane client so they
    /// can verify each other; this is public).
    pub cert_pem: String,
    /// CA private key — SECRET. The control plane stores this encrypted at rest
    /// and never ships it to a worker.
    pub key_pem: String,
}

/// A node's freshly generated keypair + certificate-signing request. PEM-encoded.
#[derive(Debug, Clone)]
pub struct NodeCsr {
    /// The node's private key — stays on the node, never transmitted.
    pub key_pem: String,
    /// The CSR to send to the control plane for signing.
    pub csr_pem: String,
}

/// Generate a new self-signed cluster CA (ECDSA P-256, long-lived).
///
/// Call this once when multi-node is first enabled; persist the returned
/// `key_pem` encrypted and the `cert_pem` for distribution.
pub fn generate_cluster_ca() -> Result<ClusterCa, PkiError> {
    let key = KeyPair::generate().map_err(|e| PkiError::KeyGen(e.to_string()))?;

    let mut params = CertificateParams::new(Vec::<String>::new())
        .map_err(|e| PkiError::CertBuild(e.to_string()))?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "Temps Cluster CA");
    dn.push(DnType::OrganizationName, "Temps");
    params.distinguished_name = dn;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];

    let ca_cert = params
        .self_signed(&key)
        .map_err(|e| PkiError::CertBuild(e.to_string()))?;

    Ok(ClusterCa {
        cert_pem: ca_cert.pem(),
        key_pem: key.serialize_pem(),
    })
}

/// SHA-256 fingerprint (lowercase hex) of a CA certificate's DER bytes.
///
/// The enrollment token carries this so a joining node can verify the control
/// plane's CA out of band — defeating a malicious relay that tries to swap in
/// its own CA (ADR-020 WS-2.2).
pub fn ca_fingerprint_sha256(cert_pem: &str) -> Result<String, PkiError> {
    let der = pem_to_der(cert_pem, "CA certificate")?;
    let digest = Sha256::digest(&der);
    Ok(hex::encode(digest))
}

/// Node side: generate a fresh keypair and a CSR. `common_name` is the subject
/// CN; `sans` are the Subject Alternative Names the leaf must be valid for —
/// rcgen auto-classifies each entry as an IP or DNS SAN. The control plane
/// connects to a worker by its IP address, so the node's IP MUST be in `sans`
/// or server-cert hostname verification fails (ADR-020 WS-2.1). The private key
/// never leaves the caller.
pub fn generate_node_keypair_csr(common_name: &str, sans: &[String]) -> Result<NodeCsr, PkiError> {
    let key = KeyPair::generate().map_err(|e| PkiError::KeyGen(e.to_string()))?;

    let mut params =
        CertificateParams::new(sans.to_vec()).map_err(|e| PkiError::CertBuild(e.to_string()))?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, common_name);
    params.distinguished_name = dn;

    let csr = params
        .serialize_request(&key)
        .map_err(|e| PkiError::CertBuild(e.to_string()))?;

    Ok(NodeCsr {
        key_pem: key.serialize_pem(),
        csr_pem: csr.pem().map_err(|e| PkiError::CertBuild(e.to_string()))?,
    })
}

/// A signed leaf certificate plus metadata the control plane records on the
/// node row for rotation/revocation tracking.
#[derive(Debug, Clone)]
pub struct SignedNodeCert {
    pub cert_pem: String,
    /// SHA-256 fingerprint of the leaf DER (stored as the node's pinned identity).
    pub fingerprint: String,
}

/// Control-plane side: sign a node's CSR with the cluster CA, producing a
/// client+server leaf certificate (the agent uses it as a TLS server cert and
/// the control plane trusts it as a client cert; both EKUs are set).
///
/// `allowed_sans` are the **server-authoritative** Subject Alternative Names the
/// leaf is constrained to — the node's registered `{IP, name}`. The worker's own
/// SANs in the CSR are discarded. This is a security boundary: every node pins
/// the same cluster CA, so a leaf the CA signs is trusted cluster-wide; without
/// constraining SANs, a compromised worker could request (and the CA would sign)
/// a cert valid for the control plane's or another node's identity, enabling
/// impersonation / mTLS MITM. (ADR-020 WS-2.1.)
pub fn sign_node_csr(
    ca_cert_pem: &str,
    ca_key_pem: &str,
    csr_pem: &str,
    allowed_sans: &[String],
) -> Result<SignedNodeCert, PkiError> {
    // Reconstruct an issuer handle from the stored CA material.
    let ca_key = KeyPair::from_pem(ca_key_pem).map_err(|e| PkiError::PemParse {
        context: "CA key".into(),
        reason: e.to_string(),
    })?;
    let ca_issuer =
        Issuer::from_ca_cert_pem(ca_cert_pem, ca_key).map_err(|e| PkiError::PemParse {
            context: "CA certificate".into(),
            reason: e.to_string(),
        })?;

    // Parse the CSR; constrain the leaf to client+server auth.
    let mut csr =
        CertificateSigningRequestParams::from_pem(csr_pem).map_err(|e| PkiError::PemParse {
            context: "CSR".into(),
            reason: e.to_string(),
        })?;
    csr.params.is_ca = IsCa::NoCa;
    csr.params.use_authority_key_identifier_extension = true;
    csr.params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    csr.params.extended_key_usages = vec![
        ExtendedKeyUsagePurpose::ServerAuth,
        ExtendedKeyUsagePurpose::ClientAuth,
    ];

    // Server-authoritative SANs: discard whatever the worker put in the CSR and
    // set the leaf's identity from the values the control plane registered for
    // this node. See the doc comment for the threat this closes.
    let mut sans = Vec::with_capacity(allowed_sans.len());
    for s in allowed_sans {
        sans.push(san_from_str(s)?);
    }
    csr.params.subject_alt_names = sans;

    let leaf = csr.signed_by(&ca_issuer).map_err(|e| PkiError::CsrSign {
        reason: e.to_string(),
    })?;

    let cert_pem = leaf.pem();
    let fingerprint = {
        let der = pem_to_der(&cert_pem, "leaf certificate")?;
        hex::encode(Sha256::digest(&der))
    };

    Ok(SignedNodeCert {
        cert_pem,
        fingerprint,
    })
}

/// Decode the first PEM block's base64 body into DER bytes.
fn pem_to_der(pem: &str, context: &str) -> Result<Vec<u8>, PkiError> {
    use base64::{engine::general_purpose::STANDARD, Engine};

    let body: String = pem
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect::<Vec<_>>()
        .join("");
    if body.is_empty() {
        return Err(PkiError::PemParse {
            context: context.to_string(),
            reason: "no PEM body found".to_string(),
        });
    }
    STANDARD
        .decode(body.trim())
        .map_err(|e| PkiError::PemParse {
            context: context.to_string(),
            reason: e.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ca_generation_produces_pem() {
        let ca = generate_cluster_ca().expect("CA generation");
        assert!(ca.cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(ca.key_pem.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn test_ca_fingerprint_is_stable_64_hex() {
        let ca = generate_cluster_ca().unwrap();
        let fp1 = ca_fingerprint_sha256(&ca.cert_pem).unwrap();
        let fp2 = ca_fingerprint_sha256(&ca.cert_pem).unwrap();
        assert_eq!(fp1, fp2);
        assert_eq!(fp1.len(), 64);
        assert!(fp1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_node_csr_round_trip_signs_under_ca() {
        // Full enrollment round trip: CA -> node CSR -> CA signs -> leaf.
        let ca = generate_cluster_ca().unwrap();
        let node = generate_node_keypair_csr(
            "worker-1",
            &["10.0.0.5".to_string(), "worker-1".to_string()],
        )
        .unwrap();
        assert!(node.key_pem.contains("BEGIN PRIVATE KEY"));
        assert!(node.csr_pem.contains("CERTIFICATE REQUEST"));

        let signed = sign_node_csr(
            &ca.cert_pem,
            &ca.key_pem,
            &node.csr_pem,
            &["10.0.0.5".to_string(), "worker-1".to_string()],
        )
        .unwrap();
        assert!(signed.cert_pem.contains("BEGIN CERTIFICATE"));
        assert_eq!(signed.fingerprint.len(), 64);

        // The signed leaf must differ from the CA cert.
        assert_ne!(signed.cert_pem, ca.cert_pem);
    }

    #[test]
    fn test_sign_rejects_garbage_csr() {
        let ca = generate_cluster_ca().unwrap();
        let err = sign_node_csr(&ca.cert_pem, &ca.key_pem, "not a csr", &[]).unwrap_err();
        assert!(matches!(err, PkiError::PemParse { .. }));
    }

    #[test]
    fn test_sign_overwrites_worker_supplied_sans() {
        // Security boundary (ADR-020 WS-2.1): a worker crafts a CSR claiming a
        // rogue identity (the control plane's name + a wildcard). The CA must
        // sign the leaf with ONLY the server-authoritative SANs and drop the
        // worker's — otherwise the leaf would be trusted cluster-wide for an
        // identity the worker doesn't own.
        let ca = generate_cluster_ca().unwrap();
        let rogue = generate_node_keypair_csr(
            "worker-evil",
            &[
                "rogue-control-plane.invalid".to_string(),
                "wildcard-attacker.invalid".to_string(),
            ],
        )
        .unwrap();

        let signed = sign_node_csr(
            &ca.cert_pem,
            &ca.key_pem,
            &rogue.csr_pem,
            &["10.9.9.9".to_string(), "good-worker".to_string()],
        )
        .unwrap();

        let der = pem_to_der(&signed.cert_pem, "leaf").unwrap();
        let contains = |needle: &[u8]| der.windows(needle.len()).any(|w| w == needle);
        // The worker's rogue DNS SANs must NOT survive into the signed leaf.
        assert!(
            !contains(b"rogue-control-plane.invalid"),
            "worker-supplied rogue SAN leaked into the signed certificate"
        );
        assert!(
            !contains(b"wildcard-attacker.invalid"),
            "worker-supplied rogue SAN leaked into the signed certificate"
        );
        // The server-authoritative DNS SAN must be present.
        assert!(
            contains(b"good-worker"),
            "server-authoritative SAN missing from the signed certificate"
        );
    }
}
