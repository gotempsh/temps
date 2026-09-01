// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Credential redaction for persisted access logs.
//!
//! The proxy necessarily *sees* every credential a client sends — a `Cookie`
//! header has to reach the upstream, and an OAuth callback's `?code=` has to be
//! forwarded verbatim or the flow breaks. What must never happen is that those
//! values get **written down**: `proxy_logs` rows are long-lived, land in
//! backups, and are readable by anyone with proxy-log read access in the
//! console.
//!
//! So redaction lives at the persistence boundary
//! ([`ProxyLogBatchHandle::send_or_drop`](crate::service::proxy_log_batch_writer::ProxyLogBatchHandle::send_or_drop)),
//! not in [`ProxyContext`](crate::proxy::ProxyContext). That split is
//! deliberate and load-bearing:
//!
//! - `ctx.query_string` is used **functionally** — to build redirect targets,
//!   the preview-wall `next` parameter, and ephemeral-host redirects. Redacting
//!   it in place would rewrite a real OAuth callback's `?code=` to
//!   `[REDACTED]` and break the login it was meant to protect.
//! - `ctx.request_headers` is read for `content-length` and `x-cache`.
//!
//! Values are replaced rather than dropped: knowing the client *sent* a
//! `Cookie` is useful when debugging, knowing its contents is not. Keys are
//! preserved so the shape of the request stays legible.
//!
//! ## Cost
//!
//! One pass over a small flat map plus one scan of the query string, per
//! *logged* request (static assets are already filtered out upstream by
//! `should_log_request`). [`redact_query_string`] returns [`Cow::Borrowed`]
//! when nothing matches — the overwhelmingly common case — so the hot path
//! allocates nothing extra. This runs immediately before the entry is
//! serialized for storage, work the batch writer already does.

use std::borrow::Cow;
use std::collections::BTreeMap;

use crate::service::proxy_log_service::CreateProxyLogRequest;

/// Placeholder written in place of a credential value.
pub const REDACTED: &str = "[REDACTED]";

/// Header names whose values are credentials or session material.
///
/// Compared case-insensitively, though in practice `http::HeaderName` is always
/// lowercase so the fast path is a plain byte comparison. Sorted purely for
/// readability — the list is short enough that a linear scan beats the branch
/// cost of a binary search.
/// `www-authenticate` / `proxy-authenticate` are deliberately absent: they are
/// challenges telling the client *how* to authenticate, not credentials, and
/// redacting them would cost debugging information for no security gain.
const SENSITIVE_HEADERS: &[&str] = &[
    "apikey",
    "authentication",
    "authorization",
    "cookie",
    "proxy-authorization",
    "set-cookie",
    "x-access-token",
    "x-amz-security-token",
    "x-api-key",
    "x-auth",
    "x-auth-token",
    "x-csrf-token",
    "x-refresh-token",
    "x-session-token",
    "x-xsrf-token",
];

/// Header values that are URLs, and therefore carry a query string that needs
/// the same treatment as the request's own. A `Location:` on a 302 is exactly
/// how an OAuth `code` ends up in a response header.
const URL_VALUED_HEADERS: &[&str] = &["content-location", "location", "referer", "referrer"];

/// Query-parameter names whose values are credentials.
///
/// Deliberately broad: a false positive costs one unreadable debugging value,
/// a false negative writes a live credential to disk. `code` and `state` are
/// included because the OAuth authorization-code callback is the single most
/// common way a usable secret reaches an access log.
const SENSITIVE_QUERY_PARAMS: &[&str] = &[
    "access_token",
    "api_key",
    "apikey",
    "assertion",
    "auth",
    "authorization",
    "client_secret",
    "code",
    "code_challenge",
    "code_verifier",
    "credential",
    "id_token",
    "id_token_hint",
    "key",
    "otp",
    "passwd",
    "password",
    "pwd",
    "refresh_token",
    "secret",
    "session",
    "session_token",
    "sig",
    "signature",
    "state",
    "token",
    // AWS SigV4 presigned URLs (`X-Amz-*`) and Azure SAS (`sig`/`se`/`sv`).
    "x-amz-credential",
    "x-amz-security-token",
    "x-amz-signature",
];

