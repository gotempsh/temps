// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The attack-mode challenge at the proxy (ADR 066, phase 1).
//!
//! The proxy issues a signed proof-of-work token with the challenge page,
//! verifies the solution itself at [`VERIFY_PATH`], and admits later
//! requests on a signed clearance cookie. The visitor identity both are
//! bound to is computed here from the connection; nothing the client sends
//! in a body decides who it is or which environment it cleared.

use pingora::http::StatusCode;
use serde::Deserialize;
use temps_shield::{ChallengeError, ChallengeGate, ShieldKeys, VisitorIdentity, VisitorNetwork};

/// Where the challenge page posts its solution. Answered by the proxy on
/// every project host; it never reaches the console or the application.
pub const VERIFY_PATH: &str = "/api/_temps/captcha/verify";

/// Leading zero bits a challenge requires: about a million hashes, one to
/// two seconds in a current browser.
pub const CHALLENGE_DIFFICULTY: u8 = 20;

/// The verify body is a token, a nonce and a number; anything larger is not
/// a solution.
pub const MAX_VERIFY_BODY: usize = 4 * 1024;

const CHALLENGE_HTML: &str = include_str!("../captcha/challenge.html");

/// The challenge gate for this process, keyed from the master key so every
/// ingress that holds it signs and verifies the same tokens.
pub fn challenge_gate(encryption: &temps_core::EncryptionService) -> ChallengeGate {
    ChallengeGate::new(ShieldKeys::derive(|label| encryption.derive_subkey(label)))
}

/// The visitor as this request's connection presents it: the resolved client
/// address, the User-Agent, and the TLS fingerprint (until the JA4 of phase 3
/// replaces it).
pub fn visitor_identity(
    client_ip: Option<&str>,
    user_agent: &str,
    tls_fingerprint: Option<&str>,
) -> VisitorIdentity {
    let source = match tls_fingerprint {
        Some(fingerprint) => format!("tls:{fingerprint}"),
        None => "none:direct".to_string(),
    };
    VisitorIdentity::compute(
        client_ip.and_then(|ip| ip.parse().ok()),
        user_agent,
        &source,
    )
}

/// The network `client_ip` belongs to, for the per-network share of the
/// redeemed-token set.
pub fn visitor_network(client_ip: Option<&str>) -> VisitorNetwork {
    VisitorNetwork::of(client_ip.and_then(|ip| ip.parse().ok()))
}

#[derive(Debug, Deserialize)]
struct VerifyBody {
    token: String,
    nonce: String,
    /// How long the browser took, for the difficulty measurements of D6.
    #[serde(default)]
    solve_ms: Option<u64>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// The solution holds: store this `Set-Cookie` value, the clearance for
    /// `env_id`.
    Cleared {
        env_id: i32,
        set_cookie: String,
        solve_ms: Option<u64>,
    },
    /// A well-formed request whose token or solution does not hold.
    Rejected(ChallengeError),
    /// Not a verify request body at all.
    BadRequest,
}

/// Verifies a posted solution for the visitor the proxy resolved for this
/// request. The clearance is for the environment the signed token names,
/// whichever environment the verify path itself resolved to.
pub fn verify(
    gate: &ChallengeGate,
    body: &[u8],
    identity: &VisitorIdentity,
    network: VisitorNetwork,
    now: u64,
) -> VerifyOutcome {
    let Ok(body) = serde_json::from_slice::<VerifyBody>(body) else {
        return VerifyOutcome::BadRequest;
    };
    match gate.redeem(&body.token, &body.nonce, identity, network, now) {
        Ok(clearance) => VerifyOutcome::Cleared {
            env_id: clearance.env_id,
            set_cookie: clearance.set_cookie(),
            solve_ms: body.solve_ms,
        },
        Err(error) => VerifyOutcome::Rejected(error),
    }
}

