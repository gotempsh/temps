// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! OpenSSH certificates (`*-cert-v01@openssh.com` lines, PROTOCOL.certkeys).
//! Only the fields up to `valid before` are read; the signature is never
//! checked. Plain SSH keys have no expiry and yield nothing.

use super::{error, label_text, timestamp, ArtifactKind, ExpiringArtifact, Found};
use base64::Engine;

const CERT_SUFFIX: &str = "-cert-v01@openssh.com";

/// Number of length-prefixed public key fields each certificate type carries
/// between the nonce and the serial.
fn key_fields(cert_type: &str) -> Option<usize> {
    Some(match cert_type {
        "ssh-ed25519-cert-v01@openssh.com" => 1,
        "ssh-rsa-cert-v01@openssh.com" => 2,
        "ecdsa-sha2-nistp256-cert-v01@openssh.com"
        | "ecdsa-sha2-nistp384-cert-v01@openssh.com"
        | "ecdsa-sha2-nistp521-cert-v01@openssh.com" => 2,
        "sk-ssh-ed25519-cert-v01@openssh.com" => 2,
        "sk-ecdsa-sha2-nistp256-cert-v01@openssh.com" => 3,
        "ssh-dss-cert-v01@openssh.com" => 4,
        _ => return None,
    })
}

pub(super) fn from_text(text: &str) -> Found {
    let mut found = Found::default();
    let mut position = 0;
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let Some(cert_type) = fields.next().filter(|t| t.ends_with(CERT_SUFFIX)) else {
            continue;
        };
        position += 1;
        let invalid = |reason: &str| {
            error(
                "ssh_certificate_invalid",
                format!("SSH certificate {position} {reason}."),
            )
        };
        let Some(Ok(blob)) = fields
            .next()
            .map(|b| base64::engine::general_purpose::STANDARD.decode(b))
        else {
            found.findings.push(invalid("is not valid base64"));
            continue;
        };
        match parse(cert_type, &blob) {
            Ok(Some(artifact)) => found.artifacts.push(artifact),
            Ok(None) => {}
            Err(reason) => found.findings.push(invalid(reason)),
        }
    }
    found
}

/// `Ok(None)` for certificates valid forever.
fn parse(cert_type: &str, blob: &[u8]) -> Result<Option<ExpiringArtifact>, &'static str> {
    const TRUNCATED: &str = "is truncated";
    let fields = key_fields(cert_type).ok_or("has an unsupported key type")?;
    let mut reader = Reader(blob);
    if reader.string().ok_or(TRUNCATED)? != cert_type.as_bytes() {
        return Err("does not match its declared key type");
    }
    reader.string().ok_or(TRUNCATED)?; // nonce
    for _ in 0..fields {
        reader.string().ok_or(TRUNCATED)?;
    }
    let serial = reader.u64().ok_or(TRUNCATED)?;
    let role = match reader.u32().ok_or(TRUNCATED)? {
        1 => "user",
        2 => "host",
        _ => return Err("has an unknown certificate type"),
    };
    let key_id = label_text(&String::from_utf8_lossy(reader.string().ok_or(TRUNCATED)?));
    reader.string().ok_or(TRUNCATED)?; // principals
    let valid_after = reader.u64().ok_or(TRUNCATED)?;
    let valid_before = reader.u64().ok_or(TRUNCATED)?;
    // u64::MAX is "forever"; anything past i64 is no practical expiry either.
    let Some(expires_at) = i64::try_from(valid_before).ok().and_then(timestamp) else {
        return Ok(None);
    };
    let label = if key_id.is_empty() {
        format!("SSH {role} certificate (serial {serial})")
    } else {
        format!("SSH {role} certificate '{key_id}'")
    };
    Ok(Some(ExpiringArtifact {
        kind: ArtifactKind::SshCertificate,
        label,
        not_before: i64::try_from(valid_after)
            .ok()
            .filter(|t| *t > 0)
            .and_then(timestamp),
        expires_at,
    }))
}

struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Some(head)
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_be_bytes(self.take(8)?.try_into().ok()?))
    }
    fn string(&mut self) -> Option<&'a [u8]> {
        let len = usize::try_from(self.u32()?).ok()?;
        self.take(len)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{at, verify};
    use super::super::*;
    use crate::verification::CheckStatus;
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;

    fn string(out: &mut Vec<u8>, bytes: &[u8]) {
        out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        out.extend_from_slice(bytes);
    }
    /// The layout `ssh-keygen -s` writes; trailing fields are irrelevant here.
    fn certificate(role: u32, key_id: &str, valid_after: u64, valid_before: u64) -> String {
        let cert_type = "ssh-ed25519-cert-v01@openssh.com";
        let mut blob = Vec::new();
        string(&mut blob, cert_type.as_bytes());
        string(&mut blob, &[7; 32]); // nonce
        string(&mut blob, &[9; 32]); // ed25519 public key
        blob.extend_from_slice(&42u64.to_be_bytes());
        blob.extend_from_slice(&role.to_be_bytes());
        string(&mut blob, key_id.as_bytes());
        string(&mut blob, b"\x00\x00\x00\x06deploy");
        blob.extend_from_slice(&valid_after.to_be_bytes());
        blob.extend_from_slice(&valid_before.to_be_bytes());
        for _ in 0..5 {
            string(&mut blob, b""); // options, extensions, reserved, CA key, signature
        }
        format!("{cert_type} {} deploy@build-runner", STANDARD.encode(blob))
    }

    #[test]
    fn reads_validity_window_and_names_the_certificate() {
        let value = certificate(
            1,
            "deploy-key",
            at("2026-01-01").timestamp() as u64,
            at("2026-09-24").timestamp() as u64,
        );
        let inspection = inspect(&value);
        assert_eq!(inspection.artifacts.len(), 1);
        let artifact = &inspection.artifacts[0];
        assert_eq!(artifact.kind, ArtifactKind::SshCertificate);
        assert_eq!(artifact.label, "SSH user certificate 'deploy-key'");
        assert_eq!(artifact.not_before, Some(at("2026-01-01")));
        assert_eq!(artifact.expires_at, at("2026-09-24"));
        assert_eq!(
            verify(&value).findings.last().unwrap().code,
            "expires_within_7_days"
        );
        let host = certificate(2, "", 0, at("2027-01-01").timestamp() as u64);
        assert_eq!(
            inspect(&host).artifacts[0].label,
            "SSH host certificate (serial 42)"
        );
    }

    #[test]
    fn certificates_valid_forever_and_plain_keys_yield_nothing() {
        assert!(inspect(&certificate(1, "forever", 0, u64::MAX))
            .artifacts
            .is_empty());
        let plain = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA ci@example.test";
        assert_eq!(verify(plain).findings[0].code, "credential_not_inspectable");
    }

    #[test]
    fn certificates_next_to_a_private_key_are_found() {
        let value = format!(
            "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----\n{}\n",
            certificate(1, "ci", 0, at("2027-01-01").timestamp() as u64)
        );
        assert_eq!(verify(&value).status, CheckStatus::Healthy);
    }

    #[test]
    fn malformed_certificates_are_reported() {
        let truncated = {
            let full = certificate(1, "ci", 0, at("2027-01-01").timestamp() as u64);
            let (cert_type, blob) = full.split_once(' ').unwrap();
            let bytes = STANDARD.decode(blob.split(' ').next().unwrap()).unwrap();
            format!("{cert_type} {}", STANDARD.encode(&bytes[..90]))
        };
        for value in [
            "ssh-ed25519-cert-v01@openssh.com !!!!".to_owned(),
            "ssh-ed25519-cert-v01@openssh.com".to_owned(),
            truncated,
            format!(
                "ssh-rsa-cert-v01@openssh.com {}",
                STANDARD.encode(b"\x00\x00\x00\x0bssh-ed25519")
            ),
            format!("ssh-future-cert-v01@openssh.com {}", STANDARD.encode(b"x")),
        ] {
            let inspection = inspect(&value);
            assert!(inspection.artifacts.is_empty(), "{value}");
            assert_eq!(
                inspection.findings[0].code, "ssh_certificate_invalid",
                "{value}"
            );
        }
    }
}
