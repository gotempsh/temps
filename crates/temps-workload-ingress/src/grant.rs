// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! `TempsLogin` grants (ADR-055 §11).
//!
//! The console session cannot reach an application hostname (its cookie is
//! host-only and `SameSite=Strict`, and checking it needs the database). So
//! an endpoint protected by `TempsLogin` redirects to the console, which
//! checks the user's permission on the owner and redirects back with a
//! short-lived **exchange** grant signed for that host and owner. The
//! ingress verifies it against a key in its route snapshot and answers with
//! a **session** grant in a cookie for that host, which every later request
//! carries.
//!
//! - **Keys are per owner.** Each owner's key is derived from one master
//!   secret (`HMAC-SHA256(master, "temps-ingress-grant-v1|<kind>:<id>")`),
//!   so a worker is only ever handed the keys of the endpoints it serves
//!   and cannot sign a grant for anything else.
//! - **Verification allocates nothing**: the payload is decoded into a
//!   caller-supplied stack buffer, the MAC is checked in constant time and
//!   the fields are borrowed from that buffer.
//! - **Revocation without a database call**: every grant carries an id.
//!   Revoking a user or a permission puts the affected ids in the snapshot
//!   until they expire ([`Revocations`]).
//!
//! Token: `tg1.<base64url(payload)>.<base64url(mac)>`, where the payload is
//! `use|grant_id|owner_kind|owner_id|host|permission|user_id|expires_at`.

use std::collections::HashSet;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use crate::endpoint::{ProductKind, ProductRef};

type HmacSha256 = Hmac<Sha256>;

const VERSION: &str = "tg1";
const KEY_LABEL: &[u8] = b"temps-ingress-grant-v1|";

/// The largest payload a grant may carry: a 253-byte host, a permission
/// and the numeric fields.
pub const MAX_GRANT_PAYLOAD: usize = 512;

/// How long an exchange grant lives: long enough to cross a redirect.
pub const EXCHANGE_GRANT_TTL_SECS: u64 = 120;

/// How long the session cookie an exchange grant buys lives.
pub const SESSION_GRANT_TTL_SECS: u64 = 12 * 60 * 60;

/// One owner's signing key.
#[derive(Clone, PartialEq, Eq)]
pub struct GrantKey([u8; 32]);

impl std::fmt::Debug for GrantKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GrantKey(<hidden>)")
    }
}

