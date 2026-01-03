//! Channel attribution logic for UTM parameters and referrer analysis
//!
//! This module provides functions to:
//! - Parse UTM parameters from query strings
//! - Extract hostname from referrer URLs
//! - Compute marketing channel attribution

use std::collections::HashMap;

/// UTM parameters extracted from a URL query string
#[derive(Debug, Clone, Default)]
pub struct UtmParams {
    pub utm_source: Option<String>,
    pub utm_medium: Option<String>,
    pub utm_campaign: Option<String>,
    pub utm_content: Option<String>,
    pub utm_term: Option<String>,
    /// Google Ads click ID
    pub gclid: Option<String>,
    /// Google Ads source parameter
    pub gad_source: Option<String>,
}

impl UtmParams {
    /// Check if any UTM parameter is present
    pub fn has_any(&self) -> bool {
        self.utm_source.is_some()
            || self.utm_medium.is_some()
            || self.utm_campaign.is_some()
            || self.utm_content.is_some()
            || self.utm_term.is_some()
            || self.gclid.is_some()
            || self.gad_source.is_some()
    }
}

/// Marketing channel classification
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Channel {
    Direct,
    OrganicSearch,
    PaidSearch,
    OrganicSocial,
    PaidSocial,
    Email,
    Referral,
    Display,
    Affiliate,
    Video,
    Audio,
    Sms,
    MobileApp,
    Unknown,
}

impl Channel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Channel::Direct => "Direct",
            Channel::OrganicSearch => "Organic Search",
            Channel::PaidSearch => "Paid Search",
            Channel::OrganicSocial => "Organic Social",
            Channel::PaidSocial => "Paid Social",
            Channel::Email => "Email",
            Channel::Referral => "Referral",
            Channel::Display => "Display",
            Channel::Affiliate => "Affiliate",
            Channel::Video => "Video",
            Channel::Audio => "Audio",
            Channel::Sms => "SMS",
            Channel::MobileApp => "Mobile App",
            Channel::Unknown => "Unknown",
        }
    }
}

impl std::fmt::Display for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Parse UTM parameters from a query string
///
/// # Arguments
/// * `query_string` - The query string (with or without leading `?`)
///
/// # Example
/// ```
/// use temps_analytics::channel::parse_utm_params;
///
/// let params = parse_utm_params("utm_source=google&utm_medium=cpc&utm_campaign=spring_sale");
/// assert_eq!(params.utm_source, Some("google".to_string()));
/// assert_eq!(params.utm_medium, Some("cpc".to_string()));
/// ```
pub fn parse_utm_params(query_string: &str) -> UtmParams {
    let query = query_string.trim_start_matches('?');

    let params: HashMap<String, String> = query
        .split('&')
        .filter_map(|pair| {
            let mut parts = pair.splitn(2, '=');
            let key = parts.next()?.to_lowercase();
            let value = parts.next().unwrap_or("");
            // URL decode the value
            let decoded = urlencoding::decode(value).ok()?.into_owned();
            if decoded.is_empty() {
                None
            } else {
                Some((key, decoded))
            }
        })
        .collect();

    UtmParams {
        utm_source: params.get("utm_source").cloned(),
        utm_medium: params.get("utm_medium").cloned(),
        utm_campaign: params.get("utm_campaign").cloned(),
        utm_content: params.get("utm_content").cloned(),
        utm_term: params.get("utm_term").cloned(),
        gclid: params.get("gclid").cloned(),
        gad_source: params.get("gad_source").cloned(),
    }
}

/// Extract hostname from a referrer URL
///
/// # Arguments
/// * `referrer` - The full referrer URL
///
/// # Returns
/// The hostname portion of the URL, or None if invalid
///
/// # Example
/// ```
/// use temps_analytics::channel::extract_referrer_hostname;
///
/// let hostname = extract_referrer_hostname("https://www.google.com/search?q=test");
/// assert_eq!(hostname, Some("www.google.com".to_string()));
/// ```
pub fn extract_referrer_hostname(referrer: &str) -> Option<String> {
    if referrer.is_empty() {
        return None;
    }

    // Try to parse as URL
    if let Ok(url) = url::Url::parse(referrer) {
        return url.host_str().map(|h| h.to_string());
    }

    // Fallback: manual extraction
    let referrer = referrer.trim();
    let without_protocol = if referrer.starts_with("https://") {
        &referrer[8..]
    } else if referrer.starts_with("http://") {
        &referrer[7..]
    } else {
        referrer
    };

    // Get hostname (everything before first / or ?)
    let hostname = without_protocol
        .split('/')
        .next()
        .unwrap_or(without_protocol)
        .split('?')
        .next()
        .unwrap_or(without_protocol);

    if hostname.is_empty() {
        None
    } else {
        Some(hostname.to_string())
    }
}

