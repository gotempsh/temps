// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The Atom feed of a public status page (ADR 108 D2).
//!
//! Rendered with the snapshot from the same [`PageEvent`]s subscriber mail
//! uses, so it carries nothing the page does not show, and served from
//! memory like the page. Feed and entry ids are hashes: the slug is the
//! page's capability and the event keys hold internal ids, so neither is
//! published (a custom hostname's feed must not reveal the slug URL).

use std::fmt::Write as _;

use chrono::SecondsFormat;
use sha2::{Digest, Sha256};
use temps_core::escape::escape_html;
use temps_core::UtcDateTime;

use super::events::PageEvent;
use super::types::PublicStatusPageResponse;

/// The content type the feed is served with.
pub const ATOM_CONTENT_TYPE: &str = "application/atom+xml; charset=utf-8";

/// Text for an XML element: escaped, and without the control characters XML
/// 1.0 forbids (anything below U+0020 except tab, newline and return).
fn xml_text(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r'))
        .collect();
    escape_html(&cleaned)
}

fn rfc3339(at: &UtcDateTime) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// A stable opaque id derived from `parts`.
fn opaque_id(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"status-page-feed");
    for part in parts {
        hasher.update(b":");
        hasher.update(part.as_bytes());
    }
    let digest = hex::encode(hasher.finalize());
    format!("urn:temps:status-page:{}", &digest[..32])
}

/// The feed for `page`. `base_path` is where the page is served, empty for a
/// custom hostname (`/`), and the feed's links are relative to the host the
/// reader fetched it from.
pub fn render_atom_feed(
    slug: &str,
    page: &PublicStatusPageResponse,
    events: &[PageEvent],
    base_path: &str,
) -> String {
    let page_href = if base_path.is_empty() { "/" } else { base_path };
    let updated = events
        .first()
        .map(|event| event.published_at)
        .unwrap_or(page.generated_at);
    let feed_id = opaque_id(&[slug]);

    let mut xml = String::with_capacity(1024 + events.len() * 512);
    xml.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    xml.push_str("<feed xmlns=\"http://www.w3.org/2005/Atom\">\n");
    let _ = writeln!(xml, "  <title>{}</title>", xml_text(&page.title));
    if let Some(description) = page.description.as_deref().filter(|d| !d.trim().is_empty()) {
        let _ = writeln!(xml, "  <subtitle>{}</subtitle>", xml_text(description));
    }
    let _ = writeln!(xml, "  <id>{feed_id}</id>");
    let _ = writeln!(
        xml,
        "  <link rel=\"alternate\" type=\"text/html\" href=\"{}\"/>",
        xml_text(page_href)
    );
    let _ = writeln!(
        xml,
        "  <link rel=\"self\" type=\"application/atom+xml\" href=\"{}/feed.atom\"/>",
        xml_text(base_path)
    );
    let _ = writeln!(xml, "  <updated>{}</updated>", rfc3339(&updated));
    let _ = writeln!(
        xml,
        "  <author><name>{}</name></author>",
        xml_text(&page.title)
    );

    for event in events {
        let mut content = format!("Status: {}\n", event.status.label());
        if !event.affected_components.is_empty() {
            let _ = writeln!(
                content,
                "Affected: {}",
                event.affected_components.join(", ")
            );
        }
        let _ = write!(content, "\n{}", event.message_or_default());

        xml.push_str("  <entry>\n");
        let _ = writeln!(xml, "    <id>{}</id>", opaque_id(&[slug, &event.key]));
        let _ = writeln!(
            xml,
            "    <title>{}: {}</title>",
            xml_text(&event.title),
            event.status.label()
        );
        let _ = writeln!(
            xml,
            "    <published>{}</published>",
            rfc3339(&event.published_at)
        );
        let _ = writeln!(
            xml,
            "    <updated>{}</updated>",
            rfc3339(&event.published_at)
        );
        let _ = writeln!(
            xml,
            "    <link rel=\"alternate\" type=\"text/html\" href=\"{}\"/>",
            xml_text(page_href)
        );
        let _ = writeln!(
            xml,
            "    <content type=\"text\">{}</content>",
            xml_text(&content)
        );
        xml.push_str("  </entry>\n");
    }
    xml.push_str("</feed>\n");
    xml
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::public_pages::events::PageEventKind;
    use crate::services::public_pages::types::{
        PublicComponentStatus, PublicIncidentStatus, StatusPageAppearance,
    };
    use chrono::TimeZone;

    fn at(hour: u32) -> UtcDateTime {
        chrono::Utc
            .with_ymd_and_hms(2026, 3, 2, hour, 0, 0)
            .single()
            .expect("valid time")
    }

    fn page() -> PublicStatusPageResponse {
        PublicStatusPageResponse {
            title: "Acme & Co <status>".to_string(),
            description: Some("All our \u{7}services".to_string()),
            appearance: StatusPageAppearance::default(),
            status: PublicComponentStatus::Operational,
            components: vec![],
            incidents: vec![],
            history_days: 90,
            generated_at: at(10),
        }
    }

    fn event(key: &str, hour: u32) -> PageEvent {
        PageEvent {
            key: key.to_string(),
            kind: PageEventKind::IncidentUpdate,
            title: "Elevated errors".to_string(),
            status: PublicIncidentStatus::Identified,
            message: Some("We found the cause <b>".to_string()),
            affected_components: vec!["API".to_string()],
            published_at: at(hour),
        }
    }

    #[test]
    fn the_feed_carries_each_event_escaped() {
        let xml = render_atom_feed(
            "secret-slug",
            &page(),
            &[event("u12", 9), event("u11", 4)],
            "/api/status/secret-slug",
        );
        assert!(xml.starts_with("<?xml version=\"1.0\""));
        assert!(xml.contains("<title>Acme &amp; Co &lt;status&gt;</title>"));
        assert!(
            xml.contains("<subtitle>All our services</subtitle>"),
            "{xml}"
        );
        assert_eq!(xml.matches("<entry>").count(), 2);
        assert!(xml.contains("<title>Elevated errors: Identified</title>"));
        assert!(xml.contains("We found the cause &lt;b&gt;"));
        assert!(xml.contains("<updated>2026-03-02T09:00:00Z</updated>"));
        assert!(xml.contains("href=\"/api/status/secret-slug/feed.atom\""));
    }

    #[test]
    fn ids_reveal_neither_the_slug_nor_the_internal_keys() {
        let xml = render_atom_feed("secret-slug", &page(), &[event("u12", 9)], "");
        assert!(!xml.contains("secret-slug"), "{xml}");
        assert!(!xml.contains("u12"));
        assert!(xml.contains("href=\"/\""));
        assert!(xml.contains("href=\"/feed.atom\""));
        // Stable across rebuilds, distinct per entry.
        let again = render_atom_feed("secret-slug", &page(), &[event("u12", 9)], "");
        assert_eq!(xml, again);
        assert_ne!(
            opaque_id(&["secret-slug", "u12"]),
            opaque_id(&["secret-slug", "u11"])
        );
    }

    #[test]
    fn an_empty_feed_is_dated_by_the_snapshot() {
        let xml = render_atom_feed("s", &page(), &[], "/api/status/s");
        assert!(xml.contains("<updated>2026-03-02T10:00:00Z</updated>"));
        assert!(!xml.contains("<entry>"));
    }
}
