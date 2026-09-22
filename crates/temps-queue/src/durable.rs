// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::Arc;
use std::time::Duration;

use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
use temps_core::async_trait::async_trait;
use temps_core::{Job, JobDelivery, JobQueue, JobReceipt, JobReceiver, QueueError};
use tokio::sync::{broadcast, Mutex};
use uuid::Uuid;

pub const DEPLOYMENT_CONSUMER: &str = "deployments";
pub const ROUTE_CONSUMER: &str = "routes";
pub const MAX_PENDING_DURABLE_JOBS: u64 = 10_000;
/// Retain recent terminal failures for diagnosis without permanently blocking admission.
pub const MAX_RETAINED_TERMINAL_JOBS: i64 = 1_000;
pub const MAX_DURABLE_DELIVERY_ATTEMPTS: i32 = 5;

/// PostgreSQL-backed command queue used by stateless control-plane mode.
///
/// Broadcast events remain in memory. Only commands with an explicit stable
/// consumer are persisted, which prevents replaying notifications and other
/// fan-out side effects after a restart.
pub struct DurableBroadcastQueue {
    db: Arc<DatabaseConnection>,
    sender: broadcast::Sender<Job>,
    _keep_alive: Mutex<broadcast::Receiver<Job>>,
}

impl DurableBroadcastQueue {
    pub async fn create(
        db: Arc<DatabaseConnection>,
        capacity: usize,
    ) -> Result<Arc<Self>, QueueError> {
        // A stateless control plane has one active owner. Claims left by its
        // predecessor are therefore safe to release exactly once at startup;
        // no time-based lease can duplicate a still-running workflow.
        db.execute(Statement::from_string(
            DbBackend::Postgres,
            "UPDATE durable_job_deliveries SET claimed_at = NULL \
             WHERE completed_at IS NULL AND claimed_at IS NOT NULL",
        ))
        .await
        .map_err(|error| QueueError::Persistence {
            job_type: "startup_reconciliation".to_string(),
            details: error.to_string(),
        })?;

        let (sender, receiver) = broadcast::channel(capacity);
        Ok(Arc::new(Self {
            db,
            sender,
            _keep_alive: Mutex::new(receiver),
        }))
    }

    fn consumers(job: &Job) -> &'static [&'static str] {
        match job {
            Job::GitPushEvent(_) | Job::DeployImageRequested(_) | Job::DeploymentGateRecheck(_) => {
                &[DEPLOYMENT_CONSUMER]
            }
            Job::CustomDomainAdded(_)
            | Job::CustomDomainRemoved(_)
            | Job::CustomRouteAdded(_)
            | Job::CustomRouteRemoved(_)
            | Job::ForceRouteReload(_) => &[ROUTE_CONSUMER],
            _ => &[],
        }
    }

    fn is_owned_by(job: &Job, consumer: &str) -> bool {
        Self::consumers(job).contains(&consumer)
    }
}

struct DurableReceiver {
    db: Arc<DatabaseConnection>,
    receiver: broadcast::Receiver<Job>,
    consumer: &'static str,
}

