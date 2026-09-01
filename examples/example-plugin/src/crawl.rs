// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Site crawling engine and SEO analysis.
//!
//! Discovery strategy:
//! 1. Fetch `robots.txt` to find `Sitemap:` directives
//! 2. Fall back to `/sitemap.xml` if none found
//! 3. Parse sitemaps (supports `<sitemapindex>` with sub-sitemaps)
//! 4. Seed the crawl queue with sitemap URLs
//! 5. Supplement with internal link discovery during crawling
//!
//! The main entry point is [`run_analysis`], which discovers pages,
//! analyzes them for technical SEO issues, and persists results.

use scraper::{Html as HtmlDoc, Selector};
use std::collections::HashSet;
use url::Url;

use crate::db::SeoStore;
use crate::types::*;

// ============================================================================
// Crawl orchestrator
// ============================================================================

/// Configuration for a single crawl run, derived from plugin settings
/// and per-analysis overrides.
pub struct CrawlConfig {
    pub max_pages: usize,
    pub crawl_delay: std::time::Duration,
}

/// Crawl the target site and persist results.
///
/// Discovery order:
/// 1. Parse robots.txt for Sitemap directives
/// 2. Fall back to /sitemap.xml
/// 3. Parse sitemap(s) to seed the crawl queue
/// 4. Always include the start URL
/// 5. Supplement with link-following during crawl
pub async fn run_analysis(
    store: &SeoStore,
    http_client: &reqwest::Client,
    report_id: &str,
    start_url: &str,
    config: CrawlConfig,
) {
    let start = std::time::Instant::now();

    let base = match Url::parse(start_url) {
        Ok(u) => u,
        Err(_) => {
            if let Err(e) = store.mark_failed(report_id).await {
                tracing::error!(report_id, error = %e, "Failed to mark report as failed");
            }
            return;
        }
    };

    let base_host = base.host_str().unwrap_or("").to_string();
    let base_origin = format!("{}://{}", base.scheme(), base.host_str().unwrap_or(""));

    // Phase 1: Discover URLs from sitemap
    let sitemap_urls = discover_sitemap_urls(http_client, &base_origin, &base_host).await;
    let sitemap_count = sitemap_urls.len();
    if sitemap_count > 0 {
        tracing::info!(
            report_id,
            sitemap_urls = sitemap_count,
            "Discovered URLs from sitemap"
        );
    } else {
        tracing::info!(report_id, "No sitemap found, will rely on link crawling");
    }

    // Phase 2: Build initial queue — sitemap URLs first, then start URL
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: Vec<String> = Vec::new();

    // Add start URL first (it goes to the back, processed last since we pop())
    queue.push(start_url.to_string());

    // Add sitemap URLs — these are the authoritative pages the site wants indexed
    for url in sitemap_urls {
        let norm = normalize_url(&url);
        if !visited.contains(&norm) {
            queue.push(url);
        }
    }

    let mut pages: Vec<PageAnalysis> = Vec::new();

    // Phase 3: Crawl
    while let Some(url) = queue.pop() {
        if visited.len() >= config.max_pages {
            break;
        }
        let normalized = normalize_url(&url);
        if visited.contains(&normalized) {
            continue;
        }
        visited.insert(normalized.clone());

        // Respect crawl delay between requests
        if !pages.is_empty() && !config.crawl_delay.is_zero() {
            tokio::time::sleep(config.crawl_delay).await;
        }

        let page = analyze_page(http_client, &url, &base_host).await;

        // Discover internal links for further crawling
        if page.is_some() {
            if let Ok(resp) = http_client.get(&url).send().await {
                if let Ok(body) = resp.text().await {
                    let discovered = extract_internal_links(&body, &base_host, &url);
                    for link in discovered {
                        let norm = normalize_url(&link);
                        if !visited.contains(&norm) {
                            queue.push(link);
                        }
                    }
                }
            }
        }

        if let Some(p) = page {
            pages.push(p);
        }
    }

    let elapsed = start.elapsed().as_millis() as u64;

    match store.complete_report(report_id, &pages, elapsed).await {
        Ok(()) => {
            let overall_score = if pages.is_empty() {
                0
            } else {
                (pages.iter().map(|p| p.score as u64).sum::<u64>() / pages.len() as u64) as u32
            };
            tracing::info!(
                report_id,
                pages = visited.len(),
                sitemap_urls = sitemap_count,
                score = overall_score,
                duration_ms = elapsed,
                "SEO analysis completed"
            );
        }
        Err(e) => {
            tracing::error!(report_id, error = %e, "Failed to persist analysis results");
            if let Err(e2) = store.mark_failed(report_id).await {
                tracing::error!(report_id, error = %e2, "Failed to mark report as failed after persistence error");
            }
        }
    }
}

// ============================================================================
// Sitemap Discovery
// ============================================================================

