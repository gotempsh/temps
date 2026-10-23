// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One activator for scale-to-zero (ADR-055 §11, §14).
//!
//! A scale-to-zero target (a function, an on-demand environment on the
//! workload plane) keeps its route while nothing runs. The first request
//! for it reaches the activator, which wakes it once, holds every request
//! that arrives meanwhile, and lets them through when the target is ready.
//! A sweep puts targets that saw no request for their idle time back to
//! sleep.
//!
//! The activator is the mechanism; what waking and sleeping *mean* is a
//! product's [`ActivationBackend`]: start a function's instance through the
//! workload plane, start an environment's containers.
//!
//! # The request path
//!
//! The proxy calls [`Activator::touch`], [`Activator::key_for_host`] and
//! [`Activator::activate`] per request. For an awake target they are an
//! `ArcSwap` load, a hash lookup and a few atomic operations: no lock, no
//! I/O. Only a request for a target that is not awake parks, and parking is
//! bounded: at most [`ActivatorConfig::max_waiters`] requests wait at once,
//! and the next gets [`ActivationError::Saturated`] at once (the proxy
//! answers a retryable `503`).
//!
//! # Coordination
//!
//! Each target has one slot with an atomic state (`asleep`, `waking`,
//! `awake`, `sleeping`). Waking is a compare-and-swap from `asleep`, so
//! however many requests arrive together exactly one wake runs. The wake
//! runs in its own task, so a client that disconnects does not cancel it.
//! Waiters subscribe to the slot's `watch` channel before they read the
//! state, so no transition is missed.
//!
//! # Memory and background work
//!
//! One slot per registered target (a few dozen bytes) and one host entry
//! per routed hostname. The sweep is O(registered targets) per tick and
//! does nothing for a target that is asleep.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use arc_swap::{ArcSwap, ArcSwapOption};
use async_trait::async_trait;
use tokio::sync::{watch, Semaphore};
use tokio::task::JoinHandle;

/// What kind of product object a target is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ActivationKind {
    /// An on-demand environment (`environments.on_demand`).
    Environment,
    /// A function (ADR-055 phase 10).
    Function,
}

impl ActivationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ActivationKind::Environment => "environment",
            ActivationKind::Function => "function",
        }
    }
}

/// A target: a product object, by kind and id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ActivationKey {
    pub kind: ActivationKind,
    pub id: i64,
}

impl ActivationKey {
    pub fn new(kind: ActivationKind, id: i64) -> Self {
        Self { kind, id }
    }
}

impl fmt::Display for ActivationKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.kind.as_str(), self.id)
    }
}

/// How a target sleeps and wakes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Registration {
    /// Seconds without a request before the sweep puts it to sleep.
    pub idle_secs: u32,
    /// Seconds a wake may take before waiting requests are failed.
    pub wake_timeout_secs: u32,
    /// Whether it is running now (its stored state).
    pub awake: bool,
}

/// What a successful wake produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Woken {
    /// Where requests go (`ip:port`), when the backend decides it; `None`
    /// when the product's own router resolves the upstream (environments).
    pub address: Option<String>,
}

/// What waking and sleeping mean for one kind of target.
#[async_trait]
pub trait ActivationBackend: Send + Sync {
    /// Start the target; return once it is ready for traffic. Errors are a
    /// message for the requests that waited, naming the target and why.
    async fn wake(&self, key: ActivationKey) -> Result<Woken, String>;
    /// Stop the target. `Ok(false)` when it was already stopped.
    async fn sleep(&self, key: ActivationKey) -> Result<bool, String>;
}

/// Why a request could not be let through.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ActivationError {
    #[error("{key} is not registered with the activator")]
    NotRegistered { key: ActivationKey },
    #[error("{key} cannot be woken: no activation backend is registered for its kind")]
    NoBackend { key: ActivationKey },
    #[error("{key} is waking and {limit} requests are already waiting; retry shortly")]
    Saturated { key: ActivationKey, limit: usize },
    #[error("waking {key} failed: {reason}")]
    WakeFailed { key: ActivationKey, reason: String },
    #[error("{key} did not become ready within {timeout_secs}s")]
    WakeTimeout {
        key: ActivationKey,
        timeout_secs: u64,
    },
}

