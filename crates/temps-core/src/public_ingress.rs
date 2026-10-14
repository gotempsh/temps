// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Public ingress surface (ADR-051).
//!
//! `external_url` is where operators reach the console, and that host is
//! increasingly private. Third parties that call back into Temps (mail
//! clients fetching a tracking pixel, git providers delivering a push, SNS
//! delivering a bounce) are on the open internet and cannot be allowlisted,
//! so the URL they are given is resolved separately: `public_ingress_url`
//! when the operator set one, `external_url` otherwise.
//!
//! This module owns the validation of that setting and the judgement of
//! whether the resulting host can be reached by a third party at all.

use crate::app_settings::AppSettings;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use thiserror::Error;
use utoipa::ToSchema;

/// Base URL third parties are given when neither `public_ingress_url` nor
/// `external_url` is configured. Matches `get_external_url_or_default`, so an
/// unconfigured install keeps emitting the URLs it always has.
pub const DEFAULT_PUBLIC_INGRESS_URL: &str = "http://localho.st";

/// Console path of the setting, for onboarding links.
pub const PUBLIC_INGRESS_SETUP_PATH: &str = "/settings#public-ingress";

/// Why a submitted `public_ingress_url` was refused.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PublicIngressUrlError {
    #[error("Public ingress URL '{url}' must start with http:// or https://")]
    InvalidScheme { url: String },

    #[error("Public ingress URL '{url}' is not a valid URL: {reason}")]
    Unparsable { url: String, reason: String },

    #[error(
        "Public ingress URL '{url}' must be a bare origin such as https://hooks.example.com: \
         remove the {part}"
    )]
    NotAnOrigin { url: String, part: &'static str },

    #[error("Public ingress URL host '{host}' cannot be reached by third parties: {reason}")]
    UnroutableHost { host: String, reason: String },

    #[error(
        "Public ingress URL host '{host}' is the preview domain, which is reserved for \
         deployment URLs. Use a dedicated hostname outside it"
    )]
    PreviewDomain { host: String },

    #[error(
        "Public ingress URL host '{host}' is inside the preview domain '{preview_domain}'. \
         Temps generates the hostnames directly under it (environments, deployments, \
         services and workspace previews), so a callback hostname there can collide with \
         an application now or later. Use a hostname outside '{preview_domain}'"
    )]
    InsidePreviewDomain {
        host: String,
        preview_domain: String,
    },
}

/// Where the effective ingress URL came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PublicIngressSource {
    /// The operator set `public_ingress_url`.
    Explicit,
    /// `public_ingress_url` is unset, so callbacks use `external_url`.
    ExternalUrlFallback,
}

/// The URL third parties are given, and whether Temps can tell from its own
/// state that they cannot reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicIngressResolution {
    /// Base URL with no trailing slash.
    pub effective_url: String,
    pub source: PublicIngressSource,
    /// Set when the effective host is provably unreachable for a third party
    /// (loopback, a private address, a local-only name). `None` does not prove
    /// reachability: a firewall or VPN in front of the host is invisible here.
    pub unreachable_reason: Option<String>,
}

/// Why `host` cannot be reached from the open internet, or `None` when
/// nothing about the name itself rules it out.
///
/// Only the literal host is judged. A public-looking name that resolves to a
/// private address is not detected: resolving it here would make a settings
/// save depend on DNS, and the answer can differ from what a third party sees.
pub fn unroutable_host_reason(host: &str) -> Option<String> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return Some("the host is empty".to_string());
    }

    let literal = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = literal.parse::<IpAddr>() {
        let verdict = match ip {
            IpAddr::V4(v4) => crate::url_validation::validate_ipv4(&v4),
            IpAddr::V6(v6) => crate::url_validation::validate_ipv6(&v6),
        };
        // The outbound-request errors carry a hint about trusting private
        // networks, which is about what Temps may call, not about who can
        // call Temps. Name the address class instead.
        use crate::url_validation::UrlValidationError as E;
        return verdict.err().map(|reason| {
            let class = match reason {
                E::LoopbackIp => "a loopback address",
                E::PrivateIp => "a private network address",
                E::LinkLocalIp | E::CloudMetadata => "a link-local address",
                E::UnspecifiedIp => "the unspecified address",
                _ => "not a publicly routable address",
            };
            format!("{ip} is {class}")
        });
    }

    if host == "localhost" || host.ends_with(".localhost") {
        return Some("localhost only resolves on the machine itself".to_string());
    }
    if host == "localho.st" || host.ends_with(".localho.st") {
        return Some("localho.st always resolves to the loopback address".to_string());
    }
    if host.ends_with(".local") || host.ends_with(".internal") {
        return Some("the name only resolves on a private network".to_string());
    }
    if !host.contains('.') {
        return Some("a single-label name is not resolvable on the internet".to_string());
    }
    None
}

