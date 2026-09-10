// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Request-side helpers shared by the five public analytics ingest handlers
//! (ADR-040 §3).
//!
//! `temps-analytics-events`, `temps-analytics-performance` and
//! `temps-analytics-session-replay` all implement the same auth precedence:
//!
//! ```text
//! 1. key := header X-Temps-Analytics-Key ?? query param temps_key
//! 2. if key present:
//!    2a. resolve(key); miss/inactive/malformed -> 401, NEVER fall through to Host
//!    2b. non-empty allowed_origins -> require an exact Origin match, else 403
//!    2c. rate limit on key_id -> 429 when exceeded
//!    2d. scope := the key's (project, environment, deployment); Host is not
//!        consulted for resolution
//! 3. else: today's Host/route-table behaviour, unchanged
//! ```
//!
//! Steps 2a–2d live here, in [`resolve_keyed_ingest_scope`], so all five
//! endpoints answer with identical status codes and identical RFC 7807 bodies.
//! Step 3 stays in each handler, because the "no route" and "route without a
//! project" outcomes differ per endpoint and must not be homogenised.
//!
//! # Why this header, and why a query param too
//!
//! `X-Temps-Analytics-Key` is deliberately **not** `X-Temps-Api-Key` (the OTel
//! ingest header, which carries `tk_` admin secrets) and **not**
//! `Authorization`. Sharing a header name between a secret and a value designed
//! to be pasted into a public JS bundle invites an operator to paste the wrong
//! one — and have it work. The two mechanisms stay structurally separate: no
//! function here ever consults `api_keys`, `deployment_tokens` or `project_dsns`,
//! and `AnalyticsIngestKeyService::resolve` rejects anything without the `pa_`
//! prefix before it reaches a query.
//!
//! The `?temps_key=` fallback is not optional: `navigator.sendBeacon`, which the
//! browser SDK uses for the page-unload `page_leave` event and for
//! `/_temps/speed/update`, cannot set custom headers. Header-only support would
//! silently drop exactly the unload-path events that matter most.

use axum::http::{HeaderMap, StatusCode};
use temps_core::error_builder::ErrorBuilder;
use temps_core::problemdetails::Problem;
use tracing::{error, warn};

use super::rate_limiter::AnalyticsIngestRateLimiter;
use super::service::AnalyticsIngestKeyService;
use super::types::ResolvedIngestScope;

/// Header carrying an analytics ingest key. Lowercase because `HeaderMap`
/// lookups are case-insensitive only when the probe is already lowercase.
pub const ANALYTICS_INGEST_KEY_HEADER: &str = "x-temps-analytics-key";

/// Query-string fallback for clients that cannot set headers (`sendBeacon`).
pub const ANALYTICS_INGEST_KEY_QUERY_PARAM: &str = "temps_key";

/// Extract the presented ingest key: header first, then query param.
///
/// Never the request body — a credential in a JSON body cannot be inspected
/// before the body is buffered, and `sendBeacon`'s `Blob` form cannot carry one
/// without content-type games.
///
/// `raw_query` is the un-decoded query string (axum's
/// [`axum::extract::RawQuery`]). No percent-decoding is applied: a well-formed
/// key is `pa_` + 64 lowercase hex characters, all of which are unreserved, so
/// an encoded value is by definition not one of ours and correctly fails to
/// resolve.
pub fn extract_analytics_key(headers: &HeaderMap, raw_query: Option<&str>) -> Option<String> {
    let from_header = headers
        .get(ANALYTICS_INGEST_KEY_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty());

    if let Some(value) = from_header {
        return Some(value.to_string());
    }

    raw_query.and_then(key_from_query)
}

fn key_from_query(raw_query: &str) -> Option<String> {
    raw_query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        if name != ANALYTICS_INGEST_KEY_QUERY_PARAM {
            return None;
        }
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_string())
    })
}

