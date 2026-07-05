//! Shared host-port selection helpers for Docker-backed services.
//!
//! Picking a free host port for a container is inherently racy: we can check
//! that a port is bindable *now*, but Docker only binds it later when the
//! container starts. Between those two moments a concurrent allocator can grab
//! the same port, producing `Bind for 0.0.0.0:<port> failed: port is already
//! allocated`. We saw this in CI when several `temps-providers` Docker tests
//! created Postgres containers in parallel and all landed on 5433.
//!
//! Two mitigations live here:
//!
//! 1. [`find_available_port`] advances a process-wide counter on every call so
//!    that two near-simultaneous allocations start scanning from *different*
//!    offsets, making them pick different ports even when both observe the
//!    base port as free.
//! 2. [`find_available_port_async`] additionally consults Docker's
//!    currently-published ports, so a leftover/leaked container holding a port
//!    is skipped even though the OS reports the port as bindable.

use bollard::query_parameters::ListContainersOptions;
use bollard::Docker;
use std::collections::HashSet;
use std::net::TcpListener;
use std::sync::atomic::{AtomicU16, Ordering};

/// Width of the window scanned starting from a base port.
const SCAN_WINDOW: u16 = 1000;

/// Advances on every allocation so concurrent callers diverge instead of all
/// starting from the same base port. Wraps within `SCAN_WINDOW`.
static NEXT_OFFSET: AtomicU16 = AtomicU16::new(0);

/// Returns `true` if the OS will let us bind the port right now. This does not
/// reserve the port — see the module docs for why that matters.
pub fn is_port_available(port: u16) -> bool {
    TcpListener::bind(("0.0.0.0", port)).is_ok()
}

/// Find an OS-bindable host port at or after `start_port`.
///
/// Each call begins its scan at a different offset (via [`NEXT_OFFSET`]) so two
/// concurrent allocations are unlikely to return the same port even though both
/// see `start_port` as free.
pub fn find_available_port(start_port: u16) -> Option<u16> {
    let offset = NEXT_OFFSET.fetch_add(1, Ordering::Relaxed) % SCAN_WINDOW;
    (0..SCAN_WINDOW)
        .map(|i| start_port.wrapping_add((offset + i) % SCAN_WINDOW))
        .find(|&port| port >= start_port && is_port_available(port))
}

/// Check both OS bindability and that no running Docker container has already
/// published `port`.
pub async fn is_port_available_async(docker: &Docker, port: u16) -> bool {
    if !is_port_available(port) {
        return false;
    }
    !docker_published_ports(docker).await.contains(&port)
}

/// Docker-aware port finder: skips ports already published by any container in
/// addition to the OS bindability and divergence checks of
/// [`find_available_port`]. Falls back to the OS-only scan if Docker can't be
/// queried.
pub async fn find_available_port_async(docker: &Docker, start_port: u16) -> Option<u16> {
    let docker_ports = docker_published_ports(docker).await;
    let offset = NEXT_OFFSET.fetch_add(1, Ordering::Relaxed) % SCAN_WINDOW;
    (0..SCAN_WINDOW)
        .map(|i| start_port.wrapping_add((offset + i) % SCAN_WINDOW))
        .find(|&port| {
            port >= start_port && !docker_ports.contains(&port) && is_port_available(port)
        })
}

/// Returns true if a Docker container create/start error indicates the
/// requested host port lost the race described in the module docs — i.e. the
/// port was bindable when we checked but Docker's own bind failed because
/// another allocator grabbed it first. Safe to retry with a fresh port when
/// this returns true; any other error should propagate as-is.
pub fn is_port_conflict_error(message: &str) -> bool {
    message.contains("port is already allocated") || message.contains("address already in use")
}

/// Collect every host port currently published by a Docker container. Returns
/// an empty set (treat all ports as free) if Docker can't be listed, so callers
/// degrade to the OS-only check instead of failing.
async fn docker_published_ports(docker: &Docker) -> HashSet<u16> {
    let containers = match docker
        .list_containers(Some(ListContainersOptions {
            all: true,
            ..Default::default()
        }))
        .await
    {
        Ok(c) => c,
        Err(_) => return HashSet::new(),
    };

    let mut ports = HashSet::new();
    for container in containers {
        if let Some(port_mappings) = container.ports {
            for mapping in port_mappings {
                if let Some(public_port) = mapping.public_port {
                    ports.insert(public_port);
                }
            }
        }
    }
    ports
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_available_port_returns_at_or_after_base() {
        let port = find_available_port(28000).expect("a port should be free");
        assert!(port >= 28000, "port {} must be >= base 28000", port);
        assert!(is_port_available(port), "returned port must be bindable");
    }

    #[test]
    fn test_concurrent_allocations_diverge() {
        // Two back-to-back allocations from the same base should not collide,
        // because the offset counter advances between them.
        let a = find_available_port(28100).expect("first port");
        let b = find_available_port(28100).expect("second port");
        assert_ne!(
            a, b,
            "consecutive allocations must not return the same port"
        );
    }
}
