// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! OpenPGP keys (RFC 9580), armored or binary, public or private. Only packet
//! framing, key creation times and self-signature subpackets are read: key
//! material is never interpreted and signatures are never verified, so the
//! passphrase of an encrypted private key is not needed. The expiry reported is
//! the one OpenPGP implementations enforce:
//!
//! - the primary key's from its newest self-signature (user ID certification or
//!   direct-key signature);
//! - each subkey's from its newest binding signature, unless another live key
//!   with an overlapping capability outlasts it (a replaced, expired subkey is
//!   history, not a problem);
//! - a key revocation is reported as an error.
//!
//! Only version 4 and 6 keys are read; older and LibrePGP version 5 keys are
//! reported as unsupported rather than guessed at. Armored text declares itself
//! to be a key, but binary input (a base64-encoded export) must prove it with a
//! signature naming the key's own ID as issuer, so a random base64 secret is
//! never mistaken for a key.

use super::{armor, error, timestamp, ArtifactKind, ExpiringArtifact, Found};
use crate::verification::{CheckStatus, Finding};
use base64::Engine;
use chrono::{DateTime, Utc};
use sha1::{Digest, Sha1};
use sha2::Sha256;

const ARMOR_LABELS: [&str; 3] = [
    "PGP PUBLIC KEY BLOCK",
    "PGP PRIVATE KEY BLOCK",
    "PGP SECRET KEY BLOCK",
];
const MAX_PACKETS: usize = 4096;
/// Signing, both encryption usages and authentication (RFC 9580 §5.2.3.29).
const USAGE_FLAGS: u8 = 0x02 | 0x04 | 0x08 | 0x20;

pub(super) fn from_armored_text(text: &str) -> Found {
    let mut found = Found::default();
    let mut position = 0;
    for block in armor::blocks(text) {
        if !ARMOR_LABELS.contains(&block.label) {
            continue;
        }
        position += 1;
        let invalid = |reason: &str| {
            error(
                "openpgp_invalid",
                format!("OpenPGP block {position} {reason}."),
            )
        };
        if !block.terminated {
            found.findings.push(invalid("is not terminated"));
            continue;
        }
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&block.body) else {
            found.findings.push(invalid("is not valid base64"));
            continue;
        };
        if let Some(checksum) = block.checksum {
            let expected = base64::engine::general_purpose::STANDARD.decode(checksum);
            if expected.as_deref() != Ok(&crc24(&bytes).to_be_bytes()[1..]) {
                found.findings.push(invalid("fails its checksum"));
                continue;
            }
        }
        match keys(&bytes, false) {
            Ok(keys_found) => found.extend(keys_found),
            Err(()) => found
                .findings
                .push(invalid("could not be parsed as an OpenPGP key")),
        }
    }
    found
}

/// Binary keys (`gpg --export | base64`). Only bytes that start with a key
/// packet are claimed, so unrelated binary data is never reported as broken.
pub(super) fn from_binary(bytes: &[u8]) -> Found {
    match read_packet(bytes) {
        Ok((packet, _)) if matches!(packet.tag, 5 | 6) => keys(bytes, true).unwrap_or_default(),
        _ => Found::default(),
    }
}

struct Packet<'a> {
    tag: u8,
    body: &'a [u8],
}

