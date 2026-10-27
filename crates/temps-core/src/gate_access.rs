// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Cloudflare Access verification at the listener (ADR 065 D9).
//!
//! An Access-gated address is served only through its tunnel, and every
//! request must carry a JWT Access signed for it:
//!
//! - read from the `Cf-Access-Jwt-Assertion` header only, never the
//!   `CF_Authorization` cookie, which anything else on the parent domain
//!   could plant; exactly one such header;
//! - RS256 only: the algorithm is checked before any key is used, so an
//!   `HS256` token "signed" with the public key, or `none`, never verifies;
//! - a `kid` in the team's key set from the route snapshot;
//! - `iss` equal to the team domain, `aud` containing the address's AUD,
//!   `exp` required and `nbf` honoured, with 60 seconds of skew.
//!
//! Pure functions of the request, the snapshot's [`AccessGate`] and the
//! time, so the control-plane proxy and the agent decide alike.

use axum::http::{HeaderMap, HeaderValue};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;

use crate::gate::{page, AccessGate, GateResponse, GATE_ACCESS, GATE_HEADER, USER_EMAIL_HEADER};

/// Cloudflare's team domains all end with this.
pub const TEAM_DOMAIN_SUFFIX: &str = ".cloudflareaccess.com";

/// `<team>.cloudflareaccess.com`, lower-cased, when `value` is one: a
/// single DNS label before the suffix, so nothing else can be named.
pub fn team_domain(value: &str) -> Option<String> {
    let host = value
        .trim()
        .trim_start_matches("https://")
        .trim_end_matches('/')
        .to_ascii_lowercase();
    let team = host.strip_suffix(TEAM_DOMAIN_SUFFIX)?;
    let label_ok = !team.is_empty()
        && team.len() <= 63
        && !team.starts_with('-')
        && !team.ends_with('-')
        && team
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    label_ok.then_some(host)
}

/// Whether `location` sends the browser to the sign-in of the Access team
/// `team` (its team domain): an `https` URL on exactly that host. Another
/// team's sign-in means another account's app answers there, whose tokens
/// the address refuses.
pub fn is_team_sign_in(location: Option<&str>, team: &str) -> bool {
    let Some(rest) = location.and_then(|l| l.trim().strip_prefix("https://")) else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    team_domain(host).is_some_and(|host| host.eq_ignore_ascii_case(team))
}

/// The header Access puts its JWT in, lower-case.
pub const ACCESS_JWT_HEADER: &str = "cf-access-jwt-assertion";
/// Clock skew allowed on `exp` and `nbf`.
pub const ACCESS_SKEW_SECS: i64 = 60;
/// A JWT longer than this is not Access's.
const MAX_JWT_BYTES: usize = 16 * 1024;

/// Why a request to an Access-gated address was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessRejection {
    /// No `Cf-Access-Jwt-Assertion` header, or more than one.
    Missing,
    /// Not a JWT this check can read.
    Malformed,
    /// Signed with anything but RS256.
    Algorithm,
    /// Its `kid` is not in the key set this listener has. The listener asks
    /// the control plane to fetch the keys again (rate-limited there).
    UnknownKey,
    BadSignature,
    WrongIssuer,
    WrongAudience,
    Expired,
    NotYetValid,
    /// Reached through a listener other than a tunnel: Access only sees
    /// tunnel traffic, so nothing else may serve the address.
    NotThroughTunnel,
}

