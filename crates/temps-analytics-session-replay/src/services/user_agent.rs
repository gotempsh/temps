use serde::{Deserialize, Serialize};
use woothee::parser::{Parser, WootheeResult};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BrowserInfo {
    pub browser: Option<String>,
    pub browser_version: Option<String>,
    pub operating_system: Option<String>,
    pub operating_system_version: Option<String>,
    pub device_type: Option<String>,
}

impl BrowserInfo {
    /// Parse user agent string and extract browser information
    pub fn from_user_agent(user_agent: Option<&str>) -> Self {
        let Some(ua) = user_agent else {
            return Self::default();
        };

        if ua.trim().is_empty() {
            return Self::default();
        }

        let parser = Parser::new();
        match parser.parse(ua) {
            Some(result) => Self::from_woothee_result(&result),
            None => Self::default(),
        }
    }

    fn from_woothee_result(result: &WootheeResult) -> Self {
        Self {
            browser: Self::clean_name(result.name),
            browser_version: Self::clean_version(result.version),
            operating_system: Self::clean_name(result.os),
            operating_system_version: Self::clean_version(&result.os_version),
            device_type: Self::determine_device_type(result.category),
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
        let info = BrowserInfo::from_user_agent(Some(ua));

        assert_eq!(info.browser, Some("Chrome".to_string()));
        assert_eq!(info.operating_system, Some("Windows 10".to_string()));
        assert_eq!(info.device_type, Some("Desktop".to_string()));
    }

    #[test]
    fn test_safari_mobile() {
        let ua = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.1 Mobile/15E148 Safari/604.1";
        let info = BrowserInfo::from_user_agent(Some(ua));

        assert_eq!(info.browser, Some("Safari".to_string()));
        assert_eq!(info.operating_system, Some("iPhone".to_string()));
        assert_eq!(info.device_type, Some("Mobile".to_string()));
    }

    #[test]
    fn test_empty_user_agent() {
        let info = BrowserInfo::from_user_agent(None);
        assert_eq!(info.browser, None);
        assert_eq!(info.operating_system, None);
        assert_eq!(info.device_type, None);
    }

    #[test]
    fn test_bot_user_agent() {
        let ua = "Googlebot/2.1 (+http://www.google.com/bot.html)";
        let info = BrowserInfo::from_user_agent(Some(ua));

        assert_eq!(info.device_type, Some("Bot".to_string()));
    }
}
