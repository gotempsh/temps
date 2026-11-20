// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A throwaway Sigstore instance for tests: a Fulcio-like CA, a CT log and a
//! Rekor log, all with keys generated per test run.
//!
//! It issues certificates to any identity and logs any signature, which is
//! exactly what the tests need: a bundle that is cryptographically valid end
//! to end (chain, embedded SCT, SET, inclusion proof, checkpoint) but signed
//! by a chosen identity, so a rejection can only come from the identity
//! policy. Nothing here touches the network or real Sigstore data.

use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use rcgen::{
    BasicConstraints, CertificateParams, CustomExtension, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose, PublicKeyData, SanType,
    SerialNumber, SigningKey, PKCS_ECDSA_P256_SHA256,
};
use sha2::{Digest, Sha256};
use sigstore_verify::trust_root::TrustedRoot;

use super::ReleaseTrustSource;

/// Fulcio's OIDC issuer extension (DER UTF8String).
const FULCIO_ISSUER_OID: &[u64] = &[1, 3, 6, 1, 4, 1, 57264, 1, 8];
/// RFC 6962 embedded SCT list extension.
const CT_PRECERT_SCTS_OID: &[u64] = &[1, 3, 6, 1, 4, 1, 11129, 2, 4, 2];
const CHECKPOINT_ORIGIN: &str = "rekor.test - 4242";

pub(crate) struct TestSigstore {
    ca: Issuer<'static, KeyPair>,
    ca_spki: Vec<u8>,
    ct_key: KeyPair,
    rekor_key: KeyPair,
    pub(super) root: Arc<TrustedRoot>,
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// A DER definite length.
fn der_len(len: usize) -> Vec<u8> {
    match len {
        0..=0x7f => vec![len as u8],
        0x80..=0xff => vec![0x81, len as u8],
        _ => vec![0x82, (len >> 8) as u8, len as u8],
    }
}

fn der(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    out.extend(der_len(content.len()));
    out.extend_from_slice(content);
    out
}

fn rfc3339(offset_secs: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(offset_secs))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

impl TestSigstore {
    pub(crate) fn new() -> Self {
        let ca_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let mut ca_params = CertificateParams::default();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::OrganizationName, "temps-test");
        dn.push(DnType::CommonName, "test-fulcio");
        ca_params.distinguished_name = dn;
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let ca_cert = ca_params.self_signed(&ca_key).unwrap();
        let ca_spki = ca_key.subject_public_key_info();
        let ct_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let rekor_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();

        let log = |key: &KeyPair, url: &str| {
            let spki = key.subject_public_key_info();
            serde_json::json!({
                "baseUrl": url,
                "hashAlgorithm": "SHA2_256",
                "publicKey": {
                    "rawBytes": B64.encode(&spki),
                    "keyDetails": "PKIX_ECDSA_P256_SHA_256",
                    "validFor": {"start": rfc3339(-86_400)}
                },
                "logId": {"keyId": B64.encode(sha256(&spki))}
            })
        };
        let root = serde_json::json!({
            "mediaType": "application/vnd.dev.sigstore.trustedroot+json;version=0.1",
            "tlogs": [log(&rekor_key, "https://rekor.test")],
            "certificateAuthorities": [{
                "subject": {"organization": "temps-test", "commonName": "test-fulcio"},
                "uri": "https://fulcio.test",
                "certChain": {"certificates": [{"rawBytes": B64.encode(ca_cert.der())}]},
                "validFor": {"start": rfc3339(-86_400)}
            }],
            "ctlogs": [log(&ct_key, "https://ctfe.test")],
            "timestampAuthorities": []
        });
        let root = TrustedRoot::from_json(&root.to_string()).unwrap();
        Self {
            ca: Issuer::new(ca_params, ca_key),
            ca_spki,
            ct_key,
            rekor_key,
            root: Arc::new(root),
        }
    }

    /// The trust source that accepts this instance and nothing else.
    pub(crate) fn trust(&self) -> ReleaseTrustSource {
        ReleaseTrustSource::Fixed(self.root.clone())
    }

