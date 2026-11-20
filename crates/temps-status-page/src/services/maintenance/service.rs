// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Scheduling, editing and cancelling maintenance windows (ADR 108 D2).
//!
//! Every write commits, then drops the cached uptime history of the monitors
//! involved and rebuilds the page, so the page, the feed and the outage
//! detector all see the change on the next read.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use chrono::{Duration, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use temps_core::UtcDateTime;
use temps_entities::{
    status_page_maintenance_monitors, status_page_maintenances, status_page_monitors, status_pages,
};
use tracing::{info, warn};

use super::error::MaintenanceError;
use super::types::{
    CreateMaintenanceRequest, MaintenanceListResponse, MaintenanceResponse, MaintenanceState,
    UpdateMaintenanceRequest,
};
use crate::services::public_pages::StatusSnapshotCache;

/// Longest public title.
pub const MAX_TITLE_CHARS: usize = 120;
/// Longest public description.
pub const MAX_DESCRIPTION_CHARS: usize = 2000;
/// Longest window. A longer one would hide a real outage for too long.
pub const MAX_WINDOW: Duration = Duration::days(7);
/// How far ahead a window can be scheduled.
pub const MAX_LEAD: Duration = Duration::days(365);
/// How far in the past a new start may be, to absorb a slow form. Anything
/// earlier would erase checks already recorded from public uptime.
pub const BACKDATE_SLACK: Duration = Duration::minutes(5);
/// Most windows in progress or scheduled per page. Bounds what the snapshot
/// and the outage detector read.
pub const MAX_OPEN_PER_PAGE: u64 = 20;

/// Where `model` is in its life at `now`.
pub fn maintenance_state(
    model: &status_page_maintenances::Model,
    now: UtcDateTime,
) -> MaintenanceState {
    if model.cancelled_at.is_some() {
        MaintenanceState::Cancelled
    } else if model.ends_at <= now {
        MaintenanceState::Completed
    } else if model.starts_at <= now {
        MaintenanceState::InProgress
    } else {
        MaintenanceState::Scheduled
    }
}

fn invalid(status_page_id: i32, reason: impl Into<String>) -> MaintenanceError {
    MaintenanceError::Validation {
        status_page_id,
        reason: reason.into(),
    }
}

/// Title rules; returns the trimmed title.
pub fn validate_title(status_page_id: i32, title: &str) -> Result<String, MaintenanceError> {
    let title = title.trim();
    if title.is_empty() {
        return Err(invalid(status_page_id, "the title is empty"));
    }
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err(invalid(
            status_page_id,
            format!("the title is longer than {MAX_TITLE_CHARS} characters"),
        ));
    }
    if title.chars().any(char::is_control) {
        return Err(invalid(
            status_page_id,
            "the title contains control characters",
        ));
    }
    Ok(title.to_string())
}

/// Description rules; an empty description is none.
pub fn validate_description(
    status_page_id: i32,
    description: Option<&str>,
) -> Result<Option<String>, MaintenanceError> {
    let Some(description) = description.map(str::trim).filter(|d| !d.is_empty()) else {
        return Ok(None);
    };
    if description.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(invalid(
            status_page_id,
            format!("the description is longer than {MAX_DESCRIPTION_CHARS} characters"),
        ));
    }
    Ok(Some(description.to_string()))
}

/// Time rules. `started` is whether the window being edited already began;
/// its start is then fixed and not checked against the backdating limit.
pub fn validate_window(
    status_page_id: i32,
    starts_at: UtcDateTime,
    ends_at: UtcDateTime,
    now: UtcDateTime,
    started: bool,
) -> Result<(), MaintenanceError> {
    if ends_at <= starts_at {
        return Err(invalid(
            status_page_id,
            "the window must end after it starts",
        ));
    }
    if ends_at - starts_at > MAX_WINDOW {
        return Err(invalid(
            status_page_id,
            format!("a window lasts at most {} days", MAX_WINDOW.num_days()),
        ));
    }
    if ends_at <= now {
        return Err(invalid(status_page_id, "the window must end in the future"));
    }
    if !started && starts_at < now - BACKDATE_SLACK {
        return Err(invalid(
            status_page_id,
            "the window cannot start in the past: a maintenance window never erases checks \
             already recorded",
        ));
    }
    if starts_at > now + MAX_LEAD {
        return Err(invalid(
            status_page_id,
            format!(
                "a window can be scheduled at most {} days ahead",
                MAX_LEAD.num_days()
            ),
        ));
    }
    Ok(())
}

