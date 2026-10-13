// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Default bounds for every ClickHouse READ the OTel storage issues.
//!
//! `ClickHouseOtelStorage` builds its client without settings, so a read was
//! bounded only by whatever the server profile happened to say -- on a small
//! host that is a memory limit close to the machine's RAM, hit by the first
//! window that holds enough spans. The per-project trace list, the Observe
//! feed, span stats and metrics all share that exposure, so they all read
//! through a client carrying these bounds. Writes, migrations and health
//! checks keep the unbounded client: a batch insert or a `MATERIALIZE` is not
//! a read of a user-chosen window and must not inherit a read budget.
//!
//! The limits are deliberately a ceiling for the common case, not a tuning
//! surface: spilling keeps aggregation memory flat, so a read only reaches the
//! hard cap when something is genuinely unbounded, and then it fails with a
//! typed error naming the limit instead of taking the host's memory with it.
//! Reads that need to be tighter (the global traces page) set their own
//! per-query values on top; those win over the client's.
//!
//! These are *defaults for the query, not a floor under the server profile*: a
//! profile that sets a LOWER `max_memory_usage` is overridden by the value
//! here unless the profile pins it with a constraint. They are therefore kept
//! low enough to sit under any profile sized for a host that runs this stack.

use ::clickhouse::{error::Error as ChError, Client};
use std::time::Duration;

/// Hard per-query memory cap for any bounded read.
pub const READ_MAX_MEMORY_BYTES: u64 = 1 << 30;
/// Aggregation and sort state beyond this spills to disk instead of growing.
pub const READ_SPILL_BYTES: u64 = 256 << 20;
/// Server-side wall-clock budget for any bounded read.
pub const READ_MAX_EXECUTION_TIME: Duration = Duration::from_secs(60);

// A 4 GiB host's profile tops out around 3e9 bytes; the cap must sit below it
// or it would raise the limit it is meant to enforce, and the spill threshold
// must leave room for two spilling operators under the cap.
const _: () = assert!(READ_MAX_MEMORY_BYTES < 3_000_000_000);
const _: () = assert!(READ_SPILL_BYTES * 2 < READ_MAX_MEMORY_BYTES);

/// A copy of `client` that carries the read bounds. Cheap: `Client` is
/// reference-counted, and the copy shares its connection pool.
pub fn bounded_read_client(client: &Client) -> Client {
    client
        .clone()
        .with_setting("max_memory_usage", READ_MAX_MEMORY_BYTES.to_string())
        .with_setting(
            "max_bytes_before_external_group_by",
            READ_SPILL_BYTES.to_string(),
        )
        .with_setting(
            "max_bytes_before_external_sort",
            READ_SPILL_BYTES.to_string(),
        )
        .with_setting(
            "max_execution_time",
            READ_MAX_EXECUTION_TIME.as_secs().to_string(),
        )
}

/// A ClickHouse failure, reduced to the limits this crate sets itself.
#[derive(Debug, PartialEq)]
pub(crate) enum Failure {
    MemoryLimit,
    Timeout,
    Other,
}

pub(crate) fn classify(error: &ChError) -> Failure {
    if matches!(error, ChError::TimedOut) {
        return Failure::Timeout;
    }
    let text = error.to_string();
    if text.contains("MEMORY_LIMIT_EXCEEDED") || text.contains("Code: 241") {
        Failure::MemoryLimit
    } else if text.contains("TIMEOUT_EXCEEDED") || text.contains("Code: 159") {
        Failure::Timeout
    } else {
        Failure::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_production_memory_error_is_recognised() {
        let production = ChError::BadResponse(
            "Code: 241. DB::Exception: Query memory limit exceeded: would use 2.80 GiB \
             (attempt to allocate chunk of 4.05 MiB), maximum: 2.79 GiB: (while reading column \
             span_id): (MEMORY_LIMIT_EXCEEDED) (version 26.6)"
                .into(),
        );
        assert_eq!(classify(&production), Failure::MemoryLimit);
    }

    #[test]
    fn timeouts_and_other_failures_are_told_apart() {
        let server = ChError::BadResponse(
            "Code: 159. DB::Exception: Timeout exceeded: elapsed 60.1 seconds (TIMEOUT_EXCEEDED)"
                .into(),
        );
        assert_eq!(classify(&server), Failure::Timeout);
        assert_eq!(classify(&ChError::TimedOut), Failure::Timeout);
        assert_eq!(
            classify(&ChError::BadResponse("Code: 60. UNKNOWN_TABLE".into())),
            Failure::Other
        );
    }
}
