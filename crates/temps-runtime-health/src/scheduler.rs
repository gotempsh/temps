// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::{
    probe::network_probe, Assignment, EngineError, ExecutionOwner, HealthReason, HealthState,
    InspectFailure, Observation, Probe, RuntimeInspector, MAX_ASSIGNMENTS, MAX_CONCURRENT_PROBES,
    MAX_LEASE,
};
use futures::{stream, StreamExt};
use parking_lot::Mutex;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};
use tokio::time::{timeout_at, Instant};
use uuid::Uuid;

/// One instance per authorized execution owner. Host-wide/process-restart
/// ownership belongs to the embedding host and is not supplied by this mutex.
pub struct ProbeEngine {
    owner: ExecutionOwner,
    inspector: Arc<dyn RuntimeInspector>,
    state: Arc<Mutex<EngineState>>,
}

#[derive(Default)]
struct EngineState {
    entries: HashMap<Uuid, Entry>,
    containers: HashMap<String, Uuid>,
    due: BTreeSet<(Instant, Uuid)>,
    ready: BTreeMap<u32, VecDeque<Uuid>>,
    projects: VecDeque<u32>,
    in_flight: usize,
}

struct Entry {
    assignment: Assignment,
    expires_at: Instant,
    sequence: u64,
    phase: Phase,
    due_at: Instant,
    stable: Option<Stability>,
    last: Option<(Instant, Observation)>,
}

#[derive(Clone, Copy)]
enum Phase {
    Scheduled(Instant),
    Ready,
    Running,
}

#[derive(Clone)]
struct Stability {
    started_at: String,
    restarts: u64,
    since: Instant,
    observed_at: Instant,
}

impl ProbeEngine {
    pub fn new(owner: ExecutionOwner, inspector: Arc<dyn RuntimeInspector>) -> Self {
        Self {
            owner,
            inspector,
            state: Arc::new(Mutex::new(EngineState::default())),
        }
    }

    /// Called only after the host's authorization and durable assignment commit.
    /// Lease admission uses a monotonic deadline, never a worker wall-clock date.
    pub fn install(&self, assignment: Assignment, expires_at: Instant) -> Result<(), EngineError> {
        assignment.validate()?;
        if assignment.owner != self.owner {
            return Err(EngineError::WrongOwner {
                assignment_id: assignment.id,
            });
        }
        validate_lease(assignment.id, expires_at)?;
        let mut state = self.state.lock();
        if state.entries.contains_key(&assignment.id) {
            return Err(EngineError::AlreadyInstalled {
                assignment_id: assignment.id,
            });
        }
        if let Some(id) = state.containers.get(&assignment.container_id) {
            return Err(EngineError::ContainerOwned { assignment_id: *id });
        }
        if state.entries.len() >= MAX_ASSIGNMENTS {
            return Err(EngineError::Capacity {
                limit: MAX_ASSIGNMENTS,
            });
        }
        let now = Instant::now();
        state
            .containers
            .insert(assignment.container_id.clone(), assignment.id);
        state.due.insert((now, assignment.id));
        state.entries.insert(
            assignment.id,
            Entry {
                assignment,
                expires_at,
                sequence: 0,
                phase: Phase::Scheduled(now),
                due_at: now,
                stable: None,
                last: None,
            },
        );
        Ok(())
    }

    pub(crate) fn checked_at(&self, id: Uuid) -> Option<Instant> {
        self.state
            .lock()
            .entries
            .get(&id)?
            .last
            .as_ref()
            .map(|(at, _)| *at)
    }

    /// Renewal cannot revive expired authority or change an installed revision.
    pub fn renew(&self, id: Uuid, epoch: u64, expires_at: Instant) -> Result<(), EngineError> {
        validate_lease(id, expires_at)?;
        let mut state = self.state.lock();
        let entry = state
            .entries
            .get_mut(&id)
            .filter(|e| e.assignment.epoch == epoch)
            .ok_or(EngineError::UnknownAssignment {
                assignment_id: id,
                epoch,
            })?;
        if entry.expires_at <= Instant::now() {
            return Err(EngineError::InvalidLease { assignment_id: id });
        }
        entry.expires_at = expires_at;
        Ok(())
    }