impl GrantKey {
    /// The key for `owner`, derived from the instance's master secret.
    pub fn derive(master: &[u8; 32], owner: ProductRef) -> Self {
        let mut mac = mac_for(master);
        mac.update(KEY_LABEL);
        mac.update(owner.kind.as_str().as_bytes());
        mac.update(b":");
        let mut digits = [0u8; 20];
        mac.update(format_i64(owner.id, &mut digits));
        let mut key = [0u8; 32];
        key.copy_from_slice(&mac.finalize().into_bytes());
        Self(key)
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

fn mac_for(key: &[u8; 32]) -> HmacSha256 {
    // HMAC takes a key of any length; a 32-byte key never fails.
    match HmacSha256::new_from_slice(key) {
        Ok(mac) => mac,
        Err(_) => unreachable!("HMAC-SHA256 accepts 32-byte keys"),
    }
}

/// `value` in decimal, into `buffer`.
fn format_i64(value: i64, buffer: &mut [u8; 20]) -> &[u8] {
    let negative = value < 0;
    let mut n = value.unsigned_abs();
    let mut at = buffer.len();
    loop {
        at -= 1;
        buffer[at] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    if negative {
        at -= 1;
        buffer[at] = b'-';
    }
    &buffer[at..]
}

/// What a grant may be used for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantUse {
    /// Carried in the redirect back from the console; exchanged once.
    Exchange,
    /// Carried in the host's cookie on every request.
    Session,
}

impl GrantUse {
    fn tag(&self) -> &'static str {
        match self {
            GrantUse::Exchange => "x",
            GrantUse::Session => "s",
        }
    }

    fn parse(tag: &str) -> Option<Self> {
        match tag {
            "x" => Some(GrantUse::Exchange),
            "s" => Some(GrantUse::Session),
            _ => None,
        }
    }
}

/// The signed fields of a grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrantClaims<'a> {
    pub grant_id: i64,
    pub usage: GrantUse,
    pub owner: ProductRef,
    /// Lowercase, no port.
    pub host: &'a str,
    pub permission: &'a str,
    pub user_id: i64,
    /// Unix seconds.
    pub expires_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GrantError {
    #[error("grant field '{field}' for {owner} contains '|' or is empty")]
    InvalidField { field: &'static str, owner: String },
    #[error("grant for {owner} would be {len} bytes, over the {max}-byte limit")]
    TooLarge {
        owner: String,
        len: usize,
        max: usize,
    },
    #[error("grant is malformed")]
    Malformed,
    #[error("grant signature does not match")]
    BadSignature,
    #[error("grant is for {found}, not {expected}")]
    WrongOwner {
        expected: ProductRef,
        found: ProductRef,
    },
    #[error("grant is for host '{found}', not '{expected}'")]
    WrongHost { expected: String, found: String },
    #[error("grant {grant_id} carries permission '{found}', not '{expected}'")]
    WrongPermission {
        grant_id: i64,
        expected: String,
        found: String,
    },
    #[error("grant {grant_id} is a {found:?} grant, not a {expected:?} grant")]
    WrongUse {
        grant_id: i64,
        expected: GrantUse,
        found: GrantUse,
    },
    #[error("grant {grant_id} expired at {expires_at}")]
    Expired { grant_id: i64, expires_at: u64 },
    #[error("grant {grant_id} was revoked")]
    Revoked { grant_id: i64 },
}

/// Signs `claims`. Runs on the console (exchange grants) and at the
/// ingress once per exchange (session grants), never per request.
pub fn mint(key: &GrantKey, claims: &GrantClaims<'_>) -> Result<String, GrantError> {
    let owner = claims.owner.to_string();
    for (field, value) in [("host", claims.host), ("permission", claims.permission)] {
        if value.is_empty() || value.contains('|') {
            return Err(GrantError::InvalidField {
                field,
                owner: owner.clone(),
            });
        }
    }
    let payload = format!(
        "{}|{}|{}|{}|{}|{}|{}|{}",
        claims.usage.tag(),
        claims.grant_id,
        claims.owner.kind.as_str(),
        claims.owner.id,
        claims.host.to_ascii_lowercase(),
        claims.permission,
        claims.user_id,
        claims.expires_at,
    );
    if payload.len() > MAX_GRANT_PAYLOAD {
        return Err(GrantError::TooLarge {
            owner,
            len: payload.len(),
            max: MAX_GRANT_PAYLOAD,
        });
    }
    let mut mac = mac_for(&key.0);
    mac.update(payload.as_bytes());
    let tag = mac.finalize().into_bytes();
    Ok(format!(
        "{VERSION}.{}.{}",
        URL_SAFE_NO_PAD.encode(payload.as_bytes()),
        URL_SAFE_NO_PAD.encode(tag)
    ))
}

/// Checks the signature and parses the claims into `buffer`. Allocates
/// nothing. The caller still has to [`check`] what the claims say.
pub fn verify<'b>(
    key: &GrantKey,
    token: &str,
    buffer: &'b mut [u8; MAX_GRANT_PAYLOAD],
) -> Result<GrantClaims<'b>, GrantError> {
    let mut parts = token.trim().splitn(3, '.');
    let (Some(version), Some(payload), Some(tag)) = (parts.next(), parts.next(), parts.next())
    else {
        return Err(GrantError::Malformed);
    };
    if version != VERSION || tag.len() > 64 {
        return Err(GrantError::Malformed);
    }
    let mut tag_bytes = [0u8; 48];
    let tag_len = URL_SAFE_NO_PAD
        .decode_slice(tag, &mut tag_bytes)
        .map_err(|_| GrantError::Malformed)?;
    if payload.len() > MAX_GRANT_PAYLOAD * 4 / 3 + 4 {
        return Err(GrantError::Malformed);
    }
    let len = URL_SAFE_NO_PAD
        .decode_slice(payload, &mut buffer[..])
        .map_err(|_| GrantError::Malformed)?;
    let mut mac = mac_for(&key.0);
    mac.update(&buffer[..len]);
    mac.verify_slice(&tag_bytes[..tag_len])
        .map_err(|_| GrantError::BadSignature)?;
    let text = std::str::from_utf8(&buffer[..len]).map_err(|_| GrantError::Malformed)?;
    parse_claims(text).ok_or(GrantError::Malformed)
}

fn parse_claims(text: &str) -> Option<GrantClaims<'_>> {
    let mut fields = text.split('|');
    let usage = GrantUse::parse(fields.next()?)?;
    let grant_id = fields.next()?.parse().ok()?;
    let kind = ProductKind::parse(fields.next()?)?;
    let owner_id = fields.next()?.parse().ok()?;
    let host = fields.next()?;
    let permission = fields.next()?;
    let user_id = fields.next()?.parse().ok()?;
    let expires_at = fields.next()?.parse().ok()?;
    if fields.next().is_some() {
        return None;
    }
    Some(GrantClaims {
        grant_id,
        usage,
        owner: ProductRef { kind, id: owner_id },
        host,
        permission,
        user_id,
        expires_at,
    })
}

