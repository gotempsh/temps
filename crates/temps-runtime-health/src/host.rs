// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Durable authorization around the read-only scheduler. A restart creates a
//! new session and never restores execution leases from disk.

use crate::{
    Assignment, EngineError, ExecutionOwner, Observation, ProbeEngine, RuntimeInspector, MAX_LEASE,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use thiserror::Error;
use tokio::time::Instant;
use uuid::Uuid;

pub const MAX_REPORT_BATCH: usize = 100;
/// Reserve file descriptors for Docker, the console, database and proxy.
/// Two co-located roles can each own at most this many container claims.
pub const MAX_HOST_ASSIGNMENTS: usize = 256;
const MAX_FENCE_BYTES: u64 = 16_384;

#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HostIdentity {
    /// Node-issued authorization window, valid for five monotonic seconds.
    pub authorization_window: Uuid,
    pub protocol_version: u32,
    pub session: Uuid,
    pub owner: ExecutionOwner,
}

#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RevokeRequest {
    pub session: Uuid,
    pub assignment_id: Uuid,
    pub epoch: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LeaseRequest {
    pub authorization_window: Uuid,
    pub session: Uuid,
    /// Strictly increasing, persisted by the coordinator before transmission.
    pub renewal: u64,
    pub lease_secs: u64,
    pub assignment: Assignment,
}

#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportRequest {
    pub collection_epoch: u64,
    pub session: Uuid,
    pub challenge: Uuid,
    pub assignments: Vec<Uuid>,
}

#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TimedObservation {
    pub observation: Observation,
    /// Monotonic sample age; never a worker wall-clock timestamp.
    pub age_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportBatch {
    pub collection_epoch: u64,
    pub session: Uuid,
    pub challenge: Uuid,
    pub observations: Vec<TimedObservation>,
}

#[derive(Debug, Error)]
pub enum HostError {
    #[error("Runtime health host journal operation {operation} failed: {source}")]
    Journal {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("Runtime health host journal is invalid: {source}")]
    Decode {
        #[source]
        source: serde_json::Error,
    },
    #[error("Runtime health request targets an obsolete host session or authorization")]
    Fenced,
    #[error("Runtime health request exceeds the bounded protocol limits")]
    Limit,
    #[error("Runtime health host journal has failed; restart and reauthorize the host")]
    JournalFailed,
    #[error("Runtime health shared coordination is unavailable: {reason}")]
    Coordination { reason: &'static str },
    #[error(transparent)]
    Engine(#[from] EngineError),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fence {
    assignment: Assignment,
    renewal: u64,
    revoked: bool,
}

struct Installed {
    _claim: Box<dyn crate::ProbePermit>,
    fence: Fence,
    expires_at: Instant,
}

#[derive(Default)]
struct State {
    installed: HashMap<Uuid, Installed>,
    completed: HashMap<Uuid, (u64, Instant)>,
    reporting_epoch: u64,
    authorization_window: Option<(Uuid, Instant)>,
    reporting_floor: HashMap<Uuid, u64>,
    failed: bool,
    journal_owned: bool,
}

pub struct ProbeHost {
    session: Uuid,
    engine: ProbeEngine,
    inspector: Arc<dyn RuntimeInspector>,
    owner: ExecutionOwner,
    root: PathBuf,
    // Prevent two hosts sharing a journal, even if they use different owners.
    _lock: File,
    state: Mutex<State>,
}

fn journal(operation: &'static str, source: std::io::Error) -> HostError {
    HostError::Journal { operation, source }
}

/// Create a private directory before using it as an authorization boundary.
/// The parent must be operator-owned; callers must not pass workload paths.
pub fn private_directory(path: &Path) -> Result<(), HostError> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .map_err(|e| journal("create private directory", e))?;
    verify_private_directory(path)
}

/// Validate an existing protected namespace without creating metadata paths.
pub fn verify_private_directory(path: &Path) -> Result<(), HostError> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|e| journal("inspect private directory", e))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(HostError::Fenced);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.permissions().mode() & 0o077 != 0
            || metadata.uid() != nix::unistd::geteuid().as_raw()
        {
            return Err(HostError::Fenced);
        }
    }
    Ok(())
}

pub fn private_file(path: &Path) -> Result<File, HostError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits());
    }
    options.open(path).map_err(|e| journal("open lock", e))
}