    /// Handover must wait for the prior read/network future to finish or cancel.
    pub fn remove(&self, id: Uuid, epoch: u64) -> Result<(), EngineError> {
        let mut state = self.state.lock();
        let entry = state
            .entries
            .get(&id)
            .filter(|e| e.assignment.epoch == epoch)
            .ok_or(EngineError::UnknownAssignment {
                assignment_id: id,
                epoch,
            })?;
        if matches!(entry.phase, Phase::Running) {
            return Err(EngineError::Busy { assignment_id: id });
        }
        let container = entry.assignment.container_id.clone();
        let project = entry.assignment.project_id;
        if let Phase::Scheduled(at) = entry.phase {
            state.due.remove(&(at, id));
        }
        state.entries.remove(&id);
        state.containers.remove(&container);
        if let Some(queue) = state.ready.get_mut(&project) {
            queue.retain(|candidate| *candidate != id);
            if queue.is_empty() {
                state.ready.remove(&project);
                state.projects.retain(|candidate| *candidate != project);
            }
        }
        Ok(())
    }

    /// Fresh current state only. Read-time expiry prevents stale green state
    /// even when a host has stopped calling `run_due`.
    pub fn current(&self, id: Uuid) -> Option<Observation> {
        let state = self.state.lock();
        let entry = state.entries.get(&id)?;
        let now = Instant::now();
        if entry.expires_at <= now {
            return Some(observation(
                entry,
                HealthState::Unknown,
                HealthReason::LeaseExpired,
            ));
        }
        match &entry.last {
            Some((at, report)) if now.duration_since(*at) <= entry.assignment.freshness() => {
                Some(report.clone())
            }
            _ => Some(observation(
                entry,
                HealthState::Unknown,
                HealthReason::Stale,
            )),
        }
    }

    /// One bounded scheduling cycle, with project round-robin and no task per
    /// assigned container. The host calls this from its single <=250 ms timer.
    /// Missed schedule windows reset process stability. Results
    /// are at most four typed observations; there is no unbounded report spool.
    pub async fn run_due(&self) -> Vec<Observation> {
        let jobs = {
            let mut state = self.state.lock();
            let now = Instant::now();
            // Bound promotion work even if every assignment becomes due at once.
            for _ in 0..100 {
                let Some((at, id)) = state.due.first().copied().filter(|(at, _)| *at <= now) else {
                    break;
                };
                state.due.remove(&(at, id));
                let Some(entry) = state.entries.get_mut(&id) else {
                    continue;
                };
                if entry.expires_at <= now {
                    entry.stable = None;
                    entry.last = Some((
                        now,
                        observation(entry, HealthState::Unknown, HealthReason::LeaseExpired),
                    ));
                    // Expired entries remain bounded and inspectable, but never reschedule.
                    entry.phase = Phase::Ready;
                    continue;
                }
                entry.phase = Phase::Ready;
                let project = entry.assignment.project_id;
                if !state.ready.contains_key(&project) {
                    state.projects.push_back(project);
                }
                state.ready.entry(project).or_default().push_back(id);
            }
            let available = MAX_CONCURRENT_PROBES.saturating_sub(state.in_flight);
            let mut jobs = Vec::with_capacity(available);
            while jobs.len() < available {
                let Some(project) = state.projects.pop_front() else {
                    break;
                };
                let Some(queue) = state.ready.get_mut(&project) else {
                    continue;
                };
                let id = queue.pop_front();
                if queue.is_empty() {
                    state.ready.remove(&project);
                } else {
                    state.projects.push_back(project);
                }
                let Some(id) = id else {
                    continue;
                };
                let Some(entry) = state.entries.get_mut(&id) else {
                    continue;
                };
                if entry.expires_at <= now {
                    entry.stable = None;
                    entry.last = Some((
                        now,
                        observation(entry, HealthState::Unknown, HealthReason::LeaseExpired),
                    ));
                    continue;
                }
                let Some(sequence) = entry.sequence.checked_add(1) else {
                    entry.expires_at = now;
                    continue;
                };
                entry.sequence = sequence;
                entry.phase = Phase::Running;
                jobs.push(Attempt {
                    state: self.state.clone(),
                    assignment: entry.assignment.clone(),
                    expires_at: entry.expires_at,
                    previous: entry.stable.clone(),
                    missed_interval: now.saturating_duration_since(entry.due_at)
                        > Duration::from_millis(250),
                    completed: false,
                });
                state.in_flight += 1;
            }
            jobs
        };
        stream::iter(jobs)
            .map(|attempt| self.run_attempt(attempt))
            .buffer_unordered(MAX_CONCURRENT_PROBES)
            .collect()
            .await
    }