/// Case-insensitive comparison that also treats `-` and `_` as equivalent.
///
/// Without this, `?api-key=` slips past an `api_key` entry and `?access-token=`
/// past `access_token` — both spellings are in real-world use, and listing every
/// permutation by hand is exactly the kind of list that grows a hole. Allocates
/// nothing, so it stays usable on the request path.
fn matches_name(name: &str, candidate: &str) -> bool {
    if name.len() != candidate.len() {
        return false;
    }
    name.bytes().zip(candidate.bytes()).all(|(a, b)| {
        let norm = |c: u8| match c {
            b'-' => b'_',
            other => other.to_ascii_lowercase(),
        };
        norm(a) == norm(b)
    })
}

fn is_sensitive_header(name: &str) -> bool {
    SENSITIVE_HEADERS
        .iter()
        .any(|candidate| matches_name(name, candidate))
}

fn is_url_valued_header(name: &str) -> bool {
    URL_VALUED_HEADERS
        .iter()
        .any(|candidate| matches_name(name, candidate))
}

fn is_sensitive_query_param(name: &str) -> bool {
    SENSITIVE_QUERY_PARAMS
        .iter()
        .any(|candidate| matches_name(name, candidate))
}

/// Replace the values of credential-bearing query parameters with
/// [`REDACTED`], preserving parameter order, names, and separators.
///
/// Returns [`Cow::Borrowed`] when there is nothing to redact, so the common
/// case costs a scan and no allocation. Parameters are matched on their raw
/// (still percent-encoded) name; encoded parameter *names* are vanishingly rare
/// in practice, and a missed match is a redaction miss rather than corruption.
pub fn redact_query_string(query: &str) -> Cow<'_, str> {
    let needs_redaction = query.split('&').any(|pair| {
        let name = pair.split('=').next().unwrap_or(pair);
        is_sensitive_query_param(name)
    });

    if !needs_redaction {
        return Cow::Borrowed(query);
    }

    let mut out = String::with_capacity(query.len());
    for (i, pair) in query.split('&').enumerate() {
        if i > 0 {
            out.push('&');
        }
        let name = pair.split('=').next().unwrap_or(pair);
        if is_sensitive_query_param(name) {
            out.push_str(name);
            out.push('=');
            out.push_str(REDACTED);
        } else {
            out.push_str(pair);
        }
    }
    Cow::Owned(out)
}

/// Apply [`redact_query_string`] to the query component of an absolute or
/// relative URL, leaving scheme/host/path untouched.
pub fn redact_url(url: &str) -> Cow<'_, str> {
    let Some(split) = url.find('?') else {
        return Cow::Borrowed(url);
    };
    // Split off a fragment so a `?` inside it isn't treated as a query.
    let (base, query_and_fragment) = url.split_at(split + 1);
    let (query, fragment) = match query_and_fragment.find('#') {
        Some(i) => query_and_fragment.split_at(i),
        None => (query_and_fragment, ""),
    };

    match redact_query_string(query) {
        Cow::Borrowed(_) => Cow::Borrowed(url),
        Cow::Owned(redacted) => Cow::Owned(format!("{base}{redacted}{fragment}")),
    }
}

/// Redact a captured header map in place.
///
/// Credential headers keep their name and lose their value; URL-valued headers
/// (`Location`, `Referer`) keep their value but have its query string redacted.
pub fn redact_headers(headers: &mut BTreeMap<String, String>) {
    for (name, value) in headers.iter_mut() {
        if is_sensitive_header(name) {
            *value = REDACTED.to_string();
        } else if is_url_valued_header(name) {
            if let Cow::Owned(redacted) = redact_url(value) {
                *value = redacted;
            }
        }
    }
}

/// Redact a header map already serialized to JSON.
///
/// The proxy serializes captured headers to `serde_json::Value` at capture time
/// (a flat string→string object). Redacting the parsed form avoids a
/// round-trip through `BTreeMap`. Non-object values are returned untouched —
/// there is nothing meaningful to redact and silently dropping data would be
/// worse than passing it through.
pub fn redact_headers_json(value: &mut serde_json::Value) {
    let Some(map) = value.as_object_mut() else {
        return;
    };

    for (name, entry) in map.iter_mut() {
        let Some(text) = entry.as_str() else {
            continue;
        };

        if is_sensitive_header(name) {
            *entry = serde_json::Value::String(REDACTED.to_string());
        } else if is_url_valued_header(name) {
            if let Cow::Owned(redacted) = redact_url(text) {
                *entry = serde_json::Value::String(redacted);
            }
        }
    }
}