fn read_packet(data: &[u8]) -> Result<(Packet<'_>, &[u8]), ()> {
    let mut reader = Reader(data);
    let first = reader.u8().ok_or(())?;
    if first & 0x80 == 0 {
        return Err(());
    }
    let (tag, len) = if first & 0x40 != 0 {
        let tag = first & 0x3f;
        let len = match reader.u8().ok_or(())? {
            octet @ 0..=191 => usize::from(octet),
            octet @ 192..=223 => {
                ((usize::from(octet) - 192) << 8) + usize::from(reader.u8().ok_or(())?) + 192
            }
            255 => reader.u32().ok_or(())? as usize,
            // Partial body lengths only appear in data packets, never in keys.
            _ => return Err(()),
        };
        (tag, len)
    } else {
        let tag = (first >> 2) & 0x0f;
        let len = match first & 0x03 {
            0 => usize::from(reader.u8().ok_or(())?),
            1 => usize::from(reader.u16().ok_or(())?),
            2 => reader.u32().ok_or(())? as usize,
            // Indeterminate length runs to the end of the input; keys never use it.
            _ => return Err(()),
        };
        (tag, len)
    };
    let body = reader.take(len).ok_or(())?;
    Ok((Packet { tag, body }, reader.0))
}

struct Key {
    version: u8,
    /// `None` when the key ID cannot be derived (v2/v3, or a private key with
    /// an algorithm this reader does not know).
    id: Option<[u8; 8]>,
    created: i64,
    signatures: Vec<Signature>,
}
struct Signature {
    kind: u8,
    created: i64,
    key_expiration: Option<u32>,
    issuer: Option<[u8; 8]>,
    flags: Option<u8>,
}
struct TransferableKey {
    primary: Key,
    subkeys: Vec<Key>,
}

fn keys(data: &[u8], binary: bool) -> Result<Found, ()> {
    let mut transferable: Vec<TransferableKey> = Vec::new();
    // Signatures attach to the most recent key; certifications after a user ID
    // belong to the primary key.
    let mut on_subkey = false;
    let mut rest = data;
    let mut count = 0;
    while !rest.is_empty() {
        count += 1;
        if count > MAX_PACKETS {
            return Err(());
        }
        let (packet, remaining) = read_packet(rest)?;
        rest = remaining;
        match packet.tag {
            5 | 6 => {
                transferable.push(TransferableKey {
                    primary: key(&packet)?,
                    subkeys: Vec::new(),
                });
                on_subkey = false;
            }
            7 | 14 => {
                let current = transferable.last_mut().ok_or(())?;
                current.subkeys.push(key(&packet)?);
                on_subkey = true;
            }
            13 | 17 => on_subkey = false,
            2 => {
                let Some(current) = transferable.last_mut() else {
                    return Err(());
                };
                let Some(signature) = signature(packet.body) else {
                    continue;
                };
                let target = if on_subkey {
                    current.subkeys.last_mut().ok_or(())?
                } else {
                    &mut current.primary
                };
                target.signatures.push(signature);
            }
            _ => {}
        }
    }
    if transferable.is_empty() {
        return Err(());
    }
    let mut found = Found::default();
    for key in &transferable {
        evaluate(key, binary, &mut found);
    }
    Ok(found)
}

