// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The path part of a request Temps makes to a deployed app on its own
//! schedule: health probes and HTTP crons.
//!
//! A path comes from a repository's `.temps.yaml` or from an API request, so it
//! is untrusted text that gets appended to the address of the app's container.
//! Joined as plain strings, a path such as `@10.0.0.5:8500/x` turns the
//! container's address into URL userinfo and makes the rest the real
//! destination. Everything that builds such a URL goes through here.

use thiserror::Error;
use url::Url;

/// Longest path accepted, query string included.
pub const MAX_REQUEST_PATH_LEN: usize = 2048;

/// Why a request path is not acceptable.
#[derive(Error, Debug, PartialEq, Eq)]
pub enum RequestPathError {
    #[error("length {len} exceeds the {MAX_REQUEST_PATH_LEN} byte limit")]
    TooLong { len: usize },

    #[error("must start with '/'")]
    MissingLeadingSlash,

    #[error("must not start with '//' (a protocol-relative address)")]
    ProtocolRelative,

    #[error("must not contain '@' (userinfo injection)")]
    UserinfoMarker,

    #[error("must not contain '://' (scheme injection)")]
    SchemeMarker,

    #[error("must not contain backslashes")]
    Backslash,

    #[error("must not contain control characters")]
    ControlCharacter,

    #[error("base address '{base}' is not a valid URL: {reason}")]
    InvalidBase { base: String, reason: String },

    #[error("path '{path}' would change the request's {part} from '{expected}' to '{actual}'")]
    DestinationChanged {
        path: String,
        part: &'static str,
        expected: String,
        actual: String,
    },
}

/// Check a request path without building anything.
///
/// The path must be absolute (`/...`), and must not smuggle in an authority,
/// a scheme, credentials or control characters. A query string is allowed.
pub fn validate_request_path(path: &str) -> Result<(), RequestPathError> {
    if path.len() > MAX_REQUEST_PATH_LEN {
        return Err(RequestPathError::TooLong { len: path.len() });
    }
    if !path.starts_with('/') {
        return Err(RequestPathError::MissingLeadingSlash);
    }
    if path.starts_with("//") {
        return Err(RequestPathError::ProtocolRelative);
    }
    if path.contains('@') {
        return Err(RequestPathError::UserinfoMarker);
    }
    if path.contains("://") {
        return Err(RequestPathError::SchemeMarker);
    }
    if path.contains('\\') {
        return Err(RequestPathError::Backslash);
    }
    if path.chars().any(char::is_control) {
        return Err(RequestPathError::ControlCharacter);
    }
    Ok(())
}

/// Build the URL for `path` on `base` (an address like `http://app:3000`).
///
/// The path is validated, set with [`Url::set_path`] and [`Url::set_query`]
/// rather than concatenated, and the result is checked to still point at the
/// same scheme, host and port as `base`. Any fragment is dropped, as an HTTP
/// client would.
pub fn url_for_request_path(base: &str, path: &str) -> Result<Url, RequestPathError> {
    validate_request_path(path)?;

    let origin = Url::parse(base).map_err(|error| RequestPathError::InvalidBase {
        base: base.to_string(),
        reason: error.to_string(),
    })?;
    let mut url = origin.clone();

    let without_fragment = path.split_once('#').map_or(path, |(before, _)| before);
    let (path_part, query) = match without_fragment.split_once('?') {
        Some((path_part, query)) => (path_part, Some(query)),
        None => (without_fragment, None),
    };
    url.set_path(path_part);
    url.set_query(query);
    url.set_fragment(None);

    let changed = |part: &'static str, expected: String, actual: String| {
        RequestPathError::DestinationChanged {
            path: path.to_string(),
            part,
            expected,
            actual,
        }
    };
    if url.scheme() != origin.scheme() {
        return Err(changed(
            "scheme",
            origin.scheme().to_string(),
            url.scheme().to_string(),
        ));
    }
    if url.host_str() != origin.host_str() {
        return Err(changed(
            "host",
            origin.host_str().unwrap_or_default().to_string(),
            url.host_str().unwrap_or_default().to_string(),
        ));
    }
    if url.port_or_known_default() != origin.port_or_known_default() {
        return Err(changed(
            "port",
            format!("{:?}", origin.port_or_known_default()),
            format!("{:?}", url.port_or_known_default()),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(changed(
            "credentials",
            "none".to_string(),
            "userinfo".to_string(),
        ));
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "http://app-container:3000";

    #[test]
    fn ordinary_paths_are_accepted() {
        for path in [
            "/",
            "/api/cron/report",
            "/health?ready=1",
            "/a/b%20c",
            "/path#fragment",
        ] {
            assert!(validate_request_path(path).is_ok(), "{path}");
            assert!(url_for_request_path(BASE, path).is_ok(), "{path}");
        }
    }

    #[test]
    fn a_path_that_starts_an_authority_is_refused() {
        for (path, expected) in [
            (
                "@10.0.0.5:8500/v1/agent/self",
                RequestPathError::MissingLeadingSlash,
            ),
            ("//10.0.0.5:8500/x", RequestPathError::ProtocolRelative),
            ("x", RequestPathError::MissingLeadingSlash),
            ("", RequestPathError::MissingLeadingSlash),
            ("/x@10.0.0.5", RequestPathError::UserinfoMarker),
            (
                "/redirect?to=http://10.0.0.5",
                RequestPathError::SchemeMarker,
            ),
            ("/\\\\10.0.0.5/x", RequestPathError::Backslash),
            ("/x\ny", RequestPathError::ControlCharacter),
            ("/x\0", RequestPathError::ControlCharacter),
            ("/x\t", RequestPathError::ControlCharacter),
        ] {
            assert_eq!(validate_request_path(path), Err(expected), "{path:?}");
            assert!(url_for_request_path(BASE, path).is_err(), "{path:?}");
        }
        assert_eq!(
            validate_request_path(&format!("/{}", "a".repeat(MAX_REQUEST_PATH_LEN))),
            Err(RequestPathError::TooLong {
                len: MAX_REQUEST_PATH_LEN + 1
            })
        );
    }

    #[test]
    fn the_built_url_keeps_the_container_as_its_destination() {
        let url = url_for_request_path(BASE, "/api/run?job=nightly#ignored").unwrap();
        assert_eq!(
            url.as_str(),
            "http://app-container:3000/api/run?job=nightly"
        );
        assert_eq!(url.host_str(), Some("app-container"));
        assert_eq!(url.port(), Some(3000));
    }

    #[test]
    fn a_base_that_is_not_a_url_is_reported_as_such() {
        assert!(matches!(
            url_for_request_path("not a url", "/x"),
            Err(RequestPathError::InvalidBase { .. })
        ));
    }
}
