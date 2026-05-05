//! Git credential-helper stdin/stdout protocol.
//!
//! Reference: <https://git-scm.com/docs/git-credential>
//!
//! Git sends a series of `key=value` lines terminated by a blank line.
//! The helper responds with the same format. We care about three actions
//! (`get`, `store`, `erase`) and these fields:
//!
//! - `protocol` — `https` (we only support HTTPS for the in-sandbox
//!   credential daemon; SSH wouldn't go through helpers).
//! - `host` — `github.com`, `gitlab.com`, etc.
//! - `path` — `owner/repo` (only present when git is configured with
//!   `credential.useHttpPath=true`, which we set in the sandbox image).
//! - `username`, `password` — provided on `store`/`erase`, returned on
//!   `get`.
//!
//! Anything else is passed through unchanged as best-effort to avoid
//! breaking less-common flows; we never inspect or store it.

use std::collections::BTreeMap;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum HelperProtocolError {
    #[error("Malformed credential helper line (no `=`): {line:?}")]
    MalformedLine { line: String },

    #[error("Required field missing from helper request: {field}")]
    MissingField { field: &'static str },

    #[error("Unknown protocol {protocol:?}; only `https` is supported")]
    UnsupportedProtocol { protocol: String },

    #[error("Path {path:?} is not in `owner/repo` form")]
    MalformedPath { path: String },
}

/// One credential helper request. Built by [`parse_request`] from git's
/// stdin. Fields beyond the well-known ones are kept in `extra` so
/// helpers can faithfully echo them back if needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperRequest {
    pub host: String,
    pub owner: String,
    pub repo: String,
    /// Original `path=` value verbatim, kept for echo-back fidelity.
    pub raw_path: String,
    /// Other fields git sent that we don't interpret. Stored so a
    /// well-behaved helper can echo them back unchanged on `get`/`store`.
    pub extra: BTreeMap<String, String>,
}

/// Parse git's helper stdin format. Reads `key=value` lines and stops at
/// the first blank line OR end of input.
///
/// Validates that:
/// 1. `protocol` is `https`.
/// 2. `host` is set.
/// 3. `path` is set and looks like `owner/repo` (we depend on
///    `credential.useHttpPath=true` being configured in the sandbox so
///    git includes the repo path; without it we couldn't narrow tokens
///    per-repo).
pub fn parse_request(input: &str) -> Result<HelperRequest, HelperProtocolError> {
    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    for raw_line in input.split('\n') {
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() {
            // Blank line ends the request body per git's protocol.
            break;
        }
        let (k, v) = line
            .split_once('=')
            .ok_or_else(|| HelperProtocolError::MalformedLine {
                line: line.to_string(),
            })?;
        fields.insert(k.to_string(), v.to_string());
    }

    let protocol = fields
        .remove("protocol")
        .ok_or(HelperProtocolError::MissingField { field: "protocol" })?;
    if protocol != "https" {
        return Err(HelperProtocolError::UnsupportedProtocol { protocol });
    }

    let host = fields
        .remove("host")
        .ok_or(HelperProtocolError::MissingField { field: "host" })?;

    let raw_path = fields
        .remove("path")
        .ok_or(HelperProtocolError::MissingField { field: "path" })?;

    // Strip a possible trailing `.git` (some git versions include it,
    // some don't) so equality with the project's stored repo name works
    // regardless of how the URL was specified.
    let cleaned = raw_path.strip_suffix(".git").unwrap_or(raw_path.as_str());

    let (owner, repo) =
        cleaned
            .split_once('/')
            .ok_or_else(|| HelperProtocolError::MalformedPath {
                path: raw_path.clone(),
            })?;

    if owner.is_empty() || repo.is_empty() {
        return Err(HelperProtocolError::MalformedPath { path: raw_path });
    }

    Ok(HelperRequest {
        host,
        owner: owner.to_string(),
        repo: repo.to_string(),
        raw_path,
        extra: fields,
    })
}