fn evaluate(transferable: &TransferableKey, binary: bool, found: &mut Found) {
    let primary = &transferable.primary;
    // Binary input only counts as a key when a signature names the key's own ID.
    let by_primary = |signature: &&Signature| match (signature.issuer, primary.id) {
        (Some(issuer), Some(id)) => issuer == id,
        _ => !binary,
    };
    if binary && !primary.signatures.iter().any(|s| by_primary(&s)) {
        return;
    }
    let primary_label = format!("OpenPGP key {}", key_name(primary));
    if !matches!(primary.version, 4 | 6) {
        found.findings.push(Finding {
            code: "openpgp_unsupported_version".into(),
            status: CheckStatus::Unknown,
            message: format!(
                "{primary_label} uses key version {}; only version 4 and 6 keys can be read.",
                primary.version
            ),
        });
        return;
    }
    if primary
        .signatures
        .iter()
        .filter(by_primary)
        .any(|s| s.kind == 0x20)
    {
        found.findings.push(error(
            "openpgp_key_revoked",
            format!("{primary_label} has been revoked."),
        ));
        return;
    }
    let self_signature = primary
        .signatures
        .iter()
        .filter(by_primary)
        .filter(|s| matches!(s.kind, 0x10..=0x13 | 0x1f))
        .max_by_key(|s| s.created);
    let primary_expiry = self_signature.and_then(|s| expiry(primary.created, s.key_expiration));
    // A primary key without key flags conventionally certifies and signs.
    let mut usable: Vec<(Option<DateTime<Utc>>, u8)> = vec![(
        primary_expiry,
        self_signature.and_then(|s| s.flags).unwrap_or(0x03),
    )];
    let mut subkeys = Vec::new();
    for subkey in &transferable.subkeys {
        let bindings: Vec<&Signature> = subkey.signatures.iter().filter(by_primary).collect();
        if bindings.iter().any(|s| s.kind == 0x28) {
            continue; // revoked subkeys are retired on purpose
        }
        let Some(binding) = bindings
            .iter()
            .filter(|s| s.kind == 0x18)
            .max_by_key(|s| s.created)
        else {
            continue; // an unbound subkey is not part of the key
        };
        let subkey_expiry = expiry(subkey.created, binding.key_expiration);
        let flags = binding.flags.unwrap_or(USAGE_FLAGS);
        usable.push((subkey_expiry, flags));
        subkeys.push((subkey, subkey_expiry, flags));
    }
    if let Some(expires_at) = primary_expiry {
        found
            .artifacts
            .push(artifact(primary_label.clone(), primary.created, expires_at));
    }
    for (index, (subkey, subkey_expiry, flags)) in subkeys.iter().enumerate() {
        let Some(expires_at) = *subkey_expiry else {
            continue;
        };
        // A subkey is history only when the keys that outlive it, together,
        // still provide every capability it has. A capability no other key
        // keeps is about to stop working, so the subkey is reported.
        let capabilities = flags & USAGE_FLAGS;
        let covered = usable
            .iter()
            .enumerate()
            .filter(|(other, (other_expiry, _))| {
                *other != index + 1
                    && other_expiry.is_none_or(|other_expiry| other_expiry > expires_at)
            })
            .fold(0, |covered, (_, (_, other_flags))| covered | other_flags);
        let replaced = capabilities != 0 && capabilities & !covered == 0;
        if !replaced {
            found.artifacts.push(artifact(
                format!(
                    "OpenPGP subkey {} of key {}",
                    key_name(subkey),
                    key_name(primary)
                ),
                subkey.created,
                expires_at,
            ));
        }
    }
}

fn artifact(label: String, created: i64, expires_at: DateTime<Utc>) -> ExpiringArtifact {
    ExpiringArtifact {
        kind: ArtifactKind::OpenpgpKey,
        label,
        not_before: timestamp(created),
        expires_at,
    }
}

fn expiry(created: i64, key_expiration: Option<u32>) -> Option<DateTime<Utc>> {
    key_expiration
        .filter(|seconds| *seconds > 0)
        .and_then(|seconds| timestamp(created + i64::from(seconds)))
}

fn key_name(key: &Key) -> String {
    match key.id {
        Some(id) => format!(
            "0x{}",
            id.iter().map(|b| format!("{b:02X}")).collect::<String>()
        ),
        None => match timestamp(key.created) {
            Some(created) => format!("created {}", created.format("%Y-%m-%d")),
            None => "with an unknown ID".into(),
        },
    }
}

