// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The in-memory set of inbound filter rules ingest reads (ADR 111, D1.4).
//!
//! Ingest evaluates every Sentry error event against its project's rules.
//! That path takes no lock and makes no database call: the rules live in an
//! immutable snapshot behind an `ArcSwap`, loaded once and replaced whole
//! when a rule changes (and on a timer, for rule writes made by another
//! process). A drop increments the matching rule's atomic counter; a
//! background flush adds the counters to `error_inbound_filters.dropped_count`
//! in one statement. Counters survive a reload because the new snapshot
//! reuses the old rule's counter by id.
//!
//! Memory is bounded by the rules themselves: at most
//! [`super::MAX_FILTERS_PER_PROJECT`] per project, each holding one pattern of
//! at most [`super::matcher::MAX_PATTERN_CHARS`] characters.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use arc_swap::{ArcSwap, ArcSwapOption};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};

use super::matcher::{InboundFilterKind, Matcher};
use crate::services::types::{CreateErrorEventData, ErrorTrackingError};

/// One rule ready to evaluate, with the counter of what it dropped since the
/// last flush.
#[derive(Debug)]
struct CompiledRule {
    id: i32,
    matcher: Matcher,
    dropped: Arc<AtomicU64>,
}

type Snapshot = HashMap<i32, Arc<Vec<CompiledRule>>>;

/// Lock-free inbound filters for every project.
#[derive(Default)]
pub struct InboundFilterRegistry {
    snapshot: ArcSwap<Snapshot>,
    /// Row count and newest `updated_at` the snapshot was built from, so the
    /// periodic refresh reloads only when the table changed.
    version: ArcSwapOption<(i64, Option<chrono::DateTime<chrono::Utc>>)>,
}

