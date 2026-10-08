// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Orchestration of data imports, identical for every engine.
//!
//! [`DataImportService::start`] validates everything that can be validated
//! up front — the engine supports imports, the service is a running local
//! standalone service, the target name, the source URL and its SSRF guard,
//! no restore or other import is writing into the same place, the target is
//! empty unless `replace` was asked for — then records a `running` row and
//! hands the copy to a background task. The HTTP request returns at once and
//! the run is polled.
//!
//! The background task moves through three phases, each persisted so the
//! console can show where a run is and a restart can say where it stopped:
//! `preparing_target` (create or re-create the database), `transferring`
//! (the helper container streams the dump) and `verifying` (measure what
//! landed). Only the task — or startup reconciliation for a task that died
//! with its process — writes a terminal status, and only while the row is
//! still `running`.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, Condition, ConnectionTrait,
    DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect,
    TransactionTrait,
};
use temps_core::DockerHandle;
use temps_entities::{external_services, restore_runs, service_data_imports, users};
use tracing::{error, info, warn};

use super::runner::{self, HelperOutcome, HelperRequest};
use super::source::{pin_source_hosts, scrub_secrets, ImportSource, PinnedHost};
use super::{
    plan_target_preparation, DataImportEngine, DataImportError, DataImportSpec, TargetPreparation,
    TransferTarget,
};
use crate::externalsvc::{ExternalService, ServiceConfig, ServiceType};
use crate::services::{ExternalServiceError, ExternalServiceManager};

/// Run status: the import is in progress.
pub const STATUS_RUNNING: &str = "running";
/// Run status: the data is in the target database.
pub const STATUS_SUCCEEDED: &str = "succeeded";
/// Run status: the import stopped on an error; `error_message` says why.
pub const STATUS_FAILED: &str = "failed";
/// Run status: the import was cancelled on request.
pub const STATUS_CANCELLED: &str = "cancelled";
/// Run status: the process running the import died; set at the next start.
pub const STATUS_INTERRUPTED: &str = "interrupted";

/// Phase: creating, or dropping and re-creating, the target database.
pub const PHASE_PREPARING_TARGET: &str = "preparing_target";
/// Phase: the helper container is copying the data.
pub const PHASE_TRANSFERRING: &str = "transferring";
/// Phase: measuring the imported database.
pub const PHASE_VERIFYING: &str = "verifying";
/// Phase: the run succeeded. Runs that end otherwise keep the phase they
/// stopped in.
pub const PHASE_FINISHED: &str = "finished";

/// Transfer time limit when the request names none.
pub const DEFAULT_TIMEOUT_MINUTES: u32 = 60;
/// Longest transfer time limit a request may ask for.
pub const MAX_TIMEOUT_MINUTES: u32 = 24 * 60;

/// Restore statuses that mean a restore may still be writing.
const ACTIVE_RESTORE_STATUSES: [&str; 2] = ["pending", "running"];

/// First and longest wait between attempts to settle an interrupted run.
const RECONCILE_RETRY_BASE: Duration = Duration::from_secs(2);
const RECONCILE_RETRY_MAX: Duration = Duration::from_secs(60);

/// What one attempt to settle an interrupted run achieved.
enum Reconciled {
    /// This attempt marked it `interrupted`.
    Settled,
    /// It was already terminal (or gone); nothing to do.
    AlreadySettled,
    /// Docker or the database failed; try again later, run kept locked.
    Retry,
}

/// What the caller asked for.
#[derive(Clone)]
pub struct StartDataImport {
    pub service_id: i32,
    /// Source connection string. Secret: never stored, logged or returned.
    pub source_url: String,
    pub target_database: String,
    pub replace: bool,
    pub timeout_minutes: Option<u32>,
    pub created_by: Option<i32>,
}

impl std::fmt::Debug for StartDataImport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartDataImport")
            .field("service_id", &self.service_id)
            .field("source_url", &"***")
            .field("target_database", &self.target_database)
            .field("replace", &self.replace)
            .field("timeout_minutes", &self.timeout_minutes)
            .finish()
    }
}

/// Who started a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunStarter {
    pub user_id: i32,
    pub name: String,
    pub email: String,
}

/// A run and the user who started it (`None` for runs whose user was
/// deleted, or that no user started).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataImportRun {
    pub run: service_data_imports::Model,
    pub started_by: Option<RunStarter>,
}

impl From<(service_data_imports::Model, Option<users::Model>)> for DataImportRun {
    fn from((run, user): (service_data_imports::Model, Option<users::Model>)) -> Self {
        Self {
            run,
            started_by: user.map(|user| RunStarter {
                user_id: user.id,
                name: user.name,
                email: user.email,
            }),
        }
    }
}

/// Whether a service can receive imported data, and if not, why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataImportAvailability {
    pub service_id: i32,
    pub service_type: String,
    /// The service's engine implements imports.
    pub supported: bool,
    /// An import could start right now.
    pub available: bool,
    /// Why `supported` or `available` is false.
    pub reason: Option<String>,
    /// The engine's description, when it supports imports.
    pub spec: Option<DataImportSpec>,
}

/// Starts data imports and answers questions about them.
pub struct DataImportService {
    db: Arc<DatabaseConnection>,
    manager: Arc<ExternalServiceManager>,
    docker: Arc<DockerHandle>,
}

/// A service that passed every engine-independent check, with its engine.
struct ResolvedTarget {
    service: external_services::Model,
    instance: Box<dyn ExternalService>,
}

impl ResolvedTarget {
    fn engine(&self) -> Result<&dyn DataImportEngine, DataImportError> {
        self.instance
            .data_import()
            .ok_or_else(|| unsupported_engine(&self.service))
    }
}

fn unsupported_engine(service: &external_services::Model) -> DataImportError {
    DataImportError::Unsupported {
        service_id: service.id,
        service_type: service.service_type.clone(),
        reason: format!(
            "importing data into {} services is not available yet",
            service.service_type
        ),
    }
}

impl DataImportService {
    pub fn new(
        db: Arc<DatabaseConnection>,
        manager: Arc<ExternalServiceManager>,
        docker: Arc<DockerHandle>,
    ) -> Self {
        Self {
            db,
            manager,
            docker,
        }
    }

    /// Whether `service_id` can receive imported data. Never fails for a
    /// service that exists: every obstacle becomes a `reason`, so the console
    /// can explain it instead of hiding the feature.
    pub async fn availability(
        &self,
        service_id: i32,
    ) -> Result<DataImportAvailability, DataImportError> {
        let service = self.load_service(service_id).await?;
        let mut availability = DataImportAvailability {
            service_id,
            service_type: service.service_type.clone(),
            supported: false,
            available: false,
            reason: None,
            spec: None,
        };
        let resolved = match self.resolve(service).await {
            Ok(resolved) => resolved,
            Err(error) => {
                availability.reason = Some(reason_of(&error));
                return Ok(availability);
            }
        };
        match resolved.engine() {
            Ok(engine) => {
                availability.supported = true;
                availability.spec = Some(engine.import_spec());
            }
            Err(error) => {
                availability.reason = Some(reason_of(&error));
                return Ok(availability);
            }
        }
        match ensure_running(&resolved.service) {
            Ok(()) => availability.available = true,
            Err(error) => availability.reason = Some(reason_of(&error)),
        }
        Ok(availability)
    }

