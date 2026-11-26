// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runtime state is shared by every hostname for a replica and retained on reload.
use crate::BackendEntry;
use std::sync::{
    atomic::{AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering},
    Arc, OnceLock,
};
use std::time::{Duration, Instant};
use temps_entities::deployment_config::LoadBalancingConfig;

/// Consecutive failed connects (refused, timed out, no route) after which a
/// replica is taken out of rotation (passive outlier detection).
pub const OUTLIER_CONSECUTIVE_CONNECT_FAILURES: u32 = 3;
/// How long a replica is first taken out of rotation; each ejection that
/// follows a failed probe doubles it, up to [`OUTLIER_MAX_EJECTION`].
pub const OUTLIER_BASE_EJECTION: Duration = Duration::from_secs(5);
pub const OUTLIER_MAX_EJECTION: Duration = Duration::from_secs(60);

/// Milliseconds on a monotonic clock, never 0 (0 means "in rotation").
fn now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    let elapsed = START.get_or_init(Instant::now).elapsed().as_millis();
    u64::try_from(elapsed).unwrap_or(u64::MAX).saturating_add(1)
}

/// The low bits of [`ReplicaState::connect`]: consecutive failed connects,
/// saturating.
const CONNECT_FAILURES_MASK: u64 = (1 << 31) - 1;
/// Set in [`ReplicaState::connect`] while a request is out as the probe of an
/// ejection that ran out.
const PROBE_BIT: u64 = 1 << 31;
/// Above it, [`ReplicaState::connect`] holds a generation a successful
/// connect advances.
const GENERATION_SHIFT: u32 = 32;

/// A failed connect as it was counted, before it was decided whether it
/// takes the replica out of rotation.
#[derive(Debug, Clone, Copy)]
struct CountedFailure {
    /// The generation the failure was counted in.
    generation: u64,
    /// Consecutive failures, this one included.
    failures: u64,
    /// The request was the probe of an ejection that ran out.
    was_probe: bool,
}

/// What a recorded connect failure did to the replica.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectFailureOutcome {
    /// Still in rotation (below the threshold), or already out of it.
    Counted,
    /// Taken out of rotation for this long.
    Ejected(Duration),
}

#[derive(Debug, Default)]
pub struct ReplicaState {
    pub in_flight: AtomicUsize,
    // 0 unknown, 1 healthy, 2 unhealthy
    pub health: AtomicU8,
    successes: AtomicUsize,
    failures: AtomicUsize,
    /// Consecutive failed connects seen on the request path (the low 31
    /// bits), whether a probe is out ([`PROBE_BIT`]) and, above
    /// [`GENERATION_SHIFT`], a generation each successful connect advances.
    /// One word, so a failure is counted and a success resets the count
    /// atomically, and a failure can tell that a success came in between
    /// counting it and taking the replica out of rotation.
    connect: AtomicU64,
    /// Ejections since the replica last accepted a connection.
    ejections: AtomicU32,
    /// [`now_ms`] until which the replica is out of rotation; 0 when it is
    /// in rotation.
    ejected_until_ms: AtomicU64,
}

impl ReplicaState {
    /// Whether the replica is out of rotation. One atomic load while it is
    /// in rotation; the clock is read only while it is not.
    pub fn out_of_rotation(&self) -> bool {
        self.out_of_rotation_at(now_ms)
    }

    fn out_of_rotation_at(&self, now: impl FnOnce() -> u64) -> bool {
        let until = self.ejected_until_ms.load(Ordering::Relaxed);
        until != 0 && until > now()
    }