/// Atomically replace and fsync a bounded authorization record before granting
/// authority in memory. The directory itself is synced after rename.
pub fn persist<T: Serialize>(root: &Path, name: &str, value: &T) -> Result<(), HostError> {
    let bytes = serde_json::to_vec(value).map_err(|source| HostError::Decode { source })?;
    if bytes.len() as u64 > MAX_FENCE_BYTES {
        return Err(HostError::Limit);
    }
    let temporary = root.join(format!(".{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits());
        }
        let mut file = options
            .open(&temporary)
            .map_err(|e| journal("create fence", e))?;
        file.write_all(&bytes)
            .map_err(|e| journal("write fence", e))?;
        file.sync_all().map_err(|e| journal("sync fence", e))?;
        std::fs::rename(&temporary, root.join(name)).map_err(|e| journal("replace fence", e))?;
        File::open(root)
            .and_then(|dir| dir.sync_all())
            .map_err(|e| journal("sync journal directory", e))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

pub fn read_record<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>, HostError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits());
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(journal("read fence", e)),
    };
    let mut bytes = Vec::new();
    file.take(MAX_FENCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| journal("read bounded fence", e))?;
    if bytes.len() as u64 > MAX_FENCE_BYTES {
        return Err(HostError::Limit);
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|source| HostError::Decode { source })
}

impl ProbeHost {
    pub fn open(
        owner: ExecutionOwner,
        root: PathBuf,
        inspector: Arc<dyn RuntimeInspector>,
    ) -> Result<Self, HostError> {
        private_directory(&root)?;
        let lock = private_file(&root.join("host.lock"))?;
        lock.try_lock()
            .map_err(|e| journal("claim journal ownership", e.into()))?;
        Ok(Self {
            session: Uuid::new_v4(),
            engine: ProbeEngine::new(owner, inspector.clone()),
            inspector,
            owner,
            root,
            _lock: lock,
            state: Mutex::new(State {
                journal_owned: true,
                ..State::default()
            }),
        })
    }

    pub fn session(&self) -> Uuid {
        self.session
    }
    pub fn owner(&self) -> ExecutionOwner {
        self.owner
    }
    pub fn identity(&self) -> HostIdentity {
        let window = Uuid::new_v4();
        self.state.lock().authorization_window =
            Some((window, Instant::now() + Duration::from_secs(5)));
        HostIdentity {
            authorization_window: window,
            protocol_version: 1,
            session: self.session,
            owner: self.owner,
        }
    }

    /// Do not call directly on a request executor: journal fsync is blocking.
    pub fn authorize(&self, request: LeaseRequest) -> Result<(), HostError> {
        if request.session != self.session {
            return Err(HostError::Fenced);
        }
        request.assignment.validate()?;
        if request.assignment.owner != self.owner {
            return Err(HostError::Fenced);
        }
        if request.renewal == 0
            || request.lease_secs == 0
            || Duration::from_secs(request.lease_secs) > MAX_LEASE
        {
            return Err(HostError::Limit);
        }
        let mut state = self.state.lock();
        if state.failed {
            return Err(HostError::JournalFailed);
        }
        if !state.authorization_window.is_some_and(|(window, until)| {
            window == request.authorization_window && until > Instant::now()
        }) {
            return Err(HostError::Fenced);
        }
        let expires_at = Instant::now() + Duration::from_secs(request.lease_secs);
        if !state.journal_owned {
            self._lock
                .try_lock()
                .map_err(|e| journal("reclaim journal ownership", e.into()))?;
            state.journal_owned = true;
        }
        let name = format!("{}.json", request.assignment.container_id);
        let previous: Option<Fence> = read_record(&self.root.join(&name))?;
        let active = state.installed.get(&request.assignment.id);
        let renewing = active.is_some_and(|old| {
            old.fence.assignment == request.assignment && old.expires_at > Instant::now()
        });
        if let Some(previous) = previous {
            if request.assignment.epoch < previous.assignment.epoch
                || (request.assignment.epoch == previous.assignment.epoch
                    && (!renewing
                        || previous.revoked
                        || previous.assignment != request.assignment
                        || request.renewal <= previous.renewal))
            {
                return Err(HostError::Fenced);
            }
        }
        let replacing = active.is_some_and(|old| {
            let mut expected = old.fence.assignment.clone();
            expected.epoch = request.assignment.epoch;
            expected == request.assignment && request.assignment.epoch > old.fence.assignment.epoch
        });
        if active.is_some() && !renewing && !replacing {
            return Err(HostError::Fenced);
        }
        if state.installed.iter().any(|(id, old)| {
            *id != request.assignment.id
                && old.fence.assignment.container_id == request.assignment.container_id
        }) {
            return Err(HostError::Fenced);
        }
        if replacing {
            // Higher authority never releases an unresolved check. Read-only
            // handover can proceed only after the previous future is quiescent.
            let old_epoch = active
                .map(|old| old.fence.assignment.epoch)
                .ok_or(HostError::Fenced)?;
            self.engine.remove(request.assignment.id, old_epoch)?;
            state.completed.remove(&request.assignment.id);
            state.reporting_floor.remove(&request.assignment.id);
        }
        if !renewing && !replacing && state.installed.len() >= MAX_HOST_ASSIGNMENTS {
            return Err(HostError::Limit);
        }
        let claim = if renewing || replacing {
            None
        } else {
            Some(
                self.inspector
                    .claim(&request.assignment)
                    .map_err(|_| HostError::Fenced)?,
            )
        };
        let fence = Fence {
            assignment: request.assignment.clone(),
            renewal: request.renewal,
            revoked: false,
        };
        if let Err(error) = persist(&self.root, &name, &fence) {
            state.failed = true;
            return Err(error);
        }
        if renewing {
            self.engine
                .renew(request.assignment.id, request.assignment.epoch, expires_at)?;
        } else {
            self.engine
                .install(request.assignment.clone(), expires_at)?;
        }
        if let Some(installed) = state.installed.get_mut(&request.assignment.id) {
            installed.fence = fence;
            installed.expires_at = expires_at;
        } else if let Some(claim) = claim {
            state.installed.insert(
                request.assignment.id,
                Installed {
                    fence,
                    expires_at,
                    _claim: claim,
                },
            );
        }
        Ok(())
    }