/// Discover page URLs from the site's sitemap.
///
/// 1. Fetch robots.txt and look for `Sitemap:` directives
/// 2. If none found, try /sitemap.xml directly
/// 3. Parse each sitemap (handle both urlset and sitemapindex)
async fn discover_sitemap_urls(
    client: &reqwest::Client,
    base_origin: &str,
    base_host: &str,
) -> Vec<String> {
    // Step 1: Find sitemap URLs from robots.txt
    let mut sitemap_locations = find_sitemaps_from_robots(client, base_origin).await;

    // Step 2: Fall back to /sitemap.xml if robots.txt had no Sitemap directives
    if sitemap_locations.is_empty() {
        sitemap_locations.push(format!("{}/sitemap.xml", base_origin));
    }

    // Step 3: Fetch and parse all sitemaps (with recursion for sitemap indexes)
    let mut all_urls: Vec<String> = Vec::new();
    let mut seen_sitemaps: HashSet<String> = HashSet::new();

    for loc in sitemap_locations {
        collect_sitemap_urls(
            client,
            &loc,
            base_host,
            &mut all_urls,
            &mut seen_sitemaps,
            0,
        )
        .await;
    }

    all_urls
}

/// Parse robots.txt and extract `Sitemap:` directives.
async fn find_sitemaps_from_robots(client: &reqwest::Client, base_origin: &str) -> Vec<String> {
    let robots_url = format!("{}/robots.txt", base_origin);
    let mut sitemaps = Vec::new();

    let body = match client.get(&robots_url).send().await {
        Ok(resp) if resp.status().is_success() => match resp.text().await {
            Ok(text) => text,
            Err(_) => return sitemaps,
        },
        _ => return sitemaps,
    };

    for line in body.lines() {
        let trimmed = line.trim();
        // Case-insensitive match for "Sitemap:" prefix
        if trimmed.len() > 8 && trimmed[..8].eq_ignore_ascii_case("sitemap:") {
            let url = trimmed[8..].trim();
            if !url.is_empty() {
                sitemaps.push(url.to_string());
            }
        }
    }

    if !sitemaps.is_empty() {
        tracing::debug!(
            count = sitemaps.len(),
            "Found sitemap directives in robots.txt"
        );
    }

    sitemaps
}

/// Maximum depth for following sitemap index → child sitemap references.
const MAX_SITEMAP_DEPTH: u8 = 3;

/// Recursively fetch a sitemap and collect page URLs.
///
/// Handles two XML formats:
/// - `<sitemapindex>` containing `<sitemap><loc>...</loc></sitemap>` — recurse into each
/// - `<urlset>` containing `<url><loc>...</loc></url>` — collect the URLs
fn collect_sitemap_urls<'a>(
    client: &'a reqwest::Client,
    sitemap_url: &'a str,
    base_host: &'a str,
    out: &'a mut Vec<String>,
    seen: &'a mut HashSet<String>,
    depth: u8,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        if depth > MAX_SITEMAP_DEPTH {
            tracing::warn!(sitemap_url, "Sitemap recursion depth exceeded, skipping");
            return;
        }

        if seen.contains(sitemap_url) {
            return;
        }
        seen.insert(sitemap_url.to_string());

        let body = match client.get(sitemap_url).send().await {
            Ok(resp) if resp.status().is_success() => match resp.text().await {
                Ok(text) => text,
                Err(e) => {
                    tracing::debug!(sitemap_url, error = %e, "Failed to read sitemap body");
                    return;
                }
            },
            Ok(resp) => {
                tracing::debug!(
                    sitemap_url,
                    status = resp.status().as_u16(),
                    "Sitemap returned non-200"
                );
                return;
            }
            Err(e) => {
                tracing::debug!(sitemap_url, error = %e, "Failed to fetch sitemap");
                return;
            }
        };

        // Detect if this is a sitemap index or a regular urlset
        let is_index = body.contains("<sitemapindex") || body.contains("<sitemapindex>");

        if is_index {
            // Parse as sitemap index — extract child sitemap locations
            let child_sitemaps = extract_xml_locs(&body, "sitemap");
            tracing::debug!(
                sitemap_url,
                children = child_sitemaps.len(),
                "Parsing sitemap index"
            );
            for child in child_sitemaps {
                collect_sitemap_urls(client, &child, base_host, out, seen, depth + 1).await;
            }
        } else {
            // Parse as urlset — extract page URLs
            let urls = extract_xml_locs(&body, "url");
            let before = out.len();
            for url in urls {
                // Only include URLs from the same host
                if let Ok(parsed) = Url::parse(&url) {
                    if parsed.host_str() == Some(base_host) {
                        out.push(url);
                    }
                }
            }
            tracing::debug!(
                sitemap_url,
                urls = out.len() - before,
                "Parsed sitemap urlset"
            );
        }
    }) // Box::pin(async move { ... })
}

