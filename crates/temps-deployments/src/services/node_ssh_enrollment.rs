// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runs an "add server over SSH" (ADR 048 D2c) in the background and keeps
//! its row in `node_ssh_enrollments` current: the step, the log with the
//! server's output, and how it ended. The credentials live only in the task.
//!
//! Several control-plane processes can share the database, so a running row
//! is leased: the process running it touches `heartbeat_at` while it does,
//! and a running row whose heartbeat is older than `STALE_AFTER` belongs to a
//! process that is gone. Only those are failed as interrupted.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sea_orm::{
    ActiveModelTrait, ConnectionTrait, DatabaseConnection, DbBackend, DbErr, EntityTrait,
    FromQueryResult, PaginatorTrait, QueryOrder, QuerySelect, Set, Statement, TransactionTrait,
};
use temps_core::{AuditContext, AuditLogger, DBDateTime};
use temps_entities::{node_pairings, node_ssh_enrollments, nodes};
use tokio::sync::mpsc;
use tracing::{info, warn};

use super::node_ssh::{self, AgentMode, Enrollment, Progress, SshError};
use crate::handlers::audit::{NodeSshEnrollmentFailedAudit, NodeSshEnrollmentSucceededAudit};

/// Enrollments allowed to run at once.
pub const MAX_RUNNING: u64 = 5;
/// The log keeps its last this-many bytes.
const LOG_LIMIT: i32 = 64 * 1024;
/// The stored error keeps its first this-many bytes.
const ERROR_LIMIT: usize = 8 * 1024;
/// How long to wait for the agent's first heartbeat once it runs.
const HEARTBEAT_WAIT: Duration = Duration::from_secs(120);
/// How often a running enrollment touches its row when nothing else does.
const HEARTBEAT_EVERY: Duration = Duration::from_secs(30);
/// A running enrollment whose row was not touched for this long belongs to a
/// process that is gone. Longer than any step, so a step that could not
/// write for a while (the database was unreachable) is not failed for it.
pub const STALE_AFTER: Duration = Duration::from_secs(node_ssh::LONGEST_STEP.as_secs() + 2 * 60);
const _: () = assert!(STALE_AFTER.as_secs() > node_ssh::LONGEST_STEP.as_secs());
const _: () = assert!(STALE_AFTER.as_secs() > HEARTBEAT_EVERY.as_secs() * 4);
/// Serializes starting enrollments across processes, so the `MAX_RUNNING`
/// check and the insert see the same rows ("SSHENROL").
const START_LOCK_KEY: i64 = 0x5353_4845_4E52_4F4C;
/// Progress events queued for the writer at most.
const QUEUE_CAPACITY: usize = 1024;
/// Queue slots output lines never take, so step changes always fit.
const STEP_RESERVE: usize = 16;
/// Bytes of output lines queued for the writer at most; past it lines are
/// dropped (and counted) rather than making the SSH session wait.
const QUEUE_BYTES: usize = 256 * 1024;

const INTERRUPTED: &str = "The control plane process running this stopped (it was probably \
     restarted). Check the server (`temps doctor mesh` on it), then add it again.";

#[derive(Debug, thiserror::Error)]
pub enum NodeSshEnrollmentError {
    #[error("could not {operation}: {source}")]
    Database {
        operation: &'static str,
        #[source]
        source: DbErr,
    },
    #[error("no SSH enrollment with id {id}")]
    NotFound { id: i32 },
    #[error(
        "{running} servers are already being added over SSH (at most {limit} at once); wait \
         for one to finish"
    )]
    TooManyRunning { running: u64, limit: u64 },
}

fn database(operation: &'static str) -> impl FnOnce(DbErr) -> NodeSshEnrollmentError {
    move |source| NodeSshEnrollmentError::Database { operation, source }
}

/// Why an enrollment failed, as its row and the audit log show it.
#[derive(Debug, thiserror::Error)]
pub enum EnrollmentFailure {
    #[error(
        "{error}{}",
        .pairing_report.as_deref().map(|why| format!("\nThe pairing reports: {why}")).unwrap_or_default()
    )]
    Ssh {
        #[source]
        error: SshError,
        /// Why the pairing's own record says the node was not reached (e.g.
        /// its UDP port is closed).
        pairing_report: Option<String>,
    },
    #[error("could not read pairing {pairing_id}: {source}")]
    PairingUnreadable {
        pairing_id: i32,
        #[source]
        source: DbErr,
    },
    #[error(
        "`temps join --pair` finished on the server, but no node registered with pairing \
         {pairing_id}. Run `temps doctor mesh` on the server."
    )]
    NoNodeRegistered { pairing_id: i32 },
    #[error("could not read node {node_id}: {source}")]
    NodeUnreadable {
        node_id: i32,
        #[source]
        source: DbErr,
    },
    #[error(
        "The node joined (node {node_id}), but its agent sent no heartbeat within \
         {waited_secs}s. On the server, check `temps agent service status` (or \
         /var/log/temps-agent.log without systemd) and run `temps doctor mesh`."
    )]
    NoHeartbeat { node_id: i32, waited_secs: u64 },
}

pub struct NewEnrollment {
    pub name: String,
    pub host: String,
    pub ssh_address: String,
    pub ssh_user: String,
    pub auth_method: String,
    pub host_key_fingerprint: String,
    pub pairing_id: i32,
    pub created_by_user_id: Option<i32>,
}

/// An enrollment without its log, for lists.
#[derive(Debug, Clone, PartialEq, FromQueryResult)]
pub struct EnrollmentSummary {
    pub id: i32,
    pub name: String,
    pub host: String,
    pub ssh_address: String,
    pub ssh_user: String,
    pub auth_method: String,
    pub host_key_fingerprint: String,
    pub pairing_id: Option<i32>,
    pub status: String,
    pub step: String,
    pub error: Option<String>,
    pub agent_mode: Option<String>,
    pub node_id: Option<i32>,
    pub created_at: DBDateTime,
    pub finished_at: Option<DBDateTime>,
}

/// One page of enrollments, newest first.
#[derive(Debug, Clone)]
pub struct EnrollmentPage {
    pub enrollments: Vec<EnrollmentSummary>,
    pub total: u64,
}