/// Known search engine domains
const SEARCH_ENGINES: &[&str] = &[
    "google",
    "bing",
    "yahoo",
    "duckduckgo",
    "baidu",
    "yandex",
    "ecosia",
    "ask",
    "aol",
    "naver",
    "seznam",
];

/// Known social media domains
const SOCIAL_NETWORKS: &[&str] = &[
    "facebook",
    "twitter",
    "instagram",
    "linkedin",
    "pinterest",
    "reddit",
    "tiktok",
    "snapchat",
    "youtube",
    "tumblr",
    "whatsapp",
    "telegram",
    "discord",
    "slack",
    "t.co",
    "fb.com",
    "x.com",
];

/// Known video platforms
const VIDEO_PLATFORMS: &[&str] = &["youtube", "vimeo", "dailymotion", "twitch"];

/// Check if a hostname belongs to a search engine
fn is_search_engine(hostname: &str) -> bool {
    let hostname_lower = hostname.to_lowercase();
    SEARCH_ENGINES.iter().any(|se| hostname_lower.contains(se))
}

/// Check if a hostname belongs to a social network
fn is_social_network(hostname: &str) -> bool {
    let hostname_lower = hostname.to_lowercase();
    SOCIAL_NETWORKS.iter().any(|sn| hostname_lower.contains(sn))
}

/// Check if a hostname is a video platform
fn is_video_platform(hostname: &str) -> bool {
    let hostname_lower = hostname.to_lowercase();
    VIDEO_PLATFORMS.iter().any(|vp| hostname_lower.contains(vp))
}