/// The outcome of a request let through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activation {
    /// The upstream the backend named, if any (see [`Woken::address`]).
    pub address: Option<Arc<String>>,
    /// Whether this request waited for a wake (it was not awake).
    pub waited: bool,
}

#[derive(Debug, Clone)]
pub struct ActivatorConfig {
    /// Requests that may wait for a wake at once, over every target.
    pub max_waiters: usize,
    /// How long one sleep may take before it is given up (and the target
    /// stays awake).
    pub sleep_timeout: Duration,
    /// Extra time a waiter allows beyond the target's wake timeout, so the
    /// wake task's own timeout reports first.
    pub wait_grace: Duration,
}

impl Default for ActivatorConfig {
    fn default() -> Self {
        Self {
            max_waiters: 256,
            sleep_timeout: Duration::from_secs(60),
            wait_grace: Duration::from_secs(2),
        }
    }
}

const ASLEEP: u8 = 0;
const WAKING: u8 = 1;
const AWAKE: u8 = 2;
const SLEEPING: u8 = 3;

/// A target's state, as reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationState {
    Asleep,
    Waking,
    Awake,
    Sleeping,
}

impl ActivationState {
    fn from_raw(raw: u8) -> Self {
        match raw {
            WAKING => ActivationState::Waking,
            AWAKE => ActivationState::Awake,
            SLEEPING => ActivationState::Sleeping,
            _ => ActivationState::Asleep,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ActivationState::Asleep => "asleep",
            ActivationState::Waking => "waking",
            ActivationState::Awake => "awake",
            ActivationState::Sleeping => "sleeping",
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

struct Slot {
    key: ActivationKey,
    idle_secs: AtomicU32,
    wake_timeout_secs: AtomicU32,
    state: AtomicU8,
    /// Epoch seconds of the last request.
    last_activity: AtomicU64,
    /// Bumped on every state transition.
    changed: watch::Sender<u64>,
    address: ArcSwapOption<String>,
    /// Why the last wake failed. Written only on failure.
    last_error: ArcSwapOption<String>,
}

impl Slot {
    fn new(key: ActivationKey, registration: Registration) -> Self {
        Self {
            key,
            idle_secs: AtomicU32::new(registration.idle_secs),
            wake_timeout_secs: AtomicU32::new(registration.wake_timeout_secs),
            state: AtomicU8::new(if registration.awake { AWAKE } else { ASLEEP }),
            last_activity: AtomicU64::new(now_secs()),
            changed: watch::channel(0).0,
            address: ArcSwapOption::empty(),
            last_error: ArcSwapOption::empty(),
        }
    }

    fn state(&self) -> u8 {
        self.state.load(Ordering::Acquire)
    }

    fn transition(&self, from: u8, to: u8) -> bool {
        let won = self
            .state
            .compare_exchange(from, to, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if won {
            self.changed.send_modify(|seq| *seq = seq.wrapping_add(1));
        }
        won
    }

    fn set(&self, to: u8) {
        self.state.store(to, Ordering::Release);
        self.changed.send_modify(|seq| *seq = seq.wrapping_add(1));
    }

    fn touch(&self, now: u64) {
        // `fetch_max`: a late writer never moves activity backwards.
        self.last_activity.fetch_max(now, Ordering::Relaxed);
    }

    fn wake_timeout(&self) -> Duration {
        Duration::from_secs(u64::from(
            self.wake_timeout_secs.load(Ordering::Relaxed).max(1),
        ))
    }

    fn set_timeouts(&self, registration: Registration) {
        self.idle_secs
            .store(registration.idle_secs, Ordering::Relaxed);
        self.wake_timeout_secs
            .store(registration.wake_timeout_secs, Ordering::Relaxed);
    }
}

/// Counters for the activator, all atomic.
#[derive(Debug, Default)]
struct ActivatorMetrics {
    wakes: AtomicU64,
    wake_failures: AtomicU64,
    sleeps: AtomicU64,
    sleep_failures: AtomicU64,
    saturated: AtomicU64,
    waited: AtomicU64,
}

/// A copy of the activator's counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MetricsSnapshot {
    pub wakes: u64,
    pub wake_failures: u64,
    pub sleeps: u64,
    pub sleep_failures: u64,
    /// Requests refused because `max_waiters` were already waiting.
    pub saturated: u64,
    /// Requests that waited for a wake.
    pub waited: u64,
}

/// What one sweep did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepOutcome {
    pub slept: Vec<ActivationKey>,
    pub failed: Vec<ActivationKey>,
}

type Slots = HashMap<ActivationKey, Arc<Slot>>;
type Backends = HashMap<ActivationKind, Arc<dyn ActivationBackend>>;
type Hosts = HashMap<String, ActivationKey>;

/// The activator. One per process, shared by the proxy and the products.
pub struct Activator {
    slots: ArcSwap<Slots>,
    hosts: ArcSwap<Hosts>,
    backends: ArcSwap<Backends>,
    /// Serialises writers of `slots`, `hosts` and `backends` (registration,
    /// a control-plane operation). Never taken on the request path.
    writer: Mutex<()>,
    waiters: Arc<Semaphore>,
    config: ActivatorConfig,
    metrics: Arc<ActivatorMetrics>,
}

impl fmt::Debug for Activator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Activator")
            .field("targets", &self.slots.load().len())
            .field("hosts", &self.hosts.load().len())
            .field("config", &self.config)
            .finish()
    }
}

