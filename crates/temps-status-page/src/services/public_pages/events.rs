// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What happened on a public page, as one typed list every channel renders
//! (ADR 108 D2): subscriber mail, the Atom feed and webhook payloads.
//!
//! An event is built only from the published view of the page: a public
//! incident's public heading, status, affected component display names and
//! the text of one public update. Those are exactly the fields the page
//! shows, so the status page allowlist applies to every channel unchanged:
//! no internal title, monitor name, URL, private update or error can reach a
//! feed reader, a webhook or an inbox.

use serde::Serialize;
use temps_core::UtcDateTime;
use utoipa::ToSchema;

use super::types::{PublicIncident, PublicIncidentStatus};

/// What kind of thing an event announces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PageEventKind {
    /// A public update was posted on an incident.
    IncidentUpdate,
    /// An incident was resolved without a public "resolved" update.
    IncidentResolved,
}

/// One public happening on a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageEvent {
    /// Stable and unique per page: `u{update id}` or `r{incident id}`. It
    /// keys the delivery outbox and, hashed, the feed entry id; it is never
    /// published as is.
    pub key: String,
    pub kind: PageEventKind,
    /// The incident's public heading.
    pub title: String,
    pub status: PublicIncidentStatus,
    /// The public update's text; `None` for a resolution without one.
    pub message: Option<String>,
    pub affected_components: Vec<String>,
    pub published_at: UtcDateTime,
}

impl PageEvent {
    /// The message to show, with the resolution's stock sentence.
    pub fn message_or_default(&self) -> &str {
        self.message
            .as_deref()
            .unwrap_or("This incident has been resolved.")
    }
}

/// A public incident together with the ids event keys are built from.
/// `update_ids` is aligned with `incident.updates` (newest first).
pub struct IncidentSource<'a> {
    pub incident_id: i32,
    pub update_ids: &'a [i32],
    pub incident: &'a PublicIncident,
}

/// Every event of one incident, newest first: one per public update, plus
/// one for the resolution when the incident was resolved without a public
/// "resolved" update of its own.
pub fn incident_events(source: &IncidentSource<'_>) -> Vec<PageEvent> {
    let incident = source.incident;
    let mut events = Vec::with_capacity(incident.updates.len() + 1);
    let resolution_has_update = incident
        .updates
        .iter()
        .any(|update| update.status == PublicIncidentStatus::Resolved);
    if incident.status == PublicIncidentStatus::Resolved && !resolution_has_update {
        if let Some(resolved_at) = incident.resolved_at {
            events.push(PageEvent {
                key: format!("r{}", source.incident_id),
                kind: PageEventKind::IncidentResolved,
                title: incident.title.clone(),
                status: PublicIncidentStatus::Resolved,
                message: None,
                affected_components: incident.affected_components.clone(),
                published_at: resolved_at,
            });
        }
    }
    events.extend(
        incident
            .updates
            .iter()
            .zip(source.update_ids)
            .map(|(update, id)| PageEvent {
                key: format!("u{id}"),
                kind: PageEventKind::IncidentUpdate,
                title: incident.title.clone(),
                status: update.status,
                message: Some(update.message.clone()),
                affected_components: incident.affected_components.clone(),
                published_at: update.created_at,
            }),
    );
    events
}

/// The page's events, newest first, at most `limit`.
pub fn newest_events(mut events: Vec<PageEvent>, limit: usize) -> Vec<PageEvent> {
    events.sort_by(|a, b| {
        b.published_at
            .cmp(&a.published_at)
            .then_with(|| b.key.cmp(&a.key))
    });
    events.truncate(limit);
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::public_pages::types::{PublicIncidentSeverity, PublicIncidentUpdate};
    use chrono::TimeZone;

    fn at(hour: u32) -> UtcDateTime {
        chrono::Utc
            .with_ymd_and_hms(2026, 3, 2, hour, 0, 0)
            .single()
            .expect("valid time")
    }

    fn incident(
        status: PublicIncidentStatus,
        updates: &[(PublicIncidentStatus, u32)],
    ) -> PublicIncident {
        PublicIncident {
            title: "Elevated errors".to_string(),
            severity: PublicIncidentSeverity::Major,
            status,
            affected_components: vec!["API".to_string()],
            started_at: at(1),
            resolved_at: (status == PublicIncidentStatus::Resolved).then(|| at(9)),
            updates: updates
                .iter()
                .map(|(status, hour)| PublicIncidentUpdate {
                    status: *status,
                    message: format!("update at {hour}"),
                    created_at: at(*hour),
                })
                .collect(),
        }
    }

    #[test]
    fn each_public_update_is_an_event_keyed_by_its_id() {
        let incident = incident(
            PublicIncidentStatus::Monitoring,
            &[
                (PublicIncidentStatus::Monitoring, 5),
                (PublicIncidentStatus::Investigating, 2),
            ],
        );
        let events = incident_events(&IncidentSource {
            incident_id: 7,
            update_ids: &[12, 11],
            incident: &incident,
        });
        let keys: Vec<&str> = events.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys, vec!["u12", "u11"]);
        assert_eq!(events[0].message.as_deref(), Some("update at 5"));
        assert_eq!(events[0].affected_components, vec!["API".to_string()]);
    }

    #[test]
    fn a_resolution_without_its_own_update_is_an_event() {
        let incident = incident(
            PublicIncidentStatus::Resolved,
            &[(PublicIncidentStatus::Investigating, 2)],
        );
        let events = incident_events(&IncidentSource {
            incident_id: 7,
            update_ids: &[11],
            incident: &incident,
        });
        assert_eq!(events[0].key, "r7");
        assert_eq!(events[0].kind, PageEventKind::IncidentResolved);
        assert_eq!(
            events[0].message_or_default(),
            "This incident has been resolved."
        );

        let with_update = super::super::types::PublicIncident {
            updates: vec![PublicIncidentUpdate {
                status: PublicIncidentStatus::Resolved,
                message: "fixed".to_string(),
                created_at: at(9),
            }],
            ..incident
        };
        let events = incident_events(&IncidentSource {
            incident_id: 7,
            update_ids: &[13],
            incident: &with_update,
        });
        assert_eq!(
            events.len(),
            1,
            "the public resolved update is the resolution"
        );
    }

    #[test]
    fn newest_events_sorts_and_caps() {
        let incident = incident(
            PublicIncidentStatus::Monitoring,
            &[
                (PublicIncidentStatus::Monitoring, 5),
                (PublicIncidentStatus::Investigating, 2),
            ],
        );
        let mut events = incident_events(&IncidentSource {
            incident_id: 7,
            update_ids: &[12, 11],
            incident: &incident,
        });
        events.reverse();
        let newest = newest_events(events, 1);
        assert_eq!(newest.len(), 1);
        assert_eq!(newest[0].key, "u12");
    }
}
