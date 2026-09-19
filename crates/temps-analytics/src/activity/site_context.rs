// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::types::ActivityError;
use scraper::{Html, Selector};
use serde::Serialize;
use std::time::Duration;
use temps_core::url_validation::{resolve_and_validate_domain, validate_external_url};
use url::Url;

#[derive(Debug, Serialize)]
pub(super) struct SitePage {
    pub url: String,
    pub text: String,
}

fn error(project_id: i32, reason: impl std::fmt::Display) -> ActivityError {
    ActivityError::Analysis {
        project_id,
        reason: format!("Read public site: {reason}"),
    }
}

pub(super) fn public_url(project_id: i32, raw: &str) -> Result<Url, ActivityError> {
    let url = validate_external_url(raw).map_err(|e| ActivityError::Validation {
        project_id,
        reason: format!("Choose a public HTTP(S) URL: {e}"),
    })?;
    if raw.len() > 2048
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ActivityError::Validation { project_id,
            reason: "Use a public page URL without credentials, query parameters or fragments (at most 2048 bytes)".into() });
    }
    Ok(url)
}

async fn fetch(project_id: i32, url: &Url) -> Result<String, ActivityError> {
    let host = url
        .host_str()
        .ok_or_else(|| error(project_id, "Missing hostname"))?;
    let addresses = resolve_and_validate_domain(host, url.port_or_known_default().unwrap_or(443))
        .await
        .map_err(|e| error(project_id, e))?;
    // Pin the validated addresses to prevent DNS rebinding. No ambient proxies,
    // cookies, authorization, or redirects to unvalidated destinations.
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(host, &addresses)
        .timeout(Duration::from_secs(10))
        .user_agent("Temps-Activity-Setup/1.0")
        .build()
        .map_err(|e| error(project_id, e))?;
    let mut response = client
        .get(url.clone())
        .send()
        .await
        .map_err(|e| error(project_id, e))?;
    if !response.status().is_success() {
        return Err(error(
            project_id,
            format!(
                "Site returned HTTP {}. Use its final public URL or describe the app manually.",
                response.status()
            ),
        ));
    }
    let html = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("text/html"))
        });
    if !html {
        return Err(error(project_id, "The page is not HTML"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| error(project_id, e))? {
        if bytes.len() + chunk.len() > 2 * 1024 * 1024 {
            return Err(error(project_id, "Page exceeds the 2 MiB scan limit"));
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|e| error(project_id, e))
}

fn extract(project_id: i32, url: &Url, html: &str) -> Result<(SitePage, Vec<Url>), ActivityError> {
    let document = Html::parse_document(html);
    let selector = Selector::parse("title, h1, h2, h3, p, meta[name=description]")
        .map_err(|e| error(project_id, e))?;
    let mut text = String::new();
    for element in document.select(&selector) {
        if element
            .ancestors()
            .filter_map(scraper::ElementRef::wrap)
            .any(|p| {
                matches!(
                    p.value().name(),
                    "script" | "style" | "noscript" | "template" | "form"
                )
            })
        {
            continue;
        }
        let part = if element.value().name() == "meta" {
            element.value().attr("content").unwrap_or("").to_string()
        } else {
            element.text().collect::<Vec<_>>().join(" ")
        };
        text.push_str(&part.split_whitespace().collect::<Vec<_>>().join(" "));
        text.push('\n');
        if text.len() >= 12000 {
            break;
        }
    }
    if text.len() > 12000 {
        let mut end = 12000;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    let links = Selector::parse("a[href]").map_err(|e| error(project_id, e))?;
    let mut urls = Vec::new();
    for element in document.select(&links) {
        let Some(href) = element.value().attr("href") else {
            continue;
        };
        let Ok(target) = url.join(href) else {
            continue;
        };
        if target.origin() != url.origin()
            || target == *url
            || public_url(project_id, target.as_str()).is_err()
        {
            continue;
        }
        if !target.path().split('/').any(|part| {
            matches!(
                part,
                "pricing" | "docs" | "documentation" | "blog" | "features" | "about" | "guides"
            )
        }) {
            continue;
        }
        if !urls.contains(&target) {
            urls.push(target);
        }
        if urls.len() == 3 {
            break;
        }
    }
    Ok((
        SitePage {
            url: url.to_string(),
            text,
        },
        urls,
    ))
}

pub(super) async fn read_site(project_id: i32, url: &Url) -> Result<Vec<SitePage>, ActivityError> {
    let html = fetch(project_id, url).await?;
    let (page, links) = extract(project_id, url, &html)?;
    let mut pages = vec![page];
    for link in links {
        if let Ok(html) = fetch(project_id, &link).await {
            let (page, _) = extract(project_id, &link, &html)?;
            if !page.text.trim().is_empty() {
                pages.push(page);
            }
        }
    }
    if pages.iter().all(|p| p.text.trim().is_empty()) {
        return Err(error(
            project_id,
            "No readable public content found. Describe the application manually.",
        ));
    }
    Ok(pages)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blocks_private_urls_credentials_and_query_data() {
        for url in [
            "http://localhost",
            "http://127.0.0.1",
            "http://169.254.169.254",
            "https://user:pass@example.com",
            "https://example.com/?token=x",
            "file:///etc/passwd",
        ] {
            assert!(public_url(1, url).is_err(), "{url}");
        }
    }
    #[test]
    fn extracts_bounded_public_copy_and_same_origin_links() {
        let url = Url::parse("https://example.com/").unwrap();
        let html = format!("<title>Hosting</title><script>secret</script><form><p>private</p></form><h1>Ship apps</h1><a href='/pricing'>Pricing</a><a href='https://other.test/docs'>Other</a><a href='/docs?token=x'>Bad</a><p>{}</p>", "a".repeat(20000));
        let (page, links) = extract(1, &url, &html).unwrap();
        assert!(page.text.contains("Ship apps"));
        assert!(!page.text.contains("secret"));
        assert!(!page.text.contains("private"));
        assert!(page.text.chars().count() <= 12000);
        assert_eq!(links, vec![url.join("pricing").unwrap()]);
    }
}
