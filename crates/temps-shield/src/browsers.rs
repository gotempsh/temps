// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A User-Agent that claims a browser from a client that is not one (ADR
//! 066, D4, use 2).
//!
//! Only two signals count, both conservative:
//!
//! - the JA4 is one of [`KNOWN_TOOLS`], fingerprints captured from HTTP
//!   libraries and tools (`tests/data/ja4_wireshark.txt`);
//! - the client offered no ALPN. Every current browser offers `h2` and
//!   `http/1.1`, so a "browser" that offers neither is a library.
//!
//! A mismatch is a `challenge` reason, never a block: a browser behind a
//! TLS-inspecting middlebox presents the middlebox's fingerprint.

/// JA4 fingerprints of tools, as captured. Browser fingerprints change with
/// every release; these change only with the TLS library underneath.
pub const KNOWN_TOOLS: &[(&str, &str)] = &[
    ("t13d3012h2_1d37bd780c83_882d495ac381", "curl"),
    ("t13d3012h1_1d37bd780c83_882d495ac381", "curl"),
    ("t13d3012h2_1d37bd780c83_8e6e362c5eac", "openssl s_client"),
    ("t12i270700_a2460661a67a_36cef8aed422", "openssl s_client"),
    ("t13d171100_ab0a1bf427ad_8e6e362c5eac", "Python ssl"),
];

/// JA4 fingerprints of current browsers. All Chrome builds of a release
/// line share one; the list is kept short and only ever keeps a fingerprint
/// from being learned as hostile (D9), so an entry that goes stale costs
/// nothing.
pub const KNOWN_BROWSERS: &[(&str, &str)] = &[
    ("t13d1516h2_8daaf6152771_02713d6af862", "Chrome"),
    ("t13d1516h2_8daaf6152771_e5627efa2ab1", "Chrome"),
];

/// Whether `ja4` is on [`KNOWN_BROWSERS`].
pub fn is_known_browser(ja4: &str) -> bool {
    KNOWN_BROWSERS.iter().any(|(known, _)| *known == ja4)
}

/// The browser a User-Agent claims to be, if it claims one. Crawlers that
/// say they are compatible with Mozilla do not claim to be a browser.
pub fn claimed_browser(user_agent: &str) -> Option<&'static str> {
    if !user_agent.starts_with("Mozilla/5.0") {
        return None;
    }
    let lower = user_agent.to_ascii_lowercase();
    if ["bot", "crawler", "spider", "headless", "preview"]
        .iter()
        .any(|word| lower.contains(word))
    {
        return None;
    }
    // Order matters: Edge says Chrome and Safari, Chrome says Safari.
    [
        ("Edg/", "Edge"),
        ("Firefox/", "Firefox"),
        ("Chrome/", "Chrome"),
        ("CriOS/", "Chrome"),
        ("Safari/", "Safari"),
    ]
    .into_iter()
    .find(|(token, _)| user_agent.contains(token))
    .map(|(_, name)| name)
}

/// The tool a JA4 belongs to, if it is a known one or offers no ALPN.
pub fn tool_behind(ja4: &str) -> Option<&'static str> {
    if let Some((_, tool)) = KNOWN_TOOLS.iter().find(|(known, _)| *known == ja4) {
        return Some(tool);
    }
    // ja4_a: protocol, version (2), SNI, ciphers (2), extensions (2), ALPN (2).
    (ja4.get(8..10) == Some("00")).then_some("a client without ALPN")
}

/// The browser claimed and the tool seen, when they disagree.
pub fn mismatch(user_agent: &str, ja4: Option<&str>) -> Option<(&'static str, &'static str)> {
    let claimed = claimed_browser(user_agent)?;
    let tool = tool_behind(ja4?)?;
    Some((claimed, tool))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHROME: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";
    const EDGE: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like \
                        Gecko) Chrome/141.0.0.0 Safari/537.36 Edg/141.0.0.0";
    const GOOGLEBOT: &str =
        "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)";
    const BROWSER_JA4: &str = "t13d1516h2_8daaf6152771_02713d6af862";

    #[test]
    fn browsers_and_tools_are_told_apart() {
        assert!(is_known_browser(BROWSER_JA4));
        assert!(!is_known_browser("t13d3012h2_1d37bd780c83_882d495ac381"));
        for (ja4, _) in KNOWN_BROWSERS {
            assert_eq!(tool_behind(ja4), None, "{ja4}");
        }
    }

    #[test]
    fn a_browser_user_agent_names_its_browser() {
        assert_eq!(claimed_browser(CHROME), Some("Chrome"));
        assert_eq!(claimed_browser(EDGE), Some("Edge"));
        assert_eq!(claimed_browser(GOOGLEBOT), None);
        assert_eq!(claimed_browser("curl/8.5.0"), None);
    }

    #[test]
    fn only_a_known_tool_or_a_missing_alpn_is_a_mismatch() {
        assert_eq!(
            mismatch(CHROME, Some("t13d3012h2_1d37bd780c83_882d495ac381")),
            Some(("Chrome", "curl"))
        );
        assert_eq!(
            mismatch(CHROME, Some("t13d171100_ab0a1bf427ad_8e6e362c5eac")),
            Some(("Chrome", "Python ssl"))
        );
        assert_eq!(
            mismatch(CHROME, Some("t13d151600_8daaf6152771_02713d6af862")),
            Some(("Chrome", "a client without ALPN"))
        );
        assert_eq!(mismatch(CHROME, Some(BROWSER_JA4)), None);
        assert_eq!(mismatch(CHROME, None), None);
        // curl saying it is curl is not lying.
        assert_eq!(
            mismatch("curl/8.5.0", Some("t13d3012h2_1d37bd780c83_882d495ac381")),
            None
        );
    }
}