    async fn run_attempt(&self, mut attempt: Attempt) -> Observation {
        let now = Instant::now();
        if now >= attempt.expires_at {
            return attempt.finish(HealthState::Unknown, HealthReason::LeaseExpired, None);
        }
        let assignment = &attempt.assignment;
        let deadline = (now + Duration::from_secs(assignment.timeout_secs)).min(attempt.expires_at);
        let _permit = match timeout_at(deadline, self.inspector.admit(assignment)).await {
            Ok(Ok(permit)) => permit,
            Ok(Err(InspectFailure::Unauthorized)) => {
                return attempt.finish(HealthState::Unknown, HealthReason::Unauthorized, None);
            }
            Ok(Err(InspectFailure::Unavailable)) | Err(_) => {
                return attempt.finish(
                    HealthState::Unknown,
                    HealthReason::RuntimeUnavailable,
                    None,
                );
            }
        };
        let checked = timeout_at(deadline, self.inspector.inspect(assignment)).await;
        let mut stable = None;
        let (health, reason) = match checked {
            Err(_) => (HealthState::Unknown, HealthReason::RuntimeUnavailable),
            Ok(Err(InspectFailure::Unavailable)) => {
                (HealthState::Unknown, HealthReason::RuntimeUnavailable)
            }
            Ok(Err(InspectFailure::Unauthorized)) => {
                (HealthState::Unknown, HealthReason::Unauthorized)
            }
            Ok(Ok(snapshot)) if snapshot.container_id != assignment.container_id => {
                (HealthState::Unknown, HealthReason::IdentityMismatch)
            }
            Ok(Ok(snapshot)) if !snapshot.running => {
                (HealthState::Unhealthy, HealthReason::NotRunning)
            }
            Ok(Ok(snapshot)) => match assignment.probe {
                Probe::Process { stable_for_secs } => {
                    if snapshot.started_at.is_empty() || snapshot.started_at.len() > 128 {
                        (HealthState::Unknown, HealthReason::RuntimeUnavailable)
                    } else {
                        let observed_at = Instant::now();
                        let restarted = attempt.previous.as_ref().is_some_and(|old| {
                            old.started_at != snapshot.started_at
                                || old.restarts != snapshot.restart_count
                        });
                        let previous = attempt.previous.as_ref().filter(|old| {
                            !attempt.missed_interval
                                && old.started_at == snapshot.started_at
                                && old.restarts == snapshot.restart_count
                                && observed_at.duration_since(old.observed_at)
                                    <= assignment.freshness()
                        });
                        let since = previous.map_or(observed_at, |old| old.since);
                        stable = Some(Stability {
                            since,
                            observed_at,
                            started_at: snapshot.started_at,
                            restarts: snapshot.restart_count,
                        });
                        if restarted {
                            (HealthState::Unhealthy, HealthReason::Restarted)
                        } else if observed_at.duration_since(since)
                            >= Duration::from_secs(stable_for_secs)
                        {
                            (HealthState::Healthy, HealthReason::Passed)
                        } else {
                            (HealthState::Unknown, HealthReason::Stabilizing)
                        }
                    }
                }
                _ => match network_probe(&assignment.probe, &snapshot, deadline).await {
                    Ok(()) => (HealthState::Healthy, HealthReason::Passed),
                    Err(HealthReason::EndpointUnavailable) => {
                        (HealthState::Unknown, HealthReason::EndpointUnavailable)
                    }
                    Err(reason) => (HealthState::Unhealthy, reason),
                },
            },
        };
        attempt.finish(health, reason, stable)
    }
}

fn validate_lease(id: Uuid, expiry: Instant) -> Result<(), EngineError> {
    let remaining = expiry.checked_duration_since(Instant::now());
    if remaining.is_none_or(|duration| duration.is_zero() || duration > MAX_LEASE) {
        return Err(EngineError::InvalidLease { assignment_id: id });
    }
    Ok(())
}

fn observation(entry: &Entry, state: HealthState, reason: HealthReason) -> Observation {
    Observation {
        assignment_id: entry.assignment.id,
        revision: entry.assignment.revision,
        placement_generation: entry.assignment.placement_generation,
        epoch: entry.assignment.epoch,
        sequence: entry.sequence,
        state,
        reason,
    }
}

