// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Hostnames.

use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostError {
    #[error("'{host}' is not a hostname: {reason}")]
    Invalid { host: String, reason: &'static str },
}

/// Lowercases, trims and drops a trailing dot, then checks the syntax: at
/// least two labels of `[a-z0-9-]`, 1-63 characters each, not starting or
/// ending with `-`, 253 characters in all. A leading `*.` claims exactly
/// one label.
pub fn normalize_host(host: &str) -> Result<String, HostError> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let invalid = |reason| HostError::Invalid {
        host: host.clone(),
        reason,
    };
    if host.is_empty() || host.len() > 253 {
        return Err(invalid("it must be 1-253 characters"));
    }
    let rest = host.strip_prefix("*.").unwrap_or(&host);
    let labels: Vec<&str> = rest.split('.').collect();
    if labels.len() < 2 {
        return Err(invalid("it needs at least two labels"));
    }
    for label in labels {
        if label.is_empty() || label.len() > 63 {
            return Err(invalid("every label must be 1-63 characters"));
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(invalid("labels may only hold a-z, 0-9 and '-'"));
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(invalid("a label cannot start or end with '-'"));
        }
    }
    Ok(host)
}

/// Hosts the platform owns (ADR-051): the console hostname, the public
/// ingress hostname and the preview domain apex. No endpoint may claim
/// them, and only generated and preview claims may sit under the preview
/// domain.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReservedHosts {
    exact: BTreeSet<String>,
    preview_domain: Option<String>,
}

impl ReservedHosts {
    pub fn new(exact: impl IntoIterator<Item = String>, preview_domain: Option<String>) -> Self {
        let clean = |h: String| h.trim().trim_end_matches('.').to_ascii_lowercase();
        let preview_domain = preview_domain.map(clean).filter(|d| !d.is_empty());
        let mut exact: BTreeSet<String> = exact
            .into_iter()
            .map(clean)
            .filter(|h| !h.is_empty())
            .collect();
        if let Some(apex) = &preview_domain {
            exact.insert(apex.clone());
        }
        Self {
            exact,
            preview_domain,
        }
    }

    pub fn is_reserved(&self, host: &str) -> bool {
        self.exact.contains(host)
    }

    /// Whether `host` (normalised) is under the preview domain.
    pub fn is_under_preview_domain(&self, host: &str) -> bool {
        self.preview_domain.as_deref().is_some_and(|apex| {
            host.len() > apex.len() + 1
                && host.ends_with(apex)
                && host.as_bytes()[host.len() - apex.len() - 1] == b'.'
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_are_normalised_and_checked() {
        assert_eq!(
            normalize_host(" App.Example.TEST. ").unwrap(),
            "app.example.test"
        );
        assert_eq!(normalize_host("*.example.test").unwrap(), "*.example.test");
        for bad in [
            "",
            "localhost",
            "a..b",
            "-a.test",
            "a-.test",
            "a_b.test",
            "*.test",
            "a.*.test",
            &format!("{}.test", "a".repeat(64)),
        ] {
            assert!(normalize_host(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_platform_hosts_and_preview_apex_are_reserved() {
        let reserved = ReservedHosts::new(
            ["Console.Example.TEST".to_string()],
            Some("preview.example.test.".to_string()),
        );
        assert!(reserved.is_reserved("console.example.test"));
        assert!(reserved.is_reserved("preview.example.test"));
        assert!(!reserved.is_reserved("shop.example.test"));
        assert!(reserved.is_under_preview_domain("pr-1.preview.example.test"));
        assert!(!reserved.is_under_preview_domain("preview.example.test"));
        assert!(!reserved.is_under_preview_domain("xpreview.example.test"));
    }
}