impl Default for Activator {
    fn default() -> Self {
        Self::new(ActivatorConfig::default())
    }
}

impl Activator {
    pub fn new(config: ActivatorConfig) -> Self {
        Self {
            slots: ArcSwap::from_pointee(HashMap::new()),
            hosts: ArcSwap::from_pointee(HashMap::new()),
            backends: ArcSwap::from_pointee(HashMap::new()),
            writer: Mutex::new(()),
            waiters: Arc::new(Semaphore::new(config.max_waiters)),
            config,
            metrics: Arc::new(ActivatorMetrics::default()),
        }
    }

    fn write_lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Set how `kind` targets wake and sleep. Replaces an earlier backend.
    pub fn set_backend(&self, kind: ActivationKind, backend: Arc<dyn ActivationBackend>) {
        let _guard = self.write_lock();
        let mut backends: Backends = (**self.backends.load()).clone();
        backends.insert(kind, backend);
        self.backends.store(Arc::new(backends));
    }

    pub fn has_backend(&self, kind: ActivationKind) -> bool {
        self.backends.load().contains_key(&kind)
    }

    /// Register `key`, or update its idle and wake timeouts. An existing
    /// target keeps its state and activity: a route reload re-registering
    /// every target does not wake or sleep anything.
    pub fn register(&self, key: ActivationKey, registration: Registration) {
        let _guard = self.write_lock();
        let current = self.slots.load();
        if let Some(slot) = current.get(&key) {
            slot.set_timeouts(registration);
            return;
        }
        let mut slots: Slots = (**current).clone();
        slots.insert(key, Arc::new(Slot::new(key, registration)));
        self.slots.store(Arc::new(slots));
    }

    /// Replace every `kind` target with `targets`: new ones are added with
    /// their stored state, existing ones keep theirs (only their timeouts
    /// change), and ones missing from `targets` are removed.
    pub fn replace_kind(
        &self,
        kind: ActivationKind,
        targets: impl IntoIterator<Item = (i64, Registration)>,
    ) {
        let _guard = self.write_lock();
        let current = self.slots.load();
        let mut slots: Slots = current
            .iter()
            .filter(|(key, _)| key.kind != kind)
            .map(|(key, slot)| (*key, Arc::clone(slot)))
            .collect();
        for (id, registration) in targets {
            let key = ActivationKey::new(kind, id);
            let slot = match current.get(&key) {
                Some(slot) => {
                    slot.set_timeouts(registration);
                    Arc::clone(slot)
                }
                None => Arc::new(Slot::new(key, registration)),
            };
            slots.insert(key, slot);
        }
        self.slots.store(Arc::new(slots));
    }

    /// Stop tracking `key` and its hostnames.
    pub fn unregister(&self, key: ActivationKey) {
        let _guard = self.write_lock();
        let mut slots: Slots = (**self.slots.load()).clone();
        if slots.remove(&key).is_some() {
            self.slots.store(Arc::new(slots));
        }
        let hosts = self.hosts.load();
        if hosts.values().any(|routed| *routed == key) {
            let kept: Hosts = hosts
                .iter()
                .filter(|(_, routed)| **routed != key)
                .map(|(host, routed)| (host.clone(), *routed))
                .collect();
            self.hosts.store(Arc::new(kept));
        }
    }

