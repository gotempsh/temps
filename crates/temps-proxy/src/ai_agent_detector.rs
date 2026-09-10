// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Detects AI agent / LLM crawler user agents and classifies them by provider.
//!
//! Unlike the generic [`crate::crawler_detector::CrawlerDetector`], which only
//! returns whether a request *looks* like a bot, this module returns a stable
//! `(provider, agent)` pair so the UI can surface logos, group by provider, and
//! filter the request log to "show me everything from OpenAI / ChatGPT-User".
//!
//! User-agent strings are trivially spoofable. This detector is meant for
//! observability ("how much of my traffic is AI?"), not for blocking decisions.

use once_cell::sync::Lazy;
use regex::RegexSet;

/// One row in the AI-agent taxonomy.
#[derive(Debug, Clone, Copy)]
pub struct AiAgentMatch {
    /// Vendor / company behind the crawler (e.g. `"OpenAI"`).
    pub provider: &'static str,
    /// Canonical agent name (e.g. `"GPTBot"`, `"ChatGPT-User"`).
    pub agent: &'static str,
    /// What the crawler is doing (training, search, user-initiated fetch).
    pub purpose: AiAgentPurpose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiAgentPurpose {
    Training,
    Search,
    UserFetch,
    Mixed,
}

impl AiAgentPurpose {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Training => "training",
            Self::Search => "search",
            Self::UserFetch => "user_fetch",
            Self::Mixed => "mixed",
        }
    }
}