    /// Validate the request, record a `running` run and start the copy in
    /// the background. Returns the run so the caller can poll it.
    pub async fn start(
        &self,
        request: StartDataImport,
    ) -> Result<service_data_imports::Model, DataImportError> {
        let service_id = request.service_id;
        let timeout = validate_timeout(request.timeout_minutes)?;

        let service = self.load_service(service_id).await?;
        let resolved = self.resolve(service).await?;
        let engine = resolved.engine()?;
        ensure_running(&resolved.service)?;

        engine.validate_target_database(&request.target_database)?;
        let source = engine.parse_source(&request.source_url)?;
        let pins = pin_source_hosts(&source).await?;

        // Pre-checks for a readable error before any work; the authoritative
        // checks repeat under the service lock when the run is recorded.
        ensure_no_active_restore(self.db.as_ref(), service_id).await?;
        if let Some(run) = self
            .find_running(service_id, &request.target_database)
            .await?
        {
            return Err(DataImportError::AlreadyRunning {
                service_id,
                database: request.target_database.clone(),
                run_id: run.id,
            });
        }

        let config = self.service_config(service_id).await?;
        let spec = engine.import_spec();
        let inspection = engine
            .inspect_target(&config, &request.target_database)
            .await?;
        // Refuses a non-empty target without `replace` before a run is
        // recorded. The job plans again once the run holds the database lock.
        plan_target_preparation(
            service_id,
            &request.target_database,
            inspection,
            request.replace,
            &spec.object_noun,
        )?;

        let docker = self.docker.require()?;
        let target_container = engine.target_container(&config)?;
        let (network, target_host) =
            runner::resolve_target_network(&docker, service_id, &target_container).await?;
        let target_port = resolved.instance.get_docker_internal_port();

        let run = insert_run_locked(
            self.db.as_ref(),
            service_id,
            &request.target_database,
            service_data_imports::ActiveModel {
                service_id: Set(service_id),
                service_type: Set(resolved.service.service_type.clone()),
                target_database: Set(request.target_database.clone()),
                source_display: Set(source.masked()),
                source_database: Set(source.database().to_string()),
                replace_existing: Set(request.replace),
                atomic_transfer: Set(spec.atomic),
                status: Set(STATUS_RUNNING.to_string()),
                phase: Set(PHASE_PREPARING_TARGET.to_string()),
                timeout_seconds: Set(i32::try_from(timeout.as_secs()).unwrap_or(i32::MAX)),
                created_by: Set(request.created_by),
                ..Default::default()
            },
        )
        .await?;

        info!(
            run_id = run.id,
            service_id,
            service_type = %run.service_type,
            target_database = %run.target_database,
            source = %run.source_display,
            replace = run.replace_existing,
            "Starting data import"
        );

        let job = ImportJob {
            run_id: run.id,
            service_id,
            database: request.target_database,
            replace: request.replace,
            timeout,
            config,
            source,
            pins,
            network,
            target_host,
            target_port,
            atomic: spec.atomic,
            instance: resolved.instance,
            db: self.db.clone(),
            docker,
        };
        let db = self.db.clone();
        let run_id = run.id;
        tokio::spawn(async move {
            // The job runs in its own task so a panic inside it is observed
            // here and still settles the run instead of leaving it running.
            let outcome = match tokio::spawn(job.execute()).await {
                Ok(outcome) => outcome,
                Err(join_error) => {
                    error!(run_id, error = %join_error, "Data import task panicked");
                    JobOutcome::failed("the import task stopped unexpectedly".to_string())
                }
            };
            finalize_run(&db, run_id, outcome).await;
        });

        Ok(run)
    }

    /// Runs of a service, newest first, each with the user who started it
    /// (one joined query). `page` is 1-based; `page_size` defaults to 20 and
    /// is capped at 100.
    pub async fn list_runs(
        &self,
        service_id: i32,
        page: Option<u64>,
        page_size: Option<u64>,
    ) -> Result<(Vec<DataImportRun>, u64), DataImportError> {
        self.load_service(service_id).await?;
        let page = page.unwrap_or(1).max(1);
        let page_size = page_size.unwrap_or(20).clamp(1, 100);
        let paginator = service_data_imports::Entity::find()
            .find_also_related(users::Entity)
            .filter(service_data_imports::Column::ServiceId.eq(service_id))
            .order_by_desc(service_data_imports::Column::CreatedAt)
            .order_by_desc(service_data_imports::Column::Id)
            .paginate(self.db.as_ref(), page_size);
        let total = paginator.num_items().await?;
        let items = paginator
            .fetch_page(page - 1)
            .await?
            .into_iter()
            .map(DataImportRun::from)
            .collect();
        Ok((items, total))
    }

    /// One run with the user who started it, scoped to its service: an id of
    /// another service is a 404.
    pub async fn get_run(
        &self,
        service_id: i32,
        run_id: i32,
    ) -> Result<DataImportRun, DataImportError> {
        service_data_imports::Entity::find_by_id(run_id)
            .find_also_related(users::Entity)
            .filter(service_data_imports::Column::ServiceId.eq(service_id))
            .one(self.db.as_ref())
            .await?
            .map(DataImportRun::from)
            .ok_or(DataImportError::RunNotFound { service_id, run_id })
    }

    /// The bare run row, scoped to its service.
    async fn find_run(
        &self,
        service_id: i32,
        run_id: i32,
    ) -> Result<service_data_imports::Model, DataImportError> {
        service_data_imports::Entity::find_by_id(run_id)
            .filter(service_data_imports::Column::ServiceId.eq(service_id))
            .one(self.db.as_ref())
            .await?
            .ok_or(DataImportError::RunNotFound { service_id, run_id })
    }