/// Determine marketing channel from UTM parameters and referrer
///
/// This follows Google Analytics 4 default channel groupings:
/// <https://support.google.com/analytics/answer/9756891>
///
/// # Arguments
/// * `utm` - Parsed UTM parameters
/// * `referrer_hostname` - The hostname of the referrer (if any)
/// * `current_hostname` - The hostname of the current site (to detect self-referrals)
///
/// # Returns
/// The determined marketing channel
pub fn get_channel(
    utm: &UtmParams,
    referrer_hostname: Option<&str>,
    current_hostname: Option<&str>,
) -> Channel {
    let medium = utm.utm_medium.as_deref().map(|m| m.to_lowercase());
    let source = utm.utm_source.as_deref().map(|s| s.to_lowercase());

    // Check for Google Ads click ID - always Paid Search
    if utm.gclid.is_some() || utm.gad_source.is_some() {
        return Channel::PaidSearch;
    }

    // Check medium-based classification first
    if let Some(ref med) = medium {
        // Paid Search
        if med == "cpc"
            || med == "ppc"
            || med == "paidsearch"
            || med == "paid-search"
            || med == "paid_search"
        {
            return Channel::PaidSearch;
        }

        // Paid Social
        if med == "paidsocial"
            || med == "paid-social"
            || med == "paid_social"
            || med == "social-paid"
        {
            return Channel::PaidSocial;
        }

        // Organic Social
        if med == "social"
            || med == "social-media"
            || med == "social_media"
            || med == "sm"
            || med == "organic-social"
        {
            return Channel::OrganicSocial;
        }

        // Email
        if med == "email" || med == "e-mail" || med == "newsletter" {
            return Channel::Email;
        }

        // Display/Banner
        if med == "display"
            || med == "banner"
            || med == "cpm"
            || med == "programmatic"
            || med == "native"
        {
            return Channel::Display;
        }

        // Affiliate
        if med == "affiliate" || med == "partner" || med == "referral-partner" {
            return Channel::Affiliate;
        }

        // Video
        if med == "video" || med == "youtube" {
            return Channel::Video;
        }

        // Audio/Podcast
        if med == "audio" || med == "podcast" {
            return Channel::Audio;
        }

        // SMS
        if med == "sms" || med == "text" {
            return Channel::Sms;
        }

        // Mobile App
        if med == "app" || med == "mobile-app" || med == "in-app" || med == "push" {
            return Channel::MobileApp;
        }

        // Organic Search (explicit)
        if med == "organic" || med == "organic-search" {
            return Channel::OrganicSearch;
        }

        // Referral (explicit)
        if med == "referral" || med == "link" || med == "website" {
            return Channel::Referral;
        }
    }

    // Check source-based classification if medium didn't match
    if let Some(ref src) = source {
        // Check for search engines
        if is_search_engine(src) {
            // If medium is cpc/ppc, it's paid (already handled above)
            // Otherwise organic
            return Channel::OrganicSearch;
        }

        // Check for social networks
        if is_social_network(src) {
            return Channel::OrganicSocial;
        }
    }

    // No UTM params - use referrer analysis
    if let Some(ref_host) = referrer_hostname {
        // Check for self-referral
        if let Some(current) = current_hostname {
            if ref_host.contains(current) || current.contains(ref_host) {
                // Self-referral, treat as continuation of session
                return Channel::Direct;
            }
        }

        // Check referrer hostname for channel hints
        if is_search_engine(ref_host) {
            return Channel::OrganicSearch;
        }

        if is_social_network(ref_host) {
            return Channel::OrganicSocial;
        }

        if is_video_platform(ref_host) {
            return Channel::Video;
        }

        // Known email providers as referrer
        if ref_host.contains("mail") || ref_host.contains("outlook") || ref_host.contains("gmail") {
            return Channel::Email;
        }

        // Has referrer but doesn't match known categories
        return Channel::Referral;
    }

    // No referrer and no UTM - Direct traffic
    Channel::Direct
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_utm_params_basic() {
        let params = parse_utm_params("utm_source=google&utm_medium=cpc&utm_campaign=spring_sale");

        assert_eq!(params.utm_source, Some("google".to_string()));
        assert_eq!(params.utm_medium, Some("cpc".to_string()));
        assert_eq!(params.utm_campaign, Some("spring_sale".to_string()));
        assert_eq!(params.utm_content, None);
        assert_eq!(params.utm_term, None);
    }

    #[test]
    fn test_parse_utm_params_with_question_mark() {
        let params = parse_utm_params("?utm_source=newsletter&utm_medium=email");

        assert_eq!(params.utm_source, Some("newsletter".to_string()));
        assert_eq!(params.utm_medium, Some("email".to_string()));
    }

    #[test]
    fn test_parse_utm_params_url_encoded() {
        let params = parse_utm_params("utm_campaign=spring%20sale%202024");

        assert_eq!(params.utm_campaign, Some("spring sale 2024".to_string()));
    }

    #[test]
    fn test_parse_utm_params_with_gclid() {
        let params = parse_utm_params("gclid=abc123&utm_source=google");

        assert_eq!(params.gclid, Some("abc123".to_string()));
        assert_eq!(params.utm_source, Some("google".to_string()));
    }

    #[test]
    fn test_extract_referrer_hostname() {
        assert_eq!(
            extract_referrer_hostname("https://www.google.com/search?q=test"),
            Some("www.google.com".to_string())
        );

        assert_eq!(
            extract_referrer_hostname("http://facebook.com/share"),
            Some("facebook.com".to_string())
        );

        assert_eq!(extract_referrer_hostname(""), None);
    }

    #[test]
    fn test_channel_direct() {
        let utm = UtmParams::default();
        assert_eq!(get_channel(&utm, None, None), Channel::Direct);
    }

    #[test]
    fn test_channel_paid_search_cpc() {
        let utm = UtmParams {
            utm_source: Some("google".to_string()),
            utm_medium: Some("cpc".to_string()),
            ..Default::default()
        };
        assert_eq!(get_channel(&utm, None, None), Channel::PaidSearch);
    }

    #[test]
    fn test_channel_paid_search_gclid() {
        let utm = UtmParams {
            gclid: Some("abc123".to_string()),
            ..Default::default()
        };
        assert_eq!(get_channel(&utm, None, None), Channel::PaidSearch);
    }

    #[test]
    fn test_channel_organic_search_referrer() {
        let utm = UtmParams::default();
        assert_eq!(
            get_channel(&utm, Some("www.google.com"), None),
            Channel::OrganicSearch
        );
    }

    #[test]
    fn test_channel_organic_social_utm() {
        let utm = UtmParams {
            utm_source: Some("facebook".to_string()),
            utm_medium: Some("social".to_string()),
            ..Default::default()
        };
        assert_eq!(get_channel(&utm, None, None), Channel::OrganicSocial);
    }

    #[test]
    fn test_channel_paid_social() {
        let utm = UtmParams {
            utm_source: Some("facebook".to_string()),
            utm_medium: Some("paidsocial".to_string()),
            ..Default::default()
        };
        assert_eq!(get_channel(&utm, None, None), Channel::PaidSocial);
    }

    #[test]
    fn test_channel_email() {
        let utm = UtmParams {
            utm_source: Some("newsletter".to_string()),
            utm_medium: Some("email".to_string()),
            ..Default::default()
        };
        assert_eq!(get_channel(&utm, None, None), Channel::Email);
    }

    #[test]
    fn test_channel_referral() {
        let utm = UtmParams::default();
        assert_eq!(
            get_channel(&utm, Some("example-blog.com"), Some("mysite.com")),
            Channel::Referral
        );
    }

    #[test]
    fn test_channel_self_referral_is_direct() {
        let utm = UtmParams::default();
        assert_eq!(
            get_channel(&utm, Some("mysite.com"), Some("mysite.com")),
            Channel::Direct
        );
    }

    #[test]
    fn test_channel_display() {
        let utm = UtmParams {
            utm_medium: Some("display".to_string()),
            ..Default::default()
        };
        assert_eq!(get_channel(&utm, None, None), Channel::Display);
    }

    #[test]
    fn test_channel_social_network_detection() {
        let utm = UtmParams {
            utm_source: Some("twitter".to_string()),
            ..Default::default()
        };
        assert_eq!(get_channel(&utm, None, None), Channel::OrganicSocial);
    }
}
