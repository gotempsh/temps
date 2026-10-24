// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Who may open a sandbox's preview URLs, and from where.
//!
//! Two settings live on the sandbox row and are enforced by the proxy:
//!
//! - `preview_access` (`temps_login` | `password` | `public`): the default
//!   is `temps_login`, so an anonymous visitor holding the URL is never let
//!   in. Setting a preview password selects `password`; clearing it goes
//!   back to `temps_login`.
//! - `preview_allowed_cidrs`: the client networks allowed in, checked first
//!   in every mode. Empty means any address.
//!
//! This module is the pure half: picking the mode for a new sandbox and
//! validating/normalising an allow-list. Persistence lives in
//! `SandboxService`.

use std::net::IpAddr;
use std::str::FromStr;

use ipnetwork::IpNetwork;
use temps_entities::ingress_mode::SandboxPreviewAccess;

/// Most networks one sandbox's allow-list may hold. The database enforces
/// the same bound (`sandboxes_preview_allowed_cidrs_bounded`).
pub const MAX_PREVIEW_ALLOWED_CIDRS: usize = 50;

/// The access mode a sandbox is created with: `password` when the request
/// carried a preview password, otherwise `temps_login`.
pub fn initial_access(has_password: bool) -> SandboxPreviewAccess {
    if has_password {
        SandboxPreviewAccess::Password
    } else {
        SandboxPreviewAccess::TempsLogin
    }
}

/// The access mode left after the preview password is cleared: `password`
/// falls back to `temps_login`, never to an open preview; the other modes
/// did not use the password and stay as they are.
pub fn access_after_clearing_password(current: SandboxPreviewAccess) -> SandboxPreviewAccess {
    match current {
        SandboxPreviewAccess::Password | SandboxPreviewAccess::TempsLogin => {
            SandboxPreviewAccess::TempsLogin
        }
        SandboxPreviewAccess::Public => SandboxPreviewAccess::Public,
    }
}

/// Validate and normalise an allow-list.
///
/// Each entry is an IPv4/IPv6 address (stored as a `/32` or `/128`) or a
/// CIDR (stored as its network address, so `192.0.2.7/24` becomes
/// `192.0.2.0/24`). Duplicates are dropped, order is kept. The error names
/// the offending entry and its position.
pub fn normalize_allowed_cidrs(entries: &[String]) -> Result<Vec<String>, String> {
    if entries.len() > MAX_PREVIEW_ALLOWED_CIDRS {
        return Err(format!(
            "allowed_cidrs has {} entries; at most {MAX_PREVIEW_ALLOWED_CIDRS} are allowed",
            entries.len()
        ));
    }
    let mut out: Vec<String> = Vec::with_capacity(entries.len());
    for (index, raw) in entries.iter().enumerate() {
        let entry = raw.trim();
        if entry.is_empty() {
            return Err(format!("allowed_cidrs[{index}] is empty"));
        }
        let network = parse_entry(entry).ok_or_else(|| {
            format!(
                "allowed_cidrs[{index}] '{entry}' is not an IP address or CIDR \
                 (expected e.g. 203.0.113.7, 203.0.113.0/24 or 2001:db8::/32)"
            )
        })?;
        let normalised = format!("{}/{}", network.network(), network.prefix());
        if !out.contains(&normalised) {
            out.push(normalised);
        }
    }
    Ok(out)
}

fn parse_entry(entry: &str) -> Option<IpNetwork> {
    if entry.contains('/') {
        return IpNetwork::from_str(entry).ok();
    }
    let ip = IpAddr::from_str(entry).ok()?;
    let prefix = if ip.is_ipv4() { 32 } else { 128 };
    IpNetwork::new(ip, prefix).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn new_sandboxes_are_never_public() {
        assert_eq!(initial_access(false), SandboxPreviewAccess::TempsLogin);
        assert_eq!(initial_access(true), SandboxPreviewAccess::Password);
    }

    #[test]
    fn clearing_the_password_never_opens_the_preview() {
        assert_eq!(
            access_after_clearing_password(SandboxPreviewAccess::Password),
            SandboxPreviewAccess::TempsLogin
        );
        assert_eq!(
            access_after_clearing_password(SandboxPreviewAccess::TempsLogin),
            SandboxPreviewAccess::TempsLogin
        );
        assert_eq!(
            access_after_clearing_password(SandboxPreviewAccess::Public),
            SandboxPreviewAccess::Public
        );
    }

    #[test]
    fn addresses_and_networks_are_normalised() {
        let got = normalize_allowed_cidrs(&strings(&[
            " 203.0.113.7 ",
            "198.51.100.9/24",
            "2001:db8::1",
            "2001:db8:abcd::5/48",
            "0.0.0.0/0",
        ]))
        .unwrap();
        assert_eq!(
            got,
            strings(&[
                "203.0.113.7/32",
                "198.51.100.0/24",
                "2001:db8::1/128",
                "2001:db8:abcd::/48",
                "0.0.0.0/0",
            ])
        );
    }

    #[test]
    fn duplicates_collapse_after_normalising() {
        let got =
            normalize_allowed_cidrs(&strings(&["192.0.2.0/24", "192.0.2.99/24", "192.0.2.0/24"]))
                .unwrap();
        assert_eq!(got, strings(&["192.0.2.0/24"]));
    }

    #[test]
    fn an_empty_list_is_valid_and_means_any_address() {
        assert_eq!(normalize_allowed_cidrs(&[]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn invalid_entries_are_named_with_their_position() {
        for (input, needle) in [
            ("not-an-ip", "allowed_cidrs[1] 'not-an-ip'"),
            ("192.0.2.0/33", "allowed_cidrs[1] '192.0.2.0/33'"),
            ("2001:db8::/129", "allowed_cidrs[1] '2001:db8::/129'"),
            ("300.1.1.1", "allowed_cidrs[1] '300.1.1.1'"),
            ("example.com", "allowed_cidrs[1] 'example.com'"),
            ("   ", "allowed_cidrs[1] is empty"),
        ] {
            let err = normalize_allowed_cidrs(&strings(&["192.0.2.1", input])).unwrap_err();
            assert!(err.contains(needle), "{input}: {err}");
        }
    }

    #[test]
    fn more_than_fifty_entries_are_refused() {
        let fifty: Vec<String> = (0..50).map(|i| format!("10.0.{i}.0/24")).collect();
        assert_eq!(normalize_allowed_cidrs(&fifty).unwrap().len(), 50);
        let fifty_one: Vec<String> = (0..51).map(|i| format!("10.0.{i}.0/24")).collect();
        let err = normalize_allowed_cidrs(&fifty_one).unwrap_err();
        assert!(err.contains("51 entries"), "{err}");
        assert!(err.contains("at most 50"), "{err}");
    }
}
