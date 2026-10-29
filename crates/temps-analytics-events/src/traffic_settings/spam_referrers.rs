// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The bundled referrer-spam list.
//!
//! `data/referrer-spam-list.txt` is the public-domain Matomo list
//! (<https://github.com/matomo-org/referrer-spam-list>, see `NOTICE` for the
//! commit): domains whose only purpose is planting fake referrals in analytics
//! reports. It is compiled into the binary, so there is nothing to download or
//! configure and nothing that can fail at runtime; refresh it by replacing the
//! file.
//!
//! A domain on the list matches itself and every subdomain.

use std::collections::HashSet;
use std::sync::LazyLock;

use super::filters::host_suffixes;

static SPAM_REFERRER_LIST: &str = include_str!("../../data/referrer-spam-list.txt");

/// Parsed once on first use. Entries are lowercased because the upstream file
/// is not consistently cased, and referrer hosts are matched lowercase.
static SPAM_REFERRERS: LazyLock<HashSet<String>> = LazyLock::new(|| {
    SPAM_REFERRER_LIST
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_ascii_lowercase)
        .collect()
});

/// Whether `host` (a referrer hostname) is, or is a subdomain of, a domain on
/// the bundled spam list. `host` must already be lowercase.
pub fn is_spam_referrer(host: &str) -> bool {
    host_suffixes(host).any(|suffix| SPAM_REFERRERS.contains(suffix))
}

/// Number of domains on the bundled list, for the console.
pub fn spam_referrer_list_size() -> usize {
    SPAM_REFERRERS.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_list_loads_and_is_not_trivially_small() {
        assert!(
            spam_referrer_list_size() > 1_000,
            "expected the Matomo list (~2.3k domains), got {}",
            spam_referrer_list_size()
        );
    }

    #[test]
    fn a_listed_domain_and_its_subdomains_match() {
        // `0-0.fr` is the first entry of the upstream file.
        assert!(is_spam_referrer("0-0.fr"));
        assert!(is_spam_referrer("www.0-0.fr"));
        assert!(is_spam_referrer("a.b.0-0.fr"));
    }

    #[test]
    fn mixed_case_upstream_entries_are_matched_lowercase() {
        // The upstream file carries `QIWI.xyz` in uppercase.
        assert!(is_spam_referrer("qiwi.xyz"));
    }

    #[test]
    fn ordinary_and_lookalike_domains_do_not_match() {
        assert!(!is_spam_referrer("google.com"));
        assert!(!is_spam_referrer("news.ycombinator.com"));
        // A suffix match is by whole label: `x0-0.fr` is not `0-0.fr`.
        assert!(!is_spam_referrer("x0-0.fr"));
        assert!(!is_spam_referrer("fr"));
    }
}
