// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Contiguous windows during which a Cloud-primary project's spans were
//! genuinely not captured (ADR-041 §3d).
//!
//! # Why a gap is recorded rather than merely counted
//!
//! When the durable outbox reaches its byte cap the instance stops accepting
//! new spans for that project. Those spans are lost — that is a real, accepted
//! cost of opting a project into Cloud-primary writes, and the ADR says so
//! plainly. What is *not* acceptable is a hole nobody can see: a Traces page
//! with nothing in it between 14:05 and 14:40 reads as "nothing happened",
//! which is the exact opposite of what happened.
//!
//! So the cap records a window with a start, an end, a count and a reason. A
//! visible, bounded hole is honest and diagnosable. A silent one is a bug
//! report six weeks later that nobody can reconstruct.
//!
//! # Why the newest span is rejected, not the oldest evicted
//!
//! The in-memory [`Spool`](temps_cloud_client) drops the *oldest* on overflow,
//! which is right for a liveness mirror — during an incident the newest
//! telemetry is the useful telemetry, and local storage still holds everything.
//! For a primary path it is the worst possible artefact: some spans of a trace
//! ship and others do not, rendering a broken tree that looks like an
//! instrumentation bug rather than an outage. Rejecting at the boundary
//! produces one clean interval instead.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use temps_core::DBDateTime;

use super::project_telemetry_write_intervals::TelemetryWriteIntervalReason;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "telemetry_gap_windows")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub project_id: i32,
    /// When the first span was refused.
    pub started_at: DBDateTime,
    /// When the last span was refused. Extended in place while the gap is still
    /// open, so a 40-minute outage is one row rather than one row per batch —
    /// an unbounded row count during an outage would be its own incident.
    pub ended_at: DBDateTime,
    pub dropped_spans: i64,
    /// Bytes the refused spans would have occupied. Reported next to the cap so
    /// the operator can size the setting against what actually happened instead
    /// of guessing.
    pub dropped_bytes: i64,
    pub reason: TelemetryWriteIntervalReason,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

impl Model {
    /// Whether `from..to` overlaps this gap, used by the Traces page to decide
    /// whether the displayed range contains a hole.
    ///
    /// Half-open on neither end: a gap that ends exactly when the window starts
    /// did affect the boundary instant, and hiding it there would put the
    /// banner one second away from the data it explains.
    pub fn intersects(&self, from: DBDateTime, to: DBDateTime) -> bool {
        self.started_at <= to && self.ended_at >= from
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn gap(start_secs: i64, end_secs: i64) -> Model {
        Model {
            id: 1,
            project_id: 7,
            started_at: Utc.timestamp_opt(start_secs, 0).unwrap(),
            ended_at: Utc.timestamp_opt(end_secs, 0).unwrap(),
            dropped_spans: 10,
            dropped_bytes: 2048,
            reason: TelemetryWriteIntervalReason::QueueOverflowSpill,
        }
    }

    fn at(secs: i64) -> DBDateTime {
        Utc.timestamp_opt(secs, 0).unwrap()
    }

    #[test]
    fn a_gap_inside_the_window_intersects() {
        assert!(gap(100, 200).intersects(at(0), at(300)));
    }

    #[test]
    fn a_gap_entirely_before_or_after_the_window_does_not() {
        assert!(!gap(100, 200).intersects(at(300), at(400)));
        assert!(!gap(300, 400).intersects(at(100), at(200)));
    }

    #[test]
    fn a_gap_touching_the_window_boundary_still_intersects() {
        // The banner must not disappear one second away from the hole it
        // explains.
        assert!(gap(100, 200).intersects(at(200), at(400)));
        assert!(gap(200, 300).intersects(at(100), at(200)));
    }

    #[test]
    fn a_window_entirely_inside_a_gap_intersects() {
        assert!(gap(0, 1000).intersects(at(100), at(200)));
    }
}
