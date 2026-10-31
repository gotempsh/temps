// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Classifying failed image builds: whether a step ran out of memory, and
//! whether that can be attributed to this build.

use super::DockerRuntime;
use crate::{BuildMemoryDiagnosis, BuilderError, OomAttribution};
use sysinfo::System;

/// Readings taken when a build starts, for attributing a later failure.
#[derive(Debug, Clone, Copy)]
pub(super) struct BuildStartSample {
    /// Kernel OOM-kill counter, when the daemon is on this host.
    oom_kills: Option<u64>,
    /// Boot-relative clock, when the daemon is on this host.
    uptime_us: Option<u64>,
    /// Other builds holding a permit at that moment (`None`: no semaphore).
    pub(super) others_at_start: Option<usize>,
    /// `builds_started` at that moment.
    builds_started: u64,
}

/// Kernel log timestamps and `/proc/uptime` come from different clocks and
/// `/proc/uptime` has 10 ms resolution; a kill logged this much after the
/// observed failure still counts as before it.
const CLOCK_SLOP_US: u64 = 1_000_000;

/// Everything the host could tell about a failed step, gathered after it.
#[derive(Debug, Clone, Default)]
struct MemorySignals {
    /// Kernel OOM kills on the host during the build (`None`: unavailable).
    oom_kills: Option<u64>,
    /// Victims named by the kernel log (`None`: log not readable).
    victims: Option<Vec<OomVictim>>,
    /// Other builds that overlapped this one at any point (`None`: unknown).
    other_builds: Option<usize>,
    /// When the step's failure was observed, microseconds since boot.
    failed_at_us: Option<u64>,
}

/// Outcome of looking at a failed step through `MemorySignals`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MemoryVerdict {
    /// The step ran out of memory; carry the facts.
    OutOfMemory(BuildMemoryDiagnosis),
    /// Something was killed on the host but it cannot be pinned on this
    /// build; the string is a note for the build log.
    Unattributed(String),
    /// No memory signal at all.
    NotMemory,
}

/// Exit status of a build step whose process was killed with SIGKILL, which
/// is how the kernel's OOM killer and a memory cgroup limit end a process.
const SIGKILL_EXIT_CODE: i32 = 137;

/// Whether a `DOCKER_HOST` value points at the daemon on this machine.
/// Unset means the default local socket, as it does for bollard.
pub(super) fn docker_host_is_local(docker_host: Option<&str>) -> bool {
    match docker_host.map(str::trim) {
        None | Some("") => true,
        Some(host) => host.starts_with("unix://") || host.starts_with("npipe://"),
    }
}

/// The kernel's cumulative OOM-kill counter from `/proc/vmstat` text
/// (`oom_kill`, Linux 4.13 and later).
fn parse_oom_kill_count(vmstat: &str) -> Option<u64> {
    vmstat
        .lines()
        .find_map(|line| line.strip_prefix("oom_kill "))
        .and_then(|value| value.trim().parse().ok())
}

/// Processes the kernel's OOM killer has terminated on this host since boot,
/// or `None` where the counter cannot be read.
fn host_oom_kill_count() -> Option<u64> {
    std::fs::read_to_string("/proc/vmstat")
        .ok()
        .as_deref()
        .and_then(parse_oom_kill_count)
}

/// A process the kernel's OOM killer terminated, from the kernel log.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OomVictim {
    /// Command name (`task=` in the kernel's `oom-kill:` line).
    comm: String,
    /// Memory cgroup of the victim (`task_memcg=`).
    cgroup: String,
    /// When the kernel logged the kill, microseconds since boot.
    at_us: u64,
}

/// How long after an OOM kill a build step is still considered its victim
/// when other builds were running. A killed child ends its parent build tool
/// within milliseconds; the window leaves room for slow shutdown paths.
const OOM_KILL_ATTRIBUTION_WINDOW_US: u64 = 10 * 1_000_000;