/// Extract `<loc>` values from XML elements.
///
/// For a sitemap index, `parent_tag` is "sitemap".
/// For a urlset, `parent_tag` is "url".
///
/// Uses simple string parsing instead of a full XML parser to keep
/// dependencies minimal. Handles the common sitemap formats correctly.
fn extract_xml_locs(xml: &str, parent_tag: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let open_tag = format!("<{}", parent_tag);
    let close_tag = format!("</{}>", parent_tag);

    let mut search_from = 0;
    while let Some(start) = xml[search_from..].find(&open_tag) {
        let abs_start = search_from + start;
        let Some(end) = xml[abs_start..].find(&close_tag) else {
            break;
        };
        let block = &xml[abs_start..abs_start + end + close_tag.len()];

        // Find <loc>...</loc> within this block
        if let Some(loc_start) = block.find("<loc>") {
            let loc_content_start = loc_start + 5; // len("<loc>")
            if let Some(loc_end) = block[loc_content_start..].find("</loc>") {
                let loc = block[loc_content_start..loc_content_start + loc_end].trim();
                if !loc.is_empty() {
                    urls.push(loc.to_string());
                }
            }
        }

        search_from = abs_start + end + close_tag.len();
    }

    urls
}

// ============================================================================
// Page analysis
// ============================================================================

/// Analyze a single page.
async fn analyze_page(
    client: &reqwest::Client,
    url: &str,
    base_host: &str,
) -> Option<PageAnalysis> {
    let start = std::time::Instant::now();

    let resp = match client.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(url, error = %e, "Failed to fetch page");
            return None;
        }
    };

    let status_code = resp.status().as_u16();
    let body = resp.text().await.ok()?;
    let load_time = start.elapsed().as_millis() as u64;
    let doc = HtmlDoc::parse_document(&body);

    // Extract SEO signals
    let title = extract_text(&doc, "title");
    let meta_description = extract_meta(&doc, "description");
    let canonical = extract_link_rel(&doc, "canonical");
    let h1_count = count_elements(&doc, "h1");
    let h2_count = count_elements(&doc, "h2");
    let (image_count, images_without_alt) = count_images(&doc);
    let (internal_links, external_links) = count_links(&doc, base_host);
    let word_count = count_words(&doc);
    let has_og_title = extract_meta_property(&doc, "og:title").is_some();
    let has_og_description = extract_meta_property(&doc, "og:description").is_some();
    let has_og_image = extract_meta_property(&doc, "og:image").is_some();
    let has_robots_meta = extract_meta(&doc, "robots").is_some();
    let has_viewport = extract_meta(&doc, "viewport").is_some();
    let has_charset = has_charset_declaration(&doc);
    let has_lang = has_lang_attribute(&body);

    let mut issues = Vec::new();
    generate_issues(
        &mut issues,
        &title,
        &meta_description,
        &canonical,
        h1_count,
        image_count,
        images_without_alt,
        has_og_title,
        has_og_description,
        has_og_image,
        has_viewport,
        has_lang,
        word_count,
        load_time,
        status_code,
    );

    let score = calculate_page_score(&issues);

    Some(PageAnalysis {
        url: url.to_string(),
        status_code,
        score,
        title,
        meta_description,
        canonical,
        h1_count,
        h2_count,
        image_count,
        images_without_alt,
        word_count,
        internal_links,
        external_links,
        has_og_title,
        has_og_description,
        has_og_image,
        has_robots_meta,
        has_viewport,
        has_charset,
        has_lang,
        load_time_ms: load_time,
        issues,
    })
}

// ============================================================================
// Issue Generation
// ============================================================================