    /// A leaf certificate for `identity`/`issuer`, with an SCT from the CT log.
    fn issue(&self, key: &KeyPair, identity: &str, issuer: &str) -> Vec<u8> {
        let params = |extra: Option<CustomExtension>| {
            let mut params = CertificateParams::default();
            params.distinguished_name = DistinguishedName::new();
            params.serial_number = Some(SerialNumber::from_slice(&[0x01, 0x23, 0x45]));
            params.subject_alt_names = vec![SanType::URI(identity.try_into().unwrap())];
            params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
            params.extended_key_usages = vec![ExtendedKeyUsagePurpose::CodeSigning];
            params.custom_extensions = vec![CustomExtension::from_oid_content(
                FULCIO_ISSUER_OID,
                der(0x0c, issuer.as_bytes()),
            )];
            params.custom_extensions.extend(extra);
            params
        };

        // RFC 6962 precertificate entry: the TBS without the SCT extension.
        let precert = params(None).signed_by(key, &self.ca).unwrap();
        let (_, parsed) = x509_parser::parse_x509_certificate(precert.der()).unwrap();
        let tbs = parsed.tbs_certificate.as_ref();
        let timestamp_ms = (chrono::Utc::now().timestamp() - 120) as u64 * 1000;
        let mut signed = vec![0u8, 0u8];
        signed.extend(timestamp_ms.to_be_bytes());
        signed.extend(1u16.to_be_bytes());
        signed.extend(sha256(&self.ca_spki));
        signed.extend(&(tbs.len() as u32).to_be_bytes()[1..]);
        signed.extend(tbs);
        signed.extend(0u16.to_be_bytes());
        let sct_signature = self.ct_key.sign(&signed).unwrap();

        let mut sct = vec![0u8];
        sct.extend(sha256(&self.ct_key.subject_public_key_info()));
        sct.extend(timestamp_ms.to_be_bytes());
        sct.extend(0u16.to_be_bytes());
        sct.extend([4u8, 3u8]); // SHA-256, ECDSA
        sct.extend((sct_signature.len() as u16).to_be_bytes());
        sct.extend(sct_signature);
        let mut list = ((sct.len() + 2) as u16).to_be_bytes().to_vec();
        list.extend((sct.len() as u16).to_be_bytes());
        list.extend(sct);

        let sct_ext = CustomExtension::from_oid_content(CT_PRECERT_SCTS_OID, der(0x04, &list));
        params(Some(sct_ext))
            .signed_by(key, &self.ca)
            .unwrap()
            .der()
            .to_vec()
    }

    /// A v0.3 bundle over `artifact`, as cosign `sign-blob --bundle` writes
    /// it, signed by a certificate for `identity` from OIDC `issuer`.
    pub(crate) fn sign(&self, artifact: &[u8], identity: &str, issuer: &str) -> String {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let cert = self.issue(&key, identity, issuer);
        let signature = key.sign(artifact).unwrap();
        let digest = sha256(artifact);

        let pem = format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            B64.encode(&cert)
                .as_bytes()
                .chunks(64)
                .map(|line| std::str::from_utf8(line).unwrap())
                .collect::<Vec<_>>()
                .join("\n")
        );
        // Canonical (RFC 8785) by construction: sorted keys, no whitespace.
        let body = format!(
            r#"{{"apiVersion":"0.0.1","kind":"hashedrekord","spec":{{"data":{{"hash":{{"algorithm":"sha256","value":"{}"}}}},"signature":{{"content":"{}","publicKey":{{"content":"{}"}}}}}}}}"#,
            hex::encode(digest),
            B64.encode(&signature),
            B64.encode(pem.as_bytes()),
        );

        let rekor_spki = self.rekor_key.subject_public_key_info();
        let log_id = sha256(&rekor_spki);
        let integrated_time = chrono::Utc::now().timestamp() - 60;
        let set_payload = format!(
            r#"{{"body":"{}","integratedTime":{},"logID":"{}","logIndex":0}}"#,
            B64.encode(body.as_bytes()),
            integrated_time,
            hex::encode(log_id),
        );
        let set = self.rekor_key.sign(set_payload.as_bytes()).unwrap();

        // A one-leaf tree: the root is the leaf hash and the proof is empty.
        let mut leaf = vec![0u8];
        leaf.extend(body.as_bytes());
        let root_hash = B64.encode(sha256(&leaf));
        let note = format!("{CHECKPOINT_ORIGIN}\n1\n{root_hash}\n");
        let mut note_signature = log_id[..4].to_vec();
        note_signature.extend(self.rekor_key.sign(note.as_bytes()).unwrap());
        let checkpoint = format!(
            "{note}\n\u{2014} rekor.test {}\n",
            B64.encode(note_signature)
        );

        serde_json::json!({
            "mediaType": "application/vnd.dev.sigstore.bundle.v0.3+json",
            "verificationMaterial": {
                "certificate": {"rawBytes": B64.encode(&cert)},
                "tlogEntries": [{
                    "logIndex": "0",
                    "logId": {"keyId": B64.encode(log_id)},
                    "kindVersion": {"kind": "hashedrekord", "version": "0.0.1"},
                    "integratedTime": integrated_time.to_string(),
                    "inclusionPromise": {"signedEntryTimestamp": B64.encode(set)},
                    "inclusionProof": {
                        "logIndex": "0",
                        "rootHash": root_hash,
                        "treeSize": "1",
                        "hashes": [],
                        "checkpoint": {"envelope": checkpoint}
                    },
                    "canonicalizedBody": B64.encode(body.as_bytes())
                }],
                "timestampVerificationData": {}
            },
            "messageSignature": {
                "messageDigest": {"algorithm": "SHA2_256", "digest": B64.encode(digest)},
                "signature": B64.encode(signature)
            }
        })
        .to_string()
    }
}