/// Whether a memory cgroup path belongs to a BuildKit build step rather than
/// a container. dockerd runs BuildKit steps under its default cgroup parent
/// with BuildKit's own exec id: `.../system.slice:docker:<id>` with the
/// systemd driver, `/docker/<id>` with cgroupfs. Containers are
/// `docker-<64 hex>.scope` or `/docker/<64 hex>`.
fn cgroup_is_build_step(cgroup: &str) -> bool {
    if cgroup.contains(":docker:") {
        return true;
    }
    let is_container_id =
        |segment: &str| segment.len() == 64 && segment.chars().all(|c| c.is_ascii_hexdigit());
    match cgroup.rsplit_once("/docker/") {
        Some((_, id)) => !id.is_empty() && !id.contains('/') && !is_container_id(id),
        None => false,
    }
}

/// Seconds since boot from `/proc/uptime`, in microseconds, the clock the
/// kernel log timestamps its records with.
fn uptime_us() -> Option<u64> {
    let uptime = std::fs::read_to_string("/proc/uptime").ok()?;
    let seconds: f64 = uptime.split_whitespace().next()?.parse().ok()?;
    Some((seconds * 1_000_000.0) as u64)
}

/// OOM victims named in `/dev/kmsg` records at or after `since_us`. Each
/// record is `prio,seq,timestamp_us,flags;message`; continuation lines
/// start with a space and carry no timestamp.
fn parse_oom_victims(kmsg: &str, since_us: u64) -> Vec<OomVictim> {
    kmsg.lines()
        .filter_map(|line| {
            let (header, message) = line.split_once(';')?;
            let timestamp: u64 = header.split(',').nth(2)?.trim().parse().ok()?;
            if timestamp < since_us || !message.starts_with("oom-kill:") {
                return None;
            }
            let field = |key: &str| {
                message
                    .split(',')
                    .find_map(|part| part.strip_prefix(key))
                    .map(str::to_string)
            };
            Some(OomVictim {
                comm: field("task=")?,
                cgroup: field("task_memcg=")?,
                at_us: timestamp,
            })
        })
        .collect()
}

/// The kernel log, when this process may read it (`/dev/kmsg` needs
/// CAP_SYSLOG or `dmesg_restrict=0`). Non-blocking, one record per read.
#[cfg(target_os = "linux")]
fn read_kernel_log() -> Option<String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    const O_NONBLOCK: i32 = 0o4000;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(O_NONBLOCK)
        .open("/dev/kmsg")
        .ok()?;
    let mut log = String::new();
    let mut record = [0u8; 8192];
    loop {
        match file.read(&mut record) {
            Ok(0) => break,
            Ok(n) => log.push_str(&String::from_utf8_lossy(&record[..n])),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(_) => break,
        }
    }
    Some(log)
}

#[cfg(not(target_os = "linux"))]
fn read_kernel_log() -> Option<String> {
    None
}

/// The message a failed build stream carries, without bollard's
/// `Docker stream error:` wrapper, which tells the user nothing.
pub(super) fn build_stream_error_text(error: &bollard::errors::Error) -> String {
    match error {
        bollard::errors::Error::DockerStreamError { error } => error.clone(),
        other => other.to_string(),
    }
}