pub struct StatusPageMaintenanceService {
    db: Arc<DatabaseConnection>,
    cache: Arc<StatusSnapshotCache>,
}

impl StatusPageMaintenanceService {
    pub fn new(db: Arc<DatabaseConnection>, cache: Arc<StatusSnapshotCache>) -> Self {
        Self { db, cache }
    }

    async fn ensure_page<C: ConnectionTrait>(
        conn: &C,
        status_page_id: i32,
        lock: bool,
    ) -> Result<(), MaintenanceError> {
        let mut query = status_pages::Entity::find_by_id(status_page_id)
            .select_only()
            .column(status_pages::Column::Id);
        if lock {
            query = query.lock_exclusive();
        }
        query
            .into_tuple::<i32>()
            .one(conn)
            .await?
            .map(|_| ())
            .ok_or(MaintenanceError::StatusPageNotFound { status_page_id })
    }

    async fn find<C: ConnectionTrait>(
        conn: &C,
        status_page_id: i32,
        maintenance_id: i32,
    ) -> Result<status_page_maintenances::Model, MaintenanceError> {
        status_page_maintenances::Entity::find_by_id(maintenance_id)
            .filter(status_page_maintenances::Column::StatusPageId.eq(status_page_id))
            .one(conn)
            .await?
            .ok_or(MaintenanceError::NotFound {
                status_page_id,
                maintenance_id,
            })
    }

    /// The covered monitors of each window in `ids`, one query.
    async fn monitors_of<C: ConnectionTrait>(
        conn: &C,
        ids: &[i32],
    ) -> Result<HashMap<i32, Vec<i32>>, MaintenanceError> {
        let mut by_window: HashMap<i32, Vec<i32>> = HashMap::new();
        if ids.is_empty() {
            return Ok(by_window);
        }
        let rows = status_page_maintenance_monitors::Entity::find()
            .filter(status_page_maintenance_monitors::Column::MaintenanceId.is_in(ids.to_vec()))
            .order_by_asc(status_page_maintenance_monitors::Column::MonitorId)
            .all(conn)
            .await?;
        for row in rows {
            by_window
                .entry(row.maintenance_id)
                .or_default()
                .push(row.monitor_id);
        }
        Ok(by_window)
    }

    /// `requested` checked against the page's monitors; sorted, distinct.
    async fn page_monitor_subset<C: ConnectionTrait>(
        conn: &C,
        status_page_id: i32,
        requested: &[i32],
    ) -> Result<Vec<i32>, MaintenanceError> {
        let wanted: BTreeSet<i32> = requested.iter().copied().collect();
        if wanted.is_empty() {
            return Err(invalid(
                status_page_id,
                "choose at least one monitor of this page",
            ));
        }
        let on_page: BTreeSet<i32> = status_page_monitors::Entity::find()
            .select_only()
            .column(status_page_monitors::Column::MonitorId)
            .filter(status_page_monitors::Column::StatusPageId.eq(status_page_id))
            .into_tuple::<i32>()
            .all(conn)
            .await?
            .into_iter()
            .collect();
        let foreign: Vec<String> = wanted
            .difference(&on_page)
            .map(|id| id.to_string())
            .collect();
        if !foreign.is_empty() {
            return Err(invalid(
                status_page_id,
                format!("monitors {} are not on this page", foreign.join(", ")),
            ));
        }
        Ok(wanted.into_iter().collect())
    }

    fn response(
        model: status_page_maintenances::Model,
        monitor_ids: Vec<i32>,
        now: UtcDateTime,
    ) -> MaintenanceResponse {
        MaintenanceResponse {
            state: maintenance_state(&model, now),
            id: model.id,
            status_page_id: model.status_page_id,
            title: model.title,
            description: model.description,
            starts_at: model.starts_at,
            ends_at: model.ends_at,
            cancelled_at: model.cancelled_at,
            monitor_ids,
            created_at: model.created_at,
            updated_at: model.updated_at,
        }
    }