    pub fn revoke(&self, session: Uuid, id: Uuid, epoch: u64) -> Result<(), HostError> {
        if session != self.session {
            return Err(HostError::Fenced);
        }
        let mut state = self.state.lock();
        if state.failed {
            return Err(HostError::JournalFailed);
        }
        let installed = state
            .installed
            .get_mut(&id)
            .filter(|entry| entry.fence.assignment.epoch == epoch)
            .ok_or(HostError::Fenced)?;
        // Refuse handover until the in-flight read/network operation completes.
        self.engine.remove(id, epoch)?;
        installed.fence.revoked = true;
        if let Err(error) = persist(
            &self.root,
            &format!("{}.json", installed.fence.assignment.container_id),
            &installed.fence,
        ) {
            state.failed = true;
            return Err(error);
        }
        state.installed.remove(&id);
        state.completed.remove(&id);
        Ok(())
    }

    pub async fn run_due(&self) {
        if self.state.lock().failed {
            return;
        }
        let observations = self.engine.run_due().await;
        let mut state = self.state.lock();
        for observation in observations {
            if let Some(checked_at) = self.engine.checked_at(observation.assignment_id) {
                state.completed.insert(
                    observation.assignment_id,
                    (observation.sequence, checked_at),
                );
            }
        }
        let expired = state
            .installed
            .iter()
            .filter(|(_, entry)| entry.expires_at <= Instant::now())
            .map(|(id, entry)| (*id, entry.fence.assignment.epoch))
            .collect::<Vec<_>>();
        for (id, epoch) in expired {
            if self.engine.remove(id, epoch).is_ok() {
                state.installed.remove(&id);
                state.completed.remove(&id);
            }
        }
        if state.installed.is_empty() && state.journal_owned {
            match self._lock.unlock() {
                Ok(()) => state.journal_owned = false,
                Err(_) => state.failed = true,
            }
        }
    }