#[allow(clippy::too_many_arguments)]
fn generate_issues(
    issues: &mut Vec<SeoIssue>,
    title: &Option<String>,
    meta_description: &Option<String>,
    canonical: &Option<String>,
    h1_count: usize,
    image_count: usize,
    images_without_alt: usize,
    has_og_title: bool,
    has_og_description: bool,
    has_og_image: bool,
    has_viewport: bool,
    has_lang: bool,
    word_count: usize,
    load_time: u64,
    status_code: u16,
) {
    // Title checks
    match title {
        None => issues.push(SeoIssue {
            severity: IssueSeverity::Critical,
            code: "MISSING_TITLE".into(),
            message: "Page has no <title> tag".into(),
            recommendation: "Add a unique, descriptive title tag between 30-60 characters.".into(),
        }),
        Some(t) if t.is_empty() => issues.push(SeoIssue {
            severity: IssueSeverity::Critical,
            code: "EMPTY_TITLE".into(),
            message: "Page has an empty <title> tag".into(),
            recommendation: "Add descriptive text to the title tag.".into(),
        }),
        Some(t) if t.len() < 20 => issues.push(SeoIssue {
            severity: IssueSeverity::Warning,
            code: "SHORT_TITLE".into(),
            message: format!("Title is too short ({} chars): \"{}\"", t.len(), t),
            recommendation: "Aim for 30-60 characters to maximize SERP visibility.".into(),
        }),
        Some(t) if t.len() > 60 => issues.push(SeoIssue {
            severity: IssueSeverity::Warning,
            code: "LONG_TITLE".into(),
            message: format!(
                "Title is too long ({} chars) and may be truncated in SERPs",
                t.len()
            ),
            recommendation: "Keep titles under 60 characters.".into(),
        }),
        _ => {}
    }

    // Meta description checks
    match meta_description {
        None => issues.push(SeoIssue {
            severity: IssueSeverity::Critical,
            code: "MISSING_META_DESC".into(),
            message: "No meta description found".into(),
            recommendation:
                "Add a meta description (120-155 chars) that summarizes the page content.".into(),
        }),
        Some(d) if d.is_empty() => issues.push(SeoIssue {
            severity: IssueSeverity::Critical,
            code: "EMPTY_META_DESC".into(),
            message: "Meta description is empty".into(),
            recommendation: "Write a compelling description of 120-155 characters.".into(),
        }),
        Some(d) if d.len() < 70 => issues.push(SeoIssue {
            severity: IssueSeverity::Warning,
            code: "SHORT_META_DESC".into(),
            message: format!("Meta description is short ({} chars)", d.len()),
            recommendation: "Expand to 120-155 characters to improve click-through rates.".into(),
        }),
        Some(d) if d.len() > 160 => issues.push(SeoIssue {
            severity: IssueSeverity::Warning,
            code: "LONG_META_DESC".into(),
            message: format!("Meta description is too long ({} chars)", d.len()),
            recommendation: "Keep under 155 characters to avoid truncation in SERPs.".into(),
        }),
        _ => {}
    }

    // H1 checks
    if h1_count == 0 {
        issues.push(SeoIssue {
            severity: IssueSeverity::Critical,
            code: "MISSING_H1".into(),
            message: "Page has no H1 heading".into(),
            recommendation: "Add exactly one H1 that describes the page's main topic.".into(),
        });
    } else if h1_count > 1 {
        issues.push(SeoIssue {
            severity: IssueSeverity::Warning,
            code: "MULTIPLE_H1".into(),
            message: format!("Page has {} H1 tags (expected 1)", h1_count),
            recommendation: "Use a single H1 for the main heading. Use H2-H6 for subheadings."
                .into(),
        });
    }

    // Image alt text
    if images_without_alt > 0 {
        issues.push(SeoIssue {
            severity: if images_without_alt > 3 {
                IssueSeverity::Critical
            } else {
                IssueSeverity::Warning
            },
            code: "IMAGES_MISSING_ALT".into(),
            message: format!(
                "{} of {} images missing alt text",
                images_without_alt, image_count
            ),
            recommendation:
                "Add descriptive alt attributes to all images for accessibility and SEO.".into(),
        });
    }

    // Canonical URL
    if canonical.is_none() {
        issues.push(SeoIssue {
            severity: IssueSeverity::Warning,
            code: "MISSING_CANONICAL".into(),
            message: "No canonical URL defined".into(),
            recommendation: "Add <link rel=\"canonical\"> to prevent duplicate content issues."
                .into(),
        });
    }

    // Open Graph
    if !has_og_title || !has_og_description || !has_og_image {
        let mut missing = Vec::new();
        if !has_og_title {
            missing.push("og:title");
        }
        if !has_og_description {
            missing.push("og:description");
        }
        if !has_og_image {
            missing.push("og:image");
        }
        issues.push(SeoIssue {
            severity: IssueSeverity::Warning,
            code: "INCOMPLETE_OG".into(),
            message: format!("Missing Open Graph tags: {}", missing.join(", ")),
            recommendation: "Add all OG tags for better social media sharing previews.".into(),
        });
    }

    // Viewport
    if !has_viewport {
        issues.push(SeoIssue {
            severity: IssueSeverity::Critical,
            code: "MISSING_VIEWPORT".into(),
            message: "No viewport meta tag found".into(),
            recommendation: "Add <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"> for mobile responsiveness.".into(),
        });
    }

    // Lang
    if !has_lang {
        issues.push(SeoIssue {
            severity: IssueSeverity::Warning,
            code: "MISSING_LANG".into(),
            message: "HTML tag is missing the lang attribute".into(),
            recommendation:
                "Add lang attribute to <html> (e.g., <html lang=\"en\">) for accessibility.".into(),
        });
    }

    // Thin content
    if word_count < 100 && status_code == 200 {
        issues.push(SeoIssue {
            severity: IssueSeverity::Warning,
            code: "THIN_CONTENT".into(),
            message: format!("Page has very little text content ({} words)", word_count),
            recommendation: "Pages with fewer than 300 words typically rank poorly.".into(),
        });
    }

    // Response time
    if load_time > 3000 {
        issues.push(SeoIssue {
            severity: IssueSeverity::Critical,
            code: "SLOW_RESPONSE".into(),
            message: format!("Page took {}ms to respond (>3s)", load_time),
            recommendation: "Optimize server response time. Target under 200ms TTFB.".into(),
        });
    } else if load_time > 1000 {
        issues.push(SeoIssue {
            severity: IssueSeverity::Warning,
            code: "MODERATE_RESPONSE".into(),
            message: format!("Page response time is {}ms", load_time),
            recommendation: "Response time is acceptable but could be improved.".into(),
        });
    }

    // HTTP errors
    if status_code >= 400 {
        issues.push(SeoIssue {
            severity: IssueSeverity::Critical,
            code: "HTTP_ERROR".into(),
            message: format!("Page returned HTTP {}", status_code),
            recommendation: "Fix the server error or remove links to this page.".into(),
        });
    } else if status_code >= 300 {
        issues.push(SeoIssue {
            severity: IssueSeverity::Info,
            code: "REDIRECT".into(),
            message: format!("Page returns a redirect (HTTP {})", status_code),
            recommendation: "Update internal links to point directly to the final URL.".into(),
        });
    }
}