impl DurableReceiver {
    async fn fail_claim(&self, job_id: Uuid, details: String) -> Result<(), QueueError> {
        self.db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE durable_job_deliveries SET \
                   claimed_at = CASE WHEN attempts >= $3 THEN claimed_at ELSE NULL END, \
                   completed_at = CASE WHEN attempts >= $3 THEN now() ELSE NULL END, \
                   last_error = $4 \
                 WHERE job_id = $1 AND consumer = $2 AND completed_at IS NULL",
                [
                    job_id.into(),
                    self.consumer.into(),
                    MAX_DURABLE_DELIVERY_ATTEMPTS.into(),
                    details.into(),
                ],
            ))
            .await
            .map_err(|error| QueueError::Persistence {
                job_type: "malformed_durable_delivery".to_string(),
                details: format!(
                    "settle malformed job {job_id} for {}: {error}",
                    self.consumer
                ),
            })?;
        Ok(())
    }

    async fn claim(&self) -> Result<Option<JobDelivery>, QueueError> {
        let sql = r#"
WITH candidate AS (
    SELECT d.job_id, d.consumer
    FROM durable_job_deliveries d
    JOIN durable_jobs j ON j.id = d.job_id
    WHERE d.consumer = $1 AND d.completed_at IS NULL AND d.claimed_at IS NULL
    ORDER BY j.created_at, j.id
    LIMIT 1
    FOR UPDATE OF d SKIP LOCKED
)
UPDATE durable_job_deliveries d
SET claimed_at = now(), attempts = d.attempts + 1
FROM candidate c, durable_jobs j
WHERE d.job_id = c.job_id AND d.consumer = c.consumer AND j.id = d.job_id
RETURNING d.job_id, j.payload
"#;
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                [self.consumer.into()],
            ))
            .await
            .map_err(|error| QueueError::Persistence {
                job_type: "durable_claim".to_string(),
                details: format!("consumer {}: {error}", self.consumer),
            })?;

        let Some(row) = row else {
            return Ok(None);
        };
        let job_id: Uuid = row.try_get("", "job_id").map_err(|error| {
            QueueError::InvalidData(format!("durable job id for {}: {error}", self.consumer))
        })?;
        let payload: serde_json::Value = row.try_get("", "payload").map_err(|error| {
            QueueError::InvalidData(format!("durable job payload for {job_id}: {error}"))
        })?;
        let job = match serde_json::from_value(payload) {
            Ok(job) => job,
            Err(error) => {
                self.fail_claim(job_id, format!("deserialize durable job {job_id}: {error}"))
                    .await?;
                return Ok(None);
            }
        };
        Ok(Some(JobDelivery {
            job,
            receipt: Some(JobReceipt {
                job_id,
                consumer: self.consumer.to_string(),
            }),
        }))
    }
}

#[async_trait]
impl JobReceiver for DurableReceiver {
    async fn recv(&mut self) -> Result<Job, QueueError> {
        self.recv_delivery().await.map(|delivery| delivery.job)
    }