    /// Called for the replica a request was given: when its ejection has
    /// run out, exactly one request becomes its probe (half-open). The
    /// ejection is extended by [`OUTLIER_BASE_EJECTION`] while the probe is
    /// out, so other requests keep avoiding the replica, and a probe whose
    /// outcome is never recorded (the request was cancelled) cannot keep it
    /// out of rotation for longer than that.
    ///
    /// Returns whether the request may use the replica as a replica in
    /// rotation: it was never ejected, or this request became its probe.
    /// `false` means it is still ejected (another request claimed the probe
    /// first, or it was chosen only because every replica is ejected).
    fn claim_probe_if_due(&self, now: impl FnOnce() -> u64) -> bool {
        let until = self.ejected_until_ms.load(Ordering::Relaxed);
        if until == 0 {
            return true;
        }
        let now = now();
        if until > now {
            return false;
        }
        let grace = u64::try_from(OUTLIER_BASE_EJECTION.as_millis()).unwrap_or(u64::MAX);
        let claimed = self
            .ejected_until_ms
            .compare_exchange(
                until,
                now.saturating_add(grace),
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .is_ok();
        if claimed {
            self.mark_probe();
        }
        claimed
    }

    /// The request this replica was given is the probe of its ejection: its
    /// failed connect re-ejects the replica without waiting for the threshold.
    fn mark_probe(&self) {
        self.connect.fetch_or(PROBE_BIT, Ordering::SeqCst);
    }

    /// A new connection to the replica could not be made (refused, timed
    /// out or no route). After [`OUTLIER_CONSECUTIVE_CONNECT_FAILURES`] in a
    /// row, or when the probe after an ejection fails, the replica leaves
    /// rotation for a backoff that doubles each time.
    pub fn record_connect_failure(&self) -> ConnectFailureOutcome {
        self.record_connect_failure_at(now_ms())
    }

    fn record_connect_failure_at(&self, now: u64) -> ConnectFailureOutcome {
        let counted = self.count_connect_failure();
        self.decide_after_failure(counted, now)
    }

    /// Count a failed connect and take the probe flag, in one step.
    fn count_connect_failure(&self) -> CountedFailure {
        let (Ok(previous) | Err(previous)) =
            self.connect
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |word| {
                    let failures = ((word & CONNECT_FAILURES_MASK) + 1).min(CONNECT_FAILURES_MASK);
                    Some((word & !(CONNECT_FAILURES_MASK | PROBE_BIT)) | failures)
                });
        CountedFailure {
            generation: previous >> GENERATION_SHIFT,
            failures: ((previous & CONNECT_FAILURES_MASK) + 1).min(CONNECT_FAILURES_MASK),
            was_probe: previous & PROBE_BIT != 0,
        }
    }

    /// Take the replica out of rotation if `counted` is the failure that
    /// reaches the threshold, or the failed probe of an ejection.
    ///
    /// A successful connect may run between counting the failure and the
    /// ejection: it resets the count, and finds no ejection yet to clear.
    /// Publishing the ejection anyway would leave a replica that just
    /// accepted a connection out of rotation, which neither order of the two
    /// produces. So the ejection is published, and then the generation
    /// re-read: either the success saw the ejection and cleared it, or this
    /// sees the generation it advanced and withdraws the ejection (both
    /// sides are `SeqCst`, so at least one of them sees the other).
    fn decide_after_failure(&self, counted: CountedFailure, now: u64) -> ConnectFailureOutcome {
        let until = self.ejected_until_ms.load(Ordering::SeqCst);
        let ejected_now = until != 0 && until > now;
        if !counted.was_probe
            && (counted.failures < u64::from(OUTLIER_CONSECUTIVE_CONNECT_FAILURES) || ejected_now)
        {
            return ConnectFailureOutcome::Counted;
        }
        let previous = self.ejections.fetch_add(1, Ordering::Relaxed);
        let backoff = OUTLIER_BASE_EJECTION
            .saturating_mul(1u32 << previous.min(16))
            .min(OUTLIER_MAX_EJECTION);
        let millis = u64::try_from(backoff.as_millis()).unwrap_or(u64::MAX);
        let ejected_until = now.saturating_add(millis);
        self.ejected_until_ms.store(ejected_until, Ordering::SeqCst);
        if self.connect.load(Ordering::SeqCst) >> GENERATION_SHIFT != counted.generation {
            // A connection was made since this failure was counted.
            if self
                .ejected_until_ms
                .compare_exchange(ejected_until, 0, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                let _ = self
                    .ejections
                    .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                        Some(n.saturating_sub(1))
                    });
            }
            return ConnectFailureOutcome::Counted;
        }
        ConnectFailureOutcome::Ejected(backoff)
    }

    /// A new connection to the replica was made: it is back in rotation and
    /// its failures are forgotten. Loads only, while nothing is recorded.
    pub fn record_connect_success(&self) {
        let word = self.connect.load(Ordering::SeqCst);
        if word & (CONNECT_FAILURES_MASK | PROBE_BIT) == 0
            && self.ejected_until_ms.load(Ordering::SeqCst) == 0
            && self.ejections.load(Ordering::Relaxed) == 0
        {
            return;
        }
        // A new generation, so a failure counted before this connection
        // withdraws an ejection it publishes after it (see
        // `decide_after_failure`).
        let _ = self
            .connect
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |word| {
                Some(
                    word.wrapping_add(1 << GENERATION_SHIFT) & !(CONNECT_FAILURES_MASK | PROBE_BIT),
                )
            });
        let until = self.ejected_until_ms.load(Ordering::SeqCst);
        if until != 0 {
            // Not a store: an ejection published since the load above is
            // one this success did not see, and the failure that published
            // it withdraws it itself if it was counted before this success.
            let _ = self.ejected_until_ms.compare_exchange(
                until,
                0,
                Ordering::SeqCst,
                Ordering::SeqCst,
            );
        }
        self.ejections.store(0, Ordering::Relaxed);
    }

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

    /// The state of the replica this request was given.
    pub fn state(&self) -> &ReplicaState {
        &self.0
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
            // Two requests can select a replica whose ejection just ran out;
            // only one becomes its probe. The other gives the reservation back
            // and selects again while another replica is in rotation.
            if !backends[index].state.claim_probe_if_due(now_ms)
                && another_in_rotation(backends, index, avoid)
            {
                drop(guard);
                continue;
            }
            return Some((index, guard));
        }
    }
    let (index, _) = select_candidate(backends, cursor, avoid)?;
    backends[index].state.claim_probe_if_due(now_ms);
    Some((index, InFlightGuard::acquire(backends[index].state.clone())))
}