/// Strip credentials from every field of a log entry that can carry one, in
/// place, before the entry is persisted.
///
/// Four fields are reachable by a secret:
/// - `query_string` — OAuth `?code=`/`?state=`, `?token=`, presigned signatures
/// - `request_headers` — `Cookie`, `Authorization`, `X-Api-Key`
/// - `response_headers` — `Set-Cookie`, and `Location` on an OAuth redirect
/// - `referrer` — the previous page's URL, query string included
///
/// Called from both persistence entry points
/// ([`send_or_drop`](crate::service::proxy_log_batch_writer::ProxyLogBatchHandle::send_or_drop)
/// for the batched hot path, and
/// [`ProxyLogService::create`](crate::service::proxy_log_service::ProxyLogService::create)
/// for the direct insert), so neither can persist a secret. Idempotent —
/// redacting an already-redacted entry is a no-op.
///
/// `path` is deliberately *not* touched: a secret embedded in a path segment
/// (`/reset/<token>`) can't be told apart from a legitimate identifier without
/// per-application knowledge, and blanket-redacting path segments would destroy
/// the primary axis these logs are read along. A known limitation, rather than
/// something silently half-solved.
pub fn redact_log_entry(request: &mut CreateProxyLogRequest) {
    if let Some(query) = request.query_string.as_ref() {
        if let Cow::Owned(redacted) = redact_query_string(query) {
            request.query_string = Some(redacted);
        }
    }

    if let Some(referrer) = request.referrer.as_ref() {
        if let Cow::Owned(redacted) = redact_url(referrer) {
            request.referrer = Some(redacted);
        }
    }

    if let Some(headers) = request.request_headers.as_mut() {
        redact_headers_json(headers);
    }

    if let Some(headers) = request.response_headers.as_mut() {
        redact_headers_json(headers);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── Query strings ───────────────────────────────────────────────────────

    #[test]
    fn redacts_oauth_authorization_code_and_state() {
        // The exact shape that motivated this fix: a GitHub OAuth callback.
        let query = "code=abc123def456&state=synthetic-csrf-state";
        assert_eq!(
            redact_query_string(query),
            "code=[REDACTED]&state=[REDACTED]"
        );
    }

    #[test]
    fn preserves_non_sensitive_parameters_verbatim() {
        let query = "page=2&sort=desc&filter=active";
        let redacted = redact_query_string(query);
        assert_eq!(redacted, query);
        // No allocation when there is nothing to redact.
        assert!(matches!(redacted, Cow::Borrowed(_)));
    }

    #[test]
    fn redacts_only_the_sensitive_parameter_in_a_mixed_query() {
        assert_eq!(
            redact_query_string("page=2&token=secret-value&sort=asc"),
            "page=2&token=[REDACTED]&sort=asc"
        );
    }

    #[test]
    fn matches_parameter_names_case_insensitively() {
        assert_eq!(
            redact_query_string("Token=abc&ACCESS_TOKEN=def"),
            "Token=[REDACTED]&ACCESS_TOKEN=[REDACTED]"
        );
    }

    #[test]
    fn matches_hyphen_and_underscore_spellings_interchangeably() {
        // Both spellings occur in the wild; listing every permutation by hand is
        // how a denylist grows a hole.
        assert_eq!(
            redact_query_string("api-key=abc&access-token=def&refresh_token=ghi"),
            "api-key=[REDACTED]&access-token=[REDACTED]&refresh_token=[REDACTED]"
        );
    }

    #[test]
    fn does_not_over_match_on_length_or_prefix() {
        // Guards the normalizing comparison: these are NOT credentials and must
        // survive, or the logs lose their most-used fields.
        let query = "code_of_conduct=1&stateful=no&tokenizer=bpe&keyboard=us";
        let redacted = redact_query_string(query);
        assert_eq!(redacted, query);
        assert!(matches!(redacted, Cow::Borrowed(_)));
    }

    #[test]
    fn redacts_valueless_and_empty_sensitive_parameters() {
        // `?token` (no `=`) and `?token=` must not leak the shape either, and
        // must not panic on the missing value.
        assert_eq!(redact_query_string("token"), "token=[REDACTED]");
        assert_eq!(redact_query_string("token="), "token=[REDACTED]");
    }

    #[test]
    fn redacts_every_occurrence_of_a_repeated_parameter() {
        assert_eq!(
            redact_query_string("token=one&token=two"),
            "token=[REDACTED]&token=[REDACTED]"
        );
    }

    #[test]
    fn preserves_values_containing_equals_signs() {
        // Base64 padding means `=` inside a value is routine.
        assert_eq!(
            redact_query_string("next=a=b&token=zzz=="),
            "next=a=b&token=[REDACTED]"
        );
    }

    #[test]
    fn handles_empty_query_string() {
        assert_eq!(redact_query_string(""), "");
    }

    #[test]
    fn redacts_presigned_url_signatures() {
        let query = "X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Signature=deadbeef";
        assert_eq!(
            redact_query_string(query),
            "X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Signature=[REDACTED]"
        );
    }

    // ── URLs ────────────────────────────────────────────────────────────────

    #[test]
    fn redacts_query_inside_a_url_leaving_path_intact() {
        assert_eq!(
            redact_url("https://example.test/api/integrations/callback?code=abc&ok=1"),
            "https://example.test/api/integrations/callback?code=[REDACTED]&ok=1"
        );
    }

    #[test]
    fn leaves_url_without_query_untouched() {
        let url = "https://example.test/plain/path";
        assert!(matches!(redact_url(url), Cow::Borrowed(_)));
    }

    #[test]
    fn preserves_url_fragment_after_redaction() {
        assert_eq!(
            redact_url("https://example.test/p?token=abc#section"),
            "https://example.test/p?token=[REDACTED]#section"
        );
    }

    // ── Header maps ─────────────────────────────────────────────────────────

    #[test]
    fn redacts_cookie_and_authorization_but_keeps_their_names() {
        let mut headers = BTreeMap::from([
            ("cookie".to_string(), "session=super-secret".to_string()),
            (
                "authorization".to_string(),
                "Bearer tok_live_xyz".to_string(),
            ),
            ("user-agent".to_string(), "curl/8.4.0".to_string()),
        ]);

        redact_headers(&mut headers);

        assert_eq!(headers["cookie"], REDACTED);
        assert_eq!(headers["authorization"], REDACTED);
        // Non-sensitive headers must survive — this data is the reason the
        // feature exists.
        assert_eq!(headers["user-agent"], "curl/8.4.0");
    }

    #[test]
    fn redacts_set_cookie_on_the_response_side() {
        let mut headers = BTreeMap::from([(
            "set-cookie".to_string(),
            "sid=abc; HttpOnly; Secure".to_string(),
        )]);
        redact_headers(&mut headers);
        assert_eq!(headers["set-cookie"], REDACTED);
    }

    #[test]
    fn redacts_query_inside_location_header() {
        // A 302 to an OAuth callback leaks the code through Location.
        let mut headers = BTreeMap::from([(
            "location".to_string(),
            "https://example.test/cb?code=abc&state=xyz".to_string(),
        )]);
        redact_headers(&mut headers);
        assert_eq!(
            headers["location"],
            "https://example.test/cb?code=[REDACTED]&state=[REDACTED]"
        );
    }

    #[test]
    fn redacts_header_names_case_insensitively() {
        let mut headers = BTreeMap::from([("Authorization".to_string(), "Bearer abc".to_string())]);
        redact_headers(&mut headers);
        assert_eq!(headers["Authorization"], REDACTED);
    }

    // ── JSON header maps ────────────────────────────────────────────────────

    #[test]
    fn redacts_json_encoded_headers() {
        let mut value = json!({
            "cookie": "session=secret",
            "x-api-key": "key_live_abc",
            "accept": "text/html",
        });

        redact_headers_json(&mut value);

        assert_eq!(value["cookie"], json!(REDACTED));
        assert_eq!(value["x-api-key"], json!(REDACTED));
        assert_eq!(value["accept"], json!("text/html"));
    }

    #[test]
    fn redacts_location_query_in_json_headers() {
        let mut value = json!({ "location": "/cb?code=abc" });
        redact_headers_json(&mut value);
        assert_eq!(value["location"], json!("/cb?code=[REDACTED]"));
    }

    #[test]
    fn leaves_non_object_json_untouched() {
        let mut value = json!("not-a-map");
        redact_headers_json(&mut value);
        assert_eq!(value, json!("not-a-map"));

        let mut null = serde_json::Value::Null;
        redact_headers_json(&mut null);
        assert_eq!(null, serde_json::Value::Null);
    }

    #[test]
    fn ignores_non_string_header_values() {
        let mut value = json!({ "cookie": 42 });
        redact_headers_json(&mut value);
        // Nothing to redact in a non-string; must not panic or corrupt.
        assert_eq!(value, json!({ "cookie": 42 }));
    }
}