/// Whether `origin` satisfies a key's `allowed_origins` list.
///
/// * `None` or an empty list — any origin is permitted and no check is made,
///   including when the request carries no `Origin` header at all (a `curl` or
///   server-side caller).
/// * A non-empty list — the request **must** carry an `Origin` that exactly
///   matches one entry on scheme, host and port. Host comparison is
///   case-insensitive (DNS is); port and scheme are compared as written, with
///   scheme also case-insensitive per RFC 3986.
///
/// This is a browser-enforced convenience control, not authentication: a
/// non-browser client chooses its own `Origin` or omits it. It exists to stop a
/// copy-pasted key being used casually from another site, nothing more.
pub fn is_origin_allowed(allowed_origins: Option<&[String]>, origin: Option<&str>) -> bool {
    let Some(allowed) = allowed_origins.filter(|list| !list.is_empty()) else {
        return true;
    };

    let Some(origin) = origin.map(str::trim).filter(|value| !value.is_empty()) else {
        // The list is non-empty and the browser sent nothing to match it
        // against — fail closed rather than treating "absent" as "any".
        return false;
    };

    allowed
        .iter()
        .any(|candidate| origins_match(candidate, origin))
}

fn origins_match(allowed: &str, origin: &str) -> bool {
    let (allowed_scheme, allowed_authority) = split_origin(allowed.trim());
    let (origin_scheme, origin_authority) = split_origin(origin);

    allowed_scheme.eq_ignore_ascii_case(origin_scheme)
        && authorities_match(allowed_authority, origin_authority)
}

fn split_origin(origin: &str) -> (&str, &str) {
    match origin.split_once("://") {
        Some((scheme, authority)) => (scheme, authority),
        None => ("", origin),
    }
}

fn authorities_match(allowed: &str, origin: &str) -> bool {
    let (allowed_host, allowed_port) = split_port(allowed);
    let (origin_host, origin_port) = split_port(origin);

    allowed_host.eq_ignore_ascii_case(origin_host) && allowed_port == origin_port
}

/// Split `host[:port]`, tolerating bracketed IPv6 literals (`[::1]`, whose own
/// colons must not be mistaken for a port separator).
fn split_port(authority: &str) -> (&str, &str) {
    match authority.rsplit_once(':') {
        Some((host, port))
            if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) && !host.is_empty() =>
        {
            (host, port)
        }
        _ => (authority, ""),
    }
}

/// 401 for a key that does not resolve to an active row.
///
/// Deliberately does not echo the presented key. The value is public by design,
/// but an error body is the one place it could be reflected back to a third
/// party who did not already have it.
pub fn invalid_ingest_key_problem() -> Problem {
    ErrorBuilder::new(StatusCode::UNAUTHORIZED)
        .title("Invalid analytics ingest key")
        .detail(
            "The X-Temps-Analytics-Key header or temps_key query parameter does not match an \
             active analytics ingest key. Check the key in your project's analytics settings, \
             or remove it to fall back to Host-based resolution.",
        )
        .build()
}

/// 403 for a key whose `allowed_origins` does not admit this request's `Origin`.
pub fn origin_not_allowed_problem(origin: Option<&str>) -> Problem {
    let detail = match origin {
        Some(origin) => format!(
            "Origin {origin} is not in this analytics ingest key's allowed_origins list. Add it \
             to the key, or clear the list to allow any origin."
        ),
        None => "This analytics ingest key restricts allowed_origins, but the request carried no \
                 Origin header. Send the request from a browser origin on the key's list, or \
                 clear the list to allow any origin."
            .to_string(),
    };

    ErrorBuilder::new(StatusCode::FORBIDDEN)
        .title("Origin not allowed for this analytics ingest key")
        .detail(detail)
        .build()
}

