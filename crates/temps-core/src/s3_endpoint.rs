// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One rule for turning a stored S3-compatible endpoint into the URL every
//! client receives, and for deciding whether that client must use path-style
//! addressing.
//!
//! An S3 source stores its endpoint as the operator typed it, with or without
//! a scheme. The in-process `aws-sdk-s3` clients have always read a bare
//! `host:port` as plain HTTP, so a source saved as `10.0.0.5:9000` passed its
//! connection test. WAL-G, `mc` and the AWS CLI read the same bare value as
//! HTTPS, so every `wal-push` from that source failed with "server gave HTTP
//! response to HTTPS client" while Temps reported the source healthy. Every
//! generator of a client configuration (`walg.env`, worker job environments,
//! restore and deletion jobs) goes through [`S3Endpoint`], so the scheme a
//! source was validated with is the scheme every client uses.
//!
//! Virtual-hosted addressing puts the bucket in the host name
//! (`<bucket>.<host>`). That cannot resolve when the host is an IP literal,
//! and Temps-managed object stores (RustFS, MinIO, Garage) serve path-style
//! only, so [`effective_path_style`] forces it in both cases and reports why.

use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use utoipa::ToSchema;

/// The transport an S3 endpoint is reached over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum S3Scheme {
    Http,
    Https,
}

impl S3Scheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }

    fn from_use_tls(use_tls: bool) -> Self {
        if use_tls {
            Self::Https
        } else {
            Self::Http
        }
    }
}

impl std::fmt::Display for S3Scheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why an endpoint string cannot be used for an S3 source.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum S3EndpointError {
    #[error("S3 endpoint is empty; leave it unset for AWS S3 or enter a host such as 10.0.0.5:9000 or https://s3.example.com")]
    Empty,
    #[error(
        "S3 endpoint '{endpoint}' uses scheme '{scheme}'; only http:// and https:// are supported"
    )]
    UnsupportedScheme { endpoint: String, scheme: String },
    #[error("S3 endpoint '{endpoint}' is not a valid URL: {reason}")]
    Malformed { endpoint: String, reason: String },
    #[error("S3 endpoint '{endpoint}' has no host")]
    MissingHost { endpoint: String },
    #[error("S3 endpoint '{endpoint}' must not contain credentials; enter the access key and secret key in their own fields")]
    EmbeddedCredentials { endpoint: String },
    #[error("S3 endpoint '{endpoint}' says {scheme}:// but TLS is turned {tls}; remove the scheme or make it match the TLS setting", tls = if *use_tls { "on" } else { "off" })]
    SchemeConflictsWithTls {
        endpoint: String,
        scheme: S3Scheme,
        use_tls: bool,
    },
}

/// A parsed S3-compatible endpoint with an explicit scheme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Endpoint {
    scheme: S3Scheme,
    /// Host without IPv6 brackets.
    host: String,
    port: Option<u16>,
    /// Path after the authority, without a trailing `/`; usually empty.
    path: String,
    /// Whether the input spelled the scheme out.
    scheme_was_explicit: bool,
}