// ============================================================================
// HTML Parsing Helpers
// ============================================================================

fn extract_text(doc: &HtmlDoc, selector: &str) -> Option<String> {
    let sel = Selector::parse(selector).ok()?;
    doc.select(&sel)
        .next()
        .map(|el| el.text().collect::<String>().trim().to_string())
        .filter(|t| !t.is_empty())
}

fn extract_meta(doc: &HtmlDoc, name: &str) -> Option<String> {
    let sel = Selector::parse("meta[name]").ok()?;
    for el in doc.select(&sel) {
        if let Some(n) = el.value().attr("name") {
            if n.eq_ignore_ascii_case(name) {
                return el.value().attr("content").map(|s| s.to_string());
            }
        }
    }
    None
}

fn extract_meta_property(doc: &HtmlDoc, property: &str) -> Option<String> {
    let sel = Selector::parse("meta[property]").ok()?;
    for el in doc.select(&sel) {
        if let Some(p) = el.value().attr("property") {
            if p.eq_ignore_ascii_case(property) {
                return el.value().attr("content").map(|s| s.to_string());
            }
        }
    }
    None
}

fn extract_link_rel(doc: &HtmlDoc, rel: &str) -> Option<String> {
    let sel = Selector::parse(&format!("link[rel=\"{}\"]", rel)).ok()?;
    doc.select(&sel)
        .next()
        .and_then(|el| el.value().attr("href").map(|s| s.to_string()))
}

fn count_elements(doc: &HtmlDoc, selector: &str) -> usize {
    Selector::parse(selector)
        .map(|sel| doc.select(&sel).count())
        .unwrap_or(0)
}

fn count_images(doc: &HtmlDoc) -> (usize, usize) {
    let sel = match Selector::parse("img") {
        Ok(s) => s,
        Err(_) => return (0, 0),
    };
    let mut total = 0;
    let mut missing_alt = 0;
    for el in doc.select(&sel) {
        total += 1;
        match el.value().attr("alt") {
            None | Some("") => missing_alt += 1,
            _ => {}
        }
    }
    (total, missing_alt)
}

fn count_links(doc: &HtmlDoc, base_host: &str) -> (usize, usize) {
    let sel = match Selector::parse("a[href]") {
        Ok(s) => s,
        Err(_) => return (0, 0),
    };
    let mut internal = 0;
    let mut external = 0;
    for el in doc.select(&sel) {
        if let Some(href) = el.value().attr("href") {
            if href.starts_with('#')
                || href.starts_with("javascript:")
                || href.starts_with("mailto:")
            {
                continue;
            }
            if let Ok(parsed) = Url::parse(href) {
                if parsed.host_str() == Some(base_host) {
                    internal += 1;
                } else {
                    external += 1;
                }
            } else {
                internal += 1; // Relative URL
            }
        }
    }
    (internal, external)
}

fn count_words(doc: &HtmlDoc) -> usize {
    let sel = match Selector::parse("body") {
        Ok(s) => s,
        Err(_) => return 0,
    };
    doc.select(&sel)
        .next()
        .map(|body| body.text().collect::<String>().split_whitespace().count())
        .unwrap_or(0)
}

pub fn has_charset_declaration(doc: &HtmlDoc) -> bool {
    Selector::parse("meta[charset]")
        .map(|sel| doc.select(&sel).next().is_some())
        .unwrap_or(false)
        || Selector::parse("meta[http-equiv=\"Content-Type\"]")
            .map(|sel| doc.select(&sel).next().is_some())
            .unwrap_or(false)
}