    /// Ask a running import to stop. The request is recorded first, then the
    /// helper is removed; the background task sees the removal, finds the
    /// request and records the run as cancelled. Returns the run as it is
    /// right after the request (still `running` until the task settles it).
    pub async fn cancel(
        &self,
        service_id: i32,
        run_id: i32,
    ) -> Result<DataImportRun, DataImportError> {
        let run = self.find_run(service_id, run_id).await?;
        if run.status != STATUS_RUNNING {
            return Err(DataImportError::NotCancellable {
                run_id,
                status: run.status,
                reason: "it has already finished".to_string(),
            });
        }
        // Once the copy is done, stopping the run would only hide a
        // complete import: let it record its result.
        if run.phase == PHASE_VERIFYING || run.phase == PHASE_FINISHED {
            return Err(DataImportError::NotCancellable {
                run_id,
                status: run.status,
                reason: "the data has already been copied; the run is recording its result"
                    .to_string(),
            });
        }
        let now = Utc::now();
        let updated = service_data_imports::Entity::update_many()
            .col_expr(
                service_data_imports::Column::CancelRequestedAt,
                Expr::value(now),
            )
            .col_expr(service_data_imports::Column::UpdatedAt, Expr::value(now))
            .filter(service_data_imports::Column::Id.eq(run_id))
            .filter(service_data_imports::Column::Status.eq(STATUS_RUNNING))
            .exec(self.db.as_ref())
            .await?;
        if updated.rows_affected == 0 {
            // Settled between the read and the update.
            let run = self.find_run(service_id, run_id).await?;
            return Err(DataImportError::NotCancellable {
                run_id,
                status: run.status,
                reason: "it finished while the cancellation was being recorded".to_string(),
            });
        }
        if let (Some(container), Some(docker)) = (&run.helper_container, self.docker.get()) {
            runner::stop_helper(docker, container).await;
        }
        info!(run_id, service_id, "Cancellation requested for data import");
        self.get_run(service_id, run_id).await
    }

    /// Settle runs that a previous process left `running`: stop any helper
    /// still alive, then mark the run `interrupted` with an explanation.
    ///
    /// `run_ids` must be snapshotted (with [`active_import_run_ids`]) before
    /// this process starts any import, so a run of this process can never be
    /// mistaken for an orphan. A run whose helper cannot be confirmed stopped
    /// (Docker briefly unreachable) or whose row cannot be updated keeps its
    /// lock and is retried with backoff until it settles: releasing it early
    /// would let a second import race a surviving helper, and giving up would
    /// leave the database locked until the next restart.
    pub async fn reconcile_interrupted(&self, run_ids: &[i32]) -> usize {
        self.reconcile_with_backoff(run_ids, RECONCILE_RETRY_BASE, RECONCILE_RETRY_MAX)
            .await
    }

    async fn reconcile_with_backoff(
        &self,
        run_ids: &[i32],
        base_delay: Duration,
        max_delay: Duration,
    ) -> usize {
        let mut pending = run_ids.to_vec();
        let mut settled = 0;
        let mut delay = base_delay;
        loop {
            let mut retry = Vec::new();
            for run_id in pending {
                match self.reconcile_one(run_id).await {
                    Reconciled::Settled => settled += 1,
                    Reconciled::AlreadySettled => {}
                    Reconciled::Retry => retry.push(run_id),
                }
            }
            if retry.is_empty() {
                return settled;
            }
            warn!(
                run_ids = ?retry,
                retry_in_secs = delay.as_secs_f64(),
                "Interrupted data imports not settled yet; keeping them locked and retrying"
            );
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(max_delay);
            pending = retry;
        }
    }

    async fn reconcile_one(&self, run_id: i32) -> Reconciled {
        let run = match service_data_imports::Entity::find_by_id(run_id)
            .one(self.db.as_ref())
            .await
        {
            Ok(Some(run)) if run.status == STATUS_RUNNING => run,
            Ok(_) => return Reconciled::AlreadySettled,
            Err(e) => {
                error!(run_id, error = %e, "Could not load interrupted data import");
                return Reconciled::Retry;
            }
        };
        if let Some(docker) = self.docker.get() {
            if let Err(e) = runner::fence_run_helpers(docker, run.service_id, run.id).await {
                error!(
                    run_id,
                    service_id = run.service_id,
                    error = %e,
                    "Could not stop the helper of an interrupted data import; keeping it running"
                );
                return Reconciled::Retry;
            }
        }
        let message = interrupted_message(&run.phase, run.atomic_transfer);
        match mark_terminal(
            self.db.as_ref(),
            run.id,
            STATUS_INTERRUPTED,
            Some(message),
            None,
            None,
        )
        .await
        {
            Ok(true) => {
                warn!(
                    run_id,
                    service_id = run.service_id,
                    phase = %run.phase,
                    "Marked data import interrupted by a server restart"
                );
                Reconciled::Settled
            }
            Ok(false) => Reconciled::AlreadySettled,
            Err(e) => {
                error!(run_id, error = %e, "Could not mark data import interrupted");
                Reconciled::Retry
            }
        }
    }

    async fn load_service(
        &self,
        service_id: i32,
    ) -> Result<external_services::Model, DataImportError> {
        self.manager
            .get_service(service_id)
            .await
            .map_err(|e| map_manager_error(service_id, "load the service", e))
    }

    async fn service_config(&self, service_id: i32) -> Result<ServiceConfig, DataImportError> {
        self.manager
            .get_service_config(service_id)
            .await
            .map_err(|e| map_manager_error(service_id, "read the service configuration", e))
    }

    /// Engine-independent checks, then the engine instance.
    async fn resolve(
        &self,
        service: external_services::Model,
    ) -> Result<ResolvedTarget, DataImportError> {
        let service_id = service.id;
        if service.topology != "standalone" {
            return Err(DataImportError::Unsupported {
                service_id,
                service_type: service.service_type.clone(),
                reason: format!(
                    "it is a {} service; data can only be imported into standalone services",
                    service.topology
                ),
            });
        }
        if let Some(node_id) = service.node_id {
            return Err(DataImportError::Unsupported {
                service_id,
                service_type: service.service_type.clone(),
                reason: format!(
                    "it runs on worker node {node_id}; data can only be imported into services \
                     running on the control-plane host"
                ),
            });
        }
        if !self.manager.local_workloads_enabled() {
            return Err(DataImportError::NotReady {
                service_id,
                reason: "this Temps process does not run managed-service containers".to_string(),
            });
        }
        let service_type = ServiceType::from_str(&service.service_type).map_err(|_| {
            DataImportError::Unsupported {
                service_id,
                service_type: service.service_type.clone(),
                reason: "the service type is not recognised".to_string(),
            }
        })?;
        let instance = self
            .manager
            .get_service_instance(service.name.clone(), service_type)
            .map_err(|e| map_manager_error(service_id, "load the service engine", e))?;
        Ok(ResolvedTarget { service, instance })
    }

    async fn find_running(
        &self,
        service_id: i32,
        database: &str,
    ) -> Result<Option<service_data_imports::Model>, DataImportError> {
        Ok(service_data_imports::Entity::find()
            .filter(service_data_imports::Column::ServiceId.eq(service_id))
            .filter(service_data_imports::Column::TargetDatabase.eq(database))
            .filter(service_data_imports::Column::Status.eq(STATUS_RUNNING))
            .one(self.db.as_ref())
            .await?)
    }
}