/// The exit status a builder error reports for a failed step. BuildKit
/// writes `exit code: N`; the legacy builder writes `returned a non-zero
/// code: N`.
fn build_step_exit_code(error_text: &str) -> Option<i32> {
    ["exit code: ", "returned a non-zero code: "]
        .iter()
        .find_map(|marker| {
            let start = error_text.rfind(marker)? + marker.len();
            let digits: String = error_text[start..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            digits.parse().ok()
        })
}

impl DockerRuntime {
    /// Decide whether a failed build step ran out of memory, from the
    /// builder's error text and the host's OOM-kill counter sampled before the
    /// build. A kernel kill during the build is conclusive. The SIGKILL exit
    /// status alone counts only when the counter is unavailable (remote
    /// daemon, or no `/proc/vmstat`): when the counter is readable and did
    /// not move, the kill came from something else.
    fn diagnose_out_of_memory(
        &self,
        error_text: &str,
        start: &BuildStartSample,
        requested_cap_bytes: i64,
    ) -> MemoryVerdict {
        let oom_kills = match (start.oom_kills, host_oom_kill_count()) {
            (Some(b), Some(a)) => Some(a.saturating_sub(b)),
            _ => None,
        };
        // Only read the kernel log when something was killed; it is a
        // privileged read and the ring buffer can be large.
        let victims = match (oom_kills, start.uptime_us) {
            (Some(kills), Some(since)) if kills > 0 => {
                read_kernel_log().map(|log| parse_oom_victims(&log, since))
            }
            _ => None,
        };
        let signals = MemorySignals {
            oom_kills,
            victims,
            other_builds: self.other_builds_since(start),
            failed_at_us: self.daemon_is_local.then(uptime_us).flatten(),
        };
        self.diagnose_out_of_memory_with(error_text, &signals, requested_cap_bytes)
    }

    /// [`Self::diagnose_out_of_memory`] with the host signals supplied.
    ///
    /// A kill is attributed to this build when the step's own process was
    /// killed (exit 137), or when the kernel log names a build step's process
    /// killed within [`OOM_KILL_ATTRIBUTION_WINDOW_US`] of this step's
    /// failure (a killed child ends its parent tool within milliseconds):
    /// as fact when no other build overlapped this one, as "most likely"
    /// otherwise. With the log unreadable, a kill while this was the only
    /// build is "most likely" too. Anything else, including a build-step
    /// kill long before this failure, is a note in the build log.
    fn diagnose_out_of_memory_with(
        &self,
        error_text: &str,
        signals: &MemorySignals,
        requested_cap_bytes: i64,
    ) -> MemoryVerdict {
        let exit_code = build_step_exit_code(error_text);
        let step_killed = exit_code == Some(SIGKILL_EXIT_CODE);
        let kills = signals.oom_kills;
        // The latest build-step victim killed before this step failed
        // (allowing for the clocks not agreeing exactly).
        let build_victim = signals.victims.as_ref().and_then(|victims| {
            victims
                .iter()
                .filter(|v| cgroup_is_build_step(&v.cgroup))
                .filter(|v| {
                    signals
                        .failed_at_us
                        .is_none_or(|t| v.at_us <= t.saturating_add(CLOCK_SLOP_US))
                })
                .max_by_key(|v| v.at_us)
        });
        let other_victim = signals
            .victims
            .as_ref()
            .and_then(|victims| victims.first())
            .filter(|_| build_victim.is_none());
        let alone = signals.other_builds == Some(0);
        let since_kill_us = build_victim
            .zip(signals.failed_at_us)
            .map(|(v, t)| t.saturating_sub(v.at_us));
        // With the clock known, only a kill shortly before this failure can
        // be this step's own child; without it, any build-step kill counts.
        let victim_in_window = build_victim.is_some()
            && since_kill_us.is_none_or(|elapsed| elapsed <= OOM_KILL_ATTRIBUTION_WINDOW_US);

        let attribution = if step_killed
            && kills.is_none_or(|k| k > 0)
            && (signals.victims.is_none() || victim_in_window)
        {
            OomAttribution::StepKilled
        } else if victim_in_window && alone {
            OomAttribution::VictimWasBuildStep
        } else if victim_in_window {
            OomAttribution::VictimWasBuildStepConcurrent {
                other_builds: signals.other_builds.unwrap_or(0),
                seconds_before_failure: (since_kill_us.unwrap_or(0) / 1_000_000) as u32,
            }
        } else if kills.is_some_and(|k| k > 0) && signals.victims.is_none() && alone {
            OomAttribution::OnlyBuildRunning
        } else if kills.is_some_and(|k| k > 0) {
            let note = match (build_victim, other_victim, signals.other_builds) {
                (Some(victim), _, _) => format!(
                    "NOTE: the kernel's OOM killer terminated `{}` in a build step on this host \
                     {} s before this step failed; a killed child ends its build tool within \
                     seconds, so that kill is not attributed to this build.\n",
                    victim.comm,
                    since_kill_us.map_or(0, |us| us / 1_000_000)
                ),
                (None, Some(victim), _) => format!(
                    "NOTE: the kernel's OOM killer terminated `{}` (in {}) on this host while \
                     this step ran; that process was not part of this build, so the failure is \
                     not attributed to memory.\n",
                    victim.comm, victim.cgroup
                ),
                (None, None, Some(others)) => format!(
                    "NOTE: the kernel's OOM killer terminated {} process(es) on this host while \
                     this step ran and {} other build(s) were running, so the kill cannot be \
                     attributed to this build; if this failure looks like a crash without a \
                     compiler error, the step may have run out of memory.\n",
                    kills.unwrap_or(0),
                    others
                ),
                (None, None, None) => format!(
                    "NOTE: the kernel's OOM killer terminated {} process(es) on this host while \
                     this step ran; other builds may have been running, so the kill cannot be \
                     attributed to this build.\n",
                    kills.unwrap_or(0)
                ),
            };
            return MemoryVerdict::Unattributed(note);
        } else {
            return MemoryVerdict::NotMemory;
        };

        let host_memory_mb = self.daemon_is_local.then(|| {
            let mut sys = System::new();
            sys.refresh_memory();
            sys.total_memory() / (1024 * 1024)
        });
        MemoryVerdict::OutOfMemory(BuildMemoryDiagnosis {
            attribution,
            victim: build_victim.map(|v| v.comm.clone()),
            host_oom_kills: kills,
            exit_code,
            host_memory_mb,
            requested_cap_mb: (requested_cap_bytes / (1024 * 1024)).max(0) as u64,
            cap_enforced: !self.use_buildkit,
        })
    }

    /// Readings taken as a build starts so a failure can be attributed: the
    /// kernel's OOM-kill counter and the boot-relative clock the kernel log
    /// uses (only when the daemon is on this host), and the concurrency
    /// bookkeeping. Call after the build permit is held.
    pub(super) fn sample_build_start(&self) -> BuildStartSample {
        let builds_started = self
            .builds_started
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
            + 1;
        // This build holds a permit, so every other permit holder is another
        // build. `None` when builds are ungated: the count is unknown.
        let others_at_start = self
            .build_limits
            .gate()
            .map(|gate| gate.running().saturating_sub(1));
        BuildStartSample {
            oom_kills: self.daemon_is_local.then(host_oom_kill_count).flatten(),
            uptime_us: self.daemon_is_local.then(uptime_us).flatten(),
            others_at_start,
            builds_started,
        }
    }

    /// Builds other than this one that were in flight at any point since
    /// `start`: those already running then, plus those started since.
    fn other_builds_since(&self, start: &BuildStartSample) -> Option<usize> {
        let started_since = self
            .builds_started
            .load(std::sync::atomic::Ordering::Acquire)
            .saturating_sub(start.builds_started);
        Some(start.others_at_start? + started_since as usize)
    }

    /// Map a failed build step to its error. When the failure looks like an
    /// out-of-memory kill, the second value is an extra `ERROR:` line for the
    /// build log so the deployment log says so where the user reads it.
    pub(super) fn classify_build_failure(
        &self,
        error_text: String,
        start: &BuildStartSample,
        requested_cap_bytes: i64,
    ) -> (BuilderError, Option<String>) {
        match self.diagnose_out_of_memory(&error_text, start, requested_cap_bytes) {
            MemoryVerdict::OutOfMemory(diagnosis) => {
                let line = format!("ERROR: {}\n", diagnosis);
                (
                    BuilderError::BuildOutOfMemory {
                        message: error_text,
                        diagnosis,
                    },
                    Some(line),
                )
            }
            MemoryVerdict::Unattributed(note) => {
                (BuilderError::BuildFailed(error_text), Some(note))
            }
            MemoryVerdict::NotMemory => (BuilderError::BuildFailed(error_text), None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::test_support::runtime_for_diagnosis;
    use crate::docker::DockerRuntime;
    use bollard::Docker;
    use std::sync::Arc;

    #[test]
    fn docker_host_is_local_only_for_unset_or_socket_hosts() {
        assert!(docker_host_is_local(None));
        assert!(docker_host_is_local(Some("")));
        assert!(docker_host_is_local(Some("unix:///var/run/docker.sock")));
        assert!(docker_host_is_local(Some("npipe:////./pipe/docker_engine")));
        assert!(!docker_host_is_local(Some("tcp://10.0.0.5:2376")));
        assert!(!docker_host_is_local(Some("ssh://build@10.0.0.5")));
    }

    #[test]
    fn parse_oom_kill_count_reads_the_vmstat_counter() {
        let vmstat = "nr_free_pages 12345\noom_kill 3\nswap_ra 0\n";
        assert_eq!(parse_oom_kill_count(vmstat), Some(3));
        assert_eq!(parse_oom_kill_count("nr_free_pages 12345\n"), None);
        assert_eq!(parse_oom_kill_count("oom_kill nope\n"), None);
    }

    #[test]
    fn build_step_exit_code_reads_buildkit_and_legacy_phrasing() {
        let buildkit = "Build failed: Docker stream error: process \"/bin/sh -c npm run build\" \
                        did not complete successfully: exit code: 137";
        assert_eq!(build_step_exit_code(buildkit), Some(137));
        let legacy = "The command '/bin/sh -c npm run build' returned a non-zero code: 1";
        assert_eq!(build_step_exit_code(legacy), Some(1));
        assert_eq!(
            build_step_exit_code("failed to resolve source metadata"),
            None
        );
    }

    /// Signals as observed at `FAILED_AT_US`; victims are stamped by the tests.
    fn signals(
        oom_kills: Option<u64>,
        victims: Option<Vec<OomVictim>>,
        other_builds: Option<usize>,
    ) -> MemorySignals {
        MemorySignals {
            oom_kills,
            victims,
            other_builds,
            failed_at_us: Some(FAILED_AT_US),
        }
    }

    const FAILED_AT_US: u64 = 500_000_000;

    fn victim(comm: &str, cgroup: &str) -> OomVictim {
        victim_at(comm, cgroup, FAILED_AT_US - 200_000)
    }

    fn victim_at(comm: &str, cgroup: &str, at_us: u64) -> OomVictim {
        OomVictim {
            comm: comm.into(),
            cgroup: cgroup.into(),
            at_us,
        }
    }

    const EXITED_ONE: &str = "Build failed: process \"/bin/sh -c npm run build\" did not complete \
                              successfully: exit code: 1";
    const KILLED: &str = "Build failed: process \"/bin/sh -c npm run build\" did not complete \
                          successfully: exit code: 137";
    const CAP: i64 = 512 * 1024 * 1024;

    #[test]
    fn diagnose_out_of_memory_needs_a_kill_signal() {
        let rt = runtime_for_diagnosis(true);
        // A plain failure with no OOM kill on the host is not a memory failure.
        assert_eq!(
            rt.diagnose_out_of_memory_with(EXITED_ONE, &signals(Some(0), None, Some(0)), CAP),
            MemoryVerdict::NotMemory
        );
        assert_eq!(
            rt.diagnose_out_of_memory_with(EXITED_ONE, &MemorySignals::default(), CAP),
            MemoryVerdict::NotMemory
        );
        // A SIGKILL with a readable counter that did not move came from
        // something other than the kernel's OOM killer.
        assert_eq!(
            rt.diagnose_out_of_memory_with(KILLED, &signals(Some(0), None, Some(0)), CAP),
            MemoryVerdict::NotMemory
        );
        // The live wrapper with a remote daemon (no host readings) can still
        // recognise the step's own kill signal.
        let remote = BuildStartSample {
            oom_kills: None,
            uptime_us: None,
            others_at_start: Some(0),
            builds_started: 0,
        };
        assert!(matches!(
            rt.diagnose_out_of_memory(KILLED, &remote, CAP),
            MemoryVerdict::OutOfMemory(BuildMemoryDiagnosis {
                attribution: OomAttribution::StepKilled,
                ..
            })
        ));
        assert_eq!(
            rt.diagnose_out_of_memory(EXITED_ONE, &remote, CAP),
            MemoryVerdict::NotMemory
        );
    }

    #[test]
    fn other_builds_since_counts_overlap_over_the_whole_build() {
        let rt = runtime_for_diagnosis(true);
        let first = rt.sample_build_start();
        assert_eq!(first.others_at_start, Some(0));
        assert_eq!(rt.other_builds_since(&first), Some(0));
        // A build that starts while the first one runs counts even after it
        // has finished by the time the first one fails.
        let _second = rt.sample_build_start();
        assert_eq!(rt.other_builds_since(&first), Some(1));
        // Without a semaphore the count is unknown, never zero.
        let unbounded = DockerRuntime::new(
            Arc::new(
                Docker::connect_with_http("http://127.0.0.1:1", 1, bollard::API_DEFAULT_VERSION)
                    .expect("client construction makes no connection"),
            ),
            true,
            "test-network".to_string(),
        );
        let start = unbounded.sample_build_start();
        assert_eq!(start.others_at_start, None);
        assert_eq!(unbounded.other_builds_since(&start), None);
    }

    #[test]
    fn diagnose_out_of_memory_from_the_sigkill_exit_status() {
        let rt = runtime_for_diagnosis(true);
        let MemoryVerdict::OutOfMemory(diagnosis) =
            rt.diagnose_out_of_memory_with(KILLED, &MemorySignals::default(), CAP)
        else {
            panic!("exit code 137 is a kill signal without a readable counter");
        };
        assert_eq!(diagnosis.attribution, OomAttribution::StepKilled);
        assert_eq!(diagnosis.exit_code, Some(137));
        assert_eq!(diagnosis.host_oom_kills, None);
        assert_eq!(diagnosis.victim, None);
        assert_eq!(diagnosis.requested_cap_mb, 512);
        assert!(!diagnosis.cap_enforced, "BuildKit does not apply the cap");
    }

    #[test]
    fn diagnose_out_of_memory_from_a_host_oom_kill_with_an_ordinary_exit_code() {
        let rt = runtime_for_diagnosis(false);
        // The reported shape: a child of the build tool was killed, the tool
        // itself exited 1, the kernel's counter moved by one, no other build
        // was running, and the kernel log was not readable.
        let MemoryVerdict::OutOfMemory(diagnosis) =
            rt.diagnose_out_of_memory_with(EXITED_ONE, &signals(Some(1), None, Some(0)), CAP)
        else {
            panic!("an OOM kill during the only running build is a memory failure");
        };
        assert_eq!(diagnosis.attribution, OomAttribution::OnlyBuildRunning);
        assert_eq!(diagnosis.exit_code, Some(1));
        assert_eq!(diagnosis.host_oom_kills, Some(1));
        assert!(diagnosis.cap_enforced, "the legacy builder applies the cap");
        if rt.daemon_is_local {
            assert!(diagnosis.host_memory_mb.unwrap_or(0) > 0);
        }
    }

    #[test]
    fn diagnose_out_of_memory_attributes_through_the_kernel_log() {
        let rt = runtime_for_diagnosis(true);
        let step_cgroup = "/system.slice/system.slice:docker:zglzqvzuv2kxjwkdcpi6udshi";
        // The log names a build step's cgroup and no other build was
        // running: attributed even with an ordinary exit code.
        let step = vec![victim("node", step_cgroup)];
        let MemoryVerdict::OutOfMemory(diagnosis) =
            rt.diagnose_out_of_memory_with(EXITED_ONE, &signals(Some(1), Some(step), Some(0)), CAP)
        else {
            panic!("a killed build-step process is a memory failure");
        };
        assert_eq!(diagnosis.attribution, OomAttribution::VictimWasBuildStep);
        assert_eq!(diagnosis.victim.as_deref(), Some("node"));

        // Another build was running: the victim could be its child. Only a
        // kill within the attribution window of this step's failure counts,
        // and then as "most likely".
        let just_before = vec![victim_at("node", step_cgroup, FAILED_AT_US - 200_000)];
        let MemoryVerdict::OutOfMemory(diagnosis) = rt.diagnose_out_of_memory_with(
            EXITED_ONE,
            &signals(Some(1), Some(just_before), Some(1)),
            CAP,
        ) else {
            panic!("a build-step kill right before the failure is attributed");
        };
        assert_eq!(
            diagnosis.attribution,
            OomAttribution::VictimWasBuildStepConcurrent {
                other_builds: 1,
                seconds_before_failure: 0,
            }
        );
        assert!(diagnosis
            .to_string()
            .starts_with("The build step most likely"));

        // A build-step kill long before this failure was some earlier
        // build's child (a killed child ends its parent within seconds),
        // even if no other build overlapped this one by the bookkeeping.
        let long_before = vec![victim_at(
            "node",
            step_cgroup,
            FAILED_AT_US - OOM_KILL_ATTRIBUTION_WINDOW_US - 1,
        )];
        for others in [Some(0), Some(1), None] {
            match rt.diagnose_out_of_memory_with(
                EXITED_ONE,
                &signals(Some(1), Some(long_before.clone()), others),
                CAP,
            ) {
                MemoryVerdict::Unattributed(note) => {
                    assert!(note.contains("`node` in a build step"), "{note}");
                    assert!(note.contains("10 s before this step failed"), "{note}");
                    assert!(note.contains("not attributed to this build"), "{note}");
                }
                other => panic!("expected an unattributed note, got {other:?}"),
            }
        }
        // Even the step's own SIGKILL is not pinned on a kill that old.
        assert!(matches!(
            rt.diagnose_out_of_memory_with(
                KILLED,
                &signals(Some(1), Some(long_before), Some(0)),
                CAP
            ),
            MemoryVerdict::Unattributed(_)
        ));

        // The kernel log and /proc/uptime are different clocks: a kill logged
        // within the slop after the observed failure still counts as before
        // it; one clearly after it belongs to someone else.
        let within_slop = vec![victim_at("node", step_cgroup, FAILED_AT_US + CLOCK_SLOP_US)];
        assert!(matches!(
            rt.diagnose_out_of_memory_with(
                EXITED_ONE,
                &signals(Some(1), Some(within_slop), Some(0)),
                CAP
            ),
            MemoryVerdict::OutOfMemory(BuildMemoryDiagnosis {
                attribution: OomAttribution::VictimWasBuildStep,
                ..
            })
        ));
        let after = vec![victim_at(
            "node",
            step_cgroup,
            FAILED_AT_US + CLOCK_SLOP_US + 1,
        )];
        assert!(matches!(
            rt.diagnose_out_of_memory_with(
                EXITED_ONE,
                &signals(Some(1), Some(after), Some(0)),
                CAP
            ),
            MemoryVerdict::Unattributed(_)
        ));

        // The log names a container instead: not this build's problem, even
        // though the counter moved and no other build was running.
        let container = vec![victim(
            "postgres",
            "/system.slice/docker-2a9f3c6d0e1b4a5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b9c.scope",
        )];
        match rt.diagnose_out_of_memory_with(
            EXITED_ONE,
            &signals(Some(1), Some(container.clone()), Some(0)),
            CAP,
        ) {
            MemoryVerdict::Unattributed(note) => {
                assert!(note.contains("terminated `postgres`"), "{note}");
                assert!(note.contains("not attributed to memory"), "{note}");
            }
            other => panic!("expected an unattributed note, got {other:?}"),
        }
        // Same with the step's own SIGKILL: the kernel killed something else,
        // so this SIGKILL came from elsewhere.
        assert!(matches!(
            rt.diagnose_out_of_memory_with(
                KILLED,
                &signals(Some(1), Some(container), Some(0)),
                CAP
            ),
            MemoryVerdict::Unattributed(_)
        ));
    }

    #[test]
    fn diagnose_out_of_memory_hedges_when_other_builds_were_running() {
        let rt = runtime_for_diagnosis(true);
        // Counter moved, log unreadable, another build in flight: a note, not
        // a diagnosis, because the kill could be the other build's.
        match rt.diagnose_out_of_memory_with(EXITED_ONE, &signals(Some(1), None, Some(1)), CAP) {
            MemoryVerdict::Unattributed(note) => {
                assert!(note.contains("1 other build(s) were running"), "{note}");
                assert!(
                    note.contains("cannot be attributed to this build"),
                    "{note}"
                );
            }
            other => panic!("expected an unattributed note, got {other:?}"),
        }
        // Concurrency unknown (no semaphore): same hedge, different wording.
        match rt.diagnose_out_of_memory_with(EXITED_ONE, &signals(Some(2), None, None), CAP) {
            MemoryVerdict::Unattributed(note) => {
                assert!(note.contains("terminated 2 process(es)"), "{note}");
                assert!(
                    note.contains("other builds may have been running"),
                    "{note}"
                );
            }
            other => panic!("expected an unattributed note, got {other:?}"),
        }
        // The step's own SIGKILL is still conclusive with other builds running.
        assert!(matches!(
            rt.diagnose_out_of_memory_with(KILLED, &signals(Some(1), None, Some(1)), CAP),
            MemoryVerdict::OutOfMemory(BuildMemoryDiagnosis {
                attribution: OomAttribution::StepKilled,
                ..
            })
        ));
    }

    #[test]
    fn cgroup_is_build_step_tells_buildkit_execs_from_containers() {
        assert!(cgroup_is_build_step(
            "/system.slice/system.slice:docker:zglzqvzuv2kxjwkdcpi6udshi"
        ));
        assert!(cgroup_is_build_step("/docker/zglzqvzuv2kxjwkdcpi6udshi"));
        assert!(!cgroup_is_build_step(
            "/system.slice/docker-2a9f3c6d0e1b4a5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b9c.scope"
        ));
        assert!(!cgroup_is_build_step(
            "/docker/2a9f3c6d0e1b4a5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b9c"
        ));
        assert!(!cgroup_is_build_step(
            "/user.slice/user-1000.slice/session-3.scope"
        ));
        assert!(!cgroup_is_build_step("/docker/"));
    }

    #[test]
    fn parse_oom_victims_reads_kernel_log_records_since_the_build_started() {
        let kmsg = "6,100,1000000,-;usb 1-1: new device\n\
                    3,101,2000000,-;oom-kill:constraint=CONSTRAINT_NONE,nodemask=(null),cpuset=user.slice,mems_allowed=0,global_oom,task_memcg=/system.slice/docker-abc.scope,task=postgres,pid=42,uid=0\n\
                    3,102,2000500,-;Out of memory: Killed process 42 (postgres) total-vm:1kB\n\
                    3,103,3000000,-;oom-kill:constraint=CONSTRAINT_NONE,nodemask=(null),cpuset=user.slice,mems_allowed=0,global_oom,task_memcg=/system.slice/system.slice:docker:zglzqvzuv2kxjwkdcpi6udshi,task=node,pid=99,uid=0\n\
                     SUBSYSTEM=mem\n";
        let since_build_start = parse_oom_victims(kmsg, 2_500_000);
        assert_eq!(
            since_build_start,
            vec![victim_at(
                "node",
                "/system.slice/system.slice:docker:zglzqvzuv2kxjwkdcpi6udshi",
                3_000_000
            )]
        );
        let all = parse_oom_victims(kmsg, 0);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].comm, "postgres");
        assert_eq!(all[0].at_us, 2_000_000);
        assert!(parse_oom_victims("garbage without separators\n", 0).is_empty());
    }

    // ── DockerHandle::Disabled path ───────────────────────────────────────────
    //
    // A DockerRuntime built with a Disabled handle must return a typed
    // DeployerError::DockerUnavailable for every daemon-dependent operation
    // rather than panicking or producing a confusing bollard connection error.
    // None of these tests require a live daemon: they are pure in-process
    // checks of the enum's behaviour under the control-plane profile.

    #[test]
    fn build_stream_error_text_drops_the_stream_wrapper() {
        let stream = bollard::errors::Error::DockerStreamError {
            error: "process \"/bin/sh -c make build\" did not complete successfully: exit code: 2"
                .to_string(),
        };
        let text = build_stream_error_text(&stream);
        assert_eq!(
            text,
            "process \"/bin/sh -c make build\" did not complete successfully: exit code: 2"
        );

        // The builder error adds the prefix exactly once.
        let error = BuilderError::BuildFailed(text);
        assert_eq!(
            error.to_string(),
            "Build failed: process \"/bin/sh -c make build\" did not complete successfully: \
             exit code: 2"
        );
        assert_eq!(build_step_exit_code(&error.to_string()), Some(2));

        let other = bollard::errors::Error::RequestTimeoutError;
        assert_eq!(build_stream_error_text(&other), other.to_string());
    }
}
