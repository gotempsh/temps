// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a subscriber is told, derived from the public page alone.
//!
//! A notice is built from a [`PublicIncident`] of a published snapshot: the
//! public heading, the public status, the display names of the affected
//! components and the text of one public update. Those are exactly the fields
//! the page shows, so the status page allowlist applies to mail unchanged: no
//! internal title, monitor name, URL, private update or error ever reaches a
//! subscriber. Everything user-written is escaped for the HTML part.

use std::fmt::Write as _;

use temps_core::escape::escape_html;
use temps_core::UtcDateTime;

use crate::services::public_pages::events::{incident_events, IncidentSource, PageEvent};

/// Only updates published within this window are announced. A notice row is
/// pruned well after it, so an update can never be queued a second time after
/// a restart, and a server that was down for days does not mail out stale
/// news when it comes back.
pub const NOTICE_WINDOW: chrono::Duration = chrono::Duration::hours(24);

/// One announcement for a page's subscribers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageNotice {
    /// `u{update id}` or `r{incident id}`; unique per page.
    pub key: String,
    /// When the announced update was published.
    pub published_at: UtcDateTime,
    pub subject: String,
    pub body_text: String,
    /// HTML fragment; the per-recipient footer is added at send time.
    pub body_html: String,
    /// What the notice announces, for channels that are not mail.
    pub event: PageEvent,
}

/// The notices for one incident whose events were published at or after
/// `since` and no later than `now` (see [`incident_events`]).
pub fn incident_notices(
    page_title: &str,
    source: &IncidentSource<'_>,
    since: UtcDateTime,
    now: UtcDateTime,
) -> Vec<PageNotice> {
    incident_events(source)
        .into_iter()
        .rev()
        .filter(|event| event.published_at >= since && event.published_at <= now)
        .map(|event| notice_for(page_title, event))
        .collect()
}

/// The mail for one event.
pub fn notice_for(page_title: &str, event: PageEvent) -> PageNotice {
    let label = event.status.label();
    let when = event.published_at.format("%Y-%m-%d %H:%M UTC").to_string();
    let subject = one_line(&format!("[{page_title}] {}: {label}", event.title));
    let affected =
        (!event.affected_components.is_empty()).then(|| event.affected_components.join(", "));
    let message = event.message_or_default();

    let window = event.window_line();
    let mut body_text = format!("{}\nStatus: {label}\n", event.title);
    if let Some(window) = &window {
        let _ = writeln!(body_text, "{window}");
    }
    if let Some(affected) = &affected {
        let _ = writeln!(body_text, "Affected: {affected}");
    }
    let _ = write!(body_text, "Posted: {when}\n\n{message}\n");

    let mut body_html = format!(
        "<h2 style=\"margin:0 0 8px;font-size:18px\">{}</h2>\
         <p style=\"margin:0;color:#52525b\">Status: <strong>{label}</strong></p>",
        escape_html(&event.title)
    );
    if let Some(window) = &window {
        let _ = write!(
            body_html,
            "<p style=\"margin:4px 0 0;color:#52525b\">{}</p>",
            escape_html(window)
        );
    }
    if let Some(affected) = &affected {
        let _ = write!(
            body_html,
            "<p style=\"margin:4px 0 0;color:#52525b\">Affected: {}</p>",
            escape_html(affected)
        );
    }
    let _ = write!(
        body_html,
        "<p style=\"margin:4px 0 0;color:#52525b\">Posted: {when}</p>\
         <p style=\"margin:16px 0 0;white-space:pre-wrap\">{}</p>",
        escape_html(message)
    );

    PageNotice {
        key: event.key.clone(),
        published_at: event.published_at,
        subject,
        body_text,
        body_html,
        event,
    }
}