impl AccessRejection {
    /// Why, in words a visitor (or the person debugging) can act on.
    pub fn message(self) -> &'static str {
        match self {
            AccessRejection::NotThroughTunnel => {
                "This address is protected by Cloudflare Access and is served only through its \
                 Cloudflare Tunnel. Open it at its Cloudflare address."
            }
            AccessRejection::Missing => {
                "This address is protected by Cloudflare Access, and the request did not come \
                 through Access. Open it at its Cloudflare address to sign in."
            }
            AccessRejection::UnknownKey => {
                "Cloudflare Access signed this request with a key this server does not know \
                 yet. Try again in a few minutes."
            }
            AccessRejection::Expired | AccessRejection::NotYetValid => {
                "Your Cloudflare Access sign-in has expired or is not valid yet. Reload the page \
                 to sign in again."
            }
            AccessRejection::Malformed
            | AccessRejection::Algorithm
            | AccessRejection::BadSignature
            | AccessRejection::WrongIssuer
            | AccessRejection::WrongAudience => {
                "Cloudflare Access did not let this request in for this address."
            }
        }
    }

    /// A short label for logs.
    pub fn as_str(self) -> &'static str {
        match self {
            AccessRejection::Missing => "missing",
            AccessRejection::Malformed => "malformed",
            AccessRejection::Algorithm => "algorithm",
            AccessRejection::UnknownKey => "unknown_key",
            AccessRejection::BadSignature => "bad_signature",
            AccessRejection::WrongIssuer => "wrong_issuer",
            AccessRejection::WrongAudience => "wrong_audience",
            AccessRejection::Expired => "expired",
            AccessRejection::NotYetValid => "not_yet_valid",
            AccessRejection::NotThroughTunnel => "not_through_tunnel",
        }
    }
}

/// Who Access let in, as far as the JWT says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessIdentity {
    pub email: Option<String>,
    pub subject: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

impl Audience {
    fn contains(&self, wanted: &str) -> bool {
        match self {
            Audience::One(aud) => aud == wanted,
            Audience::Many(auds) => auds.iter().any(|aud| aud == wanted),
        }
    }
}

#[derive(Debug, Deserialize)]
struct Claims {
    iss: Option<String>,
    aud: Option<Audience>,
    exp: Option<i64>,
    nbf: Option<i64>,
    email: Option<String>,
    sub: Option<String>,
}

fn same_issuer(claimed: &str, expected: &str) -> bool {
    !expected.is_empty() && claimed.trim_end_matches('/') == expected.trim_end_matches('/')
}

/// Verify `token` for the address `gate` describes, at `now` (unix seconds).
pub fn verify_access_jwt(
    token: &str,
    gate: &AccessGate,
    now: i64,
) -> Result<AccessIdentity, AccessRejection> {
    if token.is_empty() || token.len() > MAX_JWT_BYTES {
        return Err(AccessRejection::Malformed);
    }
    let header = jsonwebtoken::decode_header(token).map_err(|_| AccessRejection::Malformed)?;
    if header.alg != Algorithm::RS256 {
        return Err(AccessRejection::Algorithm);
    }
    let kid = header.kid.ok_or(AccessRejection::UnknownKey)?;
    let key = gate
        .keys
        .iter()
        .find(|key| key.kid == kid)
        .ok_or(AccessRejection::UnknownKey)?;
    let decoding = DecodingKey::from_rsa_components(&key.n, &key.e)
        .map_err(|_| AccessRejection::UnknownKey)?;
    // The signature and algorithm are checked here; the claims below, with
    // the caller's clock, so the decision is the same on every listener and
    // testable.
    let mut validation = Validation::new(Algorithm::RS256);
    validation.algorithms = vec![Algorithm::RS256];
    validation.validate_exp = false;
    validation.validate_nbf = false;
    validation.validate_aud = false;
    validation.required_spec_claims.clear();
    let claims = jsonwebtoken::decode::<Claims>(token, &decoding, &validation)
        .map_err(|error| match error.kind() {
            jsonwebtoken::errors::ErrorKind::InvalidAlgorithm => AccessRejection::Algorithm,
            jsonwebtoken::errors::ErrorKind::InvalidSignature => AccessRejection::BadSignature,
            _ => AccessRejection::Malformed,
        })?
        .claims;
    if !claims
        .iss
        .as_deref()
        .is_some_and(|iss| same_issuer(iss, &gate.issuer))
    {
        return Err(AccessRejection::WrongIssuer);
    }
    if gate.audience.is_empty()
        || !claims
            .aud
            .as_ref()
            .is_some_and(|aud| aud.contains(&gate.audience))
    {
        return Err(AccessRejection::WrongAudience);
    }
    match claims.exp {
        Some(exp) if exp + ACCESS_SKEW_SECS >= now => {}
        _ => return Err(AccessRejection::Expired),
    }
    if claims.nbf.is_some_and(|nbf| nbf - ACCESS_SKEW_SECS > now) {
        return Err(AccessRejection::NotYetValid);
    }
    Ok(AccessIdentity {
        email: claims.email,
        subject: claims.sub,
    })
}