impl S3Endpoint {
    /// Parse an endpoint as the operator entered it.
    ///
    /// `use_tls` is the optional TLS choice made next to the endpoint. With no
    /// scheme in `raw`, it decides the scheme (`None` means plain HTTP, the
    /// rule the SDK clients have always applied). With a scheme, it must agree
    /// with it.
    pub fn parse(raw: &str, use_tls: Option<bool>) -> Result<Self, S3EndpointError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(S3EndpointError::Empty);
        }
        let (candidate, scheme_was_explicit) = match trimmed.split_once("://") {
            Some((scheme, _)) => {
                let scheme = scheme.to_ascii_lowercase();
                if scheme != "http" && scheme != "https" {
                    return Err(S3EndpointError::UnsupportedScheme {
                        endpoint: trimmed.to_string(),
                        scheme,
                    });
                }
                (trimmed.to_string(), true)
            }
            None => {
                let scheme = S3Scheme::from_use_tls(use_tls.unwrap_or(false));
                (format!("{scheme}://{trimmed}"), false)
            }
        };
        let url = url::Url::parse(&candidate).map_err(|error| S3EndpointError::Malformed {
            endpoint: trimmed.to_string(),
            reason: error.to_string(),
        })?;
        if !url.username().is_empty() || url.password().is_some() {
            return Err(S3EndpointError::EmbeddedCredentials {
                endpoint: redact_userinfo(trimmed),
            });
        }
        let scheme = match url.scheme() {
            "https" => S3Scheme::Https,
            _ => S3Scheme::Http,
        };
        if scheme_was_explicit {
            if let Some(use_tls) = use_tls {
                if use_tls != (scheme == S3Scheme::Https) {
                    return Err(S3EndpointError::SchemeConflictsWithTls {
                        endpoint: trimmed.to_string(),
                        scheme,
                        use_tls,
                    });
                }
            }
        }
        let host = match url.host() {
            Some(url::Host::Ipv6(address)) => address.to_string(),
            Some(host) => host.to_string(),
            None => {
                return Err(S3EndpointError::MissingHost {
                    endpoint: trimmed.to_string(),
                })
            }
        };
        if host.is_empty() {
            return Err(S3EndpointError::MissingHost {
                endpoint: trimmed.to_string(),
            });
        }
        Ok(Self {
            scheme,
            host,
            port: url.port(),
            path: url.path().trim_end_matches('/').to_string(),
            scheme_was_explicit,
        })
    }

    pub fn scheme(&self) -> S3Scheme {
        self.scheme
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> Option<u16> {
        self.port
    }

    /// Whether the input spelled out `http://` or `https://`.
    pub fn scheme_was_explicit(&self) -> bool {
        self.scheme_was_explicit
    }

    /// Whether the host is an IPv4 or IPv6 literal, where virtual-hosted
    /// addressing (`<bucket>.<host>`) cannot resolve.
    pub fn host_is_ip_literal(&self) -> bool {
        self.host.parse::<IpAddr>().is_ok()
    }

    /// The full URL with its scheme, as every S3 client must receive it.
    pub fn url(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        match self.port {
            Some(port) => format!("{}://{host}:{port}{}", self.scheme, self.path),
            None => format!("{}://{host}{}", self.scheme, self.path),
        }
    }
}

/// The URL every S3 client receives for a **stored** endpoint.
///
/// A bare `host:port` becomes `http://host:port`, matching how the source was
/// validated when it was saved. A value that cannot be parsed is passed
/// through with `http://` prepended when it has no scheme, so a client still
/// fails with its own connection error instead of the source silently
/// disappearing; new values are rejected by [`S3Endpoint::parse`] on write.
pub fn endpoint_url(stored: &str) -> String {
    match S3Endpoint::parse(stored, None) {
        Ok(endpoint) => endpoint.url(),
        Err(_) => {
            let trimmed = stored.trim();
            if trimmed.contains("://") {
                trimmed.to_string()
            } else {
                format!("http://{trimmed}")
            }
        }
    }
}

/// Whether a stored endpoint names an IP-literal host.
pub fn endpoint_host_is_ip_literal(stored: &str) -> bool {
    S3Endpoint::parse(stored, None).is_ok_and(|endpoint| endpoint.host_is_ip_literal())
}

/// Why path-style addressing is or is not used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PathStyleReason {
    /// The source's own `force_path_style` setting decided it.
    Configured,
    /// Forced: the endpoint host is an IP address, where `<bucket>.<ip>`
    /// cannot resolve.
    IpAddressEndpoint,
    /// Forced: the source is a Temps-managed object store, which serves
    /// path-style requests only.
    ManagedObjectStore,
}

impl PathStyleReason {
    /// A sentence for the console and for error messages.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Configured => "as configured on the S3 source",
            Self::IpAddressEndpoint => "forced: the endpoint is an IP address",
            Self::ManagedObjectStore => "forced: the endpoint is a Temps-managed object store",
        }
    }
}

/// The addressing style a client actually uses, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EffectivePathStyle {
    pub force_path_style: bool,
    pub reason: PathStyleReason,
}

/// Decide path-style addressing for a source.
///
/// `configured` is the source's own setting, `managed_object_store` whether
/// the source is backed by a Temps-managed RustFS/MinIO/Garage service. An
/// IP-literal host or a managed store forces path style; otherwise the
/// operator's choice stands.
pub fn effective_path_style(
    endpoint: Option<&str>,
    configured: bool,
    managed_object_store: bool,
) -> EffectivePathStyle {
    let reason = if endpoint.is_some_and(endpoint_host_is_ip_literal) {
        Some(PathStyleReason::IpAddressEndpoint)
    } else if managed_object_store {
        Some(PathStyleReason::ManagedObjectStore)
    } else {
        None
    };
    match reason {
        // Only report a forced reason when it actually changed the outcome,
        // so an operator who already ticked the box is not told it was
        // overridden.
        Some(reason) if !configured => EffectivePathStyle {
            force_path_style: true,
            reason,
        },
        _ => EffectivePathStyle {
            force_path_style: configured,
            reason: PathStyleReason::Configured,
        },
    }
}

