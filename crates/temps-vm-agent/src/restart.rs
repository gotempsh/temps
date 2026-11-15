// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Restart policy of a spawned workload process.
//!
//! In a container the runtime restarts a crashed main process; the workload
//! plane relies on that and deliberately leaves an exited long-running
//! replica alone. A microVM has no container runtime, so the guest agent —
//! the closest thing to the process — plays that role: it waits on the
//! process and, per the policy the host passed in [`crate::Request::Spawn`],
//! starts it again after an exponential backoff (Docker's model: the delay
//! doubles from [`RESTART_BACKOFF_INITIAL`] up to [`RESTART_BACKOFF_MAX`],
//! and resets once a run lasted [`RESTART_BACKOFF_RESET_AFTER`]).
//!
//! The decision and the backoff are pure so they are tested without a VM.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// First pause before restarting a process that exited.
pub const RESTART_BACKOFF_INITIAL: Duration = Duration::from_secs(1);

/// Longest pause between restarts, however often the process crashed.
pub const RESTART_BACKOFF_MAX: Duration = Duration::from_secs(60);

/// A run at least this long counts as healthy: the next crash starts the
/// backoff from [`RESTART_BACKOFF_INITIAL`] again.
pub const RESTART_BACKOFF_RESET_AFTER: Duration = Duration::from_secs(10);

/// Exit code recorded when a restart could not even start the process (the
/// binary vanished, `fork` failed): treated as a failure, like a shell's
/// "command not found".
pub const SPAWN_FAILED_EXIT_CODE: i32 = 127;

/// What the agent does when a spawned process exits. Absent from a request
/// (an older host) means [`SpawnRestartPolicy::Never`], the behaviour that
/// host expected.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "policy", rename_all = "snake_case")]
pub enum SpawnRestartPolicy {
    /// Run once.
    #[default]
    Never,
    /// Restart after every exit.
    Always,
    /// Restart after every exit until the host stops it. Inside one VM this
    /// is the same as [`SpawnRestartPolicy::Always`]: a deliberate stop is
    /// never restarted under either.
    UnlessStopped,
    /// Restart after a non-zero exit (or a kill by signal), at most
    /// `max_retries` times when set.
    OnFailure {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_retries: Option<u32>,
    },
}

/// Whether a process that exited with `exit_code`, after `restarts_so_far`
/// restarts, is started again. A deliberate stop (`stopped`) never is.
pub fn should_restart(
    policy: SpawnRestartPolicy,
    exit_code: i32,
    restarts_so_far: u32,
    stopped: bool,
) -> bool {
    if stopped {
        return false;
    }
    match policy {
        SpawnRestartPolicy::Never => false,
        SpawnRestartPolicy::Always | SpawnRestartPolicy::UnlessStopped => true,
        SpawnRestartPolicy::OnFailure { max_retries } => {
            exit_code != 0 && max_retries.is_none_or(|max| restarts_so_far < max)
        }
    }
}

/// Exponential restart backoff: 1s, 2s, 4s, … capped at 60s, reset after a
/// run that lasted [`RESTART_BACKOFF_RESET_AFTER`].
#[derive(Debug, Clone)]
pub struct RestartBackoff {
    initial: Duration,
    max: Duration,
    reset_after: Duration,
    consecutive: u32,
}

impl Default for RestartBackoff {
    fn default() -> Self {
        Self::new(
            RESTART_BACKOFF_INITIAL,
            RESTART_BACKOFF_MAX,
            RESTART_BACKOFF_RESET_AFTER,
        )
    }
}

impl RestartBackoff {
    /// A backoff with its own timings (tests shrink them to milliseconds).
    pub fn new(initial: Duration, max: Duration, reset_after: Duration) -> Self {
        Self {
            initial,
            max,
            reset_after,
            consecutive: 0,
        }
    }

    /// The pause before the next start, given how long the run that just
    /// ended lasted.
    pub fn next_delay(&mut self, uptime: Duration) -> Duration {
        if uptime >= self.reset_after {
            self.consecutive = 0;
        }
        let delay = self
            .initial
            .checked_mul(1u32.checked_shl(self.consecutive).unwrap_or(u32::MAX))
            .unwrap_or(self.max)
            .min(self.max);
        self.consecutive = self.consecutive.saturating_add(1);
        delay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_runs_once_and_always_restarts_every_exit() {
        for code in [0, 1, -1] {
            assert!(!should_restart(SpawnRestartPolicy::Never, code, 0, false));
            assert!(should_restart(SpawnRestartPolicy::Always, code, 0, false));
            assert!(should_restart(
                SpawnRestartPolicy::UnlessStopped,
                code,
                1_000,
                false
            ));
        }
    }

    #[test]
    fn on_failure_restarts_only_failures_up_to_its_retry_limit() {
        let unlimited = SpawnRestartPolicy::OnFailure { max_retries: None };
        assert!(!should_restart(unlimited, 0, 0, false), "clean exit");
        assert!(should_restart(unlimited, 1, 5_000, false));
        assert!(should_restart(unlimited, -1, 0, false), "killed by signal");

        let limited = SpawnRestartPolicy::OnFailure {
            max_retries: Some(2),
        };
        assert!(should_restart(limited, 3, 0, false));
        assert!(should_restart(limited, 3, 1, false));
        assert!(!should_restart(limited, 3, 2, false), "limit reached");
    }

    #[test]
    fn a_deliberate_stop_is_never_restarted() {
        for policy in [
            SpawnRestartPolicy::Never,
            SpawnRestartPolicy::Always,
            SpawnRestartPolicy::UnlessStopped,
            SpawnRestartPolicy::OnFailure { max_retries: None },
        ] {
            assert!(!should_restart(policy, 137, 0, true), "{policy:?}");
        }
    }

    #[test]
    fn the_backoff_doubles_to_its_cap_and_resets_after_a_healthy_run() {
        let mut backoff = RestartBackoff::default();
        let crash = Duration::from_millis(50);
        let delays: Vec<u64> = (0..9)
            .map(|_| backoff.next_delay(crash).as_secs())
            .collect();
        assert_eq!(delays, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
        for _ in 0..100 {
            assert_eq!(backoff.next_delay(crash), RESTART_BACKOFF_MAX);
        }
        assert_eq!(
            backoff.next_delay(RESTART_BACKOFF_RESET_AFTER),
            RESTART_BACKOFF_INITIAL
        );
        assert_eq!(backoff.next_delay(crash), Duration::from_secs(2));
    }

    #[test]
    fn the_policy_is_optional_on_the_wire_and_defaults_to_never() {
        assert_eq!(SpawnRestartPolicy::default(), SpawnRestartPolicy::Never);
        let json = serde_json::to_string(&SpawnRestartPolicy::OnFailure {
            max_retries: Some(3),
        })
        .unwrap();
        assert_eq!(json, r#"{"policy":"on_failure","max_retries":3}"#);
        let parsed: SpawnRestartPolicy =
            serde_json::from_str(r#"{"policy":"on_failure"}"#).unwrap();
        assert_eq!(parsed, SpawnRestartPolicy::OnFailure { max_retries: None });
        let parsed: SpawnRestartPolicy =
            serde_json::from_str(r#"{"policy":"unless_stopped"}"#).unwrap();
        assert_eq!(parsed, SpawnRestartPolicy::UnlessStopped);
    }
}