/// Ordered pattern → metadata table. The order matters: more specific tokens
/// (e.g. `OAI-SearchBot`) must come before more generic ones (`openai/`).
const AGENT_PATTERNS: &[(&str, AiAgentMatch)] = &[
    // OpenAI
    (
        r"(?i)\bGPTBot\b",
        AiAgentMatch {
            provider: "OpenAI",
            agent: "GPTBot",
            purpose: AiAgentPurpose::Training,
        },
    ),
    (
        r"(?i)\bOAI-SearchBot\b",
        AiAgentMatch {
            provider: "OpenAI",
            agent: "OAI-SearchBot",
            purpose: AiAgentPurpose::Search,
        },
    ),
    (
        r"(?i)\bChatGPT-User\b",
        AiAgentMatch {
            provider: "OpenAI",
            agent: "ChatGPT-User",
            purpose: AiAgentPurpose::UserFetch,
        },
    ),
    (
        r"(?i)\bopenai/",
        AiAgentMatch {
            provider: "OpenAI",
            agent: "OpenAI",
            purpose: AiAgentPurpose::Mixed,
        },
    ),
    // Anthropic
    (
        r"(?i)\bClaudeBot\b",
        AiAgentMatch {
            provider: "Anthropic",
            agent: "ClaudeBot",
            purpose: AiAgentPurpose::Training,
        },
    ),
    (
        r"(?i)\bClaude-SearchBot\b",
        AiAgentMatch {
            provider: "Anthropic",
            agent: "Claude-SearchBot",
            purpose: AiAgentPurpose::Search,
        },
    ),
    (
        r"(?i)\bClaude-User\b",
        AiAgentMatch {
            provider: "Anthropic",
            agent: "Claude-User",
            purpose: AiAgentPurpose::UserFetch,
        },
    ),
    (
        r"(?i)\banthropic-ai\b",
        AiAgentMatch {
            provider: "Anthropic",
            agent: "anthropic-ai",
            purpose: AiAgentPurpose::Mixed,
        },
    ),
    // Perplexity
    (
        r"(?i)\bPerplexityBot\b",
        AiAgentMatch {
            provider: "Perplexity",
            agent: "PerplexityBot",
            purpose: AiAgentPurpose::Search,
        },
    ),
    (
        r"(?i)\bPerplexity-User\b",
        AiAgentMatch {
            provider: "Perplexity",
            agent: "Perplexity-User",
            purpose: AiAgentPurpose::UserFetch,
        },
    ),
    // Google AI surfaces — Google-Extended is a robots.txt token, not a UA:
    // per Google's own developer docs, "Google-Extended doesn't have a
    // separate HTTP request user agent string. Crawling is done with
    // existing Google user agent strings; the robots.txt user-agent token
    // is used in a control capacity." So there is nothing to pattern-match
    // here — it can't be distinguished from plain Googlebot traffic at the
    // request level, only opted out of via robots.txt.
    //
    // GoogleOther and Gemini-Deep-Research, unlike Google-Extended, are real
    // distinct UAs.
    (
        r"(?i)\bGoogleOther\b",
        AiAgentMatch {
            provider: "Google",
            agent: "GoogleOther",
            purpose: AiAgentPurpose::Mixed,
        },
    ),
    // Gemini's Deep Research agent fetches pages live in response to a
    // user's research query (same category as ChatGPT-User / Claude-User),
    // distinct from both Googlebot indexing and Google-Extended's
    // training-opt-out token.
    (
        r"(?i)\bGemini-Deep-Research\b",
        AiAgentMatch {
            provider: "Google",
            agent: "Gemini-Deep-Research",
            purpose: AiAgentPurpose::UserFetch,
        },
    ),
    // Plain Googlebot. Google's own AI-features documentation says AI
    // Overviews and Gemini's Search-grounded answers are "rooted in our
    // core Search ranking and quality systems" — there is no separate
    // crawler for AI answers, they're built from the same index Googlebot
    // already crawls for regular Search. So this traffic is genuinely
    // dual-purpose (Search ranking + AI answer citation), not exclusively
    // a non-AI "Search / SEO crawler" the way Bingbot or YandexBot are.
    //
    // Caution: this is by far the highest-volume pattern in this table.
    // Ordinary Googlebot indexing crawls every page constantly, so this
    // will typically dominate the AI-agents view by orders of magnitude
    // over agents like GPTBot or ClaudeBot — expected, not a bug, but
    // worth knowing before reading the dashboard as "how much AI traffic
    // do I get."
    (
        r"(?i)\bGooglebot\b",
        AiAgentMatch {
            provider: "Google",
            agent: "Googlebot",
            purpose: AiAgentPurpose::Mixed,
        },
    ),
    // Bingbot — same dual-purpose reasoning as plain Googlebot above.
    // Microsoft's crawlers behind Bing Search and Copilot include Bingbot,
    // AdIdxBot, and BingPreview; Bingbot itself is the primary indexing
    // crawler and its access directly controls how content can appear in
    // Copilot's Bing-grounded answers, not just classic search results.
    (
        r"(?i)\bBingbot\b",
        AiAgentMatch {
            provider: "Microsoft",
            agent: "Bingbot",
            purpose: AiAgentPurpose::Mixed,
        },
    ),
    // Apple
    (
        r"(?i)\bApplebot-Extended\b",
        AiAgentMatch {
            provider: "Apple",
            agent: "Applebot-Extended",
            purpose: AiAgentPurpose::Training,
        },
    ),
    (
        r"(?i)\bApplebot\b",
        AiAgentMatch {
            provider: "Apple",
            agent: "Applebot",
            purpose: AiAgentPurpose::Search,
        },
    ),
    // Meta
    (
        r"(?i)\bMeta-ExternalAgent\b",
        AiAgentMatch {
            provider: "Meta",
            agent: "Meta-ExternalAgent",
            purpose: AiAgentPurpose::Training,
        },
    ),
    (
        r"(?i)\bMeta-ExternalFetcher\b",
        AiAgentMatch {
            provider: "Meta",
            agent: "Meta-ExternalFetcher",
            purpose: AiAgentPurpose::UserFetch,
        },
    ),
    // Amazon
    (
        r"(?i)\bAmazonbot\b",
        AiAgentMatch {
            provider: "Amazon",
            agent: "Amazonbot",
            purpose: AiAgentPurpose::Mixed,
        },
    ),
    // ByteDance
    (
        r"(?i)\bBytespider\b",
        AiAgentMatch {
            provider: "ByteDance",
            agent: "Bytespider",
            purpose: AiAgentPurpose::Training,
        },
    ),
    // Common Crawl
    (
        r"(?i)\bCCBot\b",
        AiAgentMatch {
            provider: "Common Crawl",
            agent: "CCBot",
            purpose: AiAgentPurpose::Training,
        },
    ),
    // DeepSeek
    (
        r"(?i)\bDeepSeekBot\b",
        AiAgentMatch {
            provider: "DeepSeek",
            agent: "DeepSeekBot",
            purpose: AiAgentPurpose::Mixed,
        },
    ),
    // Cohere
    (
        r"(?i)\bcohere-ai\b",
        AiAgentMatch {
            provider: "Cohere",
            agent: "cohere-ai",
            purpose: AiAgentPurpose::Mixed,
        },
    ),
    (
        r"(?i)\bcohere-training-data-crawler\b",
        AiAgentMatch {
            provider: "Cohere",
            agent: "cohere-training-data-crawler",
            purpose: AiAgentPurpose::Training,
        },
    ),
    // Diffbot
    (
        r"(?i)\bDiffbot\b",
        AiAgentMatch {
            provider: "Diffbot",
            agent: "Diffbot",
            purpose: AiAgentPurpose::Mixed,
        },
    ),
    // You.com
    (
        r"(?i)\bYouBot\b",
        AiAgentMatch {
            provider: "You.com",
            agent: "YouBot",
            purpose: AiAgentPurpose::Search,
        },
    ),
    // DuckDuckGo (DuckAssist)
    (
        r"(?i)\bDuckAssistBot\b",
        AiAgentMatch {
            provider: "DuckDuckGo",
            agent: "DuckAssistBot",
            purpose: AiAgentPurpose::Search,
        },
    ),
    // Brave
    (
        r"(?i)\bBraveBot\b",
        AiAgentMatch {
            provider: "Brave",
            agent: "BraveBot",
            purpose: AiAgentPurpose::Search,
        },
    ),
    // Andi
    (
        r"(?i)\bAndibot\b",
        AiAgentMatch {
            provider: "Andi",
            agent: "Andibot",
            purpose: AiAgentPurpose::Search,
        },
    ),
    // Omgili / Webz.io
    (
        r"(?i)\bOmgilibot\b",
        AiAgentMatch {
            provider: "Omgili",
            agent: "Omgilibot",
            purpose: AiAgentPurpose::Training,
        },
    ),
    (
        r"(?i)\bomgili\b",
        AiAgentMatch {
            provider: "Omgili",
            agent: "Omgili",
            purpose: AiAgentPurpose::Training,
        },
    ),
    // ImageSift
    (
        r"(?i)\bImagesiftBot\b",
        AiAgentMatch {
            provider: "ImageSift",
            agent: "ImagesiftBot",
            purpose: AiAgentPurpose::Training,
        },
    ),
    // Timpi
    (
        r"(?i)\bTimpibot\b",
        AiAgentMatch {
            provider: "Timpi",
            agent: "Timpibot",
            purpose: AiAgentPurpose::Search,
        },
    ),
    // Kangaroo
    (
        r"(?i)\bKangaroo Bot\b",
        AiAgentMatch {
            provider: "Kangaroo",
            agent: "Kangaroo Bot",
            purpose: AiAgentPurpose::Mixed,
        },
    ),
    // Mistral
    (
        r"(?i)\bMistralAI-User\b",
        AiAgentMatch {
            provider: "Mistral",
            agent: "MistralAI-User",
            purpose: AiAgentPurpose::UserFetch,
        },
    ),
    // xAI / Grok — xAI documents two separate tokens (GrokBot and
    // xAI-Grok). Independent observation has repeatedly found xAI's actual
    // crawler spoofing ordinary browser UAs rather than honoring either
    // one, so treat a match here as "traffic that *claims* to be xAI", not
    // a reliable signal of total xAI crawl volume.
    (
        r"(?i)\bGrokBot\b",
        AiAgentMatch {
            provider: "xAI",
            agent: "GrokBot",
            purpose: AiAgentPurpose::Training,
        },
    ),
    (
        r"(?i)\bxAI-Grok\b",
        AiAgentMatch {
            provider: "xAI",
            agent: "xAI-Grok",
            purpose: AiAgentPurpose::Training,
        },
    ),
];