/// Verify the request's Access JWT: the header only, exactly one.
pub fn verify_access_request(
    headers: &HeaderMap,
    gate: &AccessGate,
    now: i64,
) -> Result<AccessIdentity, AccessRejection> {
    let mut values = headers.get_all(ACCESS_JWT_HEADER).iter();
    let (Some(value), None) = (values.next(), values.next()) else {
        return Err(AccessRejection::Missing);
    };
    let token = value.to_str().map_err(|_| AccessRejection::Malformed)?;
    verify_access_jwt(token.trim(), gate, now)
}

/// `403` with `x-temps-gate: access`: the request did not carry a valid
/// Access JWT for this address, or did not come through the tunnel.
pub fn access_denied(rejection: AccessRejection) -> GateResponse {
    GateResponse {
        status: 403,
        headers: vec![
            ("cache-control", "no-store".to_string()),
            (GATE_HEADER, GATE_ACCESS.to_string()),
            ("content-type", "text/html; charset=utf-8".to_string()),
        ],
        body: Some(page("Access required", rejection.message())),
    }
}

/// The identity header for a request Access let in: only the email, since
/// an Access subject is not a Temps user id. A JWT without an email (a
/// service token) sends none.
pub fn access_identity_headers(identity: &AccessIdentity) -> Vec<(&'static str, HeaderValue)> {
    identity
        .email
        .as_deref()
        .and_then(|email| HeaderValue::from_str(email).ok())
        .map(|value| vec![(USER_EMAIL_HEADER, value)])
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::AccessKey;
    use base64::Engine;
    use jsonwebtoken::{EncodingKey, Header};
    use rsa::pkcs1::EncodeRsaPrivateKey;
    use rsa::traits::PublicKeyParts;
    use std::sync::OnceLock;

    const ISSUER: &str = "https://team.cloudflareaccess.com";
    const AUD: &str = "3f1c0aud";
    const NOW: i64 = 1_800_000_000;

    struct TestKey {
        der: Vec<u8>,
        n: String,
        e: String,
    }

    /// One RSA key for every test: generating one is slow in a debug build.
    fn test_key() -> &'static TestKey {
        static KEY: OnceLock<TestKey> = OnceLock::new();
        KEY.get_or_init(|| {
            let mut rng = rsa::rand_core::OsRng;
            let private = rsa::RsaPrivateKey::new(&mut rng, 2048).expect("rsa key");
            let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
            TestKey {
                der: private.to_pkcs1_der().expect("der").as_bytes().to_vec(),
                n: b64.encode(private.n().to_bytes_be()),
                e: b64.encode(private.e().to_bytes_be()),
            }
        })
    }

    fn gate() -> AccessGate {
        let key = test_key();
        AccessGate {
            issuer: ISSUER.into(),
            audience: AUD.into(),
            keys: vec![AccessKey {
                kid: "k1".into(),
                n: key.n.clone(),
                e: key.e.clone(),
            }],
        }
    }

    fn sign(kid: Option<&str>, claims: serde_json::Value) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = kid.map(str::to_string);
        jsonwebtoken::encode(
            &header,
            &claims,
            &EncodingKey::from_rsa_der(&test_key().der),
        )
        .expect("sign")
    }

    fn claims() -> serde_json::Value {
        serde_json::json!({
            "iss": ISSUER,
            "aud": [AUD],
            "exp": NOW + 600,
            "nbf": NOW - 10,
            "iat": NOW - 10,
            "email": "person@example.test",
            "sub": "subject-1",
        })
    }

    fn with(field: &str, value: serde_json::Value) -> serde_json::Value {
        let mut claims = claims();
        claims[field] = value;
        claims
    }

    #[test]
    fn a_valid_access_jwt_lets_the_person_in() {
        let identity =
            verify_access_jwt(&sign(Some("k1"), claims()), &gate(), NOW).expect("admitted");
        assert_eq!(identity.email.as_deref(), Some("person@example.test"));
        // A single-string `aud` and a trailing slash on `iss` are the same.
        let mut alike = with("aud", AUD.into());
        alike["iss"] = format!("{ISSUER}/").into();
        assert!(verify_access_jwt(&sign(Some("k1"), alike), &gate(), NOW).is_ok());
    }

    #[test]
    fn wrong_audience_or_issuer_is_refused() {
        let other_aud = sign(Some("k1"), with("aud", serde_json::json!(["another-app"])));
        assert_eq!(
            verify_access_jwt(&other_aud, &gate(), NOW),
            Err(AccessRejection::WrongAudience)
        );
        let no_aud = {
            let mut c = claims();
            c.as_object_mut().map(|o| o.remove("aud"));
            sign(Some("k1"), c)
        };
        assert_eq!(
            verify_access_jwt(&no_aud, &gate(), NOW),
            Err(AccessRejection::WrongAudience)
        );
        let other_team = sign(
            Some("k1"),
            with("iss", "https://other.cloudflareaccess.com".into()),
        );
        assert_eq!(
            verify_access_jwt(&other_team, &gate(), NOW),
            Err(AccessRejection::WrongIssuer)
        );
        // An address with no AUD in its snapshot admits nobody.
        let mut empty = gate();
        empty.audience.clear();
        assert_eq!(
            verify_access_jwt(&sign(Some("k1"), claims()), &empty, NOW),
            Err(AccessRejection::WrongAudience)
        );
    }

    #[test]
    fn expiry_and_not_before_allow_sixty_seconds_of_skew() {
        let expired = sign(Some("k1"), with("exp", (NOW - 61).into()));
        assert_eq!(
            verify_access_jwt(&expired, &gate(), NOW),
            Err(AccessRejection::Expired)
        );
        let just = sign(Some("k1"), with("exp", (NOW - 59).into()));
        assert!(verify_access_jwt(&just, &gate(), NOW).is_ok());
        let no_exp = {
            let mut c = claims();
            c.as_object_mut().map(|o| o.remove("exp"));
            sign(Some("k1"), c)
        };
        assert_eq!(
            verify_access_jwt(&no_exp, &gate(), NOW),
            Err(AccessRejection::Expired)
        );
        let early = sign(Some("k1"), with("nbf", (NOW + 61).into()));
        assert_eq!(
            verify_access_jwt(&early, &gate(), NOW),
            Err(AccessRejection::NotYetValid)
        );
        let slightly = sign(Some("k1"), with("nbf", (NOW + 59).into()));
        assert!(verify_access_jwt(&slightly, &gate(), NOW).is_ok());
    }

    #[test]
    fn an_unknown_or_missing_kid_is_refused_as_unknown() {
        assert_eq!(
            verify_access_jwt(&sign(Some("rotated"), claims()), &gate(), NOW),
            Err(AccessRejection::UnknownKey)
        );
        assert_eq!(
            verify_access_jwt(&sign(None, claims()), &gate(), NOW),
            Err(AccessRejection::UnknownKey)
        );
    }

    #[test]
    fn algorithm_confusion_is_refused() {
        // HS256 "signed" with the public modulus as the secret: the classic
        // confusion attack. Refused on the algorithm, before any key is used.
        let mut header = Header::new(Algorithm::HS256);
        header.kid = Some("k1".into());
        let hs = jsonwebtoken::encode(
            &header,
            &claims(),
            &EncodingKey::from_secret(test_key().n.as_bytes()),
        )
        .expect("sign");
        assert_eq!(
            verify_access_jwt(&hs, &gate(), NOW),
            Err(AccessRejection::Algorithm)
        );
        // `alg: none` with no signature.
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let none = format!(
            "{}.{}.",
            b64.encode(br#"{"alg":"none","kid":"k1","typ":"JWT"}"#),
            b64.encode(claims().to_string())
        );
        assert!(matches!(
            verify_access_jwt(&none, &gate(), NOW),
            Err(AccessRejection::Malformed | AccessRejection::Algorithm)
        ));
        // RS512 with the right key is still not RS256.
        let mut header = Header::new(Algorithm::RS512);
        header.kid = Some("k1".into());
        let rs512 = jsonwebtoken::encode(
            &header,
            &claims(),
            &EncodingKey::from_rsa_der(&test_key().der),
        )
        .expect("sign");
        assert_eq!(
            verify_access_jwt(&rs512, &gate(), NOW),
            Err(AccessRejection::Algorithm)
        );
    }

    #[test]
    fn a_tampered_token_fails_its_signature() {
        let token = sign(Some("k1"), claims());
        let mut parts: Vec<&str> = token.split('.').collect();
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let forged = b64.encode(with("email", "admin@example.test".into()).to_string());
        parts[1] = &forged;
        assert_eq!(
            verify_access_jwt(&parts.join("."), &gate(), NOW),
            Err(AccessRejection::BadSignature)
        );
        assert_eq!(
            verify_access_jwt("not-a-jwt", &gate(), NOW),
            Err(AccessRejection::Malformed)
        );
    }

    #[test]
    fn only_one_header_counts_and_never_the_cookie() {
        let token = sign(Some("k1"), claims());
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            format!("CF_Authorization={token}").parse().expect("header"),
        );
        assert_eq!(
            verify_access_request(&headers, &gate(), NOW),
            Err(AccessRejection::Missing)
        );
        headers.insert(ACCESS_JWT_HEADER, token.parse().expect("header"));
        assert!(verify_access_request(&headers, &gate(), NOW).is_ok());
        headers.append(ACCESS_JWT_HEADER, token.parse().expect("header"));
        assert_eq!(
            verify_access_request(&headers, &gate(), NOW),
            Err(AccessRejection::Missing)
        );
    }

    fn access_policy(access: Option<AccessGate>) -> crate::gate::GatePolicy {
        crate::gate::GatePolicy {
            address_id: "custom:7".into(),
            gate: crate::gate::GateKind::CloudflareAccess,
            temps: None,
            access,
        }
    }

    fn decide_at(
        policy: &crate::gate::GatePolicy,
        path: &str,
        headers: &HeaderMap,
        secure: bool,
        via_tunnel: bool,
    ) -> crate::gate::GateDecision {
        crate::gate::decide(
            policy,
            &crate::gate::GateRequest {
                hostname: "app.example.test",
                method: &axum::http::Method::GET,
                path,
                query: None,
                headers,
                secure,
                via_tunnel,
            },
            NOW,
        )
        .expect("decision")
    }

    #[test]
    fn an_access_address_is_served_only_through_the_tunnel() {
        use crate::gate::GateDecision;
        let policy = access_policy(Some(gate()));
        let mut headers = HeaderMap::new();
        headers.insert(
            ACCESS_JWT_HEADER,
            sign(Some("k1"), claims()).parse().expect("header"),
        );
        // A valid JWT on any other listener is still refused: Access never
        // saw that request.
        assert_eq!(
            decide_at(&policy, "/", &headers, true, false),
            GateDecision::AccessDenied(AccessRejection::NotThroughTunnel)
        );
        // Through the tunnel it is let in, with no HTTPS upgrade: the
        // tunnel's edge is HTTPS whatever the origin hop says.
        assert!(matches!(
            decide_at(&policy, "/", &headers, false, true),
            GateDecision::AdmitAccess(AccessIdentity { email: Some(_), .. })
        ));
        // Temps' own sign-in paths do not exist on an Access address.
        assert_eq!(
            decide_at(&policy, "/_temps/gate/callback", &headers, true, true),
            GateDecision::NotFound
        );
        // No JWT through the tunnel: refused, never the app.
        assert_eq!(
            decide_at(&policy, "/", &HeaderMap::new(), true, true),
            GateDecision::AccessDenied(AccessRejection::Missing)
        );
        // No Access part in the snapshot: unavailable, never open.
        assert!(matches!(
            decide_at(&access_policy(None), "/", &headers, true, true),
            GateDecision::Unavailable { .. }
        ));
    }

    #[test]
    fn an_access_refusal_is_a_403_marked_access_and_never_cached() {
        let response = access_denied(AccessRejection::NotThroughTunnel);
        assert_eq!(response.status, 403);
        assert!(response
            .headers
            .iter()
            .any(|(k, v)| *k == GATE_HEADER && v == GATE_ACCESS));
        assert!(response
            .headers
            .iter()
            .any(|(k, v)| *k == "cache-control" && v == "no-store"));
        // Only the email is passed on: an Access subject is not a Temps user.
        let headers = access_identity_headers(&AccessIdentity {
            email: Some("person@example.test".into()),
            subject: Some("subject-1".into()),
        });
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].0, USER_EMAIL_HEADER);
        assert!(access_identity_headers(&AccessIdentity {
            email: None,
            subject: Some("service-token".into()),
        })
        .is_empty());
    }
}
