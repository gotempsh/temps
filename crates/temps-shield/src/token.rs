// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Wire formats (ADR 066, I7). Integers are big-endian, `mac` is
//! HMAC-SHA256 truncated to 16 bytes, and strings are base64url without
//! padding.
//!
//! ```text
//! challenge token (60 bytes → 80 chars)
//!   ver u8 = 1 | kid u8 | env_id u32 | identity [16] | difficulty u8
//!   | flags u8 | issued_at u32 | salt [16] | mac [16]
//!
//! clearance (56 bytes → 75 chars)
//!   ver u8 = 1 | kid u8 | env_id u32 | identity [16] | issued_at u32
//!   | expires_at u32 | budget_id [8] | flags u8 | reserved u8 | mac [16]
//! ```

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

use crate::identity::VisitorIdentity;
use crate::keys::ShieldKeys;

type HmacSha256 = Hmac<Sha256>;

const VERSION: u8 = 1;
const MAC_LEN: usize = 16;
const CHALLENGE_LEN: usize = 60;
const CLEARANCE_LEN: usize = 56;

/// How long a challenge token can be solved and submitted.
pub const CHALLENGE_TTL_SECS: u64 = 5 * 60;
/// How long a clearance cookie is valid.
pub const CLEARANCE_TTL_SECS: u64 = 24 * 60 * 60;
/// Tolerated clock difference between the ingress that signs and the one
/// that verifies.
pub(crate) const CLOCK_SKEW_SECS: u64 = 30;
/// A nonce is a decimal `u64`; anything longer is not one.
const MAX_NONCE_LEN: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ChallengeError {
    #[error("the token is malformed")]
    Malformed,
    #[error("the token signature does not verify")]
    BadSignature,
    #[error("the token has expired")]
    Expired,
    #[error("the token was issued for another environment")]
    WrongEnvironment,
    #[error("the token was issued to another visitor")]
    WrongVisitor,
    #[error("the nonce is not a decimal number")]
    BadNonce,
    #[error("the proof of work does not meet the difficulty")]
    InsufficientWork,
    #[error("the token was already redeemed")]
    Replayed,
    #[error("the signing key was rejected")]
    Key,
}

pub(crate) struct ChallengeClaims {
    pub(crate) env_id: i32,
    pub(crate) identity: VisitorIdentity,
    pub(crate) difficulty: u8,
    pub(crate) issued_at: u64,
    pub(crate) salt: [u8; 16],
}

pub(crate) struct ClearanceClaims {
    pub(crate) env_id: i32,
    pub(crate) identity: VisitorIdentity,
    pub(crate) issued_at: u64,
    pub(crate) expires_at: u64,
    pub(crate) budget_id: [u8; 8],
}

fn mac(key: &[u8; 32], body: &[u8]) -> Result<[u8; MAC_LEN], ChallengeError> {
    let mut mac = HmacSha256::new_from_slice(key).map_err(|_| ChallengeError::Key)?;
    mac.update(body);
    let full = mac.finalize().into_bytes();
    let mut truncated = [0u8; MAC_LEN];
    truncated.copy_from_slice(&full[..MAC_LEN]);
    Ok(truncated)
}

/// Compares in time that depends only on the length.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Seconds as the u32 the formats carry. Unix time fits until 2106.
fn secs_u32(secs: u64) -> [u8; 4] {
    u32::try_from(secs).unwrap_or(u32::MAX).to_be_bytes()
}

fn read_u32(bytes: &[u8]) -> u64 {
    let mut buf = [0u8; 4];
    buf.copy_from_slice(&bytes[..4]);
    u64::from(u32::from_be_bytes(buf))
}

/// Decodes `text` to exactly `len` bytes, checks the version and the MAC
/// under the key the token names, and returns the bytes.
fn open(
    text: &str,
    len: usize,
    keys: &ShieldKeys,
    key_of: impl Fn(&crate::keys::KeyGeneration) -> &[u8; 32],
) -> Result<Vec<u8>, ChallengeError> {
    // base64url of `len` bytes without padding is `ceil(len * 4 / 3)` chars;
    // reject anything else before decoding.
    if text.len() != (len * 4).div_ceil(3) {
        return Err(ChallengeError::Malformed);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|_| ChallengeError::Malformed)?;
    if bytes.len() != len || bytes[0] != VERSION {
        return Err(ChallengeError::Malformed);
    }
    let generation = keys.by_kid(bytes[1]).ok_or(ChallengeError::BadSignature)?;
    let (body, tag) = bytes.split_at(len - MAC_LEN);
    if !constant_time_eq(&mac(key_of(generation), body)?, tag) {
        return Err(ChallengeError::BadSignature);
    }
    Ok(bytes)
}