/// Refuse when a restore may be writing into `service_id`: one restoring in
/// place (or a PITR in place) onto it, or one whose new service it is.
async fn ensure_no_active_restore<C: ConnectionTrait>(
    conn: &C,
    service_id: i32,
) -> Result<(), DataImportError> {
    let writes_here = Condition::any()
        .add(
            Condition::all()
                .add(restore_runs::Column::SourceServiceId.eq(service_id))
                .add(restore_runs::Column::TargetServiceId.is_null())
                .add(restore_runs::Column::TargetServiceName.is_null()),
        )
        .add(restore_runs::Column::TargetServiceId.eq(service_id));
    let active = restore_runs::Entity::find()
        .filter(restore_runs::Column::Status.is_in(ACTIVE_RESTORE_STATUSES))
        .filter(writes_here)
        .one(conn)
        .await?;
    match active {
        Some(restore) => Err(DataImportError::RestoreInProgress {
            service_id,
            restore_run_id: restore.id,
        }),
        None => Ok(()),
    }
}

/// Record a new run under the service's row lock — the lock destructive
/// restores take before checking for imports — after checking for a restore
/// inside it, so an import and a restore can never both start on the same
/// service. The partial unique index still settles two imports into the
/// same database.
async fn insert_run_locked(
    db: &DatabaseConnection,
    service_id: i32,
    target_database: &str,
    run: service_data_imports::ActiveModel,
) -> Result<service_data_imports::Model, DataImportError> {
    let transaction = db.begin().await?;
    let inserted = async {
        external_services::Entity::find_by_id(service_id)
            .lock_exclusive()
            .one(&transaction)
            .await?
            .ok_or(DataImportError::ServiceNotFound { service_id })?;
        ensure_no_active_restore(&transaction, service_id).await?;
        run.insert(&transaction)
            .await
            .map_err(|e| match e.sql_err() {
                Some(sea_orm::SqlErr::UniqueConstraintViolation(_)) => {
                    DataImportError::AlreadyRunning {
                        service_id,
                        database: target_database.to_string(),
                        run_id: 0,
                    }
                }
                _ => DataImportError::Database(e),
            })
    }
    .await;
    match inserted {
        Ok(model) => {
            transaction.commit().await?;
            Ok(model)
        }
        Err(error) => {
            // Roll back now rather than on drop, so the row lock is released
            // immediately instead of when the pool next uses the connection.
            if let Err(rollback_error) = transaction.rollback().await {
                warn!(
                    service_id,
                    error = %rollback_error,
                    "Rolling back a refused data import failed"
                );
            }
            Err(error)
        }
    }
}

/// Ids of the runs still marked `running`. Call at startup, before any
/// import can be started, and pass the result to
/// [`DataImportService::reconcile_interrupted`].
pub async fn active_import_run_ids(db: &DatabaseConnection) -> Result<Vec<i32>, DataImportError> {
    Ok(service_data_imports::Entity::find()
        .filter(service_data_imports::Column::Status.eq(STATUS_RUNNING))
        .order_by_asc(service_data_imports::Column::Id)
        .all(db)
        .await?
        .into_iter()
        .map(|run| run.id)
        .collect())
}

fn validate_timeout(timeout_minutes: Option<u32>) -> Result<Duration, DataImportError> {
    let minutes = timeout_minutes.unwrap_or(DEFAULT_TIMEOUT_MINUTES);
    if minutes == 0 || minutes > MAX_TIMEOUT_MINUTES {
        return Err(DataImportError::Validation {
            message: format!(
                "timeout_minutes must be between 1 and {MAX_TIMEOUT_MINUTES}, got {minutes}"
            ),
        });
    }
    Ok(Duration::from_secs(u64::from(minutes) * 60))
}

fn ensure_running(service: &external_services::Model) -> Result<(), DataImportError> {
    if service.status == "running" {
        Ok(())
    } else {
        Err(DataImportError::NotReady {
            service_id: service.id,
            reason: format!(
                "the service is {}; start it before importing data",
                service.status
            ),
        })
    }
}

/// The part of an availability error worth showing as a reason.
fn reason_of(error: &DataImportError) -> String {
    match error {
        DataImportError::Unsupported { reason, .. } | DataImportError::NotReady { reason, .. } => {
            reason.clone()
        }
        DataImportError::DockerUnavailable(e) => {
            format!("this Temps process has no Docker daemon ({})", e.reason)
        }
        other => other.to_string(),
    }
}

fn map_manager_error(
    service_id: i32,
    operation: &str,
    error: ExternalServiceError,
) -> DataImportError {
    match error {
        ExternalServiceError::ServiceNotFound { .. } => {
            DataImportError::ServiceNotFound { service_id }
        }
        ExternalServiceError::DockerUnavailable(e) => DataImportError::DockerUnavailable(e),
        other => DataImportError::target(format!("#{service_id}"), operation, other.to_string()),
    }
}

/// Message for a run whose process died, by the phase it was in.
fn interrupted_message(phase: &str, atomic: bool) -> String {
    let consequence = match phase {
        PHASE_PREPARING_TARGET => {
            "No data had been copied yet; the target database may have been created or emptied."
        }
        PHASE_TRANSFERRING if atomic => {
            "The copy is applied in a single transaction, so none of it was kept."
        }
        PHASE_TRANSFERRING => "The target database may contain part of the data.",
        _ => "The data had been copied; only the final measurement is missing.",
    };
    format!(
        "Interrupted: the Temps server restarted while this import was {}. {} Run the import \
         again — with replace enabled if the database is not empty.",
        phase.replace('_', " "),
        consequence
    )
}

/// How a background job ended.
#[derive(Debug, Clone, PartialEq, Eq)]
struct JobOutcome {
    status: &'static str,
    error_message: Option<String>,
    /// What the transfer printed, already scrubbed.
    output: Option<String>,
    object_count: Option<i64>,
    size_bytes: Option<i64>,
}

impl JobOutcome {
    fn succeeded(object_count: Option<i64>, size_bytes: Option<i64>) -> Self {
        Self {
            status: STATUS_SUCCEEDED,
            error_message: None,
            output: None,
            object_count,
            size_bytes,
        }
    }

    fn failed(message: String) -> Self {
        Self {
            status: STATUS_FAILED,
            error_message: Some(message),
            output: None,
            object_count: None,
            size_bytes: None,
        }
    }

    fn cancelled() -> Self {
        Self {
            status: STATUS_CANCELLED,
            error_message: Some("Cancelled on request.".to_string()),
            output: None,
            object_count: None,
            size_bytes: None,
        }
    }

    fn with_output(mut self, output: Option<&str>) -> Self {
        self.output = output.map(str::to_string);
        self
    }
}