    pub fn report(&self, request: ReportRequest) -> Result<ReportBatch, HostError> {
        if request.session != self.session
            || request.challenge.is_nil()
            || request.collection_epoch == 0
        {
            return Err(HostError::Fenced);
        }
        if request.assignments.len() > MAX_REPORT_BATCH {
            return Err(HostError::Limit);
        }
        let mut state = self.state.lock();
        if state.failed {
            return Err(HostError::JournalFailed);
        }
        if request.collection_epoch < state.reporting_epoch {
            return Err(HostError::Fenced);
        }
        if request.collection_epoch > state.reporting_epoch {
            state.reporting_epoch = request.collection_epoch;
            // A reconnect/failover starts from a high-water mark, never a
            // queued success produced before the new reporting session.
            state.reporting_floor = state
                .completed
                .iter()
                .map(|(id, (sequence, _))| (*id, *sequence))
                .collect();
        }
        let observations = request
            .assignments
            .into_iter()
            .filter_map(|id| {
                let (sequence, checked_at) = state.completed.get(&id)?;
                if *sequence <= state.reporting_floor.get(&id).copied().unwrap_or(0) {
                    return None;
                }
                let observation = self.engine.current(id)?;
                if observation.sequence != *sequence {
                    return None;
                }
                Some(TimedObservation {
                    observation,
                    age_ms: checked_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                })
            })
            .collect();
        Ok(ReportBatch {
            session: self.session,
            challenge: request.challenge,
            collection_epoch: request.collection_epoch,
            observations,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HealthReason, HealthState, InspectFailure, Probe, RuntimeSnapshot};
    use async_trait::async_trait;

    struct Inspector;
    #[async_trait]
    impl RuntimeInspector for Inspector {
        async fn inspect(
            &self,
            assignment: &Assignment,
        ) -> Result<RuntimeSnapshot, InspectFailure> {
            Ok(RuntimeSnapshot {
                container_id: assignment.container_id.clone(),
                running: false,
                restart_count: 0,
                started_at: "started".into(),
                endpoint: None,
            })
        }
    }
    fn assignment(owner: ExecutionOwner) -> Assignment {
        Assignment {
            id: Uuid::new_v4(),
            owner,
            project_id: 1,
            environment_id: 2,
            deployment_id: 3,
            replica_id: 4,
            placement_generation: 1,
            revision: Uuid::new_v4(),
            epoch: 1,
            container_id: "a".repeat(64),
            container_port: None,
            probe: Probe::Process { stable_for_secs: 1 },
            interval_secs: 1,
            timeout_secs: 1,
        }
    }
    fn request(host: &ProbeHost, assignment: Assignment) -> LeaseRequest {
        LeaseRequest {
            authorization_window: host.identity().authorization_window,
            session: host.session(),
            renewal: 1,
            lease_secs: 10,
            assignment,
        }
    }
    fn host(dir: &Path, owner: ExecutionOwner) -> ProbeHost {
        ProbeHost::open(owner, dir.join("journal"), Arc::new(Inspector)).unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn reports_are_bounded_challenged_and_expire_without_touching_the_container() {
        let dir = tempfile::tempdir().unwrap();
        let owner = ExecutionOwner::Embedded(Uuid::new_v4());
        let host = host(dir.path(), owner);
        let assignment = assignment(owner);
        host.authorize(request(&host, assignment.clone())).unwrap();
        host.report(ReportRequest {
            collection_epoch: 1,
            session: host.session(),
            challenge: Uuid::new_v4(),
            assignments: vec![assignment.id],
        })
        .unwrap();
        host.run_due().await;
        let challenge = Uuid::new_v4();
        let report = host
            .report(ReportRequest {
                collection_epoch: 1,
                session: host.session(),
                challenge,
                assignments: vec![assignment.id],
            })
            .unwrap();
        assert_eq!(report.challenge, challenge);
        assert_eq!(
            report.observations[0].observation.state,
            HealthState::Unhealthy
        );
        assert_eq!(
            report.observations[0].observation.reason,
            HealthReason::NotRunning
        );
        assert_eq!(report.observations[0].age_ms, 0);
        tokio::time::advance(Duration::from_secs(5)).await;
        let report = host
            .report(ReportRequest {
                collection_epoch: 1,
                session: host.session(),
                challenge,
                assignments: vec![assignment.id],
            })
            .unwrap();
        assert_eq!(
            report.observations[0].observation.state,
            HealthState::Unknown
        );
        assert_eq!(
            report.observations[0].observation.reason,
            HealthReason::Stale
        );
        assert_eq!(report.observations[0].age_ms, 5000);
        tokio::time::advance(Duration::from_secs(5)).await;
        host.run_due().await;
        assert!(host
            .report(ReportRequest {
                collection_epoch: 1,
                session: host.session(),
                challenge,
                assignments: vec![assignment.id]
            })
            .unwrap()
            .observations
            .is_empty());
    }

    #[test]
    fn restart_does_not_restore_leases_or_accept_previous_sessions_or_epochs() {
        let dir = tempfile::tempdir().unwrap();
        let owner = ExecutionOwner::Agent(Uuid::new_v4());
        let assignment = assignment(owner);
        let original = host(dir.path(), owner);
        let mut lease = request(&original, assignment.clone());
        original.authorize(lease.clone()).unwrap();
        let old_session = original.session();
        drop(original);
        let restarted = host(dir.path(), owner);
        assert_ne!(restarted.session(), old_session);
        assert!(matches!(
            restarted.authorize(lease.clone()),
            Err(HostError::Fenced)
        ));
        lease.session = restarted.session();
        lease.authorization_window = restarted.identity().authorization_window;
        lease.renewal = 2;
        assert!(matches!(
            restarted.authorize(lease.clone()),
            Err(HostError::Fenced)
        ));
        lease.assignment.epoch = 2;
        restarted.authorize(lease).unwrap();
    }

    #[test]
    fn renewals_are_durable_and_replays_cannot_extend_authority() {
        let dir = tempfile::tempdir().unwrap();
        let owner = ExecutionOwner::Agent(Uuid::new_v4());
        let host = host(dir.path(), owner);
        let mut lease = request(&host, assignment(owner));
        host.authorize(lease.clone()).unwrap();
        assert!(matches!(
            host.authorize(lease.clone()),
            Err(HostError::Fenced)
        ));
        lease.renewal += 1;
        host.authorize(lease.clone()).unwrap();
        assert!(matches!(
            host.authorize(lease.clone()),
            Err(HostError::Fenced)
        ));
        lease.assignment.probe = Probe::Tcp {};
        lease.assignment.container_port = Some(8080);
        lease.renewal += 1;
        assert!(host.authorize(lease).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn expired_authorization_requires_a_new_epoch_even_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let owner = ExecutionOwner::Embedded(Uuid::new_v4());
        let host = host(dir.path(), owner);
        let mut lease = request(&host, assignment(owner));
        host.authorize(lease.clone()).unwrap();
        tokio::time::advance(Duration::from_secs(11)).await;
        lease.renewal = 2;
        assert!(matches!(
            host.authorize(lease.clone()),
            Err(HostError::Fenced)
        ));
        host.run_due().await;
        assert!(matches!(
            host.authorize(lease.clone()),
            Err(HostError::Fenced)
        ));
        lease.assignment.epoch += 1;
        lease.authorization_window = host.identity().authorization_window;
        host.authorize(lease).unwrap();
    }

    #[test]
    fn revocation_cannot_be_undone_by_a_delayed_renewal() {
        let dir = tempfile::tempdir().unwrap();
        let owner = ExecutionOwner::Embedded(Uuid::new_v4());
        let host = host(dir.path(), owner);
        let mut lease = request(&host, assignment(owner));
        host.authorize(lease.clone()).unwrap();
        host.revoke(host.session(), lease.assignment.id, lease.assignment.epoch)
            .unwrap();
        lease.renewal += 1;
        assert!(matches!(
            host.authorize(lease.clone()),
            Err(HostError::Fenced)
        ));
        lease.assignment.epoch += 1;
        lease.authorization_window = host.identity().authorization_window;
        host.authorize(lease).unwrap();
    }

    #[test]
    fn duplicate_processes_and_oversized_or_corrupt_journals_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let owner = ExecutionOwner::Agent(Uuid::new_v4());
        let host = host(dir.path(), owner);
        assert!(ProbeHost::open(owner, dir.path().join("journal"), Arc::new(Inspector)).is_err());
        let lease = request(&host, assignment(owner));
        let path = host
            .root
            .join(format!("{}.json", lease.assignment.container_id));
        std::fs::write(&path, "x".repeat(MAX_FENCE_BYTES as usize + 1)).unwrap();
        assert!(matches!(
            host.authorize(lease.clone()),
            Err(HostError::Limit)
        ));
        std::fs::write(&path, b"{}").unwrap();
        assert!(matches!(
            host.authorize(lease),
            Err(HostError::Decode { .. })
        ));
    }

    #[test]
    fn protocol_limits_and_output_allowlist_do_not_accept_exec_or_addresses() {
        let dir = tempfile::tempdir().unwrap();
        let owner = ExecutionOwner::Agent(Uuid::new_v4());
        let host = host(dir.path(), owner);
        let mut lease = request(&host, assignment(owner));
        lease.lease_secs = 121;
        assert!(matches!(host.authorize(lease), Err(HostError::Limit)));
        assert!(matches!(
            host.report(ReportRequest {
                collection_epoch: 1,
                session: host.session(),
                challenge: Uuid::new_v4(),
                assignments: vec![Uuid::new_v4(); MAX_REPORT_BATCH + 1]
            }),
            Err(HostError::Limit)
        ));
        let mut encoded = serde_json::to_value(assignment(owner)).unwrap();
        encoded["probe"] = serde_json::json!({"type":"exec", "command":["sh"]});
        assert!(serde_json::from_value::<Assignment>(encoded.clone()).is_err());
        encoded["probe"] = serde_json::json!({"type":"tcp", "address":"127.0.0.1:1"});
        assert!(serde_json::from_value::<Assignment>(encoded).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_and_public_journals_are_not_adopted() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(private_directory(&target).is_err());
        let link = dir.path().join("link");
        symlink(&target, &link).unwrap();
        assert!(private_directory(&link).is_err());
        let file = dir.path().join("file");
        std::fs::write(&file, b"{}").unwrap();
        let link = dir.path().join("file-link");
        symlink(&file, &link).unwrap();
        assert!(private_file(&link).is_err());
        assert!(read_record::<Fence>(&link).is_err());
    }
    #[tokio::test(start_paused = true)]
    async fn a_new_collection_epoch_requires_a_new_observation_and_fences_old_collectors() {
        let directory = tempfile::tempdir().unwrap();
        let owner = ExecutionOwner::Embedded(Uuid::new_v4());
        let host = host(directory.path(), owner);
        let assignment = assignment(owner);
        host.authorize(request(&host, assignment.clone())).unwrap();
        host.run_due().await;
        let request = |epoch| ReportRequest {
            session: host.session(),
            collection_epoch: epoch,
            challenge: Uuid::new_v4(),
            assignments: vec![assignment.id],
        };
        assert!(host.report(request(1)).unwrap().observations.is_empty());
        tokio::time::advance(Duration::from_secs(1)).await;
        host.run_due().await;
        let batch = host.report(request(1)).unwrap();
        assert_eq!(batch.observations.len(), 1);
        let sequence = batch.observations[0].observation.sequence;
        assert!(host.report(request(2)).unwrap().observations.is_empty());
        assert!(matches!(host.report(request(1)), Err(HostError::Fenced)));
        tokio::time::advance(Duration::from_secs(1)).await;
        host.run_due().await;
        assert!(
            host.report(request(2)).unwrap().observations[0]
                .observation
                .sequence
                > sequence
        );
    }

    #[tokio::test]
    async fn a_higher_epoch_can_replace_only_a_quiescent_assignment() {
        let directory = tempfile::tempdir().unwrap();
        let owner = ExecutionOwner::Embedded(Uuid::new_v4());
        let host = host(directory.path(), owner);
        let mut lease = request(&host, assignment(owner));
        host.authorize(lease.clone()).unwrap();
        host.run_due().await;
        lease.assignment.epoch += 1;
        host.authorize(lease.clone()).unwrap();
        assert!(host
            .report(ReportRequest {
                session: host.session(),
                collection_epoch: 1,
                challenge: Uuid::new_v4(),
                assignments: vec![lease.assignment.id]
            })
            .unwrap()
            .observations
            .is_empty());
        lease.assignment.probe = Probe::Process { stable_for_secs: 2 };
        lease.assignment.epoch += 1;
        assert!(matches!(host.authorize(lease), Err(HostError::Fenced)));
    }
    #[tokio::test(start_paused = true)]
    async fn delayed_authorizations_cannot_start_a_new_lease_from_an_old_request() {
        let directory = tempfile::tempdir().unwrap();
        let owner = ExecutionOwner::Embedded(Uuid::new_v4());
        let host = host(directory.path(), owner);
        let mut request = request(&host, assignment(owner));
        tokio::time::advance(Duration::from_secs(6)).await;
        assert!(matches!(
            host.authorize(request.clone()),
            Err(HostError::Fenced)
        ));
        request.authorization_window = host.identity().authorization_window;
        host.authorize(request).unwrap();
    }
    #[tokio::test(start_paused = true)]
    async fn quiescent_hosts_release_the_journal_for_same_host_coordinator_failover() {
        let directory = tempfile::tempdir().unwrap();
        let owner = ExecutionOwner::Embedded(Uuid::new_v4());
        let original = host(directory.path(), owner);
        let lease = request(&original, assignment(owner));
        original.authorize(lease.clone()).unwrap();
        tokio::time::advance(Duration::from_secs(11)).await;
        original.run_due().await;
        let successor = host(directory.path(), owner);
        let mut lease = request(&successor, lease.assignment);
        lease.assignment.epoch += 1;
        successor.authorize(lease.clone()).unwrap();
        lease.session = original.session();
        lease.authorization_window = original.identity().authorization_window;
        assert!(
            original.authorize(lease).is_err(),
            "the old live process cannot retake an active successor journal"
        );
    }
}