/// Render a `get` response back to git: writes `username=...` +
/// `password=...` lines plus a trailing blank line. Other fields from
/// the request are echoed unchanged.
pub fn render_get_response(req: &HelperRequest, username: &str, password: &str) -> String {
    let mut out = String::new();
    out.push_str("protocol=https\n");
    out.push_str(&format!("host={}\n", req.host));
    out.push_str(&format!("path={}\n", req.raw_path));
    out.push_str(&format!("username={}\n", username));
    out.push_str(&format!("password={}\n", password));
    for (k, v) in &req.extra {
        // Don't re-emit any `username`/`password` we may have parsed —
        // we wrote our own above. Anything else: echo verbatim.
        if k == "username" || k == "password" {
            continue;
        }
        out.push_str(&format!("{}={}\n", k, v));
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimum viable git helper request — protocol, host, path. Our
    /// parser must extract owner/repo correctly from `acme/web`.
    #[test]
    fn parses_minimal_request() {
        let input = "protocol=https\nhost=github.com\npath=acme/web\n\n";
        let req = parse_request(input).unwrap();
        assert_eq!(req.host, "github.com");
        assert_eq!(req.owner, "acme");
        assert_eq!(req.repo, "web");
    }

    /// A `path` of `acme/web.git` (older gits / bare-clone URLs) must
    /// strip the `.git` suffix so the daemon's exact-match against the
    /// project's `repo_name` (`web`, never `web.git`) still passes.
    #[test]
    fn strips_dot_git_suffix() {
        let req = parse_request("protocol=https\nhost=github.com\npath=acme/web.git\n\n").unwrap();
        assert_eq!(req.owner, "acme");
        assert_eq!(req.repo, "web");
        // raw_path retains the original for echo-back fidelity:
        assert_eq!(req.raw_path, "acme/web.git");
    }

    /// SSH or git:// would land us in territory the daemon can't serve —
    /// reject up-front rather than minting a token and watching git fail
    /// later.
    #[test]
    fn rejects_non_https_protocol() {
        let err = parse_request("protocol=ssh\nhost=github.com\npath=acme/web\n").unwrap_err();
        assert!(matches!(
            err,
            HelperProtocolError::UnsupportedProtocol { .. }
        ));
    }

    /// Without `credential.useHttpPath=true` in git config, git won't
    /// send `path=`. We can't mint a per-repo token without it, so this
    /// must fail loudly rather than silently fall back to a wider scope.
    #[test]
    fn requires_path_field() {
        let err = parse_request("protocol=https\nhost=github.com\n").unwrap_err();
        match err {
            HelperProtocolError::MissingField { field } => assert_eq!(field, "path"),
            other => panic!("expected MissingField{{path}}, got {other:?}"),
        }
    }

    /// A `path` like `bare-name` (no slash) is malformed for our use —
    /// we need owner/repo. Reject explicitly.
    #[test]
    fn rejects_path_without_slash() {
        let err = parse_request("protocol=https\nhost=github.com\npath=just-a-name\n").unwrap_err();
        assert!(matches!(err, HelperProtocolError::MalformedPath { .. }));
    }

    /// Unknown fields (e.g. `wwwauth[]=Basic`) must be preserved so
    /// helpers stay forward-compatible with newer gits.
    #[test]
    fn preserves_unknown_fields() {
        let req = parse_request(
            "protocol=https\nhost=github.com\npath=acme/web\nwwwauth[]=Basic realm=foo\n",
        )
        .unwrap();
        assert_eq!(
            req.extra.get("wwwauth[]").map(String::as_str),
            Some("Basic realm=foo")
        );
    }

    /// Render must produce parseable output that git accepts: no
    /// missing trailing newline, no missing key=value separator.
    #[test]
    fn render_produces_parseable_response() {
        let req = parse_request("protocol=https\nhost=github.com\npath=acme/web\n\n").unwrap();
        let out = render_get_response(&req, "x-access-token", "ghs_xxx");
        assert!(out.contains("username=x-access-token\n"));
        assert!(out.contains("password=ghs_xxx\n"));
        assert!(out.ends_with("\n\n"));
    }
}