fn key(packet: &Packet<'_>) -> Result<Key, ()> {
    let body = packet.body;
    let mut reader = Reader(body);
    let version = reader.u8().ok_or(())?;
    let created = i64::from(reader.u32().ok_or(())?);
    let public_key_packet = matches!(packet.tag, 6 | 14);
    let id = match version {
        4 => {
            let algorithm = reader.u8().ok_or(())?;
            let public = if public_key_packet {
                Some(body)
            } else {
                public_material_len(algorithm, reader.0).map(|len| &body[..6 + len])
            };
            public.map(|public| {
                let mut hasher = Sha1::new();
                hasher.update([0x99]);
                hasher.update((public.len() as u16).to_be_bytes());
                hasher.update(public);
                let digest = hasher.finalize();
                let mut id = [0; 8];
                id.copy_from_slice(&digest[12..20]);
                id
            })
        }
        5 | 6 => {
            reader.u8().ok_or(())?; // algorithm
            let material = reader.u32().ok_or(())? as usize;
            let public = body.get(..10 + material).ok_or(())?;
            let mut hasher = Sha256::new();
            hasher.update([if version == 6 { 0x9b } else { 0x9a }]);
            hasher.update((public.len() as u32).to_be_bytes());
            hasher.update(public);
            let digest = hasher.finalize();
            let mut id = [0; 8];
            id.copy_from_slice(&digest[..8]);
            Some(id)
        }
        2 | 3 => None,
        _ => return Err(()),
    };
    Ok(Key {
        version,
        id,
        created,
        signatures: Vec::new(),
    })
}

/// Length of v4 public key material, so a private key's key ID can be derived
/// from its public part (RFC 9580 §5.5.5).
fn public_material_len(algorithm: u8, material: &[u8]) -> Option<usize> {
    let mut reader = Reader(material);
    match algorithm {
        1..=3 => reader.mpis(2)?,
        16 => reader.mpis(3)?,
        17 => reader.mpis(4)?,
        18 => {
            reader.prefixed()?; // curve OID
            reader.mpis(1)?;
            reader.prefixed()?; // KDF parameters
        }
        19 | 22 => {
            reader.prefixed()?;
            reader.mpis(1)?;
        }
        25 | 27 => {
            reader.take(32)?;
        }
        26 => {
            reader.take(56)?;
        }
        28 => {
            reader.take(57)?;
        }
        _ => return None,
    }
    Some(material.len() - reader.0.len())
}

/// `None` for signature versions this reader does not understand; they are skipped.
fn signature(body: &[u8]) -> Option<Signature> {
    let mut reader = Reader(body);
    let version = reader.u8()?;
    if version == 3 {
        reader.u8()?; // hashed material length, always 5
        let kind = reader.u8()?;
        let created = i64::from(reader.u32()?);
        let issuer = reader.take(8)?.try_into().ok();
        return Some(Signature {
            kind,
            created,
            key_expiration: None,
            issuer,
            flags: None,
        });
    }
    if !matches!(version, 4 | 6) {
        return None;
    }
    let kind = reader.u8()?;
    reader.take(2)?; // public key and hash algorithms
    let wide = version == 6;
    let hashed_len = if wide {
        reader.u32()? as usize
    } else {
        usize::from(reader.u16()?)
    };
    let hashed = reader.take(hashed_len)?;
    let unhashed_len = if wide {
        reader.u32()? as usize
    } else {
        usize::from(reader.u16()?)
    };
    let unhashed = reader.take(unhashed_len)?;
    let mut signature = Signature {
        kind,
        created: 0,
        key_expiration: None,
        issuer: None,
        flags: None,
    };
    for (area, hashed_area) in [(hashed, true), (unhashed, false)] {
        let mut subpackets = Reader(area);
        while !subpackets.0.is_empty() {
            let len = match subpackets.u8()? {
                octet @ 0..=191 => usize::from(octet),
                octet @ 192..=254 => {
                    ((usize::from(octet) - 192) << 8) + usize::from(subpackets.u8()?) + 192
                }
                255 => subpackets.u32()? as usize,
            };
            let data = subpackets.take(len)?;
            let Some((&kind, data)) = data.split_first() else {
                continue;
            };
            let mut data = Reader(data);
            match (kind & 0x7f, hashed_area) {
                (2, true) => signature.created = i64::from(data.u32()?),
                (9, true) => signature.key_expiration = Some(data.u32()?),
                (27, true) => signature.flags = data.u8(),
                (16, _) => signature.issuer = data.take(8)?.try_into().ok(),
                (33, _) => {
                    signature.issuer = match (data.u8()?, data.0.len()) {
                        (4, 20) => data.0[12..].try_into().ok(),
                        (5 | 6, 32) => data.0[..8].try_into().ok(),
                        _ => signature.issuer,
                    }
                }
                _ => {}
            }
        }
    }
    Some(signature)
}

