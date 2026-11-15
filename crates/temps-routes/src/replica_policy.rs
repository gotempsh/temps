// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runtime state is shared by every hostname for a replica and retained on reload.
use crate::BackendEntry;
use std::sync::{
    atomic::{AtomicU8, AtomicUsize, Ordering},
    Arc,
};
use temps_entities::deployment_config::LoadBalancingConfig;

#[derive(Debug, Default)]
pub struct ReplicaState {
    pub in_flight: AtomicUsize,
    // 0 unknown, 1 healthy, 2 unhealthy
    pub health: AtomicU8,
    successes: AtomicUsize,
    failures: AtomicUsize,
}

impl ReplicaState {
    pub fn record_probe(&self, passed: bool, config: &LoadBalancingConfig) {
        if passed {
            self.failures.store(0, Ordering::Relaxed);
            if self.successes.fetch_add(1, Ordering::Relaxed) + 1
                >= config.health_check.healthy_threshold as usize
            {
                self.successes.store(0, Ordering::Relaxed);
                self.health.store(1, Ordering::Relaxed);
            }
        } else {
            self.successes.store(0, Ordering::Relaxed);
            if self.failures.fetch_add(1, Ordering::Relaxed) + 1
                >= config.health_check.unhealthy_threshold as usize
            {
                self.failures.store(0, Ordering::Relaxed);
                self.health.store(2, Ordering::Relaxed);
            }
        }
    }
}

#[derive(Debug)]
pub struct InFlightGuard(Arc<ReplicaState>);
impl InFlightGuard {
    pub fn acquire(state: Arc<ReplicaState>) -> Self {
        state.in_flight.fetch_add(1, Ordering::Relaxed);
        Self(state)
    }
}
impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Reserve a slot with a bounded CAS retry so concurrent requests do not all
/// consume the last free slot of the preferred tier. On sustained contention
/// use the current choice rather than spin indefinitely on the hot path.
pub fn reserve(backends: &[BackendEntry], cursor: usize) -> Option<(usize, InFlightGuard)> {
    for _ in 0..3 {
        let (index, had_capacity) = select_candidate(backends, cursor)?;
        if let Some(guard) = reserve_candidate(&backends[index], had_capacity) {
            return Some((index, guard));
        }
    }
    let (index, _) = select_candidate(backends, cursor)?;
    Some((index, InFlightGuard::acquire(backends[index].state.clone())))
}

fn reserve_candidate(backend: &BackendEntry, had_capacity: bool) -> Option<InFlightGuard> {
    if had_capacity {
        if let Some(cap) = backend.capacity {
            let count = backend.state.in_flight.load(Ordering::Relaxed);
            // Selection may have seen the last free slot just before another
            // request reserved it. Re-select instead of treating that race as
            // the intentional fallback for an entirely saturated environment.
            if count >= cap as usize {
                return None;
            }
            backend
                .state
                .in_flight
                .compare_exchange(count, count + 1, Ordering::Relaxed, Ordering::Relaxed)
                .ok()?;
            return Some(InFlightGuard(backend.state.clone()));
        }
    }
    Some(InFlightGuard::acquire(backend.state.clone()))
}

#[cfg(test)]
fn select(backends: &[BackendEntry], cursor: usize) -> Option<usize> {
    select_candidate(backends, cursor).map(|(index, _)| index)
}

/// O(replicas), no locks, allocations, or I/O. Rotation breaks equal-tier ties.
fn select_candidate(backends: &[BackendEntry], cursor: usize) -> Option<(usize, bool)> {
    if backends.is_empty() {
        return None;
    }
    let has_healthy = backends
        .iter()
        .any(|b| b.state.health.load(Ordering::Relaxed) == 1);
    let eligible = |b: &BackendEntry| {
        !b.check_health || !has_healthy || b.state.health.load(Ordering::Relaxed) == 1
    };
    let mut best = None;
    let mut least = None;
    for offset in 0..backends.len() {
        let i = (cursor.wrapping_add(offset)) % backends.len();
        let b = &backends[i];
        if !eligible(b) {
            continue;
        }
        let count = b.state.in_flight.load(Ordering::Relaxed);
        if least.is_none_or(|(_, n)| count < n) {
            least = Some((i, count));
        }
        if b.capacity.is_none_or(|limit| count < limit as usize)
            && best.is_none_or(|(_, tier)| b.tier < tier)
        {
            best = Some((i, b.tier));
        }
    }
    let (chosen, _) = best.or(least)?;
    let tier = backends[chosen].tier;
    let is_best = |b: &BackendEntry| {
        eligible(b)
            && if best.is_some() {
                b.tier == tier
                    && b.capacity.is_none_or(|limit| {
                        b.state.in_flight.load(Ordering::Relaxed) < limit as usize
                    })
            } else {
                b.state.in_flight.load(Ordering::Relaxed) == least.map(|(_, n)| n).unwrap_or(0)
            }
    };
    let count = backends.iter().filter(|b| is_best(b)).count();
    if count == 0 {
        return Some((chosen, best.is_some()));
    }
    backends
        .iter()
        .enumerate()
        .filter(|(_, b)| is_best(b))
        .nth(cursor % count)
        .map(|(i, _)| (i, best.is_some()))
        .or(Some((chosen, best.is_some())))
}