/// Why `host` cannot be the public ingress hostname given the preview domain,
/// or `None` when the two do not collide.
///
/// Every hostname Temps generates is a single label directly under the
/// preview domain: an environment (`shop-production.<preview>`), a deployment,
/// a Compose service, a workspace preview (`ws-<id>-<port>.<preview>`). The
/// ingress hostname is reserved for the platform, so if it lived there it
/// could take over a name an application has, or one an application is given
/// later, and the proxy would treat a preview-shaped name as a sandbox
/// preview before it ever reached the callback routes. Keeping the ingress
/// hostname out of that namespace rules out the whole class, instead of
/// chasing each generator.
///
/// A deeper name (`hooks.cb.<preview>`) is not generated and not matched by
/// the preview wildcard, so it stays allowed.
pub fn preview_namespace_conflict(
    host: &str,
    preview_domain: &str,
) -> Option<PublicIngressUrlError> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let preview = preview_domain
        .trim()
        .trim_start_matches("*.")
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host.is_empty() || preview.is_empty() {
        return None;
    }
    if host == preview {
        return Some(PublicIngressUrlError::PreviewDomain { host });
    }
    let generated_label = host
        .strip_suffix(preview.as_str())
        .and_then(|label| label.strip_suffix('.'))
        .is_some_and(|label| !label.is_empty() && !label.contains('.'));
    generated_label.then_some(PublicIngressUrlError::InsidePreviewDomain {
        host,
        preview_domain: preview,
    })
}

/// Validate and canonicalise a submitted `public_ingress_url`.
///
/// Returns `scheme://host[:port]` with no trailing slash. The URL must be a
/// bare origin, because every callback path is appended to it, its host must
/// be one a third party can reach, and it must sit outside the namespace
/// Temps generates hostnames in (see [`preview_namespace_conflict`]).
pub fn normalize_public_ingress_url(
    raw: &str,
    preview_domain: &str,
) -> Result<String, PublicIngressUrlError> {
    let trimmed = raw.trim();
    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        return Err(PublicIngressUrlError::InvalidScheme {
            url: trimmed.to_string(),
        });
    }
    let not_an_origin = |part: &'static str| PublicIngressUrlError::NotAnOrigin {
        url: trimmed.to_string(),
        part,
    };
    // `Url::parse` normalises an empty query/fragment away, so check the raw
    // text: "https://h?" must be refused just like "https://h?a=1".
    if trimmed.contains('?') {
        return Err(not_an_origin("query string"));
    }
    if trimmed.contains('#') {
        return Err(not_an_origin("fragment"));
    }

    let parsed = url::Url::parse(trimmed).map_err(|e| PublicIngressUrlError::Unparsable {
        url: trimmed.to_string(),
        reason: e.to_string(),
    })?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(not_an_origin("credentials"));
    }
    if !matches!(parsed.path(), "" | "/") {
        return Err(not_an_origin("path"));
    }
    let host = parsed
        .host_str()
        .map(|h| h.trim_end_matches('.').to_ascii_lowercase())
        .filter(|h| !h.is_empty())
        .ok_or_else(|| PublicIngressUrlError::Unparsable {
            url: trimmed.to_string(),
            reason: "the URL has no host".to_string(),
        })?;

    if let Some(reason) = unroutable_host_reason(&host) {
        return Err(PublicIngressUrlError::UnroutableHost { host, reason });
    }
    if let Some(conflict) = preview_namespace_conflict(&host, preview_domain) {
        return Err(conflict);
    }

    let port = parsed.port().map(|p| format!(":{p}")).unwrap_or_default();
    Ok(format!("{}://{}{}", parsed.scheme(), host, port))
}