pub(crate) fn seal_challenge(
    keys: &ShieldKeys,
    claims: &ChallengeClaims,
) -> Result<String, ChallengeError> {
    let generation = keys.current();
    let mut bytes = Vec::with_capacity(CHALLENGE_LEN);
    bytes.push(VERSION);
    bytes.push(generation.kid);
    bytes.extend_from_slice(&claims.env_id.to_be_bytes());
    bytes.extend_from_slice(&claims.identity.0);
    bytes.push(claims.difficulty);
    bytes.push(0); // flags
    bytes.extend_from_slice(&secs_u32(claims.issued_at));
    bytes.extend_from_slice(&claims.salt);
    let tag = mac(&generation.challenge, &bytes)?;
    bytes.extend_from_slice(&tag);
    debug_assert_eq!(bytes.len(), CHALLENGE_LEN);
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

pub(crate) fn open_challenge(
    keys: &ShieldKeys,
    token: &str,
) -> Result<ChallengeClaims, ChallengeError> {
    let bytes = open(token, CHALLENGE_LEN, keys, |g| &g.challenge)?;
    let mut env = [0u8; 4];
    env.copy_from_slice(&bytes[2..6]);
    let mut identity = [0u8; 16];
    identity.copy_from_slice(&bytes[6..22]);
    let mut salt = [0u8; 16];
    salt.copy_from_slice(&bytes[28..44]);
    Ok(ChallengeClaims {
        env_id: i32::from_be_bytes(env),
        identity: VisitorIdentity(identity),
        difficulty: bytes[22],
        issued_at: read_u32(&bytes[24..28]),
        salt,
    })
}

pub(crate) fn seal_clearance(
    keys: &ShieldKeys,
    claims: &ClearanceClaims,
) -> Result<String, ChallengeError> {
    let generation = keys.current();
    let mut bytes = Vec::with_capacity(CLEARANCE_LEN);
    bytes.push(VERSION);
    bytes.push(generation.kid);
    bytes.extend_from_slice(&claims.env_id.to_be_bytes());
    bytes.extend_from_slice(&claims.identity.0);
    bytes.extend_from_slice(&secs_u32(claims.issued_at));
    bytes.extend_from_slice(&secs_u32(claims.expires_at));
    bytes.extend_from_slice(&claims.budget_id);
    bytes.push(0); // flags
    bytes.push(0); // reserved
    let tag = mac(&generation.clearance, &bytes)?;
    bytes.extend_from_slice(&tag);
    debug_assert_eq!(bytes.len(), CLEARANCE_LEN);
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

pub(crate) fn open_clearance(
    keys: &ShieldKeys,
    cookie: &str,
) -> Result<ClearanceClaims, ChallengeError> {
    let bytes = open(cookie, CLEARANCE_LEN, keys, |g| &g.clearance)?;
    let mut env = [0u8; 4];
    env.copy_from_slice(&bytes[2..6]);
    let mut identity = [0u8; 16];
    identity.copy_from_slice(&bytes[6..22]);
    let mut budget_id = [0u8; 8];
    budget_id.copy_from_slice(&bytes[30..38]);
    Ok(ClearanceClaims {
        env_id: i32::from_be_bytes(env),
        identity: VisitorIdentity(identity),
        issued_at: read_u32(&bytes[22..26]),
        expires_at: read_u32(&bytes[26..30]),
        budget_id,
    })
}

/// Whether `issued_at` lies within `ttl` of `now`, allowing for clock skew
/// between ingresses.
pub(crate) fn is_fresh(issued_at: u64, ttl: u64, now: u64) -> bool {
    issued_at <= now.saturating_add(CLOCK_SKEW_SECS) && now < issued_at.saturating_add(ttl)
}

/// Checks the proof of work: `sha256(token ‖ nonce)` must start with
/// `difficulty` zero bits, where `nonce` is the decimal number the solver
/// found. This is the input the `temps-captcha-wasm` solver hashes.
pub(crate) fn check_work(token: &str, nonce: &str, difficulty: u8) -> Result<(), ChallengeError> {
    if nonce.is_empty() || nonce.len() > MAX_NONCE_LEN || !nonce.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(ChallengeError::BadNonce);
    }
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hasher.update(nonce.as_bytes());
    if has_leading_zero_bits(&hasher.finalize(), u32::from(difficulty)) {
        Ok(())
    } else {
        Err(ChallengeError::InsufficientWork)
    }
}

/// Whether `hash` starts with at least `bits` zero bits.
pub fn has_leading_zero_bits(hash: &[u8], bits: u32) -> bool {
    let mut remaining = bits;
    for &byte in hash {
        if remaining == 0 {
            return true;
        }
        if remaining >= 8 {
            if byte != 0 {
                return false;
            }
            remaining -= 8;
        } else {
            return byte.leading_zeros() >= remaining;
        }
    }
    remaining == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(seed: u8) -> ShieldKeys {
        ShieldKeys::derive(|label| {
            let mut hasher = Sha256::new();
            hasher.update([seed]);
            hasher.update(label.as_bytes());
            hasher.finalize().into()
        })
    }

    fn challenge() -> ChallengeClaims {
        ChallengeClaims {
            env_id: 42,
            identity: VisitorIdentity([7; 16]),
            difficulty: 4,
            issued_at: 1_800_000_000,
            salt: [9; 16],
        }
    }

    #[test]
    fn challenge_round_trips_with_the_documented_length() {
        let token = seal_challenge(&keys(1), &challenge()).unwrap();
        assert_eq!(token.len(), 80);
        let claims = open_challenge(&keys(1), &token).unwrap();
        assert_eq!(claims.env_id, 42);
        assert_eq!(claims.identity, VisitorIdentity([7; 16]));
        assert_eq!(claims.difficulty, 4);
        assert_eq!(claims.issued_at, 1_800_000_000);
        assert_eq!(claims.salt, [9; 16]);
    }

    #[test]
    fn clearance_round_trips_with_the_documented_length() {
        let cookie = seal_clearance(
            &keys(1),
            &ClearanceClaims {
                env_id: -3,
                identity: VisitorIdentity([1; 16]),
                issued_at: 1_800_000_000,
                expires_at: 1_800_086_400,
                budget_id: [5; 8],
            },
        )
        .unwrap();
        assert_eq!(cookie.len(), 75);
        let claims = open_clearance(&keys(1), &cookie).unwrap();
        assert_eq!(claims.env_id, -3);
        assert_eq!(claims.expires_at, 1_800_086_400);
        assert_eq!(claims.budget_id, [5; 8]);
    }

    #[test]
    fn another_key_or_a_flipped_bit_fails_the_signature() {
        let token = seal_challenge(&keys(1), &challenge()).unwrap();
        assert_eq!(
            open_challenge(&keys(2), &token).err(),
            Some(ChallengeError::BadSignature)
        );
        let mut bytes = URL_SAFE_NO_PAD.decode(&token).unwrap();
        bytes[3] ^= 1; // env_id
        let tampered = URL_SAFE_NO_PAD.encode(bytes);
        assert_eq!(
            open_challenge(&keys(1), &tampered).err(),
            Some(ChallengeError::BadSignature)
        );
    }

    #[test]
    fn a_challenge_token_is_not_a_clearance() {
        // Different lengths and keys: neither can stand in for the other.
        let token = seal_challenge(&keys(1), &challenge()).unwrap();
        assert_eq!(
            open_clearance(&keys(1), &token).err(),
            Some(ChallengeError::Malformed)
        );
    }

    #[test]
    fn garbage_is_malformed() {
        for text in ["", "x", &"A".repeat(80), &"!".repeat(80)] {
            assert!(open_challenge(&keys(1), text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn freshness_window() {
        assert!(is_fresh(1000, 300, 1000));
        assert!(is_fresh(1000, 300, 1299));
        assert!(!is_fresh(1000, 300, 1300));
        assert!(is_fresh(1020, 300, 1000)); // within skew
        assert!(!is_fresh(1031, 300, 1000));
    }

    #[test]
    fn leading_zero_bits() {
        assert!(has_leading_zero_bits(&[0x00, 0x0f], 12));
        assert!(!has_leading_zero_bits(&[0x00, 0x1f], 12));
        assert!(has_leading_zero_bits(&[0x00, 0x00], 16));
        assert!(!has_leading_zero_bits(&[0x00, 0x00], 17));
        assert!(has_leading_zero_bits(&[0xff], 0));
    }

    #[test]
    fn nonce_must_be_decimal() {
        for nonce in ["", "-1", "1e3", "0x10", "123456789012345678901"] {
            assert_eq!(
                check_work("t", nonce, 0),
                Err(ChallengeError::BadNonce),
                "{nonce:?}"
            );
        }
        assert_eq!(check_work("t", "0", 0), Ok(()));
    }
}
