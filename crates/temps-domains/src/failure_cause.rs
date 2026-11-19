// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Why a certificate could not be issued, as a typed cause.
//!
//! The CA reports every validation failure as an RFC 8555 problem whose
//! `type` is a URN (`urn:ietf:params:acme:error:connection`, `...:dns`, ...).
//! The provider boundary keeps that URN in the error text, so classifying on
//! the URN is exact. Matching loose words instead misfiled failures: every
//! HTTP-01 error mentions `/.well-known/acme-challenge/`, so "challenge"
//! matched everything, and a hostname containing "dns" read as a DNS failure.
//!
//! The cause is what the console explains and what decides whether retrying
//! on its own can help: a closed port or a CDN in front of the host does not
//! fix itself, a CA hiccup does.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::tls::models::ChallengeType;

/// Prefix of every ACME problem type (RFC 8555 §6.7).
const ACME_ERROR_URN: &str = "urn:ietf:params:acme:error:";

/// Prefix of the error written by the on-demand DNS preflight
/// (`DomainService::preflight_dns`).
const DNS_PREFLIGHT_PREFIX: &str = "DNS preflight: ";

/// Why the last certificate attempt for a domain failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CertFailureCause {
    /// The hostname has no usable DNS record, or its DNS could not be read.
    DnsLookup,
    /// The CA could not connect to the host on port 80: closed, filtered,
    /// timing out, or an address the internet cannot reach (a private network
    /// or tailnet).
    Unreachable,
    /// HTTP-01: something answered the CA, but not with the challenge token:
    /// a CDN or tunnel error page, another web server, or a redirect to an
    /// HTTPS endpoint that is not this server.
    UpstreamAnswered,
    /// DNS-01: the CA looked up the `_acme-challenge` TXT record but found no
    /// value for this order: the record is missing, still holds an old value,
    /// lives at a DNS provider other than the one answering for the domain, or
    /// has not propagated yet.
    DnsRecordMismatch,
    /// A CAA record on the domain does not allow this CA to issue.
    CaaForbids,
    /// The CA's rate limit for this domain or account was reached.
    RateLimited,
    /// The CA or the connection to it failed in a way that can clear on its
    /// own (CA internal error, network error, validation still pending).
    CaTransient,
    /// The request itself is wrong here: no ACME contact email, an
    /// identifier or account the CA rejects.
    Config,
    /// None of the above could be recognised.
    Unknown,
}

impl CertFailureCause {
    /// Every cause, for tests and schema listings.
    pub const ALL: [CertFailureCause; 9] = [
        CertFailureCause::DnsLookup,
        CertFailureCause::Unreachable,
        CertFailureCause::UpstreamAnswered,
        CertFailureCause::DnsRecordMismatch,
        CertFailureCause::CaaForbids,
        CertFailureCause::RateLimited,
        CertFailureCause::CaTransient,
        CertFailureCause::Config,
        CertFailureCause::Unknown,
    ];

    /// The value stored in `domains.failure_cause`.
    pub fn as_str(self) -> &'static str {
        match self {
            CertFailureCause::DnsLookup => "dns_lookup",
            CertFailureCause::Unreachable => "unreachable",
            CertFailureCause::UpstreamAnswered => "upstream_answered",
            CertFailureCause::DnsRecordMismatch => "dns_record_mismatch",
            CertFailureCause::CaaForbids => "caa_forbids",
            CertFailureCause::RateLimited => "rate_limited",
            CertFailureCause::CaTransient => "ca_transient",
            CertFailureCause::Config => "config",
            CertFailureCause::Unknown => "unknown",
        }
    }

    /// Read a stored value. A value this build does not know (written by a
    /// newer Temps) reads as [`CertFailureCause::Unknown`], never an error.
    pub fn from_stored(value: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|cause| cause.as_str() == value)
            .unwrap_or(CertFailureCause::Unknown)
    }

    /// Whether trying again later, unchanged, can succeed. Only these causes
    /// justify an automatic retry; the others need the operator to change
    /// something first.
    pub fn is_transient(self) -> bool {
        matches!(
            self,
            CertFailureCause::CaTransient | CertFailureCause::RateLimited
        )
    }

    /// The coarse `error_category` the on-demand audit rows and their console
    /// labels already use (`web/src/lib/on-demand-certs.ts`).
    pub fn legacy_category(self) -> &'static str {
        match self {
            CertFailureCause::RateLimited => "rate_limited",
            CertFailureCause::DnsLookup | CertFailureCause::DnsRecordMismatch => "dns_failure",
            // The console reads this one as "is port 80 open and reachable?",
            // which is the question for both.
            CertFailureCause::Unreachable | CertFailureCause::UpstreamAnswered => {
                "challenge_mismatch"
            }
            CertFailureCause::CaTransient => "timeout",
            CertFailureCause::CaaForbids | CertFailureCause::Config | CertFailureCause::Unknown => {
                "internal"
            }
        }
    }
}

