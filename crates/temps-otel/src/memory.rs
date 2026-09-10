// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Startup-time memory sizing for the OTel ingest and relay paths.
//!
//! Limits are derived once from the effective process memory ceiling. Inside a
//! container that is the lower of the cgroup limit and host RAM; on a regular
//! host it is physical RAM. This deliberately does not react to free memory or
//! RSS at runtime, which would make admission capacity oscillate under load.

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

/// Conservative effective-memory fallback when neither procfs nor cgroups are
/// readable. It produces 8 concurrent ingests, an 8 MiB OSS relay handoff,
/// and a 32 MiB external-forwarding budget.
pub const FALLBACK_EFFECTIVE_MEMORY_BYTES: u64 = 2 * GIB;

pub const MIN_INGEST_CONCURRENCY: usize = 2;
pub const MAX_INGEST_CONCURRENCY: usize = 32;
pub const MIN_RELAY_QUEUE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_RELAY_QUEUE_BYTES: usize = 16 * 1024 * 1024;
pub const MIN_EXTERNAL_RELAY_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_EXTERNAL_RELAY_BYTES: usize = 64 * 1024 * 1024;

/// Source of the effective memory value selected at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryLimitSource {
    CgroupV2,
    CgroupV1,
    Host,
    Fallback,
}

impl MemoryLimitSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CgroupV2 => "cgroup_v2",
            Self::CgroupV1 => "cgroup_v1",
            Self::Host => "host",
            Self::Fallback => "fallback",
        }
    }
}

/// Fixed-for-process-lifetime limits selected from effective memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OtelMemoryProfile {
    pub effective_memory_bytes: u64,
    pub source: MemoryLimitSource,
    pub max_concurrent_ingest_requests: usize,
    pub relay_queue_max_bytes: usize,
    /// Budget available to a plugin-owned external-forwarding queue.
    pub external_relay_max_bytes: usize,
}

impl OtelMemoryProfile {
    /// Detect the effective memory ceiling and derive bounded OTel limits.
    pub fn detect() -> Self {
        let host = read_host_memory_bytes();
        let membership = std::fs::read_to_string("/proc/self/cgroup").ok();
        let cgroup_v2 = lowest_limit(
            read_cgroup_limit("/sys/fs/cgroup/memory.max"),
            membership.as_deref().and_then(|contents| {
                cgroup_relative_path(contents, None).and_then(|relative| {
                    read_cgroup_hierarchy_limit("/sys/fs/cgroup", relative, "memory.max")
                })
            }),
        );
        let cgroup_v1 = lowest_limit(
            read_cgroup_limit("/sys/fs/cgroup/memory/memory.limit_in_bytes"),
            membership.as_deref().and_then(|contents| {
                cgroup_relative_path(contents, Some("memory")).and_then(|relative| {
                    read_cgroup_hierarchy_limit(
                        "/sys/fs/cgroup/memory",
                        relative,
                        "memory.limit_in_bytes",
                    )
                })
            }),
        );
        Self::from_detected(host, cgroup_v2, cgroup_v1)
    }

    /// Conservative deterministic profile used by `Default` and detection
    /// fallback paths.
    pub const fn fallback() -> Self {
        Self::for_memory_bytes(FALLBACK_EFFECTIVE_MEMORY_BYTES, MemoryLimitSource::Fallback)
    }

    /// Derive limits from a known effective-memory ceiling.
    pub const fn for_memory_bytes(memory_bytes: u64, source: MemoryLimitSource) -> Self {
        let ingest = clamp_u64(
            memory_bytes / (256 * MIB),
            MIN_INGEST_CONCURRENCY as u64,
            MAX_INGEST_CONCURRENCY as u64,
        ) as usize;
        let relay = clamp_u64(
            memory_bytes / 256,
            MIN_RELAY_QUEUE_BYTES as u64,
            MAX_RELAY_QUEUE_BYTES as u64,
        ) as usize;
        let external_relay = clamp_u64(
            memory_bytes / 64,
            MIN_EXTERNAL_RELAY_BYTES as u64,
            MAX_EXTERNAL_RELAY_BYTES as u64,
        ) as usize;

        Self {
            effective_memory_bytes: memory_bytes,
            source,
            max_concurrent_ingest_requests: ingest,
            relay_queue_max_bytes: relay,
            external_relay_max_bytes: external_relay,
        }
    }

    fn from_detected(host: Option<u64>, cgroup_v2: Option<u64>, cgroup_v1: Option<u64>) -> Self {
        let candidates = [
            cgroup_v2.map(|bytes| (bytes, MemoryLimitSource::CgroupV2)),
            cgroup_v1.map(|bytes| (bytes, MemoryLimitSource::CgroupV1)),
            host.map(|bytes| (bytes, MemoryLimitSource::Host)),
        ];

        candidates
            .into_iter()
            .flatten()
            .min_by_key(|(bytes, _)| *bytes)
            .map_or_else(Self::fallback, |(bytes, source)| {
                Self::for_memory_bytes(bytes, source)
            })
    }
}

impl Default for OtelMemoryProfile {
    fn default() -> Self {
        Self::fallback()
    }
}

const fn clamp_u64(value: u64, min: u64, max: u64) -> u64 {
    if value < min {
        min
    } else if value > max {
        max
    } else {
        value
    }
}

fn read_host_memory_bytes() -> Option<u64> {
    let contents = std::fs::read_to_string("/proc/meminfo").ok()?;
    parse_meminfo_total(&contents)
}