/// What the background task needs to run and report on an enrollment.
pub struct EnrollmentJob {
    pub id: i32,
    pub pairing_id: i32,
    pub name: String,
    pub ssh_address: String,
    pub request: Enrollment,
    /// The operator who started it, for the audit record of how it ended.
    pub audit_context: AuditContext,
}

#[derive(Clone)]
pub struct NodeSshEnrollmentService {
    db: Arc<DatabaseConnection>,
    enrollment_tokens: Arc<temps_config::EnrollmentTokenService>,
    audit: Arc<dyn AuditLogger>,
}

impl NodeSshEnrollmentService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        enrollment_tokens: Arc<temps_config::EnrollmentTokenService>,
        audit: Arc<dyn AuditLogger>,
    ) -> Self {
        Self {
            db,
            enrollment_tokens,
            audit,
        }
    }

    /// Enrollments running now, not counting those of processes that are
    /// gone. A quick check before any work; `create` checks again under a
    /// lock.
    pub async fn running_count(&self) -> Result<u64, NodeSshEnrollmentError> {
        count_running(self.db.as_ref())
            .await
            .map_err(database("count running enrollments"))
    }

    /// Record a new running enrollment, unless `MAX_RUNNING` already run.
    /// The count and the insert happen in one transaction under an advisory
    /// lock, so concurrent requests (in this process or another) cannot both
    /// pass the check.
    pub async fn create(
        &self,
        new: NewEnrollment,
    ) -> Result<node_ssh_enrollments::Model, NodeSshEnrollmentError> {
        let txn = self
            .db
            .begin()
            .await
            .map_err(database("start recording the enrollment"))?;
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT pg_advisory_xact_lock($1)",
            [START_LOCK_KEY.into()],
        ))
        .await
        .map_err(database("lock the running enrollments"))?;
        let failed = fail_stale(&txn)
            .await
            .map_err(database("fail interrupted enrollments"))?;
        let running = count_running(&txn)
            .await
            .map_err(database("count running enrollments"))?;
        if running >= MAX_RUNNING {
            return Err(NodeSshEnrollmentError::TooManyRunning {
                running,
                limit: MAX_RUNNING,
            });
        }
        let now = chrono::Utc::now();
        let model = node_ssh_enrollments::ActiveModel {
            name: Set(new.name),
            host: Set(new.host),
            ssh_address: Set(new.ssh_address),
            ssh_user: Set(new.ssh_user),
            auth_method: Set(new.auth_method),
            host_key_fingerprint: Set(new.host_key_fingerprint),
            pairing_id: Set(Some(new.pairing_id)),
            status: Set("running".to_string()),
            step: Set("queued".to_string()),
            log: Set(String::new()),
            created_by_user_id: Set(new.created_by_user_id),
            created_at: Set(now),
            updated_at: Set(now),
            // heartbeat_at is left to the database's NOW(): the clock
            // staleness is judged by.
            ..Default::default()
        }
        .insert(&txn)
        .await
        .map_err(database("record the enrollment"))?;
        txn.commit()
            .await
            .map_err(database("record the enrollment"))?;
        // Once committed: the pairings of the rows failed above.
        self.after_failing(failed).await;
        Ok(model)
    }

    /// Page `page` (from 1) of `per_page` enrollments, newest first, without
    /// their logs.
    pub async fn list(
        &self,
        page: u64,
        per_page: u64,
    ) -> Result<EnrollmentPage, NodeSshEnrollmentError> {
        use node_ssh_enrollments::Column;
        let db = self.db.as_ref();
        let failed = fail_stale(db)
            .await
            .map_err(database("fail interrupted enrollments"))?;
        self.after_failing(failed).await;
        let total = node_ssh_enrollments::Entity::find()
            .count(db)
            .await
            .map_err(database("count enrollments"))?;
        let enrollments = node_ssh_enrollments::Entity::find()
            .select_only()
            .columns([
                Column::Id,
                Column::Name,
                Column::Host,
                Column::SshAddress,
                Column::SshUser,
                Column::AuthMethod,
                Column::HostKeyFingerprint,
                Column::PairingId,
                Column::Status,
                Column::Step,
                Column::Error,
                Column::AgentMode,
                Column::NodeId,
                Column::CreatedAt,
                Column::FinishedAt,
            ])
            .order_by_desc(Column::CreatedAt)
            .order_by_desc(Column::Id)
            .offset(page.saturating_sub(1).saturating_mul(per_page))
            .limit(per_page)
            .into_model::<EnrollmentSummary>()
            .all(db)
            .await
            .map_err(database("list enrollments"))?;
        Ok(EnrollmentPage { enrollments, total })
    }

    pub async fn get(
        &self,
        id: i32,
    ) -> Result<node_ssh_enrollments::Model, NodeSshEnrollmentError> {
        let db = self.db.as_ref();
        let failed = fail_stale(db)
            .await
            .map_err(database("fail interrupted enrollments"))?;
        self.after_failing(failed).await;
        node_ssh_enrollments::Entity::find_by_id(id)
            .one(db)
            .await
            .map_err(database("load the enrollment"))?
            .ok_or(NodeSshEnrollmentError::NotFound { id })
    }

    /// Fail the running enrollments of processes that are gone, and abandon
    /// the pairings they left pending.
    pub async fn fail_interrupted(&self) -> Result<u64, NodeSshEnrollmentError> {
        let failed = fail_stale(self.db.as_ref())
            .await
            .map_err(database("fail interrupted enrollments"))?;
        self.abandon_orphaned_pairings().await;
        Ok(failed)
    }

    /// After `fail_stale` failed `failed` rows: abandon their pairings.
    async fn after_failing(&self, failed: u64) {
        if failed > 0 {
            self.abandon_orphaned_pairings().await;
        }
    }

    /// Best effort: what is left is retried by the next recovery.
    async fn abandon_orphaned_pairings(&self) {
        if let Err(error) =
            abandon_orphaned_pairings(self.db.as_ref(), &self.enrollment_tokens).await
        {
            warn!(%error, "could not look for the pending pairings of failed SSH enrollments");
        }
    }

    /// Stop a failed enrollment's pairing from being dialed and its code
    /// from working. A node that already registered with it keeps it.
    pub async fn abandon_pairing(&self, pairing_id: i32) {
        match abandon(self.db.as_ref(), &self.enrollment_tokens, pairing_id).await {
            Ok(Abandoned::Cancelled { token_id }) => info!(
                pairing = pairing_id,
                token = token_id,
                "cancelled the pairing of a failed SSH enrollment and revoked its enrollment token"
            ),
            Ok(Abandoned::Untouched) => {}
            Err(error) => {
                warn!(pairing = pairing_id, %error, "could not abandon the pairing of a failed SSH enrollment")
            }
        }
    }

    /// Run `job` in the background.
    pub fn spawn(&self, job: EnrollmentJob) {
        let service = self.clone();
        tokio::spawn(async move { service.drive(job).await });
    }

    async fn drive(&self, job: EnrollmentJob) {
        let id = job.id;
        let (sender, events) = mpsc::channel(QUEUE_CAPACITY);
        let queue = Arc::new(Queue::default());
        let writer = {
            let service = self.clone();
            let queue = queue.clone();
            tokio::spawn(async move { service.write_progress(id, events, &queue).await })
        };
        let reporter = Reporter::new(sender, queue);
        let outcome = self.run(job.pairing_id, job.request, &reporter).await;
        if let Err(failure) = &outcome {
            reporter.log_always(format!("Failed: {failure}"));
        }
        let step = reporter.current_step();
        drop(reporter);
        let _ = writer.await;

        let (agent_mode, node_id, error) = match outcome {
            Ok((mode, node_id)) => {
                info!(enrollment = id, node = node_id, "server added over SSH");
                (Some(mode), Some(node_id), None)
            }
            Err(failure) => {
                warn!(enrollment = id, error = %failure, "adding a server over SSH failed");
                self.abandon_pairing(job.pairing_id).await;
                (None, None, Some(clip(&failure.to_string())))
            }
        };
        if let Err(error) = finish(self.db.as_ref(), id, agent_mode, node_id, error.clone()).await {
            warn!(enrollment = id, %error, "could not record how an SSH enrollment ended");
        }

        let recorded = match agent_mode {
            Some(mode) => {
                self.audit
                    .create_audit_log(&NodeSshEnrollmentSucceededAudit {
                        context: job.audit_context,
                        enrollment_id: id,
                        pairing_id: job.pairing_id,
                        name: job.name,
                        ssh_address: job.ssh_address,
                        node_id,
                        agent_mode: agent_mode_name(mode).to_string(),
                    })
                    .await
            }
            None => {
                self.audit
                    .create_audit_log(&NodeSshEnrollmentFailedAudit {
                        context: job.audit_context,
                        enrollment_id: id,
                        pairing_id: job.pairing_id,
                        name: job.name,
                        ssh_address: job.ssh_address,
                        step,
                        error: error.unwrap_or_default(),
                    })
                    .await
            }
        };
        if let Err(error) = recorded {
            warn!(enrollment = id, %error, "SSH enrollment ended but its audit record failed");
        }
    }

    /// Write progress as it comes, and touch the row while nothing else
    /// does, until the reporter is dropped.
    async fn write_progress(&self, id: i32, mut events: mpsc::Receiver<Event>, queue: &Queue) {
        let db = self.db.as_ref();
        let mut ticks = tokio::time::interval(HEARTBEAT_EVERY);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticks.tick().await;
        let mut batch = Vec::new();
        loop {
            tokio::select! {
                received = events.recv_many(&mut batch, 64) => {
                    if received == 0 {
                        break;
                    }
                    write_batch(db, id, batch.drain(..), queue).await;
                }
                _ = ticks.tick() => {
                    if let Err(error) = touch(db, id).await {
                        warn!(enrollment = id, %error, "could not record that an SSH enrollment is running");
                    }
                }
            }
        }
        // Lines dropped after the last batch.
        write_batch(db, id, std::iter::empty(), queue).await;
    }

    async fn run(
        &self,
        pairing_id: i32,
        request: Enrollment,
        progress: &Reporter,
    ) -> Result<(AgentMode, i32), EnrollmentFailure> {
        let db = self.db.as_ref();
        let started = chrono::Utc::now();
        let mode = match node_ssh::enroll(request, progress).await {
            Ok(mode) => mode,
            Err(error) => {
                let pairing_report = node_pairings::Entity::find_by_id(pairing_id)
                    .one(db)
                    .await
                    .ok()
                    .flatten()
                    .and_then(|pairing| pairing.last_rejection.or(pairing.last_error));
                return Err(EnrollmentFailure::Ssh {
                    error,
                    pairing_report,
                });
            }
        };

        progress.step("waiting for the first heartbeat");
        let node_id = node_pairings::Entity::find_by_id(pairing_id)
            .one(db)
            .await
            .map_err(|source| EnrollmentFailure::PairingUnreadable { pairing_id, source })?
            .and_then(|pairing| pairing.node_id)
            .ok_or(EnrollmentFailure::NoNodeRegistered { pairing_id })?;
        let deadline = tokio::time::Instant::now() + HEARTBEAT_WAIT;
        loop {
            let node = nodes::Entity::find_by_id(node_id)
                .one(db)
                .await
                .map_err(|source| EnrollmentFailure::NodeUnreadable { node_id, source })?;
            if node
                .and_then(|node| node.last_heartbeat)
                .is_some_and(|at| at >= started)
            {
                progress.log("The agent is sending heartbeats.");
                return Ok((mode, node_id));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(EnrollmentFailure::NoHeartbeat {
                    node_id,
                    waited_secs: HEARTBEAT_WAIT.as_secs(),
                });
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    }
}

/// Enrollments a process that is gone was running cannot finish: their SSH
/// sessions died with it. Called at startup; only rows whose heartbeat is
/// older than `STALE_AFTER` are failed, so enrollments another process
/// sharing the database is running are left alone.
///
/// Their pairings are then abandoned (`abandon_orphaned_pairings`): left
/// pending, the server could still register with the pairing's token though
/// the operator sees the enrollment failed. That runs on every startup, not
/// only when rows were failed here, so a recovery a crash cut short is
/// finished by the next one.
pub async fn fail_interrupted(db: &Arc<DatabaseConnection>) -> Result<u64, DbErr> {
    let failed = fail_stale(db.as_ref()).await?;
    let tokens = temps_config::EnrollmentTokenService::new(db.clone());
    if let Err(error) = abandon_orphaned_pairings(db.as_ref(), &tokens).await {
        warn!(%error, "could not look for the pending pairings of failed SSH enrollments");
    }
    Ok(failed)
}

/// Why a pairing could not be abandoned.
#[derive(Debug, thiserror::Error)]
enum AbandonError {
    #[error("could not {action} pairing {pairing_id}: {source}")]
    Pairing {
        action: &'static str,
        pairing_id: i32,
        #[source]
        source: temps_network::mesh::MeshError,
    },
    #[error("could not revoke enrollment token {token_id} of pairing {pairing_id}: {source}")]
    RevokeToken {
        pairing_id: i32,
        token_id: i32,
        #[source]
        source: temps_config::EnrollmentError,
    },
}

/// What abandoning a pairing did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Abandoned {
    /// Its enrollment token was revoked and it was cancelled.
    Cancelled { token_id: i32 },
    /// Nothing to do: there is no such pairing, it is not pending, or a node
    /// registered with it.
    Untouched,
}

