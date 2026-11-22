// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The browser and operating system a visitor's user agent names, parsed with
//! the same library and names as event ingest, so the visitor list labels
//! them the way the analytics breakdowns do.

use woothee::parser::Parser;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserAgentSummary {
    pub browser: Option<String>,
    pub browser_version: Option<String>,
    pub operating_system: Option<String>,
    pub operating_system_version: Option<String>,
}

impl UserAgentSummary {
    /// Parses `user_agent` with a shared `parser`; a missing, empty or
    /// unrecognised user agent yields no names rather than a placeholder,
    /// and so does a crawler's: its name is not a browser (the visitor
    /// already carries `crawler_name`).
    pub fn parse(parser: &Parser, user_agent: Option<&str>) -> Self {
        let Some(user_agent) = user_agent.map(str::trim).filter(|ua| !ua.is_empty()) else {
            return Self::default();
        };
        let Some(result) = parser.parse(user_agent) else {
            return Self::default();
        };
        if result.category == "crawler" {
            return Self::default();
        }
        Self {
            browser: known(result.name),
            browser_version: known(result.version),
            operating_system: known(result.os),
            operating_system_version: known(&result.os_version),
        }
    }
}

fn known(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value != "UNKNOWN").then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(user_agent: Option<&str>) -> UserAgentSummary {
        UserAgentSummary::parse(&Parser::new(), user_agent)
    }

    #[test]
    fn desktop_browser_and_os_come_with_their_versions() {
        let summary = parse(Some(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/119.0.0.0 Safari/537.36",
        ));
        assert_eq!(summary.browser.as_deref(), Some("Chrome"));
        assert_eq!(summary.browser_version.as_deref(), Some("119.0.0.0"));
        assert_eq!(summary.operating_system.as_deref(), Some("Windows 10"));
        assert_eq!(summary.operating_system_version.as_deref(), Some("NT 10.0"));
    }

    #[test]
    fn mobile_safari_names_the_device_os_and_its_version() {
        let summary = parse(Some(
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_1 like Mac OS X) AppleWebKit/605.1.15 \
             (KHTML, like Gecko) Version/17.1 Mobile/15E148 Safari/604.1",
        ));
        assert_eq!(summary.browser.as_deref(), Some("Safari"));
        assert_eq!(summary.browser_version.as_deref(), Some("17.1"));
        assert_eq!(summary.operating_system.as_deref(), Some("iPhone"));
        assert_eq!(summary.operating_system_version.as_deref(), Some("17.1"));
    }

    #[tokio::test]
    async fn the_visitor_list_names_browser_and_os_with_versions() -> anyhow::Result<()> {
        use crate::Analytics;
        use chrono::{Duration, Utc};
        use sea_orm::{ConnectionTrait, EntityTrait};
        use std::sync::Arc;
        use temps_entities::visitor;

        let (analytics, db, _container) = crate::create_test_analytics_service!(
            "the_visitor_list_names_browser_and_os_with_versions"
        );
        let seeded = visitor::Entity::find()
            .one(db.as_ref())
            .await?
            .ok_or_else(|| anyhow::anyhow!("the seeded visitor exists"))?;
        let list = || {
            analytics.get_visitors(
                seeded.first_seen - Duration::days(1),
                Utc::now() + Duration::days(1),
                seeded.project_id,
                None,
                None,
                None,
                None,
                None,
                Default::default(),
            )
        };

        // No user agent: nothing is named, not "Unknown".
        let before = list().await?;
        let row = before
            .visitors
            .iter()
            .find(|v| v.id == seeded.id)
            .ok_or_else(|| anyhow::anyhow!("seeded visitor listed"))?;
        assert_eq!(
            (row.browser.as_deref(), row.operating_system.as_deref()),
            (None, None)
        );

        db.execute_unprepared(&format!(
            "UPDATE visitor SET user_agent = 'Mozilla/5.0 (X11; Linux x86_64; rv:120.0) \
             Gecko/20100101 Firefox/120.0' WHERE id = {}",
            seeded.id
        ))
        .await?;
        let after = list().await?;
        let row = after
            .visitors
            .iter()
            .find(|v| v.id == seeded.id)
            .ok_or_else(|| anyhow::anyhow!("seeded visitor listed"))?;
        assert_eq!(row.browser.as_deref(), Some("Firefox"));
        assert_eq!(row.browser_version.as_deref(), Some("120.0"));
        assert_eq!(row.operating_system.as_deref(), Some("Linux"));
        Ok(())
    }

    #[test]
    fn missing_empty_or_unrecognised_user_agents_name_nothing() {
        assert_eq!(parse(None), UserAgentSummary::default());
        assert_eq!(parse(Some("   ")), UserAgentSummary::default());
        assert_eq!(parse(Some("curl-like-tool")), UserAgentSummary::default());
        assert_eq!(
            parse(Some(
                "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)"
            )),
            UserAgentSummary::default()
        );
    }
}