pub fn has_lang_attribute(html: &str) -> bool {
    let lower = html.to_lowercase();
    if let Some(pos) = lower.find("<html") {
        let tag_end = lower[pos..].find('>').unwrap_or(lower.len() - pos);
        let tag = &lower[pos..pos + tag_end];
        tag.contains("lang=") || tag.contains("lang =")
    } else {
        false
    }
}

pub fn extract_internal_links(html: &str, base_host: &str, current_url: &str) -> Vec<String> {
    let doc = HtmlDoc::parse_document(html);
    let sel = match Selector::parse("a[href]") {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    let base = Url::parse(current_url).ok();
    let mut links = Vec::new();

    for el in doc.select(&sel) {
        if let Some(href) = el.value().attr("href") {
            if href.starts_with('#')
                || href.starts_with("javascript:")
                || href.starts_with("mailto:")
            {
                continue;
            }
            let resolved = if let Ok(abs) = Url::parse(href) {
                abs
            } else if let Some(ref b) = base {
                match b.join(href) {
                    Ok(u) => u,
                    Err(_) => continue,
                }
            } else {
                continue;
            };

            if resolved.host_str() == Some(base_host)
                && (resolved.scheme() == "http" || resolved.scheme() == "https")
            {
                let mut clean = resolved.clone();
                clean.set_fragment(None);
                links.push(clean.to_string());
            }
        }
    }

    links
}

pub fn normalize_url(url: &str) -> String {
    if let Ok(mut parsed) = Url::parse(url) {
        parsed.set_fragment(None);
        let s = parsed.to_string();
        s.trim_end_matches('/').to_string()
    } else {
        url.to_string()
    }
}

// ============================================================================
// Scoring
// ============================================================================

pub fn calculate_page_score(issues: &[SeoIssue]) -> u32 {
    let mut score: i32 = 100;
    for issue in issues {
        match issue.severity {
            IssueSeverity::Critical => score -= 15,
            IssueSeverity::Warning => score -= 5,
            IssueSeverity::Info => score -= 1,
        }
    }
    score.max(0) as u32
}

pub fn compute_summary(pages: &[PageAnalysis]) -> ReportSummaryStats {
    let mut stats = ReportSummaryStats {
        pages_crawled: pages.len(),
        total_issues: 0,
        critical: 0,
        warnings: 0,
        info: 0,
        avg_page_score: 0,
        missing_titles: 0,
        missing_descriptions: 0,
        missing_h1: 0,
        images_without_alt: 0,
        missing_canonical: 0,
        missing_og_tags: 0,
    };

    for page in pages {
        for issue in &page.issues {
            stats.total_issues += 1;
            match issue.severity {
                IssueSeverity::Critical => stats.critical += 1,
                IssueSeverity::Warning => stats.warnings += 1,
                IssueSeverity::Info => stats.info += 1,
            }
        }
        if page.title.is_none() {
            stats.missing_titles += 1;
        }
        if page.meta_description.is_none() {
            stats.missing_descriptions += 1;
        }
        if page.h1_count == 0 {
            stats.missing_h1 += 1;
        }
        stats.images_without_alt += page.images_without_alt;
        if page.canonical.is_none() {
            stats.missing_canonical += 1;
        }
        if !page.has_og_title || !page.has_og_description || !page.has_og_image {
            stats.missing_og_tags += 1;
        }
    }

    if !pages.is_empty() {
        stats.avg_page_score =
            (pages.iter().map(|p| p.score as u64).sum::<u64>() / pages.len() as u64) as u32;
    }

    stats
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_page_score_perfect() {
        let issues: Vec<SeoIssue> = vec![];
        assert_eq!(calculate_page_score(&issues), 100);
    }

    #[test]
    fn test_calculate_page_score_with_issues() {
        let issues = vec![
            SeoIssue {
                severity: IssueSeverity::Critical,
                code: "TEST".into(),
                message: "test".into(),
                recommendation: "fix".into(),
            },
            SeoIssue {
                severity: IssueSeverity::Warning,
                code: "TEST2".into(),
                message: "test".into(),
                recommendation: "fix".into(),
            },
        ];
        // 100 - 15 (critical) - 5 (warning) = 80
        assert_eq!(calculate_page_score(&issues), 80);
    }

    #[test]
    fn test_calculate_page_score_floor_at_zero() {
        let issues: Vec<SeoIssue> = (0..10)
            .map(|i| SeoIssue {
                severity: IssueSeverity::Critical,
                code: format!("C{}", i),
                message: "test".into(),
                recommendation: "fix".into(),
            })
            .collect();
        assert_eq!(calculate_page_score(&issues), 0);
    }

    #[test]
    fn test_normalize_url_strips_fragment() {
        assert_eq!(
            normalize_url("https://example.com/page#section"),
            "https://example.com/page"
        );
    }

    #[test]
    fn test_normalize_url_strips_trailing_slash() {
        assert_eq!(
            normalize_url("https://example.com/page/"),
            "https://example.com/page"
        );
    }

    #[test]
    fn test_has_lang_attribute_present() {
        assert!(has_lang_attribute(
            r#"<html lang="en"><head></head></html>"#
        ));
    }

    #[test]
    fn test_has_lang_attribute_missing() {
        assert!(!has_lang_attribute(r#"<html><head></head></html>"#));
    }

    #[test]
    fn test_has_charset_declaration() {
        let doc = HtmlDoc::parse_document(r#"<html><head><meta charset="utf-8"></head></html>"#);
        assert!(has_charset_declaration(&doc));
    }

    #[test]
    fn test_extract_text_title() {
        let doc = HtmlDoc::parse_document("<html><head><title>Hello World</title></head></html>");
        assert_eq!(extract_text(&doc, "title"), Some("Hello World".into()));
    }

    #[test]
    fn test_extract_meta_description() {
        let doc = HtmlDoc::parse_document(
            r#"<html><head><meta name="description" content="A great page"></head></html>"#,
        );
        assert_eq!(
            extract_meta(&doc, "description"),
            Some("A great page".into())
        );
    }

    #[test]
    fn test_extract_meta_missing() {
        let doc = HtmlDoc::parse_document("<html><head></head></html>");
        assert_eq!(extract_meta(&doc, "description"), None);
    }

    #[test]
    fn test_count_images_with_alt() {
        let doc = HtmlDoc::parse_document(
            r#"<img src="a.jpg" alt="A"><img src="b.jpg"><img src="c.jpg" alt="">"#,
        );
        let (total, missing) = count_images(&doc);
        assert_eq!(total, 3);
        assert_eq!(missing, 2); // b.jpg has no alt, c.jpg has empty alt
    }

    #[test]
    fn test_extract_internal_links() {
        let html = r##"
            <a href="/about">About</a>
            <a href="https://example.com/pricing">Pricing</a>
            <a href="https://other.com/ext">External</a>
            <a href="#top">Anchor</a>
        "##;
        let links = extract_internal_links(html, "example.com", "https://example.com/");
        assert_eq!(links.len(), 2);
        assert!(links.iter().any(|l| l.contains("/about")));
        assert!(links.iter().any(|l| l.contains("/pricing")));
    }

    #[test]
    fn test_compute_summary() {
        let pages = vec![PageAnalysis {
            url: "https://example.com/".into(),
            status_code: 200,
            score: 80,
            title: Some("Home".into()),
            meta_description: None,
            canonical: None,
            h1_count: 1,
            h2_count: 2,
            image_count: 3,
            images_without_alt: 1,
            word_count: 500,
            internal_links: 10,
            external_links: 2,
            has_og_title: true,
            has_og_description: true,
            has_og_image: false,
            has_robots_meta: true,
            has_viewport: true,
            has_charset: true,
            has_lang: true,
            load_time_ms: 200,
            issues: vec![
                SeoIssue {
                    severity: IssueSeverity::Critical,
                    code: "MISSING_META_DESC".into(),
                    message: "test".into(),
                    recommendation: "fix".into(),
                },
                SeoIssue {
                    severity: IssueSeverity::Warning,
                    code: "MISSING_CANONICAL".into(),
                    message: "test".into(),
                    recommendation: "fix".into(),
                },
            ],
        }];

        let summary = compute_summary(&pages);
        assert_eq!(summary.pages_crawled, 1);
        assert_eq!(summary.total_issues, 2);
        assert_eq!(summary.critical, 1);
        assert_eq!(summary.warnings, 1);
        assert_eq!(summary.missing_descriptions, 1);
        assert_eq!(summary.missing_canonical, 1);
        assert_eq!(summary.images_without_alt, 1);
        assert_eq!(summary.missing_og_tags, 1);
        assert_eq!(summary.avg_page_score, 80);
    }

    #[test]
    fn test_generate_issues_perfect_page() {
        let mut issues = Vec::new();
        generate_issues(
            &mut issues,
            &Some("A Great Page Title That Is Proper Length".into()),
            &Some("This is a well-crafted meta description that provides a clear summary of what this page is about and encourages clicks from search results.".into()),
            &Some("https://example.com/page".into()),
            1, 5, 0, true, true, true, true, true, 500, 150, 200,
        );
        // Should have no issues for a perfect page
        assert!(issues.is_empty(), "Expected no issues, got: {:?}", issues);
    }

    #[test]
    fn test_generate_issues_missing_everything() {
        let mut issues = Vec::new();
        generate_issues(
            &mut issues,
            &None,
            &None,
            &None,
            0,
            5,
            5,
            false,
            false,
            false,
            false,
            false,
            50,
            4000,
            200,
        );
        let codes: Vec<&str> = issues.iter().map(|i| i.code.as_str()).collect();
        assert!(codes.contains(&"MISSING_TITLE"));
        assert!(codes.contains(&"MISSING_META_DESC"));
        assert!(codes.contains(&"MISSING_H1"));
        assert!(codes.contains(&"IMAGES_MISSING_ALT"));
        assert!(codes.contains(&"MISSING_VIEWPORT"));
        assert!(codes.contains(&"SLOW_RESPONSE"));
    }

    // ========================================================================
    // Sitemap XML parsing tests
    // ========================================================================

    #[test]
    fn test_extract_xml_locs_urlset() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <url>
    <loc>https://example.com/</loc>
    <lastmod>2025-01-01</lastmod>
  </url>
  <url>
    <loc>https://example.com/about</loc>
  </url>
  <url>
    <loc>https://example.com/pricing</loc>
    <changefreq>monthly</changefreq>
  </url>
</urlset>"#;
        let urls = extract_xml_locs(xml, "url");
        assert_eq!(urls.len(), 3);
        assert_eq!(urls[0], "https://example.com/");
        assert_eq!(urls[1], "https://example.com/about");
        assert_eq!(urls[2], "https://example.com/pricing");
    }

    #[test]
    fn test_extract_xml_locs_sitemapindex() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <sitemap>
    <loc>https://example.com/sitemap-pages.xml</loc>
    <lastmod>2025-06-01</lastmod>
  </sitemap>
  <sitemap>
    <loc>https://example.com/sitemap-blog.xml</loc>
  </sitemap>
</sitemapindex>"#;
        let sitemaps = extract_xml_locs(xml, "sitemap");
        assert_eq!(sitemaps.len(), 2);
        assert_eq!(sitemaps[0], "https://example.com/sitemap-pages.xml");
        assert_eq!(sitemaps[1], "https://example.com/sitemap-blog.xml");
    }

    #[test]
    fn test_extract_xml_locs_empty_urlset() {
        let xml = r#"<?xml version="1.0"?><urlset></urlset>"#;
        let urls = extract_xml_locs(xml, "url");
        assert!(urls.is_empty());
    }

    #[test]
    fn test_extract_xml_locs_missing_loc() {
        let xml = r#"<urlset>
  <url>
    <priority>0.8</priority>
  </url>
</urlset>"#;
        let urls = extract_xml_locs(xml, "url");
        assert!(urls.is_empty());
    }

    #[test]
    fn test_extract_xml_locs_whitespace_in_loc() {
        let xml = r#"<urlset>
  <url>
    <loc>
      https://example.com/page
    </loc>
  </url>
</urlset>"#;
        let urls = extract_xml_locs(xml, "url");
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0], "https://example.com/page");
    }

    #[test]
    fn test_extract_xml_locs_does_not_match_wrong_parent() {
        // When looking for "url" parents, should not match "sitemap" blocks
        let xml = r#"<sitemapindex>
  <sitemap>
    <loc>https://example.com/sitemap.xml</loc>
  </sitemap>
</sitemapindex>"#;
        let urls = extract_xml_locs(xml, "url");
        assert!(urls.is_empty());
    }

    #[test]
    fn test_sitemapindex_detection() {
        let index_xml = r#"<?xml version="1.0"?><sitemapindex><sitemap><loc>https://example.com/s.xml</loc></sitemap></sitemapindex>"#;
        let urlset_xml =
            r#"<?xml version="1.0"?><urlset><url><loc>https://example.com/</loc></url></urlset>"#;

        // The detection logic used in collect_sitemap_urls
        assert!(index_xml.contains("<sitemapindex"));
        assert!(!urlset_xml.contains("<sitemapindex"));
    }

    #[test]
    fn test_robots_txt_parsing_logic() {
        // Simulate the robots.txt parsing logic from find_sitemaps_from_robots
        let robots = "User-agent: *\nDisallow: /admin\nSitemap: https://example.com/sitemap.xml\nsitemap: https://example.com/sitemap2.xml\n";
        let mut sitemaps = Vec::new();
        for line in robots.lines() {
            let trimmed = line.trim();
            if trimmed.len() > 8 && trimmed[..8].eq_ignore_ascii_case("sitemap:") {
                let url = trimmed[8..].trim();
                if !url.is_empty() {
                    sitemaps.push(url.to_string());
                }
            }
        }
        assert_eq!(sitemaps.len(), 2);
        assert_eq!(sitemaps[0], "https://example.com/sitemap.xml");
        assert_eq!(sitemaps[1], "https://example.com/sitemap2.xml");
    }

    #[test]
    fn test_robots_txt_no_sitemap_directives() {
        let robots = "User-agent: *\nDisallow: /private\nAllow: /\n";
        let mut sitemaps = Vec::new();
        for line in robots.lines() {
            let trimmed = line.trim();
            if trimmed.len() > 8 && trimmed[..8].eq_ignore_ascii_case("sitemap:") {
                let url = trimmed[8..].trim();
                if !url.is_empty() {
                    sitemaps.push(url.to_string());
                }
            }
        }
        assert!(sitemaps.is_empty());
    }
}