/// The background half of a run. Owns everything it needs.
struct ImportJob {
    run_id: i32,
    service_id: i32,
    database: String,
    replace: bool,
    timeout: Duration,
    config: ServiceConfig,
    source: ImportSource,
    pins: Vec<PinnedHost>,
    network: String,
    target_host: String,
    target_port: String,
    atomic: bool,
    instance: Box<dyn ExternalService>,
    db: Arc<DatabaseConnection>,
    docker: Arc<bollard::Docker>,
}

impl ImportJob {
    async fn execute(self) -> JobOutcome {
        let Some(engine) = self.instance.data_import() else {
            return JobOutcome::failed("the service engine no longer supports imports".to_string());
        };
        let mut secrets = self.source.secrets();

        // `start` inspected the target before the run held the database
        // lock: since then a deployment may have created tables, or another
        // import may have created and filled a database that was missing.
        // Plan again from what is there now, whatever was found then.
        let preparation = match current_preparation(
            engine,
            &self.config,
            self.service_id,
            &self.database,
            self.replace,
        )
        .await
        {
            Ok(preparation) => preparation,
            Err(e) => return JobOutcome::failed(scrub_secrets(&e.to_string(), &secrets)),
        };

        if self.cancel_requested().await {
            return JobOutcome::cancelled();
        }
        if let Err(e) = engine
            .prepare_target(&self.config, &self.database, preparation)
            .await
        {
            return JobOutcome::failed(scrub_secrets(&e.to_string(), &secrets));
        }

        let plan = match engine
            .transfer_plan(
                &self.config,
                &self.source,
                &TransferTarget {
                    host: &self.target_host,
                    port: &self.target_port,
                    database: &self.database,
                },
            )
            .await
        {
            Ok(plan) => plan,
            Err(e) => return JobOutcome::failed(scrub_secrets(&e.to_string(), &secrets)),
        };
        secrets.extend(
            plan.env
                .iter()
                .filter(|e| e.secret)
                .map(|e| e.value.clone()),
        );

        let container_name = format!(
            "temps-data-import-{}-{}",
            self.run_id,
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        if let Err(e) = self.enter_transfer(&container_name).await {
            return JobOutcome::failed(format!("recording the transfer phase failed: {e}"));
        }
        // A cancel that arrived before the container name was recorded could
        // not stop it; honour it now.
        if self.cancel_requested().await {
            return JobOutcome::cancelled();
        }

        let request = HelperRequest {
            run_id: self.run_id,
            service_id: self.service_id,
            container_name: &container_name,
            plan: &plan,
            network: &self.network,
            extra_hosts: self.pins.iter().map(PinnedHost::extra_host_entry).collect(),
            timeout: self.timeout,
            secrets: &secrets,
        };
        // Created, not started: a cancellation that arrived while the image
        // was being pulled is honoured before anything runs.
        let container_id = match runner::create_helper(&self.docker, &request).await {
            Ok(id) => id,
            Err(e) => {
                if self.cancel_requested().await {
                    return JobOutcome::cancelled();
                }
                return JobOutcome::failed(scrub_secrets(&e.to_string(), &secrets));
            }
        };
        if self.cancel_requested().await {
            self.settle_helper(&container_id).await;
            return JobOutcome::cancelled();
        }
        let started = runner::start_and_wait(&self.docker, &container_id, &request).await;
        // The run — and the database lock it holds — is only released once
        // the helper can no longer write.
        self.settle_helper(&container_id).await;
        let outcome = match started {
            Ok(outcome) => outcome,
            Err(e) => {
                // A cancel that removed the container before it started
                // makes the start fail: that is a cancellation, not an error.
                if self.cancel_requested().await {
                    return JobOutcome::cancelled();
                }
                return JobOutcome::failed(scrub_secrets(&e.to_string(), &secrets));
            }
        };

        let output = outcome.output().map(str::to_string);
        if !matches!(outcome, HelperOutcome::Succeeded { .. }) {
            if self.cancel_requested().await {
                return JobOutcome::cancelled().with_output(output.as_deref());
            }
            return JobOutcome::failed(self.describe_failure(engine, &outcome))
                .with_output(output.as_deref());
        }

        if let Err(e) = self.set_phase(PHASE_VERIFYING).await {
            warn!(run_id = self.run_id, error = %e, "Could not record the verifying phase");
        }
        // The copy succeeded; a failed measurement must not turn it into a
        // failure.
        let measured = match engine.inspect_target(&self.config, &self.database).await {
            Ok(inspection) => {
                JobOutcome::succeeded(Some(inspection.object_count), inspection.size_bytes)
            }
            Err(e) => {
                warn!(
                    run_id = self.run_id,
                    error = %scrub_secrets(&e.to_string(), &secrets),
                    "Data import finished but the target could not be measured"
                );
                JobOutcome::succeeded(None, None)
            }
        };
        measured.with_output(output.as_deref())
    }

    /// One or two sentences: what failed, the likely cause when the engine
    /// recognises it, and what the target was left with. The tool output
    /// itself is recorded separately (`helper_output`).
    fn describe_failure(&self, engine: &dyn DataImportEngine, outcome: &HelperOutcome) -> String {
        let leftover = if self.atomic {
            "Nothing was committed to the target database."
        } else {
            "The target database may contain part of the data; run the import again with \
             replace enabled."
        };
        let cause = outcome
            .output()
            .and_then(|output| engine.failure_hint(output))
            .unwrap_or_else(|| "see the transfer output for details".to_string());
        let what = match outcome {
            HelperOutcome::Succeeded { .. } => return String::new(),
            HelperOutcome::SourceFailed { .. } => "Reading the source database failed".to_string(),
            HelperOutcome::TargetFailed { .. } => {
                format!("Writing into database '{}' failed", self.database)
            }
            HelperOutcome::Exited { exit_code, .. } => {
                format!("The import helper exited with status {exit_code}")
            }
            HelperOutcome::TimedOut { .. } => format!(
                "The transfer did not finish within {} minutes and was stopped",
                self.timeout.as_secs() / 60
            ),
            HelperOutcome::Vanished { reason } => {
                return format!(
                    "The import helper container disappeared while running ({reason}). {leftover}"
                )
            }
        };
        format!("{what}: {cause}. {leftover}")
    }

    /// Wait until the helper is confirmed stopped, retrying with backoff for
    /// as long as it takes. Until then the run stays `running`, so the
    /// partial unique index keeps a second import out of the database a
    /// surviving helper might still be writing into.
    async fn settle_helper(&self, container_id: &str) {
        let mut delay = Duration::from_secs(2);
        loop {
            match runner::ensure_helper_stopped(&self.docker, self.service_id, container_id).await {
                Ok(()) => return,
                Err(e) => {
                    warn!(
                        run_id = self.run_id,
                        service_id = self.service_id,
                        error = %e,
                        retry_in_secs = delay.as_secs(),
                        "Data import helper not confirmed stopped; keeping the run active"
                    );
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(60));
                }
            }
        }
    }

