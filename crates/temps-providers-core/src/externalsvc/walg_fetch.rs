// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! How a `wal-g backup-fetch` selects its backup. Shared by every engine that
//! restores with WAL-G, so none of them passes the selected backup's user data
//! under a name WAL-G reads as its own setting.

/// The environment variable a WAL-G fetch script reads the selected backup's
/// user data from. Deliberately not `WALG_FETCH_TARGET_USER_DATA`: WAL-G reads
/// that name as its own setting, and then refuses a fetch that also names a
/// backup ("Specify target backup name OR target userdata, not both").
pub const FETCH_TARGET_USER_DATA_ENV: &str = "TEMPS_FETCH_TARGET_USER_DATA";

/// How a `wal-g backup-fetch` picks its backup: exactly one of the exact base
/// backup, the backup tagged with the given user data, or the latest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalgFetchSelector<'a> {
    /// An exact base backup name, already validated by the caller (for
    /// PostgreSQL, `walg_snapshot`).
    Snapshot(&'a str),
    /// The backup whose user data equals this value.
    UserData(&'a str),
    Latest,
}

impl<'a> WalgFetchSelector<'a> {
    /// An exact snapshot wins over user data: both identify the same backup,
    /// and WAL-G accepts only one of them.
    pub fn new(snapshot: Option<&'a str>, target_user_data: Option<&'a str>) -> Self {
        match (snapshot, target_user_data) {
            (Some(name), _) => Self::Snapshot(name),
            (None, Some(user_data)) => Self::UserData(user_data),
            (None, None) => Self::Latest,
        }
    }

    /// The `backup-fetch` argument after the destination directory. User
    /// data is read from [`FETCH_TARGET_USER_DATA_ENV`], never interpolated.
    pub fn fetch_argument(&self) -> String {
        match self {
            Self::Snapshot(name) => (*name).to_string(),
            Self::UserData(_) => format!("--target-user-data \"${FETCH_TARGET_USER_DATA_ENV}\""),
            Self::Latest => "LATEST".to_string(),
        }
    }

    /// The `NAME=value` environment entry the fetch argument reads, if any,
    /// for an exec environment (no shell quoting).
    pub fn env_entry(&self) -> Option<String> {
        self.user_data()
            .map(|user_data| format!("{FETCH_TARGET_USER_DATA_ENV}={user_data}"))
    }

    /// The user data the fetch argument reads from the environment, if any.
    pub fn user_data(&self) -> Option<&'a str> {
        match self {
            Self::UserData(user_data) => Some(user_data),
            Self::Snapshot(_) | Self::Latest => None,
        }
    }
}