impl std::fmt::Display for CertFailureCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The cause for one ACME problem type, given without its URN prefix
/// (`connection`, `dns`, ...).
fn cause_of_problem_type(kind: &str) -> CertFailureCause {
    match kind {
        "dns" => CertFailureCause::DnsLookup,
        "connection" => CertFailureCause::Unreachable,
        // `unauthorized` / `incorrectResponse`: the CA fetched something that
        // was not the token. `tls`: it followed a redirect to an HTTPS
        // endpoint whose handshake failed (a CDN or proxy in front).
        "unauthorized" | "incorrectResponse" | "tls" => CertFailureCause::UpstreamAnswered,
        "caa" => CertFailureCause::CaaForbids,
        "rateLimited" => CertFailureCause::RateLimited,
        "serverInternal" | "badNonce" | "orderNotReady" => CertFailureCause::CaTransient,
        "malformed"
        | "rejectedIdentifier"
        | "unsupportedIdentifier"
        | "invalidContact"
        | "unsupportedContact"
        | "accountDoesNotExist"
        | "externalAccountRequired"
        | "badCSR"
        | "badPublicKey"
        | "badSignatureAlgorithm"
        | "userActionRequired" => CertFailureCause::Config,
        _ => CertFailureCause::Unknown,
    }
}

/// The ACME problem types named in `text`, in order (`connection`, `dns`, ...).
fn problem_types(text: &str) -> impl Iterator<Item = &str> {
    text.match_indices(ACME_ERROR_URN).map(move |(at, _)| {
        let rest = &text[at + ACME_ERROR_URN.len()..];
        let end = rest
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(rest.len());
        &rest[..end]
    })
}

/// The cause named by the CA's problem types in `text`, if any.
///
/// A rate limit anywhere wins, because it says nothing more may be tried for
/// now. Otherwise the first recognised per-challenge problem names the cause;
/// an authorization can carry several, and the first is the one the CA hit.
/// `None` when the text carries no recognised ACME problem type.
pub fn classify_problem(text: &str) -> Option<CertFailureCause> {
    let mut first = None;
    for kind in problem_types(text) {
        let cause = cause_of_problem_type(kind);
        if cause == CertFailureCause::RateLimited {
            return Some(cause);
        }
        if first.is_none() && cause != CertFailureCause::Unknown {
            first = Some(cause);
        }
    }
    first
}

/// Classify a failed attempt for a challenge of type `challenge`.
///
/// The CA reports a proof it could not verify as `unauthorized` (or
/// `incorrectResponse`) for both challenge types, and [`classify`] reads that
/// as HTTP-01's "something else answered on port 80". For DNS-01 no web
/// server was involved: the TXT record did not hold this order's value.
pub fn classify_for(text: &str, challenge: &ChallengeType) -> CertFailureCause {
    match (challenge, classify(text)) {
        (ChallengeType::Dns01, CertFailureCause::UpstreamAnswered) => {
            CertFailureCause::DnsRecordMismatch
        }
        (_, cause) => cause,
    }
}

