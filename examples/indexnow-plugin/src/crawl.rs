// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Site crawling and page discovery for IndexNow.
//!
//! Discovers pages by:
//! 1. Fetching and parsing the sitemap.xml
//! 2. Following internal links from the homepage (if no sitemap)
//! 3. Checking Last-Modified headers and computing content hashes

use reqwest::Client;
use scraper::{Html, Selector};
use std::collections::HashSet;
use url::Url;

use crate::types::CrawledPage;

/// Discover pages from a site, starting with sitemap, then link-following.
pub async fn discover_pages(
    site_url: &str,
    max_pages: usize,
    user_agent: &str,
    request_timeout_secs: u64,
) -> Result<Vec<CrawledPage>, CrawlError> {
    let client = Client::builder()
        .user_agent(user_agent)
        .timeout(std::time::Duration::from_secs(request_timeout_secs))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|e| CrawlError::HttpClient(e.to_string()))?;

    let base_url = Url::parse(site_url).map_err(|e| CrawlError::InvalidUrl {
        url: site_url.to_string(),
        reason: e.to_string(),
    })?;

    // Try sitemap first
    let mut urls = try_sitemap(&client, &base_url).await;

    // If sitemap didn't yield results, crawl by following links
    if urls.is_empty() {
        urls = crawl_links(&client, &base_url, max_pages).await;
    } else {
        // Limit sitemap URLs to max_pages
        urls.truncate(max_pages);
    }

    // Fetch metadata for each discovered URL
    let mut pages = Vec::with_capacity(urls.len());
    for page_url in &urls {
        match fetch_page_metadata(&client, page_url).await {
            Ok(page) => pages.push(page),
            Err(e) => {
                tracing::debug!(url = %page_url, error = %e, "Failed to fetch page metadata");
            }
        }
    }

    Ok(pages)
}

/// Try to parse sitemap.xml and extract URLs.
async fn try_sitemap(client: &Client, base_url: &Url) -> Vec<String> {
    let sitemap_url = format!(
        "{}sitemap.xml",
        base_url.as_str().trim_end_matches('/').to_string() + "/"
    );

    let response = match client.get(&sitemap_url).send().await {
        Ok(r) if r.status().is_success() => r,
        _ => return Vec::new(),
    };

    let body = match response.text().await {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };

    parse_sitemap_urls(&body, base_url)
}

/// Parse URLs from a sitemap XML body.
fn parse_sitemap_urls(xml: &str, base_url: &Url) -> Vec<String> {
    let mut urls = Vec::new();

    // Simple XML parsing — look for <loc> tags
    for line in xml.lines() {
        let trimmed = line.trim();
        if let Some(start) = trimmed.find("<loc>") {
            if let Some(end) = trimmed.find("</loc>") {
                let url_str = &trimmed[start + 5..end];
                let url_str = url_str.trim();
                // Only include URLs from the same host
                if let Ok(parsed) = Url::parse(url_str) {
                    if parsed.host_str() == base_url.host_str() {
                        urls.push(parsed.to_string());
                    }
                }
            }
        }
    }

    urls
}

