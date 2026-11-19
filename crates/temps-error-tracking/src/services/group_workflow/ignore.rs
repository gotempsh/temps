// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! "Ignore until" conditions (ADR 074, D1.3): what keeps an ignored group
//! ignored, and how a status write records it.

use chrono::{DateTime, Duration, Utc};

use super::super::types::ErrorTrackingError;

/// Most additional occurrences an ignore may wait for.
pub const MAX_IGNORE_EVENT_COUNT: i32 = 1_000_000;
/// Most new distinct users an ignore may wait for. It also bounds the rows
/// kept per ignored group to count them.
pub const MAX_IGNORE_USER_COUNT: i32 = 10_000;
/// Furthest in the future an ignore may last by time.
pub const MAX_IGNORE_DAYS: i64 = 366;

/// When an ignored group comes back. With no field set, the next event
/// reopens it (the behaviour of a plain ignore). With fields set, the group
/// stays ignored until any one of them is met.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IgnoreConditions {
    /// Stay ignored until this time.
    pub until: Option<DateTime<Utc>>,
    /// Stay ignored for this many more occurrences.
    pub event_count: Option<i32>,
    /// Stay ignored until this many new distinct users are affected.
    pub user_count: Option<i32>,
    /// Stay ignored whatever happens. Excludes every other field.
    pub forever: bool,
}

impl IgnoreConditions {
    pub fn is_empty(&self) -> bool {
        self.until.is_none()
            && self.event_count.is_none()
            && self.user_count.is_none()
            && !self.forever
    }

    /// Check the conditions against `now`.
    pub fn validate(&self, now: DateTime<Utc>) -> Result<(), ErrorTrackingError> {
        if self.forever
            && (self.until.is_some() || self.event_count.is_some() || self.user_count.is_some())
        {
            return Err(ErrorTrackingError::Validation(
                "ignore.forever cannot be combined with until, event_count or user_count".into(),
            ));
        }
        if let Some(until) = self.until {
            if until <= now {
                return Err(ErrorTrackingError::Validation(format!(
                    "ignore.until {} is not in the future",
                    until.to_rfc3339()
                )));
            }
            if until > now + Duration::days(MAX_IGNORE_DAYS) {
                return Err(ErrorTrackingError::Validation(format!(
                    "ignore.until {} is more than {MAX_IGNORE_DAYS} days away; use forever instead",
                    until.to_rfc3339()
                )));
            }
        }
        if let Some(count) = self.event_count {
            if !(1..=MAX_IGNORE_EVENT_COUNT).contains(&count) {
                return Err(ErrorTrackingError::Validation(format!(
                    "ignore.event_count {count} must be between 1 and {MAX_IGNORE_EVENT_COUNT}"
                )));
            }
        }
        if let Some(count) = self.user_count {
            if !(1..=MAX_IGNORE_USER_COUNT).contains(&count) {
                return Err(ErrorTrackingError::Validation(format!(
                    "ignore.user_count {count} must be between 1 and {MAX_IGNORE_USER_COUNT}"
                )));
            }
        }
        Ok(())
    }

    /// Validate `conditions` for a write to `status`: conditions only make
    /// sense on `ignored`.
    pub fn check_for_status(
        conditions: Option<&IgnoreConditions>,
        status: &str,
        now: DateTime<Utc>,
    ) -> Result<(), ErrorTrackingError> {
        let Some(conditions) = conditions else {
            return Ok(());
        };
        if status != "ignored" && !conditions.is_empty() {
            return Err(ErrorTrackingError::Validation(format!(
                "ignore conditions apply only to status 'ignored', not '{status}'"
            )));
        }
        conditions.validate(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conditions_are_bounded_and_exclusive_with_forever() {
        let now = Utc::now();
        assert!(IgnoreConditions::default().validate(now).is_ok());
        assert!(IgnoreConditions {
            until: Some(now + Duration::days(7)),
            event_count: Some(100),
            user_count: Some(10),
            forever: false,
        }
        .validate(now)
        .is_ok());
        for bad in [
            IgnoreConditions {
                until: Some(now - Duration::seconds(1)),
                ..Default::default()
            },
            IgnoreConditions {
                until: Some(now + Duration::days(400)),
                ..Default::default()
            },
            IgnoreConditions {
                event_count: Some(0),
                ..Default::default()
            },
            IgnoreConditions {
                user_count: Some(MAX_IGNORE_USER_COUNT + 1),
                ..Default::default()
            },
            IgnoreConditions {
                forever: true,
                event_count: Some(5),
                ..Default::default()
            },
        ] {
            assert!(bad.validate(now).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn conditions_only_go_with_the_ignored_status() {
        let now = Utc::now();
        let conditions = IgnoreConditions {
            forever: true,
            ..Default::default()
        };
        assert!(IgnoreConditions::check_for_status(Some(&conditions), "ignored", now).is_ok());
        assert!(IgnoreConditions::check_for_status(Some(&conditions), "resolved", now).is_err());
        assert!(IgnoreConditions::check_for_status(None, "resolved", now).is_ok());
    }
}
