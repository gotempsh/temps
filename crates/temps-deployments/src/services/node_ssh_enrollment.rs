// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runs an "add server over SSH" (ADR 048 D2c) in the background and keeps
//! its row in `node_ssh_enrollments` current: the step, the log with the
//! server's output, and how it ended. The credentials live only in the task.

use std::sync::Arc;
use std::time::Duration;

use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, DbErr,
    EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set, Statement,
};
use temps_entities::{node_pairings, node_ssh_enrollments, nodes};
use tokio::sync::mpsc;
use tracing::{info, warn};

use super::node_ssh::{self, AgentMode, Enrollment, Progress};

/// Enrollments allowed to run at once.
pub const MAX_RUNNING: u64 = 5;
/// The log keeps its last this-many bytes.
const LOG_LIMIT: i32 = 64 * 1024;
/// How long to wait for the agent's first heartbeat once it runs.
const HEARTBEAT_WAIT: Duration = Duration::from_secs(120);

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

pub async fn create(
    db: &DatabaseConnection,
    new: NewEnrollment,
) -> Result<node_ssh_enrollments::Model, DbErr> {
    let now = chrono::Utc::now();
    node_ssh_enrollments::ActiveModel {
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
        ..Default::default()
    }
    .insert(db)
    .await
}

pub async fn running_count(db: &DatabaseConnection) -> Result<u64, DbErr> {
    node_ssh_enrollments::Entity::find()
        .filter(node_ssh_enrollments::Column::Status.eq("running"))
        .count(db)
        .await
}

pub async fn list(db: &DatabaseConnection) -> Result<Vec<node_ssh_enrollments::Model>, DbErr> {
    node_ssh_enrollments::Entity::find()
        .order_by_desc(node_ssh_enrollments::Column::CreatedAt)
        .limit(20)
        .all(db)
        .await
}

pub async fn get(
    db: &DatabaseConnection,
    id: i32,
) -> Result<Option<node_ssh_enrollments::Model>, DbErr> {
    node_ssh_enrollments::Entity::find_by_id(id).one(db).await
}

/// Enrollments a previous process was running cannot finish: their SSH
/// sessions died with it. Called once at startup.
pub async fn fail_interrupted(db: &DatabaseConnection) -> Result<u64, DbErr> {
    let result = db
        .execute(Statement::from_string(
            DbBackend::Postgres,
            "UPDATE node_ssh_enrollments SET status = 'failed', \
             error = 'The control plane restarted while this was running. Check the server \
             (`temps doctor mesh` on it), then add it again.', \
             finished_at = NOW(), updated_at = NOW() WHERE status = 'running'",
        ))
        .await?;
    Ok(result.rows_affected())
}

enum Event {
    Step(String),
    Log(String),
}

/// Sends progress to the task that writes it, so SSH output never waits on
/// the database.
struct Reporter(mpsc::UnboundedSender<Event>);

impl Progress for Reporter {
    fn step(&self, step: &str) {
        let _ = self.0.send(Event::Step(step.to_string()));
    }
    fn log(&self, line: &str) {
        let _ = self.0.send(Event::Log(line.to_string()));
    }
}

async fn write_step(db: &DatabaseConnection, id: i32, step: &str) -> Result<(), DbErr> {
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE node_ssh_enrollments SET step = $2, updated_at = NOW() WHERE id = $1",
        [id.into(), step.into()],
    ))
    .await?;
    Ok(())
}

async fn append_log(db: &DatabaseConnection, id: i32, lines: &str) -> Result<(), DbErr> {
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE node_ssh_enrollments SET log = RIGHT(log || $2, $3), updated_at = NOW() \
         WHERE id = $1",
        [id.into(), lines.into(), LOG_LIMIT.into()],
    ))
    .await?;
    Ok(())
}

async fn finish(
    db: &DatabaseConnection,
    id: i32,
    outcome: Result<(AgentMode, Option<i32>), String>,
) -> Result<(), DbErr> {
    let (status, error, agent_mode, node_id) = match outcome {
        Ok((mode, node_id)) => (
            "succeeded",
            None,
            Some(match mode {
                AgentMode::Service => "service",
                AgentMode::Detached => "detached",
            }),
            node_id,
        ),
        Err(error) => ("failed", Some(error), None, None),
    };
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE node_ssh_enrollments SET status = $2, error = $3, agent_mode = $4, \
         node_id = COALESCE($5, node_id), step = CASE WHEN $2 = 'succeeded' THEN 'done' \
         ELSE step END, finished_at = NOW(), updated_at = NOW() WHERE id = $1",
        [
            id.into(),
            status.into(),
            error.into(),
            agent_mode.map(str::to_string).into(),
            node_id.into(),
        ],
    ))
    .await?;
    Ok(())
}