/// Header-safe single line, bounded: a subject never carries a line break and
/// is cut on a character boundary.
pub(crate) fn one_line(text: &str) -> String {
    const MAX_SUBJECT_CHARS: usize = 200;
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut chars = flat.chars();
    let mut out: String = chars.by_ref().take(MAX_SUBJECT_CHARS).collect();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

/// The full message for one recipient: the notice plus a footer with the page
/// link and the recipient's own unsubscribe link.
pub fn with_footer(
    body_text: &str,
    body_html: &str,
    page_title: &str,
    page_url: &str,
    unsubscribe_url: &str,
) -> (String, String) {
    let text = format!(
        "{body_text}\nView the status page: {page_url}\n\n--\nYou receive this because you \
         subscribed to updates from {page_title}.\nUnsubscribe: {unsubscribe_url}\n"
    );
    let html = format!(
        "<!doctype html><html><body style=\"font-family:system-ui,-apple-system,Segoe UI,\
         Roboto,sans-serif;color:#18181b;max-width:560px;margin:0 auto;padding:24px\">\
         {body_html}<p style=\"margin:24px 0 0\"><a href=\"{}\">View the status page</a></p>\
         <hr style=\"border:none;border-top:1px solid #e4e4e7;margin:24px 0\">\
         <p style=\"font-size:12px;color:#71717a;margin:0\">You receive this because you \
         subscribed to updates from {}. <a href=\"{}\">Unsubscribe</a></p></body></html>",
        escape_html(page_url),
        escape_html(page_title),
        escape_html(unsubscribe_url),
    );
    (text, html)
}

/// The double opt-in message.
pub fn confirmation_message(
    page_title: &str,
    confirm_url: &str,
    valid_hours: i64,
) -> (String, String, String) {
    let subject = one_line(&format!("Confirm your subscription to {page_title}"));
    let text = format!(
        "Someone, hopefully you, asked to receive incident updates from {page_title} at this \
         address.\n\nConfirm the subscription: {confirm_url}\n\nThe link is valid for \
         {valid_hours} hours. If you did not ask for this, ignore this message: nothing \
         will be sent to you.\n"
    );
    let html = format!(
        "<!doctype html><html><body style=\"font-family:system-ui,-apple-system,Segoe UI,\
         Roboto,sans-serif;color:#18181b;max-width:560px;margin:0 auto;padding:24px\">\
         <p style=\"margin:0\">Someone, hopefully you, asked to receive incident updates from \
         <strong>{}</strong> at this address.</p>\
         <p style=\"margin:20px 0\"><a href=\"{}\">Confirm the subscription</a></p>\
         <p style=\"font-size:12px;color:#71717a;margin:0\">The link is valid for {valid_hours} \
         hours. If you did not ask for this, ignore this message: nothing will be sent to \
         you.</p></body></html>",
        escape_html(page_title),
        escape_html(confirm_url),
    );
    (subject, text, html)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::public_pages::{
        PublicIncident, PublicIncidentSeverity, PublicIncidentStatus, PublicIncidentUpdate,
    };
    use chrono::TimeZone;

    fn at(hour: u32) -> UtcDateTime {
        chrono::Utc
            .with_ymd_and_hms(2026, 3, 2, hour, 0, 0)
            .single()
            .expect("valid time")
    }

    fn incident(
        status: PublicIncidentStatus,
        updates: Vec<(PublicIncidentStatus, &str, u32)>,
    ) -> PublicIncident {
        PublicIncident {
            title: "Elevated errors <b>".to_string(),
            severity: PublicIncidentSeverity::Major,
            status,
            affected_components: vec!["API".to_string(), "Web".to_string()],
            started_at: at(1),
            resolved_at: (status == PublicIncidentStatus::Resolved).then(|| at(9)),
            updates: updates
                .into_iter()
                .map(|(status, message, hour)| PublicIncidentUpdate {
                    status,
                    message: message.to_string(),
                    created_at: at(hour),
                })
                .collect(),
        }
    }

    #[test]
    fn one_notice_per_update_in_the_window_keyed_by_update_id() {
        let incident = incident(
            PublicIncidentStatus::Monitoring,
            vec![
                (PublicIncidentStatus::Monitoring, "A fix is out", 6),
                (PublicIncidentStatus::Investigating, "Looking into it", 2),
            ],
        );
        let source = IncidentSource {
            incident_id: 7,
            update_ids: &[31, 30],
            incident: &incident,
        };
        let notices = incident_notices("Acme Status", &source, at(3), at(10));
        assert_eq!(
            notices.len(),
            1,
            "the update before `since` is not announced"
        );
        assert_eq!(notices[0].key, "u31");
        assert_eq!(notices[0].published_at, at(6));
        assert_eq!(
            notices[0].subject,
            "[Acme Status] Elevated errors <b>: Monitoring"
        );
        assert!(notices[0].body_text.contains("A fix is out"));
        assert!(notices[0].body_text.contains("Affected: API, Web"));
        assert!(
            notices[0].body_html.contains("Elevated errors &lt;b&gt;"),
            "user text is escaped in HTML"
        );
        assert!(!notices[0].body_html.contains("<b>"));
    }

    #[test]
    fn a_resolution_without_its_own_public_update_gets_a_notice() {
        let silent = incident(
            PublicIncidentStatus::Resolved,
            vec![(PublicIncidentStatus::Investigating, "Looking into it", 2)],
        );
        let source = IncidentSource {
            incident_id: 7,
            update_ids: &[30],
            incident: &silent,
        };
        let notices = incident_notices("Acme", &source, at(0), at(10));
        let keys: Vec<&str> = notices.iter().map(|n| n.key.as_str()).collect();
        assert_eq!(keys, ["u30", "r7"]);
        assert!(notices[1]
            .body_text
            .contains("This incident has been resolved."));
        assert_eq!(notices[1].published_at, at(9));

        let announced = incident(
            PublicIncidentStatus::Resolved,
            vec![(PublicIncidentStatus::Resolved, "All clear", 9)],
        );
        let source = IncidentSource {
            incident_id: 7,
            update_ids: &[32],
            incident: &announced,
        };
        let keys: Vec<String> = incident_notices("Acme", &source, at(0), at(10))
            .into_iter()
            .map(|n| n.key)
            .collect();
        assert_eq!(
            keys,
            ["u32"],
            "a public resolved update is not announced twice"
        );
    }

    #[test]
    fn subjects_are_single_line_and_bounded() {
        assert_eq!(one_line("a\r\nBcc: x"), "a  Bcc: x");
        let long = "x".repeat(500);
        let cut = one_line(&long);
        assert_eq!(cut.chars().count(), 201);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn the_footer_carries_the_page_and_unsubscribe_links_escaped() {
        let (text, html) = with_footer(
            "Body",
            "<p>Body</p>",
            "Acme & Co",
            "https://status.example.test/",
            "https://status.example.test/unsubscribe/ab\"cd",
        );
        assert!(text.contains("Unsubscribe: https://status.example.test/unsubscribe/ab\"cd"));
        assert!(html.contains("Acme &amp; Co"));
        assert!(html.contains("unsubscribe/ab&quot;cd"));
    }
}