    async fn recv_delivery(&mut self) -> Result<JobDelivery, QueueError> {
        loop {
            if let Some(delivery) = self.claim().await? {
                return Ok(delivery);
            }

            tokio::select! {
                received = self.receiver.recv() => {
                    match received {
                        Ok(job) if !DurableBroadcastQueue::is_owned_by(&job, self.consumer) => {
                            return Ok(JobDelivery { job, receipt: None });
                        }
                        Ok(_) => continue,
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => return Err(QueueError::ChannelClosed),
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(250)) => {}
            }
        }
    }
}

#[async_trait]
impl JobQueue for DurableBroadcastQueue {
    async fn send(&self, job: Job) -> Result<(), QueueError> {
        if matches!(&job, Job::GitPushEvent(_)) {
            return Err(QueueError::UnsupportedInStateless {
                job_type: "GitPushEvent".to_string(),
                guidance:
                    "build the image in CI, push it to a registry, then deploy the prebuilt image"
                        .to_string(),
            });
        }
        let consumers = Self::consumers(&job);
        if !consumers.is_empty() {
            let transaction = self
                .db
                .begin()
                .await
                .map_err(|error| QueueError::Persistence {
                    job_type: job.to_string(),
                    details: format!("begin durable enqueue: {error}"),
                })?;
            transaction
                .execute(Statement::from_string(
                    DbBackend::Postgres,
                    "SELECT pg_advisory_xact_lock(837216420119)".to_string(),
                ))
                .await
                .map_err(|error| QueueError::Persistence {
                    job_type: job.to_string(),
                    details: format!("lock durable queue quota: {error}"),
                })?;
            transaction
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "DELETE FROM durable_jobs WHERE id IN (SELECT j.id FROM durable_jobs j \
                 WHERE NOT EXISTS (SELECT 1 FROM durable_job_deliveries d \
                 WHERE d.job_id = j.id AND d.completed_at IS NULL) \
                 ORDER BY j.created_at DESC, j.id DESC OFFSET $1)",
                    [MAX_RETAINED_TERMINAL_JOBS.into()],
                ))
                .await
                .map_err(|error| QueueError::Persistence {
                    job_type: job.to_string(),
                    details: format!("prune retained terminal commands: {error}"),
                })?;
            let pending_row = transaction
                .query_one(Statement::from_string(
                    DbBackend::Postgres,
                    "SELECT COUNT(*)::BIGINT AS pending FROM durable_jobs j WHERE EXISTS (\
                     SELECT 1 FROM durable_job_deliveries d WHERE d.job_id = j.id AND d.completed_at IS NULL)".to_string(),
                ))
                .await
                .map_err(|error| QueueError::Persistence {
                    job_type: job.to_string(),
                    details: format!("count pending durable jobs: {error}"),
                })?
                .ok_or_else(|| QueueError::Persistence {
                    job_type: job.to_string(),
                    details: "durable queue count returned no row".to_string(),
                })?;
            let pending: i64 =
                pending_row
                    .try_get("", "pending")
                    .map_err(|error| QueueError::Persistence {
                        job_type: job.to_string(),
                        details: format!("decode pending durable job count: {error}"),
                    })?;
            if pending >= MAX_PENDING_DURABLE_JOBS as i64 {
                return Err(QueueError::Saturated {
                    job_type: job.to_string(),
                    pending: pending as u64,
                    limit: MAX_PENDING_DURABLE_JOBS,
                });
            }
            let job_id = Uuid::new_v4();
            let payload = serde_json::to_value(&job).map_err(|error| {
                QueueError::InvalidData(format!("serialize durable job {job_id}: {error}"))
            })?;
            transaction
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "INSERT INTO durable_jobs (id, job_type, payload) VALUES ($1, $2, $3)",
                    [job_id.into(), job.to_string().into(), payload.into()],
                ))
                .await
                .map_err(|error| QueueError::Persistence {
                    job_type: job.to_string(),
                    details: format!("insert durable job {job_id}: {error}"),
                })?;
            for consumer in consumers {
                transaction
                    .execute(Statement::from_sql_and_values(
                        DbBackend::Postgres,
                        "INSERT INTO durable_job_deliveries (job_id, consumer) VALUES ($1, $2)",
                        [job_id.into(), (*consumer).into()],
                    ))
                    .await
                    .map_err(|error| QueueError::Persistence {
                        job_type: job.to_string(),
                        details: format!(
                            "insert durable delivery {job_id} for {consumer}: {error}"
                        ),
                    })?;
            }
            transaction
                .commit()
                .await
                .map_err(|error| QueueError::Persistence {
                    job_type: job.to_string(),
                    details: format!("commit durable job {job_id}: {error}"),
                })?;
        }

        // The durable row is already committed. Zero live subscribers is valid:
        // the named worker will claim it when it starts.
        let _ = self.sender.send(job);
        Ok(())
    }

    fn subscribe(&self) -> Box<dyn JobReceiver> {
        Box::new(crate::BroadcastJobReceiver {
            receiver: self.sender.subscribe(),
        })
    }

    fn subscribe_durable(&self, consumer: &'static str) -> Box<dyn JobReceiver> {
        Box::new(DurableReceiver {
            db: self.db.clone(),
            receiver: self.sender.subscribe(),
            consumer,
        })
    }

    async fn acknowledge(&self, receipt: JobReceipt) -> Result<(), QueueError> {
        let transaction = self
            .db
            .begin()
            .await
            .map_err(|error| QueueError::Persistence {
                job_type: "acknowledgement".to_string(),
                details: format!("begin acknowledgement for {}: {error}", receipt.job_id),
            })?;
        transaction
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE durable_job_deliveries SET completed_at = now(), last_error = NULL \
             WHERE job_id = $1 AND consumer = $2 AND completed_at IS NULL",
                [receipt.job_id.into(), receipt.consumer.clone().into()],
            ))
            .await
            .map_err(|error| QueueError::Persistence {
                job_type: "acknowledgement".to_string(),
                details: format!(
                    "complete {} for {}: {error}",
                    receipt.job_id, receipt.consumer
                ),
            })?;
        transaction.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM durable_jobs j WHERE j.id = $1 AND NOT EXISTS (\
             SELECT 1 FROM durable_job_deliveries d WHERE d.job_id = j.id AND d.completed_at IS NULL)",
            [receipt.job_id.into()],
        )).await.map_err(|error| QueueError::Persistence {
            job_type: "acknowledgement".to_string(),
            details: format!("prune completed job {}: {error}", receipt.job_id),
        })?;
        transaction
            .commit()
            .await
            .map_err(|error| QueueError::Persistence {
                job_type: "acknowledgement".to_string(),
                details: format!("commit acknowledgement for {}: {error}", receipt.job_id),
            })?;
        Ok(())
    }

    async fn fail(&self, receipt: JobReceipt, details: String) -> Result<(), QueueError> {
        self.db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE durable_job_deliveries SET \
                   claimed_at = CASE WHEN attempts >= $3 THEN claimed_at ELSE NULL END, \
                   completed_at = CASE WHEN attempts >= $3 THEN now() ELSE NULL END, \
                   last_error = $4 \
                 WHERE job_id = $1 AND consumer = $2 AND completed_at IS NULL",
                [
                    receipt.job_id.into(),
                    receipt.consumer.clone().into(),
                    MAX_DURABLE_DELIVERY_ATTEMPTS.into(),
                    details.into(),
                ],
            ))
            .await
            .map_err(|error| QueueError::Persistence {
                job_type: "failure".to_string(),
                details: format!(
                    "record failure for {} consumer {}: {error}",
                    receipt.job_id, receipt.consumer
                ),
            })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::ConnectionTrait;
    use temps_core::{DeployImageRequestedJob, ProjectCreatedJob};

    async fn create_durable_tables(database: &temps_database::test_utils::TestDatabase) {
        database
            .db
            .execute(Statement::from_string(
                DbBackend::Postgres,
                "CREATE TABLE durable_jobs (id UUID PRIMARY KEY, job_type TEXT NOT NULL, payload JSONB NOT NULL, created_at TIMESTAMPTZ NOT NULL DEFAULT now())"
                    .to_string(),
            ))
            .await
            .expect("create durable jobs table");
        database
            .db
            .execute(Statement::from_string(
                DbBackend::Postgres,
                "CREATE TABLE durable_job_deliveries (job_id UUID NOT NULL REFERENCES durable_jobs(id) ON DELETE CASCADE, consumer TEXT NOT NULL, claimed_at TIMESTAMPTZ, completed_at TIMESTAMPTZ, attempts INTEGER NOT NULL DEFAULT 0, last_error TEXT, PRIMARY KEY(job_id, consumer))"
                    .to_string(),
            ))
            .await
            .expect("create durable deliveries table");
    }

    #[test]
    fn durable_commands_have_one_explicit_owner() {
        let image = Job::DeployImageRequested(DeployImageRequestedJob {
            project_id: 7,
            target_environment_id: Some(9),
            image_ref: "registry.example/app:v1".to_string(),
            health_check_path: None,
            command: None,
            recovery_of_deployment_id: None,
        });
        assert_eq!(
            DurableBroadcastQueue::consumers(&image),
            &[DEPLOYMENT_CONSUMER]
        );

        let event = Job::ProjectCreated(ProjectCreatedJob {
            project_id: 7,
            project_name: "example".to_string(),
        });
        assert!(DurableBroadcastQueue::consumers(&event).is_empty());
    }

    #[tokio::test]
    async fn durable_delivery_survives_until_its_consumer_acknowledges() {
        let database = match temps_database::test_utils::TestDatabase::new().await {
            Ok(database) => database,
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) =>
            {
                eprintln!("Skipping durable queue test: Docker unavailable: {error}");
                return;
            }
            Err(error) => panic!("durable queue test database failed: {error}"),
        };
        create_durable_tables(&database).await;
        let queue = DurableBroadcastQueue::create(database.db.clone(), 8)
            .await
            .expect("create durable queue");
        let mut receiver = queue.subscribe_durable(DEPLOYMENT_CONSUMER);
        queue
            .send(Job::DeployImageRequested(DeployImageRequestedJob {
                project_id: 7,
                target_environment_id: Some(9),
                image_ref: "registry.example/app:v1".to_string(),
                health_check_path: None,
                command: None,
                recovery_of_deployment_id: None,
            }))
            .await
            .expect("persist image deployment");

        let delivery = tokio::time::timeout(Duration::from_secs(2), receiver.recv_delivery())
            .await
            .expect("delivery timeout")
            .expect("receive durable delivery");
        let receipt = delivery.receipt.expect("durable receipt");
        queue.acknowledge(receipt).await.expect("acknowledge");

        let remaining = database
            .db
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT COUNT(*)::BIGINT AS count FROM durable_jobs".to_string(),
            ))
            .await
            .expect("count durable jobs")
            .expect("count row")
            .try_get::<i64>("", "count")
            .expect("decode count");
        assert_eq!(remaining, 0);
    }

    #[tokio::test]
    async fn failed_delivery_retries_then_persists_terminal_failure() {
        let database = match temps_database::test_utils::TestDatabase::new().await {
            Ok(database) => database,
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) =>
            {
                eprintln!("Skipping durable retry test: Docker unavailable: {error}");
                return;
            }
            Err(error) => panic!("durable retry test database failed: {error}"),
        };
        create_durable_tables(&database).await;
        let queue = DurableBroadcastQueue::create(database.db.clone(), 8)
            .await
            .expect("create durable queue");
        let mut receiver = queue.subscribe_durable(DEPLOYMENT_CONSUMER);
        queue
            .send(Job::DeployImageRequested(DeployImageRequestedJob {
                project_id: 7,
                target_environment_id: Some(9),
                image_ref: "registry.example/app:v1".to_string(),
                health_check_path: None,
                command: None,
                recovery_of_deployment_id: None,
            }))
            .await
            .expect("enqueue durable command");

        let mut job_id = None;
        for attempt in 1..=MAX_DURABLE_DELIVERY_ATTEMPTS {
            let delivery = receiver.recv_delivery().await.expect("claim retry");
            let receipt = delivery.receipt.expect("durable receipt");
            job_id = Some(receipt.job_id);
            queue
                .fail(receipt, format!("attempt {attempt} failed"))
                .await
                .expect("record failure");
        }

        let row = database
            .db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT attempts, completed_at IS NOT NULL AS terminal, last_error \
                 FROM durable_job_deliveries WHERE job_id = $1 AND consumer = $2",
                [job_id.expect("job id").into(), DEPLOYMENT_CONSUMER.into()],
            ))
            .await
            .expect("query terminal failure")
            .expect("delivery row retained");
        assert_eq!(
            row.try_get::<i32>("", "attempts").expect("attempts"),
            MAX_DURABLE_DELIVERY_ATTEMPTS
        );
        assert!(row.try_get::<bool>("", "terminal").expect("terminal"));
        assert_eq!(
            row.try_get::<String>("", "last_error").expect("last error"),
            format!("attempt {MAX_DURABLE_DELIVERY_ATTEMPTS} failed")
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(350), receiver.recv_delivery())
                .await
                .is_err(),
            "terminal delivery must not be claimed again"
        );
    }

    #[tokio::test]
    async fn terminal_failures_cannot_permanently_saturate_admission() {
        let database = match temps_database::test_utils::TestDatabase::new().await {
            Ok(database) => database,
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) =>
            {
                eprintln!("Skipping terminal retention test: Docker unavailable: {error}");
                return;
            }
            Err(error) => panic!("terminal retention test database failed: {error}"),
        };
        create_durable_tables(&database).await;
        database.db.execute_unprepared("INSERT INTO durable_jobs (id, job_type, payload) SELECT md5(n::TEXT)::UUID, 'failed', '{}'::JSONB FROM generate_series(1, 10000) n; INSERT INTO durable_job_deliveries (job_id, consumer, attempts, completed_at, last_error) SELECT id, 'deployments', 5, now(), 'terminal fixture' FROM durable_jobs").await.expect("seed full terminal history");
        let queue = DurableBroadcastQueue::create(database.db.clone(), 8)
            .await
            .expect("create queue");
        queue
            .send(Job::DeployImageRequested(DeployImageRequestedJob {
                project_id: 7,
                target_environment_id: Some(9),
                image_ref: "registry.example/app:v1".into(),
                health_check_path: None,
                command: None,
                recovery_of_deployment_id: None,
            }))
            .await
            .expect("terminal history must not reject new work");
        let count = database
            .db
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT COUNT(*)::BIGINT AS count FROM durable_jobs",
            ))
            .await
            .expect("count retained jobs")
            .expect("count row")
            .try_get::<i64>("", "count")
            .expect("count");
        assert_eq!(count, MAX_RETAINED_TERMINAL_JOBS + 1);
        let mut receiver = queue.subscribe_durable(DEPLOYMENT_CONSUMER);
        let delivery = receiver
            .recv_delivery()
            .await
            .expect("new work remains claimable");
        assert!(matches!(delivery.job, Job::DeployImageRequested(_)));
    }

    #[tokio::test]
    async fn malformed_delivery_is_dead_lettered_without_stopping_consumer() {
        let database = match temps_database::test_utils::TestDatabase::new().await {
            Ok(database) => database,
            Err(error)
                if temps_database::test_utils::is_container_runtime_unavailable(
                    &error.to_string(),
                ) =>
            {
                eprintln!("Skipping malformed delivery test: Docker unavailable: {error}");
                return;
            }
            Err(error) => panic!("malformed delivery test database failed: {error}"),
        };
        create_durable_tables(&database).await;
        let malformed_id = Uuid::new_v4();
        database
            .db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO durable_jobs (id, job_type, payload) VALUES ($1, 'malformed-test', $2)",
                [
                    malformed_id.into(),
                    serde_json::json!({"unknown_job": true}).into(),
                ],
            ))
            .await
            .expect("insert malformed job");
        database
            .db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO durable_job_deliveries (job_id, consumer) VALUES ($1, $2)",
                [malformed_id.into(), DEPLOYMENT_CONSUMER.into()],
            ))
            .await
            .expect("insert malformed delivery");
        let queue = DurableBroadcastQueue::create(database.db.clone(), 8)
            .await
            .expect("create durable queue");
        let mut receiver = queue.subscribe_durable(DEPLOYMENT_CONSUMER);
        queue
            .send(Job::DeployImageRequested(DeployImageRequestedJob {
                project_id: 7,
                target_environment_id: Some(9),
                image_ref: "registry.example/app:valid".to_string(),
                health_check_path: None,
                command: None,
                recovery_of_deployment_id: None,
            }))
            .await
            .expect("enqueue valid command");

        let delivery = tokio::time::timeout(Duration::from_secs(3), receiver.recv_delivery())
            .await
            .expect("consumer must continue after malformed command")
            .expect("receive valid command");
        assert!(matches!(delivery.job, Job::DeployImageRequested(_)));

        let row = database
            .db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT attempts, completed_at IS NOT NULL AS terminal FROM durable_job_deliveries WHERE job_id = $1 AND consumer = $2",
                [malformed_id.into(), DEPLOYMENT_CONSUMER.into()],
            ))
            .await
            .expect("query malformed delivery")
            .expect("malformed delivery retained");
        assert_eq!(
            row.try_get::<i32>("", "attempts").expect("attempts"),
            MAX_DURABLE_DELIVERY_ATTEMPTS
        );
        assert!(row.try_get::<bool>("", "terminal").expect("terminal"));
    }
}
