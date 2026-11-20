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

use super::types::{PublicIncident, PublicIncidentStatus, PublicMaintenance};

/// What kind of thing an event announces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PageEventKind {
    /// A public update was posted on an incident.
    IncidentUpdate,
    /// An incident was resolved without a public "resolved" update.
    IncidentResolved,
    /// A maintenance window was scheduled.
    MaintenanceScheduled,
}

/// The public status an event reports: an incident's, or "scheduled" for a
/// maintenance window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PageEventStatus {
    Investigating,
    Identified,
    Monitoring,
    Resolved,
    Scheduled,
}

impl PageEventStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Investigating => "Investigating",
            Self::Identified => "Identified",
            Self::Monitoring => "Monitoring",
            Self::Resolved => "Resolved",
            Self::Scheduled => "Scheduled maintenance",
        }
    }
}

impl From<PublicIncidentStatus> for PageEventStatus {
    fn from(status: PublicIncidentStatus) -> Self {
        match status {
            PublicIncidentStatus::Investigating => Self::Investigating,
            PublicIncidentStatus::Identified => Self::Identified,
            PublicIncidentStatus::Monitoring => Self::Monitoring,
            PublicIncidentStatus::Resolved => Self::Resolved,
        }
    }
}

/// When a maintenance window runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventWindow {
    pub starts_at: UtcDateTime,
    pub ends_at: UtcDateTime,
}

/// One public happening on a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageEvent {
    /// Stable and unique per page: `u{update id}`, `r{incident id}` or
    /// `m{maintenance id}`. It keys the delivery outbox and, hashed, the
    /// feed entry id; it is never published as is.
    pub key: String,
    pub kind: PageEventKind,
    /// The incident's or maintenance window's public heading.
    pub title: String,
    pub status: PageEventStatus,
    /// The public update's text or the maintenance description; `None` for
    /// a resolution without one.
    pub message: Option<String>,
    pub affected_components: Vec<String>,
    pub published_at: UtcDateTime,
    /// When the window runs, for a maintenance event.
    pub window: Option<EventWindow>,
}

impl PageEvent {
    /// The message to show, with the resolution's stock sentence.
    pub fn message_or_default(&self) -> &str {
        match (self.message.as_deref(), self.kind) {
            (Some(message), _) if !message.trim().is_empty() => message,
            (_, PageEventKind::MaintenanceScheduled) => "Maintenance has been scheduled.",
            _ => "This incident has been resolved.",
        }
    }

    /// "Scheduled: <start> to <end> UTC" for a maintenance event.
    pub fn window_line(&self) -> Option<String> {
        self.window.map(|window| {
            format!(
                "Scheduled: {} to {}",
                window.starts_at.format("%Y-%m-%d %H:%M UTC"),
                window.ends_at.format("%Y-%m-%d %H:%M UTC")
            )
        })
    }
}

/// A maintenance window together with the id its event key is built from.
pub struct MaintenanceSource<'a> {
    pub maintenance_id: i32,
    pub created_at: UtcDateTime,
    pub maintenance: &'a PublicMaintenance,
}

/// The one announcement of a maintenance window, published when it was
/// scheduled. Later edits are shown on the page but not announced again.
pub fn maintenance_event(source: &MaintenanceSource<'_>) -> PageEvent {
    let maintenance = source.maintenance;
    PageEvent {
        key: format!("m{}", source.maintenance_id),
        kind: PageEventKind::MaintenanceScheduled,
        title: maintenance.title.clone(),
        status: PageEventStatus::Scheduled,
        message: maintenance.description.clone(),
        affected_components: maintenance.affected_components.clone(),
        published_at: source.created_at,
        window: Some(EventWindow {
            starts_at: maintenance.starts_at,
            ends_at: maintenance.ends_at,
        }),
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
                status: PageEventStatus::Resolved,
                message: None,
                affected_components: incident.affected_components.clone(),
                published_at: resolved_at,
                window: None,
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
                status: update.status.into(),
                message: Some(update.message.clone()),
                affected_components: incident.affected_components.clone(),
                published_at: update.created_at,
                window: None,
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
    fn a_maintenance_window_is_one_event_published_when_scheduled() {
        let maintenance = PublicMaintenance {
            title: "Database upgrade".to_string(),
            description: None,
            status: crate::services::public_pages::types::PublicMaintenanceStatus::Scheduled,
            starts_at: at(20),
            ends_at: at(22),
            affected_components: vec!["API".to_string()],
        };
        let event = maintenance_event(&MaintenanceSource {
            maintenance_id: 4,
            created_at: at(3),
            maintenance: &maintenance,
        });
        assert_eq!(event.key, "m4");
        assert_eq!(event.kind, PageEventKind::MaintenanceScheduled);
        assert_eq!(event.published_at, at(3));
        assert_eq!(
            event.message_or_default(),
            "Maintenance has been scheduled."
        );
        assert_eq!(
            event.window_line().as_deref(),
            Some("Scheduled: 2026-03-02 20:00 UTC to 2026-03-02 22:00 UTC")
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
