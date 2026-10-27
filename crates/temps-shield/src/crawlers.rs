// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Crawlers that are never challenged once verified (ADR 066, D7).
//!
//! A User-Agent can say anything, so naming a crawler only earns a check:
//! the address's reverse DNS name must end in one of the domains the
//! crawler's operator publishes, and that name must resolve back to the
//! same address. The ingress does the lookup and caches the answer per
//! address; this module only says who claims what and which names count.

/// A crawler whose operator publishes the domains its addresses resolve to.
#[derive(Debug, PartialEq, Eq)]
pub struct Crawler {
    pub name: &'static str,
    /// Substrings of the User-Agent, case-insensitive, that claim it.
    claims: &'static [&'static str],
    /// The reverse DNS name of a real one ends in `.<domain>`.
    domains: &'static [&'static str],
}

impl Crawler {
    /// Whether `hostname`, a reverse DNS answer, is one of this crawler's.
    pub fn owns(&self, hostname: &str) -> bool {
        let hostname = hostname.trim_end_matches('.').to_ascii_lowercase();
        self.domains.iter().any(|domain| {
            hostname
                .strip_suffix(domain)
                .is_some_and(|rest| rest.ends_with('.'))
        })
    }
}

/// Crawlers that publish reverse DNS domains for verification. Operators
/// that publish only address lists are not here: without a list to check,
/// their User-Agent earns nothing.
pub static CRAWLERS: &[Crawler] = &[
    Crawler {
        name: "Googlebot",
        claims: &[
            "googlebot",
            "google-inspectiontool",
            "googleother",
            "adsbot-google",
            "mediapartners-google",
            "storebot-google",
        ],
        // Google's special-case crawlers resolve under
        // `gae.googleusercontent.com`; the rest of `googleusercontent.com`
        // is customers' Cloud VMs, whose owners set their own PTR records.
        domains: &["googlebot.com", "google.com", "gae.googleusercontent.com"],
    },
    Crawler {
        name: "Bingbot",
        claims: &["bingbot", "adidxbot", "msnbot"],
        domains: &["search.msn.com"],
    },
    Crawler {
        name: "Applebot",
        claims: &["applebot"],
        domains: &["applebot.apple.com"],
    },
    Crawler {
        name: "YandexBot",
        claims: &["yandexbot", "yandeximages", "yandexmobilebot"],
        domains: &["yandex.ru", "yandex.net", "yandex.com"],
    },
    Crawler {
        name: "Baiduspider",
        claims: &["baiduspider"],
        domains: &["baidu.com", "baidu.jp"],
    },
    Crawler {
        name: "Amazonbot",
        claims: &["amazonbot"],
        domains: &["crawl.amazonbot.amazon"],
    },
    Crawler {
        name: "PetalBot",
        claims: &["petalbot"],
        domains: &["petalsearch.com", "aspiegel.com"],
    },
];

/// The crawler a User-Agent claims to be, if any.
pub fn claimed(user_agent: &str) -> Option<&'static Crawler> {
    let lower = user_agent.to_ascii_lowercase();
    CRAWLERS
        .iter()
        .find(|crawler| crawler.claims.iter().any(|claim| lower.contains(claim)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_user_agent_claims_a_crawler() {
        let googlebot = "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)";
        assert_eq!(claimed(googlebot).map(|c| c.name), Some("Googlebot"));
        assert_eq!(
            claimed("Mozilla/5.0 (compatible; bingbot/2.0)").map(|c| c.name),
            Some("Bingbot")
        );
        assert_eq!(claimed("Mozilla/5.0 Chrome/141"), None);
    }

    #[test]
    fn only_the_published_domains_verify() {
        let google = claimed("Googlebot/2.1").unwrap();
        assert!(google.owns("crawl-66-249-66-1.googlebot.com"));
        assert!(google.owns("rate-limited-proxy-66-249-90-77.google.com."));
        assert!(
            !google.owns("googlebot.com"),
            "the bare domain is no crawler's name"
        );
        assert!(!google.owns("crawl-1.googlebot.com.attacker.net"));
        assert!(!google.owns("evilgooglebot.com"));
        assert!(google.owns("rate-limited-proxy-1.gae.googleusercontent.com"));
        assert!(
            !google.owns("1.2.3.4.bc.googleusercontent.com"),
            "a Google Cloud customer's VM is no crawler"
        );
        let bing = claimed("bingbot").unwrap();
        assert!(bing.owns("msnbot-157-55-39-1.search.msn.com"));
        assert!(!bing.owns("msnbot.search.msn.com.example"));
    }
}