/// 429 once a key exceeds its per-minute budget.
pub fn ingest_rate_limited_problem(limit_per_minute: Option<i32>) -> Problem {
    let detail = match limit_per_minute {
        Some(limit) => format!(
            "This analytics ingest key exceeded its limit of {limit} requests per minute. Raise \
             rate_limit_per_minute on the key, or clear it for no limit."
        ),
        None => "This analytics ingest key exceeded its per-minute request limit.".to_string(),
    };

    ErrorBuilder::new(StatusCode::TOO_MANY_REQUESTS)
        .title("Analytics ingest rate limit exceeded")
        .detail(detail)
        .build()
}

/// 500 when the key could not be looked up at all.
///
/// Distinct from [`invalid_ingest_key_problem`] on purpose: a self-hosted
/// operator debugging alone must be able to tell "my key is wrong" from "my
/// database is down", and answering 401 for the latter would send them to
/// re-mint a perfectly good key.
fn ingest_key_lookup_failed_problem() -> Problem {
    ErrorBuilder::new(StatusCode::INTERNAL_SERVER_ERROR)
        .title("Analytics ingest key lookup failed")
        .detail(
            "The analytics ingest key could not be validated because of a storage error. This is \
             a server-side fault, not a problem with the key; check the Temps server logs.",
        )
        .build()
}

/// Run steps 2a–2d of the ADR-040 precedence for a presented key.
///
/// On `Ok`, the returned scope **replaces** Host-based resolution entirely: the
/// caller must not consult the route table, and must not merge the two sources.
/// On `Err`, the caller must return the [`Problem`] as-is and must **not** fall
/// back to the Host path — a typo'd key that silently degraded to Host would
/// either mis-attribute data or 404 confusingly, with no way for the operator
/// to tell which.
pub async fn resolve_keyed_ingest_scope(
    key_service: &AnalyticsIngestKeyService,
    rate_limiter: &AnalyticsIngestRateLimiter,
    key: &str,
    origin: Option<&str>,
) -> Result<ResolvedIngestScope, Problem> {
    // 2a-pre. A global, IP-independent backstop against a flood of distinct
    // valid-shaped garbage keys: `resolve()`'s cache only helps on an exact
    // repeated string, so without this, every unique bad key costs a DB
    // query with no limit at all (see `unresolved_budget_exhausted`'s doc).
    if rate_limiter.unresolved_budget_exhausted().await {
        warn!("Rejecting analytics ingest: global unresolved-key rate limit exceeded");
        return Err(ingest_rate_limited_problem(Some(
            super::rate_limiter::UNRESOLVED_KEY_RATE_LIMIT_PER_MINUTE,
        )));
    }

    // 2a. Resolve, or 401. A storage failure is a 500, never a 401.
    let scope = key_service.resolve(key).await.map_err(|e| {
        error!("Failed to resolve an analytics ingest key: {}", e);
        ingest_key_lookup_failed_problem()
    })?;
    let scope = match scope {
        Some(scope) => scope,
        None => {
            rate_limiter.record_unresolved_attempt().await;
            return Err(invalid_ingest_key_problem());
        }
    };

    // 2b. Rate limit, keyed by the row id so cardinality stays bounded by the
    // number of minted keys. Checked *before* the origin allowlist on
    // purpose: the key value is not a secret (it ships in client-side JS by
    // design), so anyone can read it off the target site and hammer the
    // origin check from a disallowed origin. Origin-mismatched requests must
    // still burn the key's budget, or that check becomes an unthrottled loop
    // an attacker can spin forever without ever tripping the rate limiter.
    if !rate_limiter
        .check(scope.key_id, scope.rate_limit_per_minute)
        .await
    {
        warn!(
            key_id = scope.key_id,
            project_id = scope.project_id,
            limit = ?scope.rate_limit_per_minute,
            "Rejecting analytics ingest: key over its per-minute rate limit"
        );
        return Err(ingest_rate_limited_problem(scope.rate_limit_per_minute));
    }

    // 2c. Origin allowlist, when the key carries one.
    if !is_origin_allowed(scope.allowed_origins.as_deref(), origin) {
        warn!(
            key_id = scope.key_id,
            project_id = scope.project_id,
            "Rejecting analytics ingest: Origin not in the key's allowed_origins"
        );
        return Err(origin_not_allowed_problem(origin));
    }

    // 2d. Account for the request. `record_usage` is internally throttled to at
    // most one write per key per 60s, so awaiting it here costs nothing on the
    // common path — and a usage-counter failure must never drop a real event.
    if let Err(e) = key_service.record_usage(scope.key_id).await {
        warn!(
            key_id = scope.key_id,
            "Failed to record analytics ingest key usage: {}", e
        );
    }

    Ok(scope)
}