/// Normalise an endpoint entered on a create/update request into the value
/// to store: always with an explicit scheme, so what is stored, tested and
/// handed to every client is the same string.
pub fn normalize_endpoint_input(
    raw: Option<&str>,
    use_tls: Option<bool>,
) -> Result<Option<String>, S3EndpointError> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(value) => S3Endpoint::parse(value, use_tls).map(|endpoint| Some(endpoint.url())),
    }
}

fn redact_userinfo(raw: &str) -> String {
    match raw.split_once("://") {
        Some((scheme, rest)) => match rest.rsplit_once('@') {
            Some((_, host)) => format!("{scheme}://***@{host}"),
            None => raw.to_string(),
        },
        None => match raw.rsplit_once('@') {
            Some((_, host)) => format!("***@{host}"),
            None => raw.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_ip_and_port_is_plain_http() {
        let endpoint = S3Endpoint::parse("10.0.0.5:9000", None).unwrap();
        assert_eq!(endpoint.url(), "http://10.0.0.5:9000");
        assert_eq!(endpoint.scheme(), S3Scheme::Http);
        assert!(endpoint.host_is_ip_literal());
        assert!(!endpoint.scheme_was_explicit());
    }

    #[test]
    fn a_bare_hostname_is_plain_http() {
        let endpoint = S3Endpoint::parse("objects.internal:9000", None).unwrap();
        assert_eq!(endpoint.url(), "http://objects.internal:9000");
        assert!(!endpoint.host_is_ip_literal());
    }

    #[test]
    fn a_bare_host_with_tls_on_is_https() {
        let endpoint = S3Endpoint::parse("s3.example.com", Some(true)).unwrap();
        assert_eq!(endpoint.url(), "https://s3.example.com");
        assert_eq!(endpoint.scheme(), S3Scheme::Https);
    }

    #[test]
    fn a_bare_ip_with_tls_on_is_https() {
        let endpoint = S3Endpoint::parse("10.0.0.5:9000", Some(true)).unwrap();
        assert_eq!(endpoint.url(), "https://10.0.0.5:9000");
    }

    #[test]
    fn an_explicit_scheme_is_kept_for_ip_and_hostname() {
        for (raw, expected) in [
            ("http://10.0.0.5:9000", "http://10.0.0.5:9000"),
            ("https://10.0.0.5:9000/", "https://10.0.0.5:9000"),
            (
                "HTTP://objects.internal:9000",
                "http://objects.internal:9000",
            ),
            ("https://s3.example.com", "https://s3.example.com"),
        ] {
            let endpoint = S3Endpoint::parse(raw, None).unwrap();
            assert_eq!(endpoint.url(), expected, "{raw}");
            assert!(endpoint.scheme_was_explicit(), "{raw}");
        }
    }

    #[test]
    fn a_matching_tls_flag_is_accepted() {
        assert!(S3Endpoint::parse("https://s3.example.com", Some(true)).is_ok());
        assert!(S3Endpoint::parse("http://10.0.0.5:9000", Some(false)).is_ok());
    }

    #[test]
    fn a_scheme_that_disagrees_with_the_tls_flag_is_rejected() {
        let error = S3Endpoint::parse("http://10.0.0.5:9000", Some(true)).unwrap_err();
        assert_eq!(
            error,
            S3EndpointError::SchemeConflictsWithTls {
                endpoint: "http://10.0.0.5:9000".to_string(),
                scheme: S3Scheme::Http,
                use_tls: true,
            }
        );
        assert!(error.to_string().contains("TLS is turned on"));

        let error = S3Endpoint::parse("https://s3.example.com", Some(false)).unwrap_err();
        assert!(matches!(
            error,
            S3EndpointError::SchemeConflictsWithTls {
                scheme: S3Scheme::Https,
                use_tls: false,
                ..
            }
        ));
    }

    /// The old `starts_with("http")` test read a host whose name merely
    /// begins with "http" as already having a scheme, and left it bare.
    #[test]
    fn a_host_that_starts_with_http_still_gets_a_scheme() {
        let endpoint = S3Endpoint::parse("httpstore.internal:9000", None).unwrap();
        assert_eq!(endpoint.url(), "http://httpstore.internal:9000");
        assert_eq!(
            endpoint_url("httpstore.internal:9000"),
            "http://httpstore.internal:9000"
        );
    }

    #[test]
    fn unsupported_and_malformed_input_is_rejected() {
        assert_eq!(
            S3Endpoint::parse("  ", None).unwrap_err(),
            S3EndpointError::Empty
        );
        assert!(matches!(
            S3Endpoint::parse("ftp://10.0.0.5:9000", None).unwrap_err(),
            S3EndpointError::UnsupportedScheme { scheme, .. } if scheme == "ftp"
        ));
        assert!(matches!(
            S3Endpoint::parse("10.0.0.5:notaport", None).unwrap_err(),
            S3EndpointError::Malformed { .. }
        ));
    }

    #[test]
    fn credentials_in_the_endpoint_are_rejected_without_echoing_them() {
        let error = S3Endpoint::parse("http://key:hunter2@10.0.0.5:9000", None).unwrap_err();
        assert!(matches!(error, S3EndpointError::EmbeddedCredentials { .. }));
        assert!(!error.to_string().contains("hunter2"));
    }

    #[test]
    fn ipv6_literals_are_bracketed_and_detected() {
        let endpoint = S3Endpoint::parse("[fd00::5]:9000", None).unwrap();
        assert_eq!(endpoint.url(), "http://[fd00::5]:9000");
        assert!(endpoint.host_is_ip_literal());
    }

    #[test]
    fn stored_endpoints_always_reach_clients_with_a_scheme() {
        assert_eq!(endpoint_url("10.0.0.5:9000"), "http://10.0.0.5:9000");
        assert_eq!(
            endpoint_url("https://s3.example.com/"),
            "https://s3.example.com"
        );
        assert_eq!(
            endpoint_url("rustfs-store:9000"),
            "http://rustfs-store:9000"
        );
    }

    #[test]
    fn path_style_is_forced_for_an_ip_endpoint() {
        let style = effective_path_style(Some("10.0.0.5:9000"), false, false);
        assert_eq!(
            style,
            EffectivePathStyle {
                force_path_style: true,
                reason: PathStyleReason::IpAddressEndpoint,
            }
        );
        assert_eq!(
            style.reason.describe(),
            "forced: the endpoint is an IP address"
        );
        assert!(effective_path_style(Some("https://10.0.0.5"), false, false).force_path_style);
    }

    #[test]
    fn path_style_is_forced_for_a_managed_object_store() {
        let style = effective_path_style(Some("http://rustfs-store:9000"), false, true);
        assert_eq!(style.reason, PathStyleReason::ManagedObjectStore);
        assert!(style.force_path_style);
    }

    #[test]
    fn a_hostname_keeps_the_operators_choice() {
        let off = effective_path_style(Some("https://s3.example.com"), false, false);
        assert_eq!(
            off,
            EffectivePathStyle {
                force_path_style: false,
                reason: PathStyleReason::Configured,
            }
        );
        let on = effective_path_style(Some("https://s3.example.com"), true, false);
        assert!(on.force_path_style);
        assert_eq!(on.reason, PathStyleReason::Configured);
        // AWS S3 itself (no endpoint) keeps the choice too.
        assert!(!effective_path_style(None, false, false).force_path_style);
    }

    #[test]
    fn an_ip_endpoint_already_set_to_path_style_reports_the_configured_reason() {
        let style = effective_path_style(Some("10.0.0.5:9000"), true, false);
        assert_eq!(style.reason, PathStyleReason::Configured);
        assert!(style.force_path_style);
    }

    #[test]
    fn request_input_is_normalised_before_it_is_stored() {
        assert_eq!(normalize_endpoint_input(None, None), Ok(None));
        assert_eq!(normalize_endpoint_input(Some("  "), Some(true)), Ok(None));
        assert_eq!(
            normalize_endpoint_input(Some("10.0.0.5:9000"), None),
            Ok(Some("http://10.0.0.5:9000".to_string()))
        );
        assert_eq!(
            normalize_endpoint_input(Some("s3.example.com"), Some(true)),
            Ok(Some("https://s3.example.com".to_string()))
        );
        assert!(normalize_endpoint_input(Some("https://s3.example.com"), Some(false)).is_err());
    }
}