    /// Replace every `kind` hostname with `routes` (host, target id).
    pub fn replace_hosts(
        &self,
        kind: ActivationKind,
        routes: impl IntoIterator<Item = (String, i64)>,
    ) {
        let _guard = self.write_lock();
        let mut hosts: Hosts = self
            .hosts
            .load()
            .iter()
            .filter(|(_, key)| key.kind != kind)
            .map(|(host, key)| (host.clone(), *key))
            .collect();
        for (host, id) in routes {
            hosts.insert(host.to_ascii_lowercase(), ActivationKey::new(kind, id));
        }
        self.hosts.store(Arc::new(hosts));
    }

    /// The target `host` routes to. `host` must already be lowercase and
    /// without a port (the proxy's normalised host). Request path: one
    /// `ArcSwap` load, and no hashing at all while no host is routed.
    pub fn key_for_host(&self, host: &str) -> Option<ActivationKey> {
        let hosts = self.hosts.load();
        if hosts.is_empty() {
            return None;
        }
        hosts.get(host).copied()
    }

    pub fn is_registered(&self, key: ActivationKey) -> bool {
        self.slots.load().contains_key(&key)
    }

    pub fn state(&self, key: ActivationKey) -> Option<ActivationState> {
        self.slots
            .load()
            .get(&key)
            .map(|slot| ActivationState::from_raw(slot.state()))
    }

    /// When `key` last saw a request (epoch seconds).
    pub fn last_activity(&self, key: ActivationKey) -> Option<u64> {
        self.slots
            .load()
            .get(&key)
            .map(|slot| slot.last_activity.load(Ordering::Relaxed))
    }

    /// Record a request for `key`. Request path: lock-free.
    pub fn touch(&self, key: ActivationKey) {
        if let Some(slot) = self.slots.load().get(&key) {
            slot.touch(now_secs());
        }
    }

    /// Record that `key` was woken outside the activator (the console's
    /// wake button, another process), so the sweep tracks it.
    pub fn mark_awake(&self, key: ActivationKey, address: Option<String>) {
        if let Some(slot) = self.slots.load().get(&key) {
            if matches!(slot.state(), ASLEEP | AWAKE) {
                slot.address.store(address.map(Arc::new));
                slot.touch(now_secs());
                slot.set(AWAKE);
            }
        }
    }

    /// Record that `key` was put to sleep outside the activator.
    pub fn mark_asleep(&self, key: ActivationKey) {
        if let Some(slot) = self.slots.load().get(&key) {
            if matches!(slot.state(), ASLEEP | AWAKE) {
                slot.address.store(None);
                slot.set(ASLEEP);
            }
        }
    }

    pub fn metrics(&self) -> MetricsSnapshot {
        let m = &self.metrics;
        MetricsSnapshot {
            wakes: m.wakes.load(Ordering::Relaxed),
            wake_failures: m.wake_failures.load(Ordering::Relaxed),
            sleeps: m.sleeps.load(Ordering::Relaxed),
            sleep_failures: m.sleep_failures.load(Ordering::Relaxed),
            saturated: m.saturated.load(Ordering::Relaxed),
            waited: m.waited.load(Ordering::Relaxed),
        }
    }

