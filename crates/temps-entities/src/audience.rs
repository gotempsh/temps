// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Who an address is for (ADR 064), as stored on `project_custom_domains` and
//! `environment_domains`. See `m20261024_000001_reachability`.

/// Everyone on the internet.
pub const PUBLIC: &str = "public";
/// Only devices on the operator's private network (a tailnet or a LAN),
/// served by a worker's private listener (ADR 064 D5).
pub const PRIVATE: &str = "private";
/// Anyone who signs in to Temps. Reserved for phase 4.
pub const SIGNED_IN: &str = "signed_in";

/// Every value the database accepts.
pub const ALL: [&str; 3] = [PUBLIC, PRIVATE, SIGNED_IN];

/// Serde default for rows serialized before the column existed.
pub fn default_audience() -> String {
    PUBLIC.to_string()
}
