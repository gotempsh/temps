// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Optional requests and limits (ADR-055 §5).

use serde::{Deserialize, Serialize};

/// Amounts of each resource. `None` means "not set", never zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceAmounts {
    /// Thousandths of a CPU core.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_millis: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_mb: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pids: Option<u32>,
    /// Block I/O bandwidth, in megabytes per second (reads plus writes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_io_mbps: Option<u32>,
    /// Network bandwidth, in megabytes per second, applied to each direction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_mbps: Option<u32>,
}

impl ResourceAmounts {
    pub fn is_empty(&self) -> bool {
        self.cpu_millis.is_none()
            && self.memory_mb.is_none()
            && self.disk_mb.is_none()
            && self.pids.is_none()
            && self.disk_io_mbps.is_none()
            && self.network_mbps.is_none()
    }
}

/// Requests reserve capacity (counted only on enforcing nodes); limits are
/// hard caps applied by the driver. Neither set means best effort.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resources {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests: Option<ResourceAmounts>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<ResourceAmounts>,
}

impl Resources {
    /// Best effort: no requests, no limits.
    pub fn best_effort() -> Self {
        Self::default()
    }

    pub fn is_best_effort(&self) -> bool {
        self.requests.is_none_or(|r| r.is_empty()) && self.limits.is_none_or(|l| l.is_empty())
    }

    /// Each limit is the smaller of this spec's limit and `cap`'s, where
    /// either is set. Used for the `Untrusted` safety caps (§4), which hold
    /// whatever the user sets.
    pub fn capped_by(self, cap: ResourceAmounts) -> Self {
        fn min(a: Option<u32>, b: Option<u32>) -> Option<u32> {
            match (a, b) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            }
        }
        let limits = self.limits.unwrap_or_default();
        Self {
            requests: self.requests,
            limits: Some(ResourceAmounts {
                cpu_millis: min(limits.cpu_millis, cap.cpu_millis),
                memory_mb: min(limits.memory_mb, cap.memory_mb),
                disk_mb: min(limits.disk_mb, cap.disk_mb),
                pids: min(limits.pids, cap.pids),
                disk_io_mbps: min(limits.disk_io_mbps, cap.disk_io_mbps),
                network_mbps: min(limits.network_mbps, cap.network_mbps),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_best_effort_and_serialises_empty() {
        let r = Resources::default();
        assert!(r.is_best_effort());
        assert_eq!(serde_json::to_string(&r).unwrap(), "{}");
        let empty_limits = Resources {
            limits: Some(ResourceAmounts::default()),
            ..Default::default()
        };
        assert!(empty_limits.is_best_effort());
    }

    #[test]
    fn caps_lower_limits_and_fill_unset_ones() {
        let user = Resources {
            requests: None,
            limits: Some(ResourceAmounts {
                memory_mb: Some(4096),
                cpu_millis: Some(250),
                ..Default::default()
            }),
        };
        let capped = user.capped_by(ResourceAmounts {
            memory_mb: Some(2048),
            pids: Some(256),
            cpu_millis: Some(1000),
            disk_mb: None,
            disk_io_mbps: None,
            network_mbps: None,
        });
        let limits = capped.limits.unwrap();
        assert_eq!(limits.memory_mb, Some(2048), "the cap wins when lower");
        assert_eq!(limits.cpu_millis, Some(250), "the user's lower limit stays");
        assert_eq!(limits.pids, Some(256), "an unset limit takes the cap");
        assert_eq!(limits.disk_mb, None);
        assert!(!capped.is_best_effort());
    }
}