    /// Drop the monitors' cached uptime and rebuild the page. A failed
    /// rebuild is retried by the refresh loop within a minute; the write
    /// itself has committed.
    async fn publish(&self, status_page_id: i32, monitor_ids: &[i32]) {
        self.cache.forget_history(monitor_ids).await;
        if let Err(error) = self.cache.refresh_page(status_page_id).await {
            warn!(
                status_page_id,
                error = %error,
                "failed to rebuild the status page after a maintenance change; the refresh \
                 loop will retry"
            );
        }
    }

    pub async fn list(
        &self,
        status_page_id: i32,
        page: u64,
        page_size: u64,
    ) -> Result<MaintenanceListResponse, MaintenanceError> {
        let page = page.max(1);
        let page_size = page_size.clamp(1, 100);
        Self::ensure_page(self.db.as_ref(), status_page_id, false).await?;
        let paginator = status_page_maintenances::Entity::find()
            .filter(status_page_maintenances::Column::StatusPageId.eq(status_page_id))
            .order_by_desc(status_page_maintenances::Column::CreatedAt)
            .order_by_desc(status_page_maintenances::Column::Id)
            .paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let models = paginator.fetch_page(page - 1).await?;
        let ids: Vec<i32> = models.iter().map(|m| m.id).collect();
        let mut monitors = Self::monitors_of(self.db.as_ref(), &ids).await?;
        let now = Utc::now();
        Ok(MaintenanceListResponse {
            items: models
                .into_iter()
                .map(|model| {
                    let monitor_ids = monitors.remove(&model.id).unwrap_or_default();
                    Self::response(model, monitor_ids, now)
                })
                .collect(),
            total,
            page,
            page_size,
        })
    }

    pub async fn get(
        &self,
        status_page_id: i32,
        maintenance_id: i32,
    ) -> Result<MaintenanceResponse, MaintenanceError> {
        let model = Self::find(self.db.as_ref(), status_page_id, maintenance_id).await?;
        let monitor_ids = Self::monitors_of(self.db.as_ref(), &[model.id])
            .await?
            .remove(&model.id)
            .unwrap_or_default();
        Ok(Self::response(model, monitor_ids, Utc::now()))
    }

    pub async fn create(
        &self,
        status_page_id: i32,
        request: CreateMaintenanceRequest,
    ) -> Result<MaintenanceResponse, MaintenanceError> {
        let now = Utc::now();
        let title = validate_title(status_page_id, &request.title)?;
        let description = validate_description(status_page_id, request.description.as_deref())?;
        validate_window(
            status_page_id,
            request.starts_at,
            request.ends_at,
            now,
            false,
        )?;

        let txn = self.db.begin().await?;
        // Locking the page row serializes creates, so two at once cannot
        // both pass the open-window limit.
        Self::ensure_page(&txn, status_page_id, true).await?;
        let monitor_ids =
            Self::page_monitor_subset(&txn, status_page_id, &request.monitor_ids).await?;
        let open = status_page_maintenances::Entity::find()
            .filter(status_page_maintenances::Column::StatusPageId.eq(status_page_id))
            .filter(status_page_maintenances::Column::CancelledAt.is_null())
            .filter(status_page_maintenances::Column::EndsAt.gt(now))
            .count(&txn)
            .await?;
        if open >= MAX_OPEN_PER_PAGE {
            return Err(MaintenanceError::TooManyScheduled {
                status_page_id,
                limit: MAX_OPEN_PER_PAGE,
            });
        }
        let model = status_page_maintenances::ActiveModel {
            status_page_id: Set(status_page_id),
            title: Set(title),
            description: Set(description),
            starts_at: Set(request.starts_at),
            ends_at: Set(request.ends_at),
            cancelled_at: Set(None),
            ..Default::default()
        }
        .insert(&txn)
        .await?;
        status_page_maintenance_monitors::Entity::insert_many(monitor_ids.iter().map(|&id| {
            status_page_maintenance_monitors::ActiveModel {
                maintenance_id: Set(model.id),
                monitor_id: Set(id),
            }
        }))
        .exec(&txn)
        .await?;
        txn.commit().await?;

        info!(
            status_page_id,
            maintenance_id = model.id,
            monitors = monitor_ids.len(),
            starts_at = %model.starts_at,
            ends_at = %model.ends_at,
            "scheduled status page maintenance window"
        );
        self.publish(status_page_id, &monitor_ids).await;
        Ok(Self::response(model, monitor_ids, now))
    }

