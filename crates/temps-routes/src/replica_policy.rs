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
    reserve_avoiding(backends, cursor, &[])
}

pub fn reserve_avoiding(
    backends: &[BackendEntry],
    cursor: usize,
    avoid: &[String],
) -> Option<(usize, InFlightGuard)> {
    for _ in 0..3 {
        let (index, had_capacity) = select_candidate(backends, cursor, avoid)?;
        if let Some(guard) = reserve_candidate(&backends[index], had_capacity) {
            return Some((index, guard));
        }
    }
    let (index, _) = select_candidate(backends, cursor, avoid)?;
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
    select_candidate(backends, cursor, &[]).map(|(index, _)| index)
}

/// O(replicas), no locks, allocations, or I/O. Rotation breaks equal-tier ties.
fn select_candidate(
    backends: &[BackendEntry],
    cursor: usize,
    avoid: &[String],
) -> Option<(usize, bool)> {
    select_candidate_after_snapshot(backends, cursor, avoid, || {})
}

fn select_candidate_after_snapshot(
    backends: &[BackendEntry],
    cursor: usize,
    avoid: &[String],
    after_snapshot: impl FnOnce(),
) -> Option<(usize, bool)> {
    if backends.is_empty() {
        return None;
    }
    let has_untried = backends.iter().any(|b| !avoid.contains(&b.address));
    let untried = |b: &BackendEntry| !has_untried || !avoid.contains(&b.address);
    let has_healthy = backends
        .iter()
        .any(|b| untried(b) && b.state.health.load(Ordering::Relaxed) == 1);
    after_snapshot();
    select_with_health(backends, cursor, &untried, has_healthy)
        .or_else(|| select_with_health(backends, cursor, &untried, false))
}

fn select_with_health(
    backends: &[BackendEntry],
    cursor: usize,
    untried: &impl Fn(&BackendEntry) -> bool,
    has_healthy: bool,
) -> Option<(usize, bool)> {
    let eligible = |b: &BackendEntry| {
        untried(b)
            && (!b.check_health || !has_healthy || b.state.health.load(Ordering::Relaxed) == 1)
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

/// Retry client initialization in bounded backoff cycles. Cancellation drops the
/// future; no separate task is left retrying after the scheduler is stopped.
pub(crate) async fn probe_client_with_retry(
    mut build: impl FnMut() -> Result<reqwest::Client, reqwest::Error>,
    retry: temps_core::retry::RetryConfig,
) -> reqwest::Client {
    loop {
        match retry.retry(|| std::future::ready(build())).await {
            Ok(client) => return client,
            Err(error) => {
                tracing::error!(%error, "Cannot initialize replica health probes; retrying");
                tokio::time::sleep(retry.max_delay).await;
            }
        }
    }
}

/// Treat every recorded readiness path as untrusted, including legacy job
/// outputs. Set URL components separately so a path cannot supply an authority.
pub(crate) fn probe_url(address: &str, path: &str) -> Option<String> {
    if !path.starts_with('/')
        || path.starts_with("//")
        || path.len() > 2048
        || path.contains('\\')
        || path.contains('#')
        || path.chars().any(char::is_control)
    {
        return None;
    }
    let mut url = reqwest::Url::parse(&format!("http://{address}/")).ok()?;
    let (path, query) = path
        .split_once('?')
        .map_or((path, None), |(path, query)| (path, Some(query)));
    url.set_path(path);
    url.set_query(query);
    Some(url.into())
}

#[derive(Clone, Debug)]
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
    #[test]
    fn probe_paths_cannot_replace_the_backend_authority() {
        for path in [
            "@other-host.example:8080/health",
            "http://other-host.example/health",
            "//other-host.example/health",
            "/\\other-host.example/health",
            "/health#fragment",
            "/health\r\nHost: other-host.example",
            "",
        ] {
            assert!(
                probe_url("127.0.0.1:9000", path).is_none(),
                "unsafe path was accepted"
            );
        }
        for path in [
            "/health",
            "/ready?full=true",
            "/@other-host.example:8080/health",
            "/https://other-host.example/health",
        ] {
            let value = probe_url("127.0.0.1:9000", path).expect("valid origin-relative path");
            let url = reqwest::Url::parse(&value).unwrap();
            assert_eq!(url.host_str(), Some("127.0.0.1"));
            assert_eq!(url.port(), Some(9000));
            assert_eq!(url.username(), "");
            assert_eq!(url.password(), None);
        }
        assert_eq!(
            probe_url("[::1]:9000", "/ready?full=true").as_deref(),
            Some("http://[::1]:9000/ready?full=true")
        );
    }

    fn entry(tier: usize) -> BackendEntry {
        BackendEntry {
            compose_service: None,
            pinned: None,
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
    async fn probe_client_recovers_after_a_failed_retry_cycle() {
        let mut attempts = 0;
        let _client = probe_client_with_retry(
            || {
                attempts += 1;
                if attempts < 4 {
                    reqwest::Client::builder().user_agent("\n").build()
                } else {
                    reqwest::Client::builder().build()
                }
            },
            temps_core::retry::RetryConfig::new(2)
                .with_base_delay(std::time::Duration::from_millis(1))
                .with_max_delay(std::time::Duration::from_millis(1)),
        )
        .await;
        assert_eq!(attempts, 4);
    }

    #[test]
    fn last_healthy_replica_failing_during_selection_still_routes_to_a_replica() {
        let backends = vec![entry(0), entry(1)];
        backends[0].state.health.store(1, Ordering::Relaxed);
        let selected = select_candidate_after_snapshot(&backends, 0, &[], || {
            backends[0].state.health.store(2, Ordering::Relaxed);
        });
        assert_eq!(selected, Some((0, true)));
    }

    #[test]
    fn retries_avoid_a_failed_preferred_replica_even_when_it_is_healthy() {
        let backends = vec![entry(0), entry(1)];
        backends[0].state.health.store(1, Ordering::Relaxed);
        let avoid = vec![backends[0].address.clone()];
        let (index, guard) = reserve_avoiding(&backends, 0, &avoid).unwrap();
        assert_eq!(index, 1);
        assert_eq!(backends[0].state.in_flight.load(Ordering::Relaxed), 0);
        assert_eq!(backends[1].state.in_flight.load(Ordering::Relaxed), 1);
        drop(guard);
        assert_eq!(backends[1].state.in_flight.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn retries_fail_open_after_every_replica_has_been_tried() {
        let backends = vec![entry(0), entry(1)];
        let avoid = backends
            .iter()
            .map(|b| b.address.clone())
            .collect::<Vec<_>>();
        let (index, _guard) = reserve_avoiding(&backends, 1, &avoid).unwrap();
        assert_eq!(index, 0);
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
        let (index, had_capacity) = select_candidate(&backends, 0, &[]).unwrap();
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
