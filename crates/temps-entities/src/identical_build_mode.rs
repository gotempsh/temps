// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! How a project's builds coordinate with identical builds (ADR 113).
//!
//! Several projects, or several environments of one project, often build the
//! same image: same repository, same commit, same build context and
//! Dockerfile, same build-time values. Stored on the project row
//! (`projects.identical_build_mode`, `projects.identical_build_scope`) so an
//! operator changes it per project at runtime, with an audit entry, and no
//! restart.

use sea_orm::{DeriveActiveEnum, EnumIter};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// How long after it succeeded a share build's image is offered to identical
/// builds that start later.
pub const REUSE_WINDOW_SECS: u64 = 30 * 60;

/// The longest a build waits for its group before it fails with a clear
/// reason. Longer than the worker build timeout's default (30 minutes), so a
/// few queued builds do not time out.
pub const MAX_WAIT_SECS: u64 = 90 * 60;

/// What a build does when an identical build is already running.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    ToSchema,
    DeriveActiveEnum,
    EnumIter,
    Default,
)]
#[sea_orm(rs_type = "String", db_type = "Text")]
#[serde(rename_all = "snake_case")]
pub enum IdenticalBuildMode {
    /// Every build runs on its own, at the same time as any other. The
    /// default for every existing and new project: what Temps always did.
    #[default]
    #[sea_orm(string_value = "parallel")]
    Parallel,

    /// Builds in the same group run one at a time, oldest first. Each still
    /// builds its own image.
    #[sea_orm(string_value = "queue")]
    Queue,

    /// Builds in the same group run one at a time, and a build whose full
    /// fingerprint matches one that just succeeded in another `share` project
    /// with the same access deploys that image instead of building again.
    #[sea_orm(string_value = "share")]
    Share,
}

impl IdenticalBuildMode {
    pub const ALL: [IdenticalBuildMode; 3] = [
        IdenticalBuildMode::Parallel,
        IdenticalBuildMode::Queue,
        IdenticalBuildMode::Share,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            IdenticalBuildMode::Parallel => "parallel",
            IdenticalBuildMode::Queue => "queue",
            IdenticalBuildMode::Share => "share",
        }
    }

    /// Whether builds of this project take part in coordination at all.
    pub fn coordinates(self) -> bool {
        !matches!(self, IdenticalBuildMode::Parallel)
    }

    /// Whether this project may deploy an image another build produced, and
    /// offer the images it builds to others.
    pub fn shares_images(self) -> bool {
        matches!(self, IdenticalBuildMode::Share)
    }
}

impl std::fmt::Display for IdenticalBuildMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which builds count as "the same group" for queueing.
///
/// The scope only ever widens queueing. Reusing an image always requires the
/// full fingerprint to match, whatever the scope.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    ToSchema,
    DeriveActiveEnum,
    EnumIter,
    Default,
)]
#[sea_orm(rs_type = "String", db_type = "Text")]
#[serde(rename_all = "snake_case")]
pub enum IdenticalBuildScope {
    /// Only builds with the same full fingerprint (repository, commit, build
    /// context, Dockerfile, preset, platform, build-time values) wait for
    /// each other.
    #[sea_orm(string_value = "fingerprint")]
    Fingerprint,

    /// Every build of the same repository and build path waits for the
    /// others, whatever the commit or build-time values: at most one build of
    /// that source runs at a time. The default: builds of one repository and
    /// path never run side by side once a project coordinates.
    #[default]
    #[sea_orm(string_value = "source")]
    Source,
}

impl IdenticalBuildScope {
    /// The default first, as the console and CLI list them.
    pub const ALL: [IdenticalBuildScope; 2] = [
        IdenticalBuildScope::Source,
        IdenticalBuildScope::Fingerprint,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            IdenticalBuildScope::Fingerprint => "fingerprint",
            IdenticalBuildScope::Source => "source",
        }
    }
}

impl std::fmt::Display for IdenticalBuildScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where one coordinated build is (`identical_builds.state`).
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    ToSchema,
    DeriveActiveEnum,
    EnumIter,
)]
#[sea_orm(rs_type = "String", db_type = "Text")]
#[serde(rename_all = "snake_case")]
pub enum IdenticalBuildState {
    /// Waiting for another build of its group to finish.
    #[sea_orm(string_value = "waiting")]
    Waiting,
    /// Building; at most one row per group is in this state (partial unique
    /// index), and it proves it is alive with `heartbeat_at`.
    #[sea_orm(string_value = "building")]
    Building,
    /// Built its image; `image` describes it.
    #[sea_orm(string_value = "succeeded")]
    Succeeded,
    /// The build failed; the next waiting build of the group builds instead.
    #[sea_orm(string_value = "failed")]
    Failed,
    /// The deployment was cancelled while waiting or building.
    #[sea_orm(string_value = "cancelled")]
    Cancelled,
    /// Deployed the image of `source_build_id` instead of building.
    #[sea_orm(string_value = "reused")]
    Reused,
    /// Gave up waiting after the bounded wait.
    #[sea_orm(string_value = "timed_out")]
    TimedOut,
    /// A building row whose process stopped proving it was alive (a crash or
    /// restart); a waiting build took over.
    #[sea_orm(string_value = "abandoned")]
    Abandoned,
}

impl IdenticalBuildState {
    pub fn as_str(self) -> &'static str {
        match self {
            IdenticalBuildState::Waiting => "waiting",
            IdenticalBuildState::Building => "building",
            IdenticalBuildState::Succeeded => "succeeded",
            IdenticalBuildState::Failed => "failed",
            IdenticalBuildState::Cancelled => "cancelled",
            IdenticalBuildState::Reused => "reused",
            IdenticalBuildState::TimedOut => "timed_out",
            IdenticalBuildState::Abandoned => "abandoned",
        }
    }

    /// Whether the row is finished and no longer takes part in coordination.
    pub fn is_final(self) -> bool {
        !matches!(
            self,
            IdenticalBuildState::Waiting | IdenticalBuildState::Building
        )
    }
}

impl std::fmt::Display for IdenticalBuildState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_projects_keep_building_in_parallel() {
        assert_eq!(IdenticalBuildMode::default(), IdenticalBuildMode::Parallel);
        assert!(!IdenticalBuildMode::default().coordinates());
        assert_eq!(IdenticalBuildScope::default(), IdenticalBuildScope::Source);
        assert_eq!(IdenticalBuildScope::ALL[0], IdenticalBuildScope::default());
    }

    #[test]
    fn only_share_offers_and_reuses_images() {
        assert!(IdenticalBuildMode::Share.shares_images());
        assert!(!IdenticalBuildMode::Queue.shares_images());
        assert!(!IdenticalBuildMode::Parallel.shares_images());
        assert!(IdenticalBuildMode::Queue.coordinates());
        assert!(IdenticalBuildMode::Share.coordinates());
    }

    #[test]
    fn display_and_serde_match_the_stored_values() {
        for mode in IdenticalBuildMode::ALL {
            assert_eq!(
                serde_json::to_string(&mode).expect("serialize"),
                format!("\"{mode}\"")
            );
        }
        for scope in IdenticalBuildScope::ALL {
            assert_eq!(
                serde_json::to_string(&scope).expect("serialize"),
                format!("\"{scope}\"")
            );
        }
        assert_eq!(IdenticalBuildState::TimedOut.to_string(), "timed_out");
        assert!(!IdenticalBuildState::Building.is_final());
        assert!(!IdenticalBuildState::Waiting.is_final());
        assert!(IdenticalBuildState::Abandoned.is_final());
    }
}