/// Stop the pairing of an enrollment that will not finish: revoke its
/// enrollment token, so its code no longer registers a node, then cancel it,
/// so it is no longer dialed and its address is released. A pairing a node
/// registered with (completed, or linked and completing) is left alone.
///
/// The token goes first and both steps are idempotent, so stopping part way
/// is safe: the pairing is still pending, and the next recovery finds it and
/// finishes the job.
async fn abandon(
    db: &DatabaseConnection,
    tokens: &temps_config::EnrollmentTokenService,
    pairing_id: i32,
) -> Result<Abandoned, AbandonError> {
    let pairing = temps_network::pairing::get(db, pairing_id)
        .await
        .map_err(|source| AbandonError::Pairing {
            action: "load",
            pairing_id,
            source,
        })?;
    let Some(pairing) = pairing else {
        return Ok(Abandoned::Untouched);
    };
    if !temps_network::pairing::PENDING.contains(&pairing.status.as_str())
        || pairing.node_id.is_some()
    {
        return Ok(Abandoned::Untouched);
    }
    let token_id = pairing.enrollment_token_id;
    tokens
        .revoke(token_id)
        .await
        .map_err(|source| AbandonError::RevokeToken {
            pairing_id,
            token_id,
            source,
        })?;
    let cancelled = temps_network::pairing::cancel_unclaimed(db, pairing_id)
        .await
        .map_err(|source| AbandonError::Pairing {
            action: "cancel",
            pairing_id,
            source,
        })?;
    if cancelled {
        Ok(Abandoned::Cancelled { token_id })
    } else {
        // A node registered with it between the read and the cancel; its
        // token was already used, so revoking it changes nothing for it.
        Ok(Abandoned::Untouched)
    }
}

