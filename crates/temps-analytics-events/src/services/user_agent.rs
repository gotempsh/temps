// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use woothee::parser::{Parser, WootheeResult};

/// Substring patterns (lowercased) identifying bots, crawlers, scrapers, and
/// link-preview unfurlers that woothee's crawler list does not reliably catch.
/// Covers modern AI crawlers and headless browsers — the blog is a frequent
/// target for these and they pollute analytics with zero-duration sessions.
const BOT_UA_PATTERNS: &[&str] = &[
    // AI crawlers
    "gptbot",
    "claudebot",
    "anthropic-ai",
    "claude-web",
    "perplexitybot",
    "ccbot",
    "bytespider",
    "google-extended",
    "applebot",
    "amazonbot",
    "meta-externalagent",
    "oai-searchbot",
    // Search / SEO crawlers
    "googlebot",
    "bingbot",
    "yandexbot",
    "duckduckbot",
    "baiduspider",
    "ahrefsbot",
    "semrushbot",
    "mj12bot",
    "dotbot",
    // Link-preview unfurlers
    "facebookexternalhit",
    "slackbot",
    "discordbot",
    "twitterbot",
    "linkedinbot",
    "whatsapp",
    "telegrambot",
    "pinterest",
    "redditbot",
    // Headless browsers / automation
    "headlesschrome",
    "phantomjs",
    "puppeteer",
    "playwright",
    "selenium",
    // Monitoring / generic
    "pingdom",
    "uptimerobot",
    "statuscake",
    "python-requests",
    "curl/",
    "wget/",
    "go-http-client",
    "node-fetch",
    "axios/",
    // Generic catch-all patterns (kept last)
    "bot",
    "crawler",
    "spider",
];

/// Detect whether a raw user-agent string belongs to a known bot/crawler.
/// An empty or missing UA is treated as a bot — real browsers always send one.
/// Returns the matched pattern name, or `None` for human traffic.
fn detect_bot(user_agent: Option<&str>) -> Option<String> {
    let Some(ua) = user_agent else {
        return Some("unknown".to_string());
    };
    let trimmed = ua.trim();
    if trimmed.is_empty() {
        return Some("unknown".to_string());
    }
    let lower = trimmed.to_lowercase();
    BOT_UA_PATTERNS
        .iter()
        .find(|pat| lower.contains(*pat))
        .map(|pat| pat.to_string())
}

#[derive(Debug, Clone, Default)]
pub struct ParsedUserAgent {
    pub browser: Option<String>,
    pub browser_version: Option<String>,
    pub operating_system: Option<String>,
    pub operating_system_version: Option<String>,
    pub device_type: Option<String>,
    /// The matched bot/crawler name, if this UA was identified as non-human.
    pub crawler_name: Option<String>,
}

impl ParsedUserAgent {
    /// Parse user agent string and extract browser information
    pub fn from_user_agent(user_agent: Option<&str>) -> Self {
        let crawler_name = detect_bot(user_agent);

        let Some(ua) = user_agent else {
            return Self {
                crawler_name,
                ..Self::default()
            };
        };

        if ua.trim().is_empty() {
            return Self {
                crawler_name,
                ..Self::default()
            };
        }

        let parser = Parser::new();
        let mut parsed = match parser.parse(ua) {
            Some(result) => Self::from_woothee_result(&result),
            None => Self::default(),
        };
        // Our substring match is the most specific signal and wins. woothee's
        // own crawler classification is only a fallback when the substring
        // list didn't match.
        parsed.crawler_name = crawler_name.or_else(|| {
            (parsed.device_type.as_deref() == Some("Bot")).then(|| "crawler".to_string())
        });
        parsed
    }

    /// Whether this user agent was identified as a bot, crawler, or scraper.
    pub fn is_bot(&self) -> bool {
        self.crawler_name.is_some()
    }

    /// The matched bot/crawler name, if any.
    pub fn crawler_name(&self) -> Option<String> {
        self.crawler_name.clone()
    }

    fn from_woothee_result(result: &WootheeResult) -> Self {
        Self {
            browser: Self::clean_name(result.name),
            browser_version: Self::clean_version(result.version),
            operating_system: Self::clean_name(result.os),
            operating_system_version: Self::clean_version(&result.os_version),
            device_type: Self::determine_device_type(result.category),
            crawler_name: None,
        }
    }

    fn clean_name(name: &str) -> Option<String> {
        if name.trim().is_empty() || name == "UNKNOWN" {
            None
        } else {
            Some(name.trim().to_string())
        }
    }

    fn clean_version(version: &str) -> Option<String> {
        if version.trim().is_empty() || version == "UNKNOWN" {
            None
        } else {
            Some(version.trim().to_string())
        }
    }