fn parse_meminfo_total(contents: &str) -> Option<u64> {
    let line = contents
        .lines()
        .find(|line| line.starts_with("MemTotal:"))?;
    let kib = line.split_ascii_whitespace().nth(1)?.parse::<u64>().ok()?;
    kib.checked_mul(1024).filter(|bytes| *bytes > 0)
}

fn read_cgroup_limit(path: &str) -> Option<u64> {
    let contents = std::fs::read_to_string(path).ok()?;
    parse_cgroup_limit(&contents)
}

fn read_cgroup_hierarchy_limit(base: &str, relative: &str, filename: &str) -> Option<u64> {
    use std::path::{Component, Path};

    let base = Path::new(base);
    let relative = Path::new(relative.trim_start_matches('/'));
    if relative
        .components()
        .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return None;
    }

    // A child can inherit a tighter ceiling from any ancestor. Walk back to
    // the mount root and keep the smallest finite value in the hierarchy.
    let mut current = base.join(relative);
    let mut limit = None;
    loop {
        let path = current.join(filename);
        let at_current = std::fs::read_to_string(path)
            .ok()
            .and_then(|contents| parse_cgroup_limit(&contents));
        limit = lowest_limit(limit, at_current);

        if current == base || !current.pop() {
            break;
        }
    }
    limit
}

fn cgroup_relative_path<'a>(contents: &'a str, controller: Option<&str>) -> Option<&'a str> {
    contents.lines().find_map(|line| {
        let mut fields = line.splitn(3, ':');
        let _hierarchy_id = fields.next()?;
        let controllers = fields.next()?;
        let path = fields.next()?;

        let matches = match controller {
            None => controllers.is_empty(),
            Some(wanted) => controllers.split(',').any(|value| value == wanted),
        };
        matches.then_some(path)
    })
}

fn lowest_limit(first: Option<u64>, second: Option<u64>) -> Option<u64> {
    match (first, second) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn parse_cgroup_limit(contents: &str) -> Option<u64> {
    let value = contents.trim();
    if value == "max" {
        return None;
    }

    // Cgroup v1 commonly represents "unlimited" with a huge sentinel close
    // to i64::MAX. Treat anything at or above 1 EiB as unbounded.
    value
        .parse::<u64>()
        .ok()
        .filter(|bytes| *bytes > 0 && *bytes < (1_u64 << 60))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_expected_limits_at_reference_sizes() {
        let cases = [
            (512 * MIB, 2, 4 * MIB, 8 * MIB),
            (GIB, 4, 4 * MIB, 16 * MIB),
            (2 * GIB, 8, 8 * MIB, 32 * MIB),
            (4 * GIB, 16, 16 * MIB, 64 * MIB),
            (8 * GIB, 32, 16 * MIB, 64 * MIB),
        ];

        for (memory, ingest, relay, external) in cases {
            let profile = OtelMemoryProfile::for_memory_bytes(memory, MemoryLimitSource::Host);
            assert_eq!(profile.max_concurrent_ingest_requests, ingest);
            assert_eq!(profile.relay_queue_max_bytes as u64, relay);
            assert_eq!(profile.external_relay_max_bytes as u64, external);
        }
    }

    #[test]
    fn bounds_tiny_and_large_machines() {
        let tiny = OtelMemoryProfile::for_memory_bytes(64 * MIB, MemoryLimitSource::Host);
        assert_eq!(tiny.max_concurrent_ingest_requests, MIN_INGEST_CONCURRENCY);
        assert_eq!(tiny.relay_queue_max_bytes, MIN_RELAY_QUEUE_BYTES);
        assert_eq!(tiny.external_relay_max_bytes, MIN_EXTERNAL_RELAY_BYTES);

        let large = OtelMemoryProfile::for_memory_bytes(128 * GIB, MemoryLimitSource::Host);
        assert_eq!(large.max_concurrent_ingest_requests, MAX_INGEST_CONCURRENCY);
        assert_eq!(large.relay_queue_max_bytes, MAX_RELAY_QUEUE_BYTES);
        assert_eq!(large.external_relay_max_bytes, MAX_EXTERNAL_RELAY_BYTES);
    }

    #[test]
    fn parses_procfs_and_cgroup_formats() {
        assert_eq!(
            parse_meminfo_total("MemTotal:       1048576 kB\n"),
            Some(GIB)
        );
        assert_eq!(parse_cgroup_limit("536870912\n"), Some(512 * MIB));
        assert_eq!(parse_cgroup_limit("max\n"), None);
        assert_eq!(parse_cgroup_limit("9223372036854771712\n"), None);
        assert_eq!(parse_cgroup_limit("invalid"), None);
    }

    #[test]
    fn parses_nested_cgroup_membership_paths() {
        let v2 = "0::/system.slice/temps.service\n";
        assert_eq!(
            cgroup_relative_path(v2, None),
            Some("/system.slice/temps.service")
        );

        let v1 = "7:cpu,cpuacct:/docker/abc\n5:memory:/docker/abc\n";
        assert_eq!(
            cgroup_relative_path(v1, Some("memory")),
            Some("/docker/abc")
        );
        assert_eq!(cgroup_relative_path(v1, None), None);
    }

    #[test]
    fn selects_the_lowest_available_ceiling() {
        let profile = OtelMemoryProfile::from_detected(Some(8 * GIB), Some(GIB), None);
        assert_eq!(profile.effective_memory_bytes, GIB);
        assert_eq!(profile.source, MemoryLimitSource::CgroupV2);

        let host_only = OtelMemoryProfile::from_detected(Some(4 * GIB), None, None);
        assert_eq!(host_only.source, MemoryLimitSource::Host);

        let fallback = OtelMemoryProfile::from_detected(None, None, None);
        assert_eq!(fallback, OtelMemoryProfile::fallback());
    }
}