/// A failed enrollment whose pairing is still pending.
#[derive(Debug, FromQueryResult)]
struct OrphanedPairing {
    enrollment_id: i32,
    pairing_id: i32,
}

/// Abandon the pairings failed enrollments left pending with no node
/// registered: those of enrollments failed as interrupted (their process
/// died before it could), and any a failed enrollment could not abandon at
/// the time. Pending pairings are capped (`temps_network::pairing::
/// MAX_PENDING`), so this is a handful of rows at most. How many were
/// cancelled; one that could not be is logged and left for the next run.
async fn abandon_orphaned_pairings(
    db: &DatabaseConnection,
    tokens: &temps_config::EnrollmentTokenService,
) -> Result<u64, DbErr> {
    let [waiting, key_received] = temps_network::pairing::PENDING;
    let orphans = OrphanedPairing::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT e.id AS enrollment_id, p.id AS pairing_id \
         FROM node_ssh_enrollments e JOIN node_pairings p ON p.id = e.pairing_id \
         WHERE e.status = 'failed' AND p.status IN ($1, $2) AND p.node_id IS NULL \
         ORDER BY p.id",
        [waiting.into(), key_received.into()],
    ))
    .all(db)
    .await?;
    let mut cancelled = 0;
    for orphan in orphans {
        match abandon(db, tokens, orphan.pairing_id).await {
            Ok(Abandoned::Cancelled { token_id }) => {
                cancelled += 1;
                warn!(
                    enrollment = orphan.enrollment_id,
                    pairing = orphan.pairing_id,
                    token = token_id,
                    "cancelled the pairing of a failed SSH enrollment and revoked its enrollment \
                     token, so the server cannot register with it"
                );
            }
            Ok(Abandoned::Untouched) => {}
            Err(error) => warn!(
                enrollment = orphan.enrollment_id,
                pairing = orphan.pairing_id,
                %error,
                "could not abandon the pairing of a failed SSH enrollment; the next recovery retries"
            ),
        }
    }
    Ok(cancelled)
}

async fn fail_stale(db: &impl ConnectionTrait) -> Result<u64, DbErr> {
    let result = db
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE node_ssh_enrollments SET status = 'failed', error = $1, \
             finished_at = NOW(), updated_at = NOW() \
             WHERE status = 'running' AND heartbeat_at < NOW() - make_interval(secs => $2)",
            [INTERRUPTED.into(), STALE_AFTER.as_secs_f64().into()],
        ))
        .await?;
    Ok(result.rows_affected())
}

/// Running enrollments whose process is still there.
async fn count_running(db: &impl ConnectionTrait) -> Result<u64, DbErr> {
    let row = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT COUNT(*) AS running FROM node_ssh_enrollments \
             WHERE status = 'running' AND heartbeat_at >= NOW() - make_interval(secs => $1)",
            [STALE_AFTER.as_secs_f64().into()],
        ))
        .await?;
    let running = match row {
        Some(row) => row.try_get::<i64>("", "running")?,
        None => 0,
    };
    Ok(u64::try_from(running).unwrap_or(0))
}

enum Event {
    Step(String),
    Log(String),
}

/// What is queued for the writer, so output never waits on the database and
/// never piles up in memory.
#[derive(Default)]
struct Queue {
    bytes: AtomicUsize,
    dropped: AtomicU64,
}

/// Sends progress to the task that writes it.
struct Reporter {
    sender: mpsc::Sender<Event>,
    queue: Arc<Queue>,
    step: Mutex<String>,
}

impl Reporter {
    fn new(sender: mpsc::Sender<Event>, queue: Arc<Queue>) -> Self {
        Self {
            sender,
            queue,
            step: Mutex::new("queued".to_string()),
        }
    }