/// Bounds for a client-generated visitor/session id (see
/// [`resolve_client_identity`]). Wide enough for `crypto.randomUUID()` (36
/// chars) and the SDK's non-crypto fallback shape
/// (`visitor_<timestamp>_<rand>`, ~30 chars), narrow enough that a value this
/// short or long cannot plausibly have come from either.
const MIN_CLIENT_IDENTITY_LEN: usize = 8;
const MAX_CLIENT_IDENTITY_LEN: usize = 64;

/// Whether `id` is shaped like a value the identity SDK helper could have
/// produced. `visitor_id`/`session_id` become unauthenticated `GROUP BY`/join
/// keys once stored, so anything outside this shape — HTML, oversized junk,
/// arbitrary bytes — is rejected rather than stored.
fn is_valid_client_identity(id: &str) -> bool {
    (MIN_CLIENT_IDENTITY_LEN..=MAX_CLIENT_IDENTITY_LEN).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Resolve a visitor/session identity, preferring the Temps-issued, encrypted
/// cookie (tamper-evident) over the SDK's client-generated fallback.
///
/// `keyed` must be `true` only on the ADR-040 keyed-ingest branch — the one
/// case where Temps never serves the page's HTML and so never gets a chance
/// to issue its own cookie. On the Host-resolved branch (`keyed = false`),
/// `payload_value` is never consulted, cookie-absent or not: that branch's
/// identity has always come from a cookie the Temps proxy issues and can
/// verify, and accepting a client-supplied override there — which any caller
/// can trigger simply by omitting the cookie — would let an unauthenticated
/// `/_temps/event` (etc.) request forge another visitor's identity or spray
/// fabricated ones on every Temps-hosted app, not just the cross-origin case
/// this fallback exists for.
pub fn resolve_client_identity(
    cookie: Option<String>,
    payload_value: Option<String>,
    keyed: bool,
) -> Option<String> {
    cookie.or_else(|| {
        if !keyed {
            return None;
        }
        payload_value.filter(|id| is_valid_client_identity(id))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers_with_key(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            ANALYTICS_INGEST_KEY_HEADER,
            HeaderValue::from_str(value).expect("test header value must be valid"),
        );
        headers
    }

    #[test]
    fn extracts_key_from_header() {
        let headers = headers_with_key("pa_abc");
        assert_eq!(
            extract_analytics_key(&headers, None).as_deref(),
            Some("pa_abc")
        );
    }

    #[test]
    fn header_wins_over_query_param() {
        let headers = headers_with_key("pa_from_header");
        assert_eq!(
            extract_analytics_key(&headers, Some("temps_key=pa_from_query")).as_deref(),
            Some("pa_from_header")
        );
    }

    #[test]
    fn falls_back_to_query_param() {
        let headers = HeaderMap::new();
        assert_eq!(
            extract_analytics_key(&headers, Some("temps_key=pa_from_query")).as_deref(),
            Some("pa_from_query")
        );
        // Position in the query string does not matter.
        assert_eq!(
            extract_analytics_key(&headers, Some("a=1&temps_key=pa_x&b=2")).as_deref(),
            Some("pa_x")
        );
    }

    #[test]
    fn ignores_blank_and_absent_keys() {
        assert_eq!(extract_analytics_key(&HeaderMap::new(), None), None);
        // An empty header falls through to the query param rather than
        // masking it.
        assert_eq!(
            extract_analytics_key(&headers_with_key("   "), Some("temps_key=pa_q")).as_deref(),
            Some("pa_q")
        );
        assert_eq!(
            extract_analytics_key(&HeaderMap::new(), Some("temps_key=")),
            None
        );
        assert_eq!(
            extract_analytics_key(&HeaderMap::new(), Some("other=1")),
            None
        );
        // A bare flag with no `=` is not a key.
        assert_eq!(
            extract_analytics_key(&HeaderMap::new(), Some("temps_key")),
            None
        );
    }

    #[test]
    fn query_param_name_must_match_exactly() {
        // Guard against a prefix/suffix match leaking another parameter's value.
        assert_eq!(
            extract_analytics_key(&HeaderMap::new(), Some("xtemps_key=pa_x")),
            None
        );
        assert_eq!(
            extract_analytics_key(&HeaderMap::new(), Some("temps_key_2=pa_x")),
            None
        );
    }

    /// The three ingest crates feed this constant straight into
    /// `HeaderName::from_static`, which panics on a non-lowercase or otherwise
    /// invalid name. Catch a typo here rather than at server startup.
    #[test]
    fn header_constant_is_a_valid_lowercase_header_name() {
        let name = axum::http::HeaderName::from_static(ANALYTICS_INGEST_KEY_HEADER);
        assert_eq!(name.as_str(), ANALYTICS_INGEST_KEY_HEADER);
    }

    #[test]
    fn empty_or_absent_allowed_origins_permit_anything() {
        assert!(is_origin_allowed(None, None));
        assert!(is_origin_allowed(None, Some("https://evil.example")));
        assert!(is_origin_allowed(Some(&[]), Some("https://evil.example")));
        assert!(is_origin_allowed(Some(&[]), None));
    }

    #[test]
    fn non_empty_allowed_origins_require_an_origin_header() {
        let allowed = vec!["https://app.example.com".to_string()];
        assert!(!is_origin_allowed(Some(&allowed), None));
        assert!(!is_origin_allowed(Some(&allowed), Some("")));
        assert!(!is_origin_allowed(Some(&allowed), Some("   ")));
    }

    #[test]
    fn matching_origin_is_allowed() {
        let allowed = vec![
            "https://app.example.com".to_string(),
            "http://localhost:3000".to_string(),
        ];
        assert!(is_origin_allowed(
            Some(&allowed),
            Some("https://app.example.com")
        ));
        assert!(is_origin_allowed(
            Some(&allowed),
            Some("http://localhost:3000")
        ));
    }

    #[test]
    fn host_comparison_is_case_insensitive() {
        let allowed = vec!["https://App.Example.COM".to_string()];
        assert!(is_origin_allowed(
            Some(&allowed),
            Some("https://app.example.com")
        ));
        assert!(is_origin_allowed(
            Some(&allowed),
            Some("HTTPS://APP.EXAMPLE.COM")
        ));
    }

    #[test]
    fn scheme_and_port_must_match_exactly() {
        let allowed = vec!["https://app.example.com".to_string()];
        // Different scheme.
        assert!(!is_origin_allowed(
            Some(&allowed),
            Some("http://app.example.com")
        ));
        // Explicit default port is a different origin string; we do not
        // normalise it away, matching the ADR's "exact match" wording.
        assert!(!is_origin_allowed(
            Some(&allowed),
            Some("https://app.example.com:443")
        ));

        let with_port = vec!["http://localhost:3000".to_string()];
        assert!(!is_origin_allowed(
            Some(&with_port),
            Some("http://localhost:3001")
        ));
        assert!(!is_origin_allowed(
            Some(&with_port),
            Some("http://localhost")
        ));
    }

    #[test]
    fn subdomains_and_suffixes_do_not_match() {
        let allowed = vec!["https://example.com".to_string()];
        assert!(!is_origin_allowed(
            Some(&allowed),
            Some("https://evil.example.com")
        ));
        assert!(!is_origin_allowed(
            Some(&allowed),
            Some("https://example.com.evil.test")
        ));
        assert!(!is_origin_allowed(Some(&allowed), Some("null")));
    }

    #[test]
    fn ipv6_literals_are_not_split_on_their_own_colons() {
        let allowed = vec!["http://[::1]:8080".to_string()];
        assert!(is_origin_allowed(Some(&allowed), Some("http://[::1]:8080")));
        assert!(!is_origin_allowed(Some(&allowed), Some("http://[::1]")));

        let no_port = vec!["http://[::1]".to_string()];
        assert!(is_origin_allowed(Some(&no_port), Some("http://[::1]")));
        assert!(!is_origin_allowed(Some(&no_port), Some("http://[::2]")));
    }

    #[test]
    fn problem_bodies_carry_the_documented_statuses() {
        assert_eq!(
            invalid_ingest_key_problem().status_code,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            origin_not_allowed_problem(Some("https://evil.example")).status_code,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            origin_not_allowed_problem(None).status_code,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            ingest_rate_limited_problem(Some(600)).status_code,
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            ingest_key_lookup_failed_problem().status_code,
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn invalid_key_problem_never_echoes_a_key() {
        let problem = invalid_ingest_key_problem();
        let rendered = serde_json::to_string(&problem.body).expect("problem body must serialize");
        assert!(!rendered.contains("pa_"), "{rendered}");
    }

    #[test]
    fn resolve_client_identity_prefers_cookie_over_payload() {
        assert_eq!(
            resolve_client_identity(
                Some("cookie-id".to_string()),
                Some("payload-id".to_string()),
                true,
            ),
            Some("cookie-id".to_string())
        );
    }

    #[test]
    fn resolve_client_identity_falls_back_to_payload_only_when_keyed() {
        assert_eq!(
            resolve_client_identity(None, Some("payload-id-1".to_string()), true),
            Some("payload-id-1".to_string())
        );
    }

    #[test]
    fn resolve_client_identity_ignores_payload_when_not_keyed() {
        // The Host-resolved branch must stay cookie-only: an absent cookie
        // there is never grounds to trust a client-supplied value, since any
        // unauthenticated caller can simply omit the cookie.
        assert_eq!(
            resolve_client_identity(None, Some("payload-id-1".to_string()), false),
            None
        );
    }

    #[test]
    fn resolve_client_identity_none_when_both_absent() {
        assert_eq!(resolve_client_identity(None, None, true), None);
        assert_eq!(resolve_client_identity(None, None, false), None);
    }

    #[test]
    fn resolve_client_identity_rejects_malformed_payload_values() {
        // Too short.
        assert_eq!(
            resolve_client_identity(None, Some("short".to_string()), true),
            None
        );
        // Too long.
        let oversized = "a".repeat(65);
        assert_eq!(resolve_client_identity(None, Some(oversized), true), None);
        // Disallowed characters — the values this exists to store are keys
        // into `GROUP BY`/join queries, never HTML or query-string metacharacters.
        assert_eq!(
            resolve_client_identity(None, Some("<script>alert(1)</script>".to_string()), true),
            None
        );
        assert_eq!(
            resolve_client_identity(None, Some("has spaces here".to_string()), true),
            None
        );
    }

    #[test]
    fn resolve_client_identity_accepts_the_sdks_id_shapes() {
        // crypto.randomUUID() output.
        assert_eq!(
            resolve_client_identity(
                None,
                Some("550e8400-e29b-41d4-a716-446655440000".to_string()),
                true,
            ),
            Some("550e8400-e29b-41d4-a716-446655440000".to_string())
        );
        // The non-crypto fallback shape: `prefix_timestamp_rand`.
        assert_eq!(
            resolve_client_identity(
                None,
                Some("visitor_1699999999999_abc123xyz".to_string()),
                true
            ),
            Some("visitor_1699999999999_abc123xyz".to_string())
        );
    }
}