/// What a verified grant must say to admit this request.
#[derive(Debug, Clone, Copy)]
pub struct GrantExpectation<'a> {
    pub owner: ProductRef,
    /// The request's host, lowercase, no port.
    pub host: &'a str,
    pub permission: &'a str,
    pub usage: GrantUse,
    /// Unix seconds.
    pub now: u64,
}

/// Checks verified claims against the request. Allocates only to describe
/// a refusal.
pub fn check(
    claims: &GrantClaims<'_>,
    expected: &GrantExpectation<'_>,
    revocations: &Revocations,
) -> Result<(), GrantError> {
    if claims.owner != expected.owner {
        return Err(GrantError::WrongOwner {
            expected: expected.owner,
            found: claims.owner,
        });
    }
    if !claims.host.eq_ignore_ascii_case(expected.host) {
        return Err(GrantError::WrongHost {
            expected: expected.host.to_string(),
            found: claims.host.to_string(),
        });
    }
    if claims.permission != expected.permission {
        return Err(GrantError::WrongPermission {
            grant_id: claims.grant_id,
            expected: expected.permission.to_string(),
            found: claims.permission.to_string(),
        });
    }
    if claims.usage != expected.usage {
        return Err(GrantError::WrongUse {
            grant_id: claims.grant_id,
            expected: expected.usage,
            found: claims.usage,
        });
    }
    if claims.expires_at <= expected.now {
        return Err(GrantError::Expired {
            grant_id: claims.grant_id,
            expires_at: claims.expires_at,
        });
    }
    if revocations.contains(claims.grant_id) {
        return Err(GrantError::Revoked {
            grant_id: claims.grant_id,
        });
    }
    Ok(())
}

/// [`verify`] then [`check`], answering yes or no. The request-path form:
/// a refusal allocates nothing either.
pub fn admits(
    key: &GrantKey,
    token: &str,
    expected: &GrantExpectation<'_>,
    revocations: &Revocations,
) -> bool {
    let mut buffer = [0u8; MAX_GRANT_PAYLOAD];
    let Ok(claims) = verify(key, token, &mut buffer) else {
        return false;
    };
    claims.owner == expected.owner
        && claims.host.eq_ignore_ascii_case(expected.host)
        && claims.permission == expected.permission
        && claims.usage == expected.usage
        && claims.expires_at > expected.now
        && !revocations.contains(claims.grant_id)
}

/// Grant ids revoked before they expired. Carried in the snapshot until
/// the last of them expires.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Revocations(HashSet<i64>);

impl Revocations {
    pub fn new(ids: impl IntoIterator<Item = i64>) -> Self {
        Self(ids.into_iter().collect())
    }

    pub fn contains(&self, grant_id: i64) -> bool {
        !self.0.is_empty() && self.0.contains(&grant_id)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASTER: [u8; 32] = [7u8; 32];

    fn sandbox(id: i64) -> ProductRef {
        ProductRef {
            kind: ProductKind::Sandbox,
            id,
        }
    }

    fn claims(usage: GrantUse) -> GrantClaims<'static> {
        GrantClaims {
            grant_id: 41,
            usage,
            owner: sandbox(3),
            host: "ws-a1b2c3d4e5f60718-3000.preview.example.test",
            permission: "sandboxes:read",
            user_id: 9,
            expires_at: 2_000,
        }
    }