/// Classify a failed certificate attempt from its full error text.
///
/// The CA's problem type decides when there is one ([`classify_problem`]).
/// Without one the failure never reached the CA's validation: the preflight
/// found no DNS, the contact email is missing, the order poll gave up, or the
/// CA could not be reached at all.
pub fn classify(text: &str) -> CertFailureCause {
    if let Some(cause) = classify_problem(text) {
        return cause;
    }

    let lower = text.to_ascii_lowercase();
    if text.contains(DNS_PREFLIGHT_PREFIX) {
        return CertFailureCause::DnsLookup;
    }
    if lower.contains("too many") && lower.contains("retry after") {
        // A rate limit whose URN was lost on the way.
        return CertFailureCause::RateLimited;
    }
    if lower.contains("acme contact email") || lower.contains("letsencrypt.email") {
        return CertFailureCause::Config;
    }
    if lower.contains("validation timed out")
        || lower.contains("error sending request")
        || lower.contains("connection reset")
        || lower.contains("timed out")
    {
        return CertFailureCause::CaTransient;
    }
    CertFailureCause::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    // Error texts in the shape the provider produces
    // (`providers.rs` `challenge_error_summary`), with generic names.
    const PORT_80_TIMEOUT: &str = "ACME challenge for app.example.com is no longer valid (order https://acme.example.test/order/1): Order validation failed after 3 attempt(s): [app.example.com Http01] urn:ietf:params:acme:error:connection: 203.0.113.10: Fetching http://app.example.com/.well-known/acme-challenge/abc: Timeout during connect (likely firewall problem)";
    const PORT_80_REFUSED: &str = "Order validation failed after 2 attempt(s): [app.example.com Http01] urn:ietf:params:acme:error:connection: 203.0.113.10: Fetching http://app.example.com/.well-known/acme-challenge/abc: Connection refused";
    const CDN_ERROR_PAGE: &str = "Order validation failed after 2 attempt(s): [app.example.com Http01] urn:ietf:params:acme:error:unauthorized: 198.51.100.7: Invalid response from http://app.example.com/.well-known/acme-challenge/abc: 522";
    const NXDOMAIN: &str = "Order validation failed after 1 attempt(s): [app.example.com Http01] urn:ietf:params:acme:error:dns: DNS problem: NXDOMAIN looking up A for app.example.com";

    #[test]
    fn reachability_failures_are_classified_by_their_urn() {
        assert_eq!(classify(PORT_80_TIMEOUT), CertFailureCause::Unreachable);
        assert_eq!(classify(PORT_80_REFUSED), CertFailureCause::Unreachable);
        assert_eq!(classify(CDN_ERROR_PAGE), CertFailureCause::UpstreamAnswered);
        assert_eq!(classify(NXDOMAIN), CertFailureCause::DnsLookup);
    }

    #[test]
    fn hostname_and_path_words_do_not_decide_the_cause() {
        // Regression: the substring matcher read "dns" in the hostname and
        // "challenge" in the token path as the cause.
        let text = "[mydns.example.com Http01] urn:ietf:params:acme:error:connection: Fetching http://mydns.example.com/.well-known/acme-challenge/abc: Connection refused";
        assert_eq!(classify(text), CertFailureCause::Unreachable);
    }

    #[test]
    fn a_rate_limit_anywhere_wins() {
        let text = "[a.example.com Http01] urn:ietf:params:acme:error:connection: refused; urn:ietf:params:acme:error:rateLimited: too many failed authorizations recently";
        assert_eq!(classify(text), CertFailureCause::RateLimited);
        assert_eq!(
            classify(
                "API error: too many certificates already issued, retry after 2099-01-02T03:04:05Z"
            ),
            CertFailureCause::RateLimited
        );
    }

    #[test]
    fn the_first_recognised_challenge_problem_names_the_cause() {
        let text = "urn:ietf:params:acme:error:somethingNew: x; urn:ietf:params:acme:error:caa: CAA record forbids issuance; urn:ietf:params:acme:error:dns: y";
        assert_eq!(classify(text), CertFailureCause::CaaForbids);
    }

    #[test]
    fn redirects_to_a_foreign_https_endpoint_are_upstream_answers() {
        let text =
            "urn:ietf:params:acme:error:tls: 198.51.100.7: remote error: tls: handshake failure";
        assert_eq!(classify(text), CertFailureCause::UpstreamAnswered);
        let text =
            "urn:ietf:params:acme:error:incorrectResponse: the key authorization did not match";
        assert_eq!(classify(text), CertFailureCause::UpstreamAnswered);
    }

    #[test]
    fn failures_that_never_reached_validation_are_classified_by_their_stage() {
        assert_eq!(
            classify("On-demand TLS issuance failed for a.example.com (dns_failure): DNS preflight: DNS lookup for a.example.com returned no addresses"),
            CertFailureCause::DnsLookup
        );
        assert_eq!(
            classify("no ACME contact email configured for on-demand issuance"),
            CertFailureCause::Config
        );
        assert_eq!(
            classify("Challenge failed: Order validation timed out after 6 attempts"),
            CertFailureCause::CaTransient
        );
        assert_eq!(
            classify(
                "ACME error: error sending request for url (https://acme.example.test/directory)"
            ),
            CertFailureCause::CaTransient
        );
        assert_eq!(
            classify("urn:ietf:params:acme:error:serverInternal: oops"),
            CertFailureCause::CaTransient
        );
        assert_eq!(
            classify("something nobody anticipated"),
            CertFailureCause::Unknown
        );
        assert_eq!(classify(""), CertFailureCause::Unknown);
    }

    #[test]
    fn only_transient_causes_justify_an_automatic_retry() {
        let transient: Vec<_> = CertFailureCause::ALL
            .into_iter()
            .filter(|c| c.is_transient())
            .collect();
        assert_eq!(
            transient,
            vec![CertFailureCause::RateLimited, CertFailureCause::CaTransient]
        );
    }

    #[test]
    fn stored_values_round_trip_and_unknown_values_degrade() {
        for cause in CertFailureCause::ALL {
            assert_eq!(CertFailureCause::from_stored(cause.as_str()), cause);
            // The API shape matches the stored value.
            assert_eq!(
                serde_json::to_value(cause).unwrap(),
                serde_json::Value::String(cause.as_str().to_string())
            );
        }
        assert_eq!(
            CertFailureCause::from_stored("written_by_a_newer_temps"),
            CertFailureCause::Unknown
        );
    }

    #[test]
    fn an_unverified_dns_01_proof_is_a_txt_record_mismatch_not_an_upstream_answer() {
        let dns_proof = "[app.example.com Dns01] urn:ietf:params:acme:error:unauthorized: \
                         Incorrect TXT record \"stale-value\" found at _acme-challenge.app.example.com";
        assert_eq!(
            classify_for(dns_proof, &ChallengeType::Dns01),
            CertFailureCause::DnsRecordMismatch
        );
        // The same problem type on HTTP-01 is still something else answering.
        assert_eq!(
            classify_for(
                "urn:ietf:params:acme:error:unauthorized: Invalid response from \
                 http://app.example.com/.well-known/acme-challenge/abc: 404",
                &ChallengeType::Http01
            ),
            CertFailureCause::UpstreamAnswered
        );
        assert_eq!(
            classify_for(
                "urn:ietf:params:acme:error:incorrectResponse: wrong value",
                &ChallengeType::Dns01
            ),
            CertFailureCause::DnsRecordMismatch
        );
    }

    #[test]
    fn dns_01_keeps_every_other_cause() {
        assert_eq!(
            classify_for(
                "urn:ietf:params:acme:error:dns: SERVFAIL looking up TXT for _acme-challenge.app.example.com",
                &ChallengeType::Dns01
            ),
            CertFailureCause::DnsLookup
        );
        assert_eq!(
            classify_for(
                "urn:ietf:params:acme:error:unauthorized: no TXT record \
                 (urn:ietf:params:acme:error:rateLimited)",
                &ChallengeType::Dns01
            ),
            CertFailureCause::RateLimited
        );
        assert_eq!(
            classify_for(
                "urn:ietf:params:acme:error:caa: CAA forbids",
                &ChallengeType::Dns01
            ),
            CertFailureCause::CaaForbids
        );
    }

    #[test]
    fn legacy_categories_keep_the_existing_console_labels() {
        assert_eq!(
            CertFailureCause::Unreachable.legacy_category(),
            "challenge_mismatch"
        );
        assert_eq!(CertFailureCause::DnsLookup.legacy_category(), "dns_failure");
        assert_eq!(
            CertFailureCause::DnsRecordMismatch.legacy_category(),
            "dns_failure"
        );
        assert_eq!(
            CertFailureCause::RateLimited.legacy_category(),
            "rate_limited"
        );
    }
}