impl AppSettings {
    /// The operator-configured ingress URL, with blank treated as unset.
    fn explicit_public_ingress_url(&self) -> Option<&str> {
        self.public_ingress_url
            .as_deref()
            .map(|url| url.trim().trim_end_matches('/'))
            .filter(|url| !url.is_empty())
    }

    /// Base URL handed to third parties that call back into Temps:
    /// `public_ingress_url` when set, `external_url` otherwise, and the local
    /// default when neither is. Never has a trailing slash.
    ///
    /// Use this for anything a third party will request (tracking pixels,
    /// provider webhooks, a browser-SDK DSN). Links an operator opens keep
    /// using `external_url`, and container-to-API traffic keeps using
    /// `internal_url`.
    pub fn effective_public_ingress_url(&self) -> String {
        self.resolve_public_ingress().effective_url
    }

    /// The ingress URL when one is configured: `public_ingress_url`, else
    /// `external_url`, with no trailing slash. `None` when neither is set,
    /// for callers that have their own fallback for an unconfigured install.
    pub fn configured_public_ingress_url(&self) -> Option<String> {
        self.explicit_public_ingress_url()
            .or_else(|| {
                self.external_url
                    .as_deref()
                    .map(|url| url.trim().trim_end_matches('/'))
                    .filter(|url| !url.is_empty())
            })
            .map(str::to_string)
    }

    /// Resolve the ingress URL together with its provenance and whether the
    /// host is provably unreachable for a third party.
    pub fn resolve_public_ingress(&self) -> PublicIngressResolution {
        let source = match self.explicit_public_ingress_url() {
            Some(_) => PublicIngressSource::Explicit,
            None => PublicIngressSource::ExternalUrlFallback,
        };
        let effective_url = self
            .configured_public_ingress_url()
            .unwrap_or_else(|| DEFAULT_PUBLIC_INGRESS_URL.to_string());
        let unreachable_reason = match url_host(&effective_url) {
            Some(host) => unroutable_host_reason(&host),
            None => Some("the URL has no host".to_string()),
        };
        PublicIngressResolution {
            effective_url,
            source,
            unreachable_reason,
        }
    }

    /// Hostname of the explicitly configured `public_ingress_url`.
    ///
    /// `None` when the setting is unset: the console hostname then doubles as
    /// the ingress host and there is no second platform hostname.
    pub fn public_ingress_hostname(&self) -> Option<String> {
        url_host(self.explicit_public_ingress_url()?)
    }

    /// The ingress hostname, only when it is a hostname of its own.
    ///
    /// This is the host the proxy pins to the public-only surface. It is
    /// `None` when the setting is unset or names the console host, because
    /// pinning the console host to the public-only surface would make the
    /// console unreachable.
    pub fn dedicated_public_ingress_hostname(&self) -> Option<String> {
        let host = self.public_ingress_hostname()?;
        (self.console_hostname().as_deref() != Some(host.as_str())).then_some(host)
    }
}