    fn expectation(usage: GrantUse, now: u64) -> GrantExpectation<'static> {
        GrantExpectation {
            owner: sandbox(3),
            host: "WS-A1B2C3D4E5F60718-3000.preview.example.test",
            permission: "sandboxes:read",
            usage,
            now,
        }
    }

    #[test]
    fn a_minted_grant_verifies_and_round_trips_its_claims() {
        let key = GrantKey::derive(&MASTER, sandbox(3));
        let token = mint(&key, &claims(GrantUse::Exchange)).unwrap();
        assert!(token.starts_with("tg1."));
        let mut buffer = [0u8; MAX_GRANT_PAYLOAD];
        let parsed = verify(&key, &token, &mut buffer).unwrap();
        assert_eq!(parsed, claims(GrantUse::Exchange));
        assert_eq!(
            check(
                &parsed,
                &expectation(GrantUse::Exchange, 1_000),
                &Revocations::default()
            ),
            Ok(())
        );
        assert!(admits(
            &key,
            &token,
            &expectation(GrantUse::Exchange, 1_000),
            &Revocations::default()
        ));
    }

    #[test]
    fn keys_are_per_owner_so_one_owner_cannot_sign_for_another() {
        let mine = GrantKey::derive(&MASTER, sandbox(3));
        let other = GrantKey::derive(&MASTER, sandbox(4));
        assert_ne!(mine, other);
        let env = GrantKey::derive(
            &MASTER,
            ProductRef {
                kind: ProductKind::Environment,
                id: 3,
            },
        );
        assert_ne!(mine, env, "the kind is part of the key");
        let token = mint(&other, &claims(GrantUse::Session)).unwrap();
        let mut buffer = [0u8; MAX_GRANT_PAYLOAD];
        assert_eq!(
            verify(&mine, &token, &mut buffer),
            Err(GrantError::BadSignature)
        );
    }

    #[test]
    fn every_claim_is_checked() {
        let key = GrantKey::derive(&MASTER, sandbox(3));
        let none = Revocations::default();
        let token = mint(&key, &claims(GrantUse::Exchange)).unwrap();
        let mut buffer = [0u8; MAX_GRANT_PAYLOAD];
        let parsed = verify(&key, &token, &mut buffer).unwrap();
        let base = expectation(GrantUse::Exchange, 1_000);
        assert!(matches!(
            check(
                &parsed,
                &GrantExpectation {
                    owner: sandbox(4),
                    ..base
                },
                &none
            ),
            Err(GrantError::WrongOwner { .. })
        ));
        assert!(matches!(
            check(
                &parsed,
                &GrantExpectation {
                    host: "other.example.test",
                    ..base
                },
                &none
            ),
            Err(GrantError::WrongHost { .. })
        ));
        assert!(matches!(
            check(
                &parsed,
                &GrantExpectation {
                    permission: "sandboxes:write",
                    ..base
                },
                &none
            ),
            Err(GrantError::WrongPermission { grant_id: 41, .. })
        ));
        assert!(matches!(
            check(
                &parsed,
                &GrantExpectation {
                    usage: GrantUse::Session,
                    ..base
                },
                &none
            ),
            Err(GrantError::WrongUse { .. })
        ));
        assert_eq!(
            check(&parsed, &GrantExpectation { now: 2_000, ..base }, &none),
            Err(GrantError::Expired {
                grant_id: 41,
                expires_at: 2_000
            })
        );
        assert_eq!(
            check(&parsed, &base, &Revocations::new([41])),
            Err(GrantError::Revoked { grant_id: 41 })
        );
        assert!(!admits(&key, &token, &base, &Revocations::new([41])));
    }

    #[test]
    fn tampering_and_garbage_are_refused() {
        let key = GrantKey::derive(&MASTER, sandbox(3));
        let token = mint(&key, &claims(GrantUse::Session)).unwrap();
        let mut buffer = [0u8; MAX_GRANT_PAYLOAD];
        // Re-sign a payload naming another user with the wrong key.
        let forged = mint(
            &GrantKey::from_bytes([1u8; 32]),
            &GrantClaims {
                user_id: 1,
                ..claims(GrantUse::Session)
            },
        )
        .unwrap();
        let (_, forged_payload) = forged.split_once('.').unwrap();
        let (forged_payload, _) = forged_payload.split_once('.').unwrap();
        let (_, tag) = token.rsplit_once('.').unwrap();
        let spliced = format!("tg1.{forged_payload}.{tag}");
        assert_eq!(
            verify(&key, &spliced, &mut buffer),
            Err(GrantError::BadSignature)
        );
        for garbage in [
            "",
            "tg1",
            "tg1..",
            "tg2.a.b",
            "tg1.!!!.###",
            &format!("tg1.{}.x", "A".repeat(4000)),
        ] {
            assert!(verify(&key, garbage, &mut buffer).is_err(), "{garbage}");
        }
    }

    #[test]
    fn minting_refuses_fields_that_would_break_the_payload() {
        let key = GrantKey::derive(&MASTER, sandbox(3));
        assert!(matches!(
            mint(
                &key,
                &GrantClaims {
                    permission: "a|b",
                    ..claims(GrantUse::Exchange)
                }
            ),
            Err(GrantError::InvalidField {
                field: "permission",
                ..
            })
        ));
        assert!(matches!(
            mint(
                &key,
                &GrantClaims {
                    host: "",
                    ..claims(GrantUse::Exchange)
                }
            ),
            Err(GrantError::InvalidField { field: "host", .. })
        ));
        let long = "a".repeat(600);
        assert!(matches!(
            mint(
                &key,
                &GrantClaims {
                    permission: &long,
                    ..claims(GrantUse::Exchange)
                }
            ),
            Err(GrantError::TooLarge { .. })
        ));
    }

    #[test]
    fn negative_ids_format_in_the_key_label() {
        let mut digits = [0u8; 20];
        assert_eq!(format_i64(-42, &mut digits), b"-42");
        assert_eq!(format_i64(0, &mut digits), b"0");
        assert_eq!(format_i64(i64::MIN, &mut digits), b"-9223372036854775808");
    }
}