/// Every admitted future owns a cancellation guard, including futures not yet
/// polled by the stream. Read-only cancellation cannot invoke lifecycle APIs.
struct Attempt {
    state: Arc<Mutex<EngineState>>,
    assignment: Assignment,
    expires_at: Instant,
    previous: Option<Stability>,
    missed_interval: bool,
    completed: bool,
}

impl Attempt {
    fn finish(
        &mut self,
        health: HealthState,
        reason: HealthReason,
        stable: Option<Stability>,
    ) -> Observation {
        let mut state = self.state.lock();
        let now = Instant::now();
        // Removal and revision replacement are rejected while this guard owns the entry.
        let entry = match state.entries.get_mut(&self.assignment.id) {
            Some(entry) => entry,
            None => {
                return Observation {
                    assignment_id: self.assignment.id,
                    revision: self.assignment.revision,
                    placement_generation: self.assignment.placement_generation,
                    epoch: self.assignment.epoch,
                    sequence: 0,
                    state: HealthState::Unknown,
                    reason: HealthReason::IdentityMismatch,
                }
            }
        };
        let (health, reason) = if now >= self.expires_at.min(entry.expires_at) {
            (HealthState::Unknown, HealthReason::LeaseExpired)
        } else {
            (health, reason)
        };
        let report = observation(entry, health, reason);
        entry.last = Some((now, report.clone()));
        entry.stable = if (health == HealthState::Unhealthy && reason != HealthReason::Restarted)
            || reason == HealthReason::LeaseExpired
        {
            None
        } else {
            stable
        };
        let next = now + Duration::from_secs(entry.assignment.interval_secs);
        entry.phase = Phase::Scheduled(next);
        entry.due_at = next;
        state.due.insert((next, self.assignment.id));
        state.in_flight = state.in_flight.saturating_sub(1);
        self.completed = true;
        report
    }
}