/// Whether a replica other than `index` that this request has not tried is
/// in rotation. Only reached when a probe claim is lost, so off the common path.
fn another_in_rotation(backends: &[BackendEntry], index: usize, avoid: &[String]) -> bool {
    backends
        .iter()
        .enumerate()
        .any(|(i, b)| i != index && !avoid.contains(&b.address) && !b.state.out_of_rotation())
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
    let tried_or_untried = |b: &BackendEntry| !has_untried || !avoid.contains(&b.address);
    // Replicas the request path took out of rotation are skipped while any
    // other candidate is in it; a route whose every replica is out still
    // tries them, so ejection can never make it unreachable on its own.
    let now = std::cell::OnceCell::new();
    let out = |b: &BackendEntry| b.state.out_of_rotation_at(|| *now.get_or_init(now_ms));
    let has_in_rotation = backends.iter().any(|b| tried_or_untried(b) && !out(b));
    let untried = |b: &BackendEntry| tried_or_untried(b) && (!has_in_rotation || !out(b));
    let has_healthy = backends
        .iter()
        .any(|b| untried(b) && b.state.health.load(Ordering::Relaxed) == 1);
    after_snapshot();
    select_with_health(backends, cursor, &untried, has_healthy)
        .or_else(|| select_with_health(backends, cursor, &untried, false))
        // Every replica seen in rotation was ejected after the snapshot: the
        // route is all-ejected now, and still answers.
        .or_else(|| select_with_health(backends, cursor, &tried_or_untried, false))
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
    fn eject(state: &ReplicaState, now: u64) -> ConnectFailureOutcome {
        let mut outcome = ConnectFailureOutcome::Counted;
        for _ in 0..OUTLIER_CONSECUTIVE_CONNECT_FAILURES {
            outcome = state.record_connect_failure_at(now);
        }
        outcome
    }

    #[test]
    fn a_replica_refusing_connections_leaves_rotation_after_consecutive_failures() {
        let backends = vec![entry(0), entry(0)];
        let now = now_ms();
        for _ in 1..OUTLIER_CONSECUTIVE_CONNECT_FAILURES {
            assert_eq!(
                backends[0].state.record_connect_failure_at(now),
                ConnectFailureOutcome::Counted
            );
        }
        assert!(!backends[0].state.out_of_rotation());
        assert_eq!(
            backends[0].state.record_connect_failure_at(now),
            ConnectFailureOutcome::Ejected(OUTLIER_BASE_EJECTION)
        );
        assert!(backends[0].state.out_of_rotation());
        // Rotation would pick replica 0 on even cursors; while it is out,
        // every request goes to the replica that still accepts connections.
        for cursor in 0..6 {
            let (index, _guard) = reserve(&backends, cursor).unwrap();
            assert_eq!(index, 1, "cursor {cursor} chose the ejected replica");
        }
    }

    #[test]
    fn a_success_between_failures_keeps_the_replica_in_rotation() {
        let state = ReplicaState::default();
        let now = now_ms();
        for _ in 0..OUTLIER_CONSECUTIVE_CONNECT_FAILURES * 3 {
            state.record_connect_failure_at(now);
            state.record_connect_success();
        }
        assert!(!state.out_of_rotation());
    }

    #[test]
    fn a_route_whose_every_replica_is_ejected_still_routes() {
        let backends = vec![entry(0), entry(1)];
        let now = now_ms();
        eject(&backends[0].state, now);
        eject(&backends[1].state, now);
        assert_eq!(select(&backends, 0), Some(0));
        // A failure seen while failing open does not extend the ejection.
        assert_eq!(
            backends[0].state.record_connect_failure_at(now),
            ConnectFailureOutcome::Counted
        );
    }

    #[test]
    fn a_replica_ejected_after_the_snapshot_leaves_an_all_ejected_route_that_still_routes() {
        let backends = vec![entry(0), entry(1)];
        let now = now_ms();
        eject(&backends[1].state, now);
        // The snapshot sees replica 0 in rotation; another request ejects it
        // before this one picks.
        let selected = select_candidate_after_snapshot(&backends, 0, &[], || {
            eject(&backends[0].state, now);
        });
        assert!(selected.is_some(), "an all-ejected route must still answer");
    }

    #[test]
    fn only_the_request_that_wins_the_probe_is_admitted_to_an_expired_replica() {
        let backends = vec![entry(0), entry(0)];
        let state = &backends[0].state;
        assert!(state.claim_probe_if_due(now_ms), "never ejected");
        eject(state, now_ms());
        assert!(!state.claim_probe_if_due(now_ms), "still ejected");
        state.ejected_until_ms.store(1, Ordering::Relaxed);
        // Two requests chose replica 0 from the same snapshot.
        assert!(
            state.claim_probe_if_due(now_ms),
            "the first becomes the probe"
        );
        assert!(
            !state.claim_probe_if_due(now_ms),
            "the second is not admitted"
        );
        // ... and selects again, because replica 1 is in rotation.
        assert!(another_in_rotation(&backends, 0, &[]));
        eject(&backends[1].state, now_ms());
        assert!(
            !another_in_rotation(&backends, 0, &[]),
            "with every replica ejected the request keeps the one it got"
        );
    }

    #[test]
    fn an_expired_ejection_sends_one_probe_and_a_failed_probe_doubles_the_backoff() {
        let backends = vec![entry(0), entry(0)];
        let state = &backends[0].state;
        eject(state, now_ms());
        assert!(state.out_of_rotation());
        // Let the ejection run out (the earliest instant on the clock).
        state.ejected_until_ms.store(1, Ordering::Relaxed);
        assert!(!state.out_of_rotation());

        let (first, _probe) = reserve(&backends, 0).unwrap();
        assert_eq!(first, 0, "the expired replica is probed");
        assert!(state.out_of_rotation(), "only one probe is in flight");
        let (second, _other) = reserve(&backends, 0).unwrap();
        assert_eq!(second, 1);

        assert_eq!(
            state.record_connect_failure_at(now_ms()),
            ConnectFailureOutcome::Ejected(OUTLIER_BASE_EJECTION * 2)
        );
        assert!(state.out_of_rotation());
    }

    #[test]
    fn a_success_while_the_third_failure_is_recorded_keeps_the_replica_in_rotation() {
        let state = ReplicaState::default();
        let now = now_ms();
        for _ in 0..2 {
            assert_eq!(
                state.record_connect_failure_at(now),
                ConnectFailureOutcome::Counted
            );
        }
        // The third failure is counted, then a connection is made before it
        // decides to eject.
        let third = state.count_connect_failure();
        state.record_connect_success();
        assert_eq!(
            state.decide_after_failure(third, now),
            ConnectFailureOutcome::Counted
        );
        assert!(!state.out_of_rotation());
        assert_eq!(state.ejections.load(Ordering::Relaxed), 0);

        // Three failures in a row with no connection between still eject.
        for _ in 0..2 {
            assert_eq!(
                state.record_connect_failure_at(now),
                ConnectFailureOutcome::Counted
            );
        }
        assert_eq!(
            state.record_connect_failure_at(now),
            ConnectFailureOutcome::Ejected(OUTLIER_BASE_EJECTION)
        );
        assert!(state.out_of_rotation());
    }

    #[test]
    fn a_success_while_a_failed_probe_is_recorded_keeps_the_replica_in_rotation() {
        let state = ReplicaState::default();
        let now = now_ms();
        eject(&state, now);
        // The ejection ran out and a request became its probe.
        state.ejected_until_ms.store(1, Ordering::Relaxed);
        assert!(state.claim_probe_if_due(now_ms));
        let probe = state.count_connect_failure();
        // Another request (every replica ejected) connected meanwhile.
        state.record_connect_success();
        assert_eq!(
            state.decide_after_failure(probe, now),
            ConnectFailureOutcome::Counted
        );
        assert!(!state.out_of_rotation());
    }

    #[test]
    fn the_ejection_backoff_is_capped_and_reset_by_a_successful_connect() {
        let state = ReplicaState::default();
        let now = now_ms();
        let mut last = Duration::ZERO;
        for _ in 0..10 {
            state.mark_probe();
            if let ConnectFailureOutcome::Ejected(backoff) = state.record_connect_failure_at(now) {
                last = backoff;
            }
        }
        assert_eq!(last, OUTLIER_MAX_EJECTION);
        state.record_connect_success();
        assert!(!state.out_of_rotation());
        assert_eq!(
            eject(&state, now),
            ConnectFailureOutcome::Ejected(OUTLIER_BASE_EJECTION)
        );
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
