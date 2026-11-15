// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The overlay's host-side names (ADR 059).
//!
//! A cluster's overlay appears on every host as three objects: a Docker
//! bridge network, the Linux bridge it is pinned to, and a VXLAN device
//! enslaved to that bridge. All three are derived from one name, the
//! *overlay network name* (`temps0` unless an administrator changed it):
//!
//! | object          | name            |
//! |-----------------|-----------------|
//! | Docker network  | `<name>`        |
//! | Linux bridge    | `br-<name>`     |
//! | VXLAN device    | `vxlan-<name>`  |
//!
//! The name is stored on the control plane (the cluster default on
//! `network_config`, an optional per-node override on `nodes`) and shipped
//! to each node with its allocation. A host that already has a network of
//! that name, for instance one left behind by another cluster, gets a
//! different name for this cluster instead of having the existing one
//! removed.
//!
//! Every process that reconciles an overlay records the name it reconciled
//! with [`set_current`]; code elsewhere in the same process that needs the
//! overlay (attaching a container, reading its gateway) asks [`current`]
//! instead of assuming `temps0`.

use std::sync::RwLock;

use crate::NetworkError;

/// The overlay network name a cluster uses unless it is changed.
pub const DEFAULT_OVERLAY_NETWORK_NAME: &str = "temps0";

/// Linux limits interface names to 15 bytes. `vxlan-` is the longest prefix
/// derived from the overlay name, which leaves 9.
pub const MAX_OVERLAY_NETWORK_NAME_LEN: usize = 15 - VXLAN_PREFIX.len();

const BRIDGE_PREFIX: &str = "br-";
const VXLAN_PREFIX: &str = "vxlan-";

/// The three host-side names of one overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayNames {
    /// Docker bridge network containers attach to.
    pub docker_network: String,
    /// Linux bridge the Docker network is pinned to.
    pub bridge: String,
    /// VXLAN device enslaved to the bridge.
    pub vxlan: String,
}

impl OverlayNames {
    /// Derive the names from a validated overlay network name.
    pub fn new(name: &str) -> Result<Self, NetworkError> {
        validate_overlay_network_name(name)?;
        Ok(Self::derive(name))
    }

    fn derive(name: &str) -> Self {
        Self {
            docker_network: name.to_owned(),
            bridge: format!("{BRIDGE_PREFIX}{name}"),
            vxlan: format!("{VXLAN_PREFIX}{name}"),
        }
    }
}

impl Default for OverlayNames {
    fn default() -> Self {
        Self::derive(DEFAULT_OVERLAY_NETWORK_NAME)
    }
}

/// Check an overlay network name. It must start with a letter, use only
/// lowercase letters, digits, `-` and `_`, and be at most
/// [`MAX_OVERLAY_NETWORK_NAME_LEN`] characters so the derived interface
/// names fit the kernel's limit. The narrow alphabet is also safe in nft and
/// iptables scripts and in Docker network names.
pub fn validate_overlay_network_name(name: &str) -> Result<(), NetworkError> {
    let reason = if name.is_empty() {
        Some("it is empty".to_string())
    } else if name.len() > MAX_OVERLAY_NETWORK_NAME_LEN {
        Some(format!(
            "it is {} characters long; at most {MAX_OVERLAY_NETWORK_NAME_LEN} fit, because \
             the VXLAN device `{VXLAN_PREFIX}<name>` must stay within Linux's 15-character \
             interface-name limit",
            name.len()
        ))
    } else if !name.as_bytes()[0].is_ascii_lowercase() {
        Some("it must start with a lowercase letter".to_string())
    } else if !name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    {
        Some("only lowercase letters, digits, '-' and '_' are allowed".to_string())
    } else {
        None
    };
    match reason {
        None => Ok(()),
        Some(reason) => Err(NetworkError::InvalidConfig {
            reason: format!("overlay network name '{name}' is invalid: {reason}"),
        }),
    }
}