/// The status and short reason a rejected verify returns to the page. A
/// full redeemed set is the ingress's limit and a full network share is a
/// rate limit, not a bad solution; the page offers a retry for every one.
pub fn rejection_response(error: ChallengeError) -> (StatusCode, &'static str) {
    match error {
        ChallengeError::Busy => (StatusCode::SERVICE_UNAVAILABLE, "busy"),
        ChallengeError::NetworkBusy => (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
        ChallengeError::Expired => (StatusCode::FORBIDDEN, "expired"),
        ChallengeError::Replayed => (StatusCode::FORBIDDEN, "replayed"),
        ChallengeError::InsufficientWork | ChallengeError::BadNonce => {
            (StatusCode::FORBIDDEN, "unsolved")
        }
        ChallengeError::Malformed
        | ChallengeError::BadSignature
        | ChallengeError::WrongVisitor
        | ChallengeError::Key => (StatusCode::FORBIDDEN, "invalid"),
    }
}

/// The challenge page for `token`. Every inserted value is escaped, though
/// the token is base64url and the address a parsed IP.
pub fn challenge_page(token: &str, difficulty: u8, client_ip: &str) -> String {
    CHALLENGE_HTML
        .replace("{{TOKEN}}", &escape_html(token))
        .replace("{{DIFFICULTY}}", &difficulty.to_string())
        .replace("{{IP_ADDRESS}}", &escape_html(client_ip))
}

fn escape_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(c),
        }
    }
    escaped
}

#[cfg(test)]
pub(crate) fn test_gate() -> ChallengeGate {
    ChallengeGate::new(ShieldKeys::derive(|label| {
        let mut key = [0u8; 32];
        for (slot, byte) in key.iter_mut().zip(label.bytes().cycle()) {
            *slot = byte;
        }
        key
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_escapes_what_it_inserts() {
        let page = challenge_page("tok", 20, "<script>alert(1)</script>");
        assert!(page.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!page.contains("<script>alert(1)"));
        assert!(page.contains(r#"data-token="tok""#));
        assert!(page.contains(r#"data-difficulty="20""#));
        assert!(!page.contains("{{"), "every placeholder is filled");
    }

    #[test]
    fn page_posts_only_token_nonce_and_timing() {
        // The old body named the environment and identifier; the proxy now
        // derives both, and the page must not send them.
        assert!(!CHALLENGE_HTML.contains("environment_id"));
        assert!(!CHALLENGE_HTML.contains("identifier"));
        assert!(CHALLENGE_HTML.contains(VERIFY_PATH));
    }

    #[test]
    fn identity_tracks_the_tls_fingerprint() {
        let ua = "Mozilla/5.0";
        let a = visitor_identity(Some("203.0.113.9"), ua, Some("t13_aa"));
        assert_eq!(
            a,
            visitor_identity(Some("203.0.113.10"), ua, Some("t13_aa"))
        );
        assert_ne!(a, visitor_identity(Some("203.0.113.9"), ua, Some("t13_bb")));
        assert_ne!(a, visitor_identity(Some("203.0.113.9"), ua, None));
    }

    #[test]
    fn capacity_refusals_are_not_forbidden() {
        assert_eq!(
            rejection_response(ChallengeError::Busy),
            (StatusCode::SERVICE_UNAVAILABLE, "busy")
        );
        assert_eq!(
            rejection_response(ChallengeError::NetworkBusy),
            (StatusCode::TOO_MANY_REQUESTS, "rate_limited")
        );
        assert_eq!(
            rejection_response(ChallengeError::WrongVisitor),
            (StatusCode::FORBIDDEN, "invalid")
        );
    }

    #[test]
    fn verify_rejects_bodies_that_are_not_solutions() {
        let gate = test_gate();
        let who = visitor_identity(Some("203.0.113.9"), "ua", Some("t13_aa"));
        let net = visitor_network(Some("203.0.113.9"));
        assert_eq!(verify(&gate, b"", &who, net, 0), VerifyOutcome::BadRequest);
        assert_eq!(
            verify(&gate, br#"{"challenge":"x","nonce":"1"}"#, &who, net, 0),
            VerifyOutcome::BadRequest
        );
        assert_eq!(
            verify(&gate, br#"{"token":"x","nonce":"1"}"#, &who, net, 0),
            VerifyOutcome::Rejected(ChallengeError::Malformed)
        );
    }
}