    fn current_step(&self) -> String {
        self.step
            .lock()
            .map(|step| step.clone())
            .unwrap_or_else(|_| "unknown".to_string())
    }

    /// Queue a line, or count it as dropped when the writer is behind.
    fn send_line(&self, line: String, reserved: bool) {
        let len = line.len();
        let room = reserved
            || (self.sender.capacity() > STEP_RESERVE
                && self.queue.bytes.load(Ordering::SeqCst) + len <= QUEUE_BYTES);
        if !room {
            self.queue.dropped.fetch_add(1, Ordering::SeqCst);
            return;
        }
        self.queue.bytes.fetch_add(len, Ordering::SeqCst);
        if self.sender.try_send(Event::Log(line)).is_err() {
            self.queue.bytes.fetch_sub(len, Ordering::SeqCst);
            self.queue.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// A line that must be written (how the enrollment ended): it may use
    /// the slots kept for steps.
    fn log_always(&self, line: String) {
        self.send_line(line, true);
    }
}

impl Progress for Reporter {
    fn step(&self, step: &str) {
        if let Ok(mut current) = self.step.lock() {
            *current = step.to_string();
        }
        // Output lines leave `STEP_RESERVE` slots free, more than an
        // enrollment has steps.
        let _ = self.sender.try_send(Event::Step(step.to_string()));
    }
    fn log(&self, line: &str) {
        self.send_line(line.to_string(), false);
    }
}

/// Write a batch of progress; consecutive lines are written together.
async fn write_batch(
    db: &DatabaseConnection,
    id: i32,
    batch: impl Iterator<Item = Event>,
    queue: &Queue,
) {
    let mut lines = String::new();
    for event in batch {
        let written = match event {
            Event::Log(line) => {
                queue.bytes.fetch_sub(line.len(), Ordering::SeqCst);
                lines.push_str(&line);
                lines.push('\n');
                continue;
            }
            Event::Step(step) => {
                let flushed = if lines.is_empty() {
                    Ok(())
                } else {
                    append_log(db, id, &std::mem::take(&mut lines)).await
                };
                match flushed {
                    Ok(()) => write_step(db, id, &step).await,
                    error => error,
                }
            }
        };
        if let Err(error) = written {
            warn!(enrollment = id, %error, "could not record SSH enrollment progress");
        }
    }
    let dropped = queue.dropped.swap(0, Ordering::SeqCst);
    if dropped > 0 {
        lines.push_str(&dropped_note(dropped));
        lines.push('\n');
    }
    if !lines.is_empty() {
        if let Err(error) = append_log(db, id, &lines).await {
            warn!(enrollment = id, %error, "could not record SSH enrollment progress");
        }
    }
}

fn dropped_note(dropped: u64) -> String {
    format!(
        "[{dropped} lines of output not recorded: the server wrote faster than they could be \
         saved]"
    )
}

async fn write_step(db: &DatabaseConnection, id: i32, step: &str) -> Result<(), DbErr> {
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE node_ssh_enrollments SET step = $2, updated_at = NOW(), heartbeat_at = NOW() \
         WHERE id = $1",
        [id.into(), step.into()],
    ))
    .await?;
    Ok(())
}

async fn append_log(db: &DatabaseConnection, id: i32, lines: &str) -> Result<(), DbErr> {
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE node_ssh_enrollments SET log = RIGHT(log || $2, $3), updated_at = NOW(), \
         heartbeat_at = NOW() WHERE id = $1",
        [id.into(), lines.into(), LOG_LIMIT.into()],
    ))
    .await?;
    Ok(())
}

/// Tell other processes this enrollment is still running.
async fn touch(db: &DatabaseConnection, id: i32) -> Result<(), DbErr> {
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE node_ssh_enrollments SET heartbeat_at = NOW() \
         WHERE id = $1 AND status = 'running'",
        [id.into()],
    ))
    .await?;
    Ok(())
}

fn agent_mode_name(mode: AgentMode) -> &'static str {
    match mode {
        AgentMode::Service => "service",
        AgentMode::Detached => "detached",
    }
}

/// `text` cut to `ERROR_LIMIT` bytes on a character boundary.
fn clip(text: &str) -> String {
    if text.len() <= ERROR_LIMIT {
        return text.to_string();
    }
    let mut end = ERROR_LIMIT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n{}", &text[..end], node_ssh::OUTPUT_TRUNCATED)
}