    fn determine_device_type(category: &str) -> Option<String> {
        match category {
            "pc" => Some("Desktop".to_string()),
            "smartphone" => Some("Mobile".to_string()),
            "mobilephone" => Some("Mobile".to_string()),
            "tablet" => Some("Tablet".to_string()),
            "appliance" => Some("Smart TV".to_string()),
            "crawler" => Some("Bot".to_string()),
            "misc" => Some("Other".to_string()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chrome_desktop() {
        let ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/119.0.0.0 Safari/537.36";
        let info = ParsedUserAgent::from_user_agent(Some(ua));

        assert_eq!(info.browser, Some("Chrome".to_string()));
        assert_eq!(info.operating_system, Some("Windows 10".to_string()));
        assert_eq!(info.device_type, Some("Desktop".to_string()));
    }

    #[test]
    fn test_safari_mobile() {
        let ua = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.1 Mobile/15E148 Safari/604.1";
        let info = ParsedUserAgent::from_user_agent(Some(ua));

        assert_eq!(info.browser, Some("Safari".to_string()));
        assert_eq!(info.operating_system, Some("iPhone".to_string()));
        assert_eq!(info.device_type, Some("Mobile".to_string()));
    }

    #[test]
    fn test_firefox_linux() {
        let ua = "Mozilla/5.0 (X11; Linux x86_64; rv:109.0) Gecko/20100101 Firefox/119.0";
        let info = ParsedUserAgent::from_user_agent(Some(ua));

        assert_eq!(info.browser, Some("Firefox".to_string()));
        assert_eq!(info.operating_system, Some("Linux".to_string()));
        assert_eq!(info.device_type, Some("Desktop".to_string()));
    }

    #[test]
    fn test_empty_user_agent() {
        let info = ParsedUserAgent::from_user_agent(None);
        assert_eq!(info.browser, None);
        assert_eq!(info.operating_system, None);
        assert_eq!(info.device_type, None);
    }

    #[test]
    fn test_bot_user_agent() {
        let ua = "Googlebot/2.1 (+http://www.google.com/bot.html)";
        let info = ParsedUserAgent::from_user_agent(Some(ua));

        assert_eq!(info.device_type, Some("Bot".to_string()));
    }

    #[test]
    fn test_android_chrome() {
        let ua = "Mozilla/5.0 (Linux; Android 13; SM-S908B) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/119.0.0.0 Mobile Safari/537.36";
        let info = ParsedUserAgent::from_user_agent(Some(ua));

        assert_eq!(info.browser, Some("Chrome".to_string()));
        assert_eq!(info.operating_system, Some("Android".to_string()));
        assert_eq!(info.device_type, Some("Mobile".to_string()));
    }

    #[test]
    fn test_gptbot_is_bot() {
        let ua = "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko); compatible; GPTBot/1.2; +https://openai.com/gptbot";
        let info = ParsedUserAgent::from_user_agent(Some(ua));
        assert!(info.is_bot());
        assert_eq!(info.crawler_name(), Some("gptbot".to_string()));
    }

    #[test]
    fn test_claudebot_is_bot() {
        let ua = "Mozilla/5.0 (compatible; ClaudeBot/1.0; +claudebot@anthropic.com)";
        let info = ParsedUserAgent::from_user_agent(Some(ua));
        assert!(info.is_bot());
        assert_eq!(info.crawler_name(), Some("claudebot".to_string()));
    }

    #[test]
    fn test_facebook_unfurler_is_bot() {
        let ua = "facebookexternalhit/1.1 (+http://www.facebook.com/externalhit_uatext.php)";
        let info = ParsedUserAgent::from_user_agent(Some(ua));
        assert!(info.is_bot());
        assert_eq!(info.crawler_name(), Some("facebookexternalhit".to_string()));
    }

    #[test]
    fn test_headless_chrome_is_bot() {
        let ua = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) HeadlessChrome/119.0.0.0 Safari/537.36";
        let info = ParsedUserAgent::from_user_agent(Some(ua));
        assert!(info.is_bot());
        assert_eq!(info.crawler_name(), Some("headlesschrome".to_string()));
    }

    #[test]
    fn test_empty_ua_is_bot() {
        assert!(ParsedUserAgent::from_user_agent(None).is_bot());
        assert!(ParsedUserAgent::from_user_agent(Some("")).is_bot());
        assert!(ParsedUserAgent::from_user_agent(Some("   ")).is_bot());
    }

    #[test]
    fn test_real_chrome_is_not_bot() {
        let ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/119.0.0.0 Safari/537.36";
        let info = ParsedUserAgent::from_user_agent(Some(ua));
        assert!(!info.is_bot());
        assert_eq!(info.crawler_name(), None);
    }

    #[test]
    fn test_real_safari_mobile_is_not_bot() {
        let ua = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.1 Mobile/15E148 Safari/604.1";
        let info = ParsedUserAgent::from_user_agent(Some(ua));
        assert!(!info.is_bot());
    }

    #[test]
    fn test_googlebot_caught_by_substring() {
        let ua = "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)";
        let info = ParsedUserAgent::from_user_agent(Some(ua));
        assert!(info.is_bot());
    }
}