    /// Let a request for `key` through: at once when it is awake,
    /// otherwise after the one wake that every concurrent request shares.
    pub async fn activate(&self, key: ActivationKey) -> Result<Activation, ActivationError> {
        let slot = self
            .slots
            .load()
            .get(&key)
            .cloned()
            .ok_or(ActivationError::NotRegistered { key })?;
        slot.touch(now_secs());
        if slot.state() == AWAKE {
            return Ok(Activation {
                address: slot.address.load_full(),
                waited: false,
            });
        }
        let _permit = Arc::clone(&self.waiters).try_acquire_owned().map_err(|_| {
            self.metrics.saturated.fetch_add(1, Ordering::Relaxed);
            ActivationError::Saturated {
                key,
                limit: self.config.max_waiters,
            }
        })?;
        self.metrics.waited.fetch_add(1, Ordering::Relaxed);
        // Subscribe before reading the state: a transition after this point
        // is always seen.
        let mut changes = slot.changed.subscribe();
        let timeout = slot.wake_timeout();
        let deadline = tokio::time::Instant::now() + timeout + self.config.wait_grace;
        let mut joined_a_wake = false;
        loop {
            match slot.state() {
                AWAKE => {
                    return Ok(Activation {
                        address: slot.address.load_full(),
                        waited: true,
                    })
                }
                ASLEEP if joined_a_wake => {
                    let reason = slot
                        .last_error
                        .load_full()
                        .map(|reason| (*reason).clone())
                        .unwrap_or_else(|| "the wake stopped without a reason".to_string());
                    return Err(ActivationError::WakeFailed { key, reason });
                }
                ASLEEP => {
                    joined_a_wake = true;
                    if slot.transition(ASLEEP, WAKING) {
                        self.spawn_wake(Arc::clone(&slot))?;
                    }
                    continue;
                }
                WAKING => joined_a_wake = true,
                // A sleep in progress: wait for it to end, then wake.
                _ => {}
            }
            match tokio::time::timeout_at(deadline, changes.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => {
                    return Err(ActivationError::WakeFailed {
                        key,
                        reason: "the target was unregistered while waking".to_string(),
                    })
                }
                Err(_) => {
                    return Err(ActivationError::WakeTimeout {
                        key,
                        timeout_secs: timeout.as_secs(),
                    })
                }
            }
        }
    }

    /// Run the wake of `slot` (already `waking`) in its own task.
    fn spawn_wake(&self, slot: Arc<Slot>) -> Result<(), ActivationError> {
        let key = slot.key;
        let Some(backend) = self.backends.load().get(&key.kind).cloned() else {
            slot.last_error.store(Some(Arc::new(format!(
                "no activation backend is registered for {} targets",
                key.kind.as_str()
            ))));
            slot.set(ASLEEP);
            return Err(ActivationError::NoBackend { key });
        };
        let metrics = Arc::clone(&self.metrics);
        tokio::spawn(async move {
            let timeout = slot.wake_timeout();
            let started = tokio::time::Instant::now();
            tracing::info!(target_key = %key, "activator waking a scale-to-zero target");
            match tokio::time::timeout(timeout, backend.wake(key)).await {
                Ok(Ok(woken)) => {
                    slot.address.store(woken.address.map(Arc::new));
                    slot.last_error.store(None);
                    slot.touch(now_secs());
                    metrics.wakes.fetch_add(1, Ordering::Relaxed);
                    tracing::info!(
                        target_key = %key,
                        wake_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                        "activator woke the target"
                    );
                    slot.set(AWAKE);
                }
                Ok(Err(reason)) => {
                    metrics.wake_failures.fetch_add(1, Ordering::Relaxed);
                    tracing::error!(target_key = %key, "activator could not wake the target: {reason}");
                    slot.last_error.store(Some(Arc::new(reason)));
                    slot.set(ASLEEP);
                }
                Err(_) => {
                    metrics.wake_failures.fetch_add(1, Ordering::Relaxed);
                    let reason = format!(
                        "the target did not become ready within {}s",
                        timeout.as_secs()
                    );
                    tracing::error!(target_key = %key, "activator wake timed out: {reason}");
                    slot.last_error.store(Some(Arc::new(reason)));
                    slot.set(ASLEEP);
                }
            }
        });
        Ok(())
    }

    /// Put every awake target idle for at least its idle time to sleep.
    pub async fn sweep(&self) -> SweepOutcome {
        self.sweep_at(now_secs()).await
    }