async fn finish(
    db: &DatabaseConnection,
    id: i32,
    agent_mode: Option<AgentMode>,
    node_id: Option<i32>,
    error: Option<String>,
) -> Result<(), DbErr> {
    let status = if error.is_none() {
        "succeeded"
    } else {
        "failed"
    };
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE node_ssh_enrollments SET status = $2, error = $3, agent_mode = $4, \
         node_id = COALESCE($5, node_id), step = CASE WHEN $2 = 'succeeded' THEN 'done' \
         ELSE step END, finished_at = NOW(), updated_at = NOW(), heartbeat_at = NOW() \
         WHERE id = $1",
        [
            id.into(),
            status.into(),
            error.into(),
            agent_mode
                .map(|mode| agent_mode_name(mode).to_string())
                .into(),
            node_id.into(),
        ],
    ))
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;

    struct NoAudit;

    #[async_trait::async_trait]
    impl AuditLogger for NoAudit {
        async fn create_audit_log(
            &self,
            _operation: &dyn temps_core::AuditOperation,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn service(db: Arc<DatabaseConnection>) -> NodeSshEnrollmentService {
        NodeSshEnrollmentService::new(
            db.clone(),
            Arc::new(temps_config::EnrollmentTokenService::new(db)),
            Arc::new(NoAudit),
        )
    }

    /// The statements a mock connection saw, once nothing else holds it.
    fn statements(db: Arc<DatabaseConnection>) -> Vec<String> {
        Arc::try_unwrap(db)
            .ok()
            .map(|db| db.into_transaction_log())
            .unwrap_or_default()
            .iter()
            .map(|entry| format!("{entry:?}"))
            .collect()
    }

    fn reporter(capacity: usize) -> (Reporter, mpsc::Receiver<Event>, Arc<Queue>) {
        let (sender, events) = mpsc::channel(capacity);
        let queue = Arc::new(Queue::default());
        (Reporter::new(sender, queue.clone()), events, queue)
    }

    fn new_enrollment() -> NewEnrollment {
        NewEnrollment {
            name: "worker-a".into(),
            host: "198.51.100.7".into(),
            ssh_address: "198.51.100.7:22".into(),
            ssh_user: "root".into(),
            auth_method: "password".into(),
            host_key_fingerprint: format!("SHA256:{}", "A".repeat(43)),
            pairing_id: 3,
            created_by_user_id: Some(1),
        }
    }

    fn sample_model(id: i32) -> node_ssh_enrollments::Model {
        let now = chrono::Utc::now();
        node_ssh_enrollments::Model {
            id,
            name: "worker-a".into(),
            host: "198.51.100.7".into(),
            ssh_address: "198.51.100.7:22".into(),
            ssh_user: "root".into(),
            auth_method: "password".into(),
            host_key_fingerprint: format!("SHA256:{}", "A".repeat(43)),
            pairing_id: Some(3),
            status: "running".into(),
            step: "queued".into(),
            log: String::new(),
            error: None,
            agent_mode: None,
            node_id: None,
            created_by_user_id: Some(1),
            created_at: now,
            updated_at: now,
            heartbeat_at: now,
            finished_at: None,
        }
    }

    fn count_row(column: &str, count: i64) -> BTreeMap<String, Value> {
        BTreeMap::from([(column.to_string(), Value::BigInt(Some(count)))])
    }

    fn exec(rows: u64) -> MockExecResult {
        MockExecResult {
            last_insert_id: 0,
            rows_affected: rows,
        }
    }

    #[test]
    fn progress_is_queued_in_order() {
        let (reporter, mut events, _) = reporter(QUEUE_CAPACITY);
        reporter.step("connecting");
        reporter.log("a");
        reporter.log("b");
        assert!(matches!(events.try_recv(), Ok(Event::Step(step)) if step == "connecting"));
        assert!(matches!(events.try_recv(), Ok(Event::Log(line)) if line == "a"));
        assert!(matches!(events.try_recv(), Ok(Event::Log(line)) if line == "b"));
        assert_eq!(reporter.current_step(), "connecting");
    }

    #[test]
    fn a_full_queue_drops_lines_but_keeps_room_for_steps() {
        let (reporter, mut events, queue) = reporter(STEP_RESERVE + 4);
        for n in 0..100 {
            reporter.log(&format!("line {n}"));
        }
        // Only the slots above the reserve took lines; the rest were counted.
        assert_eq!(queue.dropped.load(Ordering::SeqCst), 96);
        for step in ["a", "b", "c", "d", "e", "f", "g"] {
            reporter.step(step);
        }
        reporter.log_always("Failed: why".into());
        let mut received = Vec::new();
        while let Ok(event) = events.try_recv() {
            received.push(event);
        }
        let steps = received
            .iter()
            .filter(|event| matches!(event, Event::Step(_)))
            .count();
        assert_eq!(steps, 7);
        assert!(matches!(received.last(), Some(Event::Log(line)) if line == "Failed: why"));
        assert_eq!(reporter.current_step(), "g");
    }

    #[test]
    fn queued_output_is_bounded_in_bytes() {
        let (reporter, _events, queue) = reporter(QUEUE_CAPACITY);
        let line = "z".repeat(16 * 1024);
        for _ in 0..100 {
            reporter.log(&line);
        }
        assert!(queue.bytes.load(Ordering::SeqCst) <= QUEUE_BYTES);
        assert_eq!(
            queue.dropped.load(Ordering::SeqCst),
            100 - (QUEUE_BYTES / line.len()) as u64
        );
    }

    #[test]
    fn dropped_lines_are_noted_in_the_log() {
        assert_eq!(
            dropped_note(3),
            "[3 lines of output not recorded: the server wrote faster than they could be saved]"
        );
    }

    #[test]
    fn a_long_error_is_clipped_on_a_character_boundary() {
        assert_eq!(clip("short"), "short");
        let long = "é".repeat(ERROR_LIMIT);
        let clipped = clip(&long);
        assert!(clipped.len() <= ERROR_LIMIT + 1 + node_ssh::OUTPUT_TRUNCATED.len());
        assert!(clipped.ends_with(node_ssh::OUTPUT_TRUNCATED));
    }

    #[test]
    fn failures_explain_what_to_do() {
        let failure = EnrollmentFailure::Ssh {
            error: SshError::Auth("the server refused the password for root".into()),
            pairing_report: Some("UDP 51820 is closed".into()),
        };
        assert_eq!(
            failure.to_string(),
            "the server refused the password for root\nThe pairing reports: UDP 51820 is closed"
        );
        let failure = EnrollmentFailure::Ssh {
            error: SshError::Remote("no".into()),
            pairing_report: None,
        };
        assert_eq!(failure.to_string(), "no");
        assert!(EnrollmentFailure::NoNodeRegistered { pairing_id: 4 }
            .to_string()
            .contains("temps doctor mesh"));
        assert!(EnrollmentFailure::NoHeartbeat {
            node_id: 9,
            waited_secs: 120
        }
        .to_string()
        .contains("within 120s"));
    }

    #[test]
    fn a_row_is_stale_only_after_the_longest_step() {
        assert!(STALE_AFTER > node_ssh::LONGEST_STEP);
        assert!(STALE_AFTER > HEARTBEAT_EVERY * 4);
    }

    #[tokio::test]
    async fn creating_locks_then_counts_then_inserts_in_one_transaction() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_results([exec(1), exec(0)])
                .append_query_results([vec![count_row("running", 2)]])
                .append_query_results([vec![sample_model(7)]])
                .into_connection(),
        );
        let created = service(db.clone()).create(new_enrollment()).await.unwrap();
        assert_eq!(created.id, 7);
        let log = statements(db);
        assert_eq!(log.len(), 1, "one transaction: {log:?}");
        let statements = &log[0];
        let lock = statements.find("pg_advisory_xact_lock").unwrap();
        let stale = statements.find("heartbeat_at <").unwrap();
        let count = statements.find("COUNT(*) AS running").unwrap();
        let insert = statements.find("INSERT INTO").unwrap();
        assert!(
            lock < stale && stale < count && count < insert,
            "{statements}"
        );
    }

    #[tokio::test]
    async fn creating_past_the_limit_is_refused() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_results([exec(1), exec(0)])
                .append_query_results([vec![count_row("running", MAX_RUNNING as i64)]])
                .into_connection(),
        );
        let error = service(db.clone())
            .create(new_enrollment())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            NodeSshEnrollmentError::TooManyRunning {
                running: MAX_RUNNING,
                limit: MAX_RUNNING
            }
        ));
        assert!(!statements(db).concat().contains("INSERT INTO"));
    }

    #[tokio::test]
    async fn a_missing_enrollment_is_not_found() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_results([exec(0)])
                .append_query_results([Vec::<node_ssh_enrollments::Model>::new()])
                .into_connection(),
        );
        let error = service(db).get(42).await.unwrap_err();
        assert!(matches!(error, NodeSshEnrollmentError::NotFound { id: 42 }));
    }

    #[tokio::test]
    async fn listing_returns_a_page_without_logs_and_the_total() {
        let db = Arc::new(
            MockDatabase::new(DatabaseBackend::Postgres)
                .append_exec_results([exec(0)])
                .append_query_results([vec![count_row("num_items", 31)]])
                .append_query_results([vec![sample_model(9), sample_model(8)]])
                .into_connection(),
        );
        let page = service(db.clone()).list(3, 10).await.unwrap();
        assert_eq!(page.total, 31);
        assert_eq!(
            page.enrollments.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![9, 8]
        );
        let log = statements(db);
        let select = log.last().unwrap();
        assert!(!select.contains("\"log\""), "{select}");
        assert!(
            select.contains("LIMIT") && select.contains("OFFSET"),
            "{select}"
        );
        assert!(select.contains("Some(20)"), "offset of page 3: {select}");
    }

    #[tokio::test]
    async fn database_errors_name_what_failed() {
        let db = Arc::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let error = service(db).fail_interrupted().await.unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("could not fail interrupted enrollments"),
            "{error}"
        );
    }

    /// Against a real PostgreSQL (Docker, or `TEMPS_TEST_DATABASE_URL`):
    /// only rows whose heartbeat is stale are failed, progress writes keep a
    /// row fresh, the cap ignores stale rows, and the list pages.
    #[tokio::test]
    async fn only_enrollments_of_gone_processes_are_failed() {
        let available = std::env::var_os("TEMPS_TEST_DATABASE_URL").is_some()
            || tokio::process::Command::new("docker")
                .arg("info")
                .output()
                .await
                .map(|output| output.status.success())
                .unwrap_or(false);
        if !available {
            println!("no test database available, skipping");
            return;
        }
        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) => {
                println!("test database not available, skipping: {error}");
                return;
            }
        };
        let db = test_db.connection_arc();
        let sql = |sql: &'static str, values: Vec<Value>| {
            let db = db.clone();
            async move {
                db.execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    sql,
                    values,
                ))
                .await
                .unwrap();
            }
        };
        // Enrollments here have no pairing (it needs a token, a CA...).
        sql(
            "ALTER TABLE node_ssh_enrollments DROP CONSTRAINT node_ssh_enrollments_pairing_id_fkey",
            vec![],
        )
        .await;
        let age = |id: i32, secs: u64| {
            sql(
                "UPDATE node_ssh_enrollments \
                 SET heartbeat_at = NOW() - make_interval(secs => $2) WHERE id = $1",
                vec![id.into(), (secs as f64).into()],
            )
        };
        let service = service(db.clone());

        let mut ids = Vec::new();
        for _ in 0..3 {
            ids.push(service.create(new_enrollment()).await.unwrap().id);
        }
        // The first belongs to a process that stopped long ago, the second
        // to one in its longest step, the third is fresh.
        age(ids[0], STALE_AFTER.as_secs() + 60).await;
        age(ids[1], node_ssh::LONGEST_STEP.as_secs()).await;
        assert_eq!(service.running_count().await.unwrap(), 2);
        assert_eq!(fail_interrupted(&db).await.unwrap(), 1);
        let first = service.get(ids[0]).await.unwrap();
        assert_eq!(first.status, "failed");
        assert_eq!(first.error.as_deref(), Some(INTERRUPTED));
        assert!(first.finished_at.is_some());
        assert_eq!(service.get(ids[1]).await.unwrap().status, "running");

        // Log lines, steps and the periodic heartbeat renew the lease.
        age(ids[1], STALE_AFTER.as_secs() + 60).await;
        append_log(db.as_ref(), ids[1], "still here\n")
            .await
            .unwrap();
        age(ids[2], STALE_AFTER.as_secs() + 60).await;
        write_step(db.as_ref(), ids[2], "pairing").await.unwrap();
        assert_eq!(fail_interrupted(&db).await.unwrap(), 0);
        age(ids[2], STALE_AFTER.as_secs() + 60).await;
        touch(db.as_ref(), ids[2]).await.unwrap();
        assert_eq!(fail_interrupted(&db).await.unwrap(), 0);
        let second = service.get(ids[1]).await.unwrap();
        assert_eq!(second.log, "still here\n");
        assert_eq!(service.get(ids[2]).await.unwrap().step, "pairing");

        // The cap: two running, three more fit, the sixth is refused. A
        // stale row does not hold a slot.
        for _ in 0..3 {
            service.create(new_enrollment()).await.unwrap();
        }
        assert!(matches!(
            service.create(new_enrollment()).await,
            Err(NodeSshEnrollmentError::TooManyRunning { running: 5, .. })
        ));
        age(ids[1], STALE_AFTER.as_secs() + 60).await;
        service.create(new_enrollment()).await.unwrap();

        // How it ended.
        finish(db.as_ref(), ids[2], Some(AgentMode::Service), None, None)
            .await
            .unwrap();
        let third = service.get(ids[2]).await.unwrap();
        assert_eq!(
            (third.status.as_str(), third.step.as_str()),
            ("succeeded", "done")
        );
        assert_eq!(third.agent_mode.as_deref(), Some("service"));

        // The list pages newest first and counts everything.
        let page = service.list(1, 4).await.unwrap();
        assert_eq!(page.total, 7);
        assert_eq!(page.enrollments.len(), 4);
        assert!(page.enrollments[0].id > page.enrollments[3].id);
        let last = service.list(2, 4).await.unwrap();
        assert_eq!(
            last.enrollments.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![ids[2], ids[1], ids[0]]
        );
    }

    /// Against a real PostgreSQL: recovering interrupted enrollments cancels
    /// their pending pairings and revokes those pairings' tokens, leaves
    /// alone pairings a node registered with and those of enrollments still
    /// running, and finishes a recovery a crash cut short.
    #[tokio::test]
    async fn interrupted_enrollments_lose_their_pending_pairing_and_its_token() {
        use sea_orm::ActiveModelTrait;
        use temps_entities::{node_enrollment_tokens, node_pairings};
        use temps_network::pairing::{
            STATUS_CANCELLED, STATUS_COMPLETED, STATUS_KEY_RECEIVED, STATUS_WAITING,
        };

        let available = std::env::var_os("TEMPS_TEST_DATABASE_URL").is_some()
            || tokio::process::Command::new("docker")
                .arg("info")
                .output()
                .await
                .map(|output| output.status.success())
                .unwrap_or(false);
        if !available {
            println!("no test database available, skipping");
            return;
        }
        let test_db = match temps_database::test_utils::TestDatabase::with_migrations().await {
            Ok(db) => db,
            Err(error) => {
                println!("test database not available, skipping: {error}");
                return;
            }
        };
        let db = test_db.connection_arc();
        let tokens = temps_config::EnrollmentTokenService::new(db.clone());
        let service = service(db.clone());
        let now = chrono::Utc::now();

        let node = |name: &'static str| {
            let db = db.clone();
            async move {
                db.query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "INSERT INTO nodes (name, token_hash, address, private_address) \
                     VALUES ($1, $2, 'https://10.201.0.9:3100', '10.201.0.9') RETURNING id",
                    [name.into(), format!("{name:0>64}").into()],
                ))
                .await
                .unwrap()
                .unwrap()
                .try_get::<i32>("", "id")
                .unwrap()
            }
        };
        // A pairing in `status`, registered with by `node_id`, with its
        // token; and an enrollment for it whose process is gone.
        let mut count = 0;
        let mut pairing = |status: &'static str, node_id: Option<i32>| {
            count += 1;
            let index = count;
            let (db, tokens, service) = (db.clone(), &tokens, &service);
            async move {
                let (_, token) = tokens
                    .mint(temps_config::MintParams {
                        max_uses: 1,
                        ttl_secs: 1800,
                        bound_node_name: None,
                        bound_labels: None,
                        created_by_user_id: None,
                        ca_fingerprint: None,
                    })
                    .await
                    .unwrap();
                let pairing = node_pairings::ActiveModel {
                    pairing_id: Set(format!("pairing-{index}")),
                    name: Set(format!("worker-{index}")),
                    node_endpoint: Set(format!("198.51.100.{index}:51820")),
                    mesh_address: Set(format!("10.201.0.{}", index + 1)),
                    secret_encrypted: Set("encrypted".into()),
                    enrollment_token_id: Set(token.id),
                    status: Set(status.to_string()),
                    node_id: Set(node_id),
                    expires_at: Set(now + chrono::Duration::minutes(30)),
                    created_at: Set(now),
                    updated_at: Set(now),
                    ..Default::default()
                }
                .insert(db.as_ref())
                .await
                .unwrap();
                let enrollment = service
                    .create(NewEnrollment {
                        pairing_id: pairing.id,
                        ..new_enrollment()
                    })
                    .await
                    .unwrap();
                (enrollment.id, pairing.id, token.id)
            }
        };
        let age = |id: i32| {
            let db = db.clone();
            async move {
                db.execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE node_ssh_enrollments \
                     SET heartbeat_at = NOW() - make_interval(secs => $2) WHERE id = $1",
                    [id.into(), ((STALE_AFTER.as_secs() + 60) as f64).into()],
                ))
                .await
                .unwrap();
            }
        };
        let state = |pairing_id: i32, token_id: i32| {
            let db = db.clone();
            async move {
                let pairing = node_pairings::Entity::find_by_id(pairing_id)
                    .one(db.as_ref())
                    .await
                    .unwrap()
                    .unwrap();
                let token = node_enrollment_tokens::Entity::find_by_id(token_id)
                    .one(db.as_ref())
                    .await
                    .unwrap()
                    .unwrap();
                (pairing.status, token.revoked_at.is_some())
            }
        };

        let waiting = pairing(STATUS_WAITING, None).await;
        let key_received = pairing(STATUS_KEY_RECEIVED, None).await;
        let completed = pairing(STATUS_COMPLETED, Some(node("worker-done").await)).await;
        // A node registered with it; the pairing completes on its own.
        let linked = pairing(STATUS_KEY_RECEIVED, Some(node("worker-linked").await)).await;
        let running = pairing(STATUS_WAITING, None).await;
        for (enrollment, _, _) in [waiting, key_received, completed, linked] {
            age(enrollment).await;
        }

        assert_eq!(fail_interrupted(&db).await.unwrap(), 4);
        for (enrollment, pairing, token) in [waiting, key_received] {
            assert_eq!(service.get(enrollment).await.unwrap().status, "failed");
            assert_eq!(
                state(pairing, token).await,
                (STATUS_CANCELLED.to_string(), true),
                "pending pairing {pairing}"
            );
        }
        assert_eq!(
            state(completed.1, completed.2).await,
            (STATUS_COMPLETED.to_string(), false)
        );
        assert_eq!(
            state(linked.1, linked.2).await,
            (STATUS_KEY_RECEIVED.to_string(), false)
        );
        // Its process is alive: neither it nor its pairing is touched.
        assert_eq!(service.get(running.0).await.unwrap().status, "running");
        assert_eq!(
            state(running.1, running.2).await,
            (STATUS_WAITING.to_string(), false)
        );

        // A recovery that stopped after failing the row (or after revoking
        // the token) is finished by the next one, which changes nothing else.
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE node_ssh_enrollments SET status = 'failed' WHERE id = $1",
            [running.0.into()],
        ))
        .await
        .unwrap();
        assert_eq!(fail_interrupted(&db).await.unwrap(), 0);
        assert_eq!(
            state(running.1, running.2).await,
            (STATUS_CANCELLED.to_string(), true)
        );
        assert_eq!(
            state(completed.1, completed.2).await,
            (STATUS_COMPLETED.to_string(), false)
        );
        assert_eq!(
            abandon(db.as_ref(), &tokens, waiting.1).await.unwrap(),
            Abandoned::Untouched
        );
    }
}