impl Drop for Attempt {
    fn drop(&mut self) {
        if !self.completed {
            self.finish(HealthState::Unknown, HealthReason::Cancelled, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InspectFailure, RuntimeSnapshot};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::Notify,
        time::advance,
    };

    struct Inspector {
        snapshot: Mutex<Result<RuntimeSnapshot, InspectFailure>>,
        calls: AtomicUsize,
        active: Arc<AtomicUsize>,
        maximum: AtomicUsize,
        entered: Notify,
        blocked: bool,
    }

    struct Active(Arc<AtomicUsize>);
    impl Drop for Active {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl RuntimeInspector for Inspector {
        async fn inspect(
            &self,
            assignment: &Assignment,
        ) -> Result<RuntimeSnapshot, InspectFailure> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let current = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.maximum.fetch_max(current, Ordering::SeqCst);
            let _active = Active(self.active.clone());
            self.entered.notify_one();
            if self.blocked {
                futures::future::pending::<()>().await;
            }
            let snapshot = self.snapshot.lock().clone();
            // The multi-assignment fixture resolves the requested immutable ID.
            snapshot.map(|mut snapshot| {
                if snapshot.container_id.is_empty() {
                    snapshot.container_id = assignment.container_id.clone();
                }
                snapshot
            })
        }
    }

    fn inspector() -> Arc<Inspector> {
        Arc::new(Inspector {
            snapshot: Mutex::new(Ok(RuntimeSnapshot {
                container_id: String::new(),
                running: true,
                restart_count: 0,
                started_at: "runtime-start-1".into(),
                endpoint: None,
            })),
            calls: AtomicUsize::new(0),
            active: Arc::new(AtomicUsize::new(0)),
            maximum: AtomicUsize::new(0),
            entered: Notify::new(),
            blocked: false,
        })
    }

    fn assignment(owner: ExecutionOwner) -> Assignment {
        Assignment {
            id: Uuid::new_v4(),
            owner,
            project_id: 1,
            environment_id: 1,
            deployment_id: 1,
            replica_id: 1,
            placement_generation: 1,
            revision: Uuid::new_v4(),
            epoch: 1,
            container_id: format!("{:064x}", 1),
            container_port: None,
            probe: Probe::Process { stable_for_secs: 3 },
            interval_secs: 1,
            timeout_secs: 2,
        }
    }

    fn engine(inspector: Arc<Inspector>) -> (Arc<ProbeEngine>, Assignment) {
        let owner = ExecutionOwner::Embedded(Uuid::new_v4());
        (
            Arc::new(ProbeEngine::new(owner, inspector)),
            assignment(owner),
        )
    }

    fn install(engine: &ProbeEngine, assignment: &Assignment) {
        engine
            .install(assignment.clone(), Instant::now() + MAX_LEASE)
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn process_requires_observed_stability_and_crash_fails() {
        let runtime = inspector();
        let (engine, assignment) = engine(runtime.clone());
        install(&engine, &assignment);
        assert_eq!(engine.run_due().await[0].reason, HealthReason::Stabilizing);
        for _ in 0..3 {
            advance(Duration::from_secs(1)).await;
            engine.run_due().await;
        }
        assert_eq!(
            engine.current(assignment.id).unwrap().state,
            HealthState::Healthy
        );
        runtime.snapshot.lock().as_mut().unwrap().running = false;
        advance(Duration::from_secs(1)).await;
        let report = engine.run_due().await.remove(0);
        assert_eq!(
            (report.state, report.reason),
            (HealthState::Unhealthy, HealthReason::NotRunning)
        );
        runtime.snapshot.lock().as_mut().unwrap().running = true;
        advance(Duration::from_secs(1)).await;
        assert_eq!(engine.run_due().await[0].reason, HealthReason::Stabilizing);
    }

    #[tokio::test(start_paused = true)]
    async fn restart_identity_counter_and_observation_gap_reset_stability() {
        let runtime = inspector();
        let (engine, assignment) = engine(runtime.clone());
        install(&engine, &assignment);
        engine.run_due().await;
        for _ in 0..3 {
            advance(Duration::from_secs(1)).await;
            engine.run_due().await;
        }
        assert_eq!(
            engine.current(assignment.id).unwrap().state,
            HealthState::Healthy
        );
        runtime.snapshot.lock().as_mut().unwrap().restart_count = 1;
        advance(Duration::from_secs(1)).await;
        assert_eq!(engine.run_due().await[0].reason, HealthReason::Restarted);
        for _ in 0..3 {
            advance(Duration::from_secs(1)).await;
            engine.run_due().await;
        }
        assert_eq!(
            engine.current(assignment.id).unwrap().state,
            HealthState::Healthy
        );
        runtime.snapshot.lock().as_mut().unwrap().started_at = "runtime-start-2".into();
        advance(Duration::from_secs(1)).await;
        assert_eq!(engine.run_due().await[0].reason, HealthReason::Restarted);
        advance(Duration::from_secs(6)).await;
        assert_eq!(
            engine.current(assignment.id).unwrap().reason,
            HealthReason::Stale
        );
        assert_eq!(engine.run_due().await[0].reason, HealthReason::Stabilizing);
    }

    #[tokio::test(start_paused = true)]
    async fn failed_inspection_and_wrong_container_fail_closed_and_reset_stability() {
        let runtime = inspector();
        let (engine, assignment) = engine(runtime.clone());
        install(&engine, &assignment);
        engine.run_due().await;
        *runtime.snapshot.lock() = Err(InspectFailure::Unauthorized);
        advance(Duration::from_secs(3)).await;
        let report = engine.run_due().await.remove(0);
        assert_eq!(
            (report.state, report.reason),
            (HealthState::Unknown, HealthReason::Unauthorized)
        );
        *runtime.snapshot.lock() = inspector().snapshot.lock().clone();
        advance(Duration::from_secs(1)).await;
        assert_eq!(engine.run_due().await[0].reason, HealthReason::Stabilizing);
        runtime.snapshot.lock().as_mut().unwrap().container_id = "different-container".into();
        advance(Duration::from_secs(3)).await;
        assert_eq!(
            engine.run_due().await[0].reason,
            HealthReason::IdentityMismatch
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_missed_probe_resets_stability_before_freshness_expires() {
        let (engine, assignment) = engine(inspector());
        install(&engine, &assignment);
        engine.run_due().await;
        for _ in 0..3 {
            advance(Duration::from_secs(1)).await;
            engine.run_due().await;
        }
        assert_eq!(
            engine.current(assignment.id).unwrap().state,
            HealthState::Healthy
        );
        advance(Duration::from_secs(2)).await;
        // The old sample is still fresh, but skipping a scheduled observation
        // means a new continuous stability window is required.
        assert_eq!(
            engine.current(assignment.id).unwrap().state,
            HealthState::Healthy
        );
        assert_eq!(engine.run_due().await[0].reason, HealthReason::Stabilizing);
    }

    #[tokio::test(start_paused = true)]
    async fn expired_lease_never_inspects_or_revives_and_replay_does_not_renew() {
        let runtime = inspector();
        let (engine, assignment) = engine(runtime.clone());
        engine
            .install(assignment.clone(), Instant::now() + Duration::from_secs(1))
            .unwrap();
        assert!(matches!(
            engine.install(assignment.clone(), Instant::now() + MAX_LEASE),
            Err(EngineError::AlreadyInstalled { .. })
        ));
        advance(Duration::from_secs(1)).await;
        assert!(engine.run_due().await.is_empty());
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            engine.current(assignment.id).unwrap().reason,
            HealthReason::LeaseExpired
        );
        assert!(matches!(
            engine.renew(assignment.id, assignment.epoch, Instant::now() + MAX_LEASE),
            Err(EngineError::InvalidLease { .. })
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn renewal_requires_current_epoch_and_cannot_extend_beyond_bound() {
        let (engine, assignment) = engine(inspector());
        install(&engine, &assignment);
        assert!(matches!(
            engine.renew(assignment.id, 2, Instant::now() + MAX_LEASE),
            Err(EngineError::UnknownAssignment { .. })
        ));
        assert!(matches!(
            engine.renew(
                assignment.id,
                1,
                Instant::now() + MAX_LEASE + Duration::from_secs(1)
            ),
            Err(EngineError::InvalidLease { .. })
        ));
        advance(Duration::from_secs(60)).await;
        engine
            .renew(assignment.id, 1, Instant::now() + MAX_LEASE)
            .unwrap();
        advance(Duration::from_secs(61)).await;
        assert_ne!(
            engine.current(assignment.id).unwrap().reason,
            HealthReason::LeaseExpired
        );
    }

    #[tokio::test]
    async fn owner_revision_container_and_policy_validation_precede_inspection() {
        let runtime = inspector();
        let (engine, valid) = engine(runtime.clone());
        let mut wrong_owner = valid.clone();
        wrong_owner.owner = ExecutionOwner::Agent(Uuid::new_v4());
        assert!(matches!(
            engine.install(wrong_owner, Instant::now() + MAX_LEASE),
            Err(EngineError::WrongOwner { .. })
        ));
        for mutate in [
            |a: &mut Assignment| a.container_id = "worker-name".into(),
            |a: &mut Assignment| a.epoch = 0,
            |a: &mut Assignment| a.project_id = 0,
            |a: &mut Assignment| a.revision = Uuid::nil(),
            |a: &mut Assignment| a.timeout_secs = 0,
            |a: &mut Assignment| a.interval_secs = 0,
            |a: &mut Assignment| a.probe = Probe::Tcp {},
            |a: &mut Assignment| a.probe = Probe::Process { stable_for_secs: 0 },
        ] {
            let mut invalid = valid.clone();
            mutate(&mut invalid);
            assert!(matches!(
                engine.install(invalid, Instant::now() + MAX_LEASE),
                Err(EngineError::InvalidAssignment { .. })
            ));
        }
        install(&engine, &valid);
        let mut duplicate = valid.clone();
        duplicate.id = Uuid::new_v4();
        assert!(matches!(
            engine.install(duplicate.clone(), Instant::now() + MAX_LEASE),
            Err(EngineError::ContainerOwned { .. })
        ));
        engine.remove(valid.id, 1).unwrap();
        install(&engine, &duplicate);
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_and_cancellation_release_all_slots_without_overlap_or_orphans() {
        let runtime = Arc::new(Inspector {
            blocked: true,
            ..Arc::try_unwrap(inspector()).ok().unwrap()
        });
        let (engine, assignment) = engine(runtime.clone());
        install(&engine, &assignment);
        let task = tokio::spawn({
            let engine = engine.clone();
            async move { engine.run_due().await }
        });
        runtime.entered.notified().await;
        assert!(engine.run_due().await.is_empty());
        assert!(matches!(
            engine.remove(assignment.id, 1),
            Err(EngineError::Busy { .. })
        ));
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(runtime.active.load(Ordering::SeqCst), 0);
        assert_eq!(engine.state.lock().in_flight, 0);
        assert_eq!(
            engine.current(assignment.id).unwrap().reason,
            HealthReason::Cancelled
        );
        advance(Duration::from_secs(1)).await;
        let result = engine.run_due().await;
        assert_eq!(result[0].reason, HealthReason::RuntimeUnavailable);
        assert_eq!(runtime.maximum.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.active.load(Ordering::SeqCst), 0);
        engine.remove(assignment.id, 1).unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn expiry_during_inspection_is_unknown_and_does_not_reschedule_execution() {
        let runtime = Arc::new(Inspector {
            blocked: true,
            ..Arc::try_unwrap(inspector()).ok().unwrap()
        });
        let (engine, assignment) = engine(runtime.clone());
        engine
            .install(assignment.clone(), Instant::now() + Duration::from_secs(1))
            .unwrap();
        assert_eq!(engine.run_due().await[0].reason, HealthReason::LeaseExpired);
        advance(Duration::from_secs(10)).await;
        assert!(engine.run_due().await.is_empty());
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.active.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn scheduling_is_bounded_and_round_robins_ready_projects() {
        let runtime = Arc::new(Inspector {
            blocked: true,
            ..Arc::try_unwrap(inspector()).ok().unwrap()
        });
        let (engine, template) = engine(runtime.clone());
        let mut small_project = None;
        for i in 0..8 {
            let mut assignment = template.clone();
            assignment.id = Uuid::from_u128(i + 1);
            assignment.container_id = format!("{i:064x}");
            if i == 7 {
                assignment.project_id = 2;
                small_project = Some(assignment.id);
            }
            install(&engine, &assignment);
        }
        let task = tokio::spawn({
            let engine = engine.clone();
            async move { engine.run_due().await }
        });
        runtime.entered.notified().await;
        tokio::task::yield_now().await;
        assert_eq!(runtime.active.load(Ordering::SeqCst), MAX_CONCURRENT_PROBES);
        assert!(matches!(
            engine.state.lock().entries[&small_project.unwrap()].phase,
            Phase::Running
        ));
        assert!(engine.run_due().await.is_empty());
        task.abort();
        let _ = task.await;
        assert_eq!(runtime.active.load(Ordering::SeqCst), 0);
        assert_eq!(engine.state.lock().in_flight, 0);
        assert_eq!(
            runtime.maximum.load(Ordering::SeqCst),
            MAX_CONCURRENT_PROBES
        );
    }

    #[tokio::test]
    async fn assignment_limit_and_removal_bound_all_scheduler_indexes() {
        let (engine, template) = engine(inspector());
        for i in 0..MAX_ASSIGNMENTS {
            let mut assignment = template.clone();
            assignment.id = Uuid::from_u128(i as u128 + 1);
            assignment.container_id = format!("{i:064x}");
            install(&engine, &assignment);
        }
        assert!(matches!(
            engine.install(template, Instant::now() + MAX_LEASE),
            Err(EngineError::ContainerOwned { .. })
        ));
        let mut extra = assignment(engine.owner);
        extra.container_id = format!("{:064x}", MAX_ASSIGNMENTS);
        assert!(matches!(
            engine.install(extra, Instant::now() + MAX_LEASE),
            Err(EngineError::Capacity { .. })
        ));
        for i in 0..MAX_ASSIGNMENTS {
            engine.remove(Uuid::from_u128(i as u128 + 1), 1).unwrap();
        }
        let state = engine.state.lock();
        assert!(
            state.entries.is_empty()
                && state.containers.is_empty()
                && state.due.is_empty()
                && state.ready.is_empty()
                && state.projects.is_empty()
        );
    }

    #[tokio::test]
    async fn embedded_and_agent_owners_have_identical_process_semantics() {
        for owner in [
            ExecutionOwner::Embedded(Uuid::new_v4()),
            ExecutionOwner::Agent(Uuid::new_v4()),
        ] {
            let engine = ProbeEngine::new(owner, inspector());
            let assignment = assignment(owner);
            install(&engine, &assignment);
            assert_eq!(engine.run_due().await[0].reason, HealthReason::Stabilizing);
            assert!(engine.run_due().await.is_empty());
        }
    }

    async fn network_assignment(
        probe: Probe,
        endpoint: std::net::SocketAddr,
    ) -> (Arc<ProbeEngine>, Assignment) {
        let runtime = inspector();
        runtime.snapshot.lock().as_mut().unwrap().endpoint = Some(endpoint);
        let (engine, mut assignment) = engine(runtime);
        assignment.probe = probe;
        assignment.container_port = Some(8080);
        install(&engine, &assignment);
        (engine, assignment)
    }

    #[tokio::test]
    async fn http_success_failure_redirect_and_large_body_never_expose_response_output() {
        for (status, expected) in [
            (200, HealthReason::Passed),
            (503, HealthReason::HttpStatusMismatch),
            (302, HealthReason::HttpStatusMismatch),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0; 2048];
                let count = socket.read(&mut request).await.unwrap();
                assert!(request[..count].starts_with(b"GET /ready HTTP/1.1\r\n"));
                let response = format!("HTTP/1.1 {status} test\r\nLocation: http://127.0.0.1:1/private\r\nContent-Length: 100000\r\n\r\n{}", "sensitive-diagnostic".repeat(6000));
                let _ = socket.write_all(response.as_bytes()).await;
            });
            let (engine, assignment) = network_assignment(
                Probe::Http {
                    path: "/ready".into(),
                    expected_status: 200,
                },
                endpoint,
            )
            .await;
            let report = engine.run_due().await.remove(0);
            assert_eq!(report.reason, expected);
            assert_eq!(report.sequence, 1);
            assert_eq!(engine.current(assignment.id).unwrap(), report);
            assert!(!serde_json::to_string(&report)
                .unwrap()
                .contains("sensitive-diagnostic"));
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn tcp_accepts_passive_listener_and_banner_but_rejects_forwarder_close() {
        for mode in [0, 1, 2] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                if mode == 1 {
                    socket.write_all(b"ready\n").await.unwrap();
                }
                if mode != 2 {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                }
            });
            let (engine, _) = network_assignment(Probe::Tcp {}, endpoint).await;
            let report = engine.run_due().await.remove(0);
            assert_eq!(
                report.reason,
                if mode == 2 {
                    HealthReason::ConnectionFailed
                } else {
                    HealthReason::Passed
                }
            );
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn http_hang_and_oversized_status_line_are_bounded_failures() {
        for oversized in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                if oversized {
                    let _ = socket.write_all(&[b'x'; 1024]).await;
                } else {
                    futures::future::pending::<()>().await;
                }
            });
            let (engine, _) = network_assignment(
                Probe::Http {
                    path: "/ready".into(),
                    expected_status: 200,
                },
                endpoint,
            )
            .await;
            let report = engine.run_due().await.remove(0);
            assert_eq!(
                report.reason,
                if oversized {
                    HealthReason::InvalidHttpResponse
                } else {
                    HealthReason::ProbeTimeout
                }
            );
            server.abort();
            let _ = server.await;
        }
    }

    #[tokio::test]
    async fn missing_endpoint_is_not_success_and_http_injection_never_runs() {
        let runtime = inspector();
        let (engine, mut assignment) = engine(runtime.clone());
        assignment.probe = Probe::Tcp {};
        assignment.container_port = Some(8080);
        install(&engine, &assignment);
        assert_eq!(
            engine.run_due().await[0].reason,
            HealthReason::EndpointUnavailable
        );
        engine.remove(assignment.id, 1).unwrap();
        for path in [
            "//unrelated.invalid/path",
            "/ready\r\nHost: invalid",
            "/ready bad",
            "/\\escape",
            "/#fragment",
        ] {
            assignment.probe = Probe::Http {
                path: path.into(),
                expected_status: 200,
            };
            assert!(engine
                .install(assignment.clone(), Instant::now() + MAX_LEASE)
                .is_err());
        }
        assert_eq!(runtime.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn unknown_exec_and_address_bearing_probe_payloads_are_rejected() {
        for input in [
            r#"{"type":"exec","command":["sh","-c","exit 0"]}"#,
            r#"{"type":"unknown"}"#,
            r#"{"type":"tcp","host":"127.0.0.1"}"#,
            r#"{"type":"http","path":"/","expected_status":200,"url":"http://127.0.0.1"}"#,
        ] {
            assert!(
                serde_json::from_str::<Probe>(input).is_err(),
                "unexpected probe payload accepted: {input}"
            );
        }
    }
}