    /// [`Self::sweep`] at `now` (epoch seconds).
    pub async fn sweep_at(&self, now: u64) -> SweepOutcome {
        let mut outcome = SweepOutcome::default();
        let slots = self.slots.load_full();
        let backends = self.backends.load_full();
        for slot in slots.values() {
            if slot.state() != AWAKE {
                continue;
            }
            let idle_secs = u64::from(slot.idle_secs.load(Ordering::Relaxed));
            let last = slot.last_activity.load(Ordering::Relaxed);
            if now.saturating_sub(last) < idle_secs {
                continue;
            }
            let Some(backend) = backends.get(&slot.key.kind) else {
                continue;
            };
            if !slot.transition(AWAKE, SLEEPING) {
                continue;
            }
            // A request between the idle check and the transition keeps it
            // awake.
            if slot.last_activity.load(Ordering::Relaxed) != last {
                slot.set(AWAKE);
                continue;
            }
            let key = slot.key;
            match tokio::time::timeout(self.config.sleep_timeout, backend.sleep(key)).await {
                Ok(Ok(_)) => {
                    slot.address.store(None);
                    self.metrics.sleeps.fetch_add(1, Ordering::Relaxed);
                    tracing::info!(
                        target_key = %key,
                        idle_secs = now.saturating_sub(last),
                        "activator put an idle target to sleep"
                    );
                    slot.set(ASLEEP);
                    outcome.slept.push(key);
                }
                Ok(Err(reason)) => {
                    self.metrics.sleep_failures.fetch_add(1, Ordering::Relaxed);
                    tracing::error!(target_key = %key, "activator could not put the target to sleep: {reason}");
                    slot.set(AWAKE);
                    outcome.failed.push(key);
                }
                Err(_) => {
                    self.metrics.sleep_failures.fetch_add(1, Ordering::Relaxed);
                    tracing::error!(
                        target_key = %key,
                        timeout_secs = self.config.sleep_timeout.as_secs(),
                        "activator gave up putting the target to sleep"
                    );
                    slot.set(AWAKE);
                    outcome.failed.push(key);
                }
            }
        }
        outcome
    }

