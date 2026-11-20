// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/// A target or check setting that cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CheckConfigError {
    #[error("invalid {field}: {reason}")]
    Invalid { field: &'static str, reason: String },
}

#[derive(Debug, thiserror::Error)]
pub enum TargetMonitorError {
    #[error("database error on target monitors: {0}")]
    Database(#[from] sea_orm::DbErr),
    #[error("project {project_id} was not found")]
    ProjectNotFound { project_id: i32 },
    #[error("monitor {monitor_id} was not found")]
    NotFound { monitor_id: i32 },
    #[error(
        "monitor {monitor_id} checks a Temps environment; change it from its environment instead"
    )]
    NotTargetMonitor { monitor_id: i32 },
    #[error("monitor in project {project_id}: {source}")]
    Config {
        project_id: i32,
        #[source]
        source: CheckConfigError,
    },
    #[error("monitor in project {project_id} cannot check {target}: {reason}")]
    NonPublicTarget {
        project_id: i32,
        target: String,
        reason: String,
    },
    #[error("could not encrypt the request of a monitor in project {project_id}: {reason}")]
    Encryption { project_id: i32, reason: String },
    #[error("stored check of monitor {monitor_id} is unreadable: {reason}")]
    StoredCheck { monitor_id: i32, reason: String },
    #[error("project {project_id} already has {limit} monitors for targets outside Temps")]
    TooMany { project_id: i32, limit: u64 },
    #[error(
        "monitor {monitor_id} kept changing while it was being saved; reload it and try again"
    )]
    Concurrent { monitor_id: i32 },
}