fn crc24(data: &[u8]) -> u32 {
    let mut crc: u32 = 0x00b7_04ce;
    for &byte in data {
        crc ^= u32::from(byte) << 16;
        for _ in 0..8 {
            crc <<= 1;
            if crc & 0x0100_0000 != 0 {
                crc ^= 0x0186_4cfb;
            }
        }
    }
    crc & 0x00ff_ffff
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
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }
    /// One-octet length followed by that many bytes (curve OIDs, KDF parameters).
    fn prefixed(&mut self) -> Option<&'a [u8]> {
        let len = usize::from(self.u8()?);
        self.take(len)
    }
    fn mpis(&mut self, count: usize) -> Option<()> {
        for _ in 0..count {
            let bits = usize::from(self.u16()?);
            self.take(bits.div_ceil(8))?;
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{at, verify};
    use super::super::*;
    use super::{crc24, Digest, Sha1};
    use crate::verification::CheckStatus;
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;

    const DAY: u32 = 86_400;

    fn packet(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![0xc0 | tag];
        match body.len() {
            len @ 0..=191 => out.push(len as u8),
            len => {
                let len = len - 192;
                out.push((len >> 8) as u8 + 192);
                out.push(len as u8);
            }
        }
        out.extend_from_slice(body);
        out
    }
    /// A v4 Ed25519 public key body; the key material is filler.
    fn public_body(created: i64, fill: u8) -> Vec<u8> {
        let mut body = vec![4];
        body.extend_from_slice(&(created as u32).to_be_bytes());
        body.push(27);
        body.extend_from_slice(&[fill; 32]);
        body
    }
    fn fingerprint(public_body: &[u8]) -> [u8; 20] {
        let mut hasher = Sha1::new();
        hasher.update([0x99]);
        hasher.update((public_body.len() as u16).to_be_bytes());
        hasher.update(public_body);
        hasher.finalize().into()
    }
    struct Sig {
        kind: u8,
        created: i64,
        key_expiration: Option<u32>,
        flags: Option<u8>,
        issuer: Option<[u8; 20]>,
    }
    fn signature(sig: Sig) -> Vec<u8> {
        let mut hashed = vec![5, 2];
        hashed.extend_from_slice(&(sig.created as u32).to_be_bytes());
        if let Some(seconds) = sig.key_expiration {
            hashed.extend_from_slice(&[5, 9]);
            hashed.extend_from_slice(&seconds.to_be_bytes());
        }
        if let Some(flags) = sig.flags {
            hashed.extend_from_slice(&[2, 27, flags]);
        }
        if let Some(issuer) = sig.issuer {
            hashed.extend_from_slice(&[22, 33, 4]);
            hashed.extend_from_slice(&issuer);
        }
        let mut body = vec![4, sig.kind, 27, 8];
        body.extend_from_slice(&(hashed.len() as u16).to_be_bytes());
        body.extend_from_slice(&hashed);
        body.extend_from_slice(&0u16.to_be_bytes());
        body.extend_from_slice(&[0xab, 0xcd]);
        body.extend_from_slice(&[0x11; 64]);
        packet(2, &body)
    }
    fn armor(bytes: &[u8], label: &str) -> String {
        let body = STANDARD.encode(bytes);
        let lines: Vec<&str> = body
            .as_bytes()
            .chunks(64)
            .map(|chunk| std::str::from_utf8(chunk).unwrap())
            .collect();
        let checksum = STANDARD.encode(&crc24(bytes).to_be_bytes()[1..]);
        format!(
            "-----BEGIN {label}-----\nComment: generated in test\n\n{}\n={checksum}\n-----END {label}-----\n",
            lines.join("\n")
        )
    }
    fn hex(fingerprint: &[u8; 20]) -> String {
        fingerprint[12..]
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect()
    }

    struct KeyBuilder {
        bytes: Vec<u8>,
        primary: [u8; 20],
        created: i64,
    }
    impl KeyBuilder {
        fn new(created: i64) -> Self {
            let body = public_body(created, 1);
            let mut bytes = packet(6, &body);
            bytes.extend(packet(13, b"Test Key <key@example.test>"));
            Self {
                bytes,
                primary: fingerprint(&body),
                created,
            }
        }
        fn self_signature(mut self, created: i64, expires_after: Option<u32>) -> Self {
            self.bytes.extend(signature(Sig {
                kind: 0x13,
                created,
                key_expiration: expires_after,
                flags: Some(0x03),
                issuer: Some(self.primary),
            }));
            self
        }
        fn subkey(mut self, fill: u8, expires_after: Option<u32>, flags: u8) -> Self {
            let body = public_body(self.created, fill);
            self.bytes.extend(packet(14, &body));
            self.bytes.extend(signature(Sig {
                kind: 0x18,
                created: self.created,
                key_expiration: expires_after,
                flags: Some(flags),
                issuer: Some(self.primary),
            }));
            self
        }
        fn raw(mut self, bytes: Vec<u8>) -> Self {
            self.bytes.extend(bytes);
            self
        }
        fn armored(&self) -> String {
            armor(&self.bytes, "PGP PUBLIC KEY BLOCK")
        }
    }

    fn created() -> i64 {
        at("2025-09-21").timestamp()
    }
    fn days_until(date: &str) -> u32 {
        ((at(date).timestamp() - created()) / i64::from(DAY)) as u32 * DAY
    }

    #[test]
    fn primary_key_expiry_comes_from_its_self_signature() {
        let key =
            KeyBuilder::new(created()).self_signature(created(), Some(days_until("2026-09-26")));
        let inspection = inspect(&key.armored());
        assert_eq!(inspection.artifacts.len(), 1, "{:?}", inspection.findings);
        let artifact = &inspection.artifacts[0];
        assert_eq!(artifact.kind, ArtifactKind::OpenpgpKey);
        assert_eq!(
            artifact.label,
            format!("OpenPGP key 0x{}", hex(&key.primary))
        );
        assert_eq!(artifact.expires_at, at("2026-09-26"));
        assert_eq!(
            verify(&key.armored()).findings.last().unwrap().code,
            "expires_within_7_days"
        );
        assert!(!serde_json::to_string(&inspection.artifacts)
            .unwrap()
            .contains("key@example.test"));
    }

    #[test]
    fn the_newest_self_signature_wins_and_never_expiring_keys_yield_nothing() {
        let extended = KeyBuilder::new(created())
            .self_signature(created(), Some(days_until("2026-09-26")))
            .self_signature(created() + 1000, Some(days_until("2028-01-01")));
        assert_eq!(
            inspect(&extended.armored()).artifacts[0].expires_at,
            at("2028-01-01")
        );
        let cleared = KeyBuilder::new(created())
            .self_signature(created(), Some(days_until("2026-09-26")))
            .self_signature(created() + 1000, None);
        assert!(!has_expiring_artifact(&cleared.armored()));
    }

    #[test]
    fn third_party_certifications_do_not_override_the_self_signature() {
        let key =
            KeyBuilder::new(created()).self_signature(created(), Some(days_until("2026-09-26")));
        let primary = key.primary;
        let key = key.raw(signature(Sig {
            kind: 0x10,
            created: created() + 5000,
            key_expiration: None,
            flags: None,
            issuer: Some([0x55; 20]),
        }));
        let inspection = inspect(&key.armored());
        assert_eq!(inspection.artifacts.len(), 1);
        assert!(inspection.artifacts[0].label.contains(&hex(&primary)));
    }

    #[test]
    fn subkeys_expiring_first_are_reported_unless_a_live_one_replaces_them() {
        let key = KeyBuilder::new(created())
            .self_signature(created(), None)
            .subkey(2, Some(days_until("2026-09-24")), 0x0c);
        let inspection = inspect(&key.armored());
        assert_eq!(inspection.artifacts.len(), 1);
        assert!(inspection.artifacts[0]
            .label
            .starts_with("OpenPGP subkey 0x"));
        assert_eq!(verify(&key.armored()).status, CheckStatus::Warning);

        let replaced = KeyBuilder::new(created())
            .self_signature(created(), None)
            .subkey(2, Some(days_until("2026-01-01")), 0x0c)
            .subkey(3, Some(days_until("2028-01-01")), 0x0c);
        let inspection = inspect(&replaced.armored());
        assert_eq!(inspection.artifacts.len(), 1);
        assert_eq!(inspection.artifacts[0].expires_at, at("2028-01-01"));
        assert_eq!(verify(&replaced.armored()).status, CheckStatus::Healthy);
    }

    #[test]
    fn a_subkey_is_replaced_only_when_every_capability_lives_on() {
        // Encryption and authentication expiring, with only authentication kept
        // by a longer-lived subkey: encryption is about to stop working.
        let partly = KeyBuilder::new(created())
            .self_signature(created(), None)
            .subkey(2, Some(days_until("2026-09-24")), 0x2c)
            .subkey(3, Some(days_until("2028-01-01")), 0x20);
        let inspection = inspect(&partly.armored());
        assert_eq!(inspection.artifacts.len(), 2);
        assert!(inspection
            .artifacts
            .iter()
            .any(|a| a.expires_at == at("2026-09-24")));
        assert_eq!(verify(&partly.armored()).status, CheckStatus::Warning);

        // Two longer-lived subkeys that together keep both capabilities.
        let covered = KeyBuilder::new(created())
            .self_signature(created(), None)
            .subkey(2, Some(days_until("2026-01-01")), 0x2c)
            .subkey(3, Some(days_until("2028-01-01")), 0x0c)
            .subkey(4, Some(days_until("2028-01-01")), 0x20);
        let inspection = inspect(&covered.armored());
        assert_eq!(inspection.artifacts.len(), 2);
        assert!(inspection
            .artifacts
            .iter()
            .all(|a| a.expires_at == at("2028-01-01")));
        assert_eq!(verify(&covered.armored()).status, CheckStatus::Healthy);
    }

    #[test]
    fn revoked_keys_are_errors() {
        let key = KeyBuilder::new(created()).self_signature(created(), None);
        let primary = key.primary;
        let key = key.raw(signature(Sig {
            kind: 0x20,
            created: created() + 10,
            key_expiration: None,
            flags: None,
            issuer: Some(primary),
        }));
        let result = verify(&key.armored());
        assert_eq!(result.status, CheckStatus::Error);
        assert_eq!(result.findings[0].code, "openpgp_key_revoked");
    }

    #[test]
    fn private_keys_yield_expiry_without_the_passphrase() {
        let public = public_body(created(), 1);
        let primary = fingerprint(&public);
        let mut secret = public.clone();
        secret.extend_from_slice(&[254, 9, 3, 8]); // encrypted with an S2K specifier
        secret.extend_from_slice(&[0x77; 40]);
        let mut bytes = packet(5, &secret);
        bytes.extend(packet(13, b"Test Key <key@example.test>"));
        bytes.extend(signature(Sig {
            kind: 0x13,
            created: created(),
            key_expiration: Some(days_until("2027-01-01")),
            flags: Some(0x03),
            issuer: Some(primary),
        }));
        let armored = armor(&bytes, "PGP PRIVATE KEY BLOCK");
        let inspection = inspect(&armored);
        assert_eq!(inspection.artifacts.len(), 1);
        assert!(inspection.artifacts[0].label.contains(&hex(&primary)));
        // `gpg --export-secret-keys | base64`, as CI systems store signing keys.
        assert_eq!(inspect(&STANDARD.encode(&bytes)).artifacts.len(), 1);
        let serialized = serde_json::to_string(&verify(&armored)).unwrap();
        assert!(!serialized.contains("PRIVATE"));
    }

    #[test]
    fn corrupt_blocks_are_reported() {
        let key =
            KeyBuilder::new(created()).self_signature(created(), Some(days_until("2027-01-01")));
        let armored = key.armored();
        let checksum_line = armored.lines().find(|l| l.starts_with('=')).unwrap();
        let bad_checksum = armored.replace(checksum_line, "=AAAA");
        let garbage = "-----BEGIN PGP PUBLIC KEY BLOCK-----\n\nAAAAAAAA\n-----END PGP PUBLIC KEY BLOCK-----\n";
        let unterminated = "-----BEGIN PGP PUBLIC KEY BLOCK-----\n\nAAAA\n";
        for value in [bad_checksum.as_str(), garbage, unterminated] {
            let inspection = inspect(value);
            assert!(inspection.artifacts.is_empty(), "{value}");
            assert_eq!(inspection.findings[0].code, "openpgp_invalid", "{value}");
        }
    }
    #[test]
    fn random_base64_values_are_not_read_as_keys() {
        // Decodes to an old-format key packet of indeterminate length with a
        // v3 version byte: once read as an expired key, leaking six bytes of
        // the secret as dates.
        let reported = "lwNgAAAAABAUbA9SOKjmn4YlZFavtMNI5a47wCCivyo=";
        let inspection = inspect(reported);
        assert!(inspection.artifacts.is_empty());
        assert!(inspection.findings.is_empty());
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        for first in [0x94, 0x95, 0x97, 0x98, 0x99, 0x9b, 0xc5, 0xc6] {
            for _ in 0..2000 {
                let mut bytes = vec![first];
                bytes.extend((0..31).map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    state as u8
                }));
                let value = STANDARD.encode(&bytes);
                let inspection = inspect(&value);
                assert!(inspection.artifacts.is_empty(), "{value}");
                assert!(inspection.findings.is_empty(), "{value}");
            }
        }
    }

    #[test]
    fn binary_keys_must_name_themselves_as_issuer() {
        let body = public_body(created(), 1);
        let mut bytes = packet(6, &body);
        bytes.extend(packet(13, b"Test Key <key@example.test>"));
        bytes.extend(signature(Sig {
            kind: 0x13,
            created: created(),
            key_expiration: Some(days_until("2027-01-01")),
            flags: Some(0x03),
            issuer: None,
        }));
        // Armored text is explicitly a key, so an issuer-less self-signature counts.
        assert_eq!(
            inspect(&armor(&bytes, "PGP PUBLIC KEY BLOCK"))
                .artifacts
                .len(),
            1
        );
        assert!(inspect(&STANDARD.encode(&bytes)).artifacts.is_empty());
    }

    #[test]
    fn unsupported_key_versions_are_reported_not_guessed() {
        let mut body = vec![5];
        body.extend_from_slice(&(created() as u32).to_be_bytes());
        body.push(27);
        body.extend_from_slice(&32u32.to_be_bytes());
        body.extend_from_slice(&[1; 32]);
        let bytes = packet(6, &body);
        let result = verify(&armor(&bytes, "PGP PUBLIC KEY BLOCK"));
        assert_eq!(result.status, CheckStatus::Unknown);
        assert_eq!(result.findings[0].code, "openpgp_unsupported_version");
        assert!(inspect(&STANDARD.encode(&bytes)).findings.is_empty());
    }
}