/// The first name, starting with `preferred`, that names none of `taken`.
///
/// Used when a host already has a network called `preferred` that this
/// cluster does not own: rather than removing it, this cluster's overlay
/// uses `preferred` with the lowest numeric suffix that is free
/// (`temps0` → `temps1`, `temps2`, ...). A name whose suffix would not fit
/// gets its trailing characters replaced. Returns `None` only if every
/// candidate is taken, which needs thousands of networks.
pub fn first_free_overlay_network_name(preferred: &str, taken: &[String]) -> Option<String> {
    let is_free = |candidate: &str| {
        let names = OverlayNames::derive(candidate);
        !taken.iter().any(|existing| {
            existing == &names.docker_network
                || existing == &names.bridge
                || existing == &names.vxlan
        })
    };
    if validate_overlay_network_name(preferred).is_ok() && is_free(preferred) {
        return Some(preferred.to_owned());
    }
    // `temps0` numbers from 1; any other name keeps its stem.
    let stem = preferred.trim_end_matches(|c: char| c.is_ascii_digit());
    let stem = if stem.is_empty() || validate_overlay_network_name(stem).is_err() {
        "temps"
    } else {
        stem
    };
    (1..10_000u32).find_map(|n| {
        let suffix = n.to_string();
        let room = MAX_OVERLAY_NETWORK_NAME_LEN.checked_sub(suffix.len())?;
        let candidate = format!("{}{suffix}", &stem[..stem.len().min(room)]);
        (validate_overlay_network_name(&candidate).is_ok() && is_free(&candidate))
            .then_some(candidate)
    })
}

static CURRENT: RwLock<Option<OverlayNames>> = RwLock::new(None);

/// The overlay names this process last reconciled, or the defaults when it
/// has not reconciled one (single-host installs, or before the first sync).
pub fn current() -> OverlayNames {
    match CURRENT.read() {
        Ok(guard) => guard.clone().unwrap_or_default(),
        Err(poisoned) => poisoned.into_inner().clone().unwrap_or_default(),
    }
}

/// Record the overlay names this process reconciles. Returns the names it
/// replaces when they differ, so the caller can say that the old network
/// was left in place.
pub fn set_current(names: OverlayNames) -> Option<OverlayNames> {
    let mut guard = match CURRENT.write() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let previous = guard.replace(names.clone());
    previous.filter(|previous| previous != &names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_the_three_host_names() {
        let names = OverlayNames::new("temps1").unwrap();
        assert_eq!(names.docker_network, "temps1");
        assert_eq!(names.bridge, "br-temps1");
        assert_eq!(names.vxlan, "vxlan-temps1");
        assert_eq!(
            OverlayNames::default(),
            OverlayNames::new("temps0").unwrap()
        );
    }

    #[test]
    fn the_longest_valid_name_still_fits_the_interface_limit() {
        let name = "a".repeat(MAX_OVERLAY_NETWORK_NAME_LEN);
        let names = OverlayNames::new(&name).unwrap();
        assert_eq!(names.vxlan.len(), 15);
        assert!(OverlayNames::new(&format!("{name}a")).is_err());
    }

    #[test]
    fn rejects_names_unsafe_for_scripts_or_interfaces() {
        for bad in [
            "", "Temps0", "0temps", "temps 0", "temps\"0", "temps/0", "tem.ps",
        ] {
            assert!(
                validate_overlay_network_name(bad).is_err(),
                "{bad:?} must be rejected"
            );
        }
        for good in ["temps0", "temps1", "t", "cluster_b", "edge-2"] {
            assert!(validate_overlay_network_name(good).is_ok(), "{good:?}");
        }
    }

    #[test]
    fn keeps_the_preferred_name_when_it_is_free() {
        let taken = vec!["bridge".to_string(), "host".to_string()];
        assert_eq!(
            first_free_overlay_network_name("temps0", &taken).as_deref(),
            Some("temps0")
        );
    }

    #[test]
    fn numbers_past_a_name_another_cluster_holds() {
        let taken = vec!["temps0".to_string(), "temps1".to_string()];
        assert_eq!(
            first_free_overlay_network_name("temps0", &taken).as_deref(),
            Some("temps2")
        );
    }

    #[test]
    fn a_leftover_bridge_or_vxlan_device_also_counts_as_taken() {
        let taken = vec!["br-temps1".to_string(), "vxlan-temps2".to_string()];
        assert_eq!(
            first_free_overlay_network_name("temps1", &taken).as_deref(),
            Some("temps3")
        );
    }

    #[test]
    fn a_long_name_is_shortened_to_fit_its_suffix() {
        let taken = vec!["abcdefghi".to_string()];
        assert_eq!(
            first_free_overlay_network_name("abcdefghi", &taken).as_deref(),
            Some("abcdefgh1")
        );
    }

    #[test]
    fn records_the_current_names_and_reports_a_change() {
        // One test owns the process-wide value so parallel tests cannot race.
        let first = OverlayNames::new("temps0").unwrap();
        set_current(first.clone());
        assert_eq!(current(), first);
        assert_eq!(set_current(first.clone()), None);
        let second = OverlayNames::new("temps1").unwrap();
        assert_eq!(set_current(second.clone()), Some(first));
        assert_eq!(current(), second);
        set_current(OverlayNames::default());
    }
}