    pub async fn update(
        &self,
        status_page_id: i32,
        maintenance_id: i32,
        request: UpdateMaintenanceRequest,
    ) -> Result<MaintenanceResponse, MaintenanceError> {
        let now = Utc::now();
        let txn = self.db.begin().await?;
        Self::ensure_page(&txn, status_page_id, true).await?;
        let model = Self::find(&txn, status_page_id, maintenance_id).await?;
        let state = maintenance_state(&model, now);
        let conflict = |reason: &str| MaintenanceError::Conflict {
            maintenance_id,
            reason: reason.to_string(),
        };
        match state {
            MaintenanceState::Cancelled => return Err(conflict("it was cancelled")),
            MaintenanceState::Completed => return Err(conflict("it has already ended")),
            MaintenanceState::Scheduled | MaintenanceState::InProgress => {}
        }
        let started = state == MaintenanceState::InProgress;
        let previous_monitors = Self::monitors_of(&txn, &[model.id])
            .await?
            .remove(&model.id)
            .unwrap_or_default();

        if started {
            if request.starts_at.is_some_and(|s| s != model.starts_at) {
                return Err(conflict(
                    "it has started, so its start can no longer change; cancel it to end it now",
                ));
            }
            if request.monitor_ids.as_ref().is_some_and(|ids| {
                let ids: BTreeSet<i32> = ids.iter().copied().collect();
                ids.into_iter().collect::<Vec<_>>() != previous_monitors
            }) {
                return Err(conflict(
                    "it has started, so the monitors it covers can no longer change",
                ));
            }
        }

        let title = match request.title.as_deref() {
            Some(title) => validate_title(status_page_id, title)?,
            None => model.title.clone(),
        };
        let description = match request.description.as_deref() {
            Some(description) => validate_description(status_page_id, Some(description))?,
            None => model.description.clone(),
        };
        let starts_at = request.starts_at.unwrap_or(model.starts_at);
        let ends_at = request.ends_at.unwrap_or(model.ends_at);
        validate_window(status_page_id, starts_at, ends_at, now, started)?;
        let monitor_ids = match request.monitor_ids.as_deref() {
            Some(ids) if !started => Self::page_monitor_subset(&txn, status_page_id, ids).await?,
            _ => previous_monitors.clone(),
        };

        let mut active: status_page_maintenances::ActiveModel = model.into();
        active.title = Set(title);
        active.description = Set(description);
        active.starts_at = Set(starts_at);
        active.ends_at = Set(ends_at);
        let model = active.update(&txn).await?;
        if monitor_ids != previous_monitors {
            status_page_maintenance_monitors::Entity::delete_many()
                .filter(status_page_maintenance_monitors::Column::MaintenanceId.eq(model.id))
                .exec(&txn)
                .await?;
            status_page_maintenance_monitors::Entity::insert_many(monitor_ids.iter().map(|&id| {
                status_page_maintenance_monitors::ActiveModel {
                    maintenance_id: Set(model.id),
                    monitor_id: Set(id),
                }
            }))
            .exec(&txn)
            .await?;
        }
        txn.commit().await?;

        info!(
            status_page_id,
            maintenance_id, "updated status page maintenance window"
        );
        let mut touched: BTreeSet<i32> = previous_monitors.into_iter().collect();
        touched.extend(monitor_ids.iter().copied());
        self.publish(status_page_id, &touched.into_iter().collect::<Vec<_>>())
            .await;
        Ok(Self::response(model, monitor_ids, now))
    }