    async fn cancel_requested(&self) -> bool {
        match service_data_imports::Entity::find_by_id(self.run_id)
            .one(self.db.as_ref())
            .await
        {
            Ok(Some(run)) => run.cancel_requested_at.is_some(),
            Ok(None) => false,
            Err(e) => {
                warn!(run_id = self.run_id, error = %e, "Could not read cancellation state");
                false
            }
        }
    }

    async fn enter_transfer(&self, container_name: &str) -> Result<(), sea_orm::DbErr> {
        service_data_imports::Entity::update_many()
            .col_expr(
                service_data_imports::Column::Phase,
                Expr::value(PHASE_TRANSFERRING),
            )
            .col_expr(
                service_data_imports::Column::HelperContainer,
                Expr::value(container_name),
            )
            .col_expr(
                service_data_imports::Column::UpdatedAt,
                Expr::value(Utc::now()),
            )
            .filter(service_data_imports::Column::Id.eq(self.run_id))
            .filter(service_data_imports::Column::Status.eq(STATUS_RUNNING))
            .exec(self.db.as_ref())
            .await
            .map(|_| ())
    }

    async fn set_phase(&self, phase: &str) -> Result<(), sea_orm::DbErr> {
        service_data_imports::Entity::update_many()
            .col_expr(service_data_imports::Column::Phase, Expr::value(phase))
            .col_expr(
                service_data_imports::Column::UpdatedAt,
                Expr::value(Utc::now()),
            )
            .filter(service_data_imports::Column::Id.eq(self.run_id))
            .filter(service_data_imports::Column::Status.eq(STATUS_RUNNING))
            .exec(self.db.as_ref())
            .await
            .map(|_| ())
    }
}

/// Inspect the target and decide how to prepare it, refusing a target that
/// is not empty unless `replace` was requested. Called once the run holds the
/// database lock, so the decision reflects every import that finished before.
async fn current_preparation(
    engine: &dyn DataImportEngine,
    config: &ServiceConfig,
    service_id: i32,
    database: &str,
    replace: bool,
) -> Result<TargetPreparation, DataImportError> {
    let inspection = engine.inspect_target(config, database).await?;
    plan_target_preparation(
        service_id,
        database,
        inspection,
        replace,
        &engine.import_spec().object_noun,
    )
}

/// Write a terminal status, only if the run is still `running`. Returns
/// whether this call settled it.
async fn mark_terminal(
    db: &DatabaseConnection,
    run_id: i32,
    status: &str,
    error_message: Option<String>,
    output: Option<String>,
    measured: Option<(Option<i64>, Option<i64>)>,
) -> Result<bool, sea_orm::DbErr> {
    let now = Utc::now();
    let (object_count, size_bytes) = measured.unwrap_or((None, None));
    let mut update = service_data_imports::Entity::update_many()
        .col_expr(service_data_imports::Column::Status, Expr::value(status));
    // A run that did not succeed keeps the phase it stopped in, so the
    // console can show where it failed, was cancelled or was interrupted.
    if status == STATUS_SUCCEEDED {
        update = update.col_expr(
            service_data_imports::Column::Phase,
            Expr::value(PHASE_FINISHED),
        );
    }
    let result = update
        .col_expr(
            service_data_imports::Column::ErrorMessage,
            Expr::value(error_message),
        )
        .col_expr(
            service_data_imports::Column::HelperOutput,
            Expr::value(output),
        )
        .col_expr(
            service_data_imports::Column::TargetObjectCount,
            Expr::value(object_count),
        )
        .col_expr(
            service_data_imports::Column::TargetSizeBytes,
            Expr::value(size_bytes),
        )
        .col_expr(service_data_imports::Column::FinishedAt, Expr::value(now))
        .col_expr(service_data_imports::Column::UpdatedAt, Expr::value(now))
        .filter(service_data_imports::Column::Id.eq(run_id))
        .filter(service_data_imports::Column::Status.eq(STATUS_RUNNING))
        .exec(db)
        .await?;
    Ok(result.rows_affected > 0)
}