impl InboundFilterRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The id of the first enabled rule of the event's project that drops
    /// it, counting the drop; `None` when the event is to be stored.
    pub fn evaluate(&self, event: &CreateErrorEventData) -> Option<i32> {
        let snapshot = self.snapshot.load();
        let rules = snapshot.get(&event.project_id)?;
        let rule = rules.iter().find(|rule| rule.matcher.matches(event))?;
        rule.dropped.fetch_add(1, Ordering::Relaxed);
        Some(rule.id)
    }

    /// Reload when a rule was written since the last load, possibly by
    /// another process. One aggregate query when nothing changed.
    pub async fn refresh_if_changed(
        &self,
        db: &DatabaseConnection,
    ) -> Result<bool, ErrorTrackingError> {
        let current = Self::table_version(db).await?;
        if self.version.load().as_deref() == Some(&current) {
            return Ok(false);
        }
        self.reload(db).await?;
        Ok(true)
    }

    async fn table_version(
        db: &DatabaseConnection,
    ) -> Result<(i64, Option<chrono::DateTime<chrono::Utc>>), ErrorTrackingError> {
        let row = db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT COUNT(*) AS rules, MAX(updated_at) AS newest FROM error_inbound_filters",
            ))
            .await?
            .ok_or_else(|| {
                ErrorTrackingError::Validation(
                    "inbound filter version query returned no row".into(),
                )
            })?;
        Ok((row.try_get("", "rules")?, row.try_get("", "newest")?))
    }

    /// Replace the snapshot with every enabled rule in the database.
    pub async fn reload(&self, db: &DatabaseConnection) -> Result<(), ErrorTrackingError> {
        let version = Self::table_version(db).await?;
        let rows = db
            .query_all(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT id, project_id, kind, pattern FROM error_inbound_filters \
                 WHERE enabled ORDER BY project_id, id",
            ))
            .await?;
        let mut rules = Vec::with_capacity(rows.len());
        for row in rows {
            let id: i32 = row.try_get("", "id")?;
            let project_id: i32 = row.try_get("", "project_id")?;
            let kind: String = row.try_get("", "kind")?;
            let pattern: Option<String> = row.try_get("", "pattern")?;
            rules.push((id, project_id, kind, pattern));
        }
        self.replace(rules);
        self.version.store(Some(Arc::new(version)));
        Ok(())
    }

    /// Build and swap in a snapshot from `(id, project_id, kind, pattern)`
    /// rows, keeping the counters of rules that are still there.
    fn replace(&self, rows: Vec<(i32, i32, String, Option<String>)>) {
        let previous = self.snapshot.load();
        let counters: HashMap<i32, Arc<AtomicU64>> = previous
            .values()
            .flat_map(|rules| rules.iter())
            .map(|rule| (rule.id, rule.dropped.clone()))
            .collect();
        let mut next: HashMap<i32, Vec<CompiledRule>> = HashMap::new();
        for (id, project_id, kind, pattern) in rows {
            let Some(kind) = InboundFilterKind::parse(&kind) else {
                tracing::warn!(
                    rule_id = id,
                    kind,
                    "Skipping inbound filter of an unknown kind"
                );
                continue;
            };
            let Some(matcher) = Matcher::compile(kind, pattern.as_deref()) else {
                tracing::warn!(
                    rule_id = id,
                    "Skipping inbound filter without a usable pattern"
                );
                continue;
            };
            next.entry(project_id).or_default().push(CompiledRule {
                id,
                matcher,
                dropped: counters.get(&id).cloned().unwrap_or_default(),
            });
        }
        self.snapshot.store(Arc::new(
            next.into_iter()
                .map(|(project, rules)| (project, Arc::new(rules)))
                .collect(),
        ));
    }

    /// Add the drops counted since the last flush to the stored counters.
    /// A failed write puts the counts back, so they are retried next time.
    pub async fn flush(&self, db: &DatabaseConnection) -> Result<u64, ErrorTrackingError> {
        let snapshot = self.snapshot.load();
        let mut ids = Vec::new();
        let mut counts = Vec::new();
        let mut taken = Vec::new();
        for rule in snapshot.values().flat_map(|rules| rules.iter()) {
            let count = rule.dropped.swap(0, Ordering::Relaxed);
            if count > 0 {
                ids.push(rule.id);
                counts.push(i64::try_from(count).unwrap_or(i64::MAX));
                taken.push((rule.dropped.clone(), count));
            }
        }
        if ids.is_empty() {
            return Ok(0);
        }
        let total: u64 = taken.iter().map(|(_, count)| count).sum();
        let result = db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE error_inbound_filters f SET \
                   dropped_count = f.dropped_count + d.count, last_dropped_at = NOW() \
                 FROM UNNEST($1::int[], $2::bigint[]) AS d(id, count) WHERE f.id = d.id",
                [ids.into(), counts.into()],
            ))
            .await;
        if let Err(error) = result {
            for (counter, count) in taken {
                counter.fetch_add(count, Ordering::Relaxed);
            }
            return Err(error.into());
        }
        Ok(total)
    }

    /// Drops counted in memory and not flushed yet, per rule id. Lets a read
    /// of the rules show an up-to-date count.
    pub fn pending_drops(&self) -> HashMap<i32, u64> {
        self.snapshot
            .load()
            .values()
            .flat_map(|rules| rules.iter())
            .map(|rule| (rule.id, rule.dropped.load(Ordering::Relaxed)))
            .filter(|(_, count)| *count > 0)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<(i32, i32, String, Option<String>)> {
        vec![
            (1, 10, "release".into(), Some("0.9.*".into())),
            (2, 10, "localhost".into(), None),
            (
                3,
                20,
                "error_message".into(),
                Some("*ResizeObserver*".into()),
            ),
            (4, 20, "unknown_kind".into(), None),
        ]
    }

    #[test]
    fn evaluation_is_per_project_and_counts_each_drop() {
        let registry = InboundFilterRegistry::new();
        registry.replace(rows());

        let mut event = CreateErrorEventData {
            project_id: 10,
            release_version: Some("0.9.4".into()),
            ..Default::default()
        };
        assert_eq!(registry.evaluate(&event), Some(1));
        assert_eq!(registry.evaluate(&event), Some(1));
        event.release_version = Some("1.0.0".into());
        assert_eq!(registry.evaluate(&event), None);

        // Same release, another project with no release rule.
        event.project_id = 20;
        event.release_version = Some("0.9.4".into());
        assert_eq!(registry.evaluate(&event), None);
        event.project_id = 30;
        assert_eq!(registry.evaluate(&event), None, "no rules, nothing dropped");

        assert_eq!(registry.pending_drops(), HashMap::from([(1, 2)]));
    }

    #[test]
    fn a_reload_keeps_the_counts_of_rules_that_remain() {
        let registry = InboundFilterRegistry::new();
        registry.replace(rows());
        let event = CreateErrorEventData {
            project_id: 10,
            release_version: Some("0.9.1".into()),
            ..Default::default()
        };
        registry.evaluate(&event);
        // Rule 2 is deleted; rule 1 changes its pattern but keeps its id.
        registry.replace(vec![(1, 10, "release".into(), Some("0.*".into()))]);
        assert_eq!(registry.pending_drops(), HashMap::from([(1, 1)]));
        registry.evaluate(&event);
        assert_eq!(registry.pending_drops(), HashMap::from([(1, 2)]));
    }
}