    /// Cancel a scheduled window, or end one in progress now. Checks already
    /// taken during it stay out of uptime; from now on they count again.
    pub async fn cancel(
        &self,
        status_page_id: i32,
        maintenance_id: i32,
    ) -> Result<MaintenanceResponse, MaintenanceError> {
        let now = Utc::now();
        let txn = self.db.begin().await?;
        Self::ensure_page(&txn, status_page_id, true).await?;
        let model = Self::find(&txn, status_page_id, maintenance_id).await?;
        match maintenance_state(&model, now) {
            MaintenanceState::Cancelled => {
                return Err(MaintenanceError::Conflict {
                    maintenance_id,
                    reason: "it was already cancelled".to_string(),
                })
            }
            MaintenanceState::Completed => {
                return Err(MaintenanceError::Conflict {
                    maintenance_id,
                    reason: "it has already ended".to_string(),
                })
            }
            MaintenanceState::Scheduled | MaintenanceState::InProgress => {}
        }
        let mut active: status_page_maintenances::ActiveModel = model.into();
        active.cancelled_at = Set(Some(now));
        let model = active.update(&txn).await?;
        let monitor_ids = Self::monitors_of(&txn, &[model.id])
            .await?
            .remove(&model.id)
            .unwrap_or_default();
        txn.commit().await?;

        info!(
            status_page_id,
            maintenance_id, "cancelled status page maintenance window"
        );
        self.publish(status_page_id, &monitor_ids).await;
        Ok(Self::response(model, monitor_ids, now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(hour: u32) -> UtcDateTime {
        Utc.with_ymd_and_hms(2026, 3, 2, hour, 0, 0)
            .single()
            .expect("valid time")
    }

    fn model(starts: u32, ends: u32, cancelled: Option<u32>) -> status_page_maintenances::Model {
        status_page_maintenances::Model {
            id: 1,
            status_page_id: 2,
            title: "t".to_string(),
            description: None,
            starts_at: at(starts),
            ends_at: at(ends),
            cancelled_at: cancelled.map(at),
            created_at: at(0),
            updated_at: at(0),
        }
    }

    #[test]
    fn state_follows_the_clock_and_cancellation() {
        assert_eq!(
            maintenance_state(&model(10, 12, None), at(9)),
            MaintenanceState::Scheduled
        );
        assert_eq!(
            maintenance_state(&model(10, 12, None), at(10)),
            MaintenanceState::InProgress
        );
        assert_eq!(
            maintenance_state(&model(10, 12, None), at(12)),
            MaintenanceState::Completed
        );
        assert_eq!(
            maintenance_state(&model(10, 12, Some(11)), at(11)),
            MaintenanceState::Cancelled
        );
    }

    #[test]
    fn a_window_ends_after_it_starts_in_the_future_and_is_bounded() {
        let now = at(10);
        assert!(validate_window(2, at(11), at(12), now, false).is_ok());
        let err = validate_window(2, at(12), at(11), now, false).expect_err("reversed");
        assert!(err.to_string().contains("status page 2"), "{err}");
        assert!(err.to_string().contains("end after it starts"), "{err}");
        assert!(validate_window(2, at(11), at(11), now, false).is_err());
        assert!(validate_window(2, at(11), at(11) + Duration::days(8), now, false).is_err());
        assert!(validate_window(
            2,
            now + Duration::days(400),
            now + Duration::days(401),
            now,
            false
        )
        .is_err());
    }

    #[test]
    fn a_new_window_cannot_be_backdated_but_a_started_one_keeps_its_start() {
        let now = at(10);
        let err = validate_window(2, at(8), at(12), now, false).expect_err("backdated");
        assert!(err.to_string().contains("never erases"), "{err}");
        assert!(validate_window(2, now - Duration::minutes(4), at(12), now, false).is_ok());
        assert!(validate_window(2, at(8), at(12), now, true).is_ok());
        assert!(
            validate_window(2, at(8), at(9), now, true).is_err(),
            "a window must still end in the future"
        );
    }

    #[test]
    fn titles_and_descriptions_are_trimmed_and_bounded() {
        assert_eq!(validate_title(2, "  Upgrade ").expect("ok"), "Upgrade");
        assert!(validate_title(2, "   ").is_err());
        assert!(validate_title(2, &"x".repeat(121)).is_err());
        assert!(validate_title(2, "a\nb").is_err());
        assert_eq!(validate_description(2, Some("  ")).expect("ok"), None);
        assert_eq!(validate_description(2, None).expect("ok"), None);
        assert!(validate_description(2, Some(&"x".repeat(2001))).is_err());
    }
}