async fn finalize_run(db: &DatabaseConnection, run_id: i32, outcome: JobOutcome) {
    let measured = Some((outcome.object_count, outcome.size_bytes));
    match mark_terminal(
        db,
        run_id,
        outcome.status,
        outcome.error_message.clone(),
        outcome.output.clone(),
        measured,
    )
    .await
    {
        Ok(true) if outcome.status == STATUS_SUCCEEDED => info!(
            run_id,
            object_count = ?outcome.object_count,
            size_bytes = ?outcome.size_bytes,
            "Data import succeeded"
        ),
        Ok(true) => warn!(
            run_id,
            status = outcome.status,
            error = outcome.error_message.as_deref().unwrap_or(""),
            "Data import did not succeed"
        ),
        Ok(false) => warn!(
            run_id,
            status = outcome.status,
            "Data import was already settled; outcome not recorded"
        ),
        Err(e) => error!(
            run_id,
            status = outcome.status,
            error = %e,
            "Failed to record the outcome of a data import"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_import::{DataImportSpec, TargetInspection, TransferPlan, TransferTarget};
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    fn run(id: i32, service_id: i32, status: &str) -> service_data_imports::Model {
        let now = Utc::now();
        service_data_imports::Model {
            id,
            service_id,
            service_type: "postgres".to_string(),
            target_database: "shop_production".to_string(),
            source_display: "postgres://***:***@db.example.com:5432/shop".to_string(),
            source_database: "shop".to_string(),
            replace_existing: false,
            atomic_transfer: true,
            status: status.to_string(),
            phase: PHASE_TRANSFERRING.to_string(),
            helper_container: None,
            error_message: None,
            helper_output: None,
            target_object_count: None,
            target_size_bytes: None,
            timeout_seconds: 3600,
            created_by: Some(1),
            cancel_requested_at: None,
            started_at: now,
            finished_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn service_with(db: DatabaseConnection) -> DataImportService {
        let db = Arc::new(db);
        let encryption = Arc::new(
            temps_core::EncryptionService::new("test_encryption_key_1234567890ab")
                .expect("encryption key"),
        );
        let docker = Arc::new(DockerHandle::disabled("control-plane", "unit test"));
        let manager = Arc::new(ExternalServiceManager::new_with_handle(
            db.clone(),
            encryption,
            docker.clone(),
            false,
            Arc::new(temps_dns::DnsRegistry::new(db.clone())),
        ));
        DataImportService::new(db, manager, docker)
    }

    #[test]
    fn timeout_defaults_and_bounds() {
        assert_eq!(
            validate_timeout(None).expect("default"),
            Duration::from_secs(u64::from(DEFAULT_TIMEOUT_MINUTES) * 60)
        );
        assert_eq!(
            validate_timeout(Some(MAX_TIMEOUT_MINUTES)).expect("max"),
            Duration::from_secs(u64::from(MAX_TIMEOUT_MINUTES) * 60)
        );
        for bad in [0, MAX_TIMEOUT_MINUTES + 1] {
            assert!(matches!(
                validate_timeout(Some(bad)),
                Err(DataImportError::Validation { .. })
            ));
        }
    }

    #[test]
    fn interrupted_message_states_what_may_be_left_by_phase() {
        let preparing = interrupted_message(PHASE_PREPARING_TARGET, false);
        assert!(preparing.contains("preparing target"), "{preparing}");
        assert!(preparing.contains("No data had been copied"), "{preparing}");

        let atomic = interrupted_message(PHASE_TRANSFERRING, true);
        assert!(atomic.contains("none of it was kept"), "{atomic}");

        let partial = interrupted_message(PHASE_TRANSFERRING, false);
        assert!(partial.contains("part of the data"), "{partial}");
        assert!(partial.contains("replace enabled"), "{partial}");
    }

    #[test]
    fn availability_reasons_are_the_underlying_explanation() {
        let reason = reason_of(&DataImportError::NotReady {
            service_id: 3,
            reason: "the service is stopped; start it before importing data".to_string(),
        });
        assert_eq!(
            reason,
            "the service is stopped; start it before importing data"
        );
    }

    #[test]
    fn a_stopped_service_is_not_ready() {
        let mut service = external_services::Model {
            id: 4,
            name: "orders".to_string(),
            service_type: "postgres".to_string(),
            version: None,
            status: "stopped".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            slug: None,
            config: None,
            node_id: None,
            topology: "standalone".to_string(),
            error_message: None,
            health_status: None,
            last_health_check_at: None,
            last_health_error: None,
            consecutive_health_failures: 0,
            health_metadata: None,
            metrics_enabled: false,
            default_backup_provisioned: false,
            container_name: None,
            ai_data_access: false,
            created_by_user_id: None,
            continuous_archive_s3_source_id: None,
            continuous_archive_pinned_at: None,
        };
        assert!(matches!(
            ensure_running(&service),
            Err(DataImportError::NotReady { service_id: 4, .. })
        ));
        service.status = "running".to_string();
        assert!(ensure_running(&service).is_ok());
    }

    fn service_model(id: i32) -> external_services::Model {
        external_services::Model {
            id,
            name: "orders".to_string(),
            service_type: "postgres".to_string(),
            version: None,
            status: "running".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            slug: None,
            config: None,
            node_id: None,
            topology: "standalone".to_string(),
            error_message: None,
            health_status: None,
            last_health_check_at: None,
            last_health_error: None,
            consecutive_health_failures: 0,
            health_metadata: None,
            metrics_enabled: false,
            default_backup_provisioned: false,
            container_name: None,
            ai_data_access: false,
            created_by_user_id: None,
            continuous_archive_s3_source_id: None,
            continuous_archive_pinned_at: None,
        }
    }

    fn restore_model(id: i32, service_id: i32) -> restore_runs::Model {
        let now = Utc::now();
        restore_runs::Model {
            id,
            source_backup_id: 1,
            source_service_id: service_id,
            target_service_id: None,
            target_service_name: None,
            mode: "in_place".to_string(),
            status: "running".to_string(),
            phase: "restore".to_string(),
            recovery_target: None,
            parameter_overrides: serde_json::json!({}),
            resume_token: None,
            log_id: "log".to_string(),
            error_message: None,
            attempt: 1,
            started_at: Some(now),
            finished_at: None,
            created_by: 1,
            created_at: now,
            updated_at: now,
            cancel_requested_at: None,
            cancel_requested_by: None,
        }
    }

    fn new_run(service_id: i32) -> service_data_imports::ActiveModel {
        service_data_imports::ActiveModel {
            service_id: Set(service_id),
            service_type: Set("postgres".to_string()),
            target_database: Set("shop_production".to_string()),
            source_display: Set("postgres://***:***@db.example.com:5432/shop".to_string()),
            source_database: Set("shop".to_string()),
            status: Set(STATUS_RUNNING.to_string()),
            phase: Set(PHASE_PREPARING_TARGET.to_string()),
            timeout_seconds: Set(3600),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_restore_writing_into_the_service_blocks_the_import_under_the_lock() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // SELECT ... FOR UPDATE on the service row
            .append_query_results([vec![service_model(4)]])
            // the restore check, made while that lock is held
            .append_query_results([vec![restore_model(9, 4)]])
            .into_connection();
        let error = insert_run_locked(&db, 4, "shop_production", new_run(4))
            .await
            .expect_err("must refuse");
        assert!(matches!(
            error,
            DataImportError::RestoreInProgress {
                service_id: 4,
                restore_run_id: 9
            }
        ));
        let log = db.into_transaction_log();
        let statements = format!("{log:?}");
        assert!(statements.contains("FOR UPDATE"), "{statements}");
        assert!(
            !statements.contains("INSERT"),
            "nothing recorded: {statements}"
        );
    }

    #[tokio::test]
    async fn the_run_is_recorded_under_the_service_lock() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![service_model(4)]])
            .append_query_results([Vec::<restore_runs::Model>::new()])
            .append_query_results([vec![run(11, 4, STATUS_RUNNING)]])
            .into_connection();
        let recorded = insert_run_locked(&db, 4, "shop_production", new_run(4))
            .await
            .expect("recorded");
        assert_eq!(recorded.id, 11);
        let statements = format!("{:?}", db.into_transaction_log());
        let lock = statements.find("FOR UPDATE").expect("locked");
        let insert = statements.find("INSERT").expect("inserted");
        assert!(lock < insert, "the lock is taken before the insert");
    }

    #[tokio::test]
    async fn get_run_returns_the_run_of_the_service() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![(
                run(5, 2, STATUS_RUNNING),
                Option::<users::Model>::None,
            )]])
            .into_connection();
        let found = service_with(db).get_run(2, 5).await.expect("found");
        assert_eq!(found.run.id, 5);
        assert!(found.started_by.is_none());
    }

    #[tokio::test]
    async fn get_run_of_another_service_is_not_found() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                Vec::<(service_data_imports::Model, Option<users::Model>)>::new(),
            ])
            .into_connection();
        let error = service_with(db).get_run(2, 5).await.expect_err("not found");
        assert!(matches!(
            error,
            DataImportError::RunNotFound {
                service_id: 2,
                run_id: 5
            }
        ));
    }

    #[tokio::test]
    async fn only_running_imports_can_be_cancelled() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![run(5, 2, STATUS_SUCCEEDED)]])
            .into_connection();
        let error = service_with(db).cancel(2, 5).await.expect_err("settled");
        assert!(matches!(
            error,
            DataImportError::NotCancellable { run_id: 5, ref status, .. } if status == STATUS_SUCCEEDED
        ));
    }

    #[tokio::test]
    async fn a_run_whose_copy_finished_cannot_be_cancelled() {
        let mut verifying = run(5, 2, STATUS_RUNNING);
        verifying.phase = PHASE_VERIFYING.to_string();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![verifying]])
            .into_connection();
        let error = service_with(db).cancel(2, 5).await.expect_err("too late");
        assert!(matches!(
            error,
            DataImportError::NotCancellable { run_id: 5, .. }
        ));
        assert!(error.to_string().contains("already been copied"), "{error}");
    }

    #[tokio::test]
    async fn cancelling_records_the_request_and_returns_the_run() {
        let mut requested = run(5, 2, STATUS_RUNNING);
        requested.cancel_requested_at = Some(Utc::now());
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![run(5, 2, STATUS_RUNNING)]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![(requested, Option::<users::Model>::None)]])
            .into_connection();
        let cancelled = service_with(db).cancel(2, 5).await.expect("cancel");
        assert!(cancelled.run.cancel_requested_at.is_some());
    }

    #[tokio::test]
    async fn cancel_losing_a_race_with_completion_reports_the_final_status() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![run(5, 2, STATUS_RUNNING)]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .append_query_results([vec![run(5, 2, STATUS_FAILED)]])
            .into_connection();
        let error = service_with(db).cancel(2, 5).await.expect_err("settled");
        assert!(matches!(
            error,
            DataImportError::NotCancellable { ref status, .. } if status == STATUS_FAILED
        ));
    }

    #[tokio::test]
    async fn reconcile_marks_orphans_interrupted_and_skips_settled_runs() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![run(5, 2, STATUS_RUNNING)]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![run(6, 2, STATUS_SUCCEEDED)]])
            .into_connection();
        // Docker is disabled in this process: no helper can have been
        // started by it, so the run is settled without fencing.
        let settled = service_with(db).reconcile_interrupted(&[5, 6]).await;
        assert_eq!(settled, 1);
    }

    #[tokio::test]
    async fn reconcile_keeps_retrying_a_run_it_could_not_settle() {
        // The first attempt fails (the row cannot be read); the run is not
        // given up on but retried, and settled once the database answers.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_errors([sea_orm::DbErr::Custom("connection reset".to_string())])
            .append_query_results([vec![run(5, 2, STATUS_RUNNING)]])
            .append_exec_errors([sea_orm::DbErr::Custom("connection reset".to_string())])
            .append_query_results([vec![run(5, 2, STATUS_RUNNING)]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let settled = service_with(db)
            .reconcile_with_backoff(&[5], Duration::from_millis(1), Duration::from_millis(2))
            .await;
        assert_eq!(settled, 1);
    }

    /// Engine whose target is whatever the test says it is now.
    struct TargetIs(TargetInspection);

    #[async_trait::async_trait]
    impl DataImportEngine for TargetIs {
        fn import_spec(&self) -> DataImportSpec {
            DataImportSpec {
                engine_label: "Redis".to_string(),
                source_schemes: vec!["redis".to_string()],
                source_url_example: "redis://db.example.com:6379/0".to_string(),
                allowed_source_options: vec![],
                atomic: false,
                object_noun: "key".to_string(),
                max_target_length: 63,
            }
        }
        fn parse_source(&self, _raw: &str) -> Result<ImportSource, DataImportError> {
            Err(DataImportError::Validation {
                message: "not used".to_string(),
            })
        }
        fn validate_target_database(&self, _database: &str) -> Result<(), DataImportError> {
            Ok(())
        }
        async fn inspect_target(
            &self,
            _config: &ServiceConfig,
            _database: &str,
        ) -> Result<TargetInspection, DataImportError> {
            Ok(self.0)
        }
        async fn prepare_target(
            &self,
            _config: &ServiceConfig,
            _database: &str,
            _preparation: TargetPreparation,
        ) -> Result<(), DataImportError> {
            Ok(())
        }
        fn target_container(&self, _config: &ServiceConfig) -> Result<String, DataImportError> {
            Ok("target".to_string())
        }
        async fn transfer_plan(
            &self,
            _config: &ServiceConfig,
            _source: &ImportSource,
            _target: &TransferTarget<'_>,
        ) -> Result<TransferPlan, DataImportError> {
            Err(DataImportError::Validation {
                message: "not used".to_string(),
            })
        }
    }

    fn cache_config() -> ServiceConfig {
        ServiceConfig {
            name: "cache".to_string(),
            service_type: ServiceType::Redis,
            version: None,
            parameters: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn a_target_filled_since_start_is_refused_without_replace() {
        // `start` found "sessions" missing; another import created and filled
        // it before this run took the lock. Planning again must refuse it
        // rather than keep the stale "create" decision and write into it.
        let engine = TargetIs(TargetInspection {
            exists: true,
            object_count: 1200,
            size_bytes: None,
        });
        let error = current_preparation(&engine, &cache_config(), 3, "sessions", false)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            DataImportError::TargetNotEmpty { object_count: 1200, ref database, .. }
                if database == "sessions"
        ));
    }

    #[tokio::test]
    async fn the_plan_follows_the_target_as_it_is_now() {
        let missing = TargetIs(TargetInspection {
            exists: false,
            object_count: 0,
            size_bytes: None,
        });
        let empty = TargetIs(TargetInspection {
            exists: true,
            object_count: 0,
            size_bytes: Some(0),
        });
        let filled = TargetIs(TargetInspection {
            exists: true,
            object_count: 4,
            size_bytes: None,
        });
        let config = cache_config();
        let plan = |engine, replace| current_preparation(engine, &config, 3, "sessions", replace);
        assert_eq!(
            plan(&missing, false).await.unwrap(),
            TargetPreparation::Create
        );
        assert_eq!(
            plan(&empty, false).await.unwrap(),
            TargetPreparation::UseExisting
        );
        assert_eq!(
            plan(&filled, true).await.unwrap(),
            TargetPreparation::Recreate
        );
    }

    #[tokio::test]
    async fn start_rejects_a_bad_timeout_before_touching_anything() {
        // No query results queued: any database access would fail the test.
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let error = service_with(db)
            .start(StartDataImport {
                service_id: 1,
                source_url: "postgres://u:p@db.example.com/app".to_string(),
                target_database: "app".to_string(),
                replace: false,
                timeout_minutes: Some(0),
                created_by: None,
            })
            .await
            .expect_err("bad timeout");
        assert!(matches!(error, DataImportError::Validation { .. }));
    }

    #[test]
    fn start_request_debug_never_shows_the_source() {
        let request = StartDataImport {
            service_id: 1,
            source_url: "postgres://u:hunter2@db.example.com/app".to_string(),
            target_database: "app".to_string(),
            replace: false,
            timeout_minutes: None,
            created_by: None,
        };
        assert!(!format!("{request:?}").contains("hunter2"));
    }
}