    /// Sweep every `interval` for the life of the process.
    pub fn spawn_sweeper(self: &Arc<Self>, interval: Duration) -> JoinHandle<()> {
        let activator = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let outcome = activator.sweep().await;
                if !outcome.slept.is_empty() || !outcome.failed.is_empty() {
                    tracing::debug!(
                        slept = outcome.slept.len(),
                        failed = outcome.failed.len(),
                        "activator sweep finished"
                    );
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    /// A backend that counts its calls and can be made slow or failing.
    #[derive(Default)]
    struct Counting {
        wakes: AtomicUsize,
        sleeps: AtomicUsize,
        wake_delay_ms: AtomicU64,
        fail_wakes: AtomicBool,
        fail_sleeps: AtomicBool,
    }

    #[async_trait]
    impl ActivationBackend for Counting {
        async fn wake(&self, key: ActivationKey) -> Result<Woken, String> {
            self.wakes.fetch_add(1, Ordering::SeqCst);
            let delay = self.wake_delay_ms.load(Ordering::SeqCst);
            if delay > 0 {
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            if self.fail_wakes.load(Ordering::SeqCst) {
                return Err(format!("{key}: the instance exited 1 while starting"));
            }
            Ok(Woken {
                address: Some(format!("127.0.0.1:{}", 40_000 + key.id)),
            })
        }

        async fn sleep(&self, _key: ActivationKey) -> Result<bool, String> {
            self.sleeps.fetch_add(1, Ordering::SeqCst);
            if self.fail_sleeps.load(Ordering::SeqCst) {
                return Err("the driver did not answer".to_string());
            }
            Ok(true)
        }
    }

    fn function(id: i64) -> ActivationKey {
        ActivationKey::new(ActivationKind::Function, id)
    }

    fn asleep(idle_secs: u32) -> Registration {
        Registration {
            idle_secs,
            wake_timeout_secs: 5,
            awake: false,
        }
    }

    fn activator_with(backend: Arc<Counting>) -> Arc<Activator> {
        let activator = Arc::new(Activator::default());
        activator.set_backend(ActivationKind::Function, backend);
        activator
    }

    #[tokio::test]
    async fn the_first_request_wakes_the_target_and_later_ones_pass_straight_through() {
        let backend = Arc::new(Counting::default());
        let activator = activator_with(Arc::clone(&backend));
        activator.register(function(1), asleep(60));
        assert_eq!(activator.state(function(1)), Some(ActivationState::Asleep));

        let first = activator.activate(function(1)).await.expect("it wakes");
        assert!(first.waited);
        assert_eq!(
            first.address.as_deref().map(String::as_str),
            Some("127.0.0.1:40001")
        );
        assert_eq!(activator.state(function(1)), Some(ActivationState::Awake));

        let second = activator.activate(function(1)).await.expect("it is awake");
        assert!(!second.waited, "an awake target takes the fast path");
        assert_eq!(backend.wakes.load(Ordering::SeqCst), 1);
        assert_eq!(activator.metrics().wakes, 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_first_requests_wake_the_target_once() {
        let backend = Arc::new(Counting::default());
        backend.wake_delay_ms.store(200, Ordering::SeqCst);
        let activator = activator_with(Arc::clone(&backend));
        activator.register(function(2), asleep(60));

        let requests: Vec<_> = (0..50)
            .map(|_| {
                let activator = Arc::clone(&activator);
                tokio::spawn(async move { activator.activate(function(2)).await })
            })
            .collect();
        for request in requests {
            let activation = request
                .await
                .expect("the request task completes")
                .expect("every request is let through");
            assert_eq!(
                activation.address.as_deref().map(String::as_str),
                Some("127.0.0.1:40002")
            );
        }
        assert_eq!(
            backend.wakes.load(Ordering::SeqCst),
            1,
            "fifty concurrent first requests share one wake"
        );
    }

    #[tokio::test]
    async fn an_idle_target_is_put_to_sleep_and_an_active_one_is_not() {
        let backend = Arc::new(Counting::default());
        let activator = activator_with(Arc::clone(&backend));
        activator.register(function(3), asleep(30));
        activator.register(function(4), asleep(30));
        activator.activate(function(3)).await.expect("wake 3");
        activator.activate(function(4)).await.expect("wake 4");
        let woke_at = activator.last_activity(function(3)).expect("activity");

        // 10 s later: neither is idle.
        let outcome = activator.sweep_at(woke_at + 10).await;
        assert!(outcome.slept.is_empty());

        // 4 sees a request at +20; at +35, 3 is idle and 4 is not.
        activator
            .slots
            .load()
            .get(&function(4))
            .expect("slot")
            .touch(woke_at + 20);
        let outcome = activator.sweep_at(woke_at + 35).await;
        assert_eq!(outcome.slept, vec![function(3)]);
        assert_eq!(activator.state(function(3)), Some(ActivationState::Asleep));
        assert_eq!(activator.state(function(4)), Some(ActivationState::Awake));
        assert_eq!(backend.sleeps.load(Ordering::SeqCst), 1);

        // The next request wakes it again.
        let activation = activator.activate(function(3)).await.expect("re-wake");
        assert!(activation.waited);
        assert_eq!(backend.wakes.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn a_failed_wake_fails_every_waiting_request_with_the_reason() {
        let backend = Arc::new(Counting::default());
        backend.fail_wakes.store(true, Ordering::SeqCst);
        backend.wake_delay_ms.store(100, Ordering::SeqCst);
        let activator = activator_with(Arc::clone(&backend));
        activator.register(function(5), asleep(60));
        let requests: Vec<_> = (0..5)
            .map(|_| {
                let activator = Arc::clone(&activator);
                tokio::spawn(async move { activator.activate(function(5)).await })
            })
            .collect();
        for request in requests {
            let error = request
                .await
                .expect("the request task completes")
                .expect_err("the wake failed");
            assert!(
                error.to_string().contains("exited 1 while starting"),
                "{error}"
            );
        }
        assert_eq!(backend.wakes.load(Ordering::SeqCst), 1);
        assert_eq!(activator.state(function(5)), Some(ActivationState::Asleep));
        assert_eq!(activator.metrics().wake_failures, 1);

        // Once it is fixed, the next request wakes it.
        backend.fail_wakes.store(false, Ordering::SeqCst);
        activator.activate(function(5)).await.expect("it wakes now");
    }

    #[tokio::test]
    async fn a_wake_that_outlives_its_timeout_fails_with_the_timeout() {
        let backend = Arc::new(Counting::default());
        backend.wake_delay_ms.store(3_000, Ordering::SeqCst);
        let activator = Arc::new(Activator::new(ActivatorConfig {
            wait_grace: Duration::from_millis(500),
            ..Default::default()
        }));
        activator.set_backend(ActivationKind::Function, backend);
        activator.register(
            function(6),
            Registration {
                idle_secs: 60,
                wake_timeout_secs: 1,
                awake: false,
            },
        );
        let error = activator
            .activate(function(6))
            .await
            .expect_err("the wake is too slow");
        assert!(error.to_string().contains("within 1s"), "{error}");
        assert_eq!(activator.state(function(6)), Some(ActivationState::Asleep));
    }

    #[tokio::test]
    async fn waiters_are_bounded() {
        let backend = Arc::new(Counting::default());
        backend.wake_delay_ms.store(300, Ordering::SeqCst);
        let activator = Arc::new(Activator::new(ActivatorConfig {
            max_waiters: 2,
            ..Default::default()
        }));
        activator.set_backend(ActivationKind::Function, backend);
        activator.register(function(7), asleep(60));
        let parked: Vec<_> = (0..2)
            .map(|_| {
                let activator = Arc::clone(&activator);
                tokio::spawn(async move { activator.activate(function(7)).await })
            })
            .collect();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let error = activator
            .activate(function(7))
            .await
            .expect_err("the third waiter is refused");
        assert!(matches!(error, ActivationError::Saturated { limit: 2, .. }));
        for request in parked {
            request.await.expect("task").expect("parked requests wake");
        }
        assert_eq!(activator.metrics().saturated, 1);
        // Awake targets never take a waiter slot.
        activator.activate(function(7)).await.expect("fast path");
    }

    #[tokio::test]
    async fn a_failed_sleep_keeps_the_target_awake() {
        let backend = Arc::new(Counting::default());
        backend.fail_sleeps.store(true, Ordering::SeqCst);
        let activator = activator_with(Arc::clone(&backend));
        activator.register(function(8), asleep(1));
        activator.activate(function(8)).await.expect("wake");
        let outcome = activator.sweep_at(now_secs() + 5).await;
        assert_eq!(outcome.failed, vec![function(8)]);
        assert_eq!(activator.state(function(8)), Some(ActivationState::Awake));
        assert_eq!(activator.metrics().sleep_failures, 1);
    }

    #[tokio::test]
    async fn unregistered_and_backendless_targets_are_typed_errors() {
        let activator = Activator::default();
        let error = activator
            .activate(function(9))
            .await
            .expect_err("not registered");
        assert_eq!(error, ActivationError::NotRegistered { key: function(9) });

        activator.register(function(9), asleep(60));
        let error = activator
            .activate(function(9))
            .await
            .expect_err("no backend");
        assert_eq!(error, ActivationError::NoBackend { key: function(9) });
        assert_eq!(activator.state(function(9)), Some(ActivationState::Asleep));
    }

    #[tokio::test]
    async fn re_registering_keeps_state_and_replace_kind_removes_the_missing() {
        let backend = Arc::new(Counting::default());
        let activator = activator_with(backend);
        activator.register(function(10), asleep(60));
        activator.activate(function(10)).await.expect("wake");
        // A route reload re-registers it as stored (asleep): it stays awake.
        activator.replace_kind(
            ActivationKind::Function,
            [(10, asleep(120)), (11, asleep(60))],
        );
        assert_eq!(activator.state(function(10)), Some(ActivationState::Awake));
        assert_eq!(activator.state(function(11)), Some(ActivationState::Asleep));
        activator.replace_kind(ActivationKind::Function, [(11, asleep(60))]);
        assert!(!activator.is_registered(function(10)));
        let environment = ActivationKey::new(ActivationKind::Environment, 10);
        activator.register(environment, asleep(60));
        activator.replace_kind(ActivationKind::Function, []);
        assert!(
            activator.is_registered(environment),
            "replacing one kind leaves the others"
        );
    }

    #[test]
    fn hosts_route_to_targets_and_go_with_them() {
        let activator = Activator::default();
        assert_eq!(activator.key_for_host("fn.example.test"), None);
        activator.register(function(12), asleep(60));
        activator.replace_hosts(
            ActivationKind::Function,
            [("Fn.Example.Test".to_string(), 12)],
        );
        assert_eq!(
            activator.key_for_host("fn.example.test"),
            Some(function(12))
        );
        activator.unregister(function(12));
        assert_eq!(activator.key_for_host("fn.example.test"), None);
    }

    #[tokio::test]
    async fn marks_from_outside_are_tracked_by_the_sweep() {
        let backend = Arc::new(Counting::default());
        let activator = activator_with(Arc::clone(&backend));
        activator.register(function(13), asleep(1));
        activator.mark_awake(function(13), None);
        assert_eq!(activator.state(function(13)), Some(ActivationState::Awake));
        let outcome = activator.sweep_at(now_secs() + 5).await;
        assert_eq!(outcome.slept, vec![function(13)]);
        activator.mark_awake(function(13), Some("127.0.0.1:1".into()));
        activator.mark_asleep(function(13));
        assert_eq!(activator.state(function(13)), Some(ActivationState::Asleep));
    }
}