/// Run `request` for enrollment `id` in the background.
pub fn spawn(db: Arc<DatabaseConnection>, id: i32, pairing_id: i32, request: Enrollment) {
    tokio::spawn(async move {
        let (sender, mut events) = mpsc::unbounded_channel();
        let writer = {
            let db = db.clone();
            tokio::spawn(async move {
                let mut batch = Vec::new();
                while events.recv_many(&mut batch, 64).await > 0 {
                    // Consecutive lines are written together.
                    let mut lines = String::new();
                    for event in batch.drain(..) {
                        let written = match event {
                            Event::Log(line) => {
                                lines.push_str(&line);
                                lines.push('\n');
                                continue;
                            }
                            Event::Step(step) => {
                                let flushed = if lines.is_empty() {
                                    Ok(())
                                } else {
                                    append_log(&db, id, &std::mem::take(&mut lines)).await
                                };
                                match flushed {
                                    Ok(()) => write_step(&db, id, &step).await,
                                    error => error,
                                }
                            }
                        };
                        if let Err(error) = written {
                            warn!(enrollment = id, %error, "could not record SSH enrollment progress");
                        }
                    }
                    if !lines.is_empty() {
                        if let Err(error) = append_log(&db, id, &lines).await {
                            warn!(enrollment = id, %error, "could not record SSH enrollment progress");
                        }
                    }
                }
            })
        };
        let reporter = Reporter(sender);
        let outcome = run(&db, pairing_id, request, &reporter).await;
        if let Err(message) = &outcome {
            reporter.log(&format!("Failed: {message}"));
        }
        drop(reporter);
        let _ = writer.await;
        match &outcome {
            Ok(_) => info!(enrollment = id, "server added over SSH"),
            Err(error) => {
                warn!(enrollment = id, %error, "adding a server over SSH failed");
                abandon_pairing(&db, pairing_id).await;
            }
        }
        if let Err(error) = finish(&db, id, outcome).await {
            warn!(enrollment = id, %error, "could not record how an SSH enrollment ended");
        }
    });
}

/// Stop a failed enrollment's pairing from being dialed and its code from
/// working. A node that already joined keeps its (completed) pairing.
pub(crate) async fn abandon_pairing(db: &Arc<DatabaseConnection>, pairing_id: i32) {
    let pairing = match temps_network::pairing::get(db.as_ref(), pairing_id).await {
        Ok(Some(pairing)) => pairing,
        Ok(None) => return,
        Err(error) => {
            warn!(pairing = pairing_id, %error, "could not load the pairing of a failed SSH enrollment");
            return;
        }
    };
    match temps_network::pairing::cancel(db.as_ref(), pairing_id).await {
        Ok(true) => {
            if let Err(error) = temps_config::EnrollmentTokenService::new(db.clone())
                .revoke(pairing.enrollment_token_id)
                .await
            {
                warn!(pairing = pairing_id, %error, "could not revoke the enrollment token of a failed SSH enrollment");
            }
        }
        Ok(false) => {}
        Err(error) => {
            warn!(pairing = pairing_id, %error, "could not cancel the pairing of a failed SSH enrollment")
        }
    }
}

async fn run(
    db: &DatabaseConnection,
    pairing_id: i32,
    request: Enrollment,
    progress: &Reporter,
) -> Result<(AgentMode, Option<i32>), String> {
    let started = chrono::Utc::now();
    let mode = match node_ssh::enroll(request, progress).await {
        Ok(mode) => mode,
        Err(error) => {
            // The pairing's own record usually says why the node was not
            // reached (e.g. its UDP port is closed).
            let pairing = node_pairings::Entity::find_by_id(pairing_id)
                .one(db)
                .await
                .ok()
                .flatten();
            let why = pairing
                .and_then(|pairing| pairing.last_rejection.or(pairing.last_error))
                .map(|why| format!("\nThe pairing reports: {why}"))
                .unwrap_or_default();
            return Err(format!("{error}{why}"));
        }
    };

    progress.step("waiting for the first heartbeat");
    let node_id = node_pairings::Entity::find_by_id(pairing_id)
        .one(db)
        .await
        .map_err(|error| format!("could not read the pairing: {error}"))?
        .and_then(|pairing| pairing.node_id);
    let Some(node_id) = node_id else {
        return Err(
            "`temps join --pair` finished on the server, but no node registered with this \
             pairing. Run `temps doctor mesh` on the server."
                .to_string(),
        );
    };
    let deadline = tokio::time::Instant::now() + HEARTBEAT_WAIT;
    loop {
        let node = nodes::Entity::find_by_id(node_id)
            .one(db)
            .await
            .map_err(|error| format!("could not read the node: {error}"))?;
        if node
            .and_then(|node| node.last_heartbeat)
            .is_some_and(|at| at >= started)
        {
            progress.log("The agent is sending heartbeats.");
            return Ok((mode, Some(node_id)));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "The node joined, but its agent sent no heartbeat within {}s. On the server, \
                 check `temps agent service status` (or /var/log/temps-agent.log without \
                 systemd) and run `temps doctor mesh`.",
                HEARTBEAT_WAIT.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_is_queued_in_order() {
        let (sender, mut events) = mpsc::unbounded_channel();
        let reporter = Reporter(sender);
        reporter.step("connecting");
        reporter.log("a");
        reporter.log("b");
        assert!(matches!(events.try_recv(), Ok(Event::Step(step)) if step == "connecting"));
        assert!(matches!(events.try_recv(), Ok(Event::Log(line)) if line == "a"));
        assert!(matches!(events.try_recv(), Ok(Event::Log(line)) if line == "b"));
    }
}