#[derive(Clone)]
pub struct Probe {
    pub state: Arc<ReplicaState>,
    pub url: String,
    pub config: LoadBalancingConfig,
}

impl Probe {
    pub async fn check(&self, client: &reqwest::Client) {
        let passed = client
            .get(&self.url)
            .timeout(std::time::Duration::from_secs(u64::from(
                self.config.health_check.timeout_seconds,
            )))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success());
        self.state.record_probe(passed, &self.config);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(tier: usize) -> BackendEntry {
        BackendEntry {
            address: format!("127.0.0.1:{}", 8000 + tier),
            container_id: None,
            container_name: None,
            node_id: None,
            state: Arc::default(),
            tier,
            capacity: Some(1),
            check_health: true,
        }
    }
    #[tokio::test]
    async fn accepting_connections_without_http_response_becomes_unhealthy() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        let config = LoadBalancingConfig {
            health_check: temps_entities::deployment_config::ActiveHealthCheck {
                timeout_seconds: 1,
                unhealthy_threshold: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let probe = Probe {
            state: Arc::default(),
            url: format!("http://{address}/health"),
            config,
        };
        probe
            .check(&reqwest::Client::builder().no_proxy().build().unwrap())
            .await;
        assert_eq!(probe.state.health.load(Ordering::Relaxed), 2);
        server.abort();
    }

    #[test]
    fn aliases_share_occupancy_and_release_at_request_end() {
        let route = crate::BackendType::Upstream {
            backends: vec![entry(0), entry(1)],
            round_robin_counter: Arc::new(AtomicUsize::new(0)),
        };
        let alias = route.clone();
        let request = route.get_backend().unwrap();
        assert_eq!(request.address, "127.0.0.1:8000");
        let overflow = alias.get_backend().unwrap();
        assert_eq!(overflow.address, "127.0.0.1:8001");
        drop(request);
        let recovered = route.get_backend().unwrap();
        assert_eq!(recovered.address, "127.0.0.1:8000");
        drop(overflow);
        drop(recovered);
        if let crate::BackendType::Upstream { backends, .. } = route {
            assert!(backends
                .iter()
                .all(|b| b.state.in_flight.load(Ordering::Relaxed) == 0));
        }
    }

    #[test]
    fn a_candidate_filled_after_selection_retries_into_the_overflow_tier() {
        let backends = vec![entry(0), entry(1)];
        let (index, had_capacity) = select_candidate(&backends, 0).unwrap();
        assert_eq!(index, 0);
        let competing_request = InFlightGuard::acquire(backends[0].state.clone());
        assert!(reserve_candidate(&backends[index], had_capacity).is_none());
        let (overflow, guard) = reserve(&backends, 0).unwrap();
        assert_eq!(overflow, 1);
        assert_eq!(backends[0].state.in_flight.load(Ordering::Relaxed), 1);
        drop(guard);
        drop(competing_request);
    }

    #[test]
    fn equal_tiers_rotate_and_full_tiers_use_least_loaded() {
        let backends = vec![entry(0), entry(0), entry(2)];
        assert_eq!(select(&backends, 0), Some(0));
        assert_eq!(select(&backends, 1), Some(1));
        assert_eq!(select(&backends, 2), Some(0));
        backends[0].state.in_flight.store(3, Ordering::Relaxed);
        backends[1].state.in_flight.store(2, Ordering::Relaxed);
        backends[2].state.in_flight.store(1, Ordering::Relaxed);
        assert_eq!(select(&backends, 0), Some(2));
    }

    #[test]
    fn preferred_overflow_health_and_fail_open() {
        let backends = vec![entry(0), entry(1)];
        assert_eq!(select(&backends, 1), Some(0));
        let guard = InFlightGuard::acquire(backends[0].state.clone());
        assert_eq!(select(&backends, 0), Some(1));
        drop(guard);
        assert_eq!(select(&backends, 1), Some(0));
        backends[0].state.health.store(2, Ordering::Relaxed);
        backends[1].state.health.store(1, Ordering::Relaxed);
        assert_eq!(select(&backends, 0), Some(1));
        backends[1].state.health.store(2, Ordering::Relaxed);
        assert_eq!(select(&backends, 1), Some(0));
    }
    #[test]
    fn thresholds_and_guard_release() {
        let s = Arc::new(ReplicaState::default());
        let config = LoadBalancingConfig::default();
        for _ in 0..2 {
            s.record_probe(false, &config);
        }
        assert_eq!(s.health.load(Ordering::Relaxed), 0);
        s.record_probe(false, &config);
        assert_eq!(s.health.load(Ordering::Relaxed), 2);
        s.record_probe(true, &config);
        assert_eq!(s.health.load(Ordering::Relaxed), 2);
        s.record_probe(true, &config);
        assert_eq!(s.health.load(Ordering::Relaxed), 1);
        {
            let _guard = InFlightGuard::acquire(s.clone());
            assert_eq!(s.in_flight.load(Ordering::Relaxed), 1);
        }
        assert_eq!(s.in_flight.load(Ordering::Relaxed), 0);
    }
}