/// Compiled multi-pattern regex set. `RegexSet::matches` returns the indices of
/// every pattern that matched, so we can find the most specific entry in a
/// single pass.
static AGENT_REGEX_SET: Lazy<RegexSet> = Lazy::new(|| {
    let patterns: Vec<&str> = AGENT_PATTERNS.iter().map(|(p, _)| *p).collect();
    RegexSet::new(&patterns).expect("Failed to compile AI agent regex set")
});

/// All known agents (used by the frontend dropdown).
pub fn known_agents() -> &'static [(&'static str, AiAgentMatch)] {
    AGENT_PATTERNS
}

/// Identify the AI agent behind a user-agent string. Returns `None` for any
/// request that isn't from a known AI agent.
pub fn detect(user_agent: Option<&str>) -> Option<AiAgentMatch> {
    let ua = user_agent?.trim();
    if ua.is_empty() {
        return None;
    }
    let matches: Vec<usize> = AGENT_REGEX_SET.matches(ua).into_iter().collect();
    // Earliest pattern wins — the table is intentionally ordered most specific
    // first so e.g. `OAI-SearchBot` is preferred over `openai/`.
    matches.into_iter().min().map(|idx| AGENT_PATTERNS[idx].1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_gptbot() {
        let m = detect(Some(
            "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko); compatible; GPTBot/1.2; +https://openai.com/gptbot",
        ))
        .expect("should detect GPTBot");
        assert_eq!(m.provider, "OpenAI");
        assert_eq!(m.agent, "GPTBot");
        assert_eq!(m.purpose, AiAgentPurpose::Training);
    }

    #[test]
    fn detects_chatgpt_user_over_generic_openai() {
        let m = detect(Some(
            "Mozilla/5.0 (compatible; ChatGPT-User/1.0; +https://openai.com/bot)",
        ))
        .expect("should detect ChatGPT-User");
        assert_eq!(m.agent, "ChatGPT-User");
        assert_eq!(m.purpose, AiAgentPurpose::UserFetch);
    }

    #[test]
    fn detects_claudebot() {
        let m = detect(Some(
            "Mozilla/5.0 (compatible; ClaudeBot/1.0; +https://www.anthropic.com)",
        ))
        .expect("should detect ClaudeBot");
        assert_eq!(m.provider, "Anthropic");
        assert_eq!(m.agent, "ClaudeBot");
    }

    #[test]
    fn detects_perplexity() {
        let m = detect(Some("PerplexityBot/1.0")).expect("should detect PerplexityBot");
        assert_eq!(m.provider, "Perplexity");
    }

    #[test]
    fn detects_meta_external_agent() {
        let m = detect(Some("meta-externalagent/1.1")).expect("should detect Meta-ExternalAgent");
        assert_eq!(m.provider, "Meta");
        assert_eq!(m.agent, "Meta-ExternalAgent");
    }

    #[test]
    fn detects_googlebot_as_mixed() {
        let m = detect(Some(
            "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)",
        ))
        .expect("should detect Googlebot");
        assert_eq!(m.provider, "Google");
        assert_eq!(m.agent, "Googlebot");
        assert_eq!(m.purpose, AiAgentPurpose::Mixed);
    }

    #[test]
    fn detects_googleother_distinct_from_googlebot() {
        let m = detect(Some("Mozilla/5.0 GoogleOther")).expect("should detect GoogleOther");
        assert_eq!(m.agent, "GoogleOther");
    }

    #[test]
    fn ignores_regular_browsers() {
        assert!(detect(Some(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"
        ))
        .is_none());
    }

    #[test]
    fn ignores_empty_and_missing_ua() {
        assert!(detect(None).is_none());
        assert!(detect(Some("")).is_none());
        assert!(detect(Some("   ")).is_none());
    }

    #[test]
    fn detects_bingbot_as_mixed() {
        let m = detect(Some(
            "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; bingbot/2.0; +http://www.bing.com/bingbot.htm) Chrome/116.0.1938.76 Safari/537.36",
        ))
        .expect("should detect bingbot");
        assert_eq!(m.provider, "Microsoft");
        assert_eq!(m.agent, "Bingbot");
        assert_eq!(m.purpose, AiAgentPurpose::Mixed);
    }

    #[test]
    fn detects_deepseekbot() {
        let m = detect(Some(
            "Mozilla/5.0 (compatible; DeepSeekBot/1.0; +https://www.deepseek.com/about)",
        ))
        .expect("should detect DeepSeekBot");
        assert_eq!(m.provider, "DeepSeek");
        assert_eq!(m.agent, "DeepSeekBot");
        assert_eq!(m.purpose, AiAgentPurpose::Mixed);
    }

    #[test]
    fn detects_gemini_deep_research_distinct_from_googlebot() {
        let m =
            detect(Some("Gemini-Deep-Research/1.0")).expect("should detect Gemini-Deep-Research");
        assert_eq!(m.provider, "Google");
        assert_eq!(m.agent, "Gemini-Deep-Research");
        assert_eq!(m.purpose, AiAgentPurpose::UserFetch);
    }

    #[test]
    fn detects_xai_grok_distinct_from_grokbot() {
        let grokbot = detect(Some("GrokBot/1.0")).expect("should detect GrokBot");
        assert_eq!(grokbot.agent, "GrokBot");

        let xai_grok = detect(Some("xAI-Grok/1.0")).expect("should detect xAI-Grok");
        assert_eq!(xai_grok.provider, "xAI");
        assert_eq!(xai_grok.agent, "xAI-Grok");
        assert_eq!(xai_grok.purpose, AiAgentPurpose::Training);
    }
}