/// Lowercased host of `raw`, tolerating a bare host as well as a full URL.
fn url_host(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let candidate = if raw.contains("://") {
        raw.to_string()
    } else {
        format!("https://{raw}")
    };
    url::Url::parse(&candidate)
        .ok()?
        .host_str()
        .map(|h| h.trim_end_matches('.').to_ascii_lowercase())
        .filter(|h| !h.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(external: Option<&str>, ingress: Option<&str>) -> AppSettings {
        AppSettings {
            external_url: external.map(str::to_string),
            public_ingress_url: ingress.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn normalize_accepts_a_bare_origin_and_canonicalises_it() {
        assert_eq!(
            normalize_public_ingress_url("  https://Hooks.Example.com/  ", "apps.example.com")
                .unwrap(),
            "https://hooks.example.com"
        );
        assert_eq!(
            normalize_public_ingress_url("http://hooks.example.com:8080", "apps.example.com")
                .unwrap(),
            "http://hooks.example.com:8080"
        );
    }

    #[test]
    fn normalize_rejects_a_missing_or_foreign_scheme() {
        for url in ["hooks.example.com", "ftp://hooks.example.com"] {
            assert!(
                matches!(
                    normalize_public_ingress_url(url, "apps.example.com"),
                    Err(PublicIngressUrlError::InvalidScheme { .. })
                ),
                "{url} must be rejected for its scheme"
            );
        }
    }

    #[test]
    fn normalize_rejects_a_path_query_fragment_or_credentials() {
        let cases = [
            ("https://hooks.example.com/api", "path"),
            ("https://hooks.example.com?a=1", "query string"),
            ("https://hooks.example.com/?", "query string"),
            ("https://hooks.example.com#top", "fragment"),
            ("https://user:pw@hooks.example.com", "credentials"),
        ];
        for (url, expected_part) in cases {
            match normalize_public_ingress_url(url, "apps.example.com") {
                Err(PublicIngressUrlError::NotAnOrigin { part, .. }) => {
                    assert_eq!(part, expected_part, "wrong part reported for {url}")
                }
                other => panic!("{url} must be rejected as not an origin, got {other:?}"),
            }
        }
    }

    #[test]
    fn normalize_rejects_hosts_third_parties_cannot_reach() {
        for url in [
            "http://localhost:3000",
            "http://127.0.0.1",
            "http://[::1]",
            "http://10.1.2.3",
            "http://192.168.1.10:8080",
            "http://169.254.169.254",
            "http://localho.st",
            "http://hooks.localho.st",
            "http://temps.local",
            "http://temps",
        ] {
            assert!(
                matches!(
                    normalize_public_ingress_url(url, "apps.example.com"),
                    Err(PublicIngressUrlError::UnroutableHost { .. })
                ),
                "{url} must be rejected as unroutable"
            );
        }
    }

    #[test]
    fn normalize_rejects_the_preview_domain_apex() {
        assert_eq!(
            normalize_public_ingress_url("https://apps.example.com", "*.apps.example.com"),
            Err(PublicIngressUrlError::PreviewDomain {
                host: "apps.example.com".to_string()
            })
        );
    }

    // Every generated hostname is a single label directly under the preview
    // domain. An ingress hostname there could take over an application's
    // name, now or once one is generated, and a preview-shaped one would be
    // handled as a sandbox preview by the proxy.
    #[test]
    fn normalize_rejects_any_name_the_preview_wildcard_covers() {
        for url in [
            "https://hooks.apps.example.com",
            "https://shop-production.apps.example.com",
            "https://api--shop-production.apps.example.com",
            "https://ws-0123456789abcdef-3000.apps.example.com",
            "https://Hooks.Apps.Example.com.",
        ] {
            for preview in [
                "apps.example.com",
                "*.apps.example.com",
                "Apps.Example.com.",
            ] {
                assert!(
                    matches!(
                        normalize_public_ingress_url(url, preview),
                        Err(PublicIngressUrlError::InsidePreviewDomain { .. })
                    ),
                    "{url} is inside the generated namespace of {preview}"
                );
            }
        }
    }

    #[test]
    fn normalize_accepts_names_outside_the_generated_namespace() {
        for (url, expected) in [
            // A sibling of the preview domain.
            ("https://hooks.example.com", "https://hooks.example.com"),
            // Deeper than one label: not generated, not matched by the wildcard.
            (
                "https://hooks.cb.apps.example.com",
                "https://hooks.cb.apps.example.com",
            ),
            // Only looks like a suffix match.
            (
                "https://hooks.notapps.example.com",
                "https://hooks.notapps.example.com",
            ),
            (
                "https://hooksapps.example.com",
                "https://hooksapps.example.com",
            ),
        ] {
            assert_eq!(
                normalize_public_ingress_url(url, "apps.example.com").as_deref(),
                Ok(expected),
                "{url}"
            );
        }
        // No preview domain configured: nothing is generated anywhere.
        assert!(normalize_public_ingress_url("https://hooks.example.com", "  ").is_ok());
    }

    #[test]
    fn preview_namespace_conflict_names_both_sides() {
        let conflict = preview_namespace_conflict("hooks.apps.example.com", "*.apps.example.com")
            .expect("a single label under the preview domain conflicts");
        let message = conflict.to_string();
        assert!(message.contains("hooks.apps.example.com"), "{message}");
        assert!(message.contains("'apps.example.com'"), "{message}");
        assert_eq!(
            preview_namespace_conflict("hooks.example.com", "apps.example.com"),
            None
        );
    }

    #[test]
    fn unset_ingress_falls_back_to_the_external_url() {
        let s = settings(Some("https://console.example.com/"), None);
        let resolved = s.resolve_public_ingress();
        assert_eq!(resolved.effective_url, "https://console.example.com");
        assert_eq!(resolved.source, PublicIngressSource::ExternalUrlFallback);
        assert_eq!(resolved.unreachable_reason, None);
        assert_eq!(s.public_ingress_hostname(), None);
        assert_eq!(s.dedicated_public_ingress_hostname(), None);
    }

    #[test]
    fn configured_url_is_none_only_when_nothing_is_set() {
        assert_eq!(settings(None, None).configured_public_ingress_url(), None);
        assert_eq!(
            settings(Some("  "), Some("")).configured_public_ingress_url(),
            None
        );
        assert_eq!(
            settings(Some("https://console.example.com/"), None)
                .configured_public_ingress_url()
                .as_deref(),
            Some("https://console.example.com")
        );
        assert_eq!(
            settings(
                Some("https://console.example.com"),
                Some("https://hooks.example.com")
            )
            .configured_public_ingress_url()
            .as_deref(),
            Some("https://hooks.example.com")
        );
    }

    #[test]
    fn blank_ingress_is_treated_as_unset() {
        let s = settings(Some("https://console.example.com"), Some("   "));
        assert_eq!(
            s.resolve_public_ingress().source,
            PublicIngressSource::ExternalUrlFallback
        );
        assert_eq!(s.public_ingress_hostname(), None);
    }

    #[test]
    fn explicit_ingress_wins_and_gets_a_dedicated_hostname() {
        let s = settings(
            Some("https://console.example.com"),
            Some("https://Hooks.example.com/"),
        );
        let resolved = s.resolve_public_ingress();
        assert_eq!(resolved.effective_url, "https://Hooks.example.com");
        assert_eq!(resolved.source, PublicIngressSource::Explicit);
        assert_eq!(
            s.dedicated_public_ingress_hostname().as_deref(),
            Some("hooks.example.com")
        );
    }

    #[test]
    fn ingress_on_the_console_host_is_not_a_dedicated_hostname() {
        let s = settings(
            Some("https://console.example.com"),
            Some("https://console.example.com"),
        );
        assert_eq!(
            s.public_ingress_hostname().as_deref(),
            Some("console.example.com")
        );
        assert_eq!(s.dedicated_public_ingress_hostname(), None);
    }

    #[test]
    fn an_unconfigured_install_keeps_the_local_default_and_reports_it_unreachable() {
        let resolved = settings(None, None).resolve_public_ingress();
        assert_eq!(resolved.effective_url, DEFAULT_PUBLIC_INGRESS_URL);
        assert_eq!(resolved.source, PublicIngressSource::ExternalUrlFallback);
        assert!(resolved.unreachable_reason.is_some());
    }

    #[test]
    fn a_private_external_url_is_reported_unreachable() {
        let resolved = settings(Some("http://192.168.1.20:8080"), None).resolve_public_ingress();
        assert_eq!(
            resolved.unreachable_reason.as_deref(),
            Some("192.168.1.20 is a private network address")
        );
        assert_eq!(
            unroutable_host_reason("127.0.0.1").as_deref(),
            Some("127.0.0.1 is a loopback address")
        );
        assert_eq!(unroutable_host_reason("203.0.114.9"), None);
    }
}