/// Discover pages by following internal links from the homepage.
async fn crawl_links(client: &Client, base_url: &Url, max_pages: usize) -> Vec<String> {
    let mut visited: HashSet<String> = HashSet::new();
    let mut to_visit: Vec<String> = vec![base_url.to_string()];
    let mut discovered: Vec<String> = Vec::new();

    while let Some(current_url) = to_visit.pop() {
        if visited.len() >= max_pages {
            break;
        }
        if visited.contains(&current_url) {
            continue;
        }
        visited.insert(current_url.clone());
        discovered.push(current_url.clone());

        // Fetch and extract links
        let links = match extract_links(client, &current_url, base_url).await {
            Ok(l) => l,
            Err(_) => continue,
        };

        for link in links {
            if !visited.contains(&link) && discovered.len() + to_visit.len() < max_pages {
                to_visit.push(link);
            }
        }

        // Small delay to be polite
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    discovered
}

/// Extract internal links from an HTML page.
async fn extract_links(
    client: &Client,
    page_url: &str,
    base_url: &Url,
) -> Result<Vec<String>, CrawlError> {
    let response = client
        .get(page_url)
        .send()
        .await
        .map_err(|e| CrawlError::Fetch {
            url: page_url.to_string(),
            reason: e.to_string(),
        })?;

    if !response.status().is_success() {
        return Ok(Vec::new());
    }

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if !content_type.contains("text/html") {
        return Ok(Vec::new());
    }

    let body = response.text().await.map_err(|e| CrawlError::Fetch {
        url: page_url.to_string(),
        reason: e.to_string(),
    })?;

    let document = Html::parse_document(&body);
    let selector = Selector::parse("a[href]").unwrap();

    let current_url = Url::parse(page_url).unwrap_or_else(|_| base_url.clone());
    let mut links = Vec::new();

    for element in document.select(&selector) {
        if let Some(href) = element.value().attr("href") {
            // Resolve relative URLs
            if let Ok(absolute) = current_url.join(href) {
                // Only follow same-host links, skip fragments/anchors
                if absolute.host_str() == base_url.host_str()
                    && absolute.scheme() == base_url.scheme()
                    && absolute.fragment().is_none()
                {
                    // Normalize: strip trailing slash for dedup, then re-add
                    let mut normalized = absolute.clone();
                    normalized.set_fragment(None);
                    normalized.set_query(None);
                    links.push(normalized.to_string());
                }
            }
        }
    }

    Ok(links)
}

/// Fetch metadata for a single page (Last-Modified, ETag, content hash).
pub async fn fetch_page_metadata(
    client: &Client,
    page_url: &str,
) -> Result<CrawledPage, CrawlError> {
    let response = client
        .get(page_url)
        .send()
        .await
        .map_err(|e| CrawlError::Fetch {
            url: page_url.to_string(),
            reason: e.to_string(),
        })?;

    let status_code = response.status().as_u16();
    let last_modified = response
        .headers()
        .get("last-modified")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let etag = response
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let body = response.text().await.map_err(|e| CrawlError::Fetch {
        url: page_url.to_string(),
        reason: e.to_string(),
    })?;

    // Compute a simple content hash (first 8 bytes of sha256 as hex)
    let content_hash = simple_hash(&body);

    // Also try to extract last-modified from HTML meta tags
    let html_last_modified = if last_modified.is_none() {
        extract_html_last_modified(&body)
    } else {
        None
    };

    // Extract internal links
    let links = if status_code == 200 {
        extract_links_from_html(&body, page_url)
    } else {
        Vec::new()
    };

    Ok(CrawledPage {
        url: page_url.to_string(),
        status_code,
        last_modified: last_modified.or(html_last_modified),
        etag,
        content_hash,
        links,
    })
}

/// Extract last-modified from HTML meta tags (article:modified_time, etc.)
fn extract_html_last_modified(html: &str) -> Option<String> {
    let document = Html::parse_document(html);

    // Try <meta property="article:modified_time" content="...">
    let selector = Selector::parse(r#"meta[property="article:modified_time"]"#).ok()?;
    if let Some(element) = document.select(&selector).next() {
        if let Some(content) = element.value().attr("content") {
            return Some(content.to_string());
        }
    }

    // Try <meta name="last-modified" content="...">
    let selector = Selector::parse(r#"meta[name="last-modified"]"#).ok()?;
    if let Some(element) = document.select(&selector).next() {
        if let Some(content) = element.value().attr("content") {
            return Some(content.to_string());
        }
    }

    // Try <time datetime="..." class="updated"> or similar
    let selector = Selector::parse(r#"time[datetime]"#).ok()?;
    for element in document.select(&selector) {
        let classes = element.value().attr("class").unwrap_or("");
        if classes.contains("updated") || classes.contains("modified") {
            if let Some(dt) = element.value().attr("datetime") {
                return Some(dt.to_string());
            }
        }
    }

    None
}

/// Extract internal links from HTML body (without making HTTP requests).
fn extract_links_from_html(html: &str, page_url: &str) -> Vec<String> {
    let Ok(base) = Url::parse(page_url) else {
        return Vec::new();
    };

    let document = Html::parse_document(html);
    let Ok(selector) = Selector::parse("a[href]") else {
        return Vec::new();
    };

    let mut links = Vec::new();
    for element in document.select(&selector) {
        if let Some(href) = element.value().attr("href") {
            if let Ok(absolute) = base.join(href) {
                if absolute.host_str() == base.host_str() {
                    links.push(absolute.to_string());
                }
            }
        }
    }

    links
}

/// Compute a simple hash of the content (portable, no crypto dep needed).
/// Uses a FNV-like rolling hash and returns a hex string.
fn simple_hash(content: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325; // FNV offset basis
    for byte in content.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3); // FNV prime
    }
    format!("{:016x}", hash)
}

// ============================================================================
// Error
// ============================================================================

#[derive(Debug, thiserror::Error)]
pub enum CrawlError {
    #[error("Invalid URL '{url}': {reason}")]
    InvalidUrl { url: String, reason: String },

    #[error("Failed to create HTTP client: {0}")]
    HttpClient(String),

    #[error("Failed to fetch {url}: {reason}")]
    Fetch { url: String, reason: String },
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sitemap_urls() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <url><loc>https://example.com/</loc></url>
  <url><loc>https://example.com/about</loc></url>
  <url><loc>https://other.com/not-ours</loc></url>
</urlset>"#;

        let base = Url::parse("https://example.com").unwrap();
        let urls = parse_sitemap_urls(xml, &base);

        assert_eq!(urls.len(), 2);
        assert!(urls.contains(&"https://example.com/".to_string()));
        assert!(urls.contains(&"https://example.com/about".to_string()));
    }

    #[test]
    fn test_simple_hash_deterministic() {
        let h1 = simple_hash("hello world");
        let h2 = simple_hash("hello world");
        assert_eq!(h1, h2);

        let h3 = simple_hash("hello world!");
        assert_ne!(h1, h3);
    }

    #[test]
    fn test_extract_html_last_modified() {
        let html = r#"<html><head>
            <meta property="article:modified_time" content="2025-06-15T10:30:00Z">
        </head><body></body></html>"#;

        let result = extract_html_last_modified(html);
        assert_eq!(result, Some("2025-06-15T10:30:00Z".to_string()));
    }

    #[test]
    fn test_extract_html_last_modified_none() {
        let html = r#"<html><head><title>No meta</title></head><body></body></html>"#;
        assert!(extract_html_last_modified(html).is_none());
    }

    #[test]
    fn test_extract_links_from_html() {
        let html = r#"<html><body>
            <a href="/about">About</a>
            <a href="https://example.com/contact">Contact</a>
            <a href="https://external.com/page">External</a>
        </body></html>"#;

        let links = extract_links_from_html(html, "https://example.com/");
        assert_eq!(links.len(), 2);
        assert!(links.iter().any(|l| l.contains("/about")));
        assert!(links.iter().any(|l| l.contains("/contact")));
    }
}
