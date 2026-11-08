// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::{
    error::AccessError,
    types::{AccessMethod, SandboxAccessPolicy},
};
use tokenless_core::{
    host::HostPattern,
    path::{HttpMethod, PathPattern},
    rule::RuleScope,
};

fn invalid(field: &'static str, reason: &'static str) -> AccessError {
    AccessError::Validation { field, reason }
}

/// Validate again at the SDK boundary, including policies loaded from storage.
/// At most 32 prefixes become 64 patterns; no wildcard host or method is added.
/// This is generic HTTPS scope, not a Git read/write or MCP tool policy.
pub fn compile_policy(
    policy: &SandboxAccessPolicy,
) -> Result<(SandboxAccessPolicy, RuleScope), AccessError> {
    let mut policy = policy.clone();
    validate_policy(&mut policy)?;
    let host = HostPattern::from_https_base_url(&policy.origin)
        .map_err(|_| invalid("origin", "cannot compile HTTPS destination for Tokenless"))?;
    let mut paths = Vec::with_capacity(policy.path_prefixes.len() * 2);
    for prefix in &policy.path_prefixes {
        // `/v1` means exactly `/v1` plus `/v1/...`, never `/v10`.
        // A trailing slash remains significant: `/v1/` excludes `/v1`.
        if !prefix.ends_with('/') {
            paths.push(PathPattern::parse(prefix).map_err(|_| {
                invalid("path_prefixes", "cannot compile exact path for Tokenless")
            })?);
        }
        let subtree = format!("{}/*", prefix.trim_end_matches('/'));
        paths.push(
            PathPattern::parse(&subtree)
                .map_err(|_| invalid("path_prefixes", "cannot compile subtree for Tokenless"))?,
        );
    }
    paths.sort();
    paths.dedup();
    let mut methods = policy
        .methods
        .iter()
        .map(|method| {
            let name = match method {
                AccessMethod::Get => "GET",
                AccessMethod::Head => "HEAD",
                AccessMethod::Post => "POST",
                AccessMethod::Put => "PUT",
                AccessMethod::Patch => "PATCH",
                AccessMethod::Delete => "DELETE",
            };
            HttpMethod::parse(name)
                .map_err(|_| invalid("methods", "cannot compile HTTP method for Tokenless"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    methods.sort();
    methods.dedup();
    let scope = RuleScope {
        hosts: vec![host],
        paths,
        methods,
        mcp_tools: Some(Vec::new()),
        git: None,
    };
    scope.validate("sandbox service access").map_err(|_| {
        invalid(
            "policy",
            "Tokenless requires a destination, path and method",
        )
    })?;
    Ok((policy, scope))
}

fn validate_policy(policy: &mut SandboxAccessPolicy) -> Result<(), AccessError> {
    let url = url::Url::parse(&policy.origin)
        .map_err(|_| invalid("origin", "must be an HTTPS origin"))?;
    let host = url
        .host_str()
        .ok_or_else(|| invalid("origin", "must have a DNS hostname"))?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || host.parse::<std::net::IpAddr>().is_ok()
        || !host.contains('.')
        || host.ends_with('.')
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || !host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
    {
        return Err(invalid(
            "origin",
            "must be a DNS HTTPS origin on port 443, without credentials, path, query or fragment",
        ));
    }
    policy.origin = format!("https://{host}");
    if policy.path_prefixes.is_empty() || policy.path_prefixes.len() > 32 {
        return Err(invalid(
            "path_prefixes",
            "provide between 1 and 32 path prefixes",
        ));
    }
    for path in &policy.path_prefixes {
        if !path.starts_with('/')
            || path.len() > 512
            || path.contains("//")
            || path.split('/').any(|s| s == "." || s == "..")
            || !path
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"/-_.~".contains(&c))
        {
            return Err(invalid(
                "path_prefixes",
                "use absolute paths without encoding, traversal, query strings or wildcards",
            ));
        }
    }
    if policy.methods.is_empty() || policy.methods.len() > 6 {
        return Err(invalid("methods", "provide between 1 and 6 HTTP methods"));
    }
    Ok(())
}
